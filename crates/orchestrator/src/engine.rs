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
    Agent, AgentId, AgentKey, AgentStatus, ClaimRole, CoreError, Envelope, EventBus, EventKind,
    HandoffKind, MessageSource, PlannedTask, Role, Run, RunConfig, RunId, RunPolicy, RunStatus,
    Store, Task, TaskId, TaskReport, TaskStatus,
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
    /// Bytes a repository's shared build directory may grow to before it is removed once no
    /// run uses it (`workspace::prune_shared_targets`).
    pub build_cache_limit: u64,
}

/// `build_cache_limit` unless the hub's configuration says otherwise.
pub const DEFAULT_BUILD_CACHE_LIMIT: u64 = 30 << 30;

/// Everything needed to start a run.
#[derive(Debug, Clone)]
pub struct NewRun {
    pub request: String,
    pub title: Option<String>,
    pub planner: AgentId,
    pub config: RunConfig,
    /// The repository the run works in; `None` is the planner's own directory.
    pub repo: Option<PathBuf>,
    /// The branch the run's branch starts from and lands on; `None` is the repository's
    /// current branch.
    pub base_branch: Option<String>,
}

/// What the person settles when approving a plan (docs/tasks/W1.md §5.2); absent fields
/// keep what the run started with.
#[derive(Debug, Clone, Default)]
pub struct Approval {
    pub policy: Option<RunPolicy>,
    pub max_parallel: Option<u32>,
    /// Who works on each task, by task key: a worker, or `None` for the run's default.
    pub assignees: Vec<(String, Option<AgentId>)>,
}

/// Locks by human card id. A plain mutex around the table, so a guard can clean up on drop.
#[derive(Default)]
pub(crate) struct CardLocks(std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>);

/// Holds a card's lock ([`Orchestrator::lock_card`]).
pub(crate) struct CardGuard<'a> {
    locks: &'a CardLocks,
    id: String,
    guard: Option<OwnedMutexGuard<()>>,
}

impl Drop for CardGuard<'_> {
    fn drop(&mut self) {
        drop(self.guard.take());
        let mut locks = self.locks.0.lock().unwrap_or_else(|e| e.into_inner());
        // Only the table holds it: nobody waits for this card. Waiters clone it under this
        // same table lock, so none can appear meanwhile.
        if locks
            .get(&self.id)
            .is_some_and(|l| Arc::strong_count(l) == 1)
        {
            locks.remove(&self.id);
        }
    }
}

pub struct Orchestrator {
    pub(crate) store: Arc<dyn Store>,
    pub(crate) bus: Arc<EventBus>,
    pub(crate) host: Arc<dyn AgentHost>,
    pub(crate) workspaces: Arc<dyn WorkspaceProvider>,
    pub(crate) verifier: Arc<dyn Verifier>,
    pub(crate) config: OrchestratorConfig,
    locks: Mutex<HashMap<RunId, Arc<Mutex<()>>>>,
    /// Cards being answered or dismissed, by id ([`Self::lock_card`]).
    cards: CardLocks,
    pub(crate) messaging: Mutex<crate::messaging::MessagingState>,
    /// Approvals waiting for the person, by question id (docs/tasks/DOGFOOD.md §7).
    pub(crate) approvals: std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>,
    /// Context tokens each agent last reported in use, for handoff cards.
    pub(crate) contexts: std::sync::Mutex<HashMap<AgentId, u64>>,
}

