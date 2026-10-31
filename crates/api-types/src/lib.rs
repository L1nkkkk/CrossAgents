//! Request, response and error types of the hub's local API (docs/DESIGN.md §11).
//!
//! Running `cargo test -p crossagents-api-types` regenerates the TypeScript bindings in
//! `bindings/` for GUI clients.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub use crossagents_core::{
    Account, AccountStatus, Agent, AgentCommand, AgentCommandKind, AgentId, AgentProfile,
    AgentStatus, Capabilities, CheckResult, ClaimRole, ContinuationPolicy, DataSource, DiffSource,
    DriverKind, Event, EventKind, Handoff, HandoffKind, MessageId, MessageSource, ModelInfo,
    OutputStream, PlannedTask, Role, Run, RunId, RunPolicy, RunStatus, StopReason, Task, TaskId,
    TaskStatus, TokenTotals, ToolInfo, ToolStatus,
};

/// Handoff cards and how a task's chain of them reads.
pub use crossagents_core::handoff;

pub const API_VERSION: &str = "v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
/// `GET /v1/health`: answers without a token, so it says as little as possible. Version,
/// uptime and pid are behind the token, in `HubInfo` (`GET /v1/info`).
pub struct HealthResponse {
    /// Always `"ok"` when the hub answers.
    pub status: String,
    /// The API version, so a client can tell an incompatible hub apart from a silent one.
    pub api: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HubInfo {
    pub version: String,
    /// The commit the hub was built from (`d5011e4`), when it was built in a git checkout.
    #[serde(default)]
    #[ts(optional)]
    pub commit: Option<String>,
    /// Built without optimizations (`cargo build`), i.e. a development build.
    #[serde(default)]
    pub debug: bool,
    pub api: String,
    pub pid: u32,
    pub home: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateAgentRequest {
    /// What people and teammates call it (e.g. "Luna"); a taken name gets an ordinal
    /// ("Luna 2"). Absent: the hub makes one up (`name_source: generated`).
    #[serde(default)]
    #[ts(optional)]
    pub name: Option<String>,
    /// A conversation's title (chat mode), e.g. its first message.
    #[serde(default)]
    #[ts(optional)]
    pub topic: Option<String>,
    pub plugin: String,
    #[serde(default)]
    pub driver: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub role: Role,
    /// The agent's working directory. Empty: a new one under the hub home (`chats/<name>`),
    /// for a chat outside any project.
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub config: serde_json::Value,
    /// Display name, title, avatar and persona; all optional.
    #[serde(default)]
    pub profile: crossagents_core::AgentProfile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListAgentsResponse {
    pub agents: Vec<Agent>,
}

/// `POST /v1/agents/{key}/name`: a person renames the agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RenameAgentRequest {
    pub name: String,
}

/// `POST /v1/agents/{key}/topic`: the conversation's title; empty or absent clears it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SetTopicRequest {
    #[serde(default)]
    pub topic: Option<String>,
}

/// `POST /v1/agents/{key}/archive`: archive the conversation (and its recruits) or restore it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ArchiveAgentRequest {
    pub archived: bool,
}

/// `POST /v1/agents/{key}/settings` (docs/tasks/DOGFOOD.md §6). `setting_sources`: which
/// of Claude Code's settings it loads, comma-separated from `user`, `project`, `local`
/// (empty: none); `None` goes back to the default (`project,local`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentSettingsRequest {
    pub setting_sources: Option<String>,
}

/// `POST /v1/agents/{key}/model`. `None` leaves the model to the agent's own default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SetModelRequest {
    pub model: Option<String>,
    /// Reasoning effort for the following turns (e.g. "low", "high"); absent keeps the
    /// current one, empty goes back to the model's default. Kept in `config.effort`.
    #[serde(default)]
    #[ts(optional)]
    pub effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SendMessageRequest {
    pub text: String,
    /// Put the message ahead of everything waiting and interrupt the running turn.
    #[serde(default)]
    #[ts(optional)]
    pub now: Option<bool>,
    /// Files sent with the message (docs/tasks/DOGFOOD.md §3): at most
    /// [`MAX_ATTACHMENTS`], each at most [`MAX_ATTACHMENT_BYTES`], of [`attachment_mime_ok`]
    /// types.
    #[serde(default)]
    #[ts(optional)]
    pub attachments: Option<Vec<AttachmentUpload>>,
}

/// Limits on a message's files, the same in the hub and the GUI.
pub const MAX_ATTACHMENTS: usize = 8;
pub const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_ATTACHMENTS_TOTAL: usize = 20 * 1024 * 1024;

/// Whether a file of this type can be sent with a message.
pub fn attachment_mime_ok(mime: &str) -> bool {
    matches!(
        mime,
        "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "application/pdf"
            | "application/json"
    ) || mime.starts_with("text/")
}

/// A file as the client sends it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AttachmentUpload {
    pub name: String,
    pub mime: String,
    /// The bytes, base64 (standard alphabet, padded).
    pub data: String,
}

/// `GET /v1/agents/{key}/attachments/{id}`: one file sent to the agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AttachmentContent {
    pub id: String,
    pub mime: String,
    #[ts(type = "number")]
    pub size: u64,
    /// The bytes, base64.
    pub data: String,
}

