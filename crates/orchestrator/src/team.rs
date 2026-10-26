//! The lead's say over the team (docs/tasks/H1.md §5.6): recruit teammates for a run,
//! retire them, change a teammate's model, interrupt a turn. Bounded by the run's policy.

use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AgentKey, AgentProfile, AgentStatus, CoreError, Role, Run, RunConfig,
    RunPolicy, model::SPAWNED_BY_AGENT,
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

/// What `agent_spawn` takes, in a run or in a conversation.
#[derive(Deserialize)]
struct SpawnArgs {
    #[serde(default)]
    name: Option<String>,
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

/// A conversation's recruiting policy: its `config.policy`, else up to 3 recruits, any model
/// a logged-in account can run, no spend cap.
pub fn chat_policy(agent: &Agent) -> RunPolicy {
    agent
        .config
        .get("policy")
        .and_then(|p| serde_json::from_value(p.clone()).ok())
        .unwrap_or(RunPolicy {
            max_agents: 3,
            ..RunPolicy::default()
        })
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

    /// `agent_spawn`: a run's lead recruits for the run; a conversation outside any run
    /// recruits for itself (chat mode). Both within their policy; a refusal says why.
    pub(crate) async fn agent_spawn(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        let a: SpawnArgs = args(input)?;
        match self.run_of(caller.id).await? {
            Some((run, _)) if !run.status.is_terminal() => {
                if run.planner != caller.id {
                    return Err(CoreError::Conflict(format!(
                        "not recruited: you work in the run \"{}\", and only its lead recruits; ask the lead with agent_send if the team needs someone",
                        run.title
                    )));
                }
                self.run_spawn(run, a).await
            }
            _ => self.chat_spawn(caller, a).await,
        }
    }

    async fn run_spawn(&self, run: Run, a: SpawnArgs) -> Result<Value, CoreError> {
        let policy = &run.config.policy;
        if !policy.spawn {
            return Err(CoreError::Conflict(
                "this run does not let you recruit; use the team you have".into(),
            ));
        }
        // The same name asked again (a retried call) is the same teammate.
        if let Some(name) = &a.name
            && let Ok(existing) = self.store.get_agent(&AgentKey::Name(name.clone())).await
        {
            if spawned_by(&existing).as_deref() == Some(run.id.to_string().as_str()) {
                return Ok(json!({ "agent": existing.name, "repeated": true }));
            }
            return Err(CoreError::Conflict(format!(
                "not recruited: the name {name} is taken; choose another, or leave name out to get one"
            )));
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
            name_source: crossagents_core::NameSource::Chosen,
            topic: None,
            archived_at: None,
            name: a.name.unwrap_or_default(),
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

    /// Recruiting by a conversation outside any run: bounded by the conversation's policy
    /// (on/off, at most `max_agents` recruits, an optional model list) and by the models a
    /// logged-in account can run. Recruits work in the conversation's folder, are listed under
    /// it and archived with it.
    /// The plugin and model a recruit runs on. A model asked for without a plugin picks the
    /// plugin whose logged-in account runs it ("haiku" from a Codex chat is a Claude agent);
    /// nothing asked for is the caller's own.
    async fn recruit_target(
        &self,
        caller: &Agent,
        a: &SpawnArgs,
    ) -> Result<(String, Option<String>), CoreError> {
        let Some(wanted) = &a.model else {
            let plugin = a.plugin.clone().unwrap_or_else(|| caller.plugin.clone());
            if self.host.available_models(&plugin).await?.is_empty() {
                return Err(CoreError::Conflict(format!(
                    "not recruited: no {plugin} agent can run here (its CLI is not installed, or no {plugin} account is logged in); tell the person, or recruit with a plugin that has one"
                )));
            }
            let model = (plugin == caller.plugin)
                .then(|| caller.model.clone())
                .flatten();
            return Ok((plugin, model));
        };
        let mut plugins = match &a.plugin {
            Some(plugin) => vec![plugin.clone()],
            None => self.host.plugin_names(),
        };
        // The caller's own plugin first: a model both offer stays with it.
        plugins.sort_by_key(|p| *p != caller.plugin);
        if !plugins.contains(&caller.plugin) && a.plugin.is_none() {
            plugins.insert(0, caller.plugin.clone());
        }
        let mut offered = Vec::new();
        for plugin in plugins {
            let available = self.host.available_models(&plugin).await?;
            if let Some(model) = available.iter().find(|m| m.eq_ignore_ascii_case(wanted)) {
                return Ok((plugin, Some(model.clone())));
            }
            if !available.is_empty() {
                offered.push(format!("{plugin}: {}", available.join(", ")));
            }
        }
        Err(CoreError::Conflict(format!(
            "not recruited: no logged-in account runs model {wanted} (available: {})",
            if offered.is_empty() {
                "none".to_owned()
            } else {
                offered.join("; ")
            }
        )))
    }

    async fn chat_spawn(&self, caller: &Agent, a: SpawnArgs) -> Result<Value, CoreError> {
        if let Some(recruiter) = caller.recruiter() {
            let by = self
                .store
                .get_agent(&AgentKey::Id(recruiter))
                .await
                .map_or_else(|_| recruiter.to_string(), |r| r.name);
            return Err(CoreError::Conflict(format!(
                "not recruited: you were recruited by {by}, and recruits do not recruit; tell {by} (agent_send) if more help is needed"
            )));
        }
        let policy = chat_policy(caller);
        if !policy.spawn {
            return Err(CoreError::Conflict(
                "not recruited: recruiting is turned off for this conversation; tell the person, or do the work yourself".into(),
            ));
        }
        let recruits: Vec<Agent> = self
            .store
            .list_agents()
            .await?
            .into_iter()
            .filter(|ag| ag.recruiter() == Some(caller.id) && ag.archived_at.is_none())
            .collect();
        // The same name asked again (a retried call) is the same teammate.
        if let Some(name) = &a.name
            && let Some(existing) = recruits
                .iter()
                .find(|r| r.name.to_lowercase() == name.to_lowercase())
        {
            return Ok(
                json!({ "agent": existing.name, "id": existing.id, "model": existing.model, "repeated": true }),
            );
        }
        if recruits.len() as u32 >= policy.max_agents {
            let names: Vec<&str> = recruits.iter().map(|r| r.name.as_str()).collect();
            return Err(CoreError::Conflict(format!(
                "not recruited: this conversation may have at most {} teammates and has {} ({}); retire one with agent_retire, or ask the person to raise the limit",
                policy.max_agents,
                recruits.len(),
                names.join(", ")
            )));
        }
        let role = a.role.unwrap_or(Role::Worker);
        if role == Role::Planner {
            return Err(CoreError::Conflict(
                "not recruited: recruit a worker or a reviewer; planners lead runs".into(),
            ));
        }
        let (plugin, model) = self.recruit_target(caller, &a).await?;
        if let Some(model) = &model
            && let Some(allowed) = &policy.spawn_models
            && !allowed.contains(model)
        {
            return Err(CoreError::Conflict(format!(
                "not recruited: model {model} is not on this conversation's list of models to recruit ({})",
                allowed.join(", ")
            )));
        }
        if let Some(name) = &a.name
            && crossagents_core::names::is_vendor_word(name, &self.host.denied_name_words())
        {
            return Err(CoreError::Conflict(format!(
                "not recruited: \"{name}\" is a vendor or model word, and names are for people; choose another name, or leave name out to get one (the model goes in model, not in the name)"
            )));
        }
        let mut config = serde_json::Map::new();
        config.insert(SPAWNED_BY_AGENT.into(), json!(caller.id));
        if plugin == caller.plugin
            && let Some(account) = caller.config.get("account")
        {
            config.insert("account".into(), account.clone());
        }
        let agent = Agent {
            id: AgentId::new(),
            name: a.name.unwrap_or_default(),
            name_source: crossagents_core::NameSource::Chosen,
            topic: None,
            archived_at: None,
            plugin,
            driver: None,
            model,
            role,
            cwd: caller.cwd.clone(),
            config: Value::Object(config),
            status: AgentStatus::Stopped,
            created_at: Utc::now(),
            profile: AgentProfile {
                display_name: None,
                title: a.title,
                avatar: None,
                color: None,
                persona: a.persona,
            },
        };
        let agent = self.host.spawn_agent(agent).await?;
        Ok(json!({
            "agent": agent.name,
            "id": agent.id,
            "role": agent.role,
            "plugin": agent.plugin,
            "model": agent.model,
            "name_source": agent.name_source,
            "next": format!("{} works in this conversation's folder; give it the work with agent_send(to = \"{}\")", agent.name, agent.name),
        }))
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
        let agent = match self.store.get_agent(&AgentKey::parse(&a.name)).await {
            Ok(agent) => agent,
            Err(CoreError::NotFound { .. }) => {
                return Ok(json!({ "retired": false, "repeated": true }));
            }
            Err(err) => return Err(err),
        };
        // A conversation's recruit is archived with its transcript.
        if agent.recruiter() == Some(caller.id) {
            if agent.archived_at.is_some() {
                return Ok(json!({ "retired": true, "repeated": true }));
            }
            self.host.archive_agent(agent.id).await?;
            return Ok(json!({ "retired": true, "archived": true }));
        }
        let run = self.lead_run(caller).await?;
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