/// What a run says about a lead copied for it (docs/tasks/S1.md §3.2).
pub const COPIED_LEAD_NOTE: &str = "主导是复制出来的，没带上原会话的上下文";

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
            cards: CardLocks::default(),
            messaging: Mutex::default(),
            approvals: std::sync::Mutex::default(),
            contexts: std::sync::Mutex::default(),
        })
    }

    pub fn store(&self) -> &Arc<dyn Store> {
        &self.store
    }

    pub(crate) async fn lock(&self, run: RunId) -> OwnedMutexGuard<()> {
        let lock = self.locks.lock().await.entry(run).or_default().clone();
        lock.lock_owned().await
    }

    /// One answer or dismissal of card `id` at a time: deciding a Work card starts a run,
    /// which takes seconds, and two decisions must not both act. The entry goes when the
    /// last holder or waiter lets go.
    pub(crate) async fn lock_card(&self, id: &str) -> CardGuard<'_> {
        let lock = self
            .cards
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(id.to_owned())
            .or_default()
            .clone();
        CardGuard {
            locks: &self.cards,
            id: id.to_owned(),
            guard: Some(lock.lock_owned().await),
        }
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
        // The planner's workspace, not where it works now (a run's checkout, say).
        let repo = new
            .repo
            .clone()
            .unwrap_or_else(|| planner.home().to_path_buf());
        let mut new = new;
        let team = self.gather_team(&new.config, &repo).await?;
        new.config.team = team.iter().map(|a| a.id).collect();
        let in_place = !self.workspaces.versioned(&repo).await;
        let base_branch = if in_place {
            String::new()
        } else if let Some(base) = new.base_branch.clone() {
            base
        } else {
            self.workspaces.current_branch(&repo).await?
        };
        let id = RunId::new();
        let now = Utc::now();
        let mut run = Run {
            continuation: None,
            landed: None,
            summary: None,
            checks: Vec::new(),
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
            lead_note: None,
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
        // The lead works in the run's own checkout: what it changes stays on the run branch
        // until it is landed (docs/tasks/W1.md §4.1). In place, it works where it is.
        let lead = if in_place {
            self.claim(&planner, run.id, None, ClaimRole::Lead, false)
                .await
                .map(|()| (planner.clone(), false))
        } else {
            self.lead_in(&planner, run.id, &self.integration_path(&run))
                .await
        };
        let (lead, copied) = match lead {
            Ok(lead) => lead,
            Err(err) => {
                if !in_place {
                    self.remove_integration(&run).await;
                }
                return Err(err);
            }
        };
        run.planner = lead.id;
        if copied {
            run.lead_note = Some(COPIED_LEAD_NOTE.into());
        }
        if let Err(err) = self.store.insert_run(&run).await {
            self.let_go_run(run.id).await;
            return Err(err);
        }
        self.publish(EventKind::RunCreated { run: run.clone() })
            .await;
        // The request is the person's; the roster and ids are facts the lead needs.
        let sessions = self.sessions_section(&run.repo, run.planner).await;
        self.deliver(
            run.planner,
            MessageSource::Human,
            brief::kickoff(
                &run,
                &team,
                run.config.worker,
                &crate::project::run_checks(&self.integration_path(&run)),
                &sessions,
            ),
        );
        Ok(run)
    }

    /// The run's lead, working in `dir` (docs/tasks/S1.md §3.2): `planner` itself, held for
    /// the run and moved in with its conversation, when its driver resumes a session in
    /// another directory (or it never started one). Else a copy, which starts without that
    /// conversation: `true` then. Busy when `planner` works for another run or is answering
    /// a person.
    async fn lead_in(
        &self,
        planner: &Agent,
        run: RunId,
        dir: &Path,
    ) -> Result<(Agent, bool), CoreError> {
        let never_started = planner.status == AgentStatus::Stopped
            && self.store.last_session_id(planner.id).await?.is_none();
        if never_started || self.host.can_move(planner).await {
            self.claim(planner, run, None, ClaimRole::Lead, false)
                .await?;
            if let Err(err) = self.host.place(planner.id, dir, true).await {
                let _ = self.store.release_agent(planner.id, run).await;
                return Err(err);
            }
            let lead = self.store.get_agent(&AgentKey::Id(planner.id)).await?;
            return Ok((lead, false));
        }
        let mut config = planner.config.clone();
        // The copy leads the run; it is nobody's recruit.
        if let Some(map) = config.as_object_mut() {
            map.remove("spawned_by_agent");
            map.remove("spawned_by_run");
        }
        let copy = self
            .host
            .spawn_agent(Agent {
                id: AgentId::new(),
                // A made-up person's name.
                name: String::new(),
                name_source: crossagents_core::NameSource::Generated,
                role: Role::Planner,
                cwd: dir.to_path_buf(),
                config,
                status: AgentStatus::Stopped,
                created_at: Utc::now(),
                topic: None,
                archived_at: None,
                home: planner.home().to_path_buf(),
                ..planner.clone()
            })
            .await?;
        // Made for this run: an execution session of it, archived when the run lets go.
        self.claim(&copy, run, None, ClaimRole::Lead, true).await?;
        Ok((copy, true))
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
            let agent = self.store.get_agent(&AgentKey::Id(id)).await?;
            // No worker was given, so tasks default to copies of the lead: it is not a
            // teammate the lead assigns to.
            if agent.role != Role::Planner {
                add(agent);
            }
        }
        if config.team.is_empty() {
            for agent in self.store.list_agents().await? {
                // The workspace's own sessions, not the executions runs started in it.
                let execution = matches!(
                    self.store.claim_of(agent.id).await?,
                    Some(claim) if claim.fresh
                );
                if agent.role != Role::Planner
                    && agent.home() == repo
                    && agent.archived_at.is_none()
                    && !execution
                {
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

    /// Resolves each planned task's `assignee` and `reviewer` to agents of the right role:
    /// teammates for a new execution session (`fresh`, the default), or, with `fresh: false`,
    /// any session of the run's workspace that may work on it itself (docs/tasks/S1.md §3.3).
    pub(crate) async fn resolve_assignments(
        &self,
        run: &Run,
        tasks: &[PlannedTask],
    ) -> Result<Vec<(Option<AgentId>, Option<AgentId>)>, CoreError> {
        let team = self.team_of(run).await?;
        let mut problems = Vec::new();
        let mut resolved = Vec::new();
        for t in tasks {
            let worker = self
                .pick(
                    run,
                    &team,
                    &t.key,
                    t.assignee.as_deref(),
                    Role::Worker,
                    t.fresh,
                    t.restore,
                    &mut problems,
                )
                .await;
            let reviewer = self
                .pick(
                    run,
                    &team,
                    &t.key,
                    t.reviewer.as_deref(),
                    Role::Reviewer,
                    t.reviewer_fresh,
                    false,
                    &mut problems,
                )
                .await;
            resolved.push((worker, reviewer));
        }
        if problems.is_empty() {
            Ok(resolved)
        } else {
            Err(CoreError::Conflict(format!(
                "the plan is invalid:\n{}",
                problems.join("\n")
            )))
        }
    }

    /// One assignment of [`Self::resolve_assignments`]; what is wrong goes in `problems`.
    #[allow(clippy::too_many_arguments)]
    async fn pick(
        &self,
        run: &Run,
        team: &[Agent],
        task: &str,
        name: Option<&str>,
        role: Role,
        fresh: bool,
        restore: bool,
        problems: &mut Vec<String>,
    ) -> Option<AgentId> {
        let field = if role == Role::Worker {
            "assignee"
        } else {
            "reviewer"
        };
        let Some(name) = name else {
            if !fresh {
                problems.push(format!(
                    "task {task:?}: fresh: false needs {field}: the session that works on it itself"
                ));
            }
            return None;
        };
        if fresh {
            if let Some(agent) = team.iter().find(|a| a.name == name && a.role == role) {
                return Some(agent.id);
            }
            let known: Vec<&str> = team
                .iter()
                .filter(|a| a.role == role)
                .map(|a| a.name.as_str())
                .collect();
            problems.push(format!(
                "task {task:?}: {field} {name:?} is not a {} on the team (available: {})",
                role_word(role),
                if known.is_empty() {
                    "none".to_owned()
                } else {
                    known.join(", ")
                }
            ));
            return None;
        }
        // The session itself: any of the workspace's, archived ones too.
        match self.store.get_agent(&AgentKey::Name(name.to_owned())).await {
            Ok(agent) if agent.role != role => {
                problems.push(format!(
                    "task {task:?}: {field} {name:?} is not a {}",
                    role_word(role)
                ));
                None
            }
            Ok(agent) => match self.may_take_self(run, &agent, restore).await {
                Ok(()) => Some(agent.id),
                Err(CoreError::Conflict(why)) => {
                    problems.push(format!("task {task:?}: {why}"));
                    None
                }
                Err(err) => {
                    problems.push(format!("task {task:?}: {err}"));
                    None
                }
            },
            Err(_) => {
                problems.push(format!(
                    "task {task:?}: no session named {name:?} (agent_list lists them)"
                ));
                None
            }
        }
    }

    /// The newest run in `Planning` led by `planner`.
    async fn planning_run_of(&self, planner: AgentId) -> Result<Run, CoreError> {
        let led: Vec<Run> = self
            .store
            .list_runs()
            .await?
            .into_iter()
            .filter(|r| r.planner == planner && !r.status.is_terminal())
            .collect();
        if let Some(run) = led.iter().find(|r| r.status == RunStatus::Planning) {
            return Ok(run.clone());
        }
        if led.is_empty() {
            // A planner the person talks to in a chat.
            return Err(CoreError::Conflict(
                "you lead no run: this is a conversation with the person. Do the work here yourself or bring in teammates (agent_spawn, agent_send), or propose a Work on a branch of its own with work_start"
                    .into(),
            ));
        }
        Err(CoreError::Conflict(
            "you have no run waiting for a plan".into(),
        ))
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
            self.approve_locked(&mut run, &[]).await?;
        }
        Ok(run)
    }

    pub async fn approve(&self, id: RunId, approval: Approval) -> Result<Run, CoreError> {
        let _guard = self.lock(id).await;
        let mut run = self.store.get_run(id).await?;
        if run.status != RunStatus::AwaitingApproval {
            return Err(CoreError::Conflict(format!(
                "run is {:?}, not awaiting approval",
                run.status
            )));
        }
        let changes = self.settle(&mut run, approval).await?;
        self.approve_locked(&mut run, &changes).await?;
        Ok(run)
    }

    /// Applies the person's choices to the run before its tasks are created; returns what
    /// differs from the plan and the run's settings, for the lead. A worker picked for a task
    /// joins the team, so it is assignable like the plan's own picks.
    async fn settle(&self, run: &mut Run, approval: Approval) -> Result<Vec<String>, CoreError> {
        let mut changes = Vec::new();
        if let Some(policy) = approval.policy {
            changes.extend(policy_changes(&run.config.policy, &policy));
            run.config.policy = policy;
        }
        if let Some(n) = approval.max_parallel {
            let n = if run.in_place { 1 } else { n.max(1) };
            if n != run.config.max_parallel {
                changes.push(format!("at most {n} task(s) run at a time"));
            }
            run.config.max_parallel = n;
        }
        for (key, worker) in approval.assignees {
            let Some(index) = run.plan.iter().position(|t| t.key == key) else {
                return Err(CoreError::Conflict(format!("the plan has no task {key:?}")));
            };
            let name = match worker {
                None => None,
                Some(id) => {
                    let agent = self.store.get_agent(&AgentKey::Id(id)).await?;
                    if agent.role != Role::Worker {
                        return Err(CoreError::Conflict(format!(
                            "{} is not a worker and cannot take task {key:?}",
                            agent.name
                        )));
                    }
                    if run.config.team.is_empty() {
                        // Runs created before teams existed: make the implicit team explicit
                        // (a lead standing in as the default worker is no teammate).
                        for id in [Some(run.config.worker), run.config.reviewer]
                            .into_iter()
                            .flatten()
                        {
                            let mate = self.store.get_agent(&AgentKey::Id(id)).await;
                            if mate.is_ok_and(|m| m.role != Role::Planner) {
                                run.config.team.push(id);
                            }
                        }
                    }
                    if !run.config.team.contains(&agent.id) {
                        run.config.team.push(agent.id);
                    }
                    Some(agent.name)
                }
            };
            if run.plan[index].assignee != name {
                let who =
                    |n: &Option<String>| n.clone().unwrap_or_else(|| "the default worker".into());
                changes.push(format!(
                    "#{key} goes to {} (your plan said {})",
                    who(&name),
                    who(&run.plan[index].assignee)
                ));
            }
            run.plan[index].assignee = name;
        }
        Ok(changes)
    }

    /// Creates the plan's tasks and starts them. The lead hears who does each task, and
    /// `changes`: what the person changed when approving.
    async fn approve_locked(&self, run: &mut Run, changes: &[String]) -> Result<(), CoreError> {
        if run.status != RunStatus::AwaitingApproval {
            return Err(CoreError::Conflict(format!(
                "run is {:?}, not awaiting approval",
                run.status
            )));
        }
        let now = Utc::now();
        let assignments = self.resolve_assignments(run, &run.plan).await?;
        let mut who = Vec::new();
        for (planned, (worker_template, _)) in run.plan.iter().zip(&assignments) {
            let template = worker_template.unwrap_or(run.config.worker);
            let name = match self.store.get_agent(&AgentKey::Id(template)).await {
                Ok(agent) => format!(
                    "{} ({})",
                    agent.name,
                    agent.model.as_deref().unwrap_or(&agent.plugin)
                ),
                Err(_) => "the default worker".into(),
            };
            who.push(format!("#{} → {name}", planned.key));
        }
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
                worker_fresh: planned.fresh,
                worker_restore: planned.restore,
                reviewer_fresh: planned.reviewer_fresh,
                status: TaskStatus::Pending,
                attempts: 0,
                escalated: false,
                nudges: 0,
                branch: None,
                worktree: None,
                report: TaskReport::default(),
                issuer: Some(run.planner),
                progress: None,
                handoffs: Vec::new(),
                parent: None,
                created_at: now,
                updated_at: now,
            };
            self.store.insert_task(&task).await?;
        }
        self.set_run_status(run, RunStatus::Running, None).await?;
        let mut text = format!(
            "The plan of run {} was approved: {} task(s) start as their dependencies allow.
Who does each task: {}",
            run.id,
            run.plan.len(),
            who.join(", ")
        );
        if !changes.is_empty() {
            text.push_str(&format!(
                "
The person changed: {}",
                changes.join("; ")
            ));
        }
        self.notice(run.planner, &format!("run:{}", run.id), &text)
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
            crossagents_core::Handoff::end(
                &mut task.handoffs,
                task.assignee,
                crossagents_core::HandoffOutcome::Cancelled,
            );
            self.release_agents(&mut task).await;
            self.cleanup_worktree(&run, &task, false).await;
            task.report.failure = Some("run cancelled".into());
            self.transition(&mut task, TaskStatus::Cancelled).await?;
        }
        // Its lead goes home (or is archived, a copy) before its checkout goes.
        self.let_go_run(id).await;
        self.remove_integration(&run).await;
        self.set_run_status(&mut run, RunStatus::Cancelled, Some("cancelled".into()))
            .await?;
        Ok(run)
    }

    // -----------------------------------------------------------------------------------
    // Scheduling

    /// Starts every task that can start; finishes the run when nothing is left.
    pub(crate) async fn schedule(&self, run: &mut Run) -> Result<(), CoreError> {
        self.schedule_reporting(run).await.map(|_| ())
    }

    /// [`Self::schedule`], returning the tasks that could not start because the session
    /// they name to work on them itself would not take them now (`#key: why`). Those stay
    /// pending, their issuer is told, and the next schedule tries again.
    pub(crate) async fn schedule_reporting(&self, run: &mut Run) -> Result<Vec<String>, CoreError> {
        let mut refused = Vec::new();
        if run.status != RunStatus::Running {
            return Ok(refused);
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
            let itself = !task.worker_fresh && !task.escalated;
            if let Err(err) = self.dispatch(run, &mut task).await {
                if itself && task.status == TaskStatus::Pending {
                    refused.push(self.not_started(run, &mut task, err).await?);
                    continue;
                }
                self.fail_task(run, &mut task, format!("cannot start the task: {err}"))
                    .await?;
                // fail_task schedules again.
                return Ok(refused);
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
        Ok(refused)
    }

    /// `task` names a session to work on it itself and that session would not take it now
    /// (docs/tasks/S1.md §3.3): the task stays pending, says why, and its issuer hears it.
    async fn not_started(
        &self,
        run: &Run,
        task: &mut Task,
        err: CoreError,
    ) -> Result<String, CoreError> {
        let why = match err {
            CoreError::Conflict(why) | CoreError::Busy(why) | CoreError::Agent(why) => why,
            other => other.to_string(),
        };
        task.progress = Some(format!("not started: {why}"));
        task.updated_at = Utc::now();
        // Keeps the worktree made for it, for the next try.
        self.store.update_task(task).await?;
        self.notice(
            task.issuer_or(run.planner),
            &format!("task:{}", task.id),
            &format!(
                "Task {} did not start: {why}. It stays pending and is tried again when the run next moves on; or task_assign it (a new execution session by default, or someone else).",
                brief::task_ref(task)
            ),
        )
        .await;
        Ok(format!("#{}: {why}", task.key))
    }

    pub(crate) async fn dispatch(&self, run: &Run, task: &mut Task) -> Result<(), CoreError> {
        if task.worktree.is_none() && run.in_place {
            task.worktree = Some(run.repo.clone());
        } else if task.worktree.is_none() {
            let path = worktree_path(&self.config.worktrees, &short(run.id), &task.key);
            let branch = format!("{}-{}", run.branch, task.key);
            let (base, _) = self.base_of(run, task).await?;
            if base == run.branch {
                // The task starts from what the lead has done so far.
                self.commit_lead(run, &format!("crossagents: {}", run.title))
                    .await?;
            }
            self.ws_for(run)
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
        let left = self.left_by_issuer(run, task).await;
        let (worker, cloned, rerouted) = if !task.worker_fresh && !task.escalated {
            // The session itself, with its conversation (docs/tasks/S1.md §3.3).
            let me = self.store.get_agent(&AgentKey::Id(template)).await?;
            if let Err(err) = self
                .take_self(run, task, &me, ClaimRole::Worker, task.worker_restore)
                .await
            {
                // It could not take its conversation there: that execution failed, and the
                // card says why (docs/tasks/S1.md §3.1).
                if let CoreError::Agent(why) = &err {
                    self.hand_off(
                        run,
                        task,
                        HandoffKind::Assign,
                        left,
                        &me,
                        None,
                        Some(why.clone()),
                    )
                    .await?;
                    if let Some(card) = task.handoffs.last_mut() {
                        card.fresh = false;
                        card.outcome = Some(crossagents_core::HandoffOutcome::Failed);
                    }
                    self.store.update_task(task).await?;
                }
                return Err(err);
            }
            task.progress = None;
            let me = self.store.get_agent(&AgentKey::Id(template)).await?;
            (me, template, None)
        } else {
            // On an account with quota left (docs/tasks/DOGFOOD.md §5).
            self.routed_worker(run, task, template, &name, None).await?
        };
        task.assignee = Some(worker.id);
        task.attempts = 1;
        task.nudges = 0;
        self.transition(task, TaskStatus::Running).await?;
        let (kind, reason) = if task.escalated {
            let why = rerouted.unwrap_or_else(|| "moved to the run's escalation teammate".into());
            (HandoffKind::Escalate, Some(why))
        } else {
            (HandoffKind::Assign, rerouted)
        };
        let to_name = self.name_of(cloned).await;
        let card = self
            .hand_off(run, task, kind, left, &worker, to_name, reason)
            .await?;
        // The task as its issuer wrote it, from the issuer.
        let issuer = task.issuer_or(run.planner);
        self.relay(worker.id, issuer, brief::handover(task, &card), None);
        Ok(())
    }

    /// A new execution session of `template` for `task`, working in `cwd` and held for it
    /// (docs/tasks/S1.md §3.3). Named `name`, or `name-2`… when an earlier one (archived)
    /// has that name.
    pub(crate) async fn fresh_agent(
        &self,
        task: &Task,
        template: AgentId,
        name: &str,
        role: Role,
        cwd: &Path,
    ) -> Result<Agent, CoreError> {
        let name = self.free_name(name).await?;
        let agent = self.host.clone_agent(template, &name, role, cwd).await?;
        let claim = if role == Role::Reviewer {
            ClaimRole::Reviewer
        } else {
            ClaimRole::Worker
        };
        self.claim(&agent, task.run_id, Some(task.id), claim, true)
            .await?;
        Ok(agent)
    }

    /// `name`, or `name-2`, `name-3`… when an agent has it: execution sessions are archived,
    /// not deleted, and keep their names.
    pub(crate) async fn free_name(&self, name: &str) -> Result<String, CoreError> {
        let mut candidate = name.to_owned();
        for n in 2..1000 {
            match self
                .store
                .get_agent(&AgentKey::Name(candidate.clone()))
                .await
            {
                Err(CoreError::NotFound { .. }) => return Ok(candidate),
                Err(err) => return Err(err),
                Ok(_) => {
                    let suffix = format!("-{n}");
                    let mut base = name.to_owned();
                    base.truncate(32 - suffix.len());
                    candidate = format!("{}{suffix}", base.trim_end_matches('-'));
                }
            }
        }
        Err(CoreError::Conflict(format!("no free name like {name:?}")))
    }

    /// The task's worker and reviewer are done with it: what the run holds for the task is
    /// let go (docs/tasks/S1.md §3.4). Execution sessions are archived, agents working on it
    /// themselves go back to their workspace; nothing is deleted.
    pub(crate) async fn release_agents(&self, task: &mut Task) {
        task.assignee = None;
        task.reviewer = None;
        self.let_go_task(task.run_id, task.id, None).await;
    }

    pub(crate) async fn cleanup_worktree(&self, run: &Run, task: &Task, delete_branch: bool) {
        if let Some(path) = &task.worktree {
            let branch = task.branch.as_deref().filter(|_| delete_branch);
            if let Err(err) = self.ws_for(run).remove(&run.repo, path, branch).await {
                tracing::warn!(task = %task.key, error = %err, "cannot remove worktree");
            }
        }
    }

    pub(crate) async fn remove_integration(&self, run: &Run) {
        if let Err(err) = self
            .ws_for(run)
            .remove(&run.repo, &self.integration_path(run), None)
            .await
        {
            tracing::warn!(run = %run.id, error = %err, "cannot remove integration worktree");
        }
        self.remove_run_build(run);
    }

    /// Removes the run's own build directory (`<worktrees>/<run>/target`, used only where
    /// the repository's shared one could not be found, see `workspace::worktree_env`), and
    /// the run's directory once nothing else is in it.
    /// Called when the run's checkout goes: nothing builds there any more.
    pub(crate) fn remove_run_build(&self, run: &Run) {
        let dir = self.config.worktrees.join(short(run.id));
        let target = dir.join("target");
        if target.exists()
            && let Err(err) = std::fs::remove_dir_all(&target)
        {
            tracing::warn!(run = %run.id, error = %err, "cannot remove the run's build directory");
        }
        // Fails while task worktrees remain: those are cleaned up on their own.
        let _ = std::fs::remove_dir(&dir);
        self.prune_build_cache();
    }

    /// Removes shared build directories that grew past the limit and no run uses any more,
    /// in the background (it walks gigabytes). At startup, and whenever a run's checkout goes.
    pub fn prune_build_cache(&self) {
        let (root, limit) = (self.config.worktrees.clone(), self.config.build_cache_limit);
        tokio::task::spawn_blocking(move || {
            for (dir, size) in crate::workspace::prune_shared_targets(&root, limit) {
                tracing::info!(dir = %dir.display(), gb = size >> 30, "removed a shared build directory over its limit");
            }
        });
    }

    /// Completed when every task merged, or when some did not but the run branch changes
    /// something and the project's checks passed on it: the lead took over what the tasks
    /// left (DOGFOOD.md §10). Otherwise failed. `changed` is `has_changes`.
    pub(crate) async fn finish_run(
        &self,
        run: &mut Run,
        tasks: &[Task],
        changed: bool,
    ) -> Result<(), CoreError> {
        let merged = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Merged)
            .count();
        let missed = tasks.len() - merged;
        let checked = changed && crate::verify::all_passed(&run.checks);
        let (status, note) = match missed {
            0 => (RunStatus::Completed, None),
            _ if checked => (
                RunStatus::Completed,
                Some(format!(
                    "{missed} of {} tasks did not merge; the run branch has the rest and passed the checks",
                    tasks.len()
                )),
            ),
            _ => (
                RunStatus::Failed,
                Some(format!("{missed} of {} tasks did not merge", tasks.len())),
            ),
        };
        // The run lets go of everyone it holds: the lead itself goes home, and its checkout
        // goes once it has left (`checkout_vacated`); a lead copied for it is archived, its
        // history kept, and the checkout goes now (the callers committed what it left).
        // Restoring the copy brings the checkout back (`lead_returned`).
        self.let_go_run(run.id).await;
        if !self.lead_lives_in_integration(run).await {
            self.remove_integration(run).await;
        }
        self.set_run_status(run, status, note).await?;
        Ok(())
    }

    /// `agent` moved: the ended runs it led keep no checkout for it any more, unless the run
    /// still holds someone (a lead copied for it, who lives there). What was left
    /// uncommitted there is committed first.
    async fn checkout_vacated(&self, agent: &Agent) {
        let Ok(runs) = self.store.list_runs().await else {
            return;
        };
        for run in runs {
            if run.planner != agent.id || !run.status.is_terminal() || run.in_place {
                continue;
            }
            let dir = self.integration_path(&run);
            if !dir.exists() || agent.cwd == dir {
                continue;
            }
            let _guard = self.lock(run.id).await;
            if !self
                .store
                .list_claims(Some(run.id))
                .await
                .unwrap_or_default()
                .is_empty()
            {
                continue;
            }
            if let Err(err) = self
                .commit_lead(&run, &format!("crossagents: {} (after the run)", run.title))
                .await
            {
                tracing::warn!(run = %run.id, error = %err, "cannot commit the run's checkout");
                continue;
            }
            self.remove_integration(&run).await;
        }
    }

    /// Whether the run's lead is in its checkout and about: an archived copy keeps the
    /// directory as its own, but nobody works there until it is restored.
    pub(crate) async fn lead_lives_in_integration(&self, run: &Run) -> bool {
        !run.in_place
            && self
                .store
                .get_agent(&AgentKey::Id(run.planner))
                .await
                .is_ok_and(|lead| {
                    lead.archived_at.is_none() && lead.cwd == self.integration_path(run)
                })
    }

    /// Commits what the lead left uncommitted in the run's checkout, so that branching off
    /// the run branch, merging into it and landing it see the lead's work. `false` when there
    /// was nothing (or no checkout).
    pub(crate) async fn commit_lead(&self, run: &Run, message: &str) -> Result<bool, CoreError> {
        let dir = self.integration_path(run);
        if run.in_place || !dir.exists() {
            return Ok(false);
        }
        self.ws_for(run).commit_all(&dir, message).await
    }

    /// Whether the run branch brings anything into its base: a merged task, or the lead's
    /// own commits.
    pub(crate) async fn has_changes(&self, run: &Run, tasks: &[Task]) -> Result<bool, CoreError> {
        if tasks.iter().any(|t| t.status == TaskStatus::Merged) {
            return Ok(true);
        }
        if run.in_place {
            return Ok(false);
        }
        let diff = self
            .ws_for(run)
            .branch_diff(&run.repo, &run.base_branch, &run.branch)
            .await?;
        Ok(!diff.trim().is_empty())
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
        crossagents_core::Handoff::end(
            &mut task.handoffs,
            task.assignee,
            crossagents_core::HandoffOutcome::Failed,
        );
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
                        EventKind::AgentUpdated { agent } => {
                            let this = this.clone();
                            tokio::spawn(async move { this.checkout_vacated(&agent).await });
                        }
                        EventKind::AgentUsage { id, used, .. } => this.record_context(id, used),
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
                if run.config.policy.continuation.is_some()
                    && let Some(agent) = task.assignee
                    && self.host.is_busy(agent).await
                {
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
                // Opt-in runs recover through the durable, bounded clock. Restarting
                // must not bypass the cap or enqueue another recovery notice.
                if run.config.policy.continuation.is_some() {
                    continue;
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

/// What a person's new policy changes, in words the lead acts on.
fn policy_changes(old: &RunPolicy, new: &RunPolicy) -> Vec<String> {
    use crossagents_core::{AskPolicy, LandPolicy};
    let mut out = Vec::new();
    if old.land != new.land {
        out.push(
            match new.land {
                LandPolicy::Manual => "the person lands the result; do not land it yourself",
                LandPolicy::Auto => "you may land the result yourself (run_land)",
                LandPolicy::Pr => "you may open a pull request for the result (run_land)",
            }
            .to_owned(),
        );
    }
    if old.human_ask != new.human_ask {
        out.push(match new.human_ask {
            AskPolicy::Allow => "questions to the person wait for the answer".to_owned(),
            AskPolicy::Timeout => format!(
                "questions to the person fall back to their default after {} minutes",
                new.ask_timeout_minutes
            ),
            AskPolicy::Deny => "do not ask the person; decide yourself".to_owned(),
        });
    }
    if old.budget != new.budget {
        let b = &new.budget;
        let caps: Vec<String> = [
            b.max_usd.map(|v| format!("${v}")),
            b.max_tokens.map(|v| format!("{v} tokens")),
            b.max_minutes.map(|v| format!("{v} minutes")),
        ]
        .into_iter()
        .flatten()
        .collect();
        out.push(if caps.is_empty() {
            "the run has no budget cap".to_owned()
        } else {
            format!("budget: {}", caps.join(", "))
        });
    }
    out
}

pub(crate) fn first_line(text: &str, max: usize) -> String {
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
