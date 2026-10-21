//! The fixed pipeline (docs/DESIGN.md §8): plan → approve → dispatch → verify → retry /
//! escalate → review → merge → summarize.
//!
//! Every state change of a run happens under that run's lock. Long work (agent turns,
//! acceptance commands) happens outside it; messages to agents are delivered in the
//! background so a slow agent start never blocks the run.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AgentKey, AgentStatus, CheckResult, CoreError, EventBus, EventKind,
    MessageSource, PlannedTask, Role, Run, RunConfig, RunId, RunStatus, Store, Task, TaskId,
    TaskReport, TaskStatus,
};
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::host::AgentHost;
use crate::plan;
use crate::prompts;
use crate::verify::{Verifier, all_passed};
use crate::workspace::{WorkspaceProvider, worktree_path};

pub struct OrchestratorConfig {
    /// Root for run and task worktrees.
    pub worktrees: PathBuf,
    /// Reminders before a silent turn counts as a failed attempt.
    pub max_nudges: u32,
}

/// Everything needed to start a run.
#[derive(Debug, Clone)]
pub struct NewRun {
    pub request: String,
    pub title: Option<String>,
    pub planner: AgentId,
    pub config: RunConfig,
}

pub struct Orchestrator {
    store: Arc<dyn Store>,
    bus: Arc<EventBus>,
    host: Arc<dyn AgentHost>,
    workspaces: Arc<dyn WorkspaceProvider>,
    verifier: Arc<dyn Verifier>,
    config: OrchestratorConfig,
    locks: Mutex<HashMap<RunId, Arc<Mutex<()>>>>,
}

/// Why a worker's attempt did not produce an acceptable result.
enum Failure {
    Checks(Vec<CheckResult>),
    Rejected(String),
    Silent,
    GaveUp(String),
}

fn short(id: impl ToString) -> String {
    let s = id.to_string().replace('-', "");
    s[s.len().saturating_sub(8)..].to_owned()
}

