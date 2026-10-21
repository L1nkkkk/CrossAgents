//! Reminders and watches an agent sets for itself (docs/tasks/H1.md §5.3). The hub never
//! nudges anyone on its own: an agent asks to be woken, and what wakes it goes into its
//! notification digest.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, TaskId};

/// At most this many reminders, and as many watches, per agent.
pub const MAX_PER_AGENT: usize = 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Reminder {
    pub id: String,
    pub owner: AgentId,
    pub due_at: DateTime<Utc>,
    pub note: String,
    #[serde(default)]
    pub task: Option<TaskId>,
    /// Only fire if `task` has not ended by then.
    #[serde(default)]
    pub only_if_open: bool,
    pub created_at: DateTime<Utc>,
}

/// What a watch follows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WatchTarget {
    Task { task_id: TaskId },
    Agent { agent_id: AgentId },
}

/// A change a watch can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum WatchOn {
    /// A task finished (merged, passed) or an agent ended a turn normally.
    Done,
    /// A task failed or was cancelled; an agent's turn or process failed.
    Failed,
    /// A task's assignee asked a question.
    Asked,
    /// A task's assignee ended a turn without reporting.
    Stalled,
    /// An agent has nothing left to do.
    Idle,
    /// Any status change.
    Any,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Watch {
    pub id: String,
    pub owner: AgentId,
    pub target: WatchTarget,
    pub on: Vec<WatchOn>,
    pub created_at: DateTime<Utc>,
}

impl Watch {
    pub fn wants(&self, what: WatchOn) -> bool {
        self.on.contains(&WatchOn::Any) || self.on.contains(&what)
    }
}
