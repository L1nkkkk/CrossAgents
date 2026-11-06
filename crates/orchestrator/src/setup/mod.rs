//! Worktree setup commands (docs/tasks/TOOLCHAIN.md §7): what a project itself runs in a
//! fresh worktree after the hub creates it and before an agent gets it, so that dependencies
//! (`node_modules`, `.venv`) are there.
//!
//! ```toml
//! [run]
//! setup = [
//!     { command = "npm ci", dir = "apps/gui", paths = ["apps/gui/package.json"] },
//! ]
//! ```
//!
//! Dependency directories are not shared between worktrees: what they hold depends on the
//! lock file, the platform and the toolchain's version, and they carry absolute paths inside
//! them, so one worktree's install would change another's while it uses it (§7). Each
//! worktree installs its own; the package manager's download cache lives in the user's home
//! and is shared already, which is what keeps `npm ci` quick.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossagents_core::CheckResult;
use serde::Deserialize;

/// The checks' default when the project's file does not say (`project::RunChecks`).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);
/// What of a command's output is kept (the checks' own limit).
const MAX_OUTPUT: usize = 8_000;
/// What of a failure a note quotes: enough for the first error, not a whole install log.
const NOTE_OUTPUT: usize = 400;

/// One `[run] setup` entry as the project wrote it.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SetupDecl {
    command: String,
    #[serde(default)]
    dir: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
}

/// One setup command, its place (`dir`) and when it is needed (`paths`), checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupCommand {
    pub command: String,
    /// Relative to the checkout; `.` when the project does not say.
    pub dir: PathBuf,
    /// What the worktree has to have for the command to be worth running: at least one of
    /// them. Empty: every worktree.
    pub paths: Vec<String>,
}

/// One checkout's `[run] setup` (docs/tasks/TOOLCHAIN.md §7). No file is none; a file that
/// does not parse is an error, as it is for the checks.
pub fn commands(checkout: &Path) -> Result<Vec<SetupCommand>, String> {
    crate::project::run_setup(checkout)?
        .into_iter()
        .map(SetupCommand::check)
        .collect()
}

impl SetupCommand {
    fn check(decl: SetupDecl) -> Result<Self, String> {
        let command = decl.command.trim().to_owned();
        if command.is_empty() {
            return Err(format!(
                "{} is not valid: a [run] setup entry has no command",
                crate::project::FILE
            ));
        }
        let raw = decl.dir.as_deref().unwrap_or(".").trim();
        if Path::new(raw).is_absolute() {
            return Err(format!(
                "{} is not valid: [run] setup `{command}` has dir = \"{raw}\"; it is relative \
                 to the worktree",
                crate::project::FILE
            ));
        }
        let mut dir = normalize(raw);
        if dir.is_empty() {
            dir = ".".into();
        }
        if dir.split('/').any(|part| part == "..") {
            return Err(format!(
                "{} is not valid: [run] setup `{command}` has dir = \"{raw}\", which leaves \
                 the worktree",
                crate::project::FILE
            ));
        }
        Ok(Self {
            command,
            dir: PathBuf::from(dir),
            paths: decl
                .paths
                .iter()
                .map(|p| normalize(p))
                .filter(|p| !p.is_empty())
                .collect(),
        })
    }

    /// Whether this worktree needs the command: it has at least one of [`Self::paths`] (a
    /// file, or a directory). No paths: every worktree does.
    pub fn applies(&self, checkout: &Path) -> bool {
        self.paths.is_empty() || self.paths.iter().any(|p| checkout.join(p).exists())
    }
}

/// `./crates/` and `crates\` are `crates`, as in the checks.
fn normalize(path: &str) -> String {
    let path = path.trim().replace('\\', "/");
    let path = path.trim_start_matches("./");
    path.trim_matches('/').to_owned()
}

/// What one setup command did in a worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupOutcome {
    /// The command as written, where in the worktree it ran, and how it went.
    pub command: String,
    pub dir: PathBuf,
    pub result: CheckResult,
}

/// What preparing one worktree did: the commands that applied, and why none could be read
/// when the project's file is not valid.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Prepared {
    pub error: Option<String>,
    pub results: Vec<SetupOutcome>,
}

