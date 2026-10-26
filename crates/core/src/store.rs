//! Persistence abstraction. See docs/DESIGN.md §5.

use async_trait::async_trait;

use crate::account::Account;
use crate::error::CoreError;
use crate::event::{Event, EventKind};
use crate::human::HumanItem;
use crate::ids::{AgentId, MessageId, RunId, TaskId};
use crate::model::{Agent, AgentProfile, AgentStatus, Run, Task};
use crate::note::{Note, NoteScope};
use crate::notify::{Reminder, Watch};

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
    /// Renames the agent; fails with [`CoreError::Conflict`] if another has the name
    /// (ignoring case).
    async fn rename_agent(
        &self,
        id: AgentId,
        name: &str,
        source: crate::NameSource,
    ) -> Result<(), CoreError> {
        let _ = (id, name, source);
        Err(CoreError::Unsupported("renaming agents".into()))
    }
    /// Sets or clears a conversation's title.
    async fn update_agent_topic(&self, id: AgentId, topic: Option<&str>) -> Result<(), CoreError> {
        let _ = (id, topic);
        Err(CoreError::Unsupported("conversation titles".into()))
    }
    /// Archives the conversation (`Some`) or restores it (`None`).
    async fn set_agent_archived(
        &self,
        id: AgentId,
        at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<(), CoreError> {
        let _ = (id, at);
        Err(CoreError::Unsupported("archiving agents".into()))
    }
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

    /// Moves the agent to another working directory. Only for an agent whose session has
    /// never started: a vendor session belongs to the directory it began in.
    async fn update_agent_cwd(&self, id: AgentId, cwd: &std::path::Path) -> Result<(), CoreError> {
        let _ = (id, cwd);
        Err(CoreError::Unsupported("moving agents".into()))
    }

    /// Replaces the agent's plugin-private config (e.g. its reasoning `effort`).
    async fn update_agent_config(
        &self,
        id: AgentId,
        config: &serde_json::Value,
    ) -> Result<(), CoreError> {
        let _ = (id, config);
        Err(CoreError::Unsupported("updating agent config".into()))
    }

    /// Persists the event, assigning its `seq`, `id` and `ts`.
    async fn append_event(&self, kind: EventKind) -> Result<Event, CoreError>;
    /// Events with `seq > after`, oldest first.
    async fn events_since(&self, after: u64, limit: u32) -> Result<Vec<Event>, CoreError>;
    /// The highest `seq` stored, or 0.
    async fn latest_event_seq(&self) -> Result<u64, CoreError>;
    /// The vendor session id the agent's last session reported (`agent.session_started`).
    /// The default scans the agent's events; stores should override it.
    async fn last_session_id(&self, agent: AgentId) -> Result<Option<String>, CoreError> {
        let mut last = None;
        let mut after = 0;
        loop {
            let page = self.agent_events(agent, after, 500).await?;
            let Some(end) = page.last().map(|e| e.seq) else {
                return Ok(last);
            };
            after = end;
            for event in page {
                if let EventKind::AgentSessionStarted { session_id, .. } = event.kind {
                    last = Some(session_id);
                }
            }
        }
    }

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
    /// Deletes the run and everything the store keeps for it: its tasks, the human items and
    /// notes that name it (docs/tasks/R1.md §3.3). Events stay. Fails with
    /// [`CoreError::NotFound`] when there is no such run.
    async fn delete_run(&self, id: RunId) -> Result<(), CoreError> {
        let _ = id;
        Err(CoreError::Unsupported("deleting runs".into()))
    }

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

    async fn insert_reminder(&self, reminder: &Reminder) -> Result<(), CoreError> {
        let _ = reminder;
        Err(CoreError::Unsupported("reminders".into()))
    }
    /// `owner`'s reminders, or everyone's; soonest first.
    async fn list_reminders(&self, owner: Option<AgentId>) -> Result<Vec<Reminder>, CoreError> {
        let _ = owner;
        Ok(Vec::new())
    }
    /// Fails with [`CoreError::NotFound`] if there is no such reminder.
    async fn delete_reminder(&self, id: &str) -> Result<(), CoreError> {
        Err(CoreError::NotFound {
            kind: "reminder",
            id: id.to_owned(),
        })
    }
    async fn insert_watch(&self, watch: &Watch) -> Result<(), CoreError> {
        let _ = watch;
        Err(CoreError::Unsupported("watches".into()))
    }
    async fn list_watches(&self, owner: Option<AgentId>) -> Result<Vec<Watch>, CoreError> {
        let _ = owner;
        Ok(Vec::new())
    }
    async fn delete_watch(&self, id: &str) -> Result<(), CoreError> {
        Err(CoreError::NotFound {
            kind: "watch",
            id: id.to_owned(),
        })
    }
    /// Inserts or replaces a note (by scope, run and key).
    async fn put_note(&self, note: &Note) -> Result<(), CoreError> {
        let _ = note;
        Err(CoreError::Unsupported("notes".into()))
    }
    async fn get_note(
        &self,
        scope: NoteScope,
        run_id: Option<RunId>,
        key: &str,
    ) -> Result<Option<Note>, CoreError> {
        let _ = (scope, run_id, key);
        Ok(None)
    }
    /// Notes of a scope (and run), with keys starting with `prefix`, by key.
    async fn list_notes(
        &self,
        scope: NoteScope,
        run_id: Option<RunId>,
        prefix: &str,
    ) -> Result<Vec<Note>, CoreError> {
        let _ = (scope, run_id, prefix);
        Ok(Vec::new())
    }
    async fn delete_note(
        &self,
        scope: NoteScope,
        run_id: Option<RunId>,
        key: &str,
    ) -> Result<(), CoreError> {
        let _ = (scope, run_id, key);
        Ok(())
    }
    /// Inserts or replaces (by id) an item for the person.
    async fn put_human_item(&self, item: &HumanItem) -> Result<(), CoreError> {
        let _ = item;
        Err(CoreError::Unsupported("human items".into()))
    }
    async fn get_human_item(&self, id: &str) -> Result<HumanItem, CoreError> {
        Err(CoreError::NotFound {
            kind: "question",
            id: id.to_owned(),
        })
    }
    /// Newest first; only open ones with `open_only`.
    async fn list_human_items(&self, open_only: bool) -> Result<Vec<HumanItem>, CoreError> {
        let _ = open_only;
        Ok(Vec::new())
    }
    /// The thread and text of a message delivered earlier, if it is known.
    async fn find_message(
        &self,
        id: MessageId,
    ) -> Result<Option<(Option<MessageId>, String)>, CoreError> {
        let _ = id;
        Ok(None)
    }
}
