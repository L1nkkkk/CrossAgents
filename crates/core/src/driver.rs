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

use crate::account::{Credits, LimitGroup, RateWindow, ResetCredit};
use crate::attachment::AttachmentSupport;
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
    /// `claude -p` with its stream-json control protocol: one process per session.
    ClaudeNative,
    /// WorkBuddy's CodeBuddy CLI, which speaks the same stream-json control protocol as
    /// Claude Code: one process per session.
    CodebuddyNative,
    /// With's `with-cli chat -o stream-json`: one process per turn, the conversation kept
    /// by its session id.
    WithNative,
    /// Cursor's `agent -p`: one process per turn, with a persistent chat id.
    CursorNative,
    /// Any ACP agent; the fallback for everything else. The default, so configurations
    /// written before drivers existed keep their behaviour.
    #[default]
    AcpGeneric,
}

impl DriverKind {
    pub const ALL: [DriverKind; 6] = [
        Self::CodexNative,
        Self::ClaudeNative,
        Self::CodebuddyNative,
        Self::WithNative,
        Self::CursorNative,
        Self::AcpGeneric,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CodexNative => "codex-native",
            Self::ClaudeNative => "claude-native",
            Self::CodebuddyNative => "codebuddy-native",
            Self::WithNative => "with-native",
            Self::CursorNative => "cursor-native",
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
                    "driver {s:?} (known: codex-native, claude-native, codebuddy-native, with-native, cursor-native, acp-generic)"
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
    /// [`DriverSession::steer`] gives a running turn another message.
    #[serde(default)]
    pub steer: bool,
    /// Collaboration tools can be mounted as MCP servers.
    pub mcp: bool,
    /// One agent process serves several sessions.
    pub multi_session: bool,
    /// [`DriverSession::suspend`] and [`Driver::resume`]: the session outlives its process.
    pub resume: bool,
    /// A suspended session resumes in another working directory with its conversation:
    /// the hub can move the agent into a task's worktree and back (docs/tasks/S1.md §3.1).
    pub move_cwd: bool,
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
    /// Which kinds of file the agent takes with a message (docs/TODO.md §7). `None` while
    /// it has not said: ACP agents declare this when their session starts, so a client
    /// allows the entries it knows nothing about and lets the hub decide.
    pub attachments: Option<AttachmentSupport>,
}

/// How to answer permission requests when no [`PermissionHandler`] is given.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum PermissionPolicy {
    /// Approve everything; isolation comes from the workspace (docs/DESIGN.md §4.4).
    #[default]
    Bypass,
    /// Reject everything that asks.
    Deny,
    /// Ask about everything, and let a [`PermissionHandler`] pass what is safe and put the
    /// dangerous to the person (docs/tasks/DOGFOOD.md §7). Without a handler: allow.
    Guard,
}

impl PermissionPolicy {
    pub fn decide(self) -> PermissionDecision {
        match self {
            Self::Bypass | Self::Guard => PermissionDecision::Allow,
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
    /// The turn's outcome is unknown: its process went away with the hub that ran it (a hub
    /// restart that could not take the process over). Never retried by itself.
    Interrupted,
}

/// Why an agent process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ExitCause {
    /// It exited by itself with code 0 (e.g. its input was closed).
    Exited,
    /// It exited by itself with another code.
    Crashed,
    /// CrossAgents stopped it (stop, archive, a failed start).
    Killed,
    /// Its runtime host went away without saying how it ended (killed from outside, say).
    Lost,
}

impl ExitCause {
    /// `"exited"`, `"crashed (exit code 3)"`, ...: for messages.
    pub fn describe(self, code: Option<i32>) -> String {
        let word = match self {
            Self::Exited => "exited",
            Self::Crashed => "crashed",
            Self::Killed => "was stopped",
            Self::Lost => "was lost",
        };
        match code {
            Some(code) if self != Self::Killed => format!("{word} (exit code {code})"),
            _ => word.to_owned(),
        }
    }
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

/// One share of the context window, as the agent accounts for it (Claude Code's `/context`):
/// the system prompt, tools, MCP tools, skills, messages, the buffer kept for compaction,
/// free space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ContextPart {
    /// Stable key (`system_prompt`, `messages`, `free_space`…).
    pub id: String,
    /// As the agent names it.
    pub name: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub tokens: u64,
    /// `used` (in the window), `deferred` (loaded only when needed), `buffer` (kept free
    /// for compaction) or `free`.
    pub kind: String,
}

/// Something the agent's own CLI brings besides its tools: a slash command, or a skill.
/// What a GUI offers when the session's driver reports them (`system/init` in Claude Code's
/// stream-json).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AgentCommand {
    /// As the CLI names it, without the leading `/`.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub description: Option<String>,
    pub kind: AgentCommandKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum AgentCommandKind {
    /// A slash command (`/compact`, `/review`).
    Command,
    /// A skill the CLI can load.
    Skill,
}

/// Cumulative token counts of a session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TokenUsage {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub input: u64,
    /// Part of `input` served from the prompt cache.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub cached_input: u64,
    /// Part of `input` written to the prompt cache.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub cache_write_input: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub output: u64,
    /// Part of `output` spent on reasoning.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub reasoning_output: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total: u64,
    /// Cumulative model requests, if known.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub requests: Option<u64>,
}

#[cfg(test)]
#[test]
fn old_token_usage_defaults_new_fields() {
    let usage: TokenUsage = serde_json::from_str(
        r#"{"input":10,"cached_input":2,"output":3,"reasoning_output":1,"total":13}"#,
    )
    .unwrap();
    assert_eq!(usage.cache_write_input, 0);
    assert_eq!(usage.requests, None);
}

