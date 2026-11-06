//! One run at a time per checkout (docs/tasks/LEAD.md §7; docs/research/multica.md §2.6).
//!
//! A run whose lead works in the person's own directory writes there: the lead's own shell,
//! the hub's checks, and (from P3) what workers' results are applied back as. No lock of the
//! hub's can stop a shell, so what it keeps out is another *run* in the same checkout, and
//! landing into it. The occupancy is read off the runs themselves — an open run that has its
//! lead working in place holds it — so a hub that restarts still knows, and a run that
//! finishes, fails or is cancelled lets go by being terminal. That release is the point: an
//! ended run must not keep the person out of their own directory.

use std::path::{Path, PathBuf};

use crossagents_core::CoreError;
use crossagents_core::process::NoWindow;
use crossagents_core::{Run, RunId};

use crate::engine::Orchestrator;

/// The word for "this run waits for a directory another run is working in" (multica §2.6).
pub const WAITING: &str = "waiting_local_directory";

/// A path as one key: normalized, so the same directory spelled two ways (a trailing
/// separator, a `.` inside it, another case on Windows) is one directory.
pub fn key(dir: &Path) -> String {
    let path = dunce::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut key = path.to_string_lossy().replace('\\', "/");
    while key.len() > 1 && key.ends_with('/') {
        key.pop();
    }
    if cfg!(windows) {
        key = key.to_lowercase();
    }
    key
}

/// The checkout `dir` is in, as one key: the top of its repository, since every directory of
/// one working copy is the same checkout to write in; else `dir` itself.
pub async fn checkout_key(dir: &Path) -> String {
    let out = tokio::process::Command::new("git")
        .no_window()
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .output()
        .await;
    let root = out
        .ok()
        .filter(|out| out.status.success())
        .map(|out| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_owned()))
        .filter(|root| root.is_absolute());
    key(&root.unwrap_or_else(|| dir.to_path_buf()))
}

/// Why a run waits for `dir`, in words that start with [`WAITING`] so a client can show the
/// state by that word alone.
pub fn busy(holder: &Run, dir: &Path) -> String {
    format!(
        "{WAITING}: run \"{}\" ({}) is working in {}; a checkout takes one run whose lead works \
         in it at a time. Finish that run first, or start this one in a checkout of its own",
        holder.title,
        holder.id,
        dir.display()
    )
}

impl Orchestrator {
    /// The open run whose lead works in `dir`'s checkout, if any: the one holding it.
    pub(crate) async fn dir_holder(&self, dir: &Path) -> Result<Option<Run>, CoreError> {
        let wanted = checkout_key(dir).await;
        for run in self.store.list_runs().await? {
            if run.status.is_terminal() || !run.leads_in_place() {
                continue;
            }
            if checkout_key(&run.repo).await == wanted {
                return Ok(Some(run));
            }
        }
        Ok(None)
    }

    /// Says whether a run may take `dir` now: free, or held by `me` (a round of the same
    /// run being written). Call it with the checkout's gate held ([`Self::dir_gate`]) so
    /// that two creations cannot both find it free.
    pub(crate) async fn dir_free(&self, dir: &Path, me: Option<RunId>) -> Result<(), CoreError> {
        match self.dir_holder(dir).await? {
            Some(holder) if Some(holder.id) != me => Err(CoreError::Conflict(busy(&holder, dir))),
            _ => Ok(()),
        }
    }

    /// The lock taken while a checkout's occupancy is read and the run that takes it is
    /// recorded. A card lock by the checkout's key: one holder, waiters queue, and the entry
    /// goes when nobody holds it.
    pub(crate) async fn dir_gate(&self, dir: &Path) -> crate::engine::CardGuard<'_> {
        self.lock_card(&format!("dir:{}", checkout_key(dir).await))
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn the_same_checkout_spelled_differently_is_one_key() {
        let dir = tempfile::tempdir().unwrap();
        let plain = key(dir.path());
        assert_eq!(plain, key(&dir.path().join(".")));
        assert_eq!(
            plain,
            key(&PathBuf::from(format!("{}/", dir.path().display())))
        );
        #[cfg(windows)]
        assert_eq!(
            plain,
            key(&PathBuf::from(dir.path().to_string_lossy().to_uppercase()))
        );
    }

    #[tokio::test]
    async fn every_directory_of_one_checkout_is_that_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let out = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        assert_eq!(
            checkout_key(dir.path()).await,
            checkout_key(&dir.path().join("sub")).await,
            "a run in a subdirectory writes in the same working copy"
        );

        // A directory with no repository above it is only itself.
        let plain = tempfile::tempdir().unwrap();
        assert_eq!(checkout_key(plain.path()).await, key(plain.path()));
    }

    #[test]
    fn a_directory_answers_by_the_word_a_person_sees() {
        let now = chrono::Utc::now();
        let holder: Run = serde_json::from_value(serde_json::json!({
            "id": crossagents_core::RunId::new(), "title": "Lead's run", "request": "r",
            "planner": crossagents_core::AgentId::new(), "status": "running",
            "config": {
                "worker": crossagents_core::AgentId::new(),
                "max_parallel": 1, "max_attempts": 1,
            },
            "repo": "C:/work", "base_branch": "main", "branch": "",
            "created_at": now, "updated_at": now,
        }))
        .unwrap();
        let why = busy(&holder, std::path::Path::new("C:/work"));
        assert!(why.starts_with(WAITING), "{why}");
        assert!(why.contains("Lead's run"), "{why}");
        assert!(why.contains("C:/work"), "{why}");
    }
}
