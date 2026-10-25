//! Acceptance checks, run by the hub itself (docs/DESIGN.md §8 Verify).

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use crossagents_core::CheckResult;
use crossagents_core::process::NoWindow;
use tokio::process::Command;

/// Decides whether a task's result is acceptable.
#[async_trait]
pub trait Verifier: Send + Sync {
    async fn check(&self, dir: &Path, commands: &[String]) -> Vec<CheckResult>;
}

/// Runs each acceptance command in the system shell; all must exit 0. Stops at the first
/// failure, since later checks usually depend on earlier ones.
#[derive(Debug, Clone)]
pub struct ShellVerifier {
    pub timeout: Duration,
    pub max_output: usize,
}

impl Default for ShellVerifier {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(600),
            max_output: 8_000,
        }
    }
}

fn shell(command: &str) -> Command {
    if cfg!(windows) {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(command).no_window();
        cmd
    } else {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    }
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
        let mut results = Vec::new();
        for command in commands.iter().filter(|c| !c.trim().is_empty()) {
            let mut cmd = shell(command);
            cmd.current_dir(dir)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let result = match cmd.spawn() {
                Err(e) => CheckResult {
                    command: command.clone(),
                    exit_code: None,
                    passed: false,
                    output: format!("cannot start: {e}"),
                },
                Ok(child) => {
                    match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
                        Err(_) => CheckResult {
                            command: command.clone(),
                            exit_code: None,
                            passed: false,
                            output: format!("timed out after {}s", self.timeout.as_secs()),
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

/// One command in `dir` with its own time limit (`workspace_run`). Never fails: problems
/// are in the result.
pub async fn run_command(
    dir: &Path,
    command: &str,
    timeout: Duration,
    max_output: usize,
) -> CheckResult {
    let verifier = ShellVerifier {
        timeout,
        max_output,
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
    async fn timeout_fails() {
        let verifier = ShellVerifier {
            timeout: Duration::from_millis(200),
            max_output: 100,
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
