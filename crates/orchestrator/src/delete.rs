//! Deleting a run (docs/tasks/R1.md §3): its row and tasks, the working copies it used and,
//! when the person says so, its branches. A hard delete — runs do not come back — so only a
//! run that ended can go, and a checkout with work that never landed asks first. Sessions
//! are not the run's (docs/tasks/S1.md §3.4): what it still holds is let go, nothing is
//! deleted. Events stay: the log only grows, and the trace outlives the run.

use std::path::Path;

use crossagents_core::{CoreError, EventKind, Run, RunId};
use serde::Serialize;

use crate::claims::LetGo;
use crate::engine::{Orchestrator, short};

/// What to clean up along with a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeleteRun {
    /// Delete the run's branches too. Left out, a landed run's branches go (their work is on
    /// the base branch) and an unlanded run's stay (its changes exist nowhere else).
    pub branches: bool,
    /// Delete even when the run's checkout has uncommitted changes.
    pub force: bool,
}

/// What a delete cleaned up, and what it could not. A step that fails is a warning: the run
/// itself goes all the same, as `cancel` does.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DeletedRun {
    /// The sessions the run still held, by name: none is deleted.
    pub agents: DeletedRunAgents,
    /// The directories that went: the run's checkout, its tasks', and the build directory
    /// they shared.
    pub worktrees: Vec<String>,
    pub branches: Vec<String>,
    /// What could not be cleaned up, and why.
    pub warnings: Vec<String>,
}

/// What became of the sessions a deleted run still held (docs/tasks/S1.md §3.4). Those that
/// took part in it and are no longer its (gone home, archived, or working for another run)
/// are not listed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DeletedRunAgents {
    /// Agents working for it themselves, back in their workspace.
    pub returned: Vec<String>,
    /// Execution sessions it started (a lead copied for it, say), archived.
    pub archived: Vec<String>,
}

impl Orchestrator {
    pub async fn delete_run(&self, id: RunId, opts: DeleteRun) -> Result<DeletedRun, CoreError> {
        let _guard = self.lock(id).await;
        let run = self.store.get_run(id).await?;
        if !run.status.is_terminal() {
            return Err(CoreError::Conflict(format!(
                "the run is {:?}: cancel it first",
                run.status
            )));
        }
        // a. Work that is only in the checkout's files would go with it; the branch keeps
        // what was committed, so only an unlanded run has anything to lose.
        let dirty = self.checkout_changes(&run).await?;
        if !dirty.is_empty() && run.landed.is_none() && !opts.force {
            return Err(CoreError::ConflictFiles {
                message: format!(
                    "{} has uncommitted changes and the run never landed, so deleting it would \
                     throw them away:\n{}\nCommit them, or delete it with force.",
                    self.integration_path(&run).display(),
                    dirty.join("\n")
                ),
                files: dirty,
            });
        }

        let mut done = DeletedRun::default();
        // b. What the run still holds, by its own claims only: an agent another run holds now
        // is not touched. The agents themselves go home, its execution sessions are archived.
        for gone in self.let_go_run(id, false).await {
            match gone {
                LetGo::Home(name) => done.agents.returned.push(name),
                LetGo::Archived(name) => done.agents.archived.push(name),
            }
        }

        // c. What is left of the tasks' checkouts (a merged task's went when it merged).
        let tasks = self.store.list_tasks(id).await?;
        for task in &tasks {
            let Some(path) = &task.worktree else { continue };
            if !path.exists() {
                continue;
            }
            match self.remove_worktree(&run, path).await {
                Ok(()) => done.worktrees.push(path.display().to_string()),
                Err(err) => done.warnings.push(format!(
                    "task #{}: cannot remove {}: {err}",
                    task.key,
                    path.display()
                )),
            }
        }

        // d. The run's own checkout and the build directory its worktrees shared. An in-place
        // run works in the repository itself, which stays.
        if !run.in_place {
            let dir = self.integration_path(&run);
            if dir.exists() {
                match self.remove_worktree(&run, &dir).await {
                    Ok(()) => done.worktrees.push(dir.display().to_string()),
                    Err(err) => done
                        .warnings
                        .push(format!("cannot remove {}: {err}", dir.display())),
                }
            }
        }
        self.remove_run_build_into(&run, &mut done);

        // e. The branches, when they are the person's to lose: a landed run's carry nothing
        // that is not on the base branch, an unlanded one's carry everything.
        if opts.branches || run.landed.is_some() {
            for branch in self.run_branches(&run, &tasks) {
                match self.ws_for(&run).delete_branch(&run.repo, &branch).await {
                    Ok(()) => done.branches.push(branch),
                    Err(err) => done
                        .warnings
                        .push(format!("cannot delete branch {branch}: {err}")),
                }
            }
        }

        // f. The run itself, then the word that it is gone.
        self.store.delete_run(id).await?;
        self.publish(EventKind::RunDeleted { run_id: id }).await;
        Ok(done)
    }

