//! Drivers: how the hub talks to agents (docs/DESIGN.md §9).
//!
//! A [`Driver`] runs one Profile's sessions through one vendor interface (Codex
//! app-server, an ACP agent, ...). It reports what it can do in [`Capabilities`] and
//! translates the vendor's events into [`SessionEvent`]s; nothing above this layer sees a
//! vendor protocol. Callers check capabilities before using optional operations; a driver
//! answers an operation it lacks with [`CoreError::Unsupported`] rather than faking it.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::account::{Credits, RateWindow};
use crate::error::CoreError;
use crate::model::Agent;

/// Which driver runs a Profile (`driver = "..."` in its configuration).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "kebab-case")]
pub enum DriverKind {
    /// `codex app-server`: one process per Profile, one thread per session.
    CodexNative,
    /// Claude Agent SDK sidecar or `claude` stream-json (not implemented yet).
    ClaudeNative,
    /// Any ACP agent; the fallback for everything else. The default, so configurations
    /// written before drivers existed keep their behaviour.
    #[default]
    AcpGeneric,
}

impl DriverKind {
    pub const ALL: [DriverKind; 3] = [Self::CodexNative, Self::ClaudeNative, Self::AcpGeneric];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CodexNative => "codex-native",
            Self::ClaudeNative => "claude-native",
            Self::AcpGeneric => "acp-generic",
        }
    }
}

impl fmt::Display for DriverKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DriverKind {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| {
                CoreError::Unsupported(format!(
                    "driver {s:?} (known: codex-native, claude-native, acp-generic)"
                ))
            })
    }
}

/// What a driver can do (docs/DESIGN.md §9.2). Clients show an operation only when its
/// flag is set; a missing capability degrades the feature instead of failing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Capabilities {
    /// Output arrives while the turn runs.
    pub streaming: bool,
    /// [`DriverSession::interrupt`] stops a running turn.
    pub interrupt: bool,
    /// Collaboration tools can be mounted as MCP servers.
    pub mcp: bool,
    /// One agent process serves several sessions.
    pub multi_session: bool,
    /// [`DriverSession::suspend`] and [`Driver::resume`]: the session outlives its process.
    pub resume: bool,
    /// [`DriverSession::fork`].
    pub fork: bool,
    /// [`DriverSession::rollback`] of the conversation history.
    pub rollback: bool,
    /// `rollback` can also undo the files those turns changed.
    pub revert_files: bool,
    /// [`DriverSession::set_model`] between turns (model and reasoning effort).
    pub set_model: bool,
    /// [`SessionEventKind::TurnDiff`]: the files a turn changed, as a diff.
    pub structured_diff: bool,
    /// [`SessionEventKind::Usage`].
    pub usage: bool,
    /// [`SessionEventKind::RateLimits`].
    pub rate_limits: bool,
    /// Permission requests go through [`SessionSpec::permissions`] when one is given.
    pub permission_callback: bool,
    /// [`DriverSession::compact`].
    pub compaction: bool,
}

/// How to answer permission requests when no [`PermissionHandler`] is given.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPolicy {
    /// Approve everything; isolation comes from the workspace (docs/DESIGN.md §4.4).
    #[default]
    Bypass,
    /// Reject everything that asks.
    Deny,
}

impl PermissionPolicy {
    pub fn decide(self) -> PermissionDecision {
        match self {
            Self::Bypass => PermissionDecision::Allow,
            Self::Deny => PermissionDecision::Deny,
        }
    }
}

/// An agent asking before it acts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequest {
    /// The vendor's id for the request.
    pub id: String,
    pub turn_id: Option<String>,
    /// `command`, `file_change`, `permissions`, `mcp_elicitation`, or the vendor's word.
    pub kind: String,
    /// One line for humans, e.g. the command to run.
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    /// Allow, and do not ask again for the same thing in this session.
    AllowForSession,
    Deny,
    /// Deny and stop the turn.
    Cancel,
}

