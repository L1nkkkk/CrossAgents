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

/// Why a message waits for its turn instead of starting one, or why a steer attempt
/// delivered nothing (docs/tasks/DELIVERY-AUDIT.md §3). Clients show a sentence per code
/// and the raw code for one they do not know, so variants are only ever added, never
/// renamed or removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum QueuedReason {
    /// A turn runs; the message waits behind it.
    TurnRunning,
    /// A person's message is already waiting ahead of this one.
    OwnerQueueAhead,
    /// The agent runs outside the hub; it takes its messages when it asks for them.
    ExternalAgent,
    /// A steer was asked for, but the driver cannot take one.
    RuntimeNoSteer,
    /// A steer was asked for, but no turn runs (it just ended).
    NoActiveTurn,
    /// A steer was asked for, but writing it into the CLI failed: nothing was delivered
    /// and nothing is queued; the sender decides whether to send it again.
    SteerWriteFailed,
    /// A steer was written but the CLI did not answer in time: it may or may not have
    /// arrived. Nothing is queued and it is not sent again; the sender is told the outcome
    /// is unknown (docs/tasks/DELIVERY-AUDIT.md §3).
    OutcomeUnknown,
    /// The conversation is archived.
    Archived,
    /// The message already started; it cannot be taken back or moved.
    AlreadyStarted,
    /// The agent or its driver refused the files sent with it.
    AttachmentRefused,
    /// Nothing was sent: no text and no files.
    Empty,
}

/// What a session is doing with messages right now (docs/tasks/DELIVERY-AUDIT.md §8 P1-4).
/// The hub answers this from the inbox that admits messages, so clients do not work "busy"
/// out of process status, started turns or a queue count of their own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    /// Nothing runs and nothing waits: a message would start a turn at once.
    #[default]
    Idle,
    /// A message waits for its turn to begin: the session is being started, or the last
    /// turn just ended and the next one has not been taken yet.
    Starting,
    /// A turn runs; `DeliveryStatus::queued` says how many messages wait behind it.
    Turn,
    /// The agent runs outside the hub: it takes its messages when it asks for them.
    Held,
}

/// A session's delivery state and how many messages have not started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DeliveryStatus {
    pub state: DeliveryState,
    /// Messages that have not started: those waiting behind the running turn, or, in
    /// `starting`, those waiting for their turn to begin.
    pub queued: usize,
}

/// One account weighed for a dispatch, with why it ranks where it does: the accounts are
/// listed best first (magpie-lancer §2.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AccountCandidate {
    pub account: String,
    /// Why it is where it is in the order, in words.
    pub reason: String,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", content = "data")]