/// `GET /v1/agents/{key}/files/image?path=`: an image a tool call named by a path (a
/// `ViewImage` tool's `image_path`, say), for a client to show as a thumbnail. The bytes are
/// base64, as with attachments, so a page that only carries text (the Electron bridge) can
/// show them as a `data:` URL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ImageContent {
    /// Where the image is, as the hub read it (absolute).
    pub path: String,
    pub mime: String,
    #[ts(type = "number")]
    pub size: u64,
    /// The bytes, base64 (standard alphabet, padded).
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SendMessageResponse {
    /// Identifies the turn that answers this message in `agent.turn_*` events.
    pub message_id: crossagents_core::MessageId,
    /// `true` if the message waits behind the agent's current work.
    pub queued: bool,
}

/// A session's conversation, rebuilt from its events and grouped by turn.
/// `GET /v1/agents/{key}/transcript`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Transcript {
    pub agent: Agent,
    /// The driver the agent runs on; `None` if its plugin is no longer configured.
    pub driver: Option<DriverKind>,
    /// What that driver can do; clients show operations by these flags.
    pub capabilities: Option<Capabilities>,
    /// Oldest first. A message still waiting for its turn is the last turn, not started.
    pub turns: Vec<TranscriptTurn>,
    /// Latest context usage the agent reported.
    pub usage: Option<TranscriptUsage>,
    /// Each switch of model or reasoning effort, oldest first; clients show them between the
    /// turns by time.
    #[serde(default)]
    pub switches: Vec<ModelSwitch>,
    /// What the conversation has spent: this agent and every teammate it recruited (chat
    /// mode), as far as they report cost. `None` when none of them reports any.
    #[serde(default)]
    #[ts(optional)]
    pub conversation_cost_usd: Option<f64>,
    /// Why the agent is in error (its latest `agent.error`), while it is; also when it failed
    /// before any turn, e.g. because its CLI could not be started.
    #[serde(default)]
    #[ts(optional)]
    pub error: Option<String>,
    /// How its process ended, when that is the `error` (its `agent.exited`).
    #[serde(default)]
    #[ts(optional)]
    pub exit: Option<AgentExit>,
    /// The last event folded in; follow `GET /v1/events?agent=…&since=<last_seq>` from here.
    #[ts(type = "number")]
    pub last_seq: u64,
}

/// How an agent's process ended (`agent.exited`); either part is missing when the driver
/// cannot tell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentExit {
    pub cause: Option<crossagents_core::ExitCause>,
    pub code: Option<i32>,
}

/// The model or reasoning effort an agent runs on was switched (`agent.model_changed`);
/// `None` is the plugin's or model's default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelSwitch {
    pub at: DateTime<Utc>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub from_model: Option<String>,
    pub from_effort: Option<String>,
}

/// One message to the session and everything the agent did in answer to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TranscriptTurn {
    pub message_id: MessageId,
    pub input: String,
    /// Who sent the message; `None` for messages recorded before senders were.
    pub from: Option<MessageSource>,
    /// The message this one answers, the conversation it belongs to, and the start of the
    /// message it answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reply_to: Option<MessageId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub thread: Option<MessageId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reply_excerpt: Option<String>,
    pub sent_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Set once the turn has ended.
    pub stop_reason: Option<StopReason>,
    /// In arrival order. Consecutive output of the same stream is merged into one item.
    pub items: Vec<TranscriptItem>,
    /// What the turn changed, file by file; the diff itself is at
    /// `GET /v1/agents/{key}/turns/{message_id}/diff`. Absent for turns that changed nothing
    /// (or whose changes no one could see).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub diff: Option<TurnDiffStat>,
    /// Files sent with the message; their contents at
    /// `GET /v1/agents/{key}/attachments/{id}`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<crossagents_core::Attachment>>", optional)]
    pub attachments: Vec<crossagents_core::Attachment>,
}

/// Files a turn changed, with added and removed lines.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TurnDiffStat {
    pub files: Vec<FileStat>,
    pub added: u32,
    pub removed: u32,
    pub source: DiffSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FileStat {
    pub path: String,
    pub added: u32,
    pub removed: u32,
}