/// What a tool call did, as far as the agent tells: the command and its result, the files
/// it changed, or an MCP call's arguments and result. Fields the agent does not report
/// stay empty.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ToolDetail {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub duration_ms: Option<u64>,
    /// The command's output (the tail, if it was long).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub output: Option<String>,
    #[serde(default)]
    pub changes: Vec<FileChange>,
    /// Arguments and result of an MCP or other tool call, as JSON text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub error: Option<String>,
}

/// One file a tool call changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct FileChange {
    pub path: String,
    /// `add`, `delete`, `update` or the vendor's word.
    pub kind: String,
    /// Unified diff of the change; empty when unknown.
    pub diff: String,
}

/// The account's quota as the agent last saw it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RateLimits {
    pub windows: Vec<RateWindow>,
    pub credits: Option<Credits>,
    pub plan: Option<String>,
    /// Why the account is blocked, if it is (vendor wording).
    pub reached: Option<String>,
    /// The separate limits this report covers (a full read lists every one; a per-call
    /// update only the one in use).
    #[serde(default)]
    pub by_limit: Vec<LimitGroup>,
    /// Reset credits, when the report includes them (full reads do).
    #[serde(default)]
    pub reset_credits: Option<Vec<ResetCredit>>,
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
    /// More of a running tool call's output (e.g. a command's stdout).
    ToolOutput {
        tool_call_id: String,
        text: String,
    },
    /// What a tool call did; sent when the agent reports it, usually as the call ends.
    ToolDetail {
        tool_call_id: String,
        detail: ToolDetail,
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
    /// What fills the context window, by kind, when the agent can tell.
    ContextBreakdown {
        parts: Vec<ContextPart>,
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
    /// The model has read a message put into the running turn with
    /// [`DriverSession::steer`].
    SteerRead {
        message_id: crate::MessageId,
    },
    /// The agent compacted its own context (Claude Code's `compact_boundary`, Codex's
    /// `contextCompaction`): what was in the context before is no longer there. The hub
    /// resets what it keeps per context on this (the stream rules). A compaction the hub
    /// asked for ([`DriverSession::compact`]) resets it itself; a driver that reports that
    /// one too only repeats a harmless reset.
    Compacted,
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
        /// How it ended, when the driver knows.
        cause: Option<ExitCause>,
        code: Option<i32>,
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

    /// Like [`Self::prompt`], with the message's files. Drivers that cannot pass files on
    /// natively get them written into the text (inline or by path), never dropped.
    async fn prompt_with(&self, prompt: &crate::Prompt) -> Result<TurnOutcome, CoreError> {
        if prompt.attachments.is_empty() {
            self.prompt(&prompt.text).await
        } else {
            self.prompt(&prompt.with_references()).await
        }
    }

    /// What this session's agent takes as files, when it declared it at start (ACP agents
    /// do). `None`: whatever its driver's [`Capabilities::attachments`] says.
    fn attachment_support(&self) -> Option<AttachmentSupport> {
        None
    }

    /// The slash commands and skills the agent's own CLI reports for this session (Claude
    /// Code's `system/init`), names without the leading `/`. `None` when the driver does not
    /// report them, or the session has not said yet.
    fn commands(&self) -> Option<Vec<AgentCommand>> {
        None
    }

    /// Hands a message to the running turn, without starting a new one. Fails when the
    /// driver cannot, or no turn runs: the hub then queues the message. The driver reports
    /// [`SessionEventKind::SteerRead`] once the model has read it.
    async fn steer(
        &self,
        _message_id: crate::MessageId,
        _prompt: &crate::Prompt,
    ) -> Result<(), CoreError> {
        Err(unsupported("steer"))
    }

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

    /// The process runs in a runtime host and survives the hub: when the hub leaves, the
    /// session is left running for the next hub to [`Driver::adopt`].
    fn outlives_hub(&self) -> bool {
        false
    }

    /// For a session from [`Driver::adopt`] with `turn_running`: waits for the turn the
    /// previous hub started. `None` when there is no such turn.
    async fn adopted_turn(&self) -> Option<Result<TurnOutcome, CoreError>> {
        None
    }
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
    /// Whether the program it launches can be found on this machine. A Profile that is not
    /// installed offers no models, so nothing is started on it only to fail.
    fn installed(&self) -> bool {
        true
    }
    /// Where that program was found, and how to install it when it was not: what
    /// `PluginInfo::cli` reports (docs/DESIGN.md §9.3). `None` for drivers that launch
    /// whatever they are configured with (ACP).
    async fn cli_status(&self) -> Option<crate::CliStatus> {
        None
    }
    /// Asks the program its version, so that a later [`Self::cli_status`] can report it.
    /// The hub runs this in the background at startup, never on a request's path: the answer
    /// costs a subprocess, and `GET /v1/plugins` must not wait for one.
    async fn probe_cli(&self) {}
    /// Forgets what the last lookup and the version probe remembered, so that the next
    /// [`Self::cli_status`] looks again: called after an install or an upgrade, which may
    /// have put a new file where the old one was (docs/DESIGN.md §9.3).
    fn refresh_cli(&self) {}
    /// Everywhere the description looks for its CLI on this machine, for a failure report
    /// ("the command succeeded, but nothing was found: it looked in ...").
    fn cli_places(&self) -> Vec<String> {
        Vec::new()
    }
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
    /// Takes over the session's process left running by a previous hub (it runs in a
    /// runtime host, see [`DriverSession::outlives_hub`]). `turn_running`: the previous hub
    /// saw a turn start and not end; [`DriverSession::adopted_turn`] then waits for it.
    /// `Ok(None)` when no such process is left.
    async fn adopt(
        &self,
        _session_id: &str,
        _turn_running: bool,
        _spec: SessionSpec,
    ) -> Result<Option<Box<dyn DriverSession>>, CoreError> {
        Ok(None)
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
