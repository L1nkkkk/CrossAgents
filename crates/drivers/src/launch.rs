//! Starting agent processes: directly as children of the hub, or in a runtime host that
//! outlives it ([`crate::host`]). Drivers see the same [`Launched`] either way.
//!
//! A direct child goes in a Windows Job Object owned by the hub, so its whole tree ends
//! with the hub, crash included: no orphans.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossagents_core::process::Job;
use crossagents_core::{CoreError, ExitCause};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::Command;
use tokio::sync::{Notify, watch};

use crate::host::{self, HostSpec};
use crate::process::{StderrTail, hide_console};

pub type BoxRead = Box<dyn AsyncRead + Send + Unpin>;
pub type BoxWrite = Box<dyn AsyncWrite + Send + Unpin>;

/// What to start.
#[derive(Debug, Clone, Default)]
pub struct LaunchSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    /// Variables of the hub's environment the agent must not inherit.
    pub env_remove: Vec<String>,
    /// Set when a later hub may take the process over (see [`adopt`]): it then runs in a
    /// runtime host, if hosts are enabled. E.g. `claude-native:claude:<session id>`.
    pub owner: Option<String>,
    /// For people, e.g. `codex app-server`.
    pub label: String,
}

/// A started (or adopted) agent process.
pub struct Launched {
    pub stdin: BoxWrite,
    pub stdout: BoxRead,
    pub stderr: StderrTail,
    pub process: ProcessHandle,
}

/// How a process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitInfo {
    pub cause: ExitCause,
    pub code: Option<i32>,
}

impl ExitInfo {
    pub fn from_code(code: Option<i32>, killed: bool) -> Self {
        let cause = if killed {
            ExitCause::Killed
        } else if code == Some(0) {
            ExitCause::Exited
        } else {
            ExitCause::Crashed
        };
        Self { cause, code }
    }

    /// `"<what> crashed (exit code 3)"`.
    pub fn describe(&self, what: &str) -> String {
        format!("{what} {}", self.cause.describe(self.code))
    }
}

pub(crate) trait Killer: Send + Sync {
    fn kill(&self);
}

/// Owns the process: dropping it stops the process, unless the hub is leaving its hosted
/// processes running ([`host::leave`]).
pub struct ProcessHandle {
    pid: Option<u32>,
    /// Runs in a host; the hub generation that owns it.
    hosted: Option<u64>,
    exit: watch::Receiver<Option<ExitInfo>>,
    killer: Arc<dyn Killer>,
}

impl ProcessHandle {
    pub(crate) fn new(
        pid: Option<u32>,
        hosted: Option<u64>,
        exit: watch::Receiver<Option<ExitInfo>>,
        killer: Arc<dyn Killer>,
    ) -> Self {
        Self {
            pid,
            hosted,
            exit,
            killer,
        }
    }

    /// The agent's process id.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Runs in a runtime host, so it survives the hub.
    pub fn outlives_hub(&self) -> bool {
        self.hosted.is_some()
    }

    fn left(&self) -> bool {
        self.hosted.is_some_and(host::left)
    }

    /// Asks the process (and its tree) to stop; does not wait.
    pub fn start_kill(&self) {
        if !self.left() {
            self.killer.kill();
        }
    }

    /// How it ended, once it has.
    pub fn exit_info(&self) -> Option<ExitInfo> {
        *self.exit.borrow()
    }

    /// Waits for the process to end.
    pub async fn wait(&self) -> Option<ExitInfo> {
        let mut exit = self.exit.clone();
        exit.wait_for(Option::is_some).await.ok().and_then(|e| *e)
    }

    /// [`Self::wait`], giving up after `within`.
    pub async fn exit_within(&self, within: Duration) -> Option<ExitInfo> {
        self.watch().within(within).await
    }

    /// Watches for the end without owning the process.
    pub fn watch(&self) -> ExitWatch {
        ExitWatch(self.exit.clone())
    }
}

/// Learns how a process ended, without keeping it alive.
#[derive(Clone)]
pub struct ExitWatch(watch::Receiver<Option<ExitInfo>>);

impl ExitWatch {
    /// How it ended, waiting up to `within` for it to end.
    pub async fn within(&self, within: Duration) -> Option<ExitInfo> {
        let mut exit = self.0.clone();
        tokio::time::timeout(within, exit.wait_for(Option::is_some))
            .await
            .ok()
            .and_then(|r| r.ok().and_then(|e| *e))
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        self.start_kill();
    }
}

