//! Request, response and error types of the hub's local API (docs/DESIGN.md §11).
//!
//! Running `cargo test -p crossagents-api-types` regenerates the TypeScript bindings in
//! `bindings/` for GUI clients.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub use crossagents_core::{
    Agent, AgentId, AgentStatus, Capabilities, Event, EventKind, MessageId, OutputStream, Role,
    StopReason, ToolStatus,
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListAgentsResponse {
    pub agents: Vec<Agent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SendMessageRequest {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SendMessageResponse {
    /// Identifies the turn that answers this message in `agent.turn_*` events.
    pub message_id: crossagents_core::MessageId,
    /// `true` if the message waits behind the agent's current work.
    pub queued: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginInfo {
    pub name: String,
    pub models: Vec<String>,
    pub capabilities: crossagents_core::Capabilities,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListPluginsResponse {
    pub plugins: Vec<PluginInfo>,
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