impl Orchestrator {
    pub fn new(
        store: Arc<dyn Store>,
        bus: Arc<EventBus>,
        host: Arc<dyn AgentHost>,
        workspaces: Arc<dyn WorkspaceProvider>,
        verifier: Arc<dyn Verifier>,
        config: OrchestratorConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            bus,
            host,
            workspaces,
            verifier,
            config,
            locks: Mutex::default(),
        })
    }

    pub fn store(&self) -> &Arc<dyn Store> {
        &self.store
    }

    pub(crate) async fn lock(&self, run: RunId) -> OwnedMutexGuard<()> {
        let lock = self.locks.lock().await.entry(run).or_default().clone();
        lock.lock_owned().await
    }

    fn integration_path(&self, run: &Run) -> PathBuf {
        worktree_path(&self.config.worktrees, &short(run.id), "integration")
    }

    async fn publish(&self, kind: EventKind) {
        if let Err(err) = self.bus.publish(kind).await {
            tracing::error!(error = %err, "cannot publish event");
        }
    }

    /// Sends in the background: starting an agent can take minutes.
    pub(crate) fn deliver(&self, agent: AgentId, from: MessageSource, text: String) {
        let host = self.host.clone();
        tokio::spawn(async move {
            if let Err(err) = host.send(agent, from, text).await {
                tracing::warn!(%agent, error = %err, "cannot deliver message");
            }
        });
    }

    async fn set_run_status(
        &self,
        run: &mut Run,
        to: RunStatus,
        note: Option<String>,
    ) -> Result<(), CoreError> {
        let from = run.status;
        run.status = to;
        if note.is_some() {
            run.note = note.clone();
        }
        run.updated_at = Utc::now();
        self.store.update_run(run).await?;
        if from != to {
            self.publish(EventKind::RunStatusChanged {
                id: run.id,
                from,
                to,
                note,
            })
            .await;
        }
        Ok(())
    }

    async fn transition(&self, task: &mut Task, to: TaskStatus) -> Result<(), CoreError> {
        let from = task.status;
        task.status = from.transition(to)?;
        task.updated_at = Utc::now();
        self.store.update_task(task).await?;
        self.publish(EventKind::TaskStatusChanged {
            run_id: task.run_id,
            id: task.id,
            key: task.key.clone(),
            from,
            to,
            assignee: task.assignee,
        })
        .await;
        Ok(())
    }

    // -----------------------------------------------------------------------------------
    // Runs

    pub async fn create_run(&self, new: NewRun) -> Result<Run, CoreError> {
        let planner = self.store.get_agent(&AgentKey::Id(new.planner)).await?;
        self.store
            .get_agent(&AgentKey::Id(new.config.worker))
            .await?;
        for extra in [new.config.escalate, new.config.reviewer]
            .into_iter()
            .flatten()
        {
            self.store.get_agent(&AgentKey::Id(extra)).await?;
        }
        if new.request.trim().is_empty() {
            return Err(CoreError::Conflict("the request is empty".into()));
        }
        let repo = planner.cwd.clone();
        let mut new = new;
        let team = self.gather_team(&new.config, &repo).await?;
        new.config.team = team.iter().map(|a| a.id).collect();
        let base_branch = self.workspaces.current_branch(&repo).await?;
        let id = RunId::new();
        let now = Utc::now();
        let mut run = Run {
            id,
            title: new.title.unwrap_or_else(|| first_line(&new.request, 60)),
            request: new.request,
            planner: planner.id,
            status: RunStatus::Planning,
            config: new.config,
            repo: repo.clone(),
            base_branch: base_branch.clone(),
            branch: format!("crossagents/{}", short(id)),
            note: None,
            plan: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        run.config.max_parallel = run.config.max_parallel.max(1);
        run.config.max_attempts = run.config.max_attempts.max(1);
        self.workspaces
            .create(
                &repo,
                &run.branch,
                &base_branch,
                &self.integration_path(&run),
            )
            .await?;
        self.store.insert_run(&run).await?;
        self.publish(EventKind::RunCreated { run: run.clone() })
            .await;
        self.deliver(
            planner.id,
            MessageSource::run(run.id),
            prompts::planner_kickoff(&run, &team, run.config.worker),
        );
        Ok(run)
    }

    /// The run's team: the given teammates, or every worker and reviewer registered in the
    /// repository (clones live in worktrees, so they are not picked up). The run's worker and
    /// reviewer always belong to it, first.
    async fn gather_team(&self, config: &RunConfig, repo: &Path) -> Result<Vec<Agent>, CoreError> {
        let mut team: Vec<Agent> = Vec::new();
        let mut add = |agent: Agent| {
            if !team.iter().any(|a| a.id == agent.id) {
                team.push(agent);
            }
        };
        for id in [Some(config.worker), config.reviewer].into_iter().flatten() {
            add(self.store.get_agent(&AgentKey::Id(id)).await?);
        }
        if config.team.is_empty() {
            for agent in self.store.list_agents().await? {
                if agent.role != Role::Planner && agent.cwd == repo {
                    add(agent);
                }
            }
        } else {
            for id in &config.team {
                let agent = self.store.get_agent(&AgentKey::Id(*id)).await?;
                if agent.role == Role::Planner {
                    return Err(CoreError::Conflict(format!(
                        "{} is a planner and cannot be on a run's team",
                        agent.name
                    )));
                }
                add(agent);
            }
        }
        Ok(team)
    }

    /// The run's teammates. Runs created before teams existed have just their worker and
    /// reviewer.
    async fn team_of(&self, run: &Run) -> Result<Vec<Agent>, CoreError> {
        let ids: Vec<AgentId> = if run.config.team.is_empty() {
            [Some(run.config.worker), run.config.reviewer]
                .into_iter()
                .flatten()
                .collect()
        } else {
            run.config.team.clone()
        };
        let mut team = Vec::new();
        for id in ids {
            // A teammate removed since the run started is simply no longer assignable.
            if let Ok(agent) = self.store.get_agent(&AgentKey::Id(id)).await {
                team.push(agent);
            }
        }
        Ok(team)
    }

    /// Resolves each planned task's `assignee` and `reviewer` to teammates of the right role.
    async fn resolve_assignments(
        &self,
        run: &Run,
        tasks: &[PlannedTask],
    ) -> Result<Vec<(Option<AgentId>, Option<AgentId>)>, CoreError> {
        let team = self.team_of(run).await?;
        let names = |role: Role| {
            team.iter()
                .filter(|a| a.role == role)
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut problems = Vec::new();
        let mut pick = |task: &str, name: &Option<String>, role: Role, field: &str| {
            let name = name.as_deref()?;
            match team.iter().find(|a| a.name == name && a.role == role) {
                Some(agent) => Some(agent.id),
                None => {
                    let known = names(role);
                    problems.push(format!(
                        "task {task:?}: {field} {name:?} is not a {} on the team (available: {})",
                        role_word(role),
                        if known.is_empty() { "none" } else { &known }
                    ));
                    None
                }
            }
        };
        let resolved = tasks
            .iter()
            .map(|t| {
                (
                    pick(&t.key, &t.assignee, Role::Worker, "assignee"),
                    pick(&t.key, &t.reviewer, Role::Reviewer, "reviewer"),
                )
            })
            .collect();
        if problems.is_empty() {
            Ok(resolved)
        } else {
            Err(CoreError::Conflict(format!(
                "the plan is invalid:\n{}",
                problems.join("\n")
            )))
        }
    }

    /// The newest run in `Planning` led by `planner`.
    async fn planning_run_of(&self, planner: AgentId) -> Result<Run, CoreError> {
        self.store
            .list_runs()
            .await?
            .into_iter()
            .find(|r| r.planner == planner && r.status == RunStatus::Planning)
            .ok_or_else(|| CoreError::Conflict("you have no run waiting for a plan".into()))
    }

    pub(crate) async fn submit_plan(
        &self,
        caller: &Agent,
        tasks: Vec<PlannedTask>,
    ) -> Result<Run, CoreError> {
        let run = self.planning_run_of(caller.id).await?;
        let _guard = self.lock(run.id).await;
        let mut run = self.store.get_run(run.id).await?;
        if run.status != RunStatus::Planning {
            return Err(CoreError::Conflict(
                "the run is no longer waiting for a plan".into(),
            ));
        }
        plan::validate(&tasks)
            .map_err(|problems| CoreError::Conflict(format!("the plan is invalid:\n{problems}")))?;
        self.resolve_assignments(&run, &tasks).await?;
        let keys = tasks.iter().map(|t| t.key.clone()).collect();
        run.plan = tasks;
        run.note = None;
        self.set_run_status(&mut run, RunStatus::AwaitingApproval, None)
            .await?;
        self.publish(EventKind::RunPlanSubmitted {
            id: run.id,
            tasks: keys,
        })
        .await;
        if run.config.auto_approve {
            self.approve_locked(&mut run).await?;
        }
        Ok(run)
    }

    pub async fn approve(&self, id: RunId) -> Result<Run, CoreError> {
        let _guard = self.lock(id).await;
        let mut run = self.store.get_run(id).await?;
        self.approve_locked(&mut run).await?;
        Ok(run)
    }

    async fn approve_locked(&self, run: &mut Run) -> Result<(), CoreError> {
        if run.status != RunStatus::AwaitingApproval {
            return Err(CoreError::Conflict(format!(
                "run is {:?}, not awaiting approval",
                run.status
            )));
        }
        let now = Utc::now();
        let assignments = self.resolve_assignments(run, &run.plan).await?;
        for (planned, (worker_template, reviewer_template)) in run.plan.iter().zip(assignments) {
            let task = Task {
                id: TaskId::new(),
                run_id: run.id,
                key: planned.key.clone(),
                title: planned.title.clone(),
                spec: planned.spec.clone(),
                depends_on: planned.depends_on.clone(),
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
                created_at: now,
                updated_at: now,
            };
            self.store.insert_task(&task).await?;
        }
        self.set_run_status(run, RunStatus::Running, None).await?;
        self.deliver(
            run.planner,
            MessageSource::run(run.id),
            prompts::plan_approved(run.plan.len()),
        );
        self.schedule(run).await
    }

    pub async fn reject(&self, id: RunId, reason: String) -> Result<Run, CoreError> {
        let _guard = self.lock(id).await;
        let mut run = self.store.get_run(id).await?;
        if run.status != RunStatus::AwaitingApproval {
            return Err(CoreError::Conflict(format!(
                "run is {:?}, not awaiting approval",
                run.status
            )));
        }
        run.plan.clear();
        self.set_run_status(&mut run, RunStatus::Planning, Some(reason.clone()))
            .await?;
        self.deliver(
            run.planner,
            MessageSource::run(run.id),
            prompts::plan_rejected(&reason),
        );
        Ok(run)
    }

    pub async fn cancel(&self, id: RunId) -> Result<Run, CoreError> {
        let _guard = self.lock(id).await;
        let mut run = self.store.get_run(id).await?;
        if run.status.is_terminal() {
            return Err(CoreError::Conflict(format!(
                "run is already {:?}",
                run.status
            )));
        }
        for mut task in self.store.list_tasks(id).await? {
            if task.status.is_terminal() {
                continue;
            }
            self.release_agents(&mut task).await;
            self.cleanup_worktree(&run, &task, false).await;
            task.report.failure = Some("run cancelled".into());
            self.transition(&mut task, TaskStatus::Cancelled).await?;
        }
        self.remove_integration(&run).await;
        self.set_run_status(&mut run, RunStatus::Cancelled, Some("cancelled".into()))
            .await?;
        Ok(run)
    }

    // -----------------------------------------------------------------------------------
    // Scheduling

    /// Starts every task that can start; finishes the run when nothing is left.
    async fn schedule(&self, run: &mut Run) -> Result<(), CoreError> {
        if run.status != RunStatus::Running {
            return Ok(());
        }
        let tasks = self.store.list_tasks(run.id).await?;
        for blocked in plan::blocked_tasks(&tasks) {
            let mut task = blocked.clone();
            task.report.failure = Some("a dependency failed".into());
            self.transition(&mut task, TaskStatus::Cancelled).await?;
        }
        let tasks = self.store.list_tasks(run.id).await?;
        let ready: Vec<Task> = plan::ready_tasks(&tasks, run.config.max_parallel)
            .into_iter()
            .cloned()
            .collect();
        for mut task in ready {
            if let Err(err) = self.dispatch(run, &mut task).await {
                self.fail_task(run, &mut task, format!("cannot start the task: {err}"))
                    .await?;
                // fail_task schedules again.
                return Ok(());
            }
        }
        let tasks = self.store.list_tasks(run.id).await?;
        if !tasks.is_empty() && tasks.iter().all(|t| t.status.is_terminal()) {
            self.finish_run(run, &tasks).await?;
        }
        Ok(())
    }

    async fn dispatch(&self, run: &Run, task: &mut Task) -> Result<(), CoreError> {
        if task.worktree.is_none() {
            let path = worktree_path(&self.config.worktrees, &short(run.id), &task.key);
            let branch = format!("{}-{}", run.branch, task.key);
            self.workspaces
                .create(&run.repo, &branch, &run.branch, &path)
                .await?;
            task.worktree = Some(path);
            task.branch = Some(branch);
        }
        // The teammate the planner picked, else the run's worker; escalation overrides both.
        let assigned = task.worker_template.unwrap_or(run.config.worker);
        let template = if task.escalated {
            run.config.escalate.unwrap_or(assigned)
        } else {
            assigned
        };
        let name = agent_name(if task.escalated { "e" } else { "w" }, run, task);
        let cwd = task.worktree.clone().unwrap_or_default();
        let worker = self
            .fresh_agent(template, &name, Role::Worker, &cwd)
            .await?;
        task.assignee = Some(worker.id);
        task.attempts = 1;
        task.nudges = 0;
        self.transition(task, TaskStatus::Running).await?;
        self.deliver(
            worker.id,
            MessageSource::task(task.run_id, task.id),
            prompts::worker_task(task),
        );
        Ok(())
    }

    /// Clones `template`, replacing a leftover agent of the same name.
    async fn fresh_agent(
        &self,
        template: AgentId,
        name: &str,
        role: Role,
        cwd: &Path,
    ) -> Result<Agent, CoreError> {
        if let Ok(stale) = self.store.get_agent(&AgentKey::Name(name.to_owned())).await {
            self.host.remove_agent(stale.id).await?;
        }
        self.host.clone_agent(template, name, role, cwd).await
    }

    async fn release_agents(&self, task: &mut Task) {
        for agent in [task.assignee.take(), task.reviewer.take()]
            .into_iter()
            .flatten()
        {
            if let Err(err) = self.host.remove_agent(agent).await {
                tracing::warn!(%agent, error = %err, "cannot remove agent");
            }
        }
    }

    async fn cleanup_worktree(&self, run: &Run, task: &Task, delete_branch: bool) {
        if let Some(path) = &task.worktree {
            let branch = task.branch.as_deref().filter(|_| delete_branch);
            if let Err(err) = self.workspaces.remove(&run.repo, path, branch).await {
                tracing::warn!(task = %task.key, error = %err, "cannot remove worktree");
            }
        }
    }

    async fn remove_integration(&self, run: &Run) {
        if let Err(err) = self
            .workspaces
            .remove(&run.repo, &self.integration_path(run), None)
            .await
        {
            tracing::warn!(run = %run.id, error = %err, "cannot remove integration worktree");
        }
    }

    async fn finish_run(&self, run: &mut Run, tasks: &[Task]) -> Result<(), CoreError> {
        let merged = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Merged)
            .count();
        let (status, note) = if merged == tasks.len() {
            (RunStatus::Completed, None)
        } else {
            (
                RunStatus::Failed,
                Some(format!(
                    "{} of {} tasks did not merge",
                    tasks.len() - merged,
                    tasks.len()
                )),
            )
        };
        self.remove_integration(run).await;
        self.set_run_status(run, status, note).await?;
        self.deliver(
            run.planner,
            MessageSource::run(run.id),
            prompts::run_finished(run, tasks),
        );
        Ok(())
    }

    // -----------------------------------------------------------------------------------
    // Task outcomes

    async fn fail_task(
        &self,
        run: &mut Run,
        task: &mut Task,
        reason: String,
    ) -> Result<(), CoreError> {
        self.release_agents(task).await;
        // Keep the branch so a human can inspect what the worker did.
        self.cleanup_worktree(run, task, false).await;
        task.report.failure = Some(reason);
        self.transition(task, TaskStatus::Failed).await?;
        self.deliver(
            run.planner,
            MessageSource::task(task.run_id, task.id),
            prompts::task_failed(task),
        );
        Box::pin(self.schedule(run)).await
    }

    /// Another attempt, a stronger worker, or failure.
    async fn handle_failure(
        &self,
        run: &mut Run,
        task: &mut Task,
        failure: Failure,
    ) -> Result<(), CoreError> {
        let gave_up = matches!(failure, Failure::GaveUp(_));
        if !gave_up && task.attempts < run.config.max_attempts {
            self.transition(task, TaskStatus::Retrying).await?;
            task.attempts += 1;
            task.nudges = 0;
            let message = match &failure {
                Failure::Checks(checks) => {
                    prompts::checks_failed(task, checks, run.config.max_attempts)
                }
                Failure::Rejected(notes) => format!(
                    "The reviewer rejected your change:\n{}\n\nAddress the review and call \
                     `task_complete` again.\n\ntask_id: {}\nattempt: {}",
                    notes.trim(),
                    task.id,
                    task.attempts
                ),
                Failure::Silent => prompts::nudge(task),
                Failure::GaveUp(_) => unreachable!(),
            };
            self.transition(task, TaskStatus::Running).await?;
            if let Some(worker) = task.assignee {
                self.deliver(worker, MessageSource::task(task.run_id, task.id), message);
            }
            return Ok(());
        }
        let reason = match failure {
            Failure::Checks(checks) => format!(
                "acceptance still failing after {} attempt(s): {}",
                task.attempts,
                checks
                    .iter()
                    .find(|c| !c.passed)
                    .map_or("no checks ran".to_owned(), |c| c.command.clone())
            ),
            Failure::Rejected(notes) => format!("review rejected: {notes}"),
            Failure::Silent => "the worker stopped reporting".to_owned(),
            Failure::GaveUp(reason) => format!("the worker gave up: {reason}"),
        };
        if run.config.escalate.is_some() && !task.escalated {
            self.release_agents(task).await;
            task.escalated = true;
            task.report.failure = Some(reason);
            self.transition(task, TaskStatus::Escalated).await?;
            if let Err(err) = self.dispatch(run, task).await {
                return self
                    .fail_task(run, task, format!("cannot escalate: {err}"))
                    .await;
            }
            return Ok(());
        }
        self.fail_task(run, task, reason).await
    }

    async fn merge_task(&self, run: &mut Run, task: &mut Task) -> Result<(), CoreError> {
        let branch = task.branch.clone().unwrap_or_default();
        let message = format!("crossagents: {} ({})", task.title, task.key);
        if let Err(err) = self
            .workspaces
            .merge(&self.integration_path(run), &branch, &message)
            .await
        {
            return self.fail_task(run, task, err.to_string()).await;
        }
        self.release_agents(task).await;
        self.cleanup_worktree(run, task, true).await;
        task.report.failure = None;
        self.transition(task, TaskStatus::Merged).await?;
        self.deliver(
            run.planner,
            MessageSource::task(task.run_id, task.id),
            prompts::task_merged(task),
        );
        self.schedule(run).await
    }

    /// Commits the worker's changes and runs acceptance. Runs without the run lock.
    async fn verify(self: Arc<Self>, run_id: RunId, task_id: TaskId) {
        let task = match self.store.get_task(task_id).await {
            Ok(task) => task,
            Err(err) => return tracing::error!(error = %err, "cannot load task"),
        };
        let run = match self.store.get_run(run_id).await {
            Ok(run) => run,
            Err(err) => return tracing::error!(error = %err, "cannot load run"),
        };
        let dir = task.worktree.clone().unwrap_or_default();
        let message = format!("{}: {}", task.key, task.title);
        let committed = self.workspaces.commit_all(&dir, &message).await;
        let checks = self.verifier.check(&dir, &task.spec.acceptance).await;
        let diff_stat = self.workspaces.diff_stat(&dir, &run.branch).await.ok();

        let _guard = self.lock(run_id).await;
        let result = async {
            let mut run = self.store.get_run(run_id).await?;
            let mut task = self.store.get_task(task_id).await?;
            if task.status != TaskStatus::Verifying {
                return Ok(()); // cancelled meanwhile
            }
            if let Err(err) = committed {
                return self
                    .fail_task(&mut run, &mut task, format!("cannot commit: {err}"))
                    .await;
            }
            let passed = all_passed(&checks);
            task.report.checks = checks.clone();
            task.report.diff_stat = diff_stat;
            self.publish(EventKind::TaskChecked {
                run_id,
                id: task_id,
                key: task.key.clone(),
                passed,
                checks: checks.clone(),
            })
            .await;
            if !passed {
                return self
                    .handle_failure(&mut run, &mut task, Failure::Checks(checks))
                    .await;
            }
            self.transition(&mut task, TaskStatus::Passed).await?;
            match task.reviewer_template.or(run.config.reviewer) {
                Some(reviewer) => self.start_review(&run, &mut task, reviewer).await,
                None => self.merge_task(&mut run, &mut task).await,
            }
        }
        .await;
        if let Err(err) = result {
            tracing::error!(task = %task_id, error = %err, "verification step failed");
        }
    }

    async fn start_review(
        &self,
        run: &Run,
        task: &mut Task,
        template: AgentId,
    ) -> Result<(), CoreError> {
        let cwd = task.worktree.clone().unwrap_or_default();
        let reviewer = self
            .fresh_agent(template, &agent_name("r", run, task), Role::Reviewer, &cwd)
            .await?;
        task.reviewer = Some(reviewer.id);
        self.transition(task, TaskStatus::Reviewing).await?;
        self.deliver(
            reviewer.id,
            MessageSource::task(task.run_id, task.id),
            prompts::review_request(task, &run.branch),
        );
        Ok(())
    }

    // -----------------------------------------------------------------------------------
    // Tool entry points (callers are checked by the tools layer)

    pub(crate) async fn task_of(&self, id: TaskId) -> Result<(Run, Task), CoreError> {
        let task = self.store.get_task(id).await?;
        let run = self.store.get_run(task.run_id).await?;
        Ok((run, task))
    }

    pub(crate) async fn complete(
        self: &Arc<Self>,
        caller: &Agent,
        id: TaskId,
        summary: String,
    ) -> Result<(), CoreError> {
        let (run, _) = self.task_of(id).await?;
        let _guard = self.lock(run.id).await;
        let mut task = self.store.get_task(id).await?;
        require_assignee(&task, caller)?;
        if task.status != TaskStatus::Running {
            return Err(CoreError::Conflict(format!(
                "task is {:?}; only a running task can be completed",
                task.status
            )));
        }
        task.report.summary = Some(summary);
        self.transition(&mut task, TaskStatus::Verifying).await?;
        tokio::spawn(self.clone().verify(run.id, id));
        Ok(())
    }

    pub(crate) async fn give_up(
        &self,
        caller: &Agent,
        id: TaskId,
        reason: String,
    ) -> Result<(), CoreError> {
        let (run, _) = self.task_of(id).await?;
        let _guard = self.lock(run.id).await;
        let mut run = self.store.get_run(run.id).await?;
        let mut task = self.store.get_task(id).await?;
        require_assignee(&task, caller)?;
        if task.status != TaskStatus::Running {
            return Err(CoreError::Conflict(format!("task is {:?}", task.status)));
        }
        self.handle_failure(&mut run, &mut task, Failure::GaveUp(reason))
            .await
    }

    pub(crate) async fn ask(
        &self,
        caller: &Agent,
        id: TaskId,
        question: String,
    ) -> Result<(), CoreError> {
        let (run, _) = self.task_of(id).await?;
        let _guard = self.lock(run.id).await;
        let mut task = self.store.get_task(id).await?;
        require_assignee(&task, caller)?;
        task.report.question = Some(question.clone());
        self.transition(&mut task, TaskStatus::WaitingAnswer)
            .await?;
        self.deliver(
            run.planner,
            MessageSource::task(task.run_id, task.id),
            prompts::worker_question(&task, &question),
        );
        Ok(())
    }

    pub(crate) async fn answer(
        &self,
        caller: &Agent,
        id: TaskId,
        answer: String,
    ) -> Result<(), CoreError> {
        let (run, _) = self.task_of(id).await?;
        if run.planner != caller.id {
            return Err(CoreError::Conflict(
                "only the run's planner can answer".into(),
            ));
        }
        let _guard = self.lock(run.id).await;
        let mut task = self.store.get_task(id).await?;
        if task.status != TaskStatus::WaitingAnswer {
            return Err(CoreError::Conflict("the task has no open question".into()));
        }
        task.report.question = None;
        self.transition(&mut task, TaskStatus::Running).await?;
        if let Some(worker) = task.assignee {
            self.deliver(
                worker,
                MessageSource::task(task.run_id, task.id),
                prompts::planner_answer(&task, &answer),
            );
        }
        Ok(())
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
        let mut run = self.store.get_run(run.id).await?;
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
        if approve {
            self.transition(&mut task, TaskStatus::Passed).await?;
            self.merge_task(&mut run, &mut task).await
        } else {
            // Reviewing -> Retrying is allowed; handle_failure expects to move to Retrying.
            self.handle_failure(&mut run, &mut task, Failure::Rejected(notes))
                .await
        }
    }

    // -----------------------------------------------------------------------------------
    // Reacting to agents

    /// Follows agent events to remind silent workers and reviewers.
    pub fn watch(self: &Arc<Self>) {
        let this = self.clone();
        let mut events = self.bus.subscribe();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => match event.kind {
                        EventKind::AgentTurnCompleted { id, .. } => this.on_idle(id, false).await,
                        EventKind::AgentStatusChanged {
                            id,
                            to: AgentStatus::Error,
                            ..
                        } => this.on_idle(id, true).await,
                        _ => {}
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "orchestrator fell behind on events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }

    /// The task an agent is working on or reviewing, if any.
    async fn task_for_agent(&self, agent: AgentId) -> Option<(Run, Task)> {
        let runs = self.store.list_runs().await.ok()?;
        for run in runs.into_iter().filter(|r| r.status == RunStatus::Running) {
            let tasks = self.store.list_tasks(run.id).await.ok()?;
            if let Some(task) = tasks
                .into_iter()
                .find(|t| t.assignee == Some(agent) || t.reviewer == Some(agent))
            {
                return Some((run, task));
            }
        }
        None
    }

    /// An agent's turn ended (or it crashed) while its task still needs a report.
    async fn on_idle(&self, agent: AgentId, crashed: bool) {
        let Some((run, task)) = self.task_for_agent(agent).await else {
            return;
        };
        let _guard = self.lock(run.id).await;
        let (Ok(mut run), Ok(mut task)) = (
            self.store.get_run(run.id).await,
            self.store.get_task(task.id).await,
        ) else {
            return;
        };
        let reviewing = task.reviewer == Some(agent) && task.status == TaskStatus::Reviewing;
        let working = task.assignee == Some(agent) && task.status == TaskStatus::Running;
        if !reviewing && !working {
            return;
        }
        if task.nudges < self.config.max_nudges {
            task.nudges += 1;
            task.updated_at = Utc::now();
            if let Err(err) = self.store.update_task(&task).await {
                return tracing::error!(error = %err, "cannot update task");
            }
            // A crashed agent restarts with an empty session: resend the whole brief.
            let message = match (reviewing, crashed) {
                (true, _) => prompts::review_request(&task, &run.branch),
                (false, true) => prompts::worker_task(&task),
                (false, false) => prompts::nudge(&task),
            };
            self.deliver(agent, MessageSource::task(task.run_id, task.id), message);
            return;
        }
        let result = if reviewing {
            // A reviewer that never answers does not block the merge forever.
            if let Some(reviewer) = task.reviewer.take() {
                let _ = self.host.remove_agent(reviewer).await;
            }
            task.report.review = Some("the reviewer did not answer; merged without review".into());
            match self.transition(&mut task, TaskStatus::Passed).await {
                Ok(()) => self.merge_task(&mut run, &mut task).await,
                Err(err) => Err(err),
            }
        } else {
            self.handle_failure(&mut run, &mut task, Failure::Silent)
                .await
        };
        if let Err(err) = result {
            tracing::error!(task = %task.id, error = %err, "cannot handle a silent agent");
        }
    }

    /// After a hub restart no agent session survives: re-brief active workers and redo
    /// interrupted verifications.
    pub async fn resume(self: &Arc<Self>) -> Result<(), CoreError> {
        for run in self.store.list_runs().await? {
            if run.status != RunStatus::Running {
                continue;
            }
            for task in self.store.list_tasks(run.id).await? {
                match task.status {
                    TaskStatus::Running | TaskStatus::WaitingAnswer => {
                        if let Some(worker) = task.assignee {
                            self.deliver(
                                worker,
                                MessageSource::task(task.run_id, task.id),
                                prompts::worker_task(&task),
                            );
                        }
                    }
                    TaskStatus::Verifying => {
                        tokio::spawn(self.clone().verify(run.id, task.id));
                    }
                    TaskStatus::Reviewing => {
                        if let Some(reviewer) = task.reviewer {
                            self.deliver(
                                reviewer,
                                MessageSource::task(task.run_id, task.id),
                                prompts::review_request(&task, &run.branch),
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

fn require_assignee(task: &Task, caller: &Agent) -> Result<(), CoreError> {
    if task.assignee == Some(caller.id) {
        Ok(())
    } else {
        Err(CoreError::Conflict(
            "this task is not assigned to you".into(),
        ))
    }
}

/// `w-<run>-<task key>`, within the 32-character agent name limit.
fn role_word(role: Role) -> &'static str {
    match role {
        Role::Planner => "planner",
        Role::Worker => "worker",
        Role::Reviewer => "reviewer",
    }
}

fn agent_name(prefix: &str, run: &Run, task: &Task) -> String {
    let mut name = format!("{prefix}-{}-{}", &short(run.id)[..6], task.key);
    name.truncate(32);
    name.trim_end_matches('-').to_owned()
}

fn first_line(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() > max {
        format!("{}…", line.chars().take(max).collect::<String>())
    } else {
        line.to_owned()
    }
}
