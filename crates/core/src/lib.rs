//! CrossAgents core: domain model, events and the traits every other crate builds on.
//!
//! This crate has no dependency on any other CrossAgents crate.

pub mod account;
pub mod attachment;
pub mod bus;
pub mod claim;
pub mod cli;
pub mod driver;
pub mod error;
pub mod event;
pub mod handoff;
pub mod human;
pub mod ids;
pub mod model;
pub mod names;
pub mod note;
pub mod notify;
pub mod policy;
pub mod process;
pub mod state;
pub mod store;
pub mod tool;

pub use account::{
    Account, AccountProvider, AccountStatus, Credits, DataSource, LimitGroup, LoginCommand,
    ModelInfo, RateWindow, ResetCredit, SessionUsage, TokenTotals, mask_email,
};
pub use attachment::{Attachment, AttachmentKind, AttachmentSupport, Prompt};
pub use bus::EventBus;
pub use claim::{AgentClaim, ClaimRole};
pub use cli::{CliSource, CliStatus, Located};
pub use driver::{
    Capabilities, ContextPart, Driver, DriverKind, DriverSession, ExitCause, FileChange,
    McpServerSpec, OutputStream, PermissionDecision, PermissionHandler, PermissionPolicy,
    PermissionRequest, RateLimits, SessionEvent, SessionEventKind, SessionSpec, StopReason,
    TokenUsage, ToolDetail, ToolStatus, TurnOutcome,
};
pub use error::CoreError;
pub use event::{
    DiffSource, EVENT_SCHEMA_VERSION, Envelope, Event, EventKind, EventSink, MessageSource,
};
pub use handoff::{Handoff, HandoffKind, HandoffOutcome};
pub use human::{HumanAction, HumanItem, HumanItemKind, HumanItemStatus};
pub use ids::{AgentId, HandoffId, MessageId, RunId, SessionId, TaskId};
pub use model::{
    Agent, AgentProfile, AgentStatus, CONTEXT_LOST, CheckResult, LandMode, Landing, NameSource,
    PlannedTask, Role, Run, RunConfig, RunStatus, Scope, Task, TaskReport, TaskSpec,
    validate_agent_name, validate_slug,
};
pub use note::{EXTERNAL_PLUGIN, Note, NoteScope};
pub use notify::{Reminder, Watch, WatchOn, WatchTarget};
pub use policy::{ApprovalPolicy, AskPolicy, Budget, LandPolicy, PlanApproval, RunPolicy};
pub use state::TaskStatus;
pub use store::{AgentKey, Store};
pub use tool::ToolInfo;
