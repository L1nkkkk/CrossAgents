//! Where agents belong and what holds them (docs/tasks/S1.md §3.1): each agent's home is its
//! workspace, and a claim in the store says which run (and task) it works for now.

use std::path::PathBuf;

use crossagents_core::{
    Agent, AgentClaim, AgentId, AgentKey, ClaimRole, CoreError, EventKind, Run, RunId, Task,
    TaskId, TaskStatus,
};

use crate::brief;
use crate::engine::{Orchestrator, short};
use crate::host::Route;

/// Agent config key from before homes: the directory a lead lived in before a run moved it
/// into its checkout. Read once, by [`Orchestrator::migrate_homes`], and dropped.
const LEAD_HOME: &str = "lead_home";

impl Orchestrator {
    /// Gives agents from before homes the workspace they belong to: a lead the run moved in
    /// gets the directory it came from (`lead_home`, which goes); an agent living in a run's
    /// worktree gets that run's repository. Run at hub start; doing it again changes nothing.
    /// Returns who moved, by name, with their home.
    pub async fn migrate_homes(&self) -> Result<Vec<(String, PathBuf)>, CoreError> {
        let runs = self.store.list_runs().await?;
        let mut moved = Vec::new();
        for agent in self.store.list_agents().await? {
            let lead_home = agent
                .config
                .get(LEAD_HOME)
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from);
            let home = match &lead_home {
                Some(home) => Some(home.clone()),
                None => agent
                    .home()
                    .strip_prefix(&self.config.worktrees)
                    .ok()
                    .and_then(|rest| rest.components().next())
                    .and_then(|dir| {
                        let dir = dir.as_os_str().to_string_lossy();
                        runs.iter().find(|r| short(r.id) == dir.as_ref())
                    })
                    .map(|run| run.repo.clone()),
            };
            if lead_home.is_none()
                && home.is_none()
                && agent.home().starts_with(&self.config.worktrees)
            {
                tracing::warn!(agent = %agent.name, home = %agent.home().display(), "lives in the worktree of a run that is gone; its home stays there");
            }
            let Some(home) = home else { continue };
            if home != agent.home() {
                self.store.update_agent_home(agent.id, &home).await?;
            }
            if lead_home.is_some() {
                let mut config = agent.config.clone();
                if let Some(map) = config.as_object_mut() {
                    map.remove(LEAD_HOME);
                }
                self.store.update_agent_config(agent.id, &config).await?;
            }
            if home != agent.home() || lead_home.is_some() {
                let agent = self.store.get_agent(&AgentKey::Id(agent.id)).await?;
                self.publish(EventKind::AgentUpdated {
                    agent: agent.clone(),
                })
                .await;
                moved.push((agent.name, home));
            }
        }
        Ok(moved)
    }

    /// Drops claims left by runs and tasks that ended (or went) while no hub was there to
    /// let go of them, and gives open runs from before claims theirs (see
    /// [`Self::backfill_claims`]). Run at hub start. Returns how many were dropped.
    pub async fn settle_claims(&self) -> Result<usize, CoreError> {
        self.backfill_claims().await?;
        let mut dropped = 0;
        for claim in self.store.list_claims(None).await? {
            let run_open = match self.store.get_run(claim.run_id).await {
                Ok(run) => !run.status.is_terminal(),
                Err(CoreError::NotFound { .. }) => false,
                Err(err) => return Err(err),
            };
            let task_open = match claim.task_id {
                None => true,
                Some(task) => match self.store.get_task(task).await {
                    Ok(task) => !task.status.is_terminal(),
                    Err(CoreError::NotFound { .. }) => false,
                    Err(err) => return Err(err),
                },
            };
            if !(run_open && task_open)
                && self
                    .store
                    .release_agent(claim.agent_id, claim.run_id)
                    .await?
            {
                dropped += 1;
            }
        }
        Ok(dropped)
    }

    /// Open runs from before claims hold nothing, so ending them would let nobody go: their
    /// lead, and each open task's worker and reviewer, are claimed for them as they are now.
    /// A lead the run moved in (`lead_home`) is the agent itself; the rest were copies and
    /// clones made for the run. Agents some claim already holds are left as they are.
    async fn backfill_claims(&self) -> Result<(), CoreError> {
        for run in self.store.list_runs().await? {
            if run.status.is_terminal() || !self.store.list_claims(Some(run.id)).await?.is_empty() {
                continue;
            }
            let mut claims = Vec::new();
            if let Ok(lead) = self.store.get_agent(&AgentKey::Id(run.planner)).await
                && !run.in_place
                && lead.cwd == self.integration_path(&run)
            {
                let moved = lead.config.get(LEAD_HOME).is_some();
                claims.push(AgentClaim::new(
                    lead.id,
                    run.id,
                    None,
                    ClaimRole::Lead,
                    !moved,
                ));
            }
            for task in self.store.list_tasks(run.id).await? {
                if task.status.is_terminal() {
                    continue;
                }
                for (agent, role) in [
                    (task.assignee, ClaimRole::Worker),
                    (task.reviewer, ClaimRole::Reviewer),
                ] {
                    if let Some(agent) = agent {
                        claims.push(AgentClaim::new(agent, run.id, Some(task.id), role, true));
                    }
                }
            }
            for claim in claims {
                self.store.claim_agent(&claim).await?;
            }
        }
        Ok(())
    }
}

