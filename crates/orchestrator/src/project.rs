//! What a project declares for the hub (docs/tasks/DOGFOOD.md §9): `crossagents.toml` at the
//! root of the checkout.
//!
//! ```toml
//! [run]
//! # Run by the hub in the run's checkout before the lead lands or finishes a run that
//! # changed something; all must pass.
//! checks = ["cargo test --workspace"]
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
    pub commands: Vec<String>,
    pub timeout: Duration,
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
    checks: Vec<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
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
    Ok(RunChecks {
        commands: file
            .run
            .checks
            .into_iter()
            .map(|c| c.trim().to_owned())
            .filter(|c| !c.is_empty())
            .collect(),
        timeout: file
            .run
            .timeout_secs
            .map_or(DEFAULT_TIMEOUT, Duration::from_secs),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_run_checks() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            run_checks(dir.path()).unwrap().commands,
            Vec::<String>::new()
        );

        let file = dir.path().join(FILE);
        std::fs::write(
            &file,
            "[run]\nchecks = [\"cargo test\", \" \"]\ntimeout_secs = 5\n",
        )
        .unwrap();
        let checks = run_checks(dir.path()).unwrap();
        assert_eq!(checks.commands, ["cargo test"]);
        assert_eq!(checks.timeout, Duration::from_secs(5));

        // A misspelt key is an error, not "no checks".
        std::fs::write(&file, "[run]\ncheck = [\"cargo test\"]\n").unwrap();
        assert!(run_checks(dir.path()).unwrap_err().contains(FILE));
    }
}
