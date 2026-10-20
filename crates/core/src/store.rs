//! Persistence abstraction. See docs/DESIGN.md §5.

use async_trait::async_trait;

use crate::error::CoreError;
use crate::event::{Event, EventKind};
use crate::ids::AgentId;
use crate::model::{Agent, AgentStatus};

/// Look an agent up by id or by its unique name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentKey {
    Id(AgentId),
    Name(String),
}

impl AgentKey {
    /// Parses a user-supplied reference: a UUID is an id, anything else a name.
    pub fn parse(s: &str) -> Self {
        s.parse()
            .map_or_else(|_| Self::Name(s.to_owned()), Self::Id)
    }
}

#[async_trait]
pub trait Store: Send + Sync {
    /// Fails with [`CoreError::Conflict`] if the name is taken.
    async fn insert_agent(&self, agent: &Agent) -> Result<(), CoreError>;
    async fn get_agent(&self, key: &AgentKey) -> Result<Agent, CoreError>;
    async fn list_agents(&self) -> Result<Vec<Agent>, CoreError>;
    async fn delete_agent(&self, id: AgentId) -> Result<(), CoreError>;
    async fn update_agent_status(&self, id: AgentId, status: AgentStatus) -> Result<(), CoreError>;

    /// Persists the event, assigning its `seq`, `id` and `ts`.
    async fn append_event(&self, kind: EventKind) -> Result<Event, CoreError>;
    /// Events with `seq > after`, oldest first.
    async fn events_since(&self, after: u64, limit: u32) -> Result<Vec<Event>, CoreError>;
}
