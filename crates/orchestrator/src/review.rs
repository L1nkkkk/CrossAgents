//! The person reviewing work (docs/tasks/DOGFOOD.md §2): a task's diff, a run's total diff,
//! and comments that go straight to the worker as its next turn.

use crossagents_core::{CoreError, MessageSource, RunId, Task, TaskId, TaskStatus};

use crate::engine::Orchestrator;

/// Bytes of diff handed out at once; more is cut and flagged.
pub const DIFF_LIMIT: usize = 2 * 1024 * 1024;

/// A diff and what it compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changes {
    pub base: String,
    pub head: String,
    pub diff: String,
    pub truncated: bool,
}

/// One remark of the person's on a task's changes: a line (`line`, on the new file unless
/// `old_side`), or the whole file when `line` is `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remark {
    pub path: String,
    pub line: Option<u32>,
    pub old_side: bool,
    pub text: String,
}

impl Changes {
    fn new(base: String, head: String, mut diff: String) -> Self {
        let truncated = diff.len() > DIFF_LIMIT;
        if truncated {
            let mut end = DIFF_LIMIT;
            while !diff.is_char_boundary(end) {
                end -= 1;
            }
            diff.truncate(end);
        }
        Self {
            base,
            head,
            diff,
            truncated,
        }
    }
}

impl Orchestrator {
    /// Everything `task` changed against what it started from, committed or not.
    pub async fn task_changes(&self, run: RunId, task: TaskId) -> Result<Changes, CoreError> {
        let (run, task) = self.task_in(run, task).await?;
        let Some(dir) = task.worktree.clone().filter(|d| d.exists()) else {
            return Err(CoreError::Conflict(match task.status {
                TaskStatus::Merged => format!(
                    "#{} is merged and its worktree is gone; see the run's diff",
                    task.key
                ),
                _ => format!("#{} has no worktree (not started, or ended)", task.key),
            }));
        };
        let (base, _) = self.base_of(&run, &task).await?;
        let diff = self.ws_for(&run).diff(&dir, &base, &[]).await?;
        let head = task.branch.clone().unwrap_or_default();
        Ok(Changes::new(base, head, diff))
    }

    /// What landing the run would bring into its base branch.
    pub async fn run_changes(&self, run: RunId) -> Result<Changes, CoreError> {
        let run = self.store.get_run(run).await?;
        let diff = self
            .ws_for(&run)
            .branch_diff(&run.repo, &run.base_branch, &run.branch)
            .await?;
        Ok(Changes::new(
            run.base_branch.clone(),
            run.branch.clone(),
            diff,
        ))
    }

    /// The person's remarks on `task`, sent to its worker as its next turn. With `rework` the
    /// task goes back to running, as if its issuer had called `task_rework`.
    pub async fn review_task(
        &self,
        run: RunId,
        task: TaskId,
        remarks: Vec<Remark>,
        note: Option<String>,
        rework: bool,
    ) -> Result<Task, CoreError> {
        let note = note.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
        if remarks.iter().all(|r| r.text.trim().is_empty()) && note.is_none() && !rework {
            return Err(CoreError::Conflict("write a comment first".into()));
        }
        let _guard = self.lock(run).await;
        let (run, mut task) = self.task_in(run, task).await?;
        let Some(worker) = task.assignee else {
            return Err(CoreError::Conflict(format!(
                "#{} has no worker any more; comment on the run instead",
                task.key
            )));
        };
        let reopen = matches!(
            task.status,
            TaskStatus::Passed | TaskStatus::Stalled | TaskStatus::Reviewing
        );
        match task.status {
            TaskStatus::Running | TaskStatus::WaitingAnswer => {}
            _ if reopen => {}
            status => {
                return Err(CoreError::Conflict(format!("#{} is {status:?}", task.key)));
            }
        }
        if rework && reopen {
            if task.reviewer.take().is_some() {
                self.let_go_task(run.id, task.id, Some(crossagents_core::ClaimRole::Reviewer))
                    .await;
            }
            task.attempts += 1;
            task.report.failure = None;
            let said: Vec<&str> = note
                .as_deref()
                .into_iter()
                .chain(remarks.iter().map(|r| r.text.trim()))
                .filter(|t| !t.is_empty())
                .collect();
            if !said.is_empty() {
                task.report.rework.push(said.join("\n"));
            }
            self.transition(&mut task, TaskStatus::Running).await?;
        }
        let text = remarks_text(&task, &remarks, note.as_deref(), rework && reopen);
        self.deliver(worker, MessageSource::Human, text);
        let issuer = task.issuer_or(run.planner);
        let line = if rework && reopen {
            format!(
                "The person sent #{} back to its worker with {} comment(s).",
                task.key,
                remarks.len()
            )
        } else {
            format!(
                "The person commented on #{} ({} comment(s)); its worker has them.",
                task.key,
                remarks.len()
            )
        };
        self.notice(issuer, "task", &line).await;
        Ok(task)
    }

    async fn task_in(
        &self,
        run: RunId,
        task: TaskId,
    ) -> Result<(crossagents_core::Run, Task), CoreError> {
        let (found, task) = self.task_of(task).await?;
        if found.id != run {
            return Err(CoreError::NotFound {
                kind: "task",
                id: task.id.to_string(),
            });
        }
        Ok((found, task))
    }
}

/// The person's words, each placed at its file and line, then the task's facts.
fn remarks_text(task: &Task, remarks: &[Remark], note: Option<&str>, rework: bool) -> String {
    let mut out = String::new();
    if let Some(note) = note {
        out.push_str(note);
        out.push_str("\n\n");
    }
    let remarks: Vec<&Remark> = remarks
        .iter()
        .filter(|r| !r.text.trim().is_empty())
        .collect();
    if !remarks.is_empty() {
        out.push_str(&format!("Review comments on #{}:\n", task.key));
        for r in remarks {
            let place = match r.line {
                Some(line) if r.old_side => format!("{} (old line {line})", r.path),
                Some(line) => format!("{}:{line}", r.path),
                None => r.path.clone(),
            };
            out.push_str(&format!("- {place} — {}\n", r.text.trim()));
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "---\nfrom: the person, reviewing task #{}\ntask_id: {}",
        task.key, task.id
    ));
    if rework {
        out.push_str(&format!(
            "\nrework of task #{}\nattempt: {}",
            task.key, task.attempts
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_diffs_are_cut_on_a_char_boundary() {
        let diff = "é".repeat(DIFF_LIMIT);
        let c = Changes::new("main".into(), "run".into(), diff);
        assert!(c.truncated);
        assert!(c.diff.len() <= DIFF_LIMIT);
        let short = Changes::new("main".into(), "run".into(), "+a".into());
        assert!(!short.truncated);
    }
}
