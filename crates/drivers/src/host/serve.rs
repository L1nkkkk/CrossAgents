//! The runtime host itself: `crossagents runtime-host --dir <dir> --id <id>`.
//!
//! Reads its spec, puts itself in a kill-on-close job, starts the agent, writes its record
//! and relays the agent's stdio to whichever hub is connected. Ends once the agent has
//! ended and a hub has read how (or nobody came for [`LINGER`]).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossagents_core::process::Job;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;
use tokio::sync::{Notify, mpsc, watch};

use super::record_path;
use super::wire::{FromHost, HostRecord, HostSpec, ToHost, encode};
use crate::process::hide_console;

/// Frames kept for a hub that is away; the oldest go first beyond this.
const KEEP: usize = 100_000;
/// How long an ended agent waits for a hub to read how it ended.
const LINGER: Duration = Duration::from_secs(30 * 60);
/// How long a connected hub gets to acknowledge the end.
const LINGER_CONNECTED: Duration = Duration::from_secs(30);
/// Output still in the pipes when the agent exits (its children may hold them open).
const DRAIN: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Frames {
    next: u64,
    kept: VecDeque<(u64, String)>,
    acked: u64,
    /// The connected hub: its number and where its frames go.
    conn: Option<(u64, mpsc::UnboundedSender<String>)>,
    exit: Option<u64>,
}

#[derive(Default)]
struct State {
    frames: Mutex<Frames>,
    changed: Notify,
}

impl State {
    fn push(&self, frame: impl FnOnce(u64) -> FromHost) -> u64 {
        let mut f = self.frames.lock().expect("frames lock");
        f.next += 1;
        let seq = f.next;
        let frame = frame(seq);
        let is_exit = matches!(frame, FromHost::Exit { .. });
        let line = encode(&frame);
        if let Some((_, conn)) = &f.conn
            && conn.send(line.clone()).is_err()
        {
            f.conn = None;
        }
        f.kept.push_back((seq, line));
        if f.kept.len() > KEEP {
            f.kept.pop_front();
        }
        if is_exit {
            f.exit = Some(seq);
        }
        drop(f);
        self.changed.notify_waiters();
        seq
    }

    fn ack(&self, seq: u64) {
        let mut f = self.frames.lock().expect("frames lock");
        f.acked = f.acked.max(seq);
        while f.kept.front().is_some_and(|(s, _)| *s <= seq) {
            f.kept.pop_front();
        }
        drop(f);
        self.changed.notify_waiters();
    }

    /// Makes `conn` the connected hub, after sending it everything not acknowledged.
    fn attach(&self, id: u64, conn: mpsc::UnboundedSender<String>) {
        let mut f = self.frames.lock().expect("frames lock");
        for (_, line) in &f.kept {
            let _ = conn.send(line.clone());
        }
        f.conn = Some((id, conn));
        drop(f);
        self.changed.notify_waiters();
    }

    fn detach(&self, id: u64) {
        let mut f = self.frames.lock().expect("frames lock");
        if f.conn.as_ref().is_some_and(|(c, _)| *c == id) {
            f.conn = None;
        }
        drop(f);
        self.changed.notify_waiters();
    }

    /// The agent ended and a hub read it.
    fn delivered(&self) -> bool {
        let f = self.frames.lock().expect("frames lock");
        f.exit.is_some_and(|exit| f.acked >= exit)
    }

    fn connected(&self) -> bool {
        self.frames.lock().expect("frames lock").conn.is_some()
    }
}

fn write_record(dir: &Path, record: &HostRecord) -> std::io::Result<()> {
    let path = record_path(dir, &record.id);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(record).expect("record serializes"),
    )?;
    std::fs::rename(tmp, path)
}

