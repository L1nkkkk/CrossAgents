//! Events: the record of every change in the system. See docs/DESIGN.md §2.
//!
//! Compatibility rule: only ever *add* variants or optional fields. Clients must ignore
//! event types they do not know.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::account::Account;
use crate::driver::{
    ExitCause, OutputStream, PermissionDecision, RateLimits, StopReason, TokenUsage, ToolDetail,
    ToolStatus,
};
use crate::human::HumanItem;
use crate::ids::{AgentId, MessageId, RunId, TaskId};
use crate::model::{Agent, AgentStatus, CheckResult, Landing, Run, RunStatus};
use crate::note::NoteScope;
use crate::state::TaskStatus;

pub const EVENT_SCHEMA_VERSION: u32 = 1;

/// Who worked out what a turn changed (docs/tasks/V6-GUI.md §5).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum DiffSource {
    /// The driver reported it (`structured_diff`).
    #[default]
    Driver,
    /// The hub compared snapshots of the working directory taken before and after the turn.
    Hub,
}

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

// The variants carry whole records, so they differ a lot in size: `RunCreated { run: Run }`
// is some 592 bytes against a few dozen for most. Boxing the big ones is the real fix, and it
// touches every place that builds or matches an event (docs/TODO.md); until then this lint is
// the one thing standing between the repository and `clippy -D warnings`, which is worth more
// as a gate on everything else.
#[allow(clippy::large_enum_variant)]
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
        /// The message this one answers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        reply_to: Option<MessageId>,
        /// The first message of the conversation this one belongs to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        thread: Option<MessageId>,
        /// The start of the message replied to, for showing "回复「…」".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        reply_excerpt: Option<String>,
        /// Files sent with it (docs/tasks/DOGFOOD.md §3).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[cfg_attr(feature = "ts", ts(as = "Option<Vec<crate::Attachment>>", optional))]
        attachments: Vec<crate::Attachment>,
        /// Notices that rode along with it (docs/TODO.md §二十五 第 3 条): they are in what
        /// the agent reads, after the person's words, but not in `text`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]
        notices: Vec<String>,
    },
    /// A waiting message got new text (the hub's notification digest gathers news while
    /// the agent is busy).
    #[serde(rename = "agent.input_updated")]
    AgentInputUpdated {
        id: AgentId,
        message_id: MessageId,
        text: String,
    },
    /// The model or reasoning effort the agent runs on was switched; `None` is the plugin's
    /// default. The agent hears about it with its next message.
    #[serde(rename = "agent.model_changed")]
    AgentModelChanged {
        id: AgentId,
        model: Option<String>,
        effort: Option<String>,
        from_model: Option<String>,
        from_effort: Option<String>,
    },
    /// A queued message was taken back before its turn started.
    #[serde(rename = "agent.input_withdrawn")]
    AgentInputWithdrawn { id: AgentId, message_id: MessageId },
    /// A message went into the running turn instead of waiting for it to end.
    #[serde(rename = "agent.steered")]
    AgentSteered {
        id: AgentId,
        message_id: MessageId,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[cfg_attr(feature = "ts", ts(as = "Option<Vec<crate::Attachment>>", optional))]
        attachments: Vec<crate::Attachment>,
    },
    /// The model has read a message from [`Self::AgentSteered`].
    #[serde(rename = "agent.steer_read")]
    AgentSteerRead { id: AgentId, message_id: MessageId },
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
    /// Coalesced output of a running tool call (e.g. a command's stdout).
    #[serde(rename = "agent.tool_output")]
    AgentToolOutput {
        id: AgentId,
        tool_call_id: String,
        text: String,
    },
    /// What a tool call did: command result, changed files, MCP arguments and result.
    #[serde(rename = "agent.tool_detail")]
    AgentToolDetail {
        id: AgentId,
        tool_call_id: String,
        detail: ToolDetail,
    },
    /// Everything the current turn changed so far, as a unified diff; replaces the last one.
    #[serde(rename = "agent.turn_diff")]
    AgentTurnDiff {
        id: AgentId,
        diff: String,
        /// The turn it belongs to; absent in events written before the field existed (the
        /// running turn, else the last one that started).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        message_id: Option<MessageId>,
        /// Who worked it out; absent means the driver.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        source: Option<DiffSource>,
    },
    /// The agent asked before acting and was answered by the permission policy.
    #[serde(rename = "agent.permission")]
    AgentPermission {
        id: AgentId,
        kind: String,
        title: String,
        decision: PermissionDecision,
    },
    /// The account quota as the agent saw it on its last model call.
    #[serde(rename = "agent.rate_limits")]
    AgentRateLimits { id: AgentId, limits: RateLimits },
    /// A problem worth showing that did not fail anything (e.g. a retried request).
    #[serde(rename = "agent.warning")]
    AgentWarning { id: AgentId, message: String },
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
        /// Cumulative token counts, when the agent reports them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        tokens: Option<TokenUsage>,
    },
    #[serde(rename = "agent.context")]
    AgentContext {
        id: AgentId,
        parts: Vec<crate::ContextPart>,
    },
    #[serde(rename = "agent.error")]
    AgentError { id: AgentId, message: String },
    /// The agent's process ended without being asked to (it crashed, exited, or its runtime
    /// host was lost). `cause` is missing when the driver cannot tell.
    #[serde(rename = "agent.exited")]
    AgentExited {
        id: AgentId,
        cause: Option<ExitCause>,
        code: Option<i32>,
        message: String,
    },
    /// A new hub took over the agent's process, left running by the previous hub.
    #[serde(rename = "agent.adopted")]
    AgentAdopted {
        id: AgentId,
        /// The turn that was running and goes on.
        message_id: Option<MessageId>,
    },
    /// The agent's vendor session id, for joining with the vendor's usage logs.
    #[serde(rename = "agent.session_started")]
    AgentSessionStarted { id: AgentId, session_id: String },
    /// An agent asked the person something (`human_ask`) or told them something
    /// (`human_notify`); also the hub's own questions (budget).
    #[serde(rename = "human.asked")]
    HumanAsked { item: HumanItem },
    /// The person answered, or the default was taken (`item.status` says which).
    #[serde(rename = "human.answered")]
    HumanAnswered { item: HumanItem },
    /// A notice was read, or a question dropped.
    #[serde(rename = "human.dismissed")]
    HumanDismissed { id: String },
    /// The run's branch reached its base branch (or a pull request was opened).
    #[serde(rename = "run.landed")]
    RunLanded { id: RunId, landing: Landing },
    /// The run was deleted (docs/tasks/R1.md §3): its row, its tasks and its agents are gone.
    /// The event stays: the log only grows, and the trace outlives the run.
    #[serde(rename = "run.deleted")]
    RunDeleted { run_id: RunId },
    /// A run used `percent` of a budget cap (`what`: tokens, usd or minutes).
    #[serde(rename = "run.budget")]
    RunBudget {
        id: RunId,
        what: String,
        percent: u32,
    },
    /// The worktrees' build output is over its soft budget after reclaiming what nothing uses
    /// (docs/TODO.md §九 ②). A warning, not an action: what is in use is never deleted for it.
    #[serde(rename = "hub.build_cache")]
    BuildCache {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        used: u64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        limit: u64,
    },
    /// A shared note was written (`text`) or deleted (`None`).
    #[serde(rename = "note.written")]
    NoteWritten {
        scope: NoteScope,
        run_id: Option<RunId>,
        key: String,
        author: AgentId,
        text: Option<String>,
    },
    /// A reminder an agent set came due.
    #[serde(rename = "reminder.fired")]
    ReminderFired {
        owner: AgentId,
        reminder_id: String,
        note: String,
    },
    /// Something an agent watches happened.
    #[serde(rename = "watch.triggered")]
    WatchTriggered {
        owner: AgentId,
        watch_id: String,
        line: String,
    },
    #[serde(rename = "account.created")]
    AccountCreated { account: Account },
    #[serde(rename = "account.removed")]
    AccountRemoved { id: String },
    /// A login started with `POST /v1/accounts/{id}/login` finished.
    /// New quota figures for the account (from an agent's model call); re-read its status.
    #[serde(rename = "account.quota_updated")]
    AccountQuotaUpdated { id: String },
    #[serde(rename = "account.login_finished")]
    AccountLoginFinished {
        id: String,
        ok: bool,
        message: Option<String>,
    },
    /// A login is waiting for the human to open `url`.
    #[serde(rename = "account.login_pending")]
    AccountLoginPending { id: String, url: Option<String> },
    #[serde(rename = "work.created")]
    WorkCreated { work: crate::Work },
    #[serde(rename = "work.updated")]
    WorkUpdated { work: crate::Work },
    #[serde(rename = "run.created")]
    RunCreated { run: Run },
    #[serde(rename = "run.continuation_changed")]
    RunContinuationChanged {
        id: RunId,
        state: crate::model::ContinuationState,
    },
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
    /// The assignee's one-line progress (`task_progress`).
    #[serde(rename = "task.progress")]
    TaskProgress {
        run_id: RunId,
        id: TaskId,
        key: String,
        text: String,
    },
    /// The task went to another account or teammate than planned, because of quota
    /// (docs/tasks/DOGFOOD.md §5). `to` is the worker that now has it.
    #[serde(rename = "task.rerouted")]
    TaskRerouted {
        run_id: RunId,
        id: TaskId,
        key: String,
        to: AgentId,
        reason: String,
    },
    /// The task changed hands; `handoff` is the card, also stored on the task. Published
    /// before the new worker gets its first message.
    #[serde(rename = "task.handed_off")]
    TaskHandedOff {
        run_id: RunId,
        handoff: crate::handoff::Handoff,
    },
    /// A command ran in the task's worktree (`workspace_run`), on behalf of `by`.
    #[serde(rename = "task.command")]
    TaskCommand {
        run_id: RunId,
        id: TaskId,
        key: String,
        by: AgentId,
        command: String,
        exit_code: Option<i32>,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        duration_ms: u64,
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
    /// A CLI install or upgrade started (`action`: `install` or `upgrade`), with the exact
    /// argv the hub is about to run (docs/DESIGN.md §9.3). Only the description's commands
    /// are ever run: the request names a plugin and an action, never a command.
    #[serde(rename = "cli.install.started")]
    CliInstallStarted {
        plugin: String,
        action: String,
        argv: Vec<String>,
        /// Where the description that says how came from.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        described_by: Option<String>,
    },
    /// One line its output produced; `dropped` counts the lines past the cap that are not
    /// sent, so a chatty installer cannot flood the stream.
    #[serde(rename = "cli.install.output")]
    CliInstallOutput {
        plugin: String,
        action: String,
        line: String,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        dropped: u64,
    },
    /// It ended. `ok` is what the lookup after it found, not the exit code alone: a command
    /// that succeeded but left no CLI behind is a failure, and `reason` says why.
    #[serde(rename = "cli.install.finished")]
    CliInstallFinished {
        plugin: String,
        action: String,
        ok: bool,
        code: Option<i32>,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        reason: Option<String>,
        /// Lines past the cap that were dropped.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        dropped: u64,
    },
    /// A plugin's CLI changed in a way clients should re-read (`GET /v1/plugins`), after an
    /// install or an upgrade and the lookup that followed it.
    #[serde(rename = "plugin.changed")]
    PluginChanged { name: String },
    /// An extension changed in a way clients should re-read (`GET /v1/extensions`): its
    /// source directory was found changed by the hub's poll, a snapshot is waiting for
    /// approval, a version was enabled or disabled, or auto-trust was switched
    /// (docs/EXTENSIONS.md §5, §6). The state itself is not carried: the client reads it, as
    /// it does after `plugin.changed`.
    #[serde(rename = "extension.changed")]
    ExtensionChanged { name: String },
    /// An extension tool call ended. The result is not carried: it is fetched from
    /// `GET /v1/extensions/calls/{call_id}`, which keeps it for ten minutes
    /// (docs/EXTENSIONS.md §3).
    #[serde(rename = "extension.call.finished")]
    ExtensionCallFinished {
        /// The id the 202 answered with.
        call_id: String,
        extension: String,
        /// The bare tool name, as the description file names it.
        tool: String,
        /// The snapshot it ran on, which a version switch under it does not change.
        fingerprint: String,
        /// `ok`, `failed`, `cancelled` or `timed_out`
        /// (`crossagents_api_types::ExtensionCallStatus`).
        status: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        duration_ms: u64,
        /// Why it failed, for a status other than `ok`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        error: Option<String>,
    },
}

