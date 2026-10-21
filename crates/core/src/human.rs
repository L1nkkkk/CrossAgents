//! What agents put in front of the person (docs/tasks/H1.md §5.4): questions to answer and
//! notes to read. They show in the GUI's 需要你 strip.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, RunId, TaskId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum HumanItemKind {
    /// Wants an answer.
    Question,
    /// For reading only.
    Notice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum HumanItemStatus {
    Open,
    Answered,
    /// Nobody answered in time; the default answer was used.
    Defaulted,
    /// Read (a notice) or no longer relevant.
    Dismissed,
}

/// A hub action an answer triggers, for questions the hub itself asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum HumanAction {
    /// The run hit its budget: the first option raises it by half and resumes, the second
    /// cancels the run.
    Budget,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HumanItem {
    pub id: String,
    pub kind: HumanItemKind,
    /// The agent asking or telling.
    pub from: AgentId,
    #[serde(default)]
    pub run_id: Option<RunId>,
    #[serde(default)]
    pub task_id: Option<TaskId>,
    pub text: String,
    /// Suggested answers; the person may also write their own.
    #[serde(default)]
    pub options: Vec<String>,
    /// Used when nobody answers by `timeout_at`.
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub timeout_at: Option<DateTime<Utc>>,
    pub status: HumanItemStatus,
    #[serde(default)]
    pub answer: Option<String>,
    #[serde(default)]
    pub answered_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub action: Option<HumanAction>,
    pub created_at: DateTime<Utc>,
}
