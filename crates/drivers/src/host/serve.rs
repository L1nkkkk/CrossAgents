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

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{Notify, mpsc};

use super::job::Job;
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
    std::fs::write(&tmp, serde_json::to_vec_pretty(record).expect("record serializes"))?;
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
    let stdin = Arc::new(tokio::sync::Mutex::new(child.stdin.take()));
    let kill = Arc::new(Notify::new());
    let killed = Arc::new(AtomicBool::new(false));

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
        let stdin = stdin.clone();
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
                    stdin.clone(),
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
    stdin: Arc<tokio::sync::Mutex<Option<ChildStdin>>>,
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
                let mut stdin = stdin.lock().await;
                if let Some(pipe) = stdin.as_mut() {
                    let mut bytes = line.into_bytes();
                    bytes.push(b'\n');
                    let written = async {
                        pipe.write_all(&bytes).await?;
                        pipe.flush().await
                    };
                    if written.await.is_err() {
                        stdin.take();
                    }
                }
            }
            ToHost::Ack { seq } => state.ack(seq),
            ToHost::CloseStdin => {
                stdin.lock().await.take();
            }
            ToHost::Kill => kill.notify_one(),
            ToHost::Hello { .. } => {}
        }
    }
    state.detach(conn);
}

#[cfg(test)]
mod tests {
    use super::*;

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
