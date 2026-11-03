//! What a run leaves behind in its workspace (docs/tasks/R1.md §3): runs are hard-deleted
//! (`delete.rs`), so the record of what a run did is kept apart from it. It is written when the
//! run reaches a terminal state, again when it lands (the landing is part of what it did), and
//! once more just before a delete, so a run that ended before this record existed is kept too.
//! `delete_run` never deletes it: the workspace's history outlives the runs in it.

use chrono::Utc;
use crossagents_core::{Agent, CoreError, HistoryTask, Run, RunHistory, Task, TaskStatus};

use crate::engine::Orchestrator;

impl Orchestrator {
    /// Writes (or rewrites) the run's history record. Idempotent, and never in the way: a run
    /// that ended is not held up by its history, so a failure is only logged.
    pub(crate) async fn record_history(&self, run: &Run) {
        if let Err(err) = self.write_history(run).await {
            tracing::warn!(run = %run.id, error = %err, "cannot record the run's history");
        }
    }

    async fn write_history(&self, run: &Run) -> Result<(), CoreError> {
        let tasks = self.store.list_tasks(run.id).await?;
        let agents = self.store.list_agents().await?;
        let history = RunHistory {
            run_id: run.id,
            work_id: run.work_id,
            repo: run.repo.clone(),
            title: run.title.clone(),
            request: run.request.clone(),
            status: run.status,
            summary: run.summary.clone(),
            note: run.note.clone(),
            base_branch: run.base_branch.clone(),
            branch: run.branch.clone(),
            landed: run.landed.clone(),
            tasks: tasks
                .iter()
                .map(|task| history_task(task, &agents))
                .collect(),
            // What each merged task brought into the run branch, in merge order.
            merged_commits: tasks
                .iter()
                .filter(|task| task.status == TaskStatus::Merged)
                .filter_map(|task| task.report.verified_sha.clone())
                .collect(),
            created_at: run.created_at,
            ended_at: run.status.is_terminal().then(Utc::now),
            deleted: false,
        };
        self.store.upsert_run_history(&history).await
    }
}

/// A task as the history keeps it: the worker's name and model, not an id that may name
/// nothing by the time the history is read.
fn history_task(task: &Task, agents: &[Agent]) -> HistoryTask {
    let worker = task
        .assignee
        .or(task.worker_template)
        .and_then(|id| agents.iter().find(|agent| agent.id == id));
    HistoryTask {
        key: task.key.clone(),
        title: task.title.clone(),
        assignee: worker.map(|agent| agent.name.clone()),
        model: worker.and_then(|agent| agent.model.clone()),
        status: task.status,
    }
}
