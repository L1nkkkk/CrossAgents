//! Running a description's install or upgrade command (docs/DESIGN.md §9.3).
//!
//! Exactly one command is run, and it is the description's own argv: nothing is elevated
//! (no `sudo`, no `runas`; an installer that needs administrator rights fails and says so),
//! and nothing a caller sent is executed. A command that runs past its deadline is killed
//! with its whole process tree (a Windows Job Object), so an installer that spawned children
//! leaves none behind.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::host::job::Job;
use crate::process::{hide_console, resolve_program};

/// How long an install or upgrade may take.
pub const TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How many output lines reach the caller; the rest are counted and dropped.
pub const OUTPUT_LIMIT: usize = 500;
/// How long one line may be: an installer that prints a whole file is cut here.
const LINE_LIMIT: usize = 2000;
/// How long the output already in the pipes is read after the command exits. A grandchild
/// may keep a write end open; waiting for every reader to see EOF would then hang the job
/// long after the installer itself finished.
const DRAIN_GRACE: Duration = Duration::from_millis(800);

/// What running the command came to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Run {
    /// The exit code, when the command exited on its own.
    pub code: Option<i32>,
    /// The command was stopped at [`TIMEOUT`], with its tree.
    pub timed_out: bool,
    /// Lines past [`OUTPUT_LIMIT`] that were not handed over.
    pub dropped: u64,
    /// The program could not be started at all (the first line says why).
    pub not_started: bool,
}

/// Runs `argv`, handing every line it prints to `on_line` with the number of lines dropped
/// so far.
///
/// `on_line` runs in the calling task, one line at a time: a slow callback delays reading,
/// but no output is lost up to the channel's buffer.
pub async fn run<F>(argv: &[String], on_line: F) -> Run
where
    F: FnMut(String, u64),
{
    run_within(argv, TIMEOUT, OUTPUT_LIMIT, on_line).await
}

