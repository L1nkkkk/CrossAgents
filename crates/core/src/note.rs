//! Shared notes (docs/tasks/H1.md §5.5, §8): what a team agreed on, written down where every
//! teammate can read it. `run` notes belong to one run and are left behind with it; `team`
//! notes last across runs. Every write is also an event, so history is never lost.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, RunId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NoteScope {
    /// This run only.
    #[default]
    Run,
    /// Everyone, across runs.
    Team,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Note {
    pub scope: NoteScope,
    /// For run notes.
    #[serde(default)]
    pub run_id: Option<RunId>,
    pub key: String,
    pub text: String,
    pub author: AgentId,
    pub updated_at: DateTime<Utc>,
}

/// The agent plugin of agents that run outside the hub and connect through MCP on their own.
pub const EXTERNAL_PLUGIN: &str = "external";