pub enum EventKind {
    #[serde(rename = "device.created")]
    DeviceCreated { device: crate::Device },
    #[serde(rename = "device.revoked")]
    DeviceRevoked { device: crate::Device },
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
    /// What the agent's session does with messages now: the same [`DeliveryStatus`] the
    /// transcript carries, so clients need not work it out from other events
    /// (docs/tasks/DELIVERY-AUDIT.md §8 P1-4).
    #[serde(rename = "agent.delivery_changed")]
    AgentDeliveryChanged {
        id: AgentId,
        delivery: DeliveryStatus,
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
        /// Why it waits, when `queued`; absent when it starts a turn at once.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        queued_reason: Option<QueuedReason>,
    },
    /// A message was not delivered: a steer asked for could not be written into the CLI.
    /// Nothing waits for it and nothing started; the sender decides whether to send it
    /// again (docs/tasks/DELIVERY-AUDIT.md §3).
    #[serde(rename = "agent.input_refused")]
    AgentInputRefused {
        id: AgentId,
        message_id: MessageId,
        text: String,
        reason: QueuedReason,
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
        /// Who sent it; absent in events written before the field existed (a person).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        from: Option<MessageSource>,
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
    /// The notices a message carries from now on: written when the hub marks them carried, before
    /// the message is delivered (round accounting reads them; `AgentInput.notices` is unchanged).
    #[serde(rename = "notice.carried")]
    NoticeCarried {
        id: AgentId,
        message_id: MessageId,
        notices: Vec<CarriedNotice>,
    },
    /// A hub tool the agent called, once it returned (`ok`: it succeeded). The hub's round
    /// accounting reads these; they are not shown as work.
    #[serde(rename = "agent.tool_used")]
    AgentToolUsed { id: AgentId, tool: String, ok: bool },
    /// A prompt went to the agent as a file it reads, because it could not go on the command
    /// line (`argv::prompt_via_file`).
    #[serde(rename = "agent.prompt_via_file")]
    AgentPromptViaFile { id: AgentId },
    /// One agent turn, written when it ends: what started it and what joined it, what the
    /// turn used and whether it decided something.
    #[serde(rename = "agent.round")]
    AgentRound {
        id: AgentId,
        /// The message that started the turn, then any steered in during it.
        message_ids: Vec<MessageId>,
        sources: Vec<RoundSource>,
        notices: Vec<RoundNotice>,
        /// The turn's model usage: the growth of the session's cumulative counts. Null when
        /// the agent reported no usage during it. A request count the agent did not report
        /// is null inside.
        tokens: Option<TokenUsage>,
        /// The hub tools called, by name, each once.
        tools: Vec<String>,
        /// A tool that changes state (see `hub::rounds::decides`) succeeded.
        decided: bool,
        /// The prompt was handed over as a file this turn.
        prompt_via_file: bool,
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
    /// A task passed one cap of its budget (`what`: usd, tokens, requests, rework, minutes or
    /// idle), in that cap's unit. Usage is reported when a turn ends, so the cap may have been
    /// passed a turn earlier.
    #[serde(rename = "task.budget")]
    TaskBudget {
        run_id: RunId,
        id: TaskId,
        key: String,
        what: String,
        used: f64,
        max: f64,
    },
    /// The reviewer a review round of the task went to. Its usage counts for the task from then.
    #[serde(rename = "task.reviewer")]
    TaskReviewer {
        run_id: RunId,
        id: TaskId,
        key: String,
        reviewer: AgentId,
    },
    /// How long the task's acceptance checks took in all (`task.checked` carries the results).
    #[serde(rename = "task.check_timed")]
    TaskCheckTimed {
        run_id: RunId,
        id: TaskId,
        key: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        duration_ms: u64,
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
    /// A problem was recorded: a run note under `issue/` was written (derived from
    /// `note.written`, docs/tasks/LESSONS.md §4.6).
    #[serde(rename = "issue.recorded")]
    IssueRecorded {
        run_id: RunId,
        key: String,
        author: AgentId,
    },
    /// Recorded problems were sorted: into a lesson, merged into one, or kept in the run.
    #[serde(rename = "issue.triaged")]
    IssueTriaged {
        run_id: RunId,
        records: Vec<crate::lesson::TriageRecord>,
    },
    #[serde(rename = "lesson.created")]
    LessonCreated { lesson: crate::lesson::Lesson },
    /// A lesson changed: evidence added, a new resolution or status, or a person's edit.
    #[serde(rename = "lesson.updated")]
    LessonUpdated { lesson: crate::lesson::Lesson },
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
    /// A notice was recorded for an agent, with the class that decides whether it wakes
    /// them now. Replay and round accounting read this; delivery is a separate message.
    #[serde(rename = "notice.recorded")]
    NoticeRecorded {
        id: AgentId,
        topic: String,
        line: String,
        class: crate::NoticeClass,
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
    RunCreated { run: Box<Run> },
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
    /// A worker was dispatched for the task: the same-vendor accounts weighed, best first,
    /// and the one it got. Sent for every dispatch, not only when quota moved it
    /// (magpie-lancer §2.8).
    #[serde(rename = "task.routed")]
    TaskRouted {
        run_id: RunId,
        id: TaskId,
        key: String,
        /// The account the worker runs on; `None` when routing knew no account.
        #[serde(default)]
        account: Option<String>,
        #[serde(default)]
        candidates: Vec<AccountCandidate>,
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

/// Who sent a message that started or joined a round (see [`EventKind::AgentRound`]).
/// Unlike [`MessageSource`], a notice that a reminder woke is its own kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoundSource {
    /// A person.
    Human,
    /// The hub's digest of things the agent asked to hear about.
    Notice,
    /// A reminder or watch the agent set fired.
    Reminder,
    /// Another agent (`agent_send`).
    Teammate { agent_id: AgentId },
    /// The orchestrator, about a run and possibly one of its tasks.
    Orchestrator {
        run_id: RunId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        task_id: Option<TaskId>,
    },
}

/// A notice a round took on: its line and class, as the hub had them when the message took it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RoundNotice {
    pub line: String,
    pub class: crate::NoticeClass,
}

/// A notice a message carries (`notice.carried`): its line, class and topic. The topic says
/// where it came from (a reminder or a watch, or something else).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CarriedNotice {
    pub line: String,
    pub class: crate::NoticeClass,
    pub topic: String,
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
            Self::DeviceCreated { .. } => "device.created",
            Self::DeviceRevoked { .. } => "device.revoked",
            Self::HubStarted { .. } => "hub.started",
            Self::HubStopping => "hub.stopping",
            Self::AgentCreated { .. } => "agent.created",
            Self::AgentUpdated { .. } => "agent.updated",
            Self::AgentRemoved { .. } => "agent.removed",
            Self::AgentStatusChanged { .. } => "agent.status_changed",
            Self::AgentDeliveryChanged { .. } => "agent.delivery_changed",
            Self::AgentInput { .. } => "agent.input",
            Self::AgentInputRefused { .. } => "agent.input_refused",
            Self::AgentInputWithdrawn { .. } => "agent.input_withdrawn",
            Self::AgentSteered { .. } => "agent.steered",
            Self::AgentSteerRead { .. } => "agent.steer_read",
            Self::AgentInputUpdated { .. } => "agent.input_updated",
            Self::AgentModelChanged { .. } => "agent.model_changed",
            Self::ReminderFired { .. } => "reminder.fired",
            Self::HumanAsked { .. } => "human.asked",
            Self::NoteWritten { .. } => "note.written",
            Self::IssueRecorded { .. } => "issue.recorded",
            Self::IssueTriaged { .. } => "issue.triaged",
            Self::LessonCreated { .. } => "lesson.created",
            Self::LessonUpdated { .. } => "lesson.updated",
            Self::HumanAnswered { .. } => "human.answered",
            Self::HumanDismissed { .. } => "human.dismissed",
            Self::RunBudget { .. } => "run.budget",
            Self::TaskBudget { .. } => "task.budget",
            Self::TaskReviewer { .. } => "task.reviewer",
            Self::TaskCheckTimed { .. } => "task.check_timed",
            Self::BuildCache { .. } => "hub.build_cache",
            Self::RunLanded { .. } => "run.landed",
            Self::RunDeleted { .. } => "run.deleted",
            Self::WatchTriggered { .. } => "watch.triggered",
            Self::NoticeRecorded { .. } => "notice.recorded",
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
            Self::NoticeCarried { .. } => "notice.carried",
            Self::AgentToolUsed { .. } => "agent.tool_used",
            Self::AgentPromptViaFile { .. } => "agent.prompt_via_file",
            Self::AgentRound { .. } => "agent.round",
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
            Self::TaskRouted { .. } => "task.routed",
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
            Self::IssueRecorded { run_id, .. } | Self::IssueTriaged { run_id, .. } => Some(*run_id),
            Self::HumanAsked { item } | Self::HumanAnswered { item } => item.run_id,
            Self::TaskStatusChanged { run_id, .. }
            | Self::TaskChecked { run_id, .. }
            | Self::TaskProgress { run_id, .. }
            | Self::TaskRerouted { run_id, .. }
            | Self::TaskRouted { run_id, .. }
            | Self::TaskHandedOff { run_id, .. }
            | Self::TaskCommand { run_id, .. } => Some(*run_id),
            _ => None,
        }
    }

    /// The agent this event is about, if any.
    pub fn agent_id(&self) -> Option<AgentId> {
        match self {
            Self::DeviceCreated { .. }
            | Self::DeviceRevoked { .. }
            | Self::HubStarted { .. }
            | Self::HubStopping
            | Self::WorkCreated { .. }
            | Self::WorkUpdated { .. }
            | Self::RunCreated { .. }
            | Self::RunContinuationChanged { .. }
            | Self::RunStatusChanged { .. }
            | Self::RunPlanSubmitted { .. }
            | Self::TaskChecked { .. }
            | Self::TaskProgress { .. }
            | Self::TaskRouted { .. }
            | Self::TaskBudget { .. }
            | Self::TaskCheckTimed { .. }
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
            | Self::IssueTriaged { .. }
            | Self::LessonCreated { .. }
            | Self::LessonUpdated { .. }
            // The caller is in the call record, not here: an extension's panel calls its
            // tools as the person, with no agent to attribute them to.
            | Self::ExtensionCallFinished { .. } => None,
            Self::AgentSessionStarted { id, .. } => Some(*id),
            Self::TaskReviewer { reviewer, .. } => Some(*reviewer),
            Self::ReminderFired { owner, .. } | Self::WatchTriggered { owner, .. } => Some(*owner),
            Self::NoticeRecorded { id, .. } => Some(*id),
            Self::NoteWritten { author, .. } => Some(*author),
            Self::IssueRecorded { author, .. } => Some(*author),
            Self::HumanAsked { item } | Self::HumanAnswered { item } => Some(item.from),
            Self::TaskStatusChanged { assignee, .. } => *assignee,
            Self::TaskCommand { by, .. } => Some(*by),
            Self::TaskRerouted { to, .. } => Some(*to),
            Self::TaskHandedOff { handoff, .. } => Some(handoff.to),
            Self::AgentCreated { agent } | Self::AgentUpdated { agent } => Some(agent.id),
            Self::AgentRemoved { id, .. }
            | Self::AgentStatusChanged { id, .. }
            | Self::AgentDeliveryChanged { id, .. }
            | Self::AgentInput { id, .. }
            | Self::AgentInputRefused { id, .. }
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
            | Self::NoticeCarried { id, .. }
            | Self::AgentToolUsed { id, .. }
            | Self::AgentPromptViaFile { id, .. }
            | Self::AgentRound { id, .. }
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
            EventKind::AgentToolUsed {
                id: AgentId::new(),
                tool: "task_complete".into(),
                ok: true,
            },
            EventKind::AgentPromptViaFile { id: AgentId::new() },
            EventKind::AgentRound {
                id: AgentId::new(),
                message_ids: vec![],
                sources: vec![RoundSource::Reminder],
                notices: vec![RoundNotice {
                    line: "check".into(),
                    class: crate::NoticeClass::Decide,
                }],
                tokens: None,
                tools: vec![],
                decided: false,
                prompt_via_file: false,
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