/// Starts `spec`: in a runtime host when it has an owner and hosts are enabled, else as a
/// direct child.
pub async fn launch(spec: LaunchSpec) -> Result<Launched, CoreError> {
    if let Some(owner) = &spec.owner
        && let Some(settings) = host::settings()
    {
        let host_spec = HostSpec {
            id: uuid::Uuid::now_v7().simple().to_string(),
            owner: owner.clone(),
            label: spec.label.clone(),
            program: spec.program.clone(),
            args: spec.args.clone(),
            cwd: spec.cwd.clone(),
            env: spec.env.clone(),
            env_remove: spec.env_remove.clone(),
        };
        return host::spawn(&settings, host_spec)
            .await
            .map_err(|e| CoreError::Agent(format!("cannot start {}: {e}", spec.label)));
    }
    direct(spec)
}

/// Takes over the process a previous hub left for `owner`, if there is one.
pub async fn adopt(owner: &str) -> Option<Result<Launched, CoreError>> {
    let record = host::claim(owner)?;
    Some(
        host::connect(&record)
            .await
            .map_err(|e| CoreError::Agent(format!("cannot take over {}: {e}", record.label))),
    )
}

struct DirectKiller(Arc<Notify>);

impl Killer for DirectKiller {
    fn kill(&self) {
        self.0.notify_one();
    }
}

fn direct(spec: LaunchSpec) -> Result<Launched, CoreError> {
    let mut command = Command::new(&spec.program);
    command.args(&spec.args).current_dir(&spec.cwd);
    for key in &spec.env_remove {
        command.env_remove(key);
    }
    command
        .envs(spec.env.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    hide_console(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| CoreError::Agent(format!("cannot start {}: {e}", spec.program.display())))?;
    let pid = child.id();
    // The tree goes with the hub. Assigned right after the start: an agent does not start
    // children before reading its first input.
    let job = pid.and_then(|pid| {
        let job = Job::new().ok()?;
        match job.assign(pid) {
            Ok(()) => Some(job),
            Err(err) => {
                tracing::debug!(%err, "agent process runs outside a job object");
                None
            }
        }
    });
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = StderrTail::spawn(child.stderr.take().expect("piped stderr"));
    let (exit_tx, exit_rx) = watch::channel(None);
    let kill = Arc::new(Notify::new());
    let killed = Arc::new(AtomicBool::new(false));
    tokio::spawn({
        let kill = kill.clone();
        async move {
            let _job = job;
            let status = tokio::select! {
                status = child.wait() => status,
                _ = kill.notified() => {
                    killed.store(true, Ordering::SeqCst);
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            let code = status.ok().and_then(|s| s.code());
            let _ = exit_tx.send(Some(ExitInfo::from_code(
                code,
                killed.load(Ordering::SeqCst),
            )));
        }
    });
    Ok(Launched {
        stdin: Box::new(stdin),
        stdout: Box::new(stdout),
        stderr,
        process: ProcessHandle::new(pid, None, exit_rx, Arc::new(DirectKiller(kill))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    fn echo_spec() -> LaunchSpec {
        // A line in, the same line out; exits with code 3 when stdin closes.
        let (program, args) = if cfg!(windows) {
            (
                "powershell",
                vec![
                    "-NoProfile".to_owned(),
                    "-Command".to_owned(),
                    "$l = [Console]::In.ReadLine(); [Console]::Out.WriteLine($l); [Console]::Out.Flush(); $null = [Console]::In.ReadToEnd(); exit 3".to_owned(),
                ],
            )
        } else {
            (
                "sh",
                vec![
                    "-c".to_owned(),
                    "read l; echo \"$l\"; cat >/dev/null; exit 3".to_owned(),
                ],
            )
        };
        LaunchSpec {
            program: crate::process::resolve_program(program),
            args,
            cwd: std::env::temp_dir(),
            label: "echo".into(),
            ..LaunchSpec::default()
        }
    }

    #[tokio::test]
    async fn direct_children_report_how_they_ended() {
        let Launched {
            mut stdin,
            stdout,
            process,
            ..
        } = launch(echo_spec()).await.unwrap();
        stdin.write_all(b"hello\n").await.unwrap();
        stdin.flush().await.unwrap();
        let mut lines = BufReader::new(stdout).lines();
        assert_eq!(lines.next_line().await.unwrap().unwrap().trim(), "hello");
        drop(stdin);
        let exit = process.exit_within(Duration::from_secs(20)).await.unwrap();
        assert_eq!(exit, ExitInfo::from_code(Some(3), false));
        assert_eq!(exit.cause, ExitCause::Crashed);

        let launched = launch(echo_spec()).await.unwrap();
        launched.process.start_kill();
        let exit = launched
            .process
            .exit_within(Duration::from_secs(20))
            .await
            .unwrap();
        assert_eq!(exit.cause, ExitCause::Killed);
    }
}
