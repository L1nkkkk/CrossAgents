//! Reminders and watches an agent sets for itself (docs/tasks/H1.md §5.3). The hub never
//! nudges anyone on its own: an agent asks to be woken, and what wakes it goes into its
//! notification digest.
//!
//! A rule is bounded so that it cannot wake its owner forever (docs/research/multica.md §2.1,
//! §2.14): a chain of triggers with nobody in between, a burst within an hour, or a long run of
//! failures pauses it, and a waiting rule runs out after a week. The counting lives in the
//! orchestrator (`crossagents_orchestrator::watch` and `::breaker`); these are its records.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, TaskId};
use crate::model::Health;

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
    /// The time asked for was beyond the week a wait may last and was cut to it: the wake says
    /// that the thing waited for did not happen.
    #[serde(default)]
    pub wait_expired: bool,
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
    /// When the wait is over: absent means `created_at` plus `WAIT_MAX_DAYS`. At that moment the
    /// owner is woken once ("the thing you waited for did not happen") and the watch closes.
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    /// Triggers in a row with nobody in between. A person speaking to the owner clears it.
    #[serde(default)]
    pub chain: u32,
    /// What this rule has done lately, and whether it paused itself.
    #[serde(default)]
    pub health: Health,
}

impl Watch {
    pub fn wants(&self, what: WatchOn) -> bool {
        self.on.contains(&WatchOn::Any) || self.on.contains(&what)
    }
}
