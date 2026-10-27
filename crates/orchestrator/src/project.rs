//! What a project declares for the hub (docs/tasks/DOGFOOD.md §9): `crossagents.toml` at the
//! root of the checkout.
//!
//! ```toml
//! [run]
//! # Run by the hub in the run's checkout before the lead lands or finishes a run that
//! # changed something; all must pass. A check with `paths` runs only when the run changed
//! # one of them: a file, or a directory and everything under it.
//! checks = [
//!     "npm run lint",
//!     { command = "cargo test --workspace", paths = ["crates", "Cargo.toml", "Cargo.lock"] },
//! ]
//! timeout_secs = 1800   # per command; default 600
//! ```

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

pub const FILE: &str = "crossagents.toml";

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

/// The checks every run of the project must pass; none when the project declares none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunChecks {
    pub checks: Vec<RunCheck>,
    pub timeout: Duration,
}

/// One command, and the paths that call for it (empty: every change does).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunCheck {
    pub command: String,
    pub paths: Vec<String>,
}

impl RunCheck {
    /// Whether a run that changed `changed` (paths relative to the checkout, `/`-separated)
    /// needs this check.
    pub fn applies_to(&self, changed: &[String]) -> bool {
        self.paths.is_empty()
            || changed.iter().any(|file| {
                self.paths.iter().any(|p| {
                    file == p
                        || file
                            .strip_prefix(p.as_str())
                            .is_some_and(|r| r.starts_with('/'))
                })
            })
    }
}

impl RunChecks {
    /// The commands a run that changed `changed` must pass; all of them when what changed is
    /// not known (empty), so that a failure to list the changes does not skip a check.
    pub fn commands_for(&self, changed: &[String]) -> Vec<String> {
        self.checks
            .iter()
            .filter(|c| changed.is_empty() || c.applies_to(changed))
            .map(|c| c.command.clone())
            .collect()
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    run: Run,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Run {
    #[serde(default)]
    checks: Vec<Check>,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Check {
    Command(String),
    Scoped(Scoped),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Scoped {
    command: String,
    paths: Vec<String>,
}

/// `./crates/` and `crates\` are `crates`.
fn normalize(path: &str) -> String {
    let path = path.trim().replace('\\', "/");
    let path = path.trim_start_matches("./");
    path.trim_matches('/').to_owned()
}

/// The run checks `dir` declares. No file is no checks; a file that does not parse is an
/// error, so that a typo does not quietly turn the checks off.
pub fn run_checks(dir: &Path) -> Result<RunChecks, String> {
    let text = match std::fs::read_to_string(dir.join(FILE)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read {FILE}: {e}")),
    };
    let file: File = toml::from_str(&text).map_err(|e| format!("{FILE} is not valid: {e}"))?;
    let mut checks = Vec::new();
    for check in file.run.checks {
        let (command, paths) = match check {
            Check::Command(command) => (command, Vec::new()),
            Check::Scoped(s) => {
                let paths: Vec<String> = s
                    .paths
                    .iter()
                    .map(|p| normalize(p))
                    .filter(|p| !p.is_empty())
                    .collect();
                // An empty list would quietly mean "always": say so instead.
                if paths.is_empty() {
                    return Err(format!(
                        "{FILE} is not valid: `{}` has no paths; leave `paths` out to run it on every change",
                        s.command.trim()
                    ));
                }
                (s.command, paths)
            }
        };
        let command = command.trim().to_owned();
        if !command.is_empty() {
            checks.push(RunCheck { command, paths });
        }
    }
    Ok(RunChecks {
        checks,
        timeout: file
            .run
            .timeout_secs
            .map_or(DEFAULT_TIMEOUT, Duration::from_secs),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(files: &[&str]) -> Vec<String> {
        files.iter().map(|f| (*f).to_owned()).collect()
    }

    #[test]
    fn reads_the_run_checks() {
        let dir = tempfile::tempdir().unwrap();
        assert!(run_checks(dir.path()).unwrap().checks.is_empty());

        let file = dir.path().join(FILE);
        std::fs::write(
            &file,
            "[run]\nchecks = [\"cargo test\", \" \"]\ntimeout_secs = 5\n",
        )
        .unwrap();
        let checks = run_checks(dir.path()).unwrap();
        assert_eq!(checks.commands_for(&[]), ["cargo test"]);
        assert_eq!(checks.timeout, Duration::from_secs(5));

        // A misspelt key is an error, not "no checks".
        std::fs::write(&file, "[run]\ncheck = [\"cargo test\"]\n").unwrap();
        assert!(run_checks(dir.path()).unwrap_err().contains(FILE));
    }

    #[test]
    fn a_check_with_paths_runs_only_when_one_of_them_changed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE);
        std::fs::write(
            &file,
            r#"[run]
checks = [
    "npm run lint",
    { command = "cargo test", paths = ["./crates/", "Cargo.toml"] },
]
"#,
        )
        .unwrap();
        let checks = run_checks(dir.path()).unwrap();
        assert_eq!(checks.checks[1].paths, ["crates", "Cargo.toml"]);

        // Only the GUI changed: no cargo.
        assert_eq!(
            checks.commands_for(&changed(&["apps/gui/src/App.tsx"])),
            ["npm run lint"]
        );
        // A file under a listed directory, or a listed file itself.
        for files in [&["crates/hub/src/api.rs"][..], &["Cargo.toml"]] {
            assert_eq!(
                checks.commands_for(&changed(files)),
                ["npm run lint", "cargo test"]
            );
        }
        // A name that only starts like a listed one is not under it.
        assert_eq!(
            checks.commands_for(&changed(&["crates-old/x.rs", "Cargo.toml.bak"])),
            ["npm run lint"]
        );
        // Not knowing what changed runs everything.
        assert_eq!(checks.commands_for(&[]), ["npm run lint", "cargo test"]);

        // Empty paths would mean "always" by accident.
        std::fs::write(
            &file,
            "[run]\nchecks = [{ command = \"cargo test\", paths = [] }]\n",
        )
        .unwrap();
        assert!(run_checks(dir.path()).unwrap_err().contains("no paths"));
        // So would a misspelt `paths`.
        std::fs::write(
            &file,
            "[run]\nchecks = [{ command = \"cargo test\", path = [\"crates\"] }]\n",
        )
        .unwrap();
        assert!(run_checks(dir.path()).is_err());
    }

    #[test]
    fn this_repository_runs_cargo_only_for_rust_changes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let checks = run_checks(&root).unwrap();
        let cargo = ["cargo test --workspace"];
        assert_eq!(
            checks.commands_for(&changed(&["crates/hub/src/api.rs"])),
            cargo
        );
        assert_eq!(checks.commands_for(&changed(&["Cargo.lock"])), cargo);
        assert!(
            checks
                .commands_for(&changed(&["apps/gui/src/styles.css", "docs/DESIGN.md"]))
                .is_empty()
        );
    }
}