/// A message on its way to an agent. The id is fixed before delivery, so a sender can
/// refer to the message (and others reply to it) before the agent has even started.
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub id: MessageId,
    pub from: MessageSource,
    pub text: String,
    /// The message this one answers.
    pub reply_to: Option<MessageId>,
    /// Files sent with it.
    pub attachments: Vec<crate::Attachment>,
}

impl Envelope {
    pub fn new(from: MessageSource, text: String) -> Self {
        Self {
            id: MessageId::new(),
            from,
            text,
            reply_to: None,
            attachments: Vec::new(),
        }
    }
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
    /// The hub's digest of things the agent asked to hear about (reminders, watches,
    /// task events).
    Notice,
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
            Self::AgentSteered { .. } => "agent.steered",
            Self::AgentSteerRead { .. } => "agent.steer_read",
            Self::AgentInputUpdated { .. } => "agent.input_updated",
            Self::AgentModelChanged { .. } => "agent.model_changed",
            Self::ReminderFired { .. } => "reminder.fired",
            Self::HumanAsked { .. } => "human.asked",
            Self::NoteWritten { .. } => "note.written",
            Self::HumanAnswered { .. } => "human.answered",
            Self::HumanDismissed { .. } => "human.dismissed",
            Self::RunBudget { .. } => "run.budget",
            Self::BuildCache { .. } => "hub.build_cache",
            Self::RunLanded { .. } => "run.landed",
            Self::RunDeleted { .. } => "run.deleted",
            Self::WatchTriggered { .. } => "watch.triggered",
            Self::AgentTurnStarted { .. } => "agent.turn_started",
            Self::AgentOutput { .. } => "agent.output",
            Self::AgentToolCall { .. } => "agent.tool_call",
            Self::AgentToolCallUpdated { .. } => "agent.tool_call_updated",
            Self::AgentToolOutput { .. } => "agent.tool_output",
            Self::AgentToolDetail { .. } => "agent.tool_detail",
            Self::AgentTurnDiff { .. } => "agent.turn_diff",
            Self::AgentPermission { .. } => "agent.permission",
            Self::AgentRateLimits { .. } => "agent.rate_limits",
            Self::AgentWarning { .. } => "agent.warning",
            Self::AgentTurnCompleted { .. } => "agent.turn_completed",
            Self::AgentUsage { .. } => "agent.usage",
            Self::AgentContext { .. } => "agent.context",
            Self::AgentError { .. } => "agent.error",
            Self::AgentExited { .. } => "agent.exited",
            Self::AgentAdopted { .. } => "agent.adopted",
            Self::AgentSessionStarted { .. } => "agent.session_started",
            Self::AccountCreated { .. } => "account.created",
            Self::AccountRemoved { .. } => "account.removed",
            Self::AccountQuotaUpdated { .. } => "account.quota_updated",
            Self::AccountLoginFinished { .. } => "account.login_finished",
            Self::AccountLoginPending { .. } => "account.login_pending",
            Self::WorkCreated { .. } => "work.created",
            Self::WorkUpdated { .. } => "work.updated",
            Self::RunCreated { .. } => "run.created",
            Self::RunContinuationChanged { .. } => "run.continuation_changed",
            Self::RunStatusChanged { .. } => "run.status_changed",
            Self::RunPlanSubmitted { .. } => "run.plan_submitted",
            Self::TaskStatusChanged { .. } => "task.status_changed",
            Self::TaskChecked { .. } => "task.checked",
            Self::TaskProgress { .. } => "task.progress",
            Self::TaskRerouted { .. } => "task.rerouted",
            Self::TaskHandedOff { .. } => "task.handed_off",
            Self::TaskCommand { .. } => "task.command",
            Self::CliInstallStarted { .. } => "cli.install.started",
            Self::CliInstallOutput { .. } => "cli.install.output",
            Self::CliInstallFinished { .. } => "cli.install.finished",
            Self::PluginChanged { .. } => "plugin.changed",
            Self::ExtensionChanged { .. } => "extension.changed",
            Self::ExtensionCallFinished { .. } => "extension.call.finished",
        }
    }

    /// The run this event is about, if any.
    pub fn run_id(&self) -> Option<RunId> {
        match self {
            Self::RunCreated { run } => Some(run.id),
            Self::RunContinuationChanged { id, .. }
            | Self::RunStatusChanged { id, .. }
            | Self::RunPlanSubmitted { id, .. }
            | Self::RunBudget { id, .. }
            | Self::RunLanded { id, .. } => Some(*id),
            Self::RunDeleted { run_id } => Some(*run_id),
            Self::NoteWritten { run_id, .. } => *run_id,
            Self::HumanAsked { item } | Self::HumanAnswered { item } => item.run_id,
            Self::TaskStatusChanged { run_id, .. }
            | Self::TaskChecked { run_id, .. }
            | Self::TaskProgress { run_id, .. }
            | Self::TaskRerouted { run_id, .. }
            | Self::TaskHandedOff { run_id, .. }
            | Self::TaskCommand { run_id, .. } => Some(*run_id),
            _ => None,
        }
    }

    /// The agent this event is about, if any.
    pub fn agent_id(&self) -> Option<AgentId> {
        match self {
            Self::HubStarted { .. }
            | Self::HubStopping
            | Self::WorkCreated { .. }
            | Self::WorkUpdated { .. }
            | Self::RunCreated { .. }
            | Self::RunContinuationChanged { .. }
            | Self::RunStatusChanged { .. }
            | Self::RunPlanSubmitted { .. }
            | Self::TaskChecked { .. }
            | Self::TaskProgress { .. }
            | Self::AccountCreated { .. }
            | Self::AccountRemoved { .. }
            | Self::AccountQuotaUpdated { .. }
            | Self::HumanDismissed { .. }
            | Self::RunBudget { .. }
            | Self::BuildCache { .. }
            | Self::RunLanded { .. }
            | Self::RunDeleted { .. }
            | Self::AccountLoginFinished { .. }
            | Self::AccountLoginPending { .. }
            | Self::CliInstallStarted { .. }
            | Self::CliInstallOutput { .. }
            | Self::CliInstallFinished { .. }
            | Self::PluginChanged { .. }
            | Self::ExtensionChanged { .. }
            // The caller is in the call record, not here: an extension's panel calls its
            // tools as the person, with no agent to attribute them to.
            | Self::ExtensionCallFinished { .. } => None,
            Self::AgentSessionStarted { id, .. } => Some(*id),
            Self::ReminderFired { owner, .. } | Self::WatchTriggered { owner, .. } => Some(*owner),
            Self::NoteWritten { author, .. } => Some(*author),
            Self::HumanAsked { item } | Self::HumanAnswered { item } => Some(item.from),
            Self::TaskStatusChanged { assignee, .. } => *assignee,
            Self::TaskCommand { by, .. } => Some(*by),
            Self::TaskRerouted { to, .. } => Some(*to),
            Self::TaskHandedOff { handoff, .. } => Some(handoff.to),
            Self::AgentCreated { agent } | Self::AgentUpdated { agent } => Some(agent.id),
            Self::AgentRemoved { id, .. }
            | Self::AgentStatusChanged { id, .. }
            | Self::AgentInput { id, .. }
            | Self::AgentInputWithdrawn { id, .. }
            | Self::AgentSteered { id, .. }
            | Self::AgentSteerRead { id, .. }
            | Self::AgentInputUpdated { id, .. }
            | Self::AgentModelChanged { id, .. }
            | Self::AgentTurnStarted { id, .. }
            | Self::AgentOutput { id, .. }
            | Self::AgentToolCall { id, .. }
            | Self::AgentToolCallUpdated { id, .. }
            | Self::AgentToolOutput { id, .. }
            | Self::AgentToolDetail { id, .. }
            | Self::AgentTurnDiff { id, .. }
            | Self::AgentPermission { id, .. }
            | Self::AgentRateLimits { id, .. }
            | Self::AgentWarning { id, .. }
            | Self::AgentTurnCompleted { id, .. }
            | Self::AgentUsage { id, .. }
            | Self::AgentContext { id, .. }
            | Self::AgentExited { id, .. }
            | Self::AgentAdopted { id, .. }
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
            EventKind::ExtensionChanged {
                name: "standup".into(),
            },
            EventKind::ExtensionCallFinished {
                call_id: "c1".into(),
                extension: "standup".into(),
                tool: "report".into(),
                fingerprint: "f1".into(),
                status: "ok".into(),
                duration_ms: 12,
                error: None,
            },
        ];
        for kind in kinds {
            let value = serde_json::to_value(&kind).unwrap();
            assert_eq!(value["type"], kind.type_name());
        }
    }

    /// Extension events are about an extension, not an agent or a run (docs/EXTENSIONS.md §5).
    #[test]
    fn extension_events_belong_to_no_run_or_agent() {
        let changed = EventKind::ExtensionChanged {
            name: "standup".into(),
        };
        assert_eq!(changed.type_name(), "extension.changed");
        assert_eq!(changed.run_id(), None);
        assert_eq!(changed.agent_id(), None);

        let finished = EventKind::ExtensionCallFinished {
            call_id: "c1".into(),
            extension: "standup".into(),
            tool: "report".into(),
            fingerprint: "f1".into(),
            status: "timed_out".into(),
            duration_ms: 60_000,
            error: Some("timed out after 60s".into()),
        };
        let value = serde_json::to_value(&finished).unwrap();
        assert_eq!(value["type"], "extension.call.finished");
        assert_eq!(value["data"]["call_id"], "c1");
        assert_eq!(value["data"]["status"], "timed_out");
        assert_eq!(finished.agent_id(), None);
    }
}
