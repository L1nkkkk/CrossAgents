//! Stages of a plan (docs/research/multica.md §2.2): a task may name one in `spec.stage`.
//!
//! A stage is a batch: its tasks start together (as their dependencies and scopes allow),
//! and no task of a later stage starts before every task of an earlier stage has ended
//! (merged, failed or cancelled). When a stage's batch ends, the run's lead is woken once
//! with what each task came to and word that the next stage may go; when every task has
//! ended, the closing notice in `engine::schedule_reporting` gathers the run for it. This is
//! the hub's own rule: the lead has nothing to watch.

use crossagents_core::{CoreError, Run, Task, TaskStatus};

use crate::engine::Orchestrator;

/// Longest summary of one task kept in a stage's notice.
const LINE_MAX: usize = 160;

/// The stage of `task`, if it is in one.
pub fn of(task: &Task) -> Option<u32> {
    task.spec.stage
}

/// Whether a task in `stage` may start: every task of an earlier stage has ended.
///
/// A task outside any stage is never held back, and tasks of the same stage do not wait for
/// each other (their dependencies and scopes still apply).
pub fn released(tasks: &[Task], stage: Option<u32>) -> bool {
    let Some(stage) = stage else {
        return true;
    };
    tasks
        .iter()
        .filter(|t| t.spec.stage.is_some_and(|s| s < stage))
        .all(|t| t.status.is_terminal())
}

/// The stages the run's tasks use, smallest first.
fn stages(tasks: &[Task]) -> Vec<u32> {
    let mut stages: Vec<u32> = tasks.iter().filter_map(of).collect();
    stages.sort_unstable();
    stages.dedup();
    stages
}

/// The stages whose tasks have all ended, smallest first.
fn ended(tasks: &[Task]) -> Vec<u32> {
    stages(tasks)
        .into_iter()
        .filter(|stage| {
            tasks
                .iter()
                .filter(|t| of(t) == Some(*stage))
                .all(|t| t.status.is_terminal())
        })
        .collect()
}

/// What one task of the batch came to, as one line of the notice.
fn line(task: &Task) -> String {
    let detail = task
        .report
        .summary
        .as_deref()
        .or(task.report.failure.as_deref())
        .map(first_line)
        .filter(|s| !s.is_empty());
    match detail {
        Some(detail) => format!("#{} {}: {detail}", task.key, word(task.status)),
        None => format!("#{} {}", task.key, word(task.status)),
    }
}

fn word(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Merged => "merged",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        _ => "ended",
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() > LINE_MAX {
        let mut cut: String = line.chars().take(LINE_MAX).collect();
        cut.push('…');
        cut
    } else {
        line.to_owned()
    }
}

/// The notice a stage's end is reported with: what each task came to, and whether the next
/// stage may start.
fn notice(run: &Run, tasks: &[Task], stage: u32) -> String {
    let batch: Vec<&Task> = tasks.iter().filter(|t| of(t) == Some(stage)).collect();
    let mut text = format!(
        "Stage {stage} of run {} has ended: {} task(s).\n{}",
        run.id,
        batch.len(),
        batch
            .iter()
            .map(|t| format!("- {}", line(t)))
            .collect::<Vec<_>>()
            .join("\n")
    );
    match stages(tasks).into_iter().find(|s| *s > stage) {
        Some(next) => text.push_str(&format!(
            "\nStage {next} may start now: its tasks start as their dependencies and scopes allow, and later stages wait for it."
        )),
        None => text.push_str("\nNo later stage is waiting."),
    }
    text
}