impl TurnDiffStat {
    /// Counts a unified diff (`git diff` output, one or more files). A bare hunk list without
    /// `diff --git` headers counts as one file named after its `+++`/`---` lines.
    pub fn from_diff(diff: &str, source: DiffSource) -> Self {
        let mut files: Vec<FileStat> = Vec::new();
        // In a file's header (before its first hunk), `---`/`+++` name files, not lines.
        let mut header = true;
        let new_file = |files: &mut Vec<FileStat>, path: String| {
            files.push(FileStat {
                path,
                added: 0,
                removed: 0,
            })
        };
        for line in diff.lines() {
            if let Some(rest) = line.strip_prefix("diff --git ") {
                let path = rest.rsplit_once(" b/").map_or(rest, |(_, b)| b).to_owned();
                new_file(&mut files, path);
                header = true;
                continue;
            }
            if header && (line.starts_with("+++ ") || line.starts_with("--- ")) {
                let path = line[4..].trim_start_matches("a/").trim_start_matches("b/");
                if files.is_empty() {
                    new_file(&mut files, String::new());
                }
                let file = files.last_mut().expect("just pushed");
                if file.path.is_empty() && path != "/dev/null" {
                    file.path = path.to_owned();
                }
                continue;
            }
            if line.starts_with("@@") {
                header = false;
                if files.is_empty() {
                    new_file(&mut files, String::new());
                }
                continue;
            }
            if header {
                continue;
            }
            let Some(file) = files.last_mut() else {
                continue;
            };
            if line.starts_with('+') {
                file.added += 1;
            } else if line.starts_with('-') {
                file.removed += 1;
            }
        }
        files.retain(|f| !f.path.is_empty() || f.added + f.removed > 0);
        Self {
            added: files.iter().map(|f| f.added).sum(),
            removed: files.iter().map(|f| f.removed).sum(),
            files,
            source,
        }
    }
}

/// `GET /v1/agents/{key}/turns/{message_id}/diff`: everything one turn changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TurnDiffResponse {
    pub message_id: MessageId,
    /// Unified diff.
    pub diff: String,
    pub stat: TurnDiffStat,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptItem {
    Output {
        stream: OutputStream,
        text: String,
    },
    ToolCall {
        tool_call_id: String,
        title: String,
        kind: Option<String>,
        status: ToolStatus,
        /// Live output while it runs (e.g. a command's stdout).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        output: Option<String>,
        /// What it did, once the agent reports it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        detail: Option<Box<crossagents_core::ToolDetail>>,
    },
    /// Asked before acting; `decision` is what the permission policy answered.
    Permission {
        kind: String,
        title: String,
        decision: crossagents_core::PermissionDecision,
    },
    /// Worth showing, but nothing failed.
    Warning {
        message: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TranscriptUsage {
    /// Tokens in the context window.
    #[ts(type = "number")]
    pub used: u64,
    /// Size of the context window.
    #[ts(type = "number")]
    pub size: u64,
    pub cost_usd: Option<f64>,
    /// Cumulative token counts of the session, when the agent reports them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tokens: Option<crossagents_core::TokenUsage>,
    /// What fills the window, by kind, as of the agent's last turn (Claude Code only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parts: Option<Vec<crossagents_core::ContextPart>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginInfo {
    pub name: String,
    /// The models offered: [`Self::all_models`] narrowed to those picked on the settings
    /// page (all of them when none are).
    pub models: Vec<String>,
    /// Every model it can run: what its logins report, else its configured or built-in
    /// list.
    #[serde(default)]
    pub all_models: Vec<String>,
    /// The models picked on the settings page; `None` when all are offered.
    #[serde(default)]
    pub shown: Option<Vec<String>>,
    /// `all_models` is the list set in `config.toml`, not what the logins report; each
    /// login is then offered this list too.
    #[serde(default)]
    pub models_pinned: bool,
    /// The driver its agents use unless they name another.
    pub driver: crossagents_core::DriverKind,
    pub capabilities: crossagents_core::Capabilities,
    /// Reasoning effort levels its agents may ask for, lowest first; empty when none.
    #[serde(default)]
    pub efforts: Vec<String>,
    /// Where its CLI is on this machine, for native drivers; `None` for ACP agents
    /// (docs/DESIGN.md §9.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cli: Option<CliInfo>,
}

/// Where a native driver's CLI is on this machine, and how to install it when it is not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CliInfo {
    pub installed: bool,
    /// The file that was found: the CLI's own entry point, even when it is run through
    /// `node`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    /// Where it came from: `config`, `env`, `path`, `native` or `bundled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source: Option<String>,
    /// What the CLI printed for its version; `None` while it has not answered yet (the first
    /// probe runs in the background) or when it did not answer in time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub version: Option<String>,
    /// The command that installs it (shown, never run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub install_hint: Option<String>,
    /// Where to read more about installing and signing in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub docs: Option<String>,
    /// The install method that put it here (`native`, `npm`), when its path says which one
    /// did (docs/DESIGN.md §9.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub installed_via: Option<String>,
    /// The command this machine would run to install it, as argv: what the confirmation box
    /// shows before anything runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub install_command: Option<Vec<String>>,
    /// The command this machine would run to upgrade it, as argv; absent when it cannot be
    /// upgraded here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub upgrade_command: Option<Vec<String>>,
    /// Whether the GUI offers an upgrade button.
    #[serde(default)]
    pub can_upgrade: bool,
    /// Where the description came from: `built-in`, a manifest's path, or `config.toml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub described_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListPluginsResponse {
    pub plugins: Vec<PluginInfo>,
}

/// `POST /v1/plugins/{name}/install` and `.../upgrade`: accepted, running in the background.
/// How it went arrives on the event stream (`cli.install.*`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CliInstallAccepted {
    pub plugin: String,
    /// `install` or `upgrade`.
    pub action: String,
    /// The exact argv being run: the same one `cli.install.started` carries.
    pub argv: Vec<String>,
}

