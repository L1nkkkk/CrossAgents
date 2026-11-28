//! Session-scoped background jobs managed directly by the hub.
//!
//! A job runs independently of the agent CLI process so image turns, model switches,
//! or CLI restarts never kill long-running builds/tests/releases. When a job exits,
//! the hub notifies the owning session so the agent wakes up automatically with the
//! exit code, duration, log path, and tail output.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::AgentId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SessionJobStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl SessionJobStatus {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SessionJob {
    pub id: String,
    pub agent_id: AgentId,
    /// Human-readable label (defaults to the first line of `command`).
    pub label: String,
    /// Shell command executed by the hub.
    pub command: String,
    /// Working directory where the command ran.
    pub cwd: String,
    pub status: SessionJobStatus,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub exit_code: Option<i32>,
    /// Full combined stdout/stderr log file on disk (`<hub_home>/jobs/<id>.log`).
    pub log_path: String,
    /// Rolling tail of the latest output lines (capped for UI & wakeup notices).
    #[serde(default)]
    pub tail: String,
}
