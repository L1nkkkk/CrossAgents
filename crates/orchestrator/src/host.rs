//! What the orchestrator needs from whoever runs the agents (the hub).

use std::path::Path;

use async_trait::async_trait;
use crossagents_core::{Agent, AgentId, CoreError, Envelope, MessageId, MessageSource, Role};

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
    /// Registers a new agent (`agent_spawn`); fails if its plugin is unknown or the name
    /// is taken.
    async fn spawn_agent(&self, agent: Agent) -> Result<Agent, CoreError>;
    /// Changes an agent's model, from its next turn when it is running.
    async fn set_model(&self, agent: AgentId, model: Option<String>) -> Result<Agent, CoreError>;
    /// Stops and deletes an agent created with [`Self::clone_agent`].
    async fn remove_agent(&self, agent: AgentId) -> Result<(), CoreError>;
}
