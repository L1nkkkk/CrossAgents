//! Where agents belong and what holds them (docs/tasks/S1.md §3.1): each agent's home is its
//! workspace, and a claim in the store says which run (and task) it works for now.

use std::path::PathBuf;

use crossagents_core::{AgentKey, CoreError, EventKind};

use crate::engine::{LEAD_HOME, Orchestrator, short};

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
    /// let go of them. Run at hub start. Returns how many went.
    pub async fn settle_claims(&self) -> Result<usize, CoreError> {
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
}