/// [`run`] with the deadline and the line cap given: what the tests drive with a short
/// timeout and a small cap, instead of waiting ten minutes for a 500-line cap.
pub async fn run_within<F>(argv: &[String], timeout: Duration, limit: usize, mut on_line: F) -> Run
where
    F: FnMut(String, u64),
{
    let Some((program, args)) = argv.split_first() else {
        return Run {
            not_started: true,
            ..Run::default()
        };
    };
    let program = resolve_program(program);
    let mut command = Command::new(&program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    hide_console(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            on_line(format!("cannot start {}: {err}", program.display()), 0);
            return Run {
                not_started: true,
                ..Run::default()
            };
        }
    };

    // The whole tree goes with the job: closing it (on a timeout, or here) ends every
    // process the installer started, not just the one whose pid we know.
    let job = child.id().and_then(|pid| {
        let job = Job::new().ok()?;
        job.assign(pid).ok()?;
        Some(job)
    });

    let (tx, mut rx) = mpsc::channel::<String>(512);
    if let Some(stdout) = child.stdout.take() {
        read_lines(stdout, tx.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        read_lines(stderr, tx);
    }

    let mut wait = Box::pin(child.wait());
    let mut deadline = Box::pin(tokio::time::sleep(timeout));
    let mut exit: Option<Option<i32>> = None;
    let mut readers = true;
    let mut timed_out = false;
    let mut lines = 0usize;
    let mut dropped = 0u64;

    while exit.is_none() {
        tokio::select! {
            status = &mut wait => {
                exit = Some(status.ok().and_then(|status| status.code()));
            }
            line = rx.recv(), if readers => {
                match line {
                    Some(line) => {
                        if lines < limit {
                            on_line(line, dropped);
                            lines += 1;
                        } else {
                            dropped += 1;
                        }
                    }
                    None => readers = false,
                }
            }
            _ = &mut deadline => {
                timed_out = true;
                break;
            }
        }
    }

    // The command has exited (or was killed): read what is still in the pipes, but not for
    // long — a grandchild may hold a write end open long after the installer is done.
    if !timed_out && readers {
        let drain = tokio::time::sleep(DRAIN_GRACE);
        tokio::pin!(drain);
        loop {
            tokio::select! {
                line = rx.recv() => {
                    match line {
                        Some(line) => {
                            if lines < limit {
                                on_line(line, dropped);
                                lines += 1;
                            } else {
                                dropped += 1;
                            }
                        }
                        None => break,
                    }
                }
                _ = &mut drain => break,
            }
        }
    }

    // Dropping the job closes its last handle, which kills whatever is still in it.
    drop(job);
    if timed_out {
        drop(wait);
        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        return Run {
            code: None,
            timed_out: true,
            dropped,
            not_started: false,
        };
    }
    Run {
        code: exit.unwrap_or(None),
        timed_out: false,
        dropped,
        not_started: false,
    }
}

/// Reads `stream` line by line into `tx`, until it ends.
fn read_lines(stream: impl AsyncRead + Unpin + Send + 'static, tx: mpsc::Sender<String>) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stream).lines();
        loop {
            match reader.next_line().await {
                Ok(Some(line)) => {
                    if tx.send(truncate(line)).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
    });
}

fn truncate(line: String) -> String {
    if line.chars().count() <= LINE_LIMIT {
        return line;
    }
    let mut cut: String = line.chars().take(LINE_LIMIT).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command that prints `text` and exits with `code`.
    fn script(text: &str, code: i32) -> Vec<String> {
        if cfg!(windows) {
            vec![
                "cmd".into(),
                "/c".into(),
                format!("echo {text} & exit {code}"),
            ]
        } else {
            vec![
                "sh".into(),
                "-c".into(),
                format!("echo {text}; exit {code}"),
            ]
        }
    }

    #[tokio::test]
    async fn a_command_prints_lines_and_reports_its_code() {
        let mut seen: Vec<String> = Vec::new();
        let run = run(&script("hello", 0), |line, _| seen.push(line)).await;
        assert_eq!(run.code, Some(0), "{run:?}");
        assert!(!run.timed_out && !run.not_started);
        assert!(seen.iter().any(|line| line.contains("hello")), "{seen:?}");
    }

    #[tokio::test]
    async fn a_non_zero_exit_is_reported() {
        let run = run(&script("nope", 3), |_, _| {}).await;
        assert_eq!(run.code, Some(3), "{run:?}");
    }

    #[tokio::test]
    async fn a_program_that_is_not_there_is_not_started() {
        let mut seen: Vec<String> = Vec::new();
        let run = run(&["crossagents-no-such-installer".into()], |line, _| {
            seen.push(line)
        })
        .await;
        assert!(run.not_started, "{run:?}");
        assert!(seen[0].contains("cannot start"), "{seen:?}");
    }

    #[test]
    fn long_lines_are_cut() {
        let line = "x".repeat(LINE_LIMIT + 10);
        let cut = truncate(line);
        assert_eq!(cut.chars().count(), LINE_LIMIT + 1);
    }

    #[tokio::test]
    async fn output_past_the_cap_is_dropped_and_counted() {
        let argv = if cfg!(windows) {
            vec![
                "cmd".to_owned(),
                "/c".to_owned(),
                "echo a& echo b& echo c& echo d& echo e".to_owned(),
            ]
        } else {
            vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "echo a; echo b; echo c; echo d; echo e".to_owned(),
            ]
        };
        let mut seen: Vec<String> = Vec::new();
        let run = run_within(&argv, Duration::from_secs(30), 2, |line, _| seen.push(line)).await;
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert_eq!(run.dropped, 3, "{run:?}");
        assert_eq!(run.code, Some(0), "{run:?}");
    }

    /// A timeout takes the installer's children too, not just the process that was started:
    /// a Job Object holds the tree, and closing it ends what is in it.
    ///
    /// Not run by `cargo test`. It starts a program and kills the whole tree, which is also
    /// what malware does: iOA flagged this test binary as a trojan on 2026-10-26 (md5
    /// `01a44ab4…`) and stopped it mid-run. The child is now a plain `ping` under `cmd`, and
    /// liveness is told apart by deleting a file it holds open, so the binary no longer
    /// carries `powershell` or `tasklist`. Run it on purpose:
    /// `cargo test -p crossagents-drivers -- --ignored a_timeout_kills_the_whole_tree`.
    #[cfg(windows)]
    #[ignore = "starts a child process and kills the tree; iOA flags this pattern"]
    #[tokio::test]
    async fn a_timeout_kills_the_whole_tree() {
        let dir = tempfile::tempdir().unwrap();
        let held = dir.path().join("held.txt");
        // The grandchild keeps this file open. It only becomes removable once the whole tree
        // is gone, so a successful delete is the proof that the timeout took the children too.
        let argv = vec![
            "cmd".to_owned(),
            "/c".to_owned(),
            format!("ping -n 30 127.0.0.1 > \"{}\"", held.display()),
        ];
        let run = run_within(&argv, Duration::from_secs(3), 10, |_, _| {}).await;
        assert!(run.timed_out, "{run:?}");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match std::fs::remove_file(&held) {
                Ok(()) => break,
                Err(e) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the tree outlived the timeout: {e}"
                    );
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
}
