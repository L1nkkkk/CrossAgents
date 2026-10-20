//! Events: the record of every change in the system. See docs/DESIGN.md §2.
//!
//! Compatibility rule: only ever *add* variants or optional fields. Clients must ignore
//! event types they do not know.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ids::AgentId;
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