/// `POST /v1/plugins/{name}/models`: the models to offer; `null` offers them all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SetPluginModelsRequest {
    pub shown: Option<Vec<String>>,
}

/// Starts a run. Agents are given by name or id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateRunRequest {
    pub request: String,
    #[serde(default)]
    pub title: Option<String>,
    pub planner: String,
    /// Omitted retains the existing adaptive lead selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fresh: Option<bool>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restore: bool,
    /// Template for workers. Omitted: the lead's own model, cloned (docs/tasks/W1.md).
    #[serde(default)]
    #[ts(optional)]
    pub worker: Option<String>,
    #[serde(default)]
    pub escalate: Option<String>,
    #[serde(default)]
    pub reviewer: Option<String>,
    #[serde(default)]
    pub auto_approve: bool,
    #[serde(default)]
    pub max_parallel: Option<u32>,
    #[serde(default)]
    pub max_attempts: Option<u32>,
    /// Teammates the planner may assign tasks to, by name. Omitted: every worker and
    /// reviewer registered in the planner's repository.
    #[serde(default)]
    pub team: Option<Vec<String>>,
    /// How much the run may do without a person; the hub's defaults when omitted.
    /// `auto_approve: true` still means `plan_approval: auto`.
    #[serde(default)]
    #[ts(optional)]
    pub policy: Option<crossagents_core::RunPolicy>,
    /// Files that come with the request (screenshots, specs), handed to the lead with its
    /// kickoff; the same limits as [`SendMessageRequest::attachments`].
    #[serde(default)]
    #[ts(optional)]
    pub attachments: Option<Vec<AttachmentUpload>>,
}

/// A workspace session; availability is advisory and can change before use.
/// Only the atomic claim_agent insertion provides exclusion; chat does not claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionCandidate {
    pub agent: Agent,
    pub state: String,
    pub busy_with: Option<String>,
    pub last_execution: Option<String>,
    /// Whether the session can be selected; archived sessions can be selected with restore: true.
    pub can_use: bool,
    /// busy and cannot_move block use; archived requires restore: true; null needs no action.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListSessionCandidatesResponse {
    pub sessions: Vec<SessionCandidate>,
}

/// Continue an existing conversation with a new human message and topic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StartChatRequest {
    pub text: String,
    #[serde(default)]
    pub restore: bool,
    /// Files sent with the message, as [`SendMessageRequest::attachments`].
    #[serde(default)]
    #[ts(optional)]
    pub attachments: Option<Vec<AttachmentUpload>>,
}

/// `POST /v1/repos/init`: put a directory under version control (`git init` and a first
/// commit), so runs there get worktrees instead of working in place. Answered with itself.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InitRepoRequest {
    pub path: String,
}

/// `GET /v1/runs/{key}/tasks/{task}/diff` and `GET /v1/runs/{key}/diff`: a unified diff
/// and what it compares (docs/tasks/DOGFOOD.md §2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ChangesResponse {
    /// What the changes are compared with: the task's base branch, or the run's base.
    pub base: String,
    /// The task's or the run's branch.
    pub head: String,
    pub diff: String,
    /// The diff was longer than the hub hands out at once and was cut.
    pub truncated: bool,
}

/// Which side of a diff a line number counts on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum DiffSide {
    #[default]
    New,
    Old,
}

/// One remark on a task's changes: a line of a file, or the whole file without `line`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ReviewComment {
    pub path: String,
    #[serde(default)]
    #[ts(optional)]
    pub line: Option<u32>,
    #[serde(default)]
    #[ts(optional)]
    pub side: Option<DiffSide>,
    pub text: String,
}

/// `POST /v1/runs/{key}/tasks/{task}/review`: the person's comments, delivered to the task's
/// worker as its next turn; with `rework`, the task also goes back to running.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TaskReviewRequest {
    #[serde(default)]
    pub comments: Vec<ReviewComment>,
    /// A general remark, put before the line comments.
    #[serde(default)]
    #[ts(optional)]
    pub note: Option<String>,
    #[serde(default)]
    pub rework: bool,
}

/// What a path in a workspace is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Dir,
    File,
    Symlink,
}

/// One entry of a directory listing. `path` is relative to the workspace root, with `/`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub kind: FileKind,
    #[serde(default)]
    #[ts(optional, type = "number")]
    pub size: Option<u64>,
    /// Matched by .gitignore (only listed with `all=true`).
    #[serde(default)]
    pub ignored: bool,
}

/// `GET {scope}/files/tree?path=&ref=&all=`: one directory of a workspace (scope is
/// `/v1/agents/{key}`, `/v1/runs/{key}` or `/v1/runs/{key}/tasks/{task}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FileTreeResponse {
    /// The workspace root, absolute.
    pub root: String,
    /// The directory listed, relative to the root ("" for the root).
    pub path: String,
    #[serde(default)]
    #[ts(optional)]
    pub git_ref: Option<String>,
    pub entries: Vec<FileEntry>,
}

/// `GET {scope}/files/content?path=&ref=`: a file, read-only. Text is cut at 1 MiB;
/// a binary file has no `text`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FileContentResponse {
    pub path: String,
    pub abs_path: String,
    #[ts(type = "number")]
    pub size: u64,
    pub binary: bool,
    pub truncated: bool,
    #[serde(default)]
    #[ts(optional)]
    pub text: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub git_ref: Option<String>,
}