/// Runs the host until the agent has ended and its end was read.
pub async fn serve(dir: PathBuf, id: String) -> Result<(), String> {
    let spec_path = dir.join(format!("{id}.spec.json"));
    let spec: HostSpec = std::fs::read(&spec_path)
        .map_err(|e| e.to_string())
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string()))
        .map_err(|e| format!("cannot read {}: {e}", spec_path.display()))?;
    let _ = std::fs::remove_file(&spec_path);

    // In the job before the agent starts, so nothing it starts can be outside it.
    let (job, isolation) = match Job::new().and_then(|job| job.assign_current().map(|()| job)) {
        Ok(job) if cfg!(windows) => (Some(job), "windows_job_object".to_owned()),
        Ok(_) => (None, "direct:no job objects here".to_owned()),
        Err(err) => (None, format!("direct:{err}")),
    };
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| format!("cannot listen: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let mut record = HostRecord {
        id: id.clone(),
        owner: spec.owner.clone(),
        label: spec.label.clone(),
        host_pid: std::process::id(),
        child_pid: None,
        port,
        token: format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ),
        started_at: chrono::Utc::now(),
        isolation,
        exited: false,
        error: None,
    };

    let mut command = Command::new(&spec.program);
    command.args(&spec.args).current_dir(&spec.cwd);
    for key in &spec.env_remove {
        command.env_remove(key);
    }
    command
        .envs(spec.env.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console(&mut command);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            record.exited = true;
            record.error = Some(format!("cannot start {}: {err}", spec.program.display()));
            let _ = write_record(&dir, &record);
            return Err(record.error.unwrap_or_default());
        }
    };
    record.child_pid = child.id();
    write_record(&dir, &record).map_err(|e| format!("cannot write the record: {e}"))?;

    let state = Arc::new(State::default());
    let kill = Arc::new(Notify::new());
    let killed = Arc::new(AtomicBool::new(false));
    let (input, input_rx) = mpsc::unbounded_channel();
    let (close_input, close_rx) = watch::channel(false);
    let writer = tokio::spawn(write_input(
        child.stdin.take().expect("piped stdin"),
        input_rx,
        close_rx,
        kill.clone(),
        super::STDIN_STALL,
    ));

    let out = tokio::spawn({
        let state = state.clone();
        let stdout = child.stdout.take().expect("piped stdout");
        async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                state.push(|seq| FromHost::Out { seq, line });
            }
        }
    });
    let err = tokio::spawn({
        let state = state.clone();
        let stderr = child.stderr.take().expect("piped stderr");
        async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                state.push(|seq| FromHost::Err { seq, line });
            }
        }
    });

    let accept = tokio::spawn({
        let state = state.clone();
        let input = input.clone();
        let close_input = close_input.clone();
        let kill = kill.clone();
        let (id, token, pid) = (record.id.clone(), record.token.clone(), record.child_pid);
        async move {
            let next = AtomicU64::new(0);
            while let Ok((stream, _)) = listener.accept().await {
                let conn = next.fetch_add(1, Ordering::Relaxed);
                tokio::spawn(handle(
                    stream,
                    conn,
                    state.clone(),
                    (id.clone(), token.clone(), pid),
                    input.clone(),
                    close_input.clone(),
                    kill.clone(),
                ));
            }
        }
    });

    let pid = record.child_pid;
    let status = tokio::select! {
        status = child.wait() => status,
        _ = kill.notified() => {
            killed.store(true, Ordering::SeqCst);
            kill_tree(&mut child, pid);
            child.wait().await
        }
    };
    writer.abort();
    let _ = tokio::time::timeout(DRAIN, async {
        let _ = out.await;
        let _ = err.await;
    })
    .await;
    let code = status.ok().and_then(|s| s.code());
    state.push(|seq| FromHost::Exit {
        seq,
        code,
        killed: killed.load(Ordering::SeqCst),
    });
    record.exited = true;
    let _ = write_record(&dir, &record);

    // Wait for a hub to read the end: up to LINGER for one to come, LINGER_CONNECTED for a
    // connected one to acknowledge. A hub that asked for the kill knows how it ended: no
    // need to wait for another once it is gone.
    let asked = killed.load(Ordering::SeqCst);
    let since = tokio::time::Instant::now();
    let mut connected_since = None;
    while !state.delivered() && since.elapsed() < LINGER {
        if state.connected() {
            let at = *connected_since.get_or_insert_with(tokio::time::Instant::now);
            if at.elapsed() > LINGER_CONNECTED {
                break;
            }
        } else if asked {
            break;
        } else {
            connected_since = None;
        }
        let _ = tokio::time::timeout(Duration::from_secs(1), state.changed.notified()).await;
    }
    accept.abort();
    let _ = std::fs::remove_file(record_path(&dir, &record.id));
    // Closing the job ends whatever the agent left behind.
    drop(job);
    Ok(())
}

