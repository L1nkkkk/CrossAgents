//! Acceptance checks, run by the hub itself (docs/DESIGN.md §8 Verify).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use crossagents_core::CheckResult;
use crossagents_core::process::{self, NoWindow};
use tokio::process::Command;

use crate::workspace::worktree_env;

/// Decides whether a task's result is acceptable.
#[async_trait]
pub trait Verifier: Send + Sync {
    async fn check(&self, dir: &Path, commands: &[String]) -> Vec<CheckResult>;

    /// `check` with each command's time limit given (the project's run checks).
    async fn check_within(
        &self,
        dir: &Path,
        commands: &[String],
        _timeout: Duration,
    ) -> Vec<CheckResult> {
        self.check(dir, commands).await
    }
}

/// Runs each acceptance command in the system shell; all must exit 0. Stops at the first
/// failure, since later checks usually depend on earlier ones.
#[derive(Debug, Clone)]
pub struct ShellVerifier {
    pub timeout: Duration,
    pub max_output: usize,
    /// Where runs' worktrees live: checks in one get the environment the run's agents there
    /// do (`workspace::worktree_env`), so they build in the same directory.
    pub worktrees: Option<PathBuf>,
}

impl Default for ShellVerifier {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(600),
            max_output: 8_000,
            worktrees: None,
        }
    }
}

/// `command` in the system shell. On Windows the command line is written out as is, not
/// through `arg`: that escapes the command's quotes as `\"`, which cmd does not understand and
/// hands to the program unchanged (`powershell -Command "exit 7"` printed `exit 7` and passed).
/// `/S` makes cmd strip only the outer pair of quotes added here; `/D` skips AutoRun.
#[cfg(windows)]
fn shell(command: &str) -> Command {
    let mut cmd = Command::new("cmd");
    cmd.raw_arg(format!("/D /S /C \"{command}\"")).no_window();
    cmd
}

#[cfg(not(windows))]
fn shell(command: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command);
    cmd
}

/// A failed check's output, with a word on what the exit code means when the command said
/// nothing useful: a program that is not there fails silently on Windows (9009, also what
/// the Microsoft Store's `python` placeholder returns).
fn explain(code: Option<i32>, output: String) -> String {
    let note = match code {
        Some(9009) => {
            "the program was not found (exit 9009): it is not installed or not on PATH; on Windows, `python` may be only the Microsoft Store placeholder"
        }
        Some(127) if output.is_empty() => {
            "the program was not found (exit 127): it is not installed or not on PATH"
        }
        _ if output.is_empty() => "the command printed nothing",
        _ => return output,
    };
    if output.is_empty() {
        format!("({note})")
    } else {
        format!("{output}\n({note})")
    }
}

/// Keeps the end of long output, where errors usually are.
fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…(truncated)\n{}", &text[start..])
}

#[async_trait]
impl Verifier for ShellVerifier {
    async fn check(&self, dir: &Path, commands: &[String]) -> Vec<CheckResult> {
        self.check_within(dir, commands, self.timeout).await
    }

    async fn check_within(
        &self,
        dir: &Path,
        commands: &[String],
        timeout: Duration,
    ) -> Vec<CheckResult> {
        let mut results = Vec::new();
        for command in commands.iter().filter(|c| !c.trim().is_empty()) {
            let mut cmd = shell(command);
            if let Some(root) = self.worktrees.as_deref() {
                cmd.envs(worktree_env(root, dir));
            }
            cmd.current_dir(dir)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            // The whole tree goes when the check ends, times out or fails: killing the shell
            // alone left what it started still writing to the worktree.
            let result = match process::spawn_tree(&mut cmd) {
                Err(e) => CheckResult {
                    command: command.clone(),
                    exit_code: None,
                    passed: false,
                    output: format!("cannot start: {e}"),
                },
                Ok((child, _tree)) => {
                    match tokio::time::timeout(timeout, child.wait_with_output()).await {
                        Err(_) => CheckResult {
                            command: command.clone(),
                            exit_code: None,
                            passed: false,
                            output: format!("timed out after {}s", timeout.as_secs()),
                        },
                        Ok(Err(e)) => CheckResult {
                            command: command.clone(),
                            exit_code: None,
                            passed: false,
                            output: format!("failed to run: {e}"),
                        },
                        Ok(Ok(out)) => {
                            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                            text.push_str(&String::from_utf8_lossy(&out.stderr));
                            let code = out.status.code();
                            let mut output = tail(text.trim(), self.max_output);
                            if !out.status.success() {
                                output = explain(code, output);
                            }
                            CheckResult {
                                command: command.clone(),
                                exit_code: code,
                                passed: out.status.success(),
                                output,
                            }
                        }
                    }
                }
            };
            let passed = result.passed;
            results.push(result);
            if !passed {
                break;
            }
        }
        results
    }
}

/// One command in `dir` with its own time limit (`workspace_run`), building in the run's
/// shared target when `dir` is under `worktrees`. Never fails: problems are in the result.
pub async fn run_command(
    dir: &Path,
    command: &str,
    timeout: Duration,
    max_output: usize,
    worktrees: &Path,
) -> CheckResult {
    let verifier = ShellVerifier {
        timeout,
        max_output,
        worktrees: Some(worktrees.to_path_buf()),
    };
    let mut results = verifier.check(dir, &[command.to_owned()]).await;
    results.pop().unwrap_or(CheckResult {
        command: command.to_owned(),
        exit_code: None,
        passed: false,
        output: "empty command".into(),
    })
}

