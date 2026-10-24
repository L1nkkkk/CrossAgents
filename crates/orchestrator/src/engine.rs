//! Runs and their tasks (docs/tasks/H1.md): the hub keeps the mechanics — plans, worktrees,
//! dispatch once dependencies are merged, state, records — and the agents decide. A task's
//! issuer checks, accepts, reworks or drops it with the task tools (`tasks.rs`).
//!
//! Every state change of a run happens under that run's lock. Long work (agent turns,
//! acceptance commands) happens outside it; messages to agents are delivered in the
//! background so a slow agent start never blocks the run.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AgentKey, AgentStatus, CoreError, Envelope, EventBus, EventKind, MessageSource,
    PlannedTask, Role, Run, RunConfig, RunId, RunStatus, Store, Task, TaskId, TaskReport,
    TaskStatus,
};
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::brief;
use crate::host::AgentHost;
use crate::plan;
use crate::verify::Verifier;
use crate::workspace::{InPlaceWorkspaces, WorkspaceProvider, worktree_path};

pub struct OrchestratorConfig {
    /// Root for run and task worktrees.
    pub worktrees: PathBuf,
    /// No longer used: the hub does not nudge (H1). Kept so configurations still load.
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
    pub(crate) store: Arc<dyn Store>,
    pub(crate) bus: Arc<EventBus>,
    pub(crate) host: Arc<dyn AgentHost>,
    pub(crate) workspaces: Arc<dyn WorkspaceProvider>,
    pub(crate) verifier: Arc<dyn Verifier>,
    pub(crate) config: OrchestratorConfig,
    locks: Mutex<HashMap<RunId, Arc<Mutex<()>>>>,
    pub(crate) messaging: Mutex<crate::messaging::MessagingState>,
}

