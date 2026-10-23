//! The hub's side of a runtime host: starting one and talking to it.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};

use super::wire::{FromHost, HostRecord, HostSpec, ToHost, encode};
use super::{Settings, generation, left, record_path, until_left};
use crate::launch::{ExitInfo, Killer, Launched, ProcessHandle};
use crate::process::StderrTail;
use crossagents_core::ExitCause;

const START_TIMEOUT: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Room for agent output the driver has not read yet.
const PIPE: usize = 1 << 20;

/// Starts a host for `spec` and connects to it.
pub(crate) async fn spawn(settings: &Settings, spec: HostSpec) -> Result<Launched, String> {
    let dir = &settings.dir;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let spec_path = dir.join(format!("{}.spec.json", spec.id));
    write_private(&spec_path, &serde_json::to_vec(&spec).expect("spec serializes"))
        .map_err(|e| format!("cannot write {}: {e}", spec_path.display()))?;
    let (program, args) = settings
        .command
        .split_first()
        .ok_or("empty runtime host command")?;
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .arg("--dir")
        .arg(dir)
        .arg("--id")
        .arg(&spec.id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = spawn_detached(&mut command).map_err(|e| {
        let _ = std::fs::remove_file(&spec_path);
        format!("cannot start the runtime host: {e}")
    })?;
    let path = record_path(dir, &spec.id);
    let deadline = Instant::now() + START_TIMEOUT;
    let record = loop {
        if let Ok(bytes) = std::fs::read(&path)
            && let Ok(record) = serde_json::from_slice::<HostRecord>(&bytes)
        {
            break record;
        }
        if let Ok(Some(status)) = child.try_wait() {
            let _ = std::fs::remove_file(&spec_path);
            return Err(format!("the runtime host exited at start ({status})"));
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = std::fs::remove_file(&spec_path);
            return Err("the runtime host did not start".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    // The host lives on its own; dropping this handle leaves it running.
    drop(child);
    if let Some(error) = &record.error {
        let _ = std::fs::remove_file(&path);
        return Err(error.clone());
    }
    connect(&record).await
}

#[cfg(windows)]
fn spawn_detached(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    // Out of the hub's own job if it is in one (a terminal's, say), so closing that does
    // not take the agents along. A job that forbids leaving refuses; then stay in it.
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
    match command.spawn() {
        Ok(child) => Ok(child),
        Err(err) if err.raw_os_error() == Some(5) => {
            command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
            command.spawn()
        }
        Err(err) => Err(err),
    }
}

#[cfg(unix)]
fn spawn_detached(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
    command.spawn()
}

#[cfg(unix)]
fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::File::options()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?
        .write_all(contents)
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, contents)
}

async fn hello(record: &HostRecord) -> Result<(TcpStream, Option<u32>), String> {
    let mut stream = tokio::time::timeout(
        CONNECT_TIMEOUT,
        TcpStream::connect(("127.0.0.1", record.port)),
    )
    .await
    .map_err(|_| "the runtime host does not answer".to_owned())?
    .map_err(|e| format!("cannot reach the runtime host: {e}"))?;
    let hello = ToHost::Hello {
        id: record.id.clone(),
        token: record.token.clone(),
    };
    stream
        .write_all(encode(&hello).as_bytes())
        .await
        .map_err(|e| format!("cannot reach the runtime host: {e}"))?;
    // Read the answer byte by byte: what follows it belongs to the connection's reader.
    let mut line = Vec::new();
    let read = async {
        let mut byte = [0u8; 1];
        loop {
            use tokio::io::AsyncReadExt;
            if stream.read(&mut byte).await? == 0 {
                return Err(std::io::Error::other("closed"));
            }
            if byte[0] == b'\n' {
                return Ok(());
            }
            line.push(byte[0]);
        }
    };
    tokio::time::timeout(CONNECT_TIMEOUT, read)
        .await
        .map_err(|_| "the runtime host does not answer".to_owned())?
        .map_err(|e| format!("the runtime host hung up: {e}"))?;
    match serde_json::from_slice::<FromHost>(&line) {
        Ok(FromHost::Hello { child_pid }) => Ok((stream, child_pid)),
        Ok(FromHost::Refused { reason }) => Err(format!("the runtime host refused: {reason}")),
        _ => Err("the runtime host answered something else".into()),
    }
}

/// Whether the host behind `record` answers as itself.
pub(crate) async fn probe(record: &HostRecord) -> bool {
    hello(record).await.is_ok()
}

struct HostKiller {
    out: mpsc::UnboundedSender<String>,
    requested: Arc<AtomicBool>,
}

impl Killer for HostKiller {
    fn kill(&self) {
        self.requested.store(true, Ordering::SeqCst);
        let _ = self.out.send(encode(&ToHost::Kill));
    }
}

/// Connects to a running host: what it kept comes first, then the live stream.
pub(crate) async fn connect(record: &HostRecord) -> Result<Launched, String> {
    let (stream, child_pid) = hello(record).await?;
    let owner = generation();
    let (read, mut write) = stream.into_split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        loop {
            let line = tokio::select! {
                line = out_rx.recv() => line,
                () = until_left(owner) => None,
            };
            let Some(line) = line else { return };
            if write.write_all(line.as_bytes()).await.is_err() {
                return;
            }
        }
    });

    // The driver's ends: it reads stdout and writes stdin as if the host were its child.
    let (stdout, mut feed) = tokio::io::duplex(PIPE);
    let (drain, stdin) = tokio::io::duplex(PIPE);
    tokio::spawn({
        let out = out_tx.clone();
        async move {
            let mut lines = BufReader::new(drain).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if out.send(encode(&ToHost::In { line })).is_err() {
                    return;
                }
            }
            // The driver closed its input. A hub that leaves just lets go.
            if !left(owner) {
                let _ = out.send(encode(&ToHost::CloseStdin));
            }
        }
    });

    let tail = StderrTail::default();
    let (exit_tx, exit_rx) = watch::channel(None);
    let requested = Arc::new(AtomicBool::new(false));
    let path = super::settings().map(|s| record_path(&s.dir, &record.id));
    tokio::spawn({
        let tail = tail.clone();
        let out = out_tx.clone();
        let requested = requested.clone();
        async move {
            let mut reader = BufReader::new(read);
            let mut line = String::new();
            let mut last = 0;
            let mut exited = None;
            loop {
                line.clear();
                let read = tokio::select! {
                    read = reader.read_line(&mut line) => read,
                    () = until_left(owner) => {
                        // Stop here: what the agent says next is for the next hub. The
                        // driver's side stays open so it does not take this for an exit.
                        let _keep = feed;
                        std::future::pending::<()>().await;
                        return;
                    }
                };
                match read {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let Ok(frame) = serde_json::from_str::<FromHost>(line.trim_end()) else {
                    continue;
                };
                last = frame.seq().unwrap_or(last);
                match frame {
                    FromHost::Out { line, .. } => {
                        let mut bytes = line.into_bytes();
                        bytes.push(b'\n');
                        // A driver that stopped reading is gone; keep acknowledging anyway.
                        let _ = feed.write_all(&bytes).await;
                    }
                    FromHost::Err { line, .. } => tail.push(line).await,
                    FromHost::Exit { code, killed, .. } => {
                        let killed = killed || requested.load(Ordering::SeqCst);
                        exited = Some(ExitInfo::from_code(code, killed));
                    }
                    FromHost::Hello { .. } | FromHost::Refused { .. } => {}
                }
                if reader.buffer().is_empty() || exited.is_some() {
                    let _ = out.send(encode(&ToHost::Ack { seq: last }));
                }
                if exited.is_some() {
                    break;
                }
            }
            if left(owner) && exited.is_none() {
                // The hub let go; the next one picks the host up.
                return;
            }
            let exit = exited.unwrap_or_else(|| {
                // The host went away without a word: killed from outside, or by our Kill
                // (which takes the host down with the tree).
                if let Some(path) = &path {
                    let _ = std::fs::remove_file(path);
                }
                ExitInfo {
                    cause: if requested.load(Ordering::SeqCst) {
                        ExitCause::Killed
                    } else {
                        ExitCause::Lost
                    },
                    code: None,
                }
            });
            let _ = exit_tx.send(Some(exit));
            drop(feed);
        }
    });

    Ok(Launched {
        stdin: Box::new(stdin),
        stdout: Box::new(stdout),
        stderr: tail,
        process: ProcessHandle::new(
            child_pid,
            Some(owner),
            exit_rx,
            Arc::new(HostKiller {
                out: out_tx,
                requested,
            }),
        ),
    })
}