/// `POST {scope}/files/resolve`: which of these strings (relative or absolute paths,
/// without a `:line` suffix) name something inside the workspace.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ResolvePathsRequest {
    pub paths: Vec<String>,
}

/// One answer of `files/resolve`: `path` (relative, `/`) is set only when the input names
/// an existing entry inside the workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ResolvedPath {
    pub input: String,
    #[serde(default)]
    #[ts(optional)]
    pub path: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub abs_path: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub kind: Option<FileKind>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ResolvePathsResponse {
    pub entries: Vec<ResolvedPath>,
}

/// What a ref of the commit graph is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum GraphRefKind {
    Base,
    Run,
    Task,
    Head,
    Other,
}

/// A branch shown in the commit graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GraphRef {
    pub name: String,
    pub kind: GraphRefKind,
    pub sha: String,
    #[serde(default)]
    #[ts(optional)]
    pub task_key: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub task_title: Option<String>,
}

/// One commit; `refs` are the names in `CommitGraph::refs` pointing at it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GraphCommit {
    pub sha: String,
    pub parents: Vec<String>,
    pub subject: String,
    pub author: String,
    pub time: DateTime<Utc>,
    #[serde(default)]
    pub refs: Vec<String>,
}

/// `GET /v1/runs/{key}/graph?limit=` and `GET /v1/agents/{key}/graph?limit=`: commits of
/// the base, run and task branches, newest first (topological), at most `limit` (200).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CommitGraph {
    #[serde(default)]
    #[ts(optional)]
    pub base_branch: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub run_branch: Option<String>,
    pub refs: Vec<GraphRef>,
    pub commits: Vec<GraphCommit>,
    pub truncated: bool,
}

/// `GET /v1/runs/{key}/trace` (docs/tasks/DOGFOOD.md §8): what happened in a run, by task,
/// folded from the events (so it outlives the workers).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunTrace {
    pub run_id: Option<crossagents_core::RunId>,
    /// The lead's own turns and commands.
    pub lead: LaneTrace,
    pub tasks: Vec<TaskTrace>,
    pub spent: Spent,
}

/// Tokens and dollars the agents reported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Spent {
    #[ts(type = "number")]
    pub tokens: u64,
    pub cost_usd: f64,
}

/// One task's story.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TaskTrace {
    pub task_id: Option<crossagents_core::TaskId>,
    pub key: String,
    pub title: Option<String>,
    pub status: Option<crossagents_core::TaskStatus>,
    /// Why the task failed, as recorded on it.
    pub failure: Option<String>,
    pub lane: LaneTrace,
    pub checks: Vec<CheckTrace>,
    /// Each time quota moved it (`task.rerouted`).
    pub rerouted: Vec<String>,
    /// Each time it changed hands (`task.handed_off`), oldest first.
    #[serde(default)]
    pub handoffs: Vec<Handoff>,
    /// The earliest thing that went wrong: a failed check, a failed command, a failed turn.
    pub first_failure: Option<String>,
}

/// The agents that worked on something, and what they did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LaneTrace {
    pub agents: Vec<AgentId>,
    pub turns: Vec<TurnTrace>,
    pub commands: Vec<CommandTrace>,
    pub spent: Spent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TurnTrace {
    pub agent: AgentId,
    pub message_id: MessageId,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub stop_reason: Option<StopReason>,
    /// An error or a process exit during the turn.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CommandTrace {
    /// Who ran it: the agent itself, or a checker through `workspace_run`.
    pub agent: AgentId,
    pub command: String,
    pub exit_code: Option<i32>,
    #[ts(type = "number | null")]
    pub duration_ms: Option<u64>,
    pub at: DateTime<Utc>,
    /// `true` for `workspace_run`.
    pub checker: bool,
    /// The end of its output, when it failed.
    pub output_tail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CheckTrace {
    pub at: DateTime<Utc>,
    pub passed: bool,
    pub checks: Vec<crossagents_core::CheckResult>,
}

/// `POST /v1/runs/{key}/land`; merge when `mode` is omitted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LandRequest {
    #[serde(default)]
    #[ts(optional)]
    pub mode: Option<crossagents_core::LandMode>,
}

/// `POST /v1/runs/{key}/policy`: a person changes how an already started run continues by
/// itself, and may hand its allowance out again. Only the continuation policy moves; every
/// other part of the run's policy stays as it was. Absent fields keep what the run has.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateRunPolicyRequest {
    /// Absent: automatic continuation stays as the run has it.
    #[serde(default)]
    #[ts(optional)]
    pub continuation: Option<ContinuationUpdate>,
    /// Put the allowance back: `attempts` to zero, so a run whose wakeups ran out is woken
    /// again (and tells the person once more when the new allowance runs out). Default
    /// false.
    #[serde(default)]
    pub reset_attempts: bool,
}