/// What letting go of an agent did with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LetGo {
    /// The agent itself, back in its workspace (or on its way, once its turn ends).
    Home(String),
    /// An execution session, archived: stopped, out of the lists, its conversation kept.
    Archived(String),
}

impl Orchestrator {
    /// Holds `agent` for `run` (and `task`), or says who has it: one atomic insert in the
    /// store, so two runs cannot both take it (docs/tasks/S1.md §3.1).
    pub(crate) async fn claim(
        &self,
        agent: &Agent,
        run: RunId,
        task: Option<TaskId>,
        role: ClaimRole,
        fresh: bool,
    ) -> Result<(), CoreError> {
        let claim = AgentClaim::new(agent.id, run, task, role, fresh);
        match self.store.claim_agent(&claim).await? {
            None => Ok(()),
            Some(holder) => Err(CoreError::Busy(self.held_by(agent, &holder).await)),
        }
    }

    /// `Rex is busy: it works on #auth of run "Login" (…)`.
    pub(crate) async fn held_by(&self, agent: &Agent, holder: &AgentClaim) -> String {
        let run = match self.store.get_run(holder.run_id).await {
            Ok(run) => format!("run \"{}\" ({})", run.title, run.id),
            Err(_) => format!("run {}", holder.run_id),
        };
        let task = match holder.task_id {
            Some(id) => match self.store.get_task(id).await {
                Ok(task) => format!("#{}", task.key),
                Err(_) => format!("task {id}"),
            },
            None => String::new(),
        };
        let doing = match holder.role {
            ClaimRole::Lead => format!("it leads {run}"),
            ClaimRole::Worker => format!("it works on {task} of {run}"),
            ClaimRole::Reviewer => format!("it reviews {task} of {run}"),
        };
        format!("{} is busy: {doing}", agent.name)
    }
}

impl Orchestrator {
    /// Lets go of `agent` if `run` holds it, and puts it away (docs/tasks/S1.md §3.4): the
    /// agent itself goes back to its workspace, an execution session is archived. Another
    /// run's claim on it is left alone, and so is the agent. `None` when `run` held nothing.
    pub(crate) async fn let_go(&self, run: RunId, agent: AgentId) -> Option<LetGo> {
        let claim = self.store.claim_of(agent).await.ok().flatten()?;
        if claim.run_id != run {
            return None;
        }
        match self.store.release_agent(agent, run).await {
            Ok(true) => {}
            Ok(false) => return None,
            Err(err) => {
                tracing::warn!(%agent, error = %err, "cannot let go of the agent");
                return None;
            }
        }
        let name = self
            .name_of(agent)
            .await
            .unwrap_or_else(|| agent.to_string());
        if claim.fresh {
            // A lead copied for the run keeps its directory: its driver resumes only there,
            // and restoring it brings the checkout back (`lead_returned`).
            let keep_dir = claim.role == ClaimRole::Lead;
            if let Err(err) = self.put_away(agent, keep_dir).await {
                tracing::warn!(%agent, error = %err, "cannot archive the execution session");
            }
            Some(LetGo::Archived(name))
        } else {
            if let Err(err) = self.host.send_home(agent).await {
                tracing::warn!(%agent, error = %err, "cannot send the agent home");
            }
            Some(LetGo::Home(name))
        }
    }

