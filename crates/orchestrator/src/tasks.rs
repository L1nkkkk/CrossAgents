//! The task tools (docs/tasks/H1.md §5.7–5.11): the issuer creates, checks, accepts, reworks
//! and drops tasks; the assignee reports; a reviewer gives a verdict. The hub carries out
//! each decision and relays what each side wrote — it decides nothing itself.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AgentKey, CoreError, EventKind, PlannedTask, Role, Run, RunStatus, Task,
    TaskId, TaskReport, TaskSpec, TaskStatus,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::brief;
use crate::engine::{Orchestrator, agent_name};
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
        let mut value = task_json(&task);
        value["spec"] = json!(task.spec);
        value["report"] = json!(task.report);
        value["branch"] = json!(task.branch);
        value["worktree"] = json!(task.worktree);
        value["run_branch"] = json!(run.branch);
        value["reviewer"] = json!(task.reviewer);
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
                key: task.key.clone(),
                from: TaskStatus::Pending,
                to: TaskStatus::Pending,
                assignee: None,
            })
            .await;
            created.push(task_json(&task));
        }
        self.schedule(&mut run).await?;
        let repeated: Vec<String> = repeated.into_iter().map(|t| t.key).collect();
        Ok(json!({ "created": created, "repeated": repeated }))
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
        }
        let (run, task) = self.task_arg(caller, &input).await?;
        Self::require_issuer(&run, &task, caller)?;
        let a: A = args(input)?;
        let to = self.store.get_agent(&AgentKey::parse(&a.to)).await?;
        if to.role != Role::Worker {
            return Err(CoreError::Conflict(format!("{} is not a worker", to.name)));
        }
        let _guard = self.lock(run.id).await;
        let mut task = self.store.get_task(task.id).await?;
        if task.status.is_terminal() {
            return Err(CoreError::Conflict(format!("#{} has ended", task.key)));
        }
        task.worker_template = Some(to.id);
        if task.status == TaskStatus::Pending {
            self.store_task(&mut task).await?;
            return Ok(task_json(&task));
        }
        if let Some(old) = task.assignee.take()
            && let Err(err) = self.host.remove_agent(old).await
        {
            tracing::warn!(%old, error = %err, "cannot remove agent");
        }
        let cwd = Self::worktree(&task)?;
        let worker = self
            .fresh_agent(to.id, &agent_name("w", &run, &task), Role::Worker, &cwd)
            .await?;
        task.assignee = Some(worker.id);
        task.report.question = None;
        if task.status != TaskStatus::Running {
            self.transition(&mut task, TaskStatus::Running).await?;
        } else {
            self.store_task(&mut task).await?;
        }
        self.relay(
            worker.id,
            caller.id,
            format!(
                "{}\n\nSomeone already worked on this; their changes are in your working directory.",
                brief::assignment(&task)
            ),
            None,
        );
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
        if task.status.is_terminal() {
            return Err(CoreError::Conflict(format!("#{} has ended", task.key)));
        }
        self.release_agents(&mut task).await;
        self.cleanup_worktree(&run, &task, !a.keep_branch).await;
        task.report.failure = Some(format!("cancelled by {}", caller.name));
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

    /// The assignee says it is done: the task waits for its issuer at once (so the end of
    /// this turn does not count as stalling); the hub commits, runs any checks, and relays
    /// the worker's summary with the facts.
    pub(crate) async fn complete(
        self: &Arc<Self>,
        caller: &Agent,
        id: TaskId,
        summary: String,
    ) -> Result<(), CoreError> {
        let (run, _) = self.task_of(id).await?;
        let _guard = self.lock(run.id).await;
        let mut task = self.store.get_task(id).await?;
        Self::require_assignee(&task, caller)?;
        if task.status == TaskStatus::Passed {
            return Ok(()); // a retried call
        }
        if !matches!(task.status, TaskStatus::Running | TaskStatus::Stalled) {
            return Err(CoreError::Conflict(format!(
                "#{} is {:?}; only a task being worked on can be completed",
                task.key, task.status
            )));
        }
        task.report.summary = Some(summary);
        task.report.failure = None;
        self.transition(&mut task, TaskStatus::Passed).await?;
        let this = self.clone();
        let worker = caller.id;
        tokio::spawn(async move {
            if let Err(err) = this.report_done(id, worker).await {
                tracing::error!(task = %id, error = %err, "cannot record a finished task");
            }
        });
        Ok(())
    }

    async fn report_done(&self, id: TaskId, worker: AgentId) -> Result<(), CoreError> {
        let task = self.store.get_task(id).await?;
        let run = self.store.get_run(task.run_id).await?;
        let dir = Self::worktree(&task)?;
        self.ws_for(&run)
            .commit_all(&dir, &format!("{}: {}", task.key, task.title))
            .await?;
        let (base, _) = self.base_of(&run, &task).await?;
        let diff_stat = self.ws_for(&run).diff_stat(&dir, &base).await.ok();
        let checks = if task.spec.acceptance.is_empty() {
            Vec::new()
        } else {
            self.verifier.check(&dir, &task.spec.acceptance).await
        };
        let _guard = self.lock(run.id).await;
        let mut task = self.store.get_task(id).await?;
        task.report.diff_stat = diff_stat;
        task.report.checks = checks.clone();
        self.store_task(&mut task).await?;
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
        self.transition(&mut task, TaskStatus::WaitingAnswer)
            .await?;
        Ok(
            json!({ "question_id": message, "next": "End your turn; the answer arrives as a message." }),
        )
    }

    /// The issuer answers the open question (or replies to the message that carried it).
    pub(crate) async fn answer(
        &self,
        caller: &Agent,
        id: TaskId,
        answer: String,
    ) -> Result<(), CoreError> {
        let (run, _) = self.task_of(id).await?;
        let _guard = self.lock(run.id).await;
        let mut task = self.store.get_task(id).await?;
        Self::require_issuer(&run, &task, caller)?;
        if task.status != TaskStatus::WaitingAnswer {
            return Err(CoreError::Conflict(format!(
                "#{} has no open question",
                task.key
            )));
        }
        let question = task.report.question_message.take();
        task.report.question = None;
        self.transition(&mut task, TaskStatus::Running).await?;
        if let Some(worker) = task.assignee {
            self.relay(
                worker,
                caller.id,
                format!(
                    "{}\n\n---\nanswer about task #{}\ntask_id: {}",
                    answer.trim(),
                    task.key,
                    task.id
                ),
                question,
            );
        }
        Ok(())
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
            let committed = self.ws_for(&run).diff_stat(&dir, &base).await?;
            return Ok(json!({ "base": base, "stat": committed }));
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
        let result = run_command(&dir, &a.command, limit, RUN_OUTPUT).await;
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
        }
        let (run, task) = self.task_arg(caller, &input).await?;
        Self::require_issuer(&run, &task, caller)?;
        let a: A = args(input)?;
        let template = match &a.reviewer {
            Some(name) => {
                let agent = self.store.get_agent(&AgentKey::parse(name)).await?;
                if agent.role != Role::Reviewer {
                    return Err(CoreError::Conflict(format!(
                        "{} is not a reviewer",
                        agent.name
                    )));
                }
                agent.id
            }
            None => task
                .reviewer_template
                .or(run.config.reviewer)
                .ok_or_else(|| {
                    CoreError::Conflict("name a reviewer: this run has none by default".into())
                })?,
        };
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
        let reviewer = self
            .fresh_agent(
                template,
                &agent_name("r", &run, &task),
                Role::Reviewer,
                &cwd,
            )
            .await?;
        task.reviewer = Some(reviewer.id);
        self.transition(&mut task, TaskStatus::Reviewing).await?;
        let focus = a
            .focus
            .map(|f| format!("{}\n\n", f.trim()))
            .unwrap_or_default();
        self.relay(
            reviewer.id,
            caller.id,
            format!(
                "{focus}Review branch {} against {} (your working directory has it checked out).\n\n{}\n\n{}",
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
        if let Some(reviewer) = task.reviewer.take() {
            let _ = self.host.remove_agent(reviewer).await;
        }
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

    /// Merges the task's branch into the run branch. A conflict leaves everything as it was
    /// and returns the conflicting hunks.
    pub(crate) async fn task_accept(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        let (run, task) = self.task_arg(caller, &input).await?;
        Self::require_issuer(&run, &task, caller)?;
        let _guard = self.lock(run.id).await;
        let mut run = self.store.get_run(run.id).await?;
        let mut task = self.store.get_task(task.id).await?;
        if task.status == TaskStatus::Merged {
            return Ok(json!({ "task": task_json(&task), "repeated": true }));
        }
        if !matches!(task.status, TaskStatus::Passed | TaskStatus::Stalled) {
            return Err(CoreError::Conflict(format!(
                "#{} is {:?}; accept a task once it is reported done (or stalled with work you want)",
                task.key, task.status
            )));
        }
        let dir = Self::worktree(&task)?;
        let (base, into) = self.base_of(&run, &task).await?;
        // No worktree left: an earlier accept merged it and cleaned up, then failed before
        // recording that; this one only records it.
        if dir.exists() {
            // The issuer's, written to the project's rules; the hub cannot read those.
            let message = input
                .get("message")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{}: {}", task.key, task.title));
            self.ws_for(&run).commit_all(&dir, &message).await?;
            let branch = task.branch.clone().unwrap_or_default();
            if let Err(err) = self.ws_for(&run).merge(&into, &branch, &message).await {
                return Ok(json!({
                    "merged": false,
                    "conflict": err.to_string(),
                    "next": "Resolve it: task_rework (the worker can branch_sync and fix it) or task_drop.",
                }));
            }
        }
        self.release_agents(&mut task).await;
        self.cleanup_worktree(&run, &task, true).await;
        task.report.failure = None;
        // Accepting a stalled task stands in for the report its worker never sent.
        if task.status == TaskStatus::Stalled {
            self.transition(&mut task, TaskStatus::Passed).await?;
        }
        // Before the merge is announced, so whoever reads the run on that event sees it.
        self.sync_resolved(&mut run, &task).await?;
        self.transition(&mut task, TaskStatus::Merged).await?;
        self.schedule(&mut run).await?;
        Ok(json!({ "merged": true, "task": task_json(&task), "into": base }))
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
        if let Some(reviewer) = task.reviewer.take() {
            let _ = self.host.remove_agent(reviewer).await;
        }
        task.attempts += 1;
        task.report.failure = None;
        self.transition(&mut task, TaskStatus::Running).await?;
        self.relay(
            worker,
            caller.id,
            format!(
                "{}\n\n---\nrework of task #{}\ntask_id: {}\nattempt: {}",
                a.notes.trim(),
                task.key,
                task.id,
                task.attempts
            ),
            None,
        );
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
        self.release_agents(&mut task).await;
        self.cleanup_worktree(&run, &task, true).await;
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
        self.transition(&mut task, to).await?;
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
