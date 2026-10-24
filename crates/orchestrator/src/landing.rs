//! The end of a run (docs/tasks/H1.md §5.9): landing the run branch on the base branch and
//! closing the run. The lead does both when the policy lets it; the person can always.

use chrono::Utc;
use crossagents_core::{
    Agent, CoreError, EventKind, HumanItem, HumanItemKind, HumanItemStatus, LandMode, LandPolicy,
    Landing, Run, RunId, RunStatus, Task, TaskId, TaskReport, TaskSpec, TaskStatus,
};
use serde::Deserialize;
use serde::Serialize;
use serde_json::{Value, json};

use crate::engine::Orchestrator;
use crate::tools::args;
use crate::workspace::Synced;

/// What landing did: landed, or waits for a task that resolves conflicts with the base.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LandOutcome {
    Landed(Landing),
    Deferred {
        task: String,
        conflicts: Vec<String>,
    },
}

/// Why a run did not land yet, while its sync task runs; cleared once it lands.
const SYNC_NOTE_END: &str = "land again once it is merged";

impl Orchestrator {
    /// The run the caller leads and is still open.
    async fn led_run(&self, caller: &Agent) -> Result<Run, CoreError> {
        match self.run_of(caller.id).await? {
            Some((run, _)) if run.planner == caller.id => Ok(run),
            _ => Err(CoreError::Conflict("you lead no run".into())),
        }
    }

