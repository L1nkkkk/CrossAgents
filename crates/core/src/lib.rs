//! CrossAgents core: domain model, events and the traits every other crate builds on.
//!
//! This crate has no dependency on any other CrossAgents crate.

pub mod account;
pub mod attachment;
pub mod bus;
pub mod claim;
pub mod cli;
pub mod device;
pub mod driver;
pub use device::{Device, DeviceKind, DeviceLevel};
pub mod error;
pub mod event;
pub mod exec;
pub mod expertise;
pub mod handoff;
pub mod human;
pub mod ids;
pub mod lesson;
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
    Account, AccountProvider, AccountStatus, CodebuddySite, Credits, DataSource, LimitGroup,
    LoginCommand, ModelInfo, RateWindow, RequestUsage, ResetCredit, SessionUsage, TokenTotals,
    mask_email,
};
pub use attachment::{Attachment, AttachmentKind, AttachmentSupport, Prompt};
pub use bus::EventBus;
pub use claim::{AgentClaim, ClaimRole};
pub use cli::{CliSource, CliStatus, Located};
pub use driver::{
    AgentCommand, AgentCommandKind, Capabilities, ContextPart, Driver, DriverKind, DriverSession,
    ExitCause, FileChange, McpServerSpec, OutputStream, PermissionDecision, PermissionHandler,
    PermissionPolicy, PermissionRequest, RateLimits, SessionEvent, SessionEventKind, SessionSpec,
    StopReason, TokenUsage, ToolDetail, ToolStatus, TurnOutcome,
};
pub use error::CoreError;
pub use event::{
    AccountCandidate, DeliveryState, DeliveryStatus, DiffSource, EVENT_SCHEMA_VERSION, Envelope,
    CarriedNotice, Event, EventKind, EventSink, MessageSource, QueuedReason, RoundNotice,
    RoundSource,
};
pub use expertise::{
    Candidate, CandidateOrigin, CandidateStatus, EntryKind, EntryStats, EntryStatus, Example,
    ExpertiseEntry, OwnerKind, PERSON_OWNER_ID, SourceRef, UsageOutcome, UsageRecord, UsageSource,
    estimate_tokens,
};
pub use handoff::{Handoff, HandoffKind, HandoffOutcome};
pub use human::{HumanAction, HumanItem, HumanItemKind, HumanItemStatus};
pub use ids::{AgentId, HandoffId, MessageId, RunId, SessionId, TaskId, WorkId};
pub use lesson::{
    Evidence, ISSUE_PREFIX, IssueRef, Lesson, LessonOutcome, LessonStatus, Project, ProjectPath,
    RelatedLesson, Triage, TriageRecord, TriageStep,
};
pub use model::{
    Agent, AgentProfile, AgentStatus, CONTEXT_LOST, CheckResult, ChecksState, ContinuationState,
    FindingAnswer, FindingStatus, Health, HistoryTask, LandMode, Landing, LeadPlace, NameSource,
    PlannedTask, Retention, Role, Run, RunConfig, RunHistory, RunMode, RunRecord, RunStatus, Scope,
    Task, TaskReport, TaskRun, TaskRunOutcome, TaskSpec, Work, WorkspaceOwner, WorkspaceVcs,
    validate_agent_name, validate_slug,
};
pub use note::{EXTERNAL_PLUGIN, Note, NoteScope};
pub use notify::{NoticeClass, PendingNotice, Reminder, Watch, WatchOn, WatchTarget};
pub use policy::{
    ApprovalPolicy, AskPolicy, Budget, ContinuationPolicy, LandPolicy, PlanApproval, RunPolicy,
};
pub use state::TaskStatus;
pub use store::{AgentKey, Store};
pub use tool::ToolInfo;
