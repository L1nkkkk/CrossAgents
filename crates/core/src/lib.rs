//! CrossAgents core: domain model, events and the traits every other crate builds on.
//!
//! This crate has no dependency on any other CrossAgents crate.

pub mod account;
pub mod agent;
pub mod bus;
pub mod error;
pub mod event;
pub mod ids;
pub mod model;
pub mod state;
pub mod store;
pub mod tool;

pub use account::{
    Account, AccountProvider, AccountStatus, Credits, DataSource, LoginCommand, ModelInfo,
    RateWindow, SessionUsage, TokenTotals, mask_email,
};
pub use agent::{
    AgentPlugin, AgentSession, Capabilities, McpServerSpec, OutputStream, PermissionPolicy,
    SessionEvent, SessionSpec, StopReason, ToolStatus,
};
pub use bus::EventBus;
pub use error::CoreError;
pub use event::{EVENT_SCHEMA_VERSION, Event, EventKind, EventSink};
pub use ids::{AgentId, MessageId, RunId, SessionId, TaskId};
pub use model::{
    Agent, AgentStatus, CheckResult, PlannedTask, Role, Run, RunConfig, RunStatus, Scope, Task,
    TaskReport, TaskSpec, validate_agent_name,
};
pub use state::TaskStatus;
pub use store::{AgentKey, Store};
pub use tool::ToolInfo;