    pub(crate) async fn run_land_tool(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            #[serde(default)]
            mode: Option<LandMode>,
        }
        let a: A = args(input)?;
        let run = self.led_run(caller).await?;
        let mode = match (run.config.policy.land, a.mode) {
            (LandPolicy::Manual, _) => {
                return Err(CoreError::Conflict(
                    "this run leaves landing to the person: finish it with run_finish, they land it from the run page"
                        .into(),
                ));
            }
            (LandPolicy::Pr, Some(mode)) if mode != LandMode::Pr => {
                return Err(CoreError::Conflict(
                    "this run may only open a pull request (mode pr)".into(),
                ));
            }
            (LandPolicy::Pr, _) => LandMode::Pr,
            (LandPolicy::Auto, mode) => mode.unwrap_or(LandMode::Merge),
        };
        Ok(match self.land(run.id, mode, &caller.name).await? {
            LandOutcome::Landed(landing) => json!({ "landed": landing }),
            LandOutcome::Deferred { task, conflicts } => json!({
                "landed": null,
                "deferred": { "task": task, "conflicts": conflicts },
            }),
        })
    }

    /// Lands the run branch on its base branch. Every task must have ended and at least one
    /// merged. Landing twice returns the first landing. What the base gained meanwhile is
    /// merged into the run branch first; when that conflicts, a task to resolve it goes to
    /// the run's worker and landing waits for it (docs/tasks/DOGFOOD.md §4).
    pub async fn land(
        &self,
        id: RunId,
        mode: LandMode,
        by: &str,
    ) -> Result<LandOutcome, CoreError> {
        let _guard = self.lock(id).await;
        let mut run = self.store.get_run(id).await?;
        if let Some(landing) = &run.landed {
            return Ok(LandOutcome::Landed(landing.clone()));
        }
        if !matches!(run.status, RunStatus::Running | RunStatus::Completed) {
            return Err(CoreError::Conflict(format!("the run is {:?}", run.status)));
        }
        let tasks = self.store.list_tasks(id).await?;
        let open: Vec<&str> = tasks
            .iter()
            .filter(|t| !t.status.is_terminal())
            .map(|t| t.key.as_str())
            .collect();
        if !open.is_empty() {
            return Err(CoreError::Conflict(format!(
                "tasks still open: {}; accept, drop or cancel them first",
                open.join(", ")
            )));
        }
        if !tasks.iter().any(|t| t.status == TaskStatus::Merged) {
            return Err(CoreError::Conflict(
                "nothing was merged, so there is nothing to land".into(),
            ));
        }
        if !run.in_place {
            // A finished run has no integration worktree any more; check the branch out again.
            let integration = self.integration_path(&run);
            if !integration.exists() {
                self.workspaces
                    .checkout(&run.repo, &run.branch, &integration)
                    .await?;
            }
            if let Synced::Conflicts { files, detail } = self
                .workspaces
                .sync_base(&integration, &run.base_branch)
                .await?
            {
                let key = self.sync_task(&mut run, &tasks, &files, &detail).await?;
                return Ok(LandOutcome::Deferred {
                    task: key,
                    conflicts: files,
                });
            }
        }
        let reference = self
            .ws_for(&run)
            .land(
                &run.repo,
                &run.base_branch,
                &run.branch,
                mode,
                &format!("crossagents: {}", run.title),
            )
            .await?;
        let landing = Landing {
            mode,
            by: by.to_owned(),
            reference,
            at: Utc::now(),
        };
        run.landed = Some(landing.clone());
        if run
            .note
            .as_deref()
            .is_some_and(|n| n.ends_with(SYNC_NOTE_END))
        {
            run.note = None;
        }
        run.updated_at = Utc::now();
        self.store.update_run(&run).await?;
        self.publish(EventKind::RunLanded {
            id,
            landing: landing.clone(),
        })
        .await;
        Ok(LandOutcome::Landed(landing))
    }

    /// `task` was merged: if it was the one resolving the base branch's conflict, the run's
    /// "land again once it is merged" note is done with (it would read as still blocked).
    pub(crate) async fn sync_resolved(&self, run: &mut Run, task: &Task) -> Result<(), CoreError> {
        let resolved = run.note.as_deref().is_some_and(|n| {
            n.ends_with(SYNC_NOTE_END) && n.contains(&format!("task #{} ", task.key))
        });
        if resolved {
            run.note = None;
            run.updated_at = Utc::now();
            self.store.update_run(run).await?;
        }
        Ok(())
    }

    /// A task for the run's worker: merge the base branch into the run branch and resolve
    /// `files`. Reopens a finished run so it gets done. Returns the task's key.
    async fn sync_task(
        &self,
        run: &mut Run,
        tasks: &[Task],
        files: &[String],
        detail: &str,
    ) -> Result<String, CoreError> {
        let stem: String = format!("sync-{}", run.base_branch)
            .to_lowercase()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .take(28)
            .collect();
        let key = (1..)
            .map(|n| {
                if n == 1 {
                    stem.clone()
                } else {
                    format!("{stem}-{n}")
                }
            })
            .find(|k| !tasks.iter().any(|t| &t.key == k))
            .expect("some key is free");
        let base = &run.base_branch;
        let goal = format!(
            "{base} gained commits after this run started, and they conflict with the run's \
             changes, so the run cannot land. In your worktree, run `git merge {base}`, resolve \
             the conflicts so that both sides' changes survive, run the tests, commit, and \
             task_complete.\n\nConflicting files:\n{}",
            files
                .iter()
                .map(|f| format!("- {f}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let now = Utc::now();
        let task = Task {
            id: TaskId::new(),
            run_id: run.id,
            key: key.clone(),
            title: format!("Resolve conflicts with {base}"),
            spec: TaskSpec {
                goal,
                scope: Default::default(),
                interfaces: Vec::new(),
                acceptance: Vec::new(),
                references: vec![format!("conflict hunks when merging {base}:\n{detail}")],
            },
            depends_on: Vec::new(),
            issuer: Some(run.planner),
            parent: None,
            assignee: None,
            reviewer: None,
            worker_template: Some(run.config.worker),
            reviewer_template: None,
            status: TaskStatus::Pending,
            attempts: 0,
            escalated: false,
            nudges: 0,
            branch: None,
            worktree: None,
            report: TaskReport::default(),
            progress: None,
            created_at: now,
            updated_at: now,
        };
        self.store.insert_task(&task).await?;
        self.publish(EventKind::TaskStatusChanged {
            run_id: run.id,
            id: task.id,
            key: key.clone(),
            from: TaskStatus::Pending,
            to: TaskStatus::Pending,
            assignee: None,
        })
        .await;
        let note = format!(
            "{base} moved and conflicts with the run in {}; task #{key} resolves it, {SYNC_NOTE_END}",
            files.join(", ")
        );
        if run.status == RunStatus::Running {
            run.note = Some(note.clone());
            run.updated_at = now;
            self.store.update_run(run).await?;
        } else {
            self.set_run_status(run, RunStatus::Running, Some(note.clone()))
                .await?;
        }
        self.notice(
            run.planner,
            &format!("run:{}:sync", run.id),
            &format!("Not landed: {note}."),
        )
        .await;
        self.schedule(run).await?;
        Ok(key)
    }

    pub(crate) async fn run_finish_tool(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            summary: String,
            #[serde(default)]
            cancel_open: bool,
        }
        let a: A = args(input)?;
        let run = self.led_run(caller).await?;
        let run = self.finish(run.id, Some(a.summary), a.cancel_open).await?;
        Ok(json!({ "status": run.status, "landed": run.landed }))
    }

    /// Closes the run: completed if anything merged, else failed. Open tasks are refused
    /// unless `cancel_open`. The person gets the summary, with every decision that was a
    /// default rather than theirs.
    pub async fn finish(
        &self,
        id: RunId,
        summary: Option<String>,
        cancel_open: bool,
    ) -> Result<Run, CoreError> {
        let open = self
            .store
            .list_tasks(id)
            .await?
            .into_iter()
            .filter(|t| !t.status.is_terminal())
            .count();
        if open > 0 && !cancel_open {
            return Err(CoreError::Conflict(format!(
                "{open} task(s) still open; finish them, or pass cancel_open to cancel them"
            )));
        }
        let mut run = self.store.get_run(id).await?;
        if run.status.is_terminal() {
            return Ok(run);
        }
        if open > 0 {
            for mut task in self.store.list_tasks(id).await? {
                if task.status.is_terminal() {
                    continue;
                }
                let _guard = self.lock(id).await;
                self.release_agents(&mut task).await;
                self.cleanup_worktree(&run, &task, false).await;
                task.report.failure = Some("cancelled when the run finished".into());
                self.transition(&mut task, TaskStatus::Cancelled).await?;
            }
        }
        let _guard = self.lock(id).await;
        run = self.store.get_run(id).await?;
        run.summary = summary.clone();
        let tasks = self.store.list_tasks(id).await?;
        self.finish_run(&mut run, &tasks).await?;

        // The closing note for the person: the lead's words, then what the hub knows.
        let mut text = summary.unwrap_or_else(|| format!("「{}」已结束。", run.title));
        let merged = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Merged)
            .count();
        text.push_str(&format!("\n\n— {merged}/{} 个任务已合并", tasks.len()));
        match &run.landed {
            _ if run.in_place && merged > 0 => {
                text.push_str(&format!("，改动已直接写在 {}", run.repo.display()))
            }
            Some(l) => text.push_str(&format!(
                "，已落地到 {}（{}）",
                run.base_branch, l.reference
            )),
            None if merged > 0 => text.push_str(&format!("，结果在 {}，等你落地", run.branch)),
            None => {}
        }
        let defaults = self.defaulted_decisions(id).await?;
        if !defaults.is_empty() {
            text.push_str("\n\n没等到你回答、按默认执行的决定：");
            for item in &defaults {
                text.push_str(&format!(
                    "\n- {} → {}",
                    item.text.lines().next().unwrap_or_default(),
                    item.answer.as_deref().unwrap_or("?")
                ));
            }
        }
        let note = HumanItem {
            id: format!("n-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]),
            kind: HumanItemKind::Notice,
            from: run.planner,
            run_id: Some(id),
            task_id: None,
            text,
            options: Vec::new(),
            default: None,
            timeout_at: None,
            status: HumanItemStatus::Open,
            answer: None,
            answered_at: None,
            action: None,
            created_at: Utc::now(),
        };
        self.store.put_human_item(&note).await?;
        self.publish(EventKind::HumanAsked { item: note }).await;
        Ok(run)
    }
}