/// Decides permission requests on the hub's behalf (rules, the Planner, a human).
#[async_trait]
pub trait PermissionHandler: Send + Sync {
    async fn decide(&self, request: &PermissionRequest) -> PermissionDecision;
}

/// An MCP server to mount into the session (stdio transport).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerSpec {
    pub name: String,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

/// Everything a driver needs to open, resume or fork a session.
pub struct SessionSpec {
    /// The session's record: model, working directory, driver settings in `config`.
    pub agent: Agent,
    pub mcp_servers: Vec<McpServerSpec>,
    pub permission: PermissionPolicy,
    /// Asked about permission requests instead of `permission`, by drivers with
    /// `permission_callback`.
    pub permissions: Option<Arc<dyn PermissionHandler>>,
    /// Launch command override (`[plugins.<name>] command` or the agent's `config.command`).
    pub command: Option<Vec<String>>,
    /// Opening instructions: who the session is and how it works with others.
    pub instructions: Option<String>,
    /// Extra environment for the agent process (e.g. the bound account's `CODEX_HOME`).
    pub env: Vec<(String, String)>,
    /// Where the session reports what happens.
    pub events: mpsc::UnboundedSender<SessionEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Text,
    Thought,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
    /// The turn failed (the agent reported an error, or its process died).
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// How a turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnOutcome {
    pub stop_reason: StopReason,
    /// The vendor's id for the turn, for [`DriverSession::fork`] and
    /// [`DriverSession::rollback`]. `None` when the driver has no turn ids.
    pub turn_id: Option<String>,
}

/// Cumulative token counts of a session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub input: u64,
    /// Part of `input` served from the prompt cache.
    pub cached_input: u64,
    pub output: u64,
    /// Part of `output` spent on reasoning.
    pub reasoning_output: u64,
    pub total: u64,
}

/// The account's quota as the agent last saw it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RateLimits {
    pub windows: Vec<RateWindow>,
    pub credits: Option<Credits>,
    pub plan: Option<String>,
    /// Why the account is blocked, if it is (vendor wording).
    pub reached: Option<String>,
}

/// Something that happened inside a session, in the hub's own terms.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionEvent {
    pub kind: SessionEventKind,
    /// The vendor message this was translated from, for consumers that want detail the
    /// unified model leaves out. Never interpreted by the hub.
    pub raw: Option<serde_json::Value>,
}

impl SessionEvent {
    pub fn with_raw(kind: SessionEventKind, raw: serde_json::Value) -> Self {
        Self {
            kind,
            raw: Some(raw),
        }
    }
}

