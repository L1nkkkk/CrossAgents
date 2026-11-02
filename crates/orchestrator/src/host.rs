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
    /// Keeps a contextual notice for the next message without starting an agent or a turn.
    /// Replaces an earlier notice on the same topic.
    async fn tell_later(&self, agent: AgentId, topic: &str, line: &str) -> Result<(), CoreError> {
        let _ = (agent, topic, line);
        Err(CoreError::Unsupported("passive notices".into()))
    }
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
    /// Archives an agent (and the ones its conversation recruited): stopped and out of the
    /// lists, its transcript kept. Nothing the orchestrator does deletes an agent; only a
    /// person does (docs/tasks/S1.md §3.4).
    async fn archive_agent(&self, agent: AgentId) -> Result<(), CoreError>;
    /// Archives the agent as [`AgentHost::archive_agent`] does, but only once its turn has
    /// ended and nothing waits for it (now when it is idle): a lead closing its own run is
    /// still in the turn that called `run_finish`, and stopping it there cuts off its reply
    /// to the person. `true` when it was archived now.
    async fn archive_agent_after_turn(&self, agent: AgentId) -> Result<bool, CoreError> {
        self.archive_agent(agent).await.map(|_| true)
    }
    /// Stops the agent's session and process, keeping the agent; its next message starts it
    /// again.
    async fn stop_agent(&self, agent: AgentId) -> Result<(), CoreError>;
    /// Moves the agent to work in `dir` (docs/tasks/S1.md §3.1); [`CoreError::Busy`] while
    /// a turn runs or messages wait for it. With `carry` its session resumes there at once,
    /// and failing to is an error: it never quietly becomes a new session.
    async fn place(&self, agent: AgentId, dir: &Path, carry: bool) -> Result<(), CoreError> {
        let _ = (agent, dir, carry);
        Err(CoreError::Unsupported("moving agents".into()))
    }
    /// Sends the agent back to its workspace: now when it is idle, else once its turn ends.
    /// `true` when it moved now.
    async fn send_home(&self, agent: AgentId) -> Result<bool, CoreError> {
        let _ = agent;
        Err(CoreError::Unsupported("moving agents".into()))
    }
    /// Whether a turn runs or any message is queued. Unknown hosts fail closed.
    async fn is_busy(&self, agent: AgentId) -> bool {
        let _ = agent;
        true
    }
    /// Whether a person waits on the agent: its turn answers their message, or one waits.
    async fn serving_person(&self, agent: AgentId) -> bool {
        let _ = agent;
        false
    }
    /// Brings an archived agent back to the lists (and its recruits with it).
    async fn restore_agent(&self, agent: AgentId) -> Result<(), CoreError> {
        let _ = agent;
        Err(CoreError::Unsupported("restoring agents".into()))
    }
    /// Whether the agent's driver resumes its session in another directory (`move_cwd`).
    async fn can_move(&self, agent: &Agent) -> bool {
        let _ = agent;
        false
    }
}
