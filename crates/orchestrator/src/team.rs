//! The lead's say over the team (docs/tasks/H1.md §5.6): recruit teammates for a run,
//! retire them, change a teammate's model, interrupt a turn. Bounded by the run's policy.

use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AgentKey, AgentProfile, AgentStatus, CoreError, Role, Run, RunConfig,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::Orchestrator;
use crate::tools::args;

/// `config` key marking an agent recruited by a run.
const SPAWNED_BY: &str = "spawned_by_run";

fn spawned_by(agent: &Agent) -> Option<String> {
    agent
        .config
        .get(SPAWNED_BY)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

impl Orchestrator {
    /// The open run the caller leads.
    async fn lead_run(&self, caller: &Agent) -> Result<Run, CoreError> {
        match self.run_of(caller.id).await? {
            Some((run, _)) if run.planner == caller.id && !run.status.is_terminal() => Ok(run),
            _ => Err(CoreError::Conflict("you lead no open run".into())),
        }
    }

    /// An agent the lead may manage: on the run's team, working on one of its tasks, or
    /// recruited by it.
    async fn run_member(&self, run: &Run, name: &str) -> Result<Agent, CoreError> {
        let agent = self.store.get_agent(&AgentKey::parse(name)).await?;
        let tasks = self.store.list_tasks(run.id).await?;
        let member = run.config.team.contains(&agent.id)
            || run.config.worker == agent.id
            || spawned_by(&agent).as_deref() == Some(run.id.to_string().as_str())
            || tasks
                .iter()
                .any(|t| t.assignee == Some(agent.id) || t.reviewer == Some(agent.id));
        if member {
            Ok(agent)
        } else {
            Err(CoreError::Conflict(format!(
                "{} is not part of your run",
                agent.name
            )))
        }
    }

    pub(crate) async fn agent_spawn(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            name: String,
            #[serde(default)]
            plugin: Option<String>,
            #[serde(default)]
            model: Option<String>,
            #[serde(default)]
            role: Option<Role>,
            #[serde(default)]
            display_name: Option<String>,
            #[serde(default)]
            title: Option<String>,
            #[serde(default)]
            persona: Option<String>,
        }
        let a: A = args(input)?;
        let run = self.lead_run(caller).await?;
        let policy = &run.config.policy;
        if !policy.spawn {
            return Err(CoreError::Conflict(
                "this run does not let you recruit; use the team you have".into(),
            ));
        }
        // The same name asked again (a retried call) is the same teammate.
        if let Ok(existing) = self.store.get_agent(&AgentKey::Name(a.name.clone())).await {
            if spawned_by(&existing).as_deref() == Some(run.id.to_string().as_str()) {
                return Ok(json!({ "agent": existing.name, "repeated": true }));
            }
            return Err(CoreError::Conflict(format!("the name {} is taken", a.name)));
        }
        let recruited = self
            .store
            .list_agents()
            .await?
            .iter()
            .filter(|ag| spawned_by(ag).as_deref() == Some(run.id.to_string().as_str()))
            .count();
        if recruited as u32 >= policy.max_agents {
            return Err(CoreError::Conflict(format!(
                "this run may recruit at most {} teammates; retire one first",
                policy.max_agents
            )));
        }
        let role = a.role.unwrap_or(Role::Worker);
        if role == Role::Planner {
            return Err(CoreError::Conflict(
                "a run has one lead: recruit workers or reviewers".into(),
            ));
        }
        // Defaults: the run's default worker's plugin (and model, unless one is named).
        let default = self
            .store
            .get_agent(&AgentKey::Id(run.config.worker))
            .await?;
        let plugin = a.plugin.unwrap_or_else(|| default.plugin.clone());
        let model = a.model.or_else(|| {
            (plugin == default.plugin)
                .then(|| default.model.clone())
                .flatten()
        });
        let mut config = serde_json::Map::new();
        config.insert(SPAWNED_BY.into(), json!(run.id));
        let agent = Agent {
            id: AgentId::new(),
            name: a.name,
            plugin,
            driver: None,
            model,
            role,
            cwd: run.repo.clone(),
            config: Value::Object(config),
            status: AgentStatus::Stopped,
            created_at: Utc::now(),
            profile: AgentProfile {
                display_name: a.display_name,
                title: a.title,
                avatar: None,
                color: None,
                persona: a.persona,
            },
        };
        let agent = self.host.spawn_agent(agent).await?;
        // A teammate of the run: tasks can be assigned to it by name.
        let _guard = self.lock(run.id).await;
        let mut run = self.store.get_run(run.id).await?;
        let RunConfig { team, .. } = &mut run.config;
        if !team.contains(&agent.id) {
            team.push(agent.id);
        }
        run.updated_at = Utc::now();
        self.store.update_run(&run).await?;
        Ok(
            json!({ "agent": agent.name, "role": agent.role, "plugin": agent.plugin, "model": agent.model }),
        )
    }

    pub(crate) async fn agent_retire(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            name: String,
        }
        let a: A = args(input)?;
        let run = self.lead_run(caller).await?;
        let agent = match self.store.get_agent(&AgentKey::parse(&a.name)).await {
            Ok(agent) => agent,
            Err(CoreError::NotFound { .. }) => {
                return Ok(json!({ "retired": false, "repeated": true }));
            }
            Err(err) => return Err(err),
        };
        if spawned_by(&agent).as_deref() != Some(run.id.to_string().as_str()) {
            return Err(CoreError::Conflict(format!(
                "{} was not recruited by this run; only recruits can be retired",
                agent.name
            )));
        }
        let busy = self
            .store
            .list_tasks(run.id)
            .await?
            .iter()
            .any(|t| t.worker_template == Some(agent.id) && !t.status.is_terminal());
        if busy {
            return Err(CoreError::Conflict(format!(
                "{} still has open tasks; reassign or finish them first",
                agent.name
            )));
        }
        self.host.remove_agent(agent.id).await?;
        let _guard = self.lock(run.id).await;
        let mut run = self.store.get_run(run.id).await?;
        run.config.team.retain(|id| *id != agent.id);
        self.store.update_run(&run).await?;
        Ok(json!({ "retired": true }))
    }

    pub(crate) async fn agent_set_model(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            name: String,
            model: String,
        }
        let a: A = args(input)?;
        let run = self.lead_run(caller).await?;
        let agent = self.run_member(&run, &a.name).await?;
        let agent = self.host.set_model(agent.id, Some(a.model)).await?;
        Ok(
            json!({ "agent": agent.name, "model": agent.model, "note": "applies from its next turn" }),
        )
    }

    pub(crate) async fn agent_interrupt(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            name: String,
        }
        let a: A = args(input)?;
        let run = self.lead_run(caller).await?;
        let agent = self.run_member(&run, &a.name).await?;
        self.host.interrupt(agent.id).await?;
        Ok(json!({ "interrupted": agent.name }))
    }
}
