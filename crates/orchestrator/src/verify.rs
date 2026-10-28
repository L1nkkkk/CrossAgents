//! Acceptance checks, run by the hub itself (docs/DESIGN.md §8 Verify).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use crossagents_core::CheckResult;
use crossagents_core::exec;
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
///
/// In a run's worktree the whole group holds the checkout's resources first
/// (docs/tasks/LOCKS.md §6): a `cargo test` an agent started there and the hub's checks take
/// turns, by waiting for the lock rather than by failing.
#[derive(Debug, Clone)]
pub struct ShellVerifier {
    pub timeout: Duration,
    pub max_output: usize,
    /// Where runs' worktrees live: checks in one get the environment the run's agents there
    /// do (`workspace::worktree_env`), so they build in the same directory.
    pub worktrees: Option<PathBuf>,
    /// Where the executor's lock files live (`<home>/locks`, docs/tasks/LOCKS.md §4.1).
    /// `None` runs the checks without holding anything: for a directory outside the
    /// worktrees, or a check that declares no resources, that is what happens anyway.
    pub locks: Option<PathBuf>,
}

impl Default for ShellVerifier {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(600),
            max_output: 8_000,
            worktrees: None,
            locks: None,
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

/// Waits for a command's whole tree to end, and ends what is left of it once `deadline` is up:
/// the build directory is let go only then (docs/tasks/LOCKS.md §4.3).
async fn end_tree(tree: &mut process::Tree, deadline: tokio::time::Instant) {
    if tokio::time::timeout_at(deadline, exec::wait_tree_empty(tree))
        .await
        .is_ok()
    {
        return;
    }
    tree.kill();
    // The platform ends them with it; look once more rather than take that on trust.
    let _ = tokio::time::timeout(Duration::from_secs(5), exec::wait_tree_empty(tree)).await;
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
        let commands: Vec<&String> = commands.iter().filter(|c| !c.trim().is_empty()).collect();
        if commands.is_empty() {
            return Vec::new();
        }
        // One acquisition for the whole group: the checks in a checkout take turns with
        // whatever else works there (an agent's `crossagents exec`, another check), not with
        // each other (docs/tasks/LOCKS.md §6).
        let request = exec::Request {
            resources: self.resources_in(dir),
            command: commands
                .iter()
                .map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join(" && "),
            agent: Some("the hub".to_owned()),
            run: None,
            timeout,
            notice_every: exec::DEFAULT_NOTICE_EVERY,
        };
        let mut held = match (&self.locks, request.resources.is_empty()) {
            (Some(locks), false) => match exec::acquire(locks, &request, |line| {
                // Somebody else builds in this checkout: that is a wait, not a failure.
                tracing::info!(target: "crossagents::locks", "{line}");
            })
            .await
            {
                Ok(held) => Some(held),
                Err(error) => return vec![without_lock(&request, error)],
            },
            _ => None,
        };
        let mut results = Vec::new();
        for command in commands {
            let mut cmd = shell(command);
            if let Some(root) = self.worktrees.as_deref() {
                cmd.envs(worktree_env(root, dir));
            }
            cmd.current_dir(dir)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            // Under the locks the tree is the executor's (`exec::spawn`), which refuses to run
            // a command it cannot follow to its end, since the lock is let go only then.
            // Without resources the whole tree still goes when the check ends, times out or
            // fails: killing the shell alone left what it started still writing to the
            // worktree.
            let started = match held.as_mut() {
                Some(held) => exec::spawn(held, &mut cmd),
                None => process::spawn_tree(&mut cmd),
            };
            let result = match started {
                Err(e) => CheckResult {
                    command: command.clone(),
                    exit_code: None,
                    passed: false,
                    output: format!("cannot start: {e}"),
                },
                Ok((child, mut tree)) => {
                    let deadline = tokio::time::Instant::now() + timeout;
                    let result =
                        match tokio::time::timeout_at(deadline, child.wait_with_output()).await {
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
                        };
                    // The command's own process is done; what it started runs on. The lock
                    // goes only once the whole tree is gone (LOCKS.md §4.3), so the next
                    // command in the group and whoever gets the build directory next do not
                    // find a compiler of this check still writing there.
                    end_tree(&mut tree, deadline).await;
                    result
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

impl ShellVerifier {
    /// The resources a check in `dir` holds (`docs/tasks/LOCKS.md` §6): those its worktree
    /// declares, at the same paths its commands get from [`worktree_env`], so a check and an
    /// agent's `exec` ask for the same lock. Empty outside the worktrees, or for a checkout
    /// that declares none: such a check runs as it did before.
    fn resources_in(&self, dir: &Path) -> Vec<exec::Resource> {
        let Some(root) = self.worktrees.as_deref() else {
            return Vec::new();
        };
        let Some(env) = crate::workspace::worktree_run_env(root, dir) else {
            return Vec::new();
        };
        env.resources
            .iter()
            .map(|resource| {
                let access = match resource.access {
                    crate::project::ResourceAccess::Shared => exec::Access::Shared,
                    crate::project::ResourceAccess::Exclusive => exec::Access::Exclusive,
                };
                exec::Resource::new(resource.name.clone(), resource.path.clone(), access)
            })
            .collect()
    }
}

/// The group could not get the checkout's resources, so no command ran. A timeout says who
/// held them: waiting for another build is not a failure of the change being checked.
fn without_lock(request: &exec::Request, error: exec::ExecError) -> CheckResult {
    let (exit_code, output) = match error {
        error @ exec::ExecError::Timeout { .. } => (
            Some(exec::EXIT_TEMPFAIL),
            format!(
                "none of the checks ran: {error}. This is not a problem in the change; wait for \
                 it to finish, then run the checks again."
            ),
        ),
        error => (
            None,
            format!("cannot take the checkout's resources (docs/tasks/LOCKS.md): {error}"),
        ),
    };
    CheckResult {
        command: request.command.clone(),
        exit_code,
        passed: false,
        output,
    }
}

/// One command in `dir` with its own time limit (`workspace_run`), building in the run's
/// shared target when `dir` is under `worktrees` and holding its resources (LOCKS.md §6).
/// Never fails: problems are in the result.
pub async fn run_command(
    dir: &Path,
    command: &str,
    timeout: Duration,
    max_output: usize,
    worktrees: &Path,
    locks: &Path,
) -> CheckResult {
    let verifier = ShellVerifier {
        timeout,
        max_output,
        worktrees: Some(worktrees.to_path_buf()),
        locks: Some(locks.to_path_buf()),
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
        // A check holds the same build directory the agent builds into (LOCKS §3).
        let echo_resources = if cfg!(windows) {
            "echo %CROSSAGENTS_RESOURCES%"
        } else {
            "echo $CROSSAGENTS_RESOURCES"
        };
        let out = verifier.check(&dir, &[echo_resources.into()]).await;
        let resources: serde_json::Value = serde_json::from_str(&out[0].output).unwrap();
        assert_eq!(resources[0]["name"], "build");
        assert_eq!(resources[0]["access"], "exclusive");
        assert_eq!(resources[0]["path"], want.display().to_string());
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
            locks: None,
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

    /// A run's worktree with a `Cargo.toml` (so the Rust preset declares `build`), the build
    /// directory a check there holds, and a verifier that holds it
    /// (docs/tasks/LOCKS.md §6).
    fn worktree(home: &tempfile::TempDir, tree: &str) -> (PathBuf, PathBuf, ShellVerifier) {
        let root = home.path().join("worktrees");
        let dir = root.join("ab12cd34").join(tree);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        let verifier = ShellVerifier {
            timeout: Duration::from_secs(30),
            worktrees: Some(root.clone()),
            locks: Some(home.path().join("locks")),
            ..ShellVerifier::default()
        };
        // The same path the check's commands get from their environment.
        let env = crate::workspace::worktree_run_env(&root, &dir).unwrap();
        let build = PathBuf::from(&env.resources[0].path);
        assert_eq!(env.resources[0].name, "build");
        (dir, build, verifier)
    }

    /// Holds `path` the way `crossagents exec --resource build -- …` does.
    async fn hold(home: &tempfile::TempDir, path: &Path) -> exec::Held {
        let request = exec::Request::new(
            "cargo test --workspace",
            vec![exec::Resource::new("build", path, exec::Access::Exclusive)],
        );
        exec::acquire(&home.path().join("locks"), &request, |_| {})
            .await
            .expect("take the build directory")
    }

    #[tokio::test]
    async fn a_check_waits_for_a_build_another_executor_holds() {
        let home = tempfile::tempdir().unwrap();
        let (dir, build, verifier) = worktree(&home, "integration");
        std::fs::create_dir_all(&build).unwrap();
        let held = hold(&home, &build).await;

        let verifier_in_background = verifier.clone();
        let dir_in_background = dir.clone();
        let mut check = tokio::spawn(async move {
            verifier_in_background
                .check(&dir_in_background, &["echo ran > ran.txt".into()])
                .await
        });
        // Far longer than the command takes: had it not waited, it would be done.
        assert!(
            tokio::time::timeout(Duration::from_secs(2), &mut check)
                .await
                .is_err(),
            "the check did not wait for the build directory"
        );
        assert!(!dir.join("ran.txt").exists());

        drop(held);
        let out = check.await.unwrap();
        assert!(out[0].passed, "{out:?}");
        assert!(dir.join("ran.txt").exists(), "the check never ran");
    }

    #[tokio::test]
    async fn a_check_that_could_not_get_the_build_directory_says_who_held_it() {
        let home = tempfile::tempdir().unwrap();
        let (dir, build, verifier) = worktree(&home, "integration");
        std::fs::create_dir_all(&build).unwrap();
        let held = hold(&home, &build).await;

        let short = ShellVerifier {
            timeout: Duration::from_millis(300),
            ..verifier
        };
        let out = short.check(&dir, &["echo ran > ran.txt".into()]).await;
        assert!(!out[0].passed);
        // Not the command failing: the CLI's "could not get the lock" code.
        assert_eq!(out[0].exit_code, Some(exec::EXIT_TEMPFAIL), "{out:?}");
        assert!(out[0].output.contains("waiting for build"), "{out:?}");
        assert!(out[0].output.contains("cargo test --workspace"), "{out:?}");
        assert!(out[0].output.contains("not a problem"), "{out:?}");
        assert!(!dir.join("ran.txt").exists(), "the check ran anyway");
        drop(held);
    }

    #[tokio::test]
    async fn two_checks_in_one_checkout_take_turns() {
        let home = tempfile::tempdir().unwrap();
        let (dir, _build, verifier) = worktree(&home, "integration");
        // Each check writes its start and its end, so the log shows whether they overlapped.
        let command = if cfg!(windows) {
            r#"powershell -NoProfile -Command "Add-Content -Path log.txt -Value ('start ' + [DateTime]::UtcNow.Ticks); Start-Sleep -Milliseconds 300; Add-Content -Path log.txt -Value ('end ' + [DateTime]::UtcNow.Ticks)""#
        } else {
            "sh -c 'echo start $(date +%s%N) >> log.txt; sleep 0.3; echo end $(date +%s%N) >> log.txt'"
        };
        let (first, dir_one, command_one) = (verifier.clone(), dir.clone(), command.to_owned());
        let (second, dir_two, command_two) = (verifier.clone(), dir.clone(), command.to_owned());
        let (one, two) = tokio::join!(
            async move { first.check(&dir_one, &[command_one]).await },
            async move { second.check(&dir_two, &[command_two]).await },
        );
        assert!(one[0].passed, "{one:?}");
        assert!(two[0].passed, "{two:?}");

        let log = std::fs::read_to_string(dir.join("log.txt")).unwrap();
        let lines: Vec<&str> = log.lines().map(str::trim).collect();
        let phases: Vec<&str> = lines
            .iter()
            .map(|line| line.split_whitespace().next().unwrap())
            .collect();
        assert_eq!(
            phases,
            ["start", "end", "start", "end"],
            "the checks overlapped: {log}"
        );
    }

    #[tokio::test]
    async fn another_worktree_s_build_directory_does_not_hold_a_check_up() {
        let home = tempfile::tempdir().unwrap();
        let (_one_dir, one_build, _one) = worktree(&home, "integration");
        let (two_dir, _two_build, two) = worktree(&home, "auth");
        std::fs::create_dir_all(&one_build).unwrap();
        let held = hold(&home, &one_build).await;

        let started = std::time::Instant::now();
        let out = two.check(&two_dir, &["echo ok".into()]).await;
        assert!(out[0].passed, "{out:?}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "another worktree's lock held it up for {:?}",
            started.elapsed()
        );
        drop(held);
    }
}