fn kill_tree(child: &mut tokio::process::Child, pid: Option<u32>) {
    let _ = child.start_kill();
    #[cfg(unix)]
    if let Some(pid) = pid {
        // The agent leads its own process group; take the group along.
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &format!("-{pid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(unix))]
    let _ = pid;
}

async fn handle(
    stream: TcpStream,
    conn: u64,
    state: Arc<State>,
    (id, token, pid): (String, String, Option<u32>),
    input: mpsc::UnboundedSender<Vec<u8>>,
    close_input: watch::Sender<bool>,
    kill: Arc<Notify>,
) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let hello = tokio::time::timeout(Duration::from_secs(5), lines.next_line()).await;
    let accepted = match hello {
        Ok(Ok(Some(line))) => match serde_json::from_str::<ToHost>(&line) {
            Ok(ToHost::Hello { id: i, token: t }) => i == id && t == token,
            _ => false,
        },
        _ => false,
    };
    if !accepted {
        let refused = FromHost::Refused {
            reason: "wrong id or token".into(),
        };
        let _ = write.write_all(encode(&refused).as_bytes()).await;
        return;
    }
    if write
        .write_all(encode(&FromHost::Hello { child_pid: pid }).as_bytes())
        .await
        .is_err()
    {
        return;
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if write.write_all(line.as_bytes()).await.is_err() {
                return;
            }
        }
    });
    state.attach(conn, tx);
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(frame) = serde_json::from_str::<ToHost>(&line) else {
            continue;
        };
        match frame {
            ToHost::In { line } => {
                let mut bytes = line.into_bytes();
                bytes.push(b'\n');
                if *close_input.borrow() || input.send(bytes).is_err() {
                    kill.notify_one();
                }
            }
            ToHost::Ack { seq } => state.ack(seq),
            ToHost::CloseStdin => {
                let _ = close_input.send(true);
            }
            ToHost::Kill => kill.notify_one(),
            ToHost::Hello { .. } => {}
        }
    }
    state.detach(conn);
}

