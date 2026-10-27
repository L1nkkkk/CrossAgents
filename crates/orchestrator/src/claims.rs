//! Where agents belong and what holds them (docs/tasks/S1.md §3.1): each agent's home is its
//! workspace, and a claim in the store says which run (and task) it works for now.

use std::path::PathBuf;

use crossagents_core::{
    Agent, AgentClaim, AgentId, AgentKey, ClaimRole, CoreError, EventKind, RunId, TaskId,
};

use crate::engine::{Orchestrator, short};

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
            if lead_home.is_none() && home.is_none() && agent.home().starts_with(&self.config.worktrees) {
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
            if !(run_open && task_open) && self.store.release_agent(claim.agent_id, claim.run_id).await? {
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
                claims.push(AgentClaim::new(lead.id, run.id, None, ClaimRole::Lead, !moved));
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
        let name = self.name_of(agent).await.unwrap_or_else(|| agent.to_string());
        if claim.fresh {
            if let Err(err) = self.put_away(agent).await {
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

    /// Archives an execution session and puts it back in its workspace, so that restoring
    /// it resumes there rather than in a worktree that is gone.
    async fn put_away(&self, agent: AgentId) -> Result<(), CoreError> {
        self.host.archive_agent(agent).await?;
        let record = self.store.get_agent(&AgentKey::Id(agent)).await?;
        if record.cwd != record.home() {
            self.store.update_agent_cwd(agent, record.home()).await?;
            let record = self.store.get_agent(&AgentKey::Id(agent)).await?;
            self.publish(EventKind::AgentUpdated { agent: record }).await;
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

    /// Lets go of everything `run` holds; `keep_copied_lead` leaves a lead copied for the
    /// run (it lives in the run's checkout as long as that is kept).
    pub(crate) async fn let_go_run(&self, run: RunId, keep_copied_lead: bool) -> Vec<LetGo> {
        let claims = self.store.list_claims(Some(run)).await.unwrap_or_default();
        let mut done = Vec::new();
        for claim in claims {
            if keep_copied_lead && claim.role == ClaimRole::Lead && claim.fresh {
                continue;
            }
            done.extend(self.let_go(run, claim.agent_id).await);
        }
        done
    }
}
