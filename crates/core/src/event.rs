//! Events: the record of every change in the system. See docs/DESIGN.md §2.
//!
//! Compatibility rule: only ever *add* variants or optional fields. Clients must ignore
//! event types they do not know.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::{OutputStream, StopReason, ToolStatus};
use crate::ids::{AgentId, MessageId};
use crate::model::{Agent, AgentStatus};

pub const EVENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Event {
    /// Assigned by the store; strictly increasing. Used as the SSE event id.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub seq: u64,
    pub id: Uuid,
    pub ts: DateTime<Utc>,
    pub schema: u32,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "data")]
pub enum EventKind {
    #[serde(rename = "hub.started")]
    HubStarted { version: String },
    #[serde(rename = "hub.stopping")]
    HubStopping,
    #[serde(rename = "agent.created")]
    AgentCreated { agent: Agent },
    #[serde(rename = "agent.removed")]
    AgentRemoved { id: AgentId, name: String },
    #[serde(rename = "agent.status_changed")]
    AgentStatusChanged {
        id: AgentId,
        from: AgentStatus,
        to: AgentStatus,
    },
    /// A message for the agent was accepted; `queued` if it waits for the current turn.
    #[serde(rename = "agent.input")]
    AgentInput {
        id: AgentId,
        message_id: MessageId,
        text: String,
        queued: bool,
    },
    #[serde(rename = "agent.turn_started")]
    AgentTurnStarted { id: AgentId, message_id: MessageId },
    /// Coalesced output text.
    #[serde(rename = "agent.output")]
    AgentOutput {
        id: AgentId,
        stream: OutputStream,
        text: String,
    },
    #[serde(rename = "agent.tool_call")]
    AgentToolCall {
        id: AgentId,
        tool_call_id: String,
        title: String,
        kind: Option<String>,
        status: ToolStatus,
    },
    #[serde(rename = "agent.tool_call_updated")]
    AgentToolCallUpdated {
        id: AgentId,
        tool_call_id: String,
        title: Option<String>,
        status: Option<ToolStatus>,
    },
    #[serde(rename = "agent.turn_completed")]
    AgentTurnCompleted {
        id: AgentId,
        message_id: MessageId,
        stop_reason: StopReason,
    },
    #[serde(rename = "agent.usage")]
    AgentUsage {
        id: AgentId,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        used: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        size: u64,
        cost_usd: Option<f64>,
    },
    #[serde(rename = "agent.error")]
    AgentError { id: AgentId, message: String },
}

impl EventKind {
    /// The wire name, identical to the serialized `type` field.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::HubStarted { .. } => "hub.started",
            Self::HubStopping => "hub.stopping",
            Self::AgentCreated { .. } => "agent.created",
            Self::AgentRemoved { .. } => "agent.removed",
            Self::AgentStatusChanged { .. } => "agent.status_changed",
            Self::AgentInput { .. } => "agent.input",
            Self::AgentTurnStarted { .. } => "agent.turn_started",
            Self::AgentOutput { .. } => "agent.output",
            Self::AgentToolCall { .. } => "agent.tool_call",
            Self::AgentToolCallUpdated { .. } => "agent.tool_call_updated",
            Self::AgentTurnCompleted { .. } => "agent.turn_completed",
            Self::AgentUsage { .. } => "agent.usage",
            Self::AgentError { .. } => "agent.error",
        }
    }

    /// The agent this event is about, if any.
    pub fn agent_id(&self) -> Option<AgentId> {
        match self {
            Self::HubStarted { .. } | Self::HubStopping => None,
            Self::AgentCreated { agent } => Some(agent.id),
            Self::AgentRemoved { id, .. }
            | Self::AgentStatusChanged { id, .. }
            | Self::AgentInput { id, .. }
            | Self::AgentTurnStarted { id, .. }
            | Self::AgentOutput { id, .. }
            | Self::AgentToolCall { id, .. }
            | Self::AgentToolCallUpdated { id, .. }
            | Self::AgentTurnCompleted { id, .. }
            | Self::AgentUsage { id, .. }
            | Self::AgentError { id, .. } => Some(*id),
        }
    }
}

/// A consumer of published events (cost tracking, logging, notifications, ...).
#[async_trait]
pub trait EventSink: Send + Sync {
    async fn handle(&self, event: &Event);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_flat_with_type_and_data() {
        let event = Event {
            seq: 7,
            id: Uuid::now_v7(),
            ts: Utc::now(),
            schema: EVENT_SCHEMA_VERSION,
            kind: EventKind::AgentRemoved {
                id: AgentId::new(),
                name: "alice".into(),
            },
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "agent.removed");
        assert_eq!(value["data"]["name"], "alice");
        assert_eq!(value["seq"], 7);
        assert_eq!(serde_json::from_value::<Event>(value).unwrap(), event);
    }

    #[test]
    fn type_name_matches_serde() {
        let kinds = [
            EventKind::HubStarted {
                version: "0".into(),
            },
            EventKind::HubStopping,
        ];
        for kind in kinds {
            let value = serde_json::to_value(&kind).unwrap();
            assert_eq!(value["type"], kind.type_name());
        }
    }
}
