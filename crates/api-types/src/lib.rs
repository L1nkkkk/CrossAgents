//! Request, response and error types of the hub's local API (docs/DESIGN.md §11).
//!
//! Running `cargo test -p crossagents-api-types` regenerates the TypeScript bindings in
//! `bindings/` for GUI clients.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub use crossagents_core::{
    Account, AccountStatus, Agent, AgentId, AgentProfile, AgentStatus, Capabilities, CheckResult,
    DataSource, DriverKind, Event, EventKind, MessageId, MessageSource, ModelInfo, OutputStream,
    PlannedTask, Role, Run, RunId, RunStatus, StopReason, Task, TaskId, TaskStatus, TokenTotals,
    ToolInfo, ToolStatus,
};

pub const API_VERSION: &str = "v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HealthResponse {
    /// Always `"ok"` when the hub answers.
    pub status: String,
    pub version: String,
    pub api: String,
    #[ts(type = "number")]
    pub uptime_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HubInfo {
    pub version: String,
    pub api: String,
    pub pid: u32,
    pub home: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateAgentRequest {
    pub name: String,
    pub plugin: String,
    #[serde(default)]
    pub driver: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub role: Role,
    pub cwd: String,
    #[serde(default)]
    pub config: serde_json::Value,
    /// Display name, title, avatar and persona; all optional.
    #[serde(default)]
    pub profile: crossagents_core::AgentProfile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListAgentsResponse {
    pub agents: Vec<Agent>,
}

/// `POST /v1/agents/{key}/model`. `None` leaves the model to the agent's own default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SetModelRequest {
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SendMessageRequest {
    pub text: String,
    /// Put the message ahead of everything waiting and interrupt the running turn.
    #[serde(default)]
    #[ts(optional)]
    pub now: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SendMessageResponse {
    /// Identifies the turn that answers this message in `agent.turn_*` events.
    pub message_id: crossagents_core::MessageId,
    /// `true` if the message waits behind the agent's current work.
    pub queued: bool,
}

/// A session's conversation, rebuilt from its events and grouped by turn.
/// `GET /v1/agents/{key}/transcript`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Transcript {
    pub agent: Agent,
    /// The driver the agent runs on; `None` if its plugin is no longer configured.
    pub driver: Option<DriverKind>,
    /// What that driver can do; clients show operations by these flags.
    pub capabilities: Option<Capabilities>,
    /// Oldest first. A message still waiting for its turn is the last turn, not started.
    pub turns: Vec<TranscriptTurn>,
    /// Latest context usage the agent reported.
    pub usage: Option<TranscriptUsage>,
    /// The last event folded in; follow `GET /v1/events?agent=…&since=<last_seq>` from here.
    #[ts(type = "number")]
    pub last_seq: u64,
}

/// One message to the session and everything the agent did in answer to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TranscriptTurn {
    pub message_id: MessageId,
    pub input: String,
    /// Who sent the message; `None` for messages recorded before senders were.
    pub from: Option<MessageSource>,
    /// The message this one answers, the conversation it belongs to, and the start of the
    /// message it answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reply_to: Option<MessageId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub thread: Option<MessageId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reply_excerpt: Option<String>,
    pub sent_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Set once the turn has ended.
    pub stop_reason: Option<StopReason>,
    /// In arrival order. Consecutive output of the same stream is merged into one item.
    pub items: Vec<TranscriptItem>,
    /// Everything the turn changed, as a unified diff, when the agent reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub diff: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptItem {
    Output {
        stream: OutputStream,
        text: String,
    },
    ToolCall {
        tool_call_id: String,
        title: String,
        kind: Option<String>,
        status: ToolStatus,
        /// Live output while it runs (e.g. a command's stdout).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        output: Option<String>,
        /// What it did, once the agent reports it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        detail: Option<Box<crossagents_core::ToolDetail>>,
    },
    /// Asked before acting; `decision` is what the permission policy answered.
    Permission {
        kind: String,
        title: String,
        decision: crossagents_core::PermissionDecision,
    },
    /// Worth showing, but nothing failed.
    Warning {
        message: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TranscriptUsage {
    /// Tokens in the context window.
    #[ts(type = "number")]
    pub used: u64,
    /// Size of the context window.
    #[ts(type = "number")]
    pub size: u64,
    pub cost_usd: Option<f64>,
    /// Cumulative token counts of the session, when the agent reports them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tokens: Option<crossagents_core::TokenUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginInfo {
    pub name: String,
    pub models: Vec<String>,
    /// The driver its agents use unless they name another.
    pub driver: crossagents_core::DriverKind,
    pub capabilities: crossagents_core::Capabilities,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListPluginsResponse {
    pub plugins: Vec<PluginInfo>,
}

/// Starts a run. Agents are given by name or id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateRunRequest {
    pub request: String,
    #[serde(default)]
    pub title: Option<String>,
    pub planner: String,
    /// Template for workers.
    pub worker: String,
    #[serde(default)]
    pub escalate: Option<String>,
    #[serde(default)]
    pub reviewer: Option<String>,
    #[serde(default)]
    pub auto_approve: bool,
    #[serde(default)]
    pub max_parallel: Option<u32>,
    #[serde(default)]
    pub max_attempts: Option<u32>,
    /// Teammates the planner may assign tasks to, by name. Omitted: every worker and
    /// reviewer registered in the planner's repository.
    #[serde(default)]
    pub team: Option<Vec<String>>,
    /// How much the run may do without a person; the hub's defaults when omitted.
    /// `auto_approve: true` still means `plan_approval: auto`.
    #[serde(default)]
    #[ts(optional)]
    pub policy: Option<crossagents_core::RunPolicy>,
}

/// `POST /v1/runs/{key}/land`; merge when `mode` is omitted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LandRequest {
    #[serde(default)]
    #[ts(optional)]
    pub mode: Option<crossagents_core::LandMode>,
}

/// `POST /v1/runs/{key}/finish`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FinishRequest {
    #[serde(default)]
    #[ts(optional)]
    pub summary: Option<String>,
}

/// `POST /v1/questions/{id}/answer`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AnswerRequest {
    pub answer: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListQuestionsResponse {
    pub items: Vec<crossagents_core::HumanItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListRunsResponse {
    pub runs: Vec<crossagents_core::Run>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunDetail {
    pub run: crossagents_core::Run,
    pub tasks: Vec<crossagents_core::Task>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RejectRunRequest {
    pub reason: String,
}

/// How `GET /v1/usage` groups agents.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum UsageGroup {
    #[default]
    Role,
    Model,
    Run,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UsageRow {
    /// The role, model, run id or agent name.
    pub key: String,
    pub agents: u32,
    pub sessions: u32,
    /// Sum of the cost agents reported; 0 when none reported any.
    pub cost_usd: f64,
    /// Whether any agent in the group reported a cost.
    pub cost_reported: bool,
    /// Largest context (tokens) any session in the group reached.
    #[ts(type = "number")]
    pub peak_context: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UsageResponse {
    pub by: UsageGroup,
    pub rows: Vec<UsageRow>,
    pub total_cost_usd: f64,
}

/// Adds an account. Without `home`, a fresh home under the CrossAgents directory is used;
/// with `import_default`, the provider's existing default home (e.g. `~/.codex`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateAccountRequest {
    pub id: String,
    pub provider: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub home: Option<String>,
    #[serde(default)]
    pub import_default: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListAccountsResponse {
    pub accounts: Vec<crossagents_core::Account>,
}

/// One account with its live status and models. `GET /v1/quota` lists every account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AccountQuota {
    pub account: crossagents_core::Account,
    pub status: crossagents_core::AccountStatus,
    pub models: Vec<crossagents_core::ModelInfo>,
    /// Agents that run on this account (named in their config, or using the provider's
    /// own login).
    pub agents: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct QuotaResponse {
    pub accounts: Vec<AccountQuota>,
    /// Account providers the hub supports, for adding accounts.
    pub providers: Vec<String>,
}

/// How account token usage is grouped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum AccountUsageGroup {
    #[default]
    Day,
    Model,
    /// CrossAgents sessions vs. the vendor's own tools.
    Origin,
    Agent,
    Role,
    Run,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AccountUsageRow {
    pub key: String,
    pub totals: crossagents_core::TokenTotals,
    pub cache_hit_rate: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AccountUsageResponse {
    pub by: AccountUsageGroup,
    pub rows: Vec<AccountUsageRow>,
    pub total: crossagents_core::TokenTotals,
    pub total_cache_hit_rate: f64,
}

/// A login was started; the human finishes it in the browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LoginStarted {
    /// The page to open, when the login tool printed one.
    pub url: Option<String>,
    /// What runs, for display.
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListToolsResponse {
    pub tools: Vec<crossagents_core::ToolInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    Unauthorized,
    NotFound,
    Conflict,
    /// The agent process or its protocol failed.
    AgentError,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApiErrorBody {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApiError {
    pub error: ApiErrorBody,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            error: ApiErrorBody {
                code,
                message: message.into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Core types are not `#[ts(export)]` themselves; export them (and everything they
    /// reference) from here so all bindings come from one place.
    #[test]
    fn export_core_bindings() {
        let cfg = ts_rs::Config::from_env();
        Event::export_all(&cfg).unwrap();
        crossagents_core::Task::export_all(&cfg).unwrap();
        crossagents_core::Run::export_all(&cfg).unwrap();
    }

    #[test]
    fn create_agent_request_defaults() {
        let req: CreateAgentRequest =
            serde_json::from_str(r#"{"name":"a","plugin":"claude","role":"worker","cwd":"."}"#)
                .unwrap();
        assert_eq!(req.driver, None);
        assert!(req.config.is_null());
    }

    #[test]
    fn api_error_shape() {
        let json = serde_json::to_value(ApiError::new(ErrorCode::NotFound, "nope")).unwrap();
        assert_eq!(json["error"]["code"], "not_found");
    }
}
