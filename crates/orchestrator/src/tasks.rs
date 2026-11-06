//! The task tools (docs/tasks/H1.md §5.7–5.11): the issuer creates, checks, accepts, reworks
//! and drops tasks; the assignee reports; a reviewer gives a verdict. The hub carries out
//! each decision and relays what each side wrote — it decides nothing itself.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AgentKey, ClaimRole, CoreError, EventKind, HandoffKind, HandoffOutcome,
    PlannedTask, Role, Run, RunStatus, Task, TaskId, TaskReport, TaskRunOutcome, TaskSpec,
    TaskStatus, UsageOutcome,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::brief;
use crate::engine::{Orchestrator, agent_name, worker_name};
use crate::plan;
use crate::tools::args;
use crate::verify::{all_passed, run_command};

/// Largest diff or file text a tool returns.
const READ_LIMIT: usize = 60 * 1024;
/// `workspace_run`: default and longest time limit, and output kept.
const RUN_DEFAULT: Duration = Duration::from_secs(300);
const RUN_MAX: Duration = Duration::from_secs(1800);
const RUN_OUTPUT: usize = 64 * 1024;

fn clip(mut text: String, limit: usize) -> String {
    if text.len() > limit {
        let mut end = limit;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n… (cut; narrow it with paths or a line range)");
    }
    text
}

fn task_json(task: &Task) -> Value {
    json!({
        "task_id": task.id,
        "key": task.key,
        "title": task.title,
        "status": task.status,
        "issuer": task.issuer,
        "assignee": task.assignee,
        "depends_on": task.depends_on,
        "progress": task.progress,
        "open_question": task.report.question,
    })
}

#[derive(Deserialize)]
struct TaskArg {
    /// A task key (`auth` or `#auth`) in the caller's run, or a task id.
    #[serde(alias = "task_id")]
    task: String,
}

impl Orchestrator {
    pub(crate) async fn task_of(&self, id: TaskId) -> Result<(Run, Task), CoreError> {
        let task = self.store.get_task(id).await?;
        let run = self.store.get_run(task.run_id).await?;
        Ok((run, task))
    }

    /// The id of the task named by `input.task` (or `task_id`).
    pub(crate) async fn task_ref(
        &self,
        caller: &Agent,
        input: &Value,
    ) -> Result<TaskId, CoreError> {
        Ok(self.task_arg(caller, input).await?.1.id)
    }

    /// The task named by `input.task` (or `task_id`), with its run.
    async fn task_arg(&self, caller: &Agent, input: &Value) -> Result<(Run, Task), CoreError> {
        let a: TaskArg = args(input.clone())?;
        let task = self.find_task(caller, &a.task).await?;
        let run = self.store.get_run(task.run_id).await?;
        Ok((run, task))
    }

    fn require_issuer(run: &Run, task: &Task, caller: &Agent) -> Result<(), CoreError> {
        if task.issuer_or(run.planner) == caller.id {
            Ok(())
        } else {
            Err(CoreError::Conflict(format!(
                "only the issuer of #{} decides on it",
                task.key
            )))
        }
    }

    fn require_assignee(task: &Task, caller: &Agent) -> Result<(), CoreError> {
        if task.assignee == Some(caller.id) {
            Ok(())
        } else {
            Err(CoreError::Conflict(format!(
                "#{} is not assigned to you",
                task.key
            )))
        }
    }

    /// Who may look into a task's worktree: its issuer, assignee, reviewer, or the run's lead.
    fn require_insider(run: &Run, task: &Task, caller: &Agent) -> Result<(), CoreError> {
        let allowed = [
            Some(task.issuer_or(run.planner)),
            Some(run.planner),
            task.assignee,
            task.reviewer,
        ];
        if allowed.contains(&Some(caller.id)) {
            Ok(())
        } else {
            Err(CoreError::Conflict(format!(
                "#{} is not yours to inspect",
                task.key
            )))
        }
    }

    fn worktree(task: &Task) -> Result<PathBuf, CoreError> {
        task.worktree.clone().ok_or_else(|| {
            CoreError::Conflict(format!("#{} has no worktree yet (not started)", task.key))
        })
    }

    async fn store_task(&self, task: &mut Task) -> Result<(), CoreError> {
        task.updated_at = Utc::now();
        self.store.update_task(task).await
    }

    // -----------------------------------------------------------------------------------
    // Reading