impl Orchestrator {
    /// Wakes the lead once for each stage whose batch has ended, listing what each task in
    /// the batch came to and whether the next stage may start (docs/research/multica.md
    /// §2.2). Runs without stages get nothing here: `engine::schedule_reporting` tells the
    /// lead once every task has ended, as before.
    pub(crate) async fn wake_stages(&self, run: &Run, tasks: &[Task]) -> Result<(), CoreError> {
        // Marked before the notices go out, under the lock alone: two schedules of the same
        // run cannot both claim a stage, and the guard is never held across an await.
        let fresh: Vec<u32> = {
            let ended = ended(tasks);
            let mut fresh = Vec::new();
            if !ended.is_empty() {
                let mut seen = self.stage_wakeups.lock().unwrap_or_else(|e| e.into_inner());
                let seen = seen.entry(run.id).or_default();
                fresh.extend(ended.into_iter().filter(|stage| seen.insert(*stage)));
            }
            fresh
        };
        for stage in &fresh {
            self.notice(
                run.planner,
                &format!("run:{}:stage:{stage}", run.id),
                &notice(run, tasks, *stage),
                crossagents_core::NoticeClass::Decide,
            )
            .await;
        }
        if !fresh.is_empty() {
            tracing::debug!(run = %run.id, stages = ?fresh, "woke the lead for ended stages");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use crossagents_core::{RunId, TaskId, TaskReport, TaskSpec};

    use super::*;

    fn task(key: &str, stage: Option<u32>, status: TaskStatus) -> Task {
        Task {
            id: TaskId::new(),
            run_id: RunId::new(),
            key: key.into(),
            title: key.into(),
            spec: TaskSpec {
                goal: "g".into(),
                stage,
                ..TaskSpec::default()
            },
            depends_on: Vec::new(),
            issuer: None,
            assignee: None,
            reviewer: None,
            worker_template: None,
            reviewer_template: None,
            worker_fresh: true,
            worker_restore: false,
            reviewer_fresh: true,
            status,
            attempts: 0,
            runs: Vec::new(),
            escalated: false,
            nudges: 0,
            branch: None,
            worktree: None,
            report: TaskReport::default(),
            parent: None,
            progress: None,
            handoffs: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn a_later_stage_waits_for_the_earlier_one_to_end() {
        let tasks = vec![
            task("a", Some(1), TaskStatus::Running),
            task("b", Some(2), TaskStatus::Pending),
            task("c", None, TaskStatus::Pending),
        ];
        assert!(released(&tasks, Some(1)));
        assert!(!released(&tasks, Some(2)), "stage 1 is still running");
        assert!(released(&tasks, None), "no stage is never held back");

        let mut tasks = tasks;
        tasks[0].status = TaskStatus::Cancelled;
        assert!(released(&tasks, Some(2)), "ended, whatever it came to");
    }

    #[test]
    fn ended_lists_a_stage_only_when_all_its_tasks_have() {
        let tasks = vec![
            task("a", Some(1), TaskStatus::Merged),
            task("b", Some(1), TaskStatus::Running),
            task("c", Some(2), TaskStatus::Pending),
        ];
        assert!(ended(&tasks).is_empty());
        let mut tasks = tasks;
        tasks[1].status = TaskStatus::Failed;
        assert_eq!(ended(&tasks), [1]);
    }

    #[test]
    fn a_stage_notice_lists_the_batch_and_the_next_stage() {
        let run = Run {
            origin_chat_id: None,
            work_id: None,
            continuation: None,
            id: RunId::new(),
            title: "t".into(),
            request: "r".into(),
            planner: crossagents_core::AgentId::new(),
            status: crossagents_core::RunStatus::Running,
            config: crossagents_core::RunConfig {
                worker: crossagents_core::AgentId::new(),
                escalate: None,
                reviewer: None,
                auto_approve: true,
                max_parallel: 3,
                max_attempts: 3,
                team: Vec::new(),
                policy: Default::default(),
            },
            repo: std::path::PathBuf::from("."),
            base_branch: "main".into(),
            branch: "run".into(),
            in_place: false,
            mode: None,
            note: None,
            plan: Vec::new(),
            lead_note: None,
            landed: None,
            summary: None,
            checks: Vec::new(),
            checks_state: None,
            checks_snapshot: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let mut a = task("a", Some(1), TaskStatus::Merged);
        a.report.summary = Some("wrote a\nmore".into());
        let tasks = vec![a, task("b", Some(2), TaskStatus::Pending)];
        let text = notice(&run, &tasks, 1);
        assert!(text.contains("Stage 1 of run"), "{text}");
        assert!(text.contains("- #a merged: wrote a"), "{text}");
        assert!(text.contains("Stage 2 may start now"), "{text}");
    }
}
