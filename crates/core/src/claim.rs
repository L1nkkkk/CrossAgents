//! Claims (docs/tasks/S1.md §3.1): an agent works in one place at a time. Leading a run,
//! working on a task and reviewing one each hold the agent; the hold is a row the store
//! inserts atomically, so two runs that each pass their own lock still cannot both take it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, RunId, TaskId};

/// What the agent is held for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ClaimRole {
    Lead,
    Worker,
    Reviewer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AgentClaim {
    pub agent_id: AgentId,
    pub run_id: RunId,
    /// The task worked on or reviewed; `None` for a lead.
    #[serde(default)]
    pub task_id: Option<TaskId>,
    pub role: ClaimRole,
    /// An execution session started for this (archived when the claim ends), rather than
    /// the agent itself (sent back to its workspace).
    #[serde(default)]
    pub fresh: bool,
    pub since: DateTime<Utc>,
}

impl AgentClaim {
    pub fn new(
        agent_id: AgentId,
        run_id: RunId,
        task_id: Option<TaskId>,
        role: ClaimRole,
        fresh: bool,
    ) -> Self {
        Self {
            agent_id,
            run_id,
            task_id,
            role,
            fresh,
            since: Utc::now(),
        }
    }
}