pub(crate) fn short(id: impl ToString) -> String {
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
            messaging: Mutex::default(),
        })
    }

    pub fn store(&self) -> &Arc<dyn Store> {
        &self.store
    }

    pub(crate) async fn lock(&self, run: RunId) -> OwnedMutexGuard<()> {
        let lock = self.locks.lock().await.entry(run).or_default().clone();
        lock.lock_owned().await
    }

    /// Where a task's work goes: the branch it starts from and is compared with, and the
    /// working copy it merges into — its parent task's, else the run's.
    pub(crate) async fn base_of(
        &self,
        run: &Run,
        task: &Task,
    ) -> Result<(String, PathBuf), CoreError> {
        if run.in_place {
            return Ok((String::new(), run.repo.clone()));
        }
        if let Some(parent) = task.parent {
            let parent = self.store.get_task(parent).await?;
            if let (Some(branch), Some(dir)) = (parent.branch, parent.worktree) {
                return Ok((branch, dir));
            }
            return Err(CoreError::Conflict(format!(
                "the parent task #{} has no worktree any more",
                parent.key
            )));
        }
        Ok((run.branch.clone(), self.integration_path(run)))
    }

    pub(crate) fn integration_path(&self, run: &Run) -> PathBuf {
        if run.in_place {
            return run.repo.clone();
        }
        worktree_path(&self.config.worktrees, &short(run.id), "integration")
    }

    /// Where `run`'s tasks work: git worktrees, or the directory itself.
    pub(crate) fn ws_for(&self, run: &Run) -> &dyn WorkspaceProvider {
        if run.in_place {
            &InPlaceWorkspaces
        } else {
            self.workspaces.as_ref()
        }
    }

    pub(crate) async fn publish(&self, kind: EventKind) {
        if let Err(err) = self.bus.publish(kind).await {
            tracing::error!(error = %err, "cannot publish event");
        }
    }

    /// Relays what `from` wrote to `to`, in the background; returns the message id.
    pub(crate) fn relay(
        &self,
        to: AgentId,
        from: AgentId,
        text: String,
        reply_to: Option<crossagents_core::MessageId>,
    ) -> crossagents_core::MessageId {
        let envelope = Envelope {
            reply_to,
            ..Envelope::new(MessageSource::Agent { agent_id: from }, text)
        };
        let id = envelope.id;
        let host = self.host.clone();
        tokio::spawn(async move {
            if let Err(err) = host.send_envelope(to, envelope, false).await {
                tracing::warn!(agent = %to, error = %err, "cannot deliver message");
            }
        });
        id
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

    pub(crate) async fn set_run_status(
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

    pub(crate) async fn transition(
        &self,
        task: &mut Task,
        to: TaskStatus,
    ) -> Result<(), CoreError> {
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
        let in_place = !self.workspaces.versioned(&repo).await;
        let base_branch = if in_place {
            String::new()
        } else {
            self.workspaces.current_branch(&repo).await?
        };
        let id = RunId::new();
        let now = Utc::now();
        let mut run = Run {
            landed: None,
            summary: None,
            id,
            title: new.title.unwrap_or_else(|| first_line(&new.request, 60)),
            request: new.request,
            planner: planner.id,
            status: RunStatus::Planning,
            config: new.config,
            repo: repo.clone(),
            base_branch: base_branch.clone(),
            branch: if in_place {
                String::new()
            } else {
                format!("crossagents/{}", short(id))
            },
            in_place,
            note: None,
            plan: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        // One directory, so one task at a time.
        run.config.max_parallel = if in_place {
            1
        } else {
            run.config.max_parallel.max(1)
        };
        run.config.max_attempts = run.config.max_attempts.max(1);
        self.ws_for(&run)
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
        // The request is the person's; the roster and ids are facts the lead needs.
        self.deliver(
            planner.id,
            MessageSource::Human,
            brief::kickoff(&run, &team, run.config.worker),
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
    pub(crate) async fn team_of(&self, run: &Run) -> Result<Vec<Agent>, CoreError> {
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
    pub(crate) async fn resolve_assignments(
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
        if run.config.auto_approves() {
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
                issuer: Some(run.planner),
                progress: None,
                parent: None,
                created_at: now,
                updated_at: now,
            };
            self.store.insert_task(&task).await?;
        }
        self.set_run_status(run, RunStatus::Running, None).await?;
        self.notice(
            run.planner,
            &format!("run:{}", run.id),
            &format!(
                "The plan of run {} was approved: {} task(s) start as their dependencies allow.",
                run.id,
                run.plan.len()
            ),
        )
        .await;
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
        // The person's words, as they wrote them.
        self.deliver(
            run.planner,
            MessageSource::Human,
            format!(
                "The plan of run {} was not approved:
{}",
                run.id,
                reason.trim()
            ),
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
    pub(crate) async fn schedule(&self, run: &mut Run) -> Result<(), CoreError> {
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
        // Every task ended: the lead lands and closes the run (run_land, run_finish).
        let tasks = self.store.list_tasks(run.id).await?;
        if !tasks.is_empty() && tasks.iter().all(|t| t.status.is_terminal()) {
            let merged = tasks
                .iter()
                .filter(|t| t.status == TaskStatus::Merged)
                .count();
            self.notice(
                run.planner,
                &format!("run:{}:ended", run.id),
                &format!(
                    "All {} task(s) of run {} have ended: {merged} merged into {}.",
                    tasks.len(),
                    run.id,
                    run.branch
                ),
            )
            .await;
        }
        Ok(())
    }

    pub(crate) async fn dispatch(&self, run: &Run, task: &mut Task) -> Result<(), CoreError> {
        if task.worktree.is_none() && run.in_place {
            task.worktree = Some(run.repo.clone());
        } else if task.worktree.is_none() {
            let path = worktree_path(&self.config.worktrees, &short(run.id), &task.key);
            let branch = format!("{}-{}", run.branch, task.key);
            let (base, _) = self.base_of(run, task).await?;
            self.ws_for(&run)
                .create(&run.repo, &branch, &base, &path)
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
        // The task as its issuer wrote it, from the issuer.
        let issuer = task.issuer_or(run.planner);
        self.relay(worker.id, issuer, brief::task(task), None);
        Ok(())
    }

    /// Clones `template`, replacing a leftover agent of the same name.
    pub(crate) async fn fresh_agent(
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

    pub(crate) async fn release_agents(&self, task: &mut Task) {
        for agent in [task.assignee.take(), task.reviewer.take()]
            .into_iter()
            .flatten()
        {
            if let Err(err) = self.host.remove_agent(agent).await {
                tracing::warn!(%agent, error = %err, "cannot remove agent");
            }
        }
    }

    pub(crate) async fn cleanup_worktree(&self, run: &Run, task: &Task, delete_branch: bool) {
        if let Some(path) = &task.worktree {
            let branch = task.branch.as_deref().filter(|_| delete_branch);
            if let Err(err) = self.ws_for(&run).remove(&run.repo, path, branch).await {
                tracing::warn!(task = %task.key, error = %err, "cannot remove worktree");
            }
        }
    }

    async fn remove_integration(&self, run: &Run) {
        if let Err(err) = self
            .ws_for(&run)
            .remove(&run.repo, &self.integration_path(run), None)
            .await
        {
            tracing::warn!(run = %run.id, error = %err, "cannot remove integration worktree");
        }
    }

    pub(crate) async fn finish_run(&self, run: &mut Run, tasks: &[Task]) -> Result<(), CoreError> {
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
        Ok(())
    }

    // -----------------------------------------------------------------------------------
    // Task outcomes

    /// The task cannot go on (e.g. its worker could not start). Its issuer hears about it.
    pub(crate) async fn fail_task(
        &self,
        run: &mut Run,
        task: &mut Task,
        reason: String,
    ) -> Result<(), CoreError> {
        self.release_agents(task).await;
        // Keep the branch so the issuer or a person can inspect what was done.
        self.cleanup_worktree(run, task, false).await;
        task.report.failure = Some(reason.clone());
        self.transition(task, TaskStatus::Failed).await?;
        self.notice(
            task.issuer_or(run.planner),
            &format!("task:{}", task.id),
            &format!("Task {} failed: {reason}", brief::task_ref(task)),
        )
        .await;
        Box::pin(self.schedule(run)).await
    }

    /// A line in an agent's notification digest.
    pub(crate) async fn notice(&self, agent: AgentId, topic: &str, line: &str) {
        if let Err(err) = self.host.notify(agent, topic, line).await {
            tracing::warn!(%agent, error = %err, "cannot deliver a notice");
        }
    }

    // -----------------------------------------------------------------------------------
    // Reacting to agents

    /// Follows agent events to notice assignees and reviewers that stop without reporting.
    pub fn watch(self: &Arc<Self>) {
        let this = self.clone();
        let mut events = self.bus.subscribe();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => match event.kind {
                        EventKind::AgentTurnCompleted { id, .. } => this.on_idle(id).await,
                        EventKind::AgentStatusChanged {
                            id,
                            to: AgentStatus::Error,
                            ..
                        } => this.on_idle(id).await,
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
    pub(crate) async fn task_for_agent(&self, agent: AgentId) -> Option<(Run, Task)> {
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

    /// An agent's turn ended (or it crashed) while its task still waits for its report. The
    /// hub does not nudge: the task shows as stalled and its issuer decides.
    async fn on_idle(&self, agent: AgentId) {
        let Some((run, task)) = self.task_for_agent(agent).await else {
            return;
        };
        let _guard = self.lock(run.id).await;
        let Ok(mut task) = self.store.get_task(task.id).await else {
            return;
        };
        let issuer = task.issuer_or(run.planner);
        let name = self
            .store
            .get_agent(&AgentKey::Id(agent))
            .await
            .map(|a| a.name)
            .unwrap_or_else(|_| agent.to_string());
        if task.reviewer == Some(agent) && task.status == TaskStatus::Reviewing {
            self.notice(
                issuer,
                &format!("task:{}", task.id),
                &format!(
                    "{name} ended its turn without a verdict on {} (still reviewing).",
                    brief::task_ref(&task)
                ),
            )
            .await;
            return;
        }
        if task.assignee != Some(agent) || task.status != TaskStatus::Running {
            return;
        }
        if let Err(err) = self.transition(&mut task, TaskStatus::Stalled).await {
            return tracing::error!(error = %err, "cannot mark a task stalled");
        }
        self.notice(
            issuer,
            &format!("task:{}", task.id),
            &format!(
                "Task {} is stalled: {name} ended a turn without reporting.",
                brief::task_ref(&task)
            ),
        )
        .await;
    }

    /// After a hub restart sessions resume where the driver can, but nothing is running:
    /// working tasks count as stalled until their issuer looks at them.
    pub async fn resume(self: &Arc<Self>) -> Result<(), CoreError> {
        for run in self.store.list_runs().await? {
            if run.status != RunStatus::Running {
                continue;
            }
            for mut task in self.store.list_tasks(run.id).await? {
                if !matches!(
                    task.status,
                    TaskStatus::Running
                        | TaskStatus::Verifying
                        | TaskStatus::Retrying
                        | TaskStatus::Escalated
                ) {
                    continue;
                }
                if task.status == TaskStatus::Running {
                    self.transition(&mut task, TaskStatus::Stalled).await?;
                } else {
                    // Left over from the fixed pipeline: hand it to the issuer as done.
                    task.status = TaskStatus::Stalled;
                    task.updated_at = Utc::now();
                    self.store.update_task(&task).await?;
                }
                self.notice(
                    task.issuer_or(run.planner),
                    &format!("task:{}", task.id),
                    &format!(
                        "The hub restarted; task {} is stalled until you decide.",
                        brief::task_ref(&task)
                    ),
                )
                .await;
            }
        }
        Ok(())
    }
}

/// `w-<run>-<task key>`, within the 32-character agent name limit.
pub(crate) fn agent_name(prefix: &str, run: &Run, task: &Task) -> String {
    let mut name = format!("{prefix}-{}-{}", &short(run.id)[..6], task.key);
    name.truncate(32);
    name.trim_end_matches('-').to_owned()
}

fn role_word(role: Role) -> &'static str {
    match role {
        Role::Planner => "planner",
        Role::Worker => "worker",
        Role::Reviewer => "reviewer",
    }
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