    pub(crate) async fn task_list(&self, caller: &Agent, input: Value) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            #[serde(default)]
            status: Option<TaskStatus>,
            #[serde(default)]
            mine: bool,
        }
        let a: A = args(input)?;
        let Some((run, tasks)) = self.run_of(caller.id).await? else {
            return Err(CoreError::Conflict("you are in no run".into()));
        };
        let tasks: Vec<Value> = tasks
            .iter()
            .filter(|t| a.status.is_none_or(|s| t.status == s))
            .filter(|t| {
                !a.mine || t.issuer_or(run.planner) == caller.id || t.assignee == Some(caller.id)
            })
            .map(task_json)
            .collect();
        Ok(
            json!({ "run_id": run.id, "run_status": run.status, "run_branch": run.branch, "tasks": tasks }),
        )
    }

    pub(crate) async fn task_get(&self, caller: &Agent, input: Value) -> Result<Value, CoreError> {
        let (run, task) = self.task_arg(caller, &input).await?;
        if let Some(path) = input.get("path") {
            let path = path
                .as_str()
                .ok_or_else(|| CoreError::Conflict("path must be a JSON Pointer string".into()))?;
            let result = task
                .report
                .result
                .as_ref()
                .ok_or_else(|| CoreError::Conflict("task has no result".into()))?;
            return result.pointer(path).cloned().ok_or_else(|| {
                CoreError::Conflict(format!(
                    "result path {path:?} does not exist or is not a valid JSON Pointer"
                ))
            });
        }
        let mut value = task_json(&task);
        value["spec"] = json!(task.spec);
        value["report"] = json!(task.report);
        value["branch"] = json!(task.branch);
        value["worktree"] = json!(task.worktree);
        value["run_branch"] = json!(run.branch);
        value["reviewer"] = json!(task.reviewer);
        value["runs"] = json!(task.runs);
        Ok(value)
    }

    // -----------------------------------------------------------------------------------
    // Creating and changing tasks (issuer / lead)

    /// More tasks for a running run, by its lead. Keys already in the run are returned as
    /// they are (a retried call creates nothing twice).
    pub(crate) async fn task_create(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            tasks: Vec<PlannedTask>,
        }
        let a: A = args(input)?;
        let Some((run, existing)) = self.run_of(caller.id).await? else {
            return Err(CoreError::Conflict("you are in no run".into()));
        };
        // The lead adds tasks to the run; with the delegate policy an assignee splits off
        // parts of its own task, which branch from it and merge back into it.
        let parent = if run.planner == caller.id {
            None
        } else if !run.config.policy.delegate {
            return Err(CoreError::Conflict(
                "only the run's lead creates tasks (this run does not let workers delegate)".into(),
            ));
        } else {
            let own = existing
                .iter()
                .find(|t| t.assignee == Some(caller.id) && !t.status.is_terminal())
                .ok_or_else(|| CoreError::Conflict("you have no open task to split".into()))?;
            if own.worktree.is_none() {
                return Err(CoreError::Conflict("your task has not started".into()));
            }
            Some(own.id)
        };
        if run.status != RunStatus::Running {
            return Err(CoreError::Conflict(format!(
                "the run is {:?}; before it runs, put tasks in the plan (plan_submit)",
                run.status
            )));
        }
        let _guard = self.lock(run.id).await;
        let mut run = self.store.get_run(run.id).await?;
        let (repeated, new): (Vec<PlannedTask>, Vec<PlannedTask>) = a
            .tasks
            .into_iter()
            .partition(|t| existing.iter().any(|e| e.key == t.key));
        // Dependencies may name tasks already in the run.
        let mut check: Vec<PlannedTask> = new.clone();
        for task in &existing {
            if !check.iter().any(|t| t.key == task.key) {
                check.push(PlannedTask {
                    key: task.key.clone(),
                    title: task.title.clone(),
                    spec: TaskSpec {
                        goal: "existing".into(),
                        ..task.spec.clone()
                    },
                    depends_on: Vec::new(),
                    assignee: None,
                    reviewer: None,
                    fresh: true,
                    restore: false,
                    reviewer_fresh: true,
                });
            }
        }
        if !new.is_empty() {
            plan::validate(&check)
                .map_err(|p| CoreError::Conflict(format!("invalid tasks:\n{p}")))?;
        }
        let assignments = self.resolve_assignments(&run, &new).await?;
        let now = Utc::now();
        let mut created = Vec::new();
        for (planned, (worker_template, reviewer_template)) in new.into_iter().zip(assignments) {
            let task = Task {
                id: TaskId::new(),
                run_id: run.id,
                key: planned.key,
                title: planned.title,
                spec: planned.spec,
                depends_on: planned.depends_on,
                issuer: Some(caller.id),
                parent,
                assignee: None,
                reviewer: None,
                worker_template,
                reviewer_template,
                worker_fresh: planned.fresh,
                worker_restore: planned.restore,
                reviewer_fresh: planned.reviewer_fresh,
                status: TaskStatus::Pending,
                attempts: 0,
                runs: Vec::new(),
                escalated: false,
                nudges: 0,
                branch: None,
                worktree: None,
                report: TaskReport::default(),
                progress: None,
                handoffs: Vec::new(),
                created_at: now,
                updated_at: now,
            };
            self.store.insert_task(&task).await?;
            self.publish(EventKind::TaskStatusChanged {
                run_id: run.id,
                id: task.id,
                key: task.key.clone(),
                from: TaskStatus::Pending,
                to: TaskStatus::Pending,
                assignee: None,
            })
            .await;
            created.push(task.id);
        }
        let not_started = self.schedule_reporting(&mut run).await?;
        // As scheduling left them: a task that started has its worker and is running. The
        // state before it read as "assignee ignored", and the lead assigned it a second time.
        let mut shown = Vec::with_capacity(created.len());
        for id in created {
            shown.push(task_json(&self.store.get_task(id).await?));
        }
        let created = shown;
        let repeated: Vec<String> = repeated.into_iter().map(|t| t.key).collect();
        Ok(json!({ "created": created, "repeated": repeated, "not_started": not_started }))
    }

    pub(crate) async fn task_update(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            #[serde(default)]
            title: Option<String>,
            #[serde(default)]
            spec: Option<TaskSpec>,
            #[serde(default)]
            depends_on: Option<Vec<String>>,
        }
        let (run, task) = self.task_arg(caller, &input).await?;
        Self::require_issuer(&run, &task, caller)?;
        let a: A = args(input)?;
        let _guard = self.lock(run.id).await;
        let mut run = self.store.get_run(run.id).await?;
        let mut task = self.store.get_task(task.id).await?;
        if task.status.is_terminal() {
            return Err(CoreError::Conflict(format!("#{} has ended", task.key)));
        }
        if let Some(title) = a.title {
            task.title = title;
        }
        if let Some(spec) = a.spec {
            task.spec = spec;
        }
        if let Some(deps) = a.depends_on {
            task.depends_on = deps;
        }
        self.store_task(&mut task).await?;
        if let Some(worker) = task.assignee {
            self.relay(
                worker,
                caller.id,
                format!("#{} was changed:\n\n{}", task.key, brief::assignment(&task)),
                None,
            );
        }
        // New dependencies (or a narrower scope) may let it, or another pending task, start
        // now rather than when some other task next ends (docs/TODO.md §十六).
        self.schedule(&mut run).await?;
        let task = self.store.get_task(task.id).await?;
        Ok(task_json(&task))
    }

    /// Gives the task to another teammate. A started task keeps its worktree: the new
    /// worker carries on from what is there.
    pub(crate) async fn task_assign(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            to: String,
            /// Why it changes hands, for the handoff card.
            #[serde(default)]
            reason: Option<String>,
            /// `false`: `to` itself, with its conversation (docs/tasks/S1.md §3.3).
            #[serde(default = "yes")]
            fresh: bool,
            #[serde(default)]
            restore: bool,
        }
        fn yes() -> bool {
            true
        }
        let (run, task) = self.task_arg(caller, &input).await?;
        Self::require_issuer(&run, &task, caller)?;
        let a: A = args(input)?;
        let to = self.store.get_agent(&AgentKey::parse(&a.to)).await?;
        if !a.fresh {
            self.may_take_self(&run, &to, a.restore).await?;
        }
        let _guard = self.lock(run.id).await;
        let mut run = self.store.get_run(run.id).await?;
        let mut task = self.store.get_task(task.id).await?;
        if task.status.is_terminal() {
            return Err(CoreError::Conflict(format!("#{} has ended", task.key)));
        }
        if task.status == TaskStatus::Pending {
            // Its first worker gets the card when it starts, if it can start now.
            task.worker_template = Some(to.id);
            task.worker_fresh = a.fresh;
            task.worker_restore = a.restore;
            task.progress = None;
            self.store_task(&mut task).await?;
            let not_started = self.schedule_reporting(&mut run).await?;
            let task = self.store.get_task(task.id).await?;
            return Ok(json!({ "task": task_json(&task), "not_started": not_started }));
        }
        if !a.fresh && task.assignee == Some(to.id) {
            return Err(CoreError::Conflict(format!(
                "{} already works on #{}",
                to.name, task.key
            )));
        }
        let left = self.left_by_worker(&run, &task, task.assignee).await;
        let reason = a
            .reason
            .map(|r| r.trim().to_owned())
            .filter(|r| !r.is_empty())
            .or_else(|| task.report.failure.clone());
        let (kind, reason) = if run.config.escalate == Some(to.id) {
            let why = reason.unwrap_or_else(|| {
                format!("{} moved it to the run's escalation teammate", caller.name)
            });
            (HandoffKind::Escalate, Some(why))
        } else {
            (HandoffKind::Reassign, reason)
        };
        let cwd = Self::worktree(&task)?;
        // `to` itself is checked and moved in before the worker before goes: a refusal leaves
        // the task as it was.
        if !a.fresh {
            self.take_self(&run, &task, &to, ClaimRole::Worker, a.restore)
                .await?;
        }
        // The worker before: archived, or home (docs/tasks/S1.md §3.4).
        crossagents_core::Handoff::end(
            &mut task.handoffs,
            task.assignee,
            HandoffOutcome::Reassigned,
        );
        if let Some(old) = task.assignee.take()
            && let Err(err) = self.let_go(run.id, old).await
        {
            tracing::warn!(task = %task.key, error = %err, "cannot let go of the worker before");
        }
        let worker = if a.fresh {
            self.fresh_agent(
                &task,
                to.id,
                &worker_name(&run, &task, to.id),
                Role::Worker,
                &cwd,
            )
            .await?
        } else {
            self.store.get_agent(&AgentKey::Id(to.id)).await?
        };
        task.worker_template = Some(to.id);
        task.worker_fresh = a.fresh;
        task.worker_restore = a.restore;
        task.assignee = Some(worker.id);
        task.report.question = None;
        if task.status != TaskStatus::Running {
            self.transition(&mut task, TaskStatus::Running).await?;
        } else {
            self.store_task(&mut task).await?;
        }
        let card = self
            .hand_off(
                &run,
                &mut task,
                kind,
                left,
                &worker,
                a.fresh.then(|| to.name.clone()),
                reason,
            )
            .await?;
        self.notify_dispatched(caller.id, &task, &card).await;
        let mut opening = brief::handover(&task, &card);
        if let Some(pushed) = self.expertise_brief(&worker, &task).await {
            opening = format!("{opening}\n\n{pushed}");
        }
        // Handing the task over starts the new worker's run (multica §2.4).
        let envelope = self.relay_envelope(caller.id, opening);
        crate::attempts::begin(&mut task, Some(worker.id), Some(envelope.id));
        self.store_task(&mut task).await?;
        self.send_in_background(worker.id, envelope);
        Ok(task_json(&task))
    }

    pub(crate) async fn task_cancel(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            #[serde(default)]
            keep_branch: bool,
        }
        let (run, task) = self.task_arg(caller, &input).await?;
        Self::require_issuer(&run, &task, caller)?;
        let a: A = args(input)?;
        let _guard = self.lock(run.id).await;
        let mut run = self.store.get_run(run.id).await?;
        let mut task = self.store.get_task(task.id).await?;
        if task.status == TaskStatus::Cancelled {
            return Ok(json!({ "task": task_json(&task), "repeated": true }));
        }
        if task.status == TaskStatus::Verifying {
            return Err(CoreError::Conflict(format!(
                "#{} is still being verified; wait for its checks (the result arrives as a message), then cancel it",
                task.key
            )));
        }
        if task.status.is_terminal() {
            return Err(CoreError::Conflict(format!("#{} has ended", task.key)));
        }
        crossagents_core::Handoff::end(
            &mut task.handoffs,
            task.assignee,
            HandoffOutcome::Cancelled,
        );
        let released = self.release_agents(&mut task).await;
        self.cleanup_worktree(&run, &task, !a.keep_branch, released.is_empty())
            .await;
        task.report.failure = Some(format!("cancelled by {}", caller.name));
        crate::attempts::close_open(
            &mut task,
            TaskRunOutcome::Cancelled,
            Some(format!("cancelled by {}", caller.name)),
        );
        self.transition(&mut task, TaskStatus::Cancelled).await?;
        self.schedule(&mut run).await?;
        Ok(json!({ "task": task_json(&task) }))
    }

    // -----------------------------------------------------------------------------------
    // Reporting (assignee)

    pub(crate) async fn task_progress(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            text: String,
        }
        let (run, task) = self.task_arg(caller, &input).await?;
        Self::require_assignee(&task, caller)?;
        let a: A = args(input)?;
        let _guard = self.lock(run.id).await;
        let mut task = self.store.get_task(task.id).await?;
        task.progress = Some(a.text.clone());
        // Reporting progress shows the worker is alive.
        if task.status == TaskStatus::Stalled {
            self.transition(&mut task, TaskStatus::Running).await?;
        } else {
            self.store_task(&mut task).await?;
        }
        self.publish(EventKind::TaskProgress {
            run_id: run.id,
            id: task.id,
            key: task.key.clone(),
            text: a.text,
        })
        .await;
        Ok(json!({ "recorded": true }))
    }

    /// The assignee says it is done: the task goes into `Verifying` (a fresh verification id
    /// on it) and the hub commits, runs any checks, records the verified commit and only then
    /// makes it `Passed` and relays the worker's summary. Nothing the issuer sees as done has
    /// been unchecked (docs/TODO.md §九 ①).
    ///
    /// A task already `Passed` and not yet accepted can be reported again: the worker kept
    /// working, so the new report is a new round, checked like the first one. Only a call
    /// that brings nothing new - the same summary and nothing changed in the worktree since
    /// the verified commit, uncommitted work included - is the retried call it looks like.
    #[cfg(test)]
    pub(crate) async fn complete(
        self: &Arc<Self>,
        caller: &Agent,
        id: TaskId,
        summary: String,
    ) -> Result<(), CoreError> {
        self.complete_with_result(caller, id, summary, None).await
    }

    pub(crate) async fn complete_with_result(
        self: &Arc<Self>,
        caller: &Agent,
        id: TaskId,
        summary: String,
        result: Option<Value>,
    ) -> Result<(), CoreError> {
        let (run, _) = self.task_of(id).await?;
        let _guard = self.lock(run.id).await;
        let mut task = self.store.get_task(id).await?;
        Self::require_assignee(&task, caller)?;
        if let Some(schema) = &task.spec.result_schema {
            let value = result
                .as_ref()
                .ok_or_else(|| CoreError::Conflict("result is required by result_schema".into()))?;
            let validator = jsonschema::validator_for(schema)
                .map_err(|err| CoreError::Conflict(format!("invalid result_schema: {err}")))?;
            if let Err(err) = validator.validate(value) {
                return Err(CoreError::Conflict(format!(
                    "result at {}: {err}",
                    err.instance_path
                )));
            }
        }
        if task.status == TaskStatus::Verifying {
            return Ok(()); // a retried call: its checks are still running
        }
        if task.status == TaskStatus::Passed {
            if task.report.result == result && !self.brings_new_work(&run, &task, &summary).await {
                tracing::debug!(task = %task.key, "the same report again; the one it has stands");
                return Ok(());
            }
            // What the previous report said about the work no longer holds: the checks, the
            // commit they ran on and the diff they saw are about the work before this.
            task.report.checks.clear();
            task.report.diff_stat = None;
            task.report.verified_sha = None;
            task.report.unchecked = false;
        } else if !matches!(task.status, TaskStatus::Running | TaskStatus::Stalled) {
            return Err(CoreError::Conflict(format!(
                "#{} is {:?}; only a task being worked on can be completed",
                task.key, task.status
            )));
        }
        task.report.summary = Some(summary);
        task.report.result = result;
        task.report.failure = None;
        task.report.forced = None;
        task.report.interrupted = false;
        // The run ends here, with a report; the task does not (multica §2.4).
        crate::attempts::close(&mut task, TaskRunOutcome::Reported, None, false);
        let token = uuid::Uuid::new_v4().to_string();
        task.report.verification = Some(token.clone());
        self.transition(&mut task, TaskStatus::Verifying).await?;
        let this = self.clone();
        let worker = caller.id;
        tokio::spawn(async move { this.report_done(id, worker, token).await });
        Ok(())
    }

    /// Whether a report on a `Passed` task carries work its checks did not see: a summary
    /// that is new, a worktree with an uncommitted change or a commit after the verified one,
    /// or nothing verified at all (the checks could not run). Only when all of that says no
    /// is the report the same one again.
    async fn brings_new_work(&self, run: &Run, task: &Task, summary: &str) -> bool {
        if task.report.summary.as_deref().map(str::trim) != Some(summary.trim()) {
            return true;
        }
        let Some(dir) = task.worktree.as_deref().filter(|dir| dir.exists()) else {
            // No worktree to have worked in: nothing can have changed.
            return false;
        };
        let ws = self.ws_for(run);
        if !ws.uncommitted(dir).await.unwrap_or_default().is_empty() {
            return true;
        }
        let Some(verified) = task
            .report
            .verified_sha
            .as_deref()
            .map(str::trim)
            .filter(|sha| !sha.is_empty())
        else {
            // Nothing verified what it stands on: report again to have it checked.
            return true;
        };
        match ws.head(dir).await {
            Ok(head) => head.trim() != verified,
            // Cannot tell what is there: let the checks run again rather than drop it.
            Err(_) => true,
        }
    }

    /// Commits, checks and records the report of `worker`, for the verification `token` only:
    /// a rework or a newer verification in the meantime drops this one instead of overwriting
    /// it. The order matters: `Passed` and the summary reach the issuer after the checks, so
    /// accepting on that word accepts checked work.
    ///
    /// Whatever goes wrong on the way - the commit, a worktree that is gone, the base branch,
    /// the checks' own bookkeeping - the task still leaves `Verifying`: the error is written
    /// into its report as the failure and the report goes to its issuer as an unverified one.
    /// A verification that never lands would leave the task stuck, refused to accept, rework
    /// or drop at once (docs/TODO.md §九 ①).
    async fn report_done(self: &Arc<Self>, id: TaskId, worker: AgentId, token: String) {
        if let Err(err) = self.verify_and_record(id, worker, &token).await {
            self.record_verification_error(id, worker, &token, err)
                .await;
        }
    }

    /// The work of [`Self::report_done`]: everything that can fail.
    async fn verify_and_record(
        &self,
        id: TaskId,
        worker: AgentId,
        token: &str,
    ) -> Result<(), CoreError> {
        let task = self.store.get_task(id).await?;
        let run = self.store.get_run(task.run_id).await?;
        if !Self::still_verifying(&task, token) {
            tracing::debug!(task = %task.key, "the report is for a verification that ended; dropping it");
            return Ok(());
        }
        let dir = Self::worktree(&task)?;
        self.ws_for(&run)
            .commit_all(&dir, &format!("{}: {}", task.key, task.title))
            .await?;
        let sha = self.ws_for(&run).head(&dir).await.ok();
        let (base, _) = self.base_of(&run, &task).await?;
        let diff_stat = self.ws_for(&run).diff_stat(&dir, &base).await.ok();
        let unchecked = task.spec.acceptance.is_empty();
        let checks = if unchecked {
            Vec::new()
        } else {
            self.verifier.check(&dir, &task.spec.acceptance).await
        };
        let _guard = self.lock(run.id).await;
        let mut task = self.store.get_task(id).await?;
        if !Self::still_verifying(&task, token) {
            tracing::debug!(task = %task.key, "the report is for a verification that ended; dropping it");
            return Ok(());
        }
        task.report.diff_stat = diff_stat;
        task.report.checks = checks.clone();
        task.report.verified_sha = sha;
        task.report.unchecked = unchecked;
        task.report.verification = None;
        self.transition(&mut task, TaskStatus::Passed).await?;
        if !checks.is_empty() {
            self.publish(EventKind::TaskChecked {
                run_id: run.id,
                id,
                key: task.key.clone(),
                passed: all_passed(&checks),
                checks,
            })
            .await;
        }
        let summary = task.report.summary.clone().unwrap_or_default();
        self.relay(
            task.issuer_or(run.planner),
            worker,
            format!("{}\n\n{}", summary.trim(), brief::facts(&task)),
            None,
        );
        Ok(())
    }

    /// A verification that failed on the way: the task is `Passed` at once, with the error as
    /// its failure and no verified commit, so a plain accept refuses it and its issuer can
    /// still rework, drop or verify it again. Nothing is written if the round is gone (a
    /// rework, a newer verification, a cancelled run).
    async fn record_verification_error(
        &self,
        id: TaskId,
        worker: AgentId,
        token: &str,
        err: CoreError,
    ) {
        let run_id = match self.store.get_task(id).await {
            Ok(task) => task.run_id,
            Err(_) => return,
        };
        let _guard = self.lock(run_id).await;
        let mut task = match self.store.get_task(id).await {
            Ok(task) => task,
            Err(_) => return,
        };
        if !Self::still_verifying(&task, token) {
            tracing::debug!(task = %task.key, error = %err, "a verification failed after its round ended; dropping the error");
            return;
        }
        let why = err.to_string();
        task.report.verification = None;
        task.report.verified_sha = None;
        task.report.unchecked = task.spec.acceptance.is_empty();
        task.report.failure = Some(format!("its verification could not run: {why}"));
        if let Err(err) = self.transition(&mut task, TaskStatus::Passed).await {
            tracing::error!(task = %task.key, error = %err, "cannot pass a task whose verification failed");
            return;
        }
        tracing::warn!(task = %task.key, error = %why, "a verification failed; the report waits as unverified");
        let Ok(run) = self.store.get_run(run_id).await else {
            return;
        };
        let summary = task.report.summary.clone().unwrap_or_default();
        self.relay(
            task.issuer_or(run.planner),
            worker,
            format!(
                "{}\n\n{}\n\nNote: nothing verified this report - {why}. Accept it with force and a reason, verify it again (task_verify), rework it or drop it.",
                summary.trim(),
                brief::facts(&task)
            ),
            None,
        );
    }

    /// Whether `task` still waits for the verification `token` (nothing else wrote it).
    fn still_verifying(task: &Task, token: &str) -> bool {
        task.status == TaskStatus::Verifying && task.report.verification.as_deref() == Some(token)
    }

    /// Runs a task's checks again, or for the first time after the hub restarted before they
    /// finished (`interrupted`): a fresh verification id, so an older report cannot land on
    /// it. Its issuer only; the result arrives as a message.
    pub(crate) async fn task_verify(
        self: &Arc<Self>,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        let (run, task) = self.task_arg(caller, &input).await?;
        Self::require_issuer(&run, &task, caller)?;
        let _guard = self.lock(run.id).await;
        let mut task = self.store.get_task(task.id).await?;
        if task.status == TaskStatus::Verifying {
            return Ok(json!({
                "task": task_json(&task),
                "next": "Its checks are already running; the result arrives as a message.",
            }));
        }
        if task.status != TaskStatus::Passed {
            return Err(CoreError::Conflict(format!(
                "#{} is {:?}; only a task whose report is ready can be verified again",
                task.key, task.status
            )));
        }
        Self::worktree(&task)?;
        task.report.interrupted = false;
        task.report.forced = None;
        task.report.checks.clear();
        let token = uuid::Uuid::new_v4().to_string();
        task.report.verification = Some(token.clone());
        self.transition(&mut task, TaskStatus::Verifying).await?;
        let this = self.clone();
        let worker = task.assignee.unwrap_or(run.planner);
        let id = task.id;
        tokio::spawn(async move { this.report_done(id, worker, token).await });
        Ok(json!({
            "status": "verifying",
            "next": "End your turn; the checks' result arrives as a message.",
        }))
    }

    /// The assignee cannot finish: the task stalls and its issuer reads why.
    pub(crate) async fn give_up(
        &self,
        caller: &Agent,
        id: TaskId,
        reason: String,
    ) -> Result<(), CoreError> {
        let (run, _) = self.task_of(id).await?;
        let _guard = self.lock(run.id).await;
        let mut task = self.store.get_task(id).await?;
        Self::require_assignee(&task, caller)?;
        if task.status.is_terminal() {
            return Err(CoreError::Conflict(format!("#{} has ended", task.key)));
        }
        task.report.failure = Some(reason.clone());
        crate::attempts::close(
            &mut task,
            TaskRunOutcome::GaveUp,
            Some(reason.clone()),
            false,
        );
        if task.status != TaskStatus::Stalled {
            self.transition(&mut task, TaskStatus::Stalled).await?;
        } else {
            self.store_task(&mut task).await?;
        }
        self.relay(
            task.issuer_or(run.planner),
            caller.id,
            format!(
                "I cannot finish this: {}\n\n{}",
                reason.trim(),
                brief::facts(&task)
            ),
            None,
        );
        Ok(())
    }

    pub(crate) async fn ask(
        &self,
        caller: &Agent,
        id: TaskId,
        question: String,
    ) -> Result<Value, CoreError> {
        let (run, _) = self.task_of(id).await?;
        let _guard = self.lock(run.id).await;
        let mut task = self.store.get_task(id).await?;
        Self::require_assignee(&task, caller)?;
        if task.status == TaskStatus::WaitingAnswer
            && task.report.question.as_deref() == Some(&question)
        {
            return Ok(json!({ "question_id": task.report.question_message, "repeated": true }));
        }
        // Asked again while the first waits: the worker learned more (it synced, it read
        // further). The new wording replaces the open question; refusing it left the worker
        // unable to ask while its answer sat behind its own turn.
        let replaces = (task.status == TaskStatus::WaitingAnswer)
            .then_some(task.report.question_message)
            .flatten();
        let message = self.relay(
            task.issuer_or(run.planner),
            caller.id,
            format!(
                "{}\n\n---\nquestion about task {}",
                question.trim(),
                brief::task_ref(&task)
            ),
            None,
        );
        task.report.question = Some(question);
        task.report.question_message = Some(message);
        // The turn ended with a question; the answer starts the next run (multica §2.4).
        crate::attempts::close(&mut task, TaskRunOutcome::Asked, None, false);
        if replaces.is_some() {
            self.store_task(&mut task).await?;
        } else {
            self.transition(&mut task, TaskStatus::WaitingAnswer)
                .await?;
        }
        let mut out = json!({ "question_id": message, "next": "End your turn; the answer arrives as a message." });
        if let Some(old) = replaces {
            out["replaces"] = json!(old);
        }
        Ok(out)
    }

    /// The issuer answers the open question (or replies to the message that carried it).
    /// `true` when the worker reads it only after the turn it is in: it waits in its line.
    pub(crate) async fn answer(
        &self,
        caller: &Agent,
        id: TaskId,
        answer: String,
    ) -> Result<bool, CoreError> {
        let (run, _) = self.task_of(id).await?;
        let _guard = self.lock(run.id).await;
        let mut task = self.store.get_task(id).await?;
        Self::require_issuer(&run, &task, caller)?;
        if task.status != TaskStatus::WaitingAnswer {
            // An answer already given is not lost when the worker seems not to have it: it
            // waits behind the worker's running turn.
            return Err(CoreError::Conflict(format!(
                "#{} has no open question (it is {:?}); an answer you already gave reaches the worker when its current turn ends",
                task.key, task.status
            )));
        }
        let question = task.report.question_message.take();
        task.report.question = None;
        self.transition(&mut task, TaskStatus::Running).await?;
        let mut queued = false;
        if let Some(worker) = task.assignee {
            queued = self.host.is_busy(worker).await;
            // The answer starts the worker's next run (multica §2.4).
            let envelope = self.relay_envelope(
                caller.id,
                format!(
                    "{}\n\n---\nanswer about task #{}\ntask_id: {}",
                    answer.trim(),
                    task.key,
                    task.id
                ),
            );
            let envelope = crossagents_core::Envelope {
                reply_to: question,
                ..envelope
            };
            crate::attempts::begin(&mut task, Some(worker), Some(envelope.id));
            self.store_task(&mut task).await?;
            self.send_in_background(worker, envelope);
        }
        Ok(queued)
    }

    /// Closes an open question when the issuer answered with a plain reply to it.
    pub(crate) async fn reply_closes_question(
        &self,
        caller: &Agent,
        reply_to: crossagents_core::MessageId,
    ) {
        let Ok(runs) = self.store.list_runs().await else {
            return;
        };
        for run in runs.into_iter().filter(|r| r.status == RunStatus::Running) {
            let Ok(tasks) = self.store.list_tasks(run.id).await else {
                continue;
            };
            if let Some(task) = tasks.into_iter().find(|t| {
                t.status == TaskStatus::WaitingAnswer && t.report.question_message == Some(reply_to)
            }) {
                let _guard = self.lock(run.id).await;
                let Ok(mut task) = self.store.get_task(task.id).await else {
                    return;
                };
                if task.issuer_or(run.planner) != caller.id
                    || task.status != TaskStatus::WaitingAnswer
                {
                    return;
                }
                task.report.question = None;
                task.report.question_message = None;
                let _ = self.transition(&mut task, TaskStatus::Running).await;
                return;
            }
        }
    }

    // -----------------------------------------------------------------------------------
    // Checking (issuer, lead, reviewer)

    pub(crate) async fn workspace_diff(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            #[serde(default)]
            paths: Vec<String>,
            #[serde(default)]
            stat_only: bool,
        }
        let (run, task) = self.task_arg(caller, &input).await?;
        Self::require_insider(&run, &task, caller)?;
        let a: A = args(input)?;
        let dir = Self::worktree(&task)?;
        let (base, _) = self.base_of(&run, &task).await?;
        if a.stat_only {
            let stat = self.ws_for(&run).diff_stat(&dir, &base).await?;
            return Ok(json!({ "base": base, "stat": stat }));
        }
        let diff = self.ws_for(&run).diff(&dir, &base, &a.paths).await?;
        Ok(json!({ "base": base, "diff": clip(diff, READ_LIMIT) }))
    }

    pub(crate) async fn workspace_read(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            path: String,
            #[serde(default)]
            from_line: Option<usize>,
            #[serde(default)]
            to_line: Option<usize>,
        }
        let (run, task) = self.task_arg(caller, &input).await?;
        Self::require_insider(&run, &task, caller)?;
        let a: A = args(input)?;
        let dir = Self::worktree(&task)?;
        let path = inside(&dir, &a.path)?;
        let text = std::fs::read_to_string(&path)
            .map_err(|e| CoreError::Conflict(format!("cannot read {}: {e}", a.path)))?;
        let from = a.from_line.unwrap_or(1).max(1);
        let lines: Vec<&str> = text.lines().collect();
        let to = a.to_line.unwrap_or(lines.len()).min(lines.len());
        let slice = if from > to {
            String::new()
        } else {
            lines[from - 1..to].join("\n")
        };
        Ok(
            json!({ "path": a.path, "from_line": from, "to_line": to, "lines": lines.len(), "text": clip(slice, READ_LIMIT) }),
        )
    }

    pub(crate) async fn workspace_run(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            command: String,
            #[serde(default)]
            timeout_secs: Option<u64>,
        }
        let (run, task) = self.task_arg(caller, &input).await?;
        Self::require_insider(&run, &task, caller)?;
        if task.assignee == Some(caller.id) && task.issuer_or(run.planner) != caller.id {
            return Err(CoreError::Conflict(
                "run commands in your own shell; workspace_run is for whoever checks the task"
                    .into(),
            ));
        }
        let a: A = args(input)?;
        let dir = Self::worktree(&task)?;
        let limit = a
            .timeout_secs
            .map_or(RUN_DEFAULT, Duration::from_secs)
            .min(RUN_MAX);
        let started = Instant::now();
        let result = run_command(
            &dir,
            &a.command,
            limit,
            RUN_OUTPUT,
            &self.config.worktrees,
            &self.config.locks,
            self.config.shims.as_deref(),
        )
        .await;
        let duration_ms = started.elapsed().as_millis() as u64;
        self.publish(EventKind::TaskCommand {
            run_id: run.id,
            id: task.id,
            key: task.key.clone(),
            by: caller.id,
            command: a.command,
            exit_code: result.exit_code,
            duration_ms,
        })
        .await;
        Ok(
            json!({ "exit_code": result.exit_code, "passed": result.passed, "duration_ms": duration_ms, "output": result.output }),
        )
    }

    pub(crate) async fn checks_run(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        let (run, task) = self.task_arg(caller, &input).await?;
        Self::require_insider(&run, &task, caller)?;
        if task.spec.acceptance.is_empty() {
            return Err(CoreError::Conflict(format!(
                "#{} has no checks; use workspace_run, or add spec.acceptance with task_update",
                task.key
            )));
        }
        let dir = Self::worktree(&task)?;
        let checks = self.verifier.check(&dir, &task.spec.acceptance).await;
        let passed = all_passed(&checks);
        let _guard = self.lock(run.id).await;
        let mut task = self.store.get_task(task.id).await?;
        task.report.checks = checks.clone();
        self.store_task(&mut task).await?;
        self.publish(EventKind::TaskChecked {
            run_id: run.id,
            id: task.id,
            key: task.key.clone(),
            passed,
            checks: checks.clone(),
        })
        .await;
        Ok(json!({ "passed": passed, "summary": brief::checks(&checks), "checks": checks }))
    }

    /// Asks a reviewer (a teammate by name, else the task's or the run's reviewer) for a
    /// verdict on the task's branch.
    pub(crate) async fn review_request(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            #[serde(default)]
            reviewer: Option<String>,
            #[serde(default)]
            focus: Option<String>,
            /// `false`: the reviewer itself, with its conversation; unset, the task's plan
            /// decides for its own reviewer and a named one is a new execution session.
            #[serde(default)]
            fresh: Option<bool>,
            #[serde(default)]
            restore: bool,
        }
        let (run, task) = self.task_arg(caller, &input).await?;
        Self::require_issuer(&run, &task, caller)?;
        let a: A = args(input)?;
        let template = match &a.reviewer {
            // Any session may review: its role is how it was made, not what it may do
            // (whether it can take this one itself is checked below).
            Some(name) => self.store.get_agent(&AgentKey::parse(name)).await?.id,
            None => task
                .reviewer_template
                .or(run.config.reviewer)
                .ok_or_else(|| {
                    CoreError::Conflict("name a reviewer: this run has none by default".into())
                })?,
        };
        let fresh = a
            .fresh
            .unwrap_or(a.reviewer.is_some() || task.reviewer_fresh);
        let _guard = self.lock(run.id).await;
        let mut task = self.store.get_task(task.id).await?;
        if task.status == TaskStatus::Reviewing {
            return Ok(json!({ "task": task_json(&task), "repeated": true }));
        }
        if task.status != TaskStatus::Passed {
            return Err(CoreError::Conflict(format!(
                "#{} is {:?}; reviews are for tasks reported done",
                task.key, task.status
            )));
        }
        let cwd = Self::worktree(&task)?;
        self.ws_for(&run)
            .commit_all(&cwd, &format!("{}: {}", task.key, task.title))
            .await?;
        let reviewer = if fresh {
            self.fresh_agent(
                &task,
                template,
                &agent_name("r", &run, &task),
                Role::Reviewer,
                &cwd,
            )
            .await?
        } else {
            let me = self.store.get_agent(&AgentKey::Id(template)).await?;
            self.take_self(&run, &task, &me, ClaimRole::Reviewer, a.restore)
                .await?;
            self.store.get_agent(&AgentKey::Id(template)).await?
        };
        task.reviewer = Some(reviewer.id);
        self.transition(&mut task, TaskStatus::Reviewing).await?;
        let focus = a
            .focus
            .map(|f| format!("{}\n\n", f.trim()))
            .unwrap_or_default();
        // The judgements the reviewer is asked to answer pass/fail/na (EXPERTISE §6.3).
        let checklist = self
            .checklist_for(&reviewer, &task)
            .await
            .map(|text| format!("\n\n{text}"))
            .unwrap_or_default();
        self.relay(
            reviewer.id,
            caller.id,
            format!(
                "{focus}Review branch {} against {} (your working directory has it checked out).\n\n{}{checklist}\n\n{}",
                task.branch.as_deref().unwrap_or("?"),
                run.branch,
                brief::task(&task),
                brief::facts(&task)
            ),
            None,
        );
        Ok(json!({ "task": task_json(&task), "reviewer": reviewer.name }))
    }

    pub(crate) async fn review(
        &self,
        caller: &Agent,
        id: TaskId,
        approve: bool,
        notes: String,
    ) -> Result<(), CoreError> {
        let (run, _) = self.task_of(id).await?;
        let _guard = self.lock(run.id).await;
        let mut task = self.store.get_task(id).await?;
        if task.reviewer != Some(caller.id) || task.status != TaskStatus::Reviewing {
            return Err(CoreError::Conflict(
                "you are not reviewing this task".into(),
            ));
        }
        // Its review is in; the run lets go of the reviewer.
        task.reviewer = None;
        let _ = self
            .let_go_task(run.id, task.id, Some(ClaimRole::Reviewer))
            .await;
        task.report.review = Some(notes.clone());
        self.transition(&mut task, TaskStatus::Passed).await?;
        self.relay(
            task.issuer_or(run.planner),
            caller.id,
            format!(
                "Review of {}: {}\n{}",
                brief::task_ref(&task),
                if approve {
                    "approved"
                } else {
                    "changes needed"
                },
                notes.trim()
            ),
            None,
        );
        Ok(())
    }

    // -----------------------------------------------------------------------------------
    // Deciding (issuer)

    /// Merges the task's verified commit into the run branch. A conflict leaves everything as
    /// it was and returns the conflicting hunks.
    ///
    /// The gate (docs/TODO.md §九 ①): a task being verified is refused, `force` or not. A
    /// plain accept needs every check to have passed and nothing to have changed since the
    /// checks; it merges the verified commit and nothing else. Failed or missing checks, a
    /// task with none, an interrupted verification, a stalled task and work done after the
    /// checks are accepted only with `force` and a `reason`, which is recorded.
    pub(crate) async fn task_accept(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            #[serde(default)]
            message: Option<String>,
            /// Accept although nothing verified it (or it changed since); `reason` is required
            /// and recorded.
            #[serde(default)]
            force: bool,
            #[serde(default)]
            reason: Option<String>,
        }
        let (run, task) = self.task_arg(caller, &input).await?;
        Self::require_issuer(&run, &task, caller)?;
        let a: A = args(input)?;
        let _guard = self.lock(run.id).await;
        let mut run = self.store.get_run(run.id).await?;
        let mut task = self.store.get_task(task.id).await?;
        if task.status == TaskStatus::Merged {
            return Ok(json!({ "task": task_json(&task), "repeated": true }));
        }
        if task.status == TaskStatus::Verifying {
            return Err(CoreError::Conflict(format!(
                "#{} is still being verified; its checks' result arrives as a message, then accept it",
                task.key
            )));
        }
        if !matches!(task.status, TaskStatus::Passed | TaskStatus::Stalled) {
            return Err(CoreError::Conflict(format!(
                "#{} is {:?}; accept a task once it is reported done (or stalled with work you want)",
                task.key, task.status
            )));
        }
        let dir = Self::worktree(&task)?;
        let (base, into) = self.base_of(&run, &task).await?;
        // What keeps this from being a plain accept: why, in the issuer's words.
        let mut unchecked: Vec<String> = Vec::new();
        if task.report.interrupted {
            unchecked.push("the hub restarted before its checks finished".into());
        }
        if task.spec.acceptance.is_empty() {
            unchecked.push("it has no checks (spec.acceptance is empty)".into());
        } else if task.report.checks.is_empty() {
            unchecked.push("its checks never ran".into());
        } else if !all_passed(&task.report.checks) {
            unchecked.push("its checks did not pass".into());
        }
        if task.status == TaskStatus::Stalled {
            unchecked.push("it is stalled: its worker never reported".into());
        }
        let verified = task
            .report
            .verified_sha
            .clone()
            .filter(|_| !task.report.interrupted);
        match (&verified, dir.exists()) {
            (None, _) => {
                if task.status == TaskStatus::Passed {
                    unchecked.push("nothing verified its report".into());
                }
            }
            (Some(_), true) => {
                let head = self.ws_for(&run).head(&dir).await.ok();
                if let Some(verified) = &verified
                    && head.as_deref().map(str::trim) != Some(verified.trim())
                {
                    unchecked.push("it changed after its checks".into());
                }
                let dirty = self
                    .ws_for(&run)
                    .uncommitted(&dir)
                    .await
                    .unwrap_or_default();
                if !dirty.is_empty() {
                    unchecked.push(format!(
                        "it has uncommitted changes since its checks: {}",
                        dirty.join(", ")
                    ));
                }
            }
            (Some(_), false) => {}
        }
        let reason = a
            .reason
            .map(|r| r.trim().to_owned())
            .filter(|r| !r.is_empty());
        if !unchecked.is_empty() && (!a.force || reason.is_none()) {
            return Err(CoreError::Conflict(format!(
                "#{} is not verified: {}. Accept it anyway with force: true and a reason, or send it back (task_rework), or have it verified again (task_verify).",
                task.key,
                unchecked.join("; ")
            )));
        }
        // What this accept stands on: the verified commit, else what the worker left now.
        let merge_target = match (&verified, unchecked.is_empty()) {
            (Some(sha), true) => Some(sha.clone()),
            _ => None,
        };
        // What was actually merged, for the answer: the verified commit, or (forced) the
        // commit that holds what is in the worktree now. Never the verified one for a forced
        // merge: that would name a commit the run branch does not have.
        let mut merged_sha = merge_target.clone();
        // No worktree left: an earlier accept merged it and cleaned up, then failed before
        // recording that; this one only records it.
        if dir.exists() {
            // The issuer's, written to the project's rules; the hub cannot read those.
            let message = a
                .message
                .as_deref()
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{}: {}", task.key, task.title));
            let target = match merge_target {
                Some(sha) => sha,
                None => {
                    // Forced: what is there now, never checked as it is.
                    self.ws_for(&run).commit_all(&dir, &message).await?;
                    merged_sha = self.ws_for(&run).head(&dir).await.ok();
                    task.branch.clone().unwrap_or_default()
                }
            };
            if base == run.branch {
                // The lead's own edits in the run's checkout would block the merge.
                self.commit_lead(&run, &format!("crossagents: {}", run.title))
                    .await?;
            }
            if let Err(err) = self.ws_for(&run).merge(&into, &target, &message).await {
                return Ok(json!({
                    "merged": false,
                    "conflict": err.to_string(),
                    "next": "Resolve it: task_rework (the worker can branch_sync and fix it) or task_drop.",
                }));
            }
        }
        if let Some(reason) = &reason
            && !unchecked.is_empty()
        {
            task.report.forced = Some(reason.clone());
            task.progress = Some(format!(
                "merged without a verification, at {}'s word: {reason}",
                caller.name
            ));
        }
        crossagents_core::Handoff::end(
            &mut task.handoffs,
            task.assignee,
            HandoffOutcome::Delivered,
        );
        let released = self.release_agents(&mut task).await;
        self.cleanup_worktree(&run, &task, true, released.is_empty())
            .await;
        task.report.failure = None;
        // Accepting a stalled task stands in for the report its worker never sent.
        if task.status == TaskStatus::Stalled {
            self.transition(&mut task, TaskStatus::Passed).await?;
        }
        // Before the merge is announced, so whoever reads the run on that event sees it.
        self.sync_resolved(&mut run, &task).await?;
        self.transition(&mut task, TaskStatus::Merged).await?;
        self.schedule(&mut run).await?;
        let verified_word = if unchecked.is_empty() {
            json!("verified")
        } else {
            json!("forced")
        };
        // How this task ended lands on the expertise entries it used (EXPERTISE §4.6). The
        // merge is done: a profile that cannot be updated must not make an accepted task look
        // refused to its issuer.
        let outcome = if crate::expertise::tools::was_reworked(&task) {
            UsageOutcome::Reworked
        } else {
            UsageOutcome::MergedFirstTry
        };
        if let Err(err) = self.settle_usage(&task, outcome).await {
            tracing::warn!(task = %task.key, error = %err, "cannot settle expertise usage");
        }
        // What it did right is worth keeping: an example for the person (EXPERTISE §6.4). The
        // merge stands even when the profile cannot hold the candidate.
        if outcome == UsageOutcome::MergedFirstTry
            && let Err(err) = self.candidate_from_first_try(&task).await
        {
            tracing::warn!(task = %task.key, error = %err, "cannot propose an expertise candidate");
        }
        Ok(json!({
            "merged": true,
            "task": task_json(&task),
            "into": base,
            "as": verified_word,
            "merged_sha": merged_sha,
            "unverified": unchecked,
        }))
    }

    pub(crate) async fn task_rework(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            notes: String,
        }
        let (run, task) = self.task_arg(caller, &input).await?;
        Self::require_issuer(&run, &task, caller)?;
        let a: A = args(input)?;
        let _guard = self.lock(run.id).await;
        let mut task = self.store.get_task(task.id).await?;
        if task.status == TaskStatus::Running {
            return Ok(json!({ "task": task_json(&task), "repeated": true }));
        }
        if !matches!(
            task.status,
            TaskStatus::Passed | TaskStatus::Stalled | TaskStatus::Reviewing
        ) {
            return Err(CoreError::Conflict(format!(
                "#{} is {:?}",
                task.key, task.status
            )));
        }
        let Some(worker) = task.assignee else {
            return Err(CoreError::Conflict(format!(
                "#{} has no worker any more; task_assign it",
                task.key
            )));
        };
        if task.reviewer.take().is_some() {
            let _ = self
                .let_go_task(run.id, task.id, Some(ClaimRole::Reviewer))
                .await;
        }
        // A new round: an older verification's report must not land on it.
        task.report.verification = None;
        crossagents_core::Handoff::end(&mut task.handoffs, Some(worker), HandoffOutcome::Reworked);
        task.attempts += 1;
        task.report.failure = None;
        task.report.rework.push(a.notes.trim().to_owned());
        self.transition(&mut task, TaskStatus::Running).await?;
        // The rework is the worker's next run (multica §2.4).
        let envelope = self.relay_envelope(
            caller.id,
            format!(
                "{}\n\n---\nrework of task #{}\ntask_id: {}\nattempt: {}",
                a.notes.trim(),
                task.key,
                task.id,
                task.attempts
            ),
        );
        crate::attempts::begin(&mut task, Some(worker), Some(envelope.id));
        self.store_task(&mut task).await?;
        self.send_in_background(worker, envelope);
        // What went wrong, in the issuer's words, becomes a draft for the person (EXPERTISE
        // §6.4). The task is already back with its worker: a profile that cannot hold the
        // candidate only warns.
        if let Err(err) = self.candidate_from_rework(&task, a.notes.trim()).await {
            tracing::warn!(task = %task.key, error = %err, "cannot propose an expertise candidate");
        }
        Ok(json!({ "task": task_json(&task) }))
    }

    /// Discards the task's result. With `requeue` the task starts over (fresh worktree,
    /// possibly another worker after task_assign); otherwise it is cancelled.
    pub(crate) async fn task_drop(&self, caller: &Agent, input: Value) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            #[serde(default)]
            requeue: bool,
            #[serde(default)]
            reason: Option<String>,
        }
        let (run, task) = self.task_arg(caller, &input).await?;
        Self::require_issuer(&run, &task, caller)?;
        let a: A = args(input)?;
        let _guard = self.lock(run.id).await;
        let mut run = self.store.get_run(run.id).await?;
        let mut task = self.store.get_task(task.id).await?;
        if task.status.is_terminal() {
            return Ok(json!({ "task": task_json(&task), "repeated": true }));
        }
        if task.status == TaskStatus::Verifying {
            return Err(CoreError::Conflict(format!(
                "#{} is still being verified; wait for its checks (the result arrives as a message), then drop it",
                task.key
            )));
        }
        // Its result is thrown away: that execution counts as cancelled.
        crossagents_core::Handoff::end(
            &mut task.handoffs,
            task.assignee,
            HandoffOutcome::Cancelled,
        );
        let released = self.release_agents(&mut task).await;
        self.cleanup_worktree(&run, &task, true, released.is_empty())
            .await;
        task.worktree = None;
        task.branch = None;
        task.progress = None;
        task.report = TaskReport {
            failure: a.reason.clone(),
            ..TaskReport::default()
        };
        let to = if a.requeue {
            TaskStatus::Pending
        } else {
            TaskStatus::Cancelled
        };
        crate::attempts::close_open(&mut task, TaskRunOutcome::Cancelled, a.reason.clone());
        self.transition(&mut task, to).await?;
        // What it used counts as dropped, requeued or not (EXPERTISE §4.6), and the drop
        // itself stands even when the profile cannot be updated.
        if let Err(err) = self.settle_usage(&task, UsageOutcome::Dropped).await {
            tracing::warn!(task = %task.key, error = %err, "cannot settle expertise usage");
        }
        self.schedule(&mut run).await?;
        Ok(json!({ "task": task_json(&task) }))
    }

    /// Brings the run branch (what others merged since) into the task's worktree.
    pub(crate) async fn branch_sync(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        let (run, task) = self.task_arg(caller, &input).await?;
        if task.assignee != Some(caller.id) {
            Self::require_issuer(&run, &task, caller)?;
        }
        let dir = Self::worktree(&task)?;
        self.ws_for(&run)
            .commit_all(&dir, &format!("{}: work in progress", task.key))
            .await?;
        let (base, _) = self.base_of(&run, &task).await?;
        match self
            .ws_for(&run)
            .merge(
                &dir,
                &base,
                &format!("crossagents: sync {} with {base}", task.key),
            )
            .await
        {
            Ok(()) => Ok(json!({ "synced": true, "from": base })),
            Err(err) => Ok(json!({ "synced": false, "conflict": err.to_string() })),
        }
    }
}

/// `rel` inside `root`; paths leaving it are refused.
fn inside(root: &Path, rel: &str) -> Result<PathBuf, CoreError> {
    let rel = Path::new(rel);
    if rel.is_absolute()
        || rel.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err(CoreError::Conflict(format!(
            "{} must be a path inside the task's worktree",
            rel.display()
        )));
    }
    Ok(root.join(rel))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside() {
        let root = Path::new("/w");
        assert!(inside(root, "src/a.rs").is_ok());
        assert!(inside(root, "../x").is_err());
        assert!(inside(root, "/etc/passwd").is_err());
        assert_eq!(clip("abcdef".into(), 3).lines().next(), Some("abc"));
    }
}
