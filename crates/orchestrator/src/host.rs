//! What the orchestrator needs from whoever runs the agents (the hub).

use std::path::Path;

use async_trait::async_trait;
use crossagents_core::{
    Agent, AgentId, CoreError, Envelope, MessageId, MessageSource, Role, RunId,
};
use serde_json::Value;

/// Where a task's worker runs, by quota.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// As planned: the template, on its own account.
    Same,
    /// The template on another account of the same vendor.
    Account { account: String, reason: String },
    /// Another teammate (another vendor, or an account of its own) instead of the template.
    Teammate { agent: AgentId, reason: String },
    /// Nothing better is left; it runs as planned. Why, to say so.
    Exhausted { reason: String },
}

#[async_trait]
pub trait AgentHost: Send + Sync {
    /// Delivers a message to an agent, starting it if needed.
    async fn send(
        &self,
        agent: AgentId,
        from: MessageSource,
        text: String,
    ) -> Result<(), CoreError>;
    /// Delivers a prepared message (id fixed by the sender, possibly a reply); `now` puts it
    /// first and interrupts the running turn.
    async fn send_envelope(
        &self,
        agent: AgentId,
        envelope: Envelope,
        now: bool,
    ) -> Result<(), CoreError>;
    /// Stops the agent's running turn, if any.
    async fn interrupt(&self, agent: AgentId) -> Result<(), CoreError>;
    /// Takes back a message that has not started yet.
    async fn withdraw(&self, agent: AgentId, message: MessageId) -> Result<(), CoreError>;
    /// Adds a line to the agent's notification digest (see `Runtime::notify`).
    async fn notify(&self, agent: AgentId, topic: &str, line: &str) -> Result<(), CoreError>;
    /// Creates a new agent with `template`'s plugin, model and settings, working in `cwd`.
    async fn clone_agent(
        &self,
        template: AgentId,
        name: &str,
        role: Role,
        cwd: &Path,
    ) -> Result<Agent, CoreError>;
    /// Like [`Self::clone_agent`], on `account` instead of the template's when given.
    async fn clone_agent_on(
        &self,
        template: AgentId,
        name: &str,
        role: Role,
        cwd: &Path,
        account: Option<&str>,
    ) -> Result<Agent, CoreError> {
        let _ = account;
        self.clone_agent(template, name, role, cwd).await
    }
    /// Where a clone of `template` should run, given what the accounts have left
    /// (docs/tasks/DOGFOOD.md §5). `team` are the run's teammates to fall back on; `limited`
    /// is an agent whose account just hit its limit.
    async fn route(&self, template: AgentId, team: &[AgentId], limited: Option<AgentId>) -> Route {
        let _ = (template, team, limited);
        Route::Same
    }
    /// Registers a new agent (`agent_spawn`); fails if its plugin is unknown. An empty name
    /// gets a made-up one, a taken one an ordinal ("Nova 2").
    async fn spawn_agent(&self, agent: Agent) -> Result<Agent, CoreError>;
    /// The models an account that is logged in can run for `plugin` (empty: no such account).
    async fn available_models(&self, plugin: &str) -> Result<Vec<String>, CoreError>;
    /// The plugins agents can run on.
    fn plugin_names(&self) -> Vec<String> {
        Vec::new()
    }
    /// Words no agent name may contain: vendors' and every known model's.
    fn denied_name_words(&self) -> Vec<String>;
    /// Changes an agent's model, from its next turn when it is running.
    async fn set_model(&self, agent: AgentId, model: Option<String>) -> Result<Agent, CoreError>;
    /// Spend of a run (or of everything) and the accounts' quota, for `usage_get`.
    async fn usage_report(&self, run: Option<RunId>) -> Result<Value, CoreError>;
    /// Takes the messages waiting for an agent that runs outside the hub.
    async fn take_inbox(&self, agent: AgentId) -> Result<Vec<Value>, CoreError>;
    /// Archives a conversation's recruit: stopped and out of the lists, its transcript kept.
    async fn archive_agent(&self, agent: AgentId) -> Result<(), CoreError>;
    /// Stops and deletes an agent created with [`Self::clone_agent`].
    async fn remove_agent(&self, agent: AgentId) -> Result<(), CoreError>;
    /// Stops the agent's session and process, keeping the agent; its next message starts it
    /// again.
    async fn stop_agent(&self, agent: AgentId) -> Result<(), CoreError>;
}