/// Automatic continuation for a run that is already going: on or off, and its limits. The
/// cap is for the whole run, not per task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ContinuationUpdate {
    /// `false` stops the wakeups — the run itself carries on untouched; `true` starts them.
    pub enabled: bool,
    /// Absent: the cap the run had, or the default (3) when it had none.
    #[serde(default)]
    #[ts(optional)]
    pub max_attempts: Option<u32>,
    /// Absent: the interval the run had, or the default (60 seconds).
    #[serde(default)]
    #[ts(optional)]
    pub interval_seconds: Option<u32>,
    /// Report a run that waits on a busy agent once its own records go quiet for this many
    /// minutes. Absent keeps what the run had; `null` turns the report off again, which is
    /// the default. The report only ever asks the lead to decide — nothing is interrupted.
    // Skipped when it is not stated, so the value round-trips: serialising `None` as `null`
    // would come back as "the request said null", which means the opposite.
    #[serde(
        default,
        deserialize_with = "stated",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional, type = "number | null")]
    pub stuck_minutes: Option<Option<u32>>,
}

/// Tells "the request did not mention this" (`None`) from "the request said null"
/// (`Some(None)`), which a plain `Option<Option<T>>` would read as the same thing.
fn stated<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer).map(Some)
}

/// `POST /v1/runs/{key}/finish`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FinishRequest {
    #[serde(default)]
    #[ts(optional)]
    pub summary: Option<String>,
}

/// `DELETE /v1/runs/{key}?branches=&force=` (docs/tasks/R1.md §3.3): what the delete cleaned
/// up, and what it could not. The run itself is gone either way.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DeleteRunResponse {
    /// The sessions the run still held, by name. None is deleted (docs/tasks/S1.md §3.4).
    pub agents: DeletedRunAgents,
    /// The directories that went: the run's checkout, its tasks', and the build directory
    /// they shared.
    pub worktrees: Vec<String>,
    pub branches: Vec<String>,
    /// What could not be cleaned up, and why.
    pub warnings: Vec<String>,
}

/// `GET /v1/agents/{key}/execution`: what the session works on for a run now, if anything
/// (docs/tasks/S1.md §3.3). `POST …/execution/interrupt` stops it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentExecutionResponse {
    pub execution: Option<AgentExecution>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentExecution {
    pub run_id: RunId,
    pub run_title: String,
    /// `None` while it leads the run.
    pub task_id: Option<TaskId>,
    pub task_key: Option<String>,
    pub role: ClaimRole,
    /// An execution session started for this, rather than the agent itself.
    pub fresh: bool,
    pub since: DateTime<Utc>,
}

/// `GET /v1/agents/{key}/resume-command` (docs/TODO.md §十五): the command that carries the
/// session on in its own CLI, in a terminal, by the same session id. Read-only: the hub runs
/// nothing. `command` is `None` when it cannot be done, and `unavailable` says why.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ResumeCommandResponse {
    /// Ready to paste into `shell`.
    pub command: Option<String>,
    pub unavailable: Option<String>,
    /// What `command` is written for: `powershell` on Windows, `sh` elsewhere.
    pub shell: String,
    /// The directory it runs in: where the session works now.
    pub cwd: String,
    pub session_id: Option<String>,
    /// What the command sets: the account's config directory and the proxy, never a token.
    pub env: Vec<ResumeEnv>,
    /// What to know before running it (where the session file is, that the hub has it open).
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ResumeEnv {
    pub name: String,
    pub value: String,
}

/// `GET /v1/agents/{key}/commands`: the slash commands and skills the agent's own CLI brings
/// (Claude Code's `system/init`), for a GUI to offer next to its input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentCommandsResponse {
    pub commands: Vec<AgentCommand>,
    /// The CLI reported them: a session is running and named its own.
    pub known: bool,
    /// Why they are not known, in one sentence for people (`known` is false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
}

/// What became of the sessions a deleted run still held. Those that took part in it and are
/// no longer its (gone home, archived, or working for another run) are not listed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DeletedRunAgents {
    /// Agents working for it themselves, back in their workspace.
    pub returned: Vec<String>,
    /// Execution sessions it started (a lead copied for it, say), archived; they can be
    /// restored.
    pub archived: Vec<String>,
}

/// `POST /v1/agents/{key}/connect`: how an external agent reaches the hub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConnectInfo {
    pub agent: String,
    /// Valid until the hub restarts or the agent connects again.
    pub token: String,
    /// For clients configured with `mcpServers` JSON (Claude Code, Cursor, …).
    pub json_config: String,
    /// For Codex's `config.toml`.
    pub toml_config: String,
    /// The role's way of working, to give the agent as instructions.
    pub instructions: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListNotesResponse {
    pub notes: Vec<crossagents_core::Note>,
}

/// `POST /v1/questions/{id}/answer`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AnswerRequest {
    pub answer: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListQuestionsResponse {
    pub items: Vec<crossagents_core::HumanItem>,
}

/// `POST /v1/works`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateWorkRequest {
    pub title: String,
    pub lead: String,
}