pub fn all_passed(results: &[CheckResult]) -> bool {
    !results.is_empty() && results.iter().all(|r| r.passed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn passes_and_stops_at_first_failure() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "content").unwrap();
        let verifier = ShellVerifier::default();
        let ok = verifier
            .check(dir.path(), &["echo hello".into(), "git --version".into()])
            .await;
        assert!(all_passed(&ok), "{ok:?}");
        assert!(ok[0].output.contains("hello"));

        let bad = verifier
            .check(dir.path(), &["exit 3".into(), "echo never".into()])
            .await;
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].exit_code, Some(3));
        assert!(!all_passed(&bad));
    }

    #[tokio::test]
    async fn checks_in_a_run_build_in_its_own_worktrees_target() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("worktrees");
        let dir = root.join("ab12cd34").join("integration");
        std::fs::create_dir_all(&dir).unwrap();
        // A Rust project: the preset sets the build directory.
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        let verifier = ShellVerifier {
            worktrees: Some(root.clone()),
            ..ShellVerifier::default()
        };
        let echo = if cfg!(windows) {
            "echo %CARGO_TARGET_DIR%"
        } else {
            "echo $CARGO_TARGET_DIR"
        };
        let out = verifier.check(&dir, &[echo.into()]).await;
        let want = dunce::canonicalize(&root)
            .unwrap()
            .join("ab12cd34")
            .join("targets")
            .join("integration")
            .join("cargo");
        assert_eq!(out[0].output, want.display().to_string());
        // A task's checkout builds in its own, not in the run's.
        let task = root.join("ab12cd34").join("auth");
        std::fs::create_dir_all(&task).unwrap();
        std::fs::write(task.join("Cargo.toml"), "").unwrap();
        let out = verifier.check(&task, &[echo.into()]).await;
        assert!(
            Path::new(&out[0].output).ends_with(Path::new("auth").join("cargo")),
            "{}",
            out[0].output
        );
        assert_ne!(out[0].output, want.display().to_string());
    }

    #[tokio::test]
    async fn timeout_fails() {
        let verifier = ShellVerifier {
            timeout: Duration::from_millis(200),
            max_output: 100,
            worktrees: None,
        };
        let sleep = if cfg!(windows) {
            "ping -n 5 127.0.0.1"
        } else {
            "sleep 5"
        };
        let res = verifier.check(Path::new("."), &[sleep.into()]).await;
        assert!(!res[0].passed);
        assert!(res[0].output.contains("timed out"));
    }

    #[tokio::test]
    async fn a_timed_out_check_takes_what_it_started_along() {
        let dir = tempfile::tempdir().unwrap();
        let verifier = ShellVerifier {
            timeout: Duration::from_millis(500),
            ..ShellVerifier::default()
        };
        // The shell's child writes the file 3s in, long after the check gave up on it.
        let late = if cfg!(windows) {
            r#"powershell -NoProfile -Command "Start-Sleep 3; Set-Content -Path late.txt -Value x""#
        } else {
            "sh -c 'sleep 3; echo x > late.txt' & wait"
        };
        let res = verifier.check(dir.path(), &[late.into()]).await;
        assert!(res[0].output.contains("timed out"), "{res:?}");
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(
            !dir.path().join("late.txt").exists(),
            "the check's child kept running"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn quoted_commands_reach_the_program_as_written() {
        let verifier = ShellVerifier::default();
        let exit = verifier
            .check(
                Path::new("."),
                &[r#"powershell -NoProfile -Command "exit 7""#.into()],
            )
            .await;
        assert_eq!(exit[0].exit_code, Some(7), "{exit:?}");
        assert!(!exit[0].passed);

        let echo = verifier
            .check(Path::new("."), &[r#"echo "C:\Program Files\a b""#.into()])
            .await;
        assert_eq!(echo[0].output, r#""C:\Program Files\a b""#);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn plain_commands_still_work_on_windows() {
        let verifier = ShellVerifier::default();
        let out = verifier
            .check(
                Path::new("."),
                &[
                    "cargo --version".into(),
                    "echo one&& echo two".into(),
                    "echo piped | findstr piped".into(),
                ],
            )
            .await;
        assert!(all_passed(&out), "{out:?}");
        assert!(out[0].output.starts_with("cargo "));
        assert_eq!(out[1].output.lines().collect::<Vec<_>>(), ["one", "two"]);
        assert!(out[2].output.contains("piped"));
    }

    #[test]
    fn tail_keeps_the_end() {
        let t = tail("abcdefghij", 4);
        assert!(t.ends_with("ghij"));
        assert_eq!(tail("abc", 4), "abc");
    }

    #[test]
    fn a_missing_program_is_named() {
        assert!(explain(Some(9009), String::new()).contains("not found"));
        assert!(explain(Some(127), String::new()).contains("not found"));
        assert_eq!(
            explain(Some(1), String::new()),
            "(the command printed nothing)"
        );
        assert_eq!(explain(Some(1), "boom".into()), "boom");
    }

    #[test]
    fn no_checks_is_not_a_pass() {
        assert!(!all_passed(&[]));
    }
}