impl Prepared {
    /// What the agent that gets this worktree has to know: the commands that failed, and
    /// why none could run. `None` when everything the project asked for happened: a worktree
    /// that is ready says nothing.
    pub fn note(&self) -> Option<String> {
        let failed: Vec<&SetupOutcome> = self.results.iter().filter(|o| !o.result.passed).collect();
        if self.error.is_none() && failed.is_empty() {
            return None;
        }
        let mut out = String::new();
        if let Some(error) = &self.error {
            let _ = write!(
                out,
                "setup: the hub could not read this worktree's setup commands: {error}"
            );
        }
        for outcome in failed {
            if !out.is_empty() {
                out.push('\n');
            }
            let code = outcome
                .result
                .exit_code
                .map_or_else(|| "no exit code".to_owned(), |c| format!("exit {c}"));
            let _ = write!(
                out,
                "setup: the hub ran `{}` in {} in this worktree before handing it over; it \
                 failed ({code}): {}",
                outcome.command,
                outcome.dir.display(),
                excerpt(&outcome.result.output)
            );
        }
        Some(out)
    }

    /// Whether any command failed or none could be read.
    pub fn failed(&self) -> bool {
        self.error.is_some() || self.results.iter().any(|o| !o.result.passed)
    }
}

/// The first lines of a failing command's output, on one line.
fn excerpt(output: &str) -> String {
    let text = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(3)
        .collect::<Vec<_>>()
        .join(" / ");
    let text = if text.is_empty() {
        "it printed nothing".to_owned()
    } else {
        text
    };
    if text.len() <= NOTE_OUTPUT {
        return text;
    }
    let mut end = NOTE_OUTPUT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Runs the setup commands `checkout` declares that apply to this worktree, in the order the
/// project wrote them, each through the executor and holding the worktree's resources
/// (`verify::run_command`), so it takes turns with anything else working in that checkout.
/// A failure does not stop the rest: each command prepares its own part of the tree. Never
/// fails itself: what went wrong is in the result, for the agent and the task to see.
pub async fn prepare(
    checkout: &Path,
    worktrees: &Path,
    locks: &Path,
    shims: Option<&Path>,
) -> Prepared {
    let commands = match commands(checkout) {
        Ok(commands) => commands,
        Err(error) => {
            tracing::warn!(worktree = %checkout.display(), error = %error, "cannot read the setup commands");
            return Prepared {
                error: Some(error),
                results: Vec::new(),
            };
        }
    };
    let timeout =
        crate::project::run_checks(checkout).map_or(DEFAULT_TIMEOUT, |checks| checks.timeout);
    let mut results = Vec::new();
    for command in commands.iter().filter(|c| c.applies(checkout)) {
        let dir = checkout.join(&command.dir);
        let result = if dir.is_dir() {
            crate::verify::run_command(
                &dir,
                &command.command,
                timeout,
                MAX_OUTPUT,
                worktrees,
                locks,
                shims,
            )
            .await
        } else {
            CheckResult {
                command: command.command.clone(),
                exit_code: None,
                passed: false,
                output: format!("there is no {} in the worktree", command.dir.display()),
            }
        };
        if !result.passed {
            tracing::warn!(
                worktree = %checkout.display(),
                command = %command.command,
                "a setup command failed"
            );
        }
        results.push(SetupOutcome {
            command: command.command.clone(),
            dir: command.dir.clone(),
            result,
        });
    }
    Prepared {
        error: None,
        results,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run worktree as the executor and the environment expect to find it:
    /// `<worktrees>/<run>/<tree>`, with the given `crossagents.toml`.
    fn worktree(root: &Path, tree: &str, file: &str) -> PathBuf {
        let dir = root.join("ab12cd34").join(tree);
        std::fs::create_dir_all(&dir).unwrap();
        if !file.is_empty() {
            std::fs::write(dir.join(crate::project::FILE), file).unwrap();
        }
        dir
    }

    #[test]
    #[ignore = "setup is commented out in crossagents.toml until the hub runs 0.2.6 (docs/TODO.md §二十四)"]
    fn this_repository_installs_its_gui_dependencies_in_every_worktree() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let commands = commands(&root).unwrap();
        let npm = commands
            .iter()
            .find(|c| c.command == "npm ci")
            .unwrap_or_else(|| panic!("{commands:?}"));
        assert_eq!(npm.dir, Path::new("apps/gui"));
        // The GUI is in every worktree of this repository: it always applies.
        assert!(npm.applies(&root));
    }

    #[test]
    fn reads_the_setup_commands() {
        let dir = tempfile::tempdir().unwrap();
        // No file: nothing to run.
        assert!(commands(dir.path()).unwrap().is_empty());

        std::fs::write(
            dir.path().join(crate::project::FILE),
            r#"
[run]
setup = [
    { command = "npm ci", dir = "./apps/gui/", paths = ["apps\\gui\\package.json", ""] },
    { command = " pip install -e . " },
]
"#,
        )
        .unwrap();
        let read = commands(dir.path()).unwrap();
        assert_eq!(read[0].command, "npm ci");
        assert_eq!(read[0].dir, Path::new("apps/gui"));
        assert_eq!(read[0].paths, ["apps/gui/package.json"]);
        // Left out: the checkout itself, and every worktree.
        assert_eq!(read[1].command, "pip install -e .");
        assert_eq!(read[1].dir, Path::new("."));
        assert!(read[1].paths.is_empty());

        // Mistakes are errors, never quietly nothing.
        for text in [
            "[run]\nsetup = [{ command = \"  \" }]\n",
            "[run]\nsetup = [{ dir = \"apps/gui\" }]\n",
            "[run]\nsetup = [{ command = \"npm ci\", path = [\"apps/gui\"] }]\n",
            "[run]\nsetup = [{ command = \"npm ci\", dir = \"../elsewhere\" }]\n",
        ] {
            std::fs::write(dir.path().join(crate::project::FILE), text).unwrap();
            assert!(
                commands(dir.path())
                    .unwrap_err()
                    .contains(crate::project::FILE),
                "{text}"
            );
        }
    }

    #[test]
    fn a_command_with_paths_runs_only_where_the_worktree_has_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("apps/gui")).unwrap();
        let command = SetupCommand {
            command: "npm ci".into(),
            dir: Path::new("apps/gui").into(),
            paths: vec!["apps/gui/package.json".into()],
        };
        // The directory alone is not the file the project named.
        assert!(!command.applies(dir.path()));
        std::fs::write(dir.path().join("apps/gui/package.json"), "{}").unwrap();
        assert!(command.applies(dir.path()));

        // A listed directory counts when it is there.
        let dirs = SetupCommand {
            paths: vec!["apps/gui".into()],
            ..command.clone()
        };
        assert!(dirs.applies(dir.path()));

        // No paths: every worktree, even one without the directory.
        let anywhere = SetupCommand {
            paths: Vec::new(),
            ..command
        };
        let empty = tempfile::tempdir().unwrap();
        assert!(anywhere.applies(empty.path()));
    }

    #[tokio::test]
    async fn runs_what_applies_and_says_what_failed() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("worktrees");
        let checkout = worktree(
            &root,
            "integration",
            r#"
[run]
setup = [
    { command = "echo prepared > prepared.txt", dir = "." },
    { command = "exit 3" },
    { command = "echo not-for-this-one > skipped.txt", paths = ["nope/package.json"] },
]
"#,
        );
        let prepared = prepare(&checkout, &root, &home.path().join("locks"), None).await;
        // The one whose paths are not there never ran; the others did, in order.
        assert!(prepared.results.len() == 2, "{:?}", prepared.results);
        assert!(prepared.results[0].result.passed);
        assert!(
            checkout.join("prepared.txt").exists(),
            "the first never ran"
        );
        assert!(!prepared.results[1].result.passed);
        assert_eq!(prepared.results[1].result.exit_code, Some(3));
        assert!(
            !checkout.join("skipped.txt").exists(),
            "a filtered command ran"
        );

        let note = prepared.note().expect("a failure says nothing");
        assert!(note.contains("exit 3"), "{note}");
        assert!(
            !note.contains("prepared"),
            "a passing command is noted: {note}"
        );
        assert!(prepared.failed());
    }

    /// Setup runs the way the hub's checks do: with the worktree's environment, through the
    /// executor, holding the worktree's resources and waiting for whoever holds them
    /// (docs/tasks/LOCKS.md §6).
    #[tokio::test]
    async fn runs_through_the_executor_with_the_worktrees_resources() {
        use crossagents_core::exec;

        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("worktrees");
        let print = if cfg!(windows) {
            "echo %CROSSAGENTS_RESOURCES%"
        } else {
            "echo $CROSSAGENTS_RESOURCES"
        };
        let checkout = worktree(
            &root,
            "integration",
            &format!("[run]\nsetup = [{{ command = \"{print} > resources.txt\" }}]\n"),
        );
        // A Rust project: its preset declares the build directory the command holds.
        std::fs::write(checkout.join("Cargo.toml"), "").unwrap();
        let build = crate::workspace::target_dir(
            &dunce::canonicalize(&root).unwrap(),
            "ab12cd34",
            "integration",
        )
        .join("cargo");
        std::fs::create_dir_all(&build).unwrap();

        // Somebody else builds in this checkout: the setup waits, it does not run anyway.
        let locks = home.path().join("locks");
        let request = exec::Request::new(
            "cargo test --workspace",
            vec![exec::Resource::new(
                "build",
                &build,
                exec::Access::Exclusive,
            )],
        );
        let held = exec::acquire(&locks, &request, |_| {}).await.unwrap();
        let (root_in_background, checkout_in_background, locks_in_background) =
            (root.clone(), checkout.clone(), locks.clone());
        let mut waiting = tokio::spawn(async move {
            prepare(
                &checkout_in_background,
                &root_in_background,
                &locks_in_background,
                None,
            )
            .await
        });
        assert!(
            tokio::time::timeout(Duration::from_secs(2), &mut waiting)
                .await
                .is_err(),
            "the setup did not wait for the build directory"
        );
        assert!(!checkout.join("resources.txt").exists());
        drop(held);

        let prepared = waiting.await.unwrap();
        let result = &prepared.results[0].result;
        assert!(result.passed, "{result:?}");
        // The command got the same resources at the same path as a check there.
        let text = std::fs::read_to_string(checkout.join("resources.txt")).unwrap();
        let resources: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(resources[0]["name"], "build");
        assert_eq!(resources[0]["path"], build.display().to_string());
    }

    #[tokio::test]
    async fn a_command_that_cannot_run_says_where_it_was_meant_to() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("worktrees");
        let checkout = worktree(
            &root,
            "auth",
            "[run]\nsetup = [{ command = \"npm ci\", dir = \"apps/gui\" }]\n",
        );
        let prepared = prepare(&checkout, &root, &home.path().join("locks"), None).await;
        let note = prepared.note().expect("a missing directory says nothing");
        assert!(note.contains("apps/gui"), "{note}");
        assert!(note.contains("there is no"), "{note}");
    }

    #[tokio::test]
    async fn a_file_that_does_not_parse_is_reported_not_skipped() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("worktrees");
        let checkout = worktree(
            &root,
            "auth",
            "[run]\nsetup = [{ command = \"x\", pths = [] }]\n",
        );
        let prepared = prepare(&checkout, &root, &home.path().join("locks"), None).await;
        assert!(prepared.failed());
        let note = prepared.note().expect("an invalid file says nothing");
        assert!(note.contains(crate::project::FILE), "{note}");
    }

    #[tokio::test]
    async fn a_worktree_with_nothing_to_do_is_silent() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("worktrees");
        let checkout = worktree(
            &root,
            "integration",
            "[run]\nsetup = [{ command = \"echo ok\", paths = [\"apps/gui/package.json\"] }]\n",
        );
        std::fs::create_dir_all(checkout.join("apps/gui")).unwrap();
        std::fs::write(checkout.join("apps/gui/package.json"), "{}").unwrap();
        let prepared = prepare(&checkout, &root, &home.path().join("locks"), None).await;
        assert!(prepared.results[0].result.passed);
        assert_eq!(prepared.note(), None);
        assert!(!prepared.failed());
    }

    #[test]
    fn a_failure_note_keeps_the_first_lines() {
        assert_eq!(excerpt(""), "it printed nothing");
        assert_eq!(
            excerpt("\n\nnpm ERR! code EUSAGE\nnpm ERR! missing\nthird\nfourth"),
            "npm ERR! code EUSAGE / npm ERR! missing / third"
        );
        let long = "x".repeat(NOTE_OUTPUT + 10);
        let clipped = excerpt(&long);
        assert!(clipped.ends_with('…'));
        assert_eq!(clipped.len(), NOTE_OUTPUT + '…'.len_utf8());
    }
}