    /// The branches a run left behind: its own, and each task's.
    fn run_branches(&self, run: &Run, tasks: &[crossagents_core::Task]) -> Vec<String> {
        if run.in_place {
            return Vec::new();
        }
        let mut branches = vec![run.branch.clone()];
        for task in tasks {
            if let Some(branch) = &task.branch
                && !branches.contains(branch)
            {
                branches.push(branch.clone());
            }
        }
        branches
    }

    /// The files the run's checkout changed and did not commit (docs/tasks/R1.md §3.2). Empty
    /// for a run working in the repository itself: that directory is not deleted.
    async fn checkout_changes(&self, run: &Run) -> Result<Vec<String>, CoreError> {
        if run.in_place {
            return Ok(Vec::new());
        }
        let dir = self.integration_path(run);
        // Gone, or no longer a checkout (a cancelled run's directory that Windows would not
        // let go of): whatever is left in it is not versioned work.
        if !dir.exists() || !self.workspaces.versioned(&dir).await {
            return Ok(Vec::new());
        }
        self.workspaces.uncommitted(&dir).await
    }

    /// Removes a working copy, waiting out the moment Windows needs before it lets go of a
    /// directory an agent's process had open. `git worktree remove` may remove the worktree and
    /// still fail on a file someone holds; what is left is then no longer a working copy, and
    /// deleting it is ours. Still there after that: the caller says so in a warning, and the
    /// run goes anyway.
    async fn remove_worktree(&self, run: &Run, path: &Path) -> Result<(), CoreError> {
        let mut last = None;
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            match self.ws_for(run).remove(&run.repo, path, None).await {
                Ok(()) => return Ok(()),
                Err(err) => last = Some(err),
            }
            if !path.exists() {
                return Ok(());
            }
        }
        if std::fs::remove_dir_all(path).is_ok() {
            // Git still lists the working copy (`.git/worktrees/<name>`), which would keep its
            // branch from being deleted or checked out again.
            if let Err(err) = self.ws_for(run).prune(&run.repo).await {
                tracing::warn!(repo = %run.repo.display(), error = %err, "cannot prune worktrees");
            }
            return Ok(());
        }
        Err(last.unwrap_or_else(|| CoreError::Storage("cannot remove the working copy".into())))
    }

    /// Removes `<worktrees>/<short>/target`, and the run's directory once nothing else is in
    /// it, recording what went and what could not (`remove_run_build` for callers that only
    /// want it logged).
    fn remove_run_build_into(&self, run: &Run, done: &mut DeletedRun) {
        let dir = self.config.worktrees.join(short(run.id));
        let target = dir.join("target");
        if target.exists() {
            match std::fs::remove_dir_all(&target) {
                Ok(()) => done.worktrees.push(target.display().to_string()),
                Err(err) => done.warnings.push(format!(
                    "cannot remove the run's build directory {}: {err}",
                    target.display()
                )),
            }
        }
        // Fails while a task's checkout is still there; those are removed above.
        let _ = std::fs::remove_dir(&dir);
    }
}
