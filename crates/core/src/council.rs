//! Multi-model Council: consult multiple models or teammates independently on the same
//! question (with an optional second cross-critique round) and store the result so agents
//! and people can compare perspectives side by side.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::AgentId;

/// How a council runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CouncilMode {
    /// One parallel round: every member answers independently without seeing the others.
    #[default]
    Independent,
    /// Two parallel rounds: Round 1 answers independently, Round 2 lets every member review
    /// anonymized Round 1 answers from the others to critique blind spots and refine its stance.
    Debate,
}

/// Lifecycle state of a council run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CouncilStatus {
    #[default]
    Running,
    Completed,
    Failed,
}

/// One seat requested when starting a council.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CouncilMemberSpec {
    /// Human-readable seat label (defaults to the teammate name or model id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Plugin name (e.g. `claude-native`, `codex-native`). Optional when `agent` is given or
    /// when `model` uniquely matches an installed plugin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    /// Exact model identifier to run for this seat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional existing agent/teammate name or id whose plugin, model, and profile are cloned
    /// into a clean council session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Optional reasoning effort override (e.g. `high`, `medium`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Optional framing / perspective for this seat (e.g. "Focus on security risks").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perspective: Option<String>,
}

/// One seat's response recorded inside a [`CouncilRecord`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CouncilMemberResponse {
    /// Anonymized seat identifier used during debate cross-critique ("A", "B", "C", ...).
    pub seat: String,
    pub label: String,
    pub plugin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentId>,
    /// `"running"` | `"ok"` | `"error"`
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round1: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round2: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Persisted record of a multi-model council consultation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CouncilRecord {
    pub id: String,
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    pub mode: CouncilMode,
    pub status: CouncilStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub members: Vec<CouncilMemberResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
