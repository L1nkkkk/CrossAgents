//! Events: the record of every change in the system. See docs/DESIGN.md §2.
//!
//! Compatibility rule: only ever *add* variants or optional fields. Clients must ignore
//! event types they do not know.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::account::Account;
use crate::driver::{OutputStream, StopReason, ToolStatus};
use crate::ids::{AgentId, MessageId, RunId, TaskId};
use crate::model::{Agent, AgentStatus, CheckResult, Run, RunStatus};
use crate::state::TaskStatus;

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
    /// The agent's profile changed; `agent` is the new record.
    #[serde(rename = "agent.updated")]
    AgentUpdated { agent: Agent },
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
        /// Who sent it; absent in events written before the field existed (a person).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        from: Option<MessageSource>,
    },
    /// A queued message was taken back before its turn started.
    #[serde(rename = "agent.input_withdrawn")]
    AgentInputWithdrawn { id: AgentId, message_id: MessageId },
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
    /// The agent's vendor session id, for joining with the vendor's usage logs.
    #[serde(rename = "agent.session_started")]
    AgentSessionStarted { id: AgentId, session_id: String },
    #[serde(rename = "account.created")]
    AccountCreated { account: Account },
    #[serde(rename = "account.removed")]
    AccountRemoved { id: String },
    /// A login started with `POST /v1/accounts/{id}/login` finished.
    #[serde(rename = "account.login_finished")]
    AccountLoginFinished {
        id: String,
        ok: bool,
        message: Option<String>,
    },
    /// A login is waiting for the human to open `url`.
    #[serde(rename = "account.login_pending")]
    AccountLoginPending { id: String, url: Option<String> },
    #[serde(rename = "run.created")]
    RunCreated { run: Run },
    #[serde(rename = "run.status_changed")]
    RunStatusChanged {
        id: RunId,
        from: RunStatus,
        to: RunStatus,
        note: Option<String>,
    },
    /// The planner submitted a plan; `tasks` are the task keys in plan order.
    #[serde(rename = "run.plan_submitted")]
    RunPlanSubmitted { id: RunId, tasks: Vec<String> },
    #[serde(rename = "task.status_changed")]
    TaskStatusChanged {
        run_id: RunId,
        id: TaskId,
        key: String,
        from: TaskStatus,
        to: TaskStatus,
        assignee: Option<AgentId>,
    },
    /// Acceptance commands ran.
    #[serde(rename = "task.checked")]
    TaskChecked {
        run_id: RunId,
        id: TaskId,
        key: String,
        passed: bool,
        checks: Vec<CheckResult>,
    },
}

/// Who sent a message to an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageSource {
    /// A person, through the API.
    Human,
    /// The orchestrator, about a run and possibly one of its tasks.
    Orchestrator {
        run_id: RunId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        task_id: Option<TaskId>,
    },
    /// Another agent (`agent_send`).
    Agent { agent_id: AgentId },
}

impl MessageSource {
    pub fn run(run_id: RunId) -> Self {
        Self::Orchestrator {
            run_id,
            task_id: None,
        }
    }

    pub fn task(run_id: RunId, task_id: TaskId) -> Self {
        Self::Orchestrator {
            run_id,
            task_id: Some(task_id),
        }
    }
}

impl EventKind {
    /// The wire name, identical to the serialized `type` field.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::HubStarted { .. } => "hub.started",
            Self::HubStopping => "hub.stopping",
            Self::AgentCreated { .. } => "agent.created",
            Self::AgentUpdated { .. } => "agent.updated",
            Self::AgentRemoved { .. } => "agent.removed",
            Self::AgentStatusChanged { .. } => "agent.status_changed",
            Self::AgentInput { .. } => "agent.input",
            Self::AgentInputWithdrawn { .. } => "agent.input_withdrawn",
            Self::AgentTurnStarted { .. } => "agent.turn_started",
            Self::AgentOutput { .. } => "agent.output",
            Self::AgentToolCall { .. } => "agent.tool_call",
            Self::AgentToolCallUpdated { .. } => "agent.tool_call_updated",
            Self::AgentTurnCompleted { .. } => "agent.turn_completed",
            Self::AgentUsage { .. } => "agent.usage",
            Self::AgentError { .. } => "agent.error",
            Self::AgentSessionStarted { .. } => "agent.session_started",
            Self::AccountCreated { .. } => "account.created",
            Self::AccountRemoved { .. } => "account.removed",
            Self::AccountLoginFinished { .. } => "account.login_finished",
            Self::AccountLoginPending { .. } => "account.login_pending",
            Self::RunCreated { .. } => "run.created",
            Self::RunStatusChanged { .. } => "run.status_changed",
            Self::RunPlanSubmitted { .. } => "run.plan_submitted",
            Self::TaskStatusChanged { .. } => "task.status_changed",
            Self::TaskChecked { .. } => "task.checked",
        }
    }

    /// The run this event is about, if any.
    pub fn run_id(&self) -> Option<RunId> {
        match self {
            Self::RunCreated { run } => Some(run.id),
            Self::RunStatusChanged { id, .. } | Self::RunPlanSubmitted { id, .. } => Some(*id),
            Self::TaskStatusChanged { run_id, .. } | Self::TaskChecked { run_id, .. } => {
                Some(*run_id)
            }
            _ => None,
        }
    }

    /// The agent this event is about, if any.
    pub fn agent_id(&self) -> Option<AgentId> {
        match self {
            Self::HubStarted { .. }
            | Self::HubStopping
            | Self::RunCreated { .. }
            | Self::RunStatusChanged { .. }
            | Self::RunPlanSubmitted { .. }
            | Self::TaskChecked { .. }
            | Self::AccountCreated { .. }
            | Self::AccountRemoved { .. }
            | Self::AccountLoginFinished { .. }
            | Self::AccountLoginPending { .. } => None,
            Self::AgentSessionStarted { id, .. } => Some(*id),
            Self::TaskStatusChanged { assignee, .. } => *assignee,
            Self::AgentCreated { agent } | Self::AgentUpdated { agent } => Some(agent.id),
            Self::AgentRemoved { id, .. }
            | Self::AgentStatusChanged { id, .. }
            | Self::AgentInput { id, .. }
            | Self::AgentInputWithdrawn { id, .. }
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