/// `POST /v1/works/{id}/runs`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateWorkRunRequest {
    pub request: String,
    #[serde(default)]
    pub title: Option<String>,
    // Start from the baseline instead of carrying the previous round's changes.
    #[serde(default)]
    pub from: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListWorksResponse {
    pub works: Vec<crossagents_core::Work>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkDetail {
    pub work: crossagents_core::Work,
    pub runs: Vec<crossagents_core::Run>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListRunsResponse {
    pub runs: Vec<crossagents_core::Run>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunDetail {
    pub run: crossagents_core::Run,
    pub tasks: Vec<crossagents_core::Task>,
}

/// What the person settles with the approval (docs/tasks/W1.md §5.2). Every field is
/// optional; an empty body approves the plan as it is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApproveRunRequest {
    /// Replaces the run's policy from here on (how much it does without you, its caps).
    #[serde(default)]
    pub policy: Option<crossagents_core::RunPolicy>,
    #[serde(default)]
    pub max_parallel: Option<u32>,
    /// Who works on each task, by task key: a worker's name or id, or `null` for the run's
    /// default worker. Tasks left out keep the plan's choice.
    #[serde(default)]
    #[ts(type = "Record<string, string | null> | null")]
    pub assignees: Option<std::collections::BTreeMap<String, Option<String>>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RejectRunRequest {
    pub reason: String,
}

/// How `GET /v1/usage` groups agents.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum UsageGroup {
    #[default]
    Role,
    Model,
    Run,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UsageRow {
    /// The role, model, run id or agent name.
    pub key: String,
    pub agents: u32,
    pub sessions: u32,
    /// Sum of the cost agents reported; 0 when none reported any.
    pub cost_usd: f64,
    /// Whether any agent in the group reported a cost.
    pub cost_reported: bool,
    /// Largest context (tokens) any session in the group reached.
    #[ts(type = "number")]
    pub peak_context: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UsageResponse {
    pub by: UsageGroup,
    pub rows: Vec<UsageRow>,
    pub total_cost_usd: f64,
}

/// Adds an account. Without `home`, a fresh home under the CrossAgents directory is used;
/// with `import_default`, the provider's existing default home (e.g. `~/.codex`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateAccountRequest {
    #[serde(default)]
    pub codebuddy_site: Option<crossagents_core::CodebuddySite>,
    pub id: String,
    pub provider: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub home: Option<String>,
    #[serde(default)]
    pub import_default: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListAccountsResponse {
    pub accounts: Vec<crossagents_core::Account>,
}

/// One account with its live status and models. `GET /v1/quota` lists every account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AccountQuota {
    pub account: crossagents_core::Account,
    pub status: crossagents_core::AccountStatus,
    pub models: Vec<crossagents_core::ModelInfo>,
    /// Agents that run on this account (named in their config, or using the provider's
    /// own login).
    pub agents: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct QuotaResponse {
    pub accounts: Vec<AccountQuota>,
    /// Account providers the hub supports, for adding accounts.
    pub providers: Vec<String>,
}

/// How account token usage is grouped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum AccountUsageGroup {
    #[default]
    Day,
    Model,
    /// CrossAgents sessions vs. the vendor's own tools.
    Origin,
    Agent,
    Role,
    Run,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AccountUsageRow {
    pub key: String,
    pub totals: crossagents_core::TokenTotals,
    pub cache_hit_rate: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AccountUsageResponse {
    pub by: AccountUsageGroup,
    pub rows: Vec<AccountUsageRow>,
    pub total: crossagents_core::TokenTotals,
    pub total_cache_hit_rate: f64,
}

/// `GET /v1/usage/series?range=`: the window counted, today included.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum UsageSeriesRange {
    #[serde(rename = "7d")]
    Days7,
    #[default]
    #[serde(rename = "30d")]
    Days30,
    #[serde(rename = "90d")]
    Days90,
    /// Everything the logs hold.
    #[serde(rename = "all")]
    All,
}

impl UsageSeriesRange {
    /// How many days the window holds, today included (`all` counts whatever there is).
    pub fn days(self) -> i64 {
        match self {
            Self::Days7 => 7,
            Self::Days30 => 30,
            Self::Days90 => 90,
            Self::All => 0,
        }
    }
}

/// How a usage series is split besides its days.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum UsageSeriesGroup {
    /// One total, no rows: `groups` is empty.
    #[default]
    Total,
    Model,
    /// CrossAgents sessions vs. the vendor's own tools.
    Origin,
    Agent,
    Run,
}

/// What the days count. `request`: each model call at the time it was made, so `turns` and
/// `max_request` are known. `session`: each session on the day it started, when not every
/// source times its calls; `turns` and `max_request` are then null.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum UsageGranularity {
    Request,
    Session,
}

/// One day of the series; days without use are present with zeros.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UsageSeriesDay {
    /// `YYYY-MM-DD`, in the machine's local time zone.
    pub date: String,
    /// Input tokens served from the prompt cache.
    #[ts(type = "number")]
    pub cached_input: u64,
    /// Input tokens not from the cache (`input - cached_input`).
    #[ts(type = "number")]
    pub new_input: u64,
    #[ts(type = "number")]
    pub output: u64,
    /// Part of `output` spent on reasoning.
    #[ts(type = "number")]
    pub reasoning: u64,
    /// What the priced part of the day costs, in USD; `null` when nothing used that day has
    /// a known price (see `unpriced_models`).
    pub cost_usd: Option<f64>,
}

/// One key's use over the whole window (`group=total` has none of these).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UsageSeriesGroupRow {
    pub key: String,
    /// For people: the model id, "CrossAgents", an agent's name, a run id…
    pub label: String,
    #[ts(type = "number")]
    pub cached_input: u64,
    #[ts(type = "number")]
    pub new_input: u64,
    #[ts(type = "number")]
    pub output: u64,
    pub cost_usd: Option<f64>,
}

