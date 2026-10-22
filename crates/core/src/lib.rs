//! CrossAgents core: domain model, events and the traits every other crate builds on.
//!
//! This crate has no dependency on any other CrossAgents crate.

pub mod account;
pub mod bus;
pub mod driver;
pub mod error;
pub mod event;
pub mod human;
pub mod ids;
pub mod model;
pub mod note;
pub mod notify;
pub mod policy;
pub mod state;
pub mod store;
pub mod tool;

pub use account::{
    Account, AccountProvider, AccountStatus, Credits, DataSource, LimitGroup, LoginCommand,
    ModelInfo, RateWindow, ResetCredit, SessionUsage, TokenTotals, mask_email,
};
pub use bus::EventBus;
pub use driver::{
    Capabilities, Driver, DriverKind, DriverSession, FileChange, McpServerSpec, OutputStream,
    PermissionDecision, PermissionHandler, PermissionPolicy, PermissionRequest, RateLimits,
    SessionEvent, SessionEventKind, SessionSpec, StopReason, TokenUsage, ToolDetail, ToolStatus,
    TurnOutcome,
};
pub use error::CoreError;
pub use event::{
    DiffSource, EVENT_SCHEMA_VERSION, Envelope, Event, EventKind, EventSink, MessageSource,
};
pub use human::{HumanAction, HumanItem, HumanItemKind, HumanItemStatus};
pub use ids::{AgentId, MessageId, RunId, SessionId, TaskId};
pub use model::{
    Agent, AgentProfile, AgentStatus, CheckResult, LandMode, Landing, PlannedTask, Role, Run,
    RunConfig, RunStatus, Scope, Task, TaskReport, TaskSpec, validate_agent_name,
};
pub use note::{EXTERNAL_PLUGIN, Note, NoteScope};
pub use notify::{Reminder, Watch, WatchOn, WatchTarget};
pub use policy::{AskPolicy, Budget, LandPolicy, PlanApproval, RunPolicy};
pub use state::TaskStatus;
pub use store::{AgentKey, Store};
pub use tool::ToolInfo;
