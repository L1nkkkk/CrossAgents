//! What the orchestrator needs from whoever runs the agents (the hub).

use std::path::Path;

use async_trait::async_trait;
use crossagents_core::{Agent, AgentId, CoreError, Role};

#[async_trait]
pub trait AgentHost: Send + Sync {
    /// Delivers a message to an agent, starting it if needed.
    async fn send(&self, agent: AgentId, text: String) -> Result<(), CoreError>;
    /// Creates a new agent with `template`'s plugin, model and settings, working in `cwd`.
    async fn clone_agent(
        &self,
        template: AgentId,
        name: &str,
        role: Role,
        cwd: &Path,
    ) -> Result<Agent, CoreError>;
    /// Stops and deletes an agent created with [`Self::clone_agent`].
    async fn remove_agent(&self, agent: AgentId) -> Result<(), CoreError>;
}