/// The window's totals.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UsageSeriesTotal {
    #[ts(type = "number")]
    pub cached_input: u64,
    #[ts(type = "number")]
    pub new_input: u64,
    #[ts(type = "number")]
    pub output: u64,
    #[ts(type = "number")]
    pub reasoning: u64,
    /// Sessions with use in the window.
    pub sessions: u32,
    /// Model calls in the window; `null` at `granularity: session`.
    #[ts(type = "number | null")]
    pub turns: Option<u64>,
    /// input + output of the biggest single call; `null` at `granularity: session`.
    #[ts(type = "number | null")]
    pub max_request: Option<u64>,
    pub cost_usd: Option<f64>,
}

/// One account the series was read from. A source that could not be read says why and does
/// not spoil the rest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UsageSeriesSource {
    pub id: String,
    pub label: String,
    /// e.g. `claude`, `codex`, `codebuddy`.
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
}

/// `GET /v1/usage/series?range=7d|30d|90d|all&group=total|model|origin|agent|run&source=all|<account id>`:
/// token use and cost, day by day, across accounts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UsageSeriesResponse {
    pub range: UsageSeriesRange,
    pub group: UsageSeriesGroup,
    /// `all`, or the account id asked for.
    pub source: String,
    /// Whether the days count each request or each session (see [`UsageGranularity`]).
    pub granularity: UsageGranularity,
    /// The earliest day with use in the logs (outside the window too), or `null` when there
    /// is none.
    pub first_day: Option<String>,
    /// Every day of the window, oldest first, zeros for days without use.
    pub days: Vec<UsageSeriesDay>,
    /// One row per key over the window, busiest first; empty for `group=total`.
    pub groups: Vec<UsageSeriesGroupRow>,
    pub total: UsageSeriesTotal,
    /// Tokens of the window just before this one, the same length; `null` for `range=all`.
    #[ts(type = "number | null")]
    pub previous_total: Option<u64>,
    /// Models with use in the window that have no known price, so `cost_usd` leaves them out.
    pub unpriced_models: Vec<String>,
    /// The accounts read, and why any of them was not.
    pub sources: Vec<UsageSeriesSource>,
    /// In plain words: the granularity, and what cannot be known (why `turns` or
    /// `max_request` are missing), and how cost is counted.
    pub notes: Vec<String>,
}

/// A login was started; the human finishes it in the browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LoginStarted {
    /// The page to open, when the login tool printed one.
    pub url: Option<String>,
    /// What runs, for display.
    pub command: String,
    /// Set when the hub cannot run the login for you (it happens inside the vendor's
    /// interactive CLI): what to type in a terminal, in order. Nothing was started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub manual: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListToolsResponse {
    pub tools: Vec<crossagents_core::ToolInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    Unauthorized,
    /// The credential is valid but its level is too low (docs/API-AUTH.md §3).
    Forbidden,
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
    /// The files in the way, when the conflict is about a directory's changes (deleting a
    /// run whose checkout has uncommitted work, docs/tasks/R1.md §3.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub files: Option<Vec<String>>,
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
                files: None,
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

    #[test]
    fn turn_diff_stat_counts_files_and_lines() {
        let diff = "diff --git a/src/auth.rs b/src/auth.rs\n\
                    index 1..2 100644\n\
                    --- a/src/auth.rs\n\
                    +++ b/src/auth.rs\n\
                    @@ -1,3 +1,4 @@\n \
                    fn a() {}\n\
                    -fn b() {}\n\
                    +fn b() -> u8 { 1 }\n\
                    +--- not a header\n\
                    diff --git a/new.txt b/new.txt\n\
                    new file mode 100644\n\
                    --- /dev/null\n\
                    +++ b/new.txt\n\
                    @@ -0,0 +1 @@\n\
                    +hi\n";
        let stat = TurnDiffStat::from_diff(diff, DiffSource::Hub);
        assert_eq!(stat.files.len(), 2);
        assert_eq!(stat.files[0].path, "src/auth.rs");
        assert_eq!((stat.files[0].added, stat.files[0].removed), (2, 1));
        assert_eq!(stat.files[1].path, "new.txt");
        assert_eq!((stat.added, stat.removed), (3, 1));
        assert_eq!(stat.source, DiffSource::Hub);

        // Codex sometimes sends a bare hunk list for one file.
        let bare = TurnDiffStat::from_diff(
            "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n",
            DiffSource::Driver,
        );
        assert_eq!(bare.files.len(), 1);
        assert_eq!(bare.files[0].path, "x");
        assert!(
            TurnDiffStat::from_diff("", DiffSource::Driver)
                .files
                .is_empty()
        );
    }
}