    /// Archives an execution session and, unless `keep_dir`, puts it back in its workspace,
    /// so that restoring it resumes there rather than in a worktree that is gone.
    async fn put_away(&self, agent: AgentId, keep_dir: bool) -> Result<(), CoreError> {
        self.host.archive_agent(agent).await?;
        let record = self.store.get_agent(&AgentKey::Id(agent)).await?;
        if !keep_dir && record.cwd != record.home() {
            self.store.update_agent_cwd(agent, record.home()).await?;
            let record = self.store.get_agent(&AgentKey::Id(agent)).await?;
            self.publish(EventKind::AgentUpdated { agent: record })
                .await;
        }
        Ok(())
    }

    /// Lets go of what `run` holds for `task`: its worker and reviewer, or only the one in
    /// `role`. Only rows that are this run's are touched.
    pub(crate) async fn let_go_task(
        &self,
        run: RunId,
        task: TaskId,
        role: Option<ClaimRole>,
    ) -> Vec<LetGo> {
        let claims = self.store.list_claims(Some(run)).await.unwrap_or_default();
        let mut done = Vec::new();
        for claim in claims
            .into_iter()
            .filter(|c| c.task_id == Some(task) && role.is_none_or(|r| c.role == r))
        {
            done.extend(self.let_go(run, claim.agent_id).await);
        }
        done
    }

    /// Lets go of everything `run` holds.
    pub(crate) async fn let_go_run(&self, run: RunId) -> Vec<LetGo> {
        let claims = self.store.list_claims(Some(run)).await.unwrap_or_default();
        let mut done = Vec::new();
        for claim in claims {
            done.extend(self.let_go(run, claim.agent_id).await);
        }
        done
    }
}

impl Orchestrator {
    /// The checks a session must pass before it works on `task` itself, with its
    /// conversation (`fresh: false`, docs/tasks/S1.md §3.3) that do not depend on the
    /// moment: it belongs to the run's workspace, its driver resumes a session elsewhere, and
    /// it is not archived (unless `restore` says to bring it back, which happens later).
    pub(crate) async fn may_take_self(
        &self,
        run: &Run,
        agent: &Agent,
        restore: bool,
    ) -> Result<(), CoreError> {
        if agent.home() != run.repo {
            return Err(CoreError::Conflict(format!(
                "{} belongs to {}, not to this run's workspace {}: give the task to a new execution session (leave fresh out)",
                agent.name,
                agent.home().display(),
                run.repo.display()
            )));
        }
        if !self.host.can_move(agent).await {
            return Err(CoreError::Conflict(format!(
                "{}'s driver cannot resume its session in another directory, so it cannot work on a task itself: leave fresh out, and a new execution session takes the task",
                agent.name
            )));
        }
        if agent.archived_at.is_some() && !restore {
            return Err(CoreError::Conflict(format!(
                "{} is archived: restore it first (fresh: false with restore: true), or leave fresh out",
                agent.name
            )));
        }
        Ok(())
    }

    /// Puts `agent` itself on `task` (`fresh: false`, docs/tasks/S1.md §3.3), after checking
    /// in order: same workspace, a driver that moves, not archived (or restored now), no
    /// person waiting on it, held for nobody else (one atomic claim), and not moved off its
    /// account by quota. Then it moves into the task's worktree with its conversation;
    /// failing to resume there is an error, never a new session. On any refusal nothing is
    /// held and the agent is where it was.
    pub(crate) async fn take_self(
        &self,
        run: &Run,
        task: &Task,
        agent: &Agent,
        role: ClaimRole,
        restore: bool,
    ) -> Result<(), CoreError> {
        self.may_take_self(run, agent, restore).await?;
        if agent.archived_at.is_some() {
            self.host.restore_agent(agent.id).await?;
        }
        if self.host.serving_person(agent.id).await {
            return Err(CoreError::Busy(format!(
                "{} is answering a person, or a person's message waits for it: the person comes first. Pick someone else, leave fresh out, or try again later",
                agent.name
            )));
        }
        self.claim(agent, run.id, Some(task.id), role, false)
            .await?;
        let mut team: Vec<AgentId> = run.config.team.clone();
        team.push(run.config.worker);
        if let Route::Account { reason, .. } | Route::Teammate { reason, .. } =
            self.host.route(agent.id, &team, None).await
        {
            let _ = self.store.release_agent(agent.id, run.id).await;
            return Err(CoreError::Conflict(format!(
                "{} cannot work on it now: {reason}. Its conversation lives on its own account, so another account means a new session: leave fresh out",
                agent.name
            )));
        }
        let dir = task.worktree.clone().unwrap_or_else(|| run.repo.clone());
        if let Err(err) = self.host.place(agent.id, &dir, true).await {
            let _ = self.store.release_agent(agent.id, run.id).await;
            // It may have been moved before its session failed to resume: back home.
            let _ = self.host.send_home(agent.id).await;
            return Err(match err {
                CoreError::Agent(why) => CoreError::Agent(format!(
                    "{why} (上下文没能延续). Give the task to a new execution session (leave fresh out) or to someone else"
                )),
                other => other,
            });
        }
        Ok(())
    }
}

