//! Persistence abstraction. See docs/DESIGN.md §5.

use async_trait::async_trait;

use crate::account::Account;
use crate::error::CoreError;
use crate::event::{Event, EventKind};
use crate::ids::{AgentId, RunId, TaskId};
use crate::model::{Agent, AgentProfile, AgentStatus, Run, Task};

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
    /// Replaces the agent's profile.
    async fn update_agent_profile(
        &self,
        id: AgentId,
        profile: &AgentProfile,
    ) -> Result<(), CoreError> {
        let _ = (id, profile);
        Err(CoreError::Unsupported("updating agent profiles".into()))
    }
    /// Replaces the agent's model; `None` leaves it to the agent's own default.
    async fn update_agent_model(&self, id: AgentId, model: Option<&str>) -> Result<(), CoreError> {
        let _ = (id, model);
        Err(CoreError::Unsupported("updating agent models".into()))
    }

    /// Persists the event, assigning its `seq`, `id` and `ts`.
    async fn append_event(&self, kind: EventKind) -> Result<Event, CoreError>;
    /// Events with `seq > after`, oldest first.
    async fn events_since(&self, after: u64, limit: u32) -> Result<Vec<Event>, CoreError>;
    /// The highest `seq` stored, or 0.
    async fn latest_event_seq(&self) -> Result<u64, CoreError>;
    /// Events about `agent` (see [`EventKind::agent_id`]) with `seq > after`, oldest first.
    /// The default scans every event; stores should override it with an indexed lookup.
    async fn agent_events(
        &self,
        agent: AgentId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<Event>, CoreError> {
        let mut found = Vec::new();
        let mut last = after;
        loop {
            let page = self.events_since(last, 500).await?;
            let Some(end) = page.last().map(|e| e.seq) else {
                return Ok(found);
            };
            last = end;
            for event in page {
                if event.kind.agent_id() == Some(agent) {
                    found.push(event);
                    if found.len() >= limit as usize {
                        return Ok(found);
                    }
                }
            }
        }
    }

    async fn insert_run(&self, run: &Run) -> Result<(), CoreError>;
    async fn get_run(&self, id: RunId) -> Result<Run, CoreError>;
    /// Newest first.
    async fn list_runs(&self) -> Result<Vec<Run>, CoreError>;
    /// Replaces the stored run with `run` (matched by id).
    async fn update_run(&self, run: &Run) -> Result<(), CoreError>;

    async fn insert_task(&self, task: &Task) -> Result<(), CoreError>;
    async fn get_task(&self, id: TaskId) -> Result<Task, CoreError>;
    /// In creation order.
    async fn list_tasks(&self, run_id: RunId) -> Result<Vec<Task>, CoreError>;
    async fn update_task(&self, task: &Task) -> Result<(), CoreError>;

    /// Fails with [`CoreError::Conflict`] if the id is taken.
    async fn insert_account(&self, account: &Account) -> Result<(), CoreError>;
    async fn get_account(&self, id: &str) -> Result<Account, CoreError>;
    async fn list_accounts(&self) -> Result<Vec<Account>, CoreError>;
    async fn delete_account(&self, id: &str) -> Result<(), CoreError>;
}