async fn write_input<W: tokio::io::AsyncWrite + Unpin>(
    mut stdin: W,
    mut input: mpsc::UnboundedReceiver<Vec<u8>>,
    mut close: watch::Receiver<bool>,
    kill: Arc<Notify>,
    stall: Duration,
) {
    loop {
        let frame = tokio::select! {
            biased;
            _ = close.changed() => {
                if !input.is_empty() { kill.notify_one(); }
                return;
            }
            frame = input.recv() => match frame { Some(frame) => frame, None => return },
        };
        tokio::select! {
            biased;
            _ = close.changed() => { drop(stdin); kill.notify_one(); return; }
            result = super::write_progress(&mut stdin, &frame, stall) => {
                if result.is_err() { drop(stdin); kill.notify_one(); return; }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    // Run the real host in a separate test process: on Windows it owns a Job Object,
    // which must never be assigned to the parent test runner.
    #[test]
    fn host_process_entry() {
        if let Ok(dir) = std::env::var("CROSSAGENTS_TEST_HOST_DIR") {
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(super::serve(PathBuf::from(dir), "test-host".into()))
                .unwrap();
        }
    }

    #[tokio::test]
    async fn real_host_adoption_can_kill_a_child_not_reading_stdin() {
        for close_input in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let (program, args) = if cfg!(windows) {
                (
                    crate::process::resolve_program("powershell"),
                    vec![
                        "-NoProfile".into(),
                        "-Command".into(),
                        "Start-Sleep -Seconds 60".into(),
                    ],
                )
            } else {
                (PathBuf::from("sh"), vec!["-c".into(), "sleep 60".into()])
            };
            let spec = HostSpec {
                id: "test-host".into(),
                owner: "test".into(),
                label: "test".into(),
                program,
                args,
                cwd: dir.path().into(),
                env: vec![],
                env_remove: vec![],
            };
            std::fs::write(
                dir.path().join("test-host.spec.json"),
                serde_json::to_vec(&spec).unwrap(),
            )
            .unwrap();
            let mut host = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "host::serve::tests::host_process_entry",
                    "--nocapture",
                ])
                .env("CROSSAGENTS_TEST_HOST_DIR", dir.path())
                .kill_on_drop(true)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let record = tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    if let Ok(bytes) = tokio::fs::read(record_path(dir.path(), "test-host")).await
                        && let Ok(record) = serde_json::from_slice::<HostRecord>(&bytes)
                    {
                        break record;
                    }
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            })
            .await
            .expect("host record");
            // A prior hub connects and departs. The next connection is the adopted session.
            let mut first = TcpStream::connect(("127.0.0.1", record.port))
                .await
                .unwrap();
            first
                .write_all(
                    encode(&ToHost::Hello {
                        id: record.id.clone(),
                        token: record.token.clone(),
                    })
                    .as_bytes(),
                )
                .await
                .unwrap();
            let mut first = BufReader::new(first);
            let mut hello = String::new();
            first.read_line(&mut hello).await.unwrap();
            drop(first);
            let mut adopted = super::super::client::connect(&record).await.unwrap();
            adopted
                .stdin
                .write_all(format!("{}\n", "x".repeat(1024 * 1024)).as_bytes())
                .await
                .unwrap();
            adopted
                .stdin
                .write_all(b"{\"type\":\"control_response\",\"request_id\":\"permission\"}\n")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
            if close_input {
                drop(adopted.stdin);
            } else {
                adopted.process.start_kill();
            }
            let exit = tokio::time::timeout(Duration::from_secs(5), adopted.process.wait())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(exit.cause, crossagents_core::ExitCause::Killed);
            tokio::time::timeout(Duration::from_secs(5), host.wait())
                .await
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test]
    async fn hosted_permission_frame_follows_slow_large_frame() {
        let (reader, writer) = tokio::io::duplex(8);
        let (tx, rx) = mpsc::unbounded_channel();
        let (_close, close_rx) = watch::channel(false);
        let kill = Arc::new(Notify::new());
        let task = tokio::spawn(write_input(
            writer,
            rx,
            close_rx,
            kill.clone(),
            Duration::from_secs(2),
        ));
        let first = format!("{{\"prompt\":\"{}\"}}\n", "x".repeat(4096));
        let permission = b"{\"type\":\"control_response\",\"request_id\":\"permission\"}\n";
        tx.send(first.as_bytes().to_vec()).unwrap();
        tx.send(permission.to_vec()).unwrap();
        let mut lines = BufReader::new(reader).lines();
        assert_eq!(lines.next_line().await.unwrap().unwrap(), first.trim_end());
        assert_eq!(
            lines.next_line().await.unwrap().unwrap(),
            std::str::from_utf8(permission).unwrap().trim_end()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), kill.notified())
                .await
                .is_err()
        );
        drop(tx);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn large_hosted_frame_survives_a_long_tool_backpressure() {
        let (reader, writer) = tokio::io::duplex(8192);
        let (tx, rx) = mpsc::unbounded_channel();
        let (_close, close_rx) = watch::channel(false);
        let kill = Arc::new(Notify::new());
        let task = tokio::spawn(write_input(
            writer,
            rx,
            close_rx,
            kill.clone(),
            Duration::from_secs(5),
        ));
        let large = format!("{{\"prompt\":\"{}\"}}\n", "x".repeat(2 * 1024 * 1024));
        let permission = "{\"type\":\"control_response\",\"request_id\":\"permission\"}";
        tx.send(large.as_bytes().to_vec()).unwrap();
        tx.send(format!("{permission}\n").into_bytes()).unwrap();
        tokio::time::sleep(Duration::from_millis(3100)).await;
        let mut lines = BufReader::new(reader).lines();
        assert_eq!(lines.next_line().await.unwrap().unwrap(), large.trim_end());
        assert_eq!(lines.next_line().await.unwrap().unwrap(), permission);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), kill.notified())
                .await
                .is_err()
        );
        drop(tx);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn progress_for_more_than_three_seconds_does_not_kill() {
        let (mut reader, writer) = tokio::io::duplex(8);
        let (tx, rx) = mpsc::unbounded_channel();
        let (_close, close_rx) = watch::channel(false);
        let kill = Arc::new(Notify::new());
        let task = tokio::spawn(write_input(
            writer,
            rx,
            close_rx,
            kill.clone(),
            Duration::from_secs(1),
        ));
        tx.send(vec![b'x'; 400]).unwrap();
        let started = tokio::time::Instant::now();
        let mut bytes = vec![0; 400];
        for chunk in bytes.chunks_mut(8) {
            reader.read_exact(chunk).await.unwrap();
            tokio::time::sleep(Duration::from_millis(70)).await;
        }
        assert!(started.elapsed() > Duration::from_secs(3));
        assert!(bytes.iter().all(|b| *b == b'x'));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), kill.notified())
                .await
                .is_err()
        );
        drop(tx);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn stalled_input_and_close_kill_the_child() {
        for close_during_write in [false, true] {
            let (_reader, writer) = tokio::io::duplex(8);
            let (tx, rx) = mpsc::unbounded_channel();
            let (close, close_rx) = watch::channel(false);
            let kill = Arc::new(Notify::new());
            let task = tokio::spawn(write_input(
                writer,
                rx,
                close_rx,
                kill.clone(),
                Duration::from_millis(200),
            ));
            tx.send(vec![b'x'; 1024]).unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
            if close_during_write {
                close.send(true).unwrap();
            }
            tokio::time::timeout(Duration::from_secs(1), kill.notified())
                .await
                .unwrap();
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn reconnected_host_handles_kill_while_stdin_is_blocked() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let state = Arc::new(State::default());
        let kill = Arc::new(Notify::new());
        let (_reader, stdin) = tokio::io::duplex(8);
        let (input, rx) = mpsc::unbounded_channel();
        let (close, close_rx) = watch::channel(false);
        let writer = tokio::spawn(write_input(
            stdin,
            rx,
            close_rx,
            kill.clone(),
            Duration::from_secs(5),
        ));
        let server = tokio::spawn({
            let state = state.clone();
            let kill = kill.clone();
            async move {
                for conn in 0..2 {
                    let (stream, _) = listener.accept().await.unwrap();
                    tokio::spawn(handle(
                        stream,
                        conn,
                        state.clone(),
                        ("id".into(), "token".into(), None),
                        input.clone(),
                        close.clone(),
                        kill.clone(),
                    ));
                }
            }
        });
        async fn connect(
            address: std::net::SocketAddr,
        ) -> (
            tokio::net::tcp::OwnedWriteHalf,
            BufReader<tokio::net::tcp::OwnedReadHalf>,
        ) {
            let (read, mut write) = TcpStream::connect(address).await.unwrap().into_split();
            write
                .write_all(
                    encode(&ToHost::Hello {
                        id: "id".into(),
                        token: "token".into(),
                    })
                    .as_bytes(),
                )
                .await
                .unwrap();
            let mut read = BufReader::new(read);
            let mut hello = String::new();
            read.read_line(&mut hello).await.unwrap();
            (write, read)
        }
        let (mut first, first_read) = connect(address).await;
        first
            .write_all(
                encode(&ToHost::In {
                    line: "x".repeat(1024),
                })
                .as_bytes(),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(first);
        drop(first_read);
        let (mut adopted, _read) = connect(address).await;
        adopted
            .write_all(encode(&ToHost::Kill).as_bytes())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), kill.notified())
            .await
            .unwrap();
        server.await.unwrap();
        writer.abort();
    }

    #[tokio::test]
    async fn adopted_client_observes_exit_after_hosted_stdin_stalls() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let state = Arc::new(State::default());
        let kill = Arc::new(Notify::new());
        let (_reader, stdin) = tokio::io::duplex(8);
        let (input, rx) = mpsc::unbounded_channel();
        let (close, close_rx) = watch::channel(false);
        let writer = tokio::spawn(write_input(
            stdin,
            rx,
            close_rx,
            kill.clone(),
            Duration::from_millis(300),
        ));
        let server = tokio::spawn({
            let state = state.clone();
            let kill = kill.clone();
            async move {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::spawn(handle(
                    stream,
                    0,
                    state.clone(),
                    ("id".into(), "token".into(), None),
                    input,
                    close,
                    kill.clone(),
                ));
                kill.notified().await;
                state.push(|seq| FromHost::Exit {
                    seq,
                    code: None,
                    killed: true,
                });
            }
        });
        let record = HostRecord {
            id: "id".into(),
            owner: "test".into(),
            label: "test".into(),
            host_pid: 0,
            child_pid: None,
            port: address.port(),
            token: "token".into(),
            started_at: chrono::Utc::now(),
            isolation: "test".into(),
            exited: false,
            error: None,
        };
        let mut adopted = super::super::client::connect(&record).await.unwrap();
        adopted
            .stdin
            .write_all(format!("{}\n", "x".repeat(4096)).as_bytes())
            .await
            .unwrap();
        let exit = tokio::time::timeout(Duration::from_secs(2), adopted.process.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(exit.cause, crossagents_core::ExitCause::Killed);
        server.await.unwrap();
        writer.await.unwrap();
    }

    #[test]
    fn replays_what_was_not_acknowledged() {
        let state = State::default();
        for n in 0..3 {
            state.push(|seq| FromHost::Out {
                seq,
                line: format!("line {n}"),
            });
        }
        state.ack(2);
        let (tx, mut rx) = mpsc::unbounded_channel();
        state.attach(7, tx);
        let first = rx.try_recv().unwrap();
        assert!(first.contains("\"seq\":3"), "{first}");
        assert!(rx.try_recv().is_err());
        state.push(|seq| FromHost::Exit {
            seq,
            code: Some(0),
            killed: false,
        });
        assert!(rx.try_recv().unwrap().contains("exit"));
        assert!(!state.delivered());
        state.ack(4);
        assert!(state.delivered());
        state.detach(7);
        assert!(!state.connected());
    }
}