/// What a session is doing for a run, for its page (docs/tasks/S1.md §3.3).
#[derive(Debug, Clone, PartialEq)]
pub struct Execution {
    pub claim: AgentClaim,
    pub run_title: String,
    pub task_key: Option<String>,
}

impl Orchestrator {
    /// What holds `agent` now, if anything.
    pub async fn execution_of(&self, agent: AgentId) -> Result<Option<Execution>, CoreError> {
        let Some(claim) = self.store.claim_of(agent).await? else {
            return Ok(None);
        };
        let run_title = match self.store.get_run(claim.run_id).await {
            Ok(run) => run.title,
            Err(_) => claim.run_id.to_string(),
        };
        let task_key = match claim.task_id {
            Some(id) => self.store.get_task(id).await.ok().map(|t| t.key),
            None => None,
        };
        Ok(Some(Execution {
            claim,
            run_title,
            task_key,
        }))
    }

    /// The person stops what `agent` does on a task (docs/tasks/S1.md §3.3): its turn is
    /// interrupted, the execution ends as interrupted, the run lets go of it (home, or
    /// archived when it is an execution session), and the task's issuer hears it and decides
    /// who goes on. Messages the person sent it stay in line.
    pub async fn interrupt_execution(&self, agent: AgentId) -> Result<Task, CoreError> {
        let record = self.store.get_agent(&AgentKey::Id(agent)).await?;
        let Some(claim) = self.store.claim_of(agent).await? else {
            return Err(CoreError::Conflict(format!(
                "{} is not working on any task",
                record.name
            )));
        };
        let Some(task_id) = claim.task_id else {
            return Err(CoreError::Conflict(format!(
                "{} leads its run rather than working on a task: stop its turn, or cancel the run",
                record.name
            )));
        };
        let _guard = self.lock(claim.run_id).await;
        let run = self.store.get_run(claim.run_id).await?;
        let mut task = self.store.get_task(task_id).await?;
        if let Err(err) = self.host.interrupt(agent).await {
            tracing::warn!(%agent, error = %err, "cannot interrupt the turn");
        }
        if claim.role != ClaimRole::Reviewer {
            crossagents_core::Handoff::end(
                &mut task.handoffs,
                Some(agent),
                crossagents_core::HandoffOutcome::Interrupted,
            );
        }
        let what = match claim.role {
            ClaimRole::Reviewer => {
                task.reviewer = None;
                if task.status == TaskStatus::Reviewing {
                    self.transition(&mut task, TaskStatus::Passed).await?;
                } else {
                    task.updated_at = chrono::Utc::now();
                    self.store.update_task(&task).await?;
                }
                "reviewing"
            }
            _ => {
                task.assignee = None;
                if matches!(task.status, TaskStatus::Running | TaskStatus::WaitingAnswer) {
                    self.transition(&mut task, TaskStatus::Stalled).await?;
                } else {
                    task.updated_at = chrono::Utc::now();
                    self.store.update_task(&task).await?;
                }
                "working on"
            }
        };
        self.let_go(run.id, agent).await;
        self.notice(
            task.issuer_or(run.planner),
            &format!("task:{}", task.id),
            &format!(
                "The person interrupted {} {what} {}. Nobody works on it now: task_assign it (a new execution session by default, or someone with fresh: false), or drop it.",
                record.name,
                brief::task_ref(&task)
            ),
        )
        .await;
        Ok(task)
    }
}
