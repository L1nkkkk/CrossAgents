//! What an in-place run dispatched from where: its snapshot and the hub-owned integration
//! workspace workers' results gather in (docs/tasks/LEAD.md §5, §10).
//!
//! A file beside the run's worktrees, not in the run record: it is the hub's working state
//! for one checkout, and a hub that restarts reads it back as it was. Everything is written
//! whole (replace, then rename), so a crash leaves the last good record.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use crossagents_core::{CoreError, RunId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct State {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Snapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration: Option<Integration>,
}

/// The dispatch snapshot workers started from (docs/tasks/LEAD.md §5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    pub commit: String,
    pub tree: String,
    pub number: u32,
    pub head: String,
    pub at: DateTime<Utc>,
}

/// The hub-owned working copy that gathers accepted results before they are applied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Integration {
    pub branch: String,
    pub path: PathBuf,
    pub at: DateTime<Utc>,
}

pub(crate) fn read(path: &Path) -> Result<State, CoreError> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|err| {
            CoreError::Conflict(format!(
                "cannot read the in-place record {}: {err}",
                path.display()
            ))
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
        Err(err) => Err(CoreError::Storage(format!(
            "cannot read {}: {err}",
            path.display()
        ))),
    }
}

pub(crate) fn write(path: &Path, state: &State) -> Result<(), CoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| CoreError::Storage(err.to_string()))?;
    }
    let text = serde_json::to_string_pretty(state)
        .map_err(|err| CoreError::Storage(format!("cannot write the in-place record: {err}")))?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, text)
        .map_err(|err| CoreError::Storage(format!("cannot write {}: {err}", temp.display())))?;
    if std::fs::rename(&temp, path).is_err() {
        let _ = std::fs::remove_file(path);
        std::fs::rename(&temp, path)
            .map_err(|err| CoreError::Storage(format!("cannot write {}: {err}", path.display())))?;
    }
    Ok(())
}