impl From<SessionEventKind> for SessionEvent {
    fn from(kind: SessionEventKind) -> Self {
        Self { kind, raw: None }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionEventKind {
    /// The vendor's id for this session: an ACP session id, a Codex thread id. Joins
    /// CrossAgents sessions to vendor usage logs, and is what [`Driver::resume`] takes.
    Started {
        session_id: String,
    },
    TurnStarted {
        turn_id: Option<String>,
    },
    Output {
        stream: OutputStream,
        text: String,
    },
    ToolCall {
        tool_call_id: String,
        title: String,
        kind: Option<String>,
        status: ToolStatus,
    },
    ToolCallUpdated {
        tool_call_id: String,
        title: Option<String>,
        status: Option<ToolStatus>,
    },
    /// Everything the turn changed so far, as a unified diff. Each event replaces the
    /// previous one for the same turn.
    TurnDiff {
        turn_id: Option<String>,
        diff: String,
    },
    Usage {
        /// Context tokens in use and the window size (0 when unknown).
        used: u64,
        size: u64,
        cost_usd: Option<f64>,
        /// Cumulative counts, when the agent reports them.
        tokens: Option<TokenUsage>,
    },
    RateLimits {
        limits: RateLimits,
    },
    /// The agent asked before acting; `decision` is what the driver answered.
    PermissionRequested {
        request: PermissionRequest,
        decision: PermissionDecision,
    },
    TurnCompleted {
        turn_id: Option<String>,
        stop_reason: StopReason,
    },
    /// A non-fatal problem worth surfacing (e.g. the requested model was not found).
    Warning {
        message: String,
    },
    /// The turn or session failed; the session may still take new turns.
    Error {
        message: String,
    },
    /// The agent process ended. The session is gone (until resumed, if it can be).
    Exited {
        message: String,
    },
}

/// A live session. Turns run one at a time: callers wait for `prompt` to return before
/// sending the next one.
#[async_trait]
pub trait DriverSession: Send + Sync {
    /// The vendor's session id (see [`SessionEventKind::Started`]).
    fn session_id(&self) -> &str;

    /// Runs one turn to completion. Output arrives as [`SessionEvent`]s meanwhile.
    async fn prompt(&self, text: &str) -> Result<TurnOutcome, CoreError>;

    /// Stops the running turn; `prompt` then returns [`StopReason::Cancelled`]. Does
    /// nothing when no turn runs.
    async fn interrupt(&self) -> Result<(), CoreError>;

    /// Releases the session's resources while keeping it resumable with
    /// [`Driver::resume`]. Fails while a turn runs.
    async fn suspend(&self) -> Result<(), CoreError> {
        Err(unsupported("suspend"))
    }

    /// A new session with this one's history up to and including `at_turn` (all of it
    /// when `None`). `spec` describes the new session (name, directory, events).
    async fn fork(
        &self,
        _at_turn: Option<&str>,
        _spec: SessionSpec,
    ) -> Result<Box<dyn DriverSession>, CoreError> {
        Err(unsupported("fork"))
    }

    /// Drops `before_turn` and every later turn from the history. With `revert_files`,
    /// also undoes the files they changed (needs [`Capabilities::revert_files`]).
    async fn rollback(&self, _before_turn: &str, _revert_files: bool) -> Result<(), CoreError> {
        Err(unsupported("rollback"))
    }

    /// Model and reasoning effort for the following turns; `None` keeps the current one.
    async fn set_model(
        &self,
        _model: Option<&str>,
        _effort: Option<&str>,
    ) -> Result<(), CoreError> {
        Err(unsupported("set_model"))
    }

    /// Summarizes the history to free context.
    async fn compact(&self) -> Result<(), CoreError> {
        Err(unsupported("compact"))
    }

    /// Ends the session for good, stopping a running turn first.
    async fn shutdown(&self);
}

/// One Profile's way of running sessions (docs/DESIGN.md §9).
#[async_trait]
pub trait Driver: Send + Sync {
    /// The Profile's name, e.g. `codex`.
    fn name(&self) -> &str;
    fn kind(&self) -> DriverKind;
    fn capabilities(&self) -> Capabilities;
    /// Models this Profile knows about, for display and validation hints.
    fn models(&self) -> Vec<String>;
    /// Opens a new session.
    async fn open(&self, spec: SessionSpec) -> Result<Box<dyn DriverSession>, CoreError>;
    /// Reopens a suspended session by the id it reported in
    /// [`SessionEventKind::Started`].
    async fn resume(
        &self,
        _session_id: &str,
        _spec: SessionSpec,
    ) -> Result<Box<dyn DriverSession>, CoreError> {
        Err(unsupported("resume"))
    }
}

fn unsupported(what: &str) -> CoreError {
    CoreError::Unsupported(format!("{what} is not supported by this driver"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_kinds_round_trip() {
        for kind in DriverKind::ALL {
            assert_eq!(kind.as_str().parse::<DriverKind>().unwrap(), kind);
            let json = serde_json::to_value(kind).unwrap();
            assert_eq!(json, kind.as_str());
            assert_eq!(serde_json::from_value::<DriverKind>(json).unwrap(), kind);
        }
        assert_eq!(DriverKind::default(), DriverKind::AcpGeneric);
        assert!("codex".parse::<DriverKind>().is_err());
    }
}
