//! The unified agent interface (docs/DESIGN.md §6.1). Plugins translate each vendor's
//! native protocol into these commands and events; orchestration only sees this.

use std::collections::BTreeMap;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::error::CoreError;
use crate::model::Agent;

/// What a plugin's agents can do (docs/DESIGN.md §6.2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Capabilities {
    pub streaming: bool,
    pub interrupt: bool,
    pub model_switch: bool,
    /// Collaboration tools can be mounted as MCP servers.
    pub mcp: bool,
    pub usage_report: bool,
}

/// How to answer an agent's permission requests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPolicy {
    /// Approve everything; isolation comes from the workspace (docs/DESIGN.md §8).
    #[default]
    Bypass,
    /// Reject everything that asks.
    Deny,
}

/// An MCP server to mount into the agent's session (stdio transport).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerSpec {
    pub name: String,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

/// Everything a plugin needs to start a session.
pub struct SessionSpec {
    pub agent: Agent,
    pub mcp_servers: Vec<McpServerSpec>,
    pub permission: PermissionPolicy,
    /// Launch command override (`[plugins.<name>] command` or the agent's `config.command`).
    pub command: Option<Vec<String>>,
    /// Extra instructions to prepend to the first prompt (role, project memory, ...).
    pub instructions: Option<String>,
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
    /// The agent process failed during the turn.
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

/// Something that happened inside a running session.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
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
    Usage {
        /// Context tokens in use and the window size, when the agent reports them.
        used: u64,
        size: u64,
        cost_usd: Option<f64>,
    },
    /// A non-fatal problem worth surfacing (e.g. the requested model was not found).
    Warning {
        message: String,
    },
    /// The agent process ended.
    Exited {
        message: String,
    },
}

/// A live session with one agent.
#[async_trait]
pub trait AgentSession: Send + Sync {
    /// Runs one turn to completion. Output arrives as [`SessionEvent`]s meanwhile.
    async fn prompt(&self, text: &str) -> Result<StopReason, CoreError>;
    /// Asks the agent to stop the current turn; `prompt` then returns `Cancelled`.
    async fn cancel(&self) -> Result<(), CoreError>;
    /// Ends the session and the agent process.
    async fn shutdown(&self);
}

/// One vendor's agents (docs/DESIGN.md §6).
#[async_trait]
pub trait AgentPlugin: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> Capabilities;
    /// Models this plugin knows about, for display and validation hints.
    fn models(&self) -> Vec<String>;
    async fn start(&self, spec: SessionSpec) -> Result<Box<dyn AgentSession>, CoreError>;
}
