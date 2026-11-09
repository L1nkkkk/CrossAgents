//! Request, response and error types of the hub's local API (docs/DESIGN.md §11).
//!
//! Running `cargo test -p crossagents-api-types` regenerates the TypeScript bindings in
//! `bindings/` for GUI clients.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub use crossagents_core::{
    Account, AccountStatus, Agent, AgentCommand, AgentCommandKind, AgentId, AgentProfile,
    AgentStatus, Capabilities, CheckResult, ClaimRole, ContinuationPolicy, DataSource,
    DeliveryState, DeliveryStatus, Device, DeviceKind, DeviceLevel, DeviceOs, DiffSource,
    DriverKind, Event, EventKind, FindingAnswer, FindingStatus, Handoff, HandoffKind, HistoryTask,
    LeadPlace, MessageId, MessageSource, ModelInfo, OutputStream, PlannedTask, QueuedReason, Role,
    Run, RunHistory, RunId, RunPolicy, RunStatus, StopReason, Task, TaskId, TaskStatus,
    TokenTotals, ToolInfo, ToolStatus,
};

/// Handoff cards and how a task's chain of them reads.
pub use crossagents_core::handoff;

/// Issue a device credential; omitted kind and level default to client and write.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateDeviceRequest {
    pub name: String,
    #[serde(default)]
    #[ts(optional)]
    pub kind: Option<DeviceKind>,
    #[serde(default)]
    #[ts(optional)]
    pub level: Option<DeviceLevel>,
    /// The device's system, when the issuer knows it; the device's own requests report it
    /// later through `x-crossagents-client-os` either way.
    #[serde(default)]
    #[ts(optional)]
    pub os: Option<DeviceOs>,
}
/// The plaintext token appears only in this issuance response.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateDeviceResponse {
    pub device: Device,
    pub token: String,
}
/// Active and revoked device records, without credentials or hashes.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListDevicesResponse {
    pub devices: Vec<Device>,
}

pub const API_VERSION: &str = "v2";

/// Entry points this hub has that a hub from before the list did not (docs/TODO.md §二): the
/// GUI hides what a hub does not list, and a hub built before this field sends no list at all,
/// which the GUI reads as "an older hub" (`apps/gui/src/lib/features.ts`). Names are stable ids;
/// adding one is not an API break (docs/API-AUTH.md §5), removing one is.
pub const FEATURES: [&str; 5] = ["git", "lessons", "work", "apply", "devices"];

/// Checks rerun in the person's directory after applying merged files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApplyRerun {
    pub results: Vec<CheckResult>,
    pub error: Option<String>,
}

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
    /// The `FEATURES` this build has. Missing from a hub built before the field: the GUI then
    /// treats it as an older hub and hides those entries (the sidebar's version tooltip says so).
    #[serde(default)]
    pub features: Vec<String>,
    /// The machine the hub runs on. Missing from a hub built before the field.
    #[serde(default)]
    #[ts(optional)]
    pub machine: Option<HubMachine>,
}

/// The hub's own machine, as `/v1/info` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HubMachine {
    pub os: DeviceOs,
    /// The machine's host name; empty when it could not be read.
    pub hostname: String,
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

/// How many pictures the avatar gallery holds, and how many one upload may add.
pub const MAX_AVATARS: usize = 200;
pub const MAX_AVATAR_UPLOAD: usize = 50;
/// One picture; clients scale them down to a small square first.
pub const MAX_AVATAR_BYTES: usize = 256 * 1024;

/// `GET /v1/avatars`: the gallery's pictures, by id; `GET /v1/avatars/{id}` reads one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListAvatarsResponse {
    pub avatars: Vec<String>,
}

/// `POST /v1/avatars`: PNG, JPEG or WebP pictures, base64.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UploadAvatarsRequest {
    pub images: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UploadAvatarsResponse {
    /// The pictures that were not in the gallery yet.
    pub added: Vec<String>,
    /// How many sessions without an avatar got one.
    pub assigned: u32,
}

#[derive(Debug, Clone, PartialEq, TS)]
#[ts(export)]
pub struct ListAgentsResponse {
    #[ts(type = "Array<import('./Agent').Agent & { unusable?: string, unguarded: boolean }>")]
    pub agents: Vec<Agent>,
    #[ts(skip)]
    pub unusable: Vec<Option<String>>,
    #[ts(skip)]
    pub unguarded: Vec<bool>,
}

impl<'de> Deserialize<'de> for ListAgentsResponse {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Entry {
            #[serde(flatten)]
            agent: Agent,
            #[serde(default)]
            unusable: Option<String>,
            #[serde(default)]
            unguarded: bool,
        }
        #[derive(Deserialize)]
        struct Response {
            agents: Vec<Entry>,
        }
        let entries = Response::deserialize(deserializer)?.agents;
        let mut agents = Vec::new();
        let mut unusable = Vec::new();
        let mut unguarded = Vec::new();
        for e in entries {
            agents.push(e.agent);
            unusable.push(e.unusable);
            unguarded.push(e.unguarded);
        }
        Ok(Self {
            agents,
            unusable,
            unguarded,
        })
    }
}

impl Serialize for ListAgentsResponse {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error as _;
        let mut agents = Vec::with_capacity(self.agents.len());
        for (i, agent) in self.agents.iter().enumerate() {
            let mut value = serde_json::to_value(agent).map_err(S::Error::custom)?;
            if let Some(Some(reason)) = self.unusable.get(i) {
                value["unusable"] = serde_json::Value::String(reason.clone());
            }
            value["unguarded"] = self.unguarded.get(i).copied().unwrap_or(false).into();
            agents.push(value);
        }
        #[derive(Serialize)]
        struct Response {
            agents: Vec<serde_json::Value>,
        }
        Response { agents }.serialize(serializer)
    }
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
/// (empty: none); `None` goes back to the default (`project,local`). `allow_hooks`: whether
/// hooks from those files may run, default off (SELF.md §B5); `None` leaves it as it is.
/// Both are read only at launch, so an idle session is restarted after a change.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentSettingsRequest {
    pub setting_sources: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub allow_hooks: Option<bool>,
    /// Absent keeps the current override; null follows the run or default.
    #[serde(
        default,
        deserialize_with = "stated",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(
        optional,
        type = "import('./PermissionPolicy').PermissionPolicy | null"
    )]
    pub permission: Option<Option<crossagents_core::PermissionPolicy>>,
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
    /// Hand the message to the running turn: the model reads it at its next tool boundary.
    /// Falls back to the queue when no turn runs or the driver cannot steer.
    #[serde(default)]
    #[ts(optional)]
    pub steer: Option<bool>,
    /// Files sent with the message (docs/tasks/DOGFOOD.md §3): at most
    /// [`MAX_ATTACHMENTS`], each at most [`MAX_ATTACHMENT_BYTES`], of [`attachment_mime_ok`]
    /// types.
    #[serde(default)]
    #[ts(optional)]
    pub attachments: Option<Vec<AttachmentUpload>>,
    /// The sender's id for this message, fixed once and reused on a retry: the hub then
    /// answers a repeat with the first delivery instead of starting another turn
    /// (docs/tasks/DELIVERY-AUDIT.md §4).
    #[serde(default)]
    #[ts(optional)]
    pub client_message_id: Option<crossagents_core::MessageId>,
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
    /// `true` if it went into the running turn (`agent.steered`); `queued` is then `false`.
    #[serde(default)]
    pub steered: bool,
    /// Why it waits (or why a steer did not go in), for clients to show; absent when it
    /// started at once with nothing to explain (docs/tasks/DELIVERY-AUDIT.md §3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub queued_reason: Option<QueuedReason>,
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
    /// What the session is doing with messages right now, from the same inbox the hub
    /// admits them into: `state` is `idle`, `starting`, `turn` or `held`, and `queued`
    /// counts the messages that have not started. A client shows "busy" as
    /// `state != idle` and "next" as `queued`, instead of working either out itself
    /// (docs/tasks/DELIVERY-AUDIT.md §8 P1-4).
    #[serde(default)]
    pub delivery: DeliveryStatus,
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
    /// Notices that rode along with the message (docs/TODO.md §二十五 第 3 条): in what the
    /// agent read, after `input`, but not part of it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub notices: Vec<String>,
    /// Why the message waits, while it has not started: the `agent.input`'s
    /// `queued_reason`, kept so a reloaded transcript says the same thing the live events
    /// did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub queued_reason: Option<QueuedReason>,
    /// Taken back before it started: the row stays where it was sent and says so
    /// (`agent.input_withdrawn`), instead of vanishing on a reload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub withdrawn: Option<bool>,
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
    /// A message put into this turn while it ran. `read` once the model has seen it.
    Steer {
        message_id: crossagents_core::MessageId,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[ts(as = "Option<Vec<crossagents_core::Attachment>>", optional)]
        attachments: Vec<crossagents_core::Attachment>,
        read: bool,
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
    /// A version probe is running now: `version` is empty until it answers, so a client says
    /// "asking" rather than "unknown" (docs/DESIGN.md §9.3).
    #[serde(default)]
    pub probing: bool,
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
    /// `install` or `upgrade` while one is running for this plugin now; absent when nothing
    /// is. The hub keeps it in memory only: after a restart nothing is running, so it is
    /// empty again (docs/DESIGN.md §9.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub installing: Option<String>,
    /// Where the description came from: `built-in`, a manifest's path, or `config.toml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub described_by: Option<String>,
    /// The oldest CLI the description accepts; absent when it names none
    /// (docs/research/multica.md §2.9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub min_version: Option<String>,
    /// The CLI found here is older than `min_version`: its agent is not registered, so no
    /// models are offered and the row says the version is too low.
    #[serde(default)]
    pub below_minimum: bool,
    /// The upstream version series the driver was checked against (`"2.1"`); absent when
    /// the description names none (docs/DESIGN.md §9.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub verified: Option<Vec<String>>,
    /// The CLI's version is outside those series: it still runs, the row says it is
    /// unverified. False when no series is named or the version is unknown.
    #[serde(default)]
    pub unverified: bool,
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

// ---------------------------------------------------------------------------
// Extensions (docs/EXTENSIONS.md)
// ---------------------------------------------------------------------------

/// The extension interface version a description file declares (`api = 1`).
pub const EXTENSION_API: u32 = 1;

/// How long `<extension>__<tool>` may be. An MCP client prefixes its own server name
/// (`mcp__crossagents__`, 18 characters) and several cap a tool name at 64, so the hub
/// keeps the part it controls within what is left (docs/EXTENSIONS.md §3).
pub const EXTENSION_TOOL_NAME_MAX: usize = 46;

/// Separates the extension from the tool in a qualified tool name.
pub const EXTENSION_TOOL_SEPARATOR: &str = "__";

/// Whether `name` may name an extension: `^[a-z][a-z0-9-]*$`. The `__` separator cannot
/// occur in it (no underscore is allowed at all), so splitting a qualified name is
/// unambiguous. Returns why not, for a 400.
pub fn check_extension_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("an extension name cannot be empty".into());
    }
    let ok = name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{name:?} is not a valid extension name: lowercase letters, digits and `-`, starting with a letter"
        ))
    }
}

/// Whether `name` may name a tool of an extension: `^[a-z][a-z0-9_]*$`, and never `__`,
/// which would make `<extension>__<tool>` ambiguous. Returns why not, for a 400.
pub fn check_extension_tool_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("a tool name cannot be empty".into());
    }
    let ok = name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !ok {
        return Err(format!(
            "{name:?} is not a valid tool name: lowercase letters, digits and `_`, starting with a letter"
        ));
    }
    if name.contains(EXTENSION_TOOL_SEPARATOR) {
        return Err(format!(
            "{name:?} cannot contain `{EXTENSION_TOOL_SEPARATOR}`: it separates the extension from the tool"
        ));
    }
    Ok(())
}

/// What models and the GUI call the tool `tool` of extension `extension`:
/// `<extension>__<tool>`. Both names and the length are checked
/// ([`EXTENSION_TOOL_NAME_MAX`]); returns why not, for a 400.
pub fn extension_tool_name(extension: &str, tool: &str) -> Result<String, String> {
    check_extension_name(extension)?;
    check_extension_tool_name(tool)?;
    let qualified = format!("{extension}{EXTENSION_TOOL_SEPARATOR}{tool}");
    if qualified.len() > EXTENSION_TOOL_NAME_MAX {
        return Err(format!(
            "{qualified:?} is {} characters; a qualified tool name may be at most {EXTENSION_TOOL_NAME_MAX}",
            qualified.len()
        ));
    }
    Ok(qualified)
}

/// What an extension's own hub token may do. Never admin: the level only guards against
/// mistakes, not against malicious code (docs/EXTENSIONS.md §6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionAccess {
    #[default]
    Read,
    Write,
}

/// One tool a snapshot declares.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExtensionToolInfo {
    /// As the description file names it (`report`).
    pub name: String,
    /// What models and the GUI see (`standup__report`).
    pub qualified_name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    /// The groups that may call it; `human` lets the GUI call it.
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub command: Vec<String>,
    pub timeout_secs: u32,
}

/// One block of extra instructions a snapshot declares (docs/EXTENSIONS.md §4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExtensionPromptInfo {
    /// Relative to the snapshot.
    pub file: String,
    #[serde(default)]
    pub groups: Vec<String>,
    #[ts(type = "number")]
    pub bytes: u64,
}

/// One `[[on]]` block in a snapshot: a process started for every new event of this type
/// (docs/EXTENSIONS.md §3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExtensionHookInfo {
    /// The event's wire name (`run.landed`). Never `extension.*`: an extension cannot
    /// react to another one, directly or through a chain.
    pub event: String,
    #[serde(default)]
    pub command: Vec<String>,
    /// How long the process may run; past it the tree is killed (1..600).
    pub timeout_secs: u32,
}

/// The resident process declared by a snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExtensionServiceInfo {
    pub command: Vec<String>,
}

/// One snapshot of an extension: what it declares, as the hub read it from the snapshot it
/// took (not from the source directory, which may have changed since).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExtensionVersion {
    /// Hash of every path and its content in the snapshot, taken after the copy finished.
    pub fingerprint: String,
    pub api: u32,
    #[serde(default)]
    pub description: String,
    pub access: ExtensionAccess,
    #[serde(default)]
    pub tools: Vec<ExtensionToolInfo>,
    #[serde(default)]
    pub prompts: Vec<ExtensionPromptInfo>,
    /// Event hooks this snapshot declares, in description-file order. Absent and empty mean
    /// the same: a version taken before E4 has none.
    #[serde(default)]
    #[ts(as = "Option<Vec<ExtensionHookInfo>>", optional)]
    pub hooks: Vec<ExtensionHookInfo>,
    /// The resident process declared by this snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub service: Option<ExtensionServiceInfo>,
    /// The GUI entry, relative to the snapshot; `None` when it has no panel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub gui_entry: Option<String>,
    pub files: u32,
    #[ts(type = "number")]
    pub bytes: u64,
    /// When the snapshot was taken.
    pub taken_at: DateTime<Utc>,
}

/// A snapshot waiting for a person to approve it (docs/EXTENSIONS.md §6). The hub notices a
/// changed source directory by polling and records one; `extension_request_enable` records
/// one at once. It never times out and has no default answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExtensionPending {
    pub version: ExtensionVersion,
    pub requested_at: DateTime<Utc>,
    /// The agent that asked (`extension_request_enable`); absent when the hub's poll found
    /// the change by itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub requested_by: Option<AgentId>,
}

/// What the resident process of one extension is doing (docs/EXTENSIONS.md §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionServiceState {
    /// Not running: the extension is disabled, declares no service, or was just stopped.
    Stopped,
    /// Being launched.
    Starting,
    /// Alive.
    Running,
    /// It exited; a restart follows when the backoff ends.
    Backoff,
}

/// Live state of an extension's resident process. Not persisted: every hub starts its own
/// (a service never outlives the hub that started it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExtensionServiceStatus {
    pub state: ExtensionServiceState,
    /// Starts since it last ran for 60 s without exiting; reset to 0 by that.
    pub restarts: u32,
    /// Why it last exited (exit code, signal, or why it could not start).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_exit: Option<String>,
}

/// One extension as `GET /v1/extensions` reports it. Everything but `error` and `service`
/// comes from `<home>/extensions.json` and the snapshots it names, so it survives a hub
/// restart; `service` is live state of this hub's run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExtensionInfo {
    pub name: String,
    /// Its source directory (`~/.crossagents/extensions/<name>`), which people and agents
    /// edit. The hub never runs it.
    pub source: String,
    /// Calls run and the GUI loads only when this is true.
    pub enabled: bool,
    /// A changed source directory is snapshotted and switched to without asking. Only a
    /// person can turn this on; no agent tool reaches it (docs/EXTENSIONS.md §6).
    pub auto_trust: bool,
    /// The snapshot calls run against; `None` until a version has been approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub current: Option<ExtensionVersion>,
    /// The version before it, kept so calls that started on it can finish and the GUI's
    /// `crossagents-ext:` urls keep resolving.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub previous_fingerprint: Option<String>,
    /// Waiting for approval; `None` when the source matches `current`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pending: Option<ExtensionPending>,
    /// The resident process's state, when this extension declares a `[service]`. Live
    /// state of this hub's run, not part of `extensions.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub service: Option<ExtensionServiceStatus>,
    /// Why the source cannot be snapshotted or does not check out (a symlink, over the
    /// 50 MB cap, a description file that does not parse). The current version keeps
    /// running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
}

/// `GET /v1/extensions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExtensionsResponse {
    pub extensions: Vec<ExtensionInfo>,
}

/// `POST /v1/extensions/{name}/enable`: approves one exact snapshot. A fingerprint that is
/// not the pending one is refused with 409, so nobody approves a version other than the one
/// they looked at. Answers with the extension's new [`ExtensionInfo`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EnableExtensionRequest {
    pub fingerprint: String,
}

/// `POST /v1/extensions/{name}/auto-trust`. Answers with the extension's new
/// [`ExtensionInfo`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SetExtensionAutoTrustRequest {
    pub auto_trust: bool,
}

/// What happened to one file between two snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionFileChangeKind {
    Added,
    Removed,
    Modified,
}

/// One file's part of [`ExtensionDiffResponse`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExtensionFileChange {
    /// Relative to the snapshot.
    pub path: String,
    pub change: ExtensionFileChangeKind,
    pub added: u32,
    pub removed: u32,
    /// It carries no diff text: it is binary, or past the per-file cap.
    #[serde(default)]
    pub binary: bool,
}

/// `GET /v1/extensions/{name}/pending/diff`: the snapshot waiting for approval against the
/// version running now, so the approval screen can show what changes (docs/EXTENSIONS.md
/// §3, §6). 404 when nothing is pending.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExtensionDiffResponse {
    pub name: String,
    /// The pending snapshot, the `b/` side.
    pub fingerprint: String,
    /// The running snapshot, the `a/` side; `None` for a first version, which is all
    /// additions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub against: Option<String>,
    /// Unified diff of the text files.
    pub diff: String,
    /// The diff was longer than the hub hands out at once and was cut; `files` is still
    /// complete.
    #[serde(default)]
    pub truncated: bool,
    /// Every changed file, binary ones included.
    #[serde(default)]
    pub files: Vec<ExtensionFileChange>,
    /// The pending version's `access`, and whether it differs from the running one: it
    /// decides what the extension's token may do, so the screen calls it out.
    pub access: ExtensionAccess,
    #[serde(default)]
    pub access_changed: bool,
}

/// `POST /v1/extensions/{name}/tools/{tool}`: only the input. `caller` is not accepted
/// here — the hub works it out from the credential (docs/EXTENSIONS.md §3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExtensionCallRequest {
    #[serde(default)]
    pub input: serde_json::Value,
}

/// 202 from `POST /v1/extensions/{name}/tools/{tool}`: the call started. It can run for up
/// to ten minutes, longer than the GUI bridge waits, so the result is fetched from
/// `GET /v1/extensions/calls/{call_id}` or awaited on `extension.call.finished`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExtensionCallAccepted {
    /// Opaque; the hub assigns it and forgets it ten minutes after the call ends.
    pub call_id: String,
    pub extension: String,
    /// The bare tool name, as the description file names it.
    pub tool: String,
    /// The snapshot this call was pinned to when it started: a version switch while it runs
    /// does not move it.
    pub fingerprint: String,
    pub started_at: DateTime<Utc>,
    /// When the hub will kill it.
    pub timeout_secs: u32,
}

/// How one extension tool call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionCallStatus {
    Running,
    Ok,
    /// It exited non-zero, or its output was not the JSON the protocol asks for.
    Failed,
    /// `DELETE /v1/extensions/calls/{id}`, or the extension was disabled under it.
    Cancelled,
    TimedOut,
}

/// `GET /v1/extensions/calls/{id}`, and what `DELETE` on the same path answers. Kept in
/// memory for ten minutes after the call ends; a hub restart loses it (the hub never
/// retries a call, as a tool may have side effects).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExtensionCallResponse {
    pub call_id: String,
    pub extension: String,
    pub tool: String,
    pub fingerprint: String,
    pub status: ExtensionCallStatus,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "number")]
    pub duration_ms: Option<u64>,
    /// The tool's parsed stdout, when it succeeded. Images follow the `tool_content`
    /// convention, as a driver's tool output does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub output: Option<serde_json::Value>,
    /// Why it failed, for a status other than `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
    /// The last 64 KB it wrote to stderr; the whole of it is in the extension's log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stderr: Option<String>,
}

/// `GET /v1/extensions/{name}/logs` and `extension_logs`: the current log's last
/// 64 KiB, decoded as UTF-8 with replacement. Empty before the first call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExtensionLogsResponse {
    pub name: String,
    pub content: String,
    /// Earlier bytes of the current log were omitted. Rotated logs are not included.
    pub truncated: bool,
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
    /// Where the lead works (docs/tasks/LEAD.md §2): in the person's own directory
    /// (`in_place`), or in a checkout the hub makes (the default). A directory outside
    /// version control gets one in place whatever this says.
    #[serde(default)]
    #[ts(optional)]
    pub lead_place: Option<crossagents_core::LeadPlace>,
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
    #[serde(default)]
    pub unguarded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub unusable: Option<String>,
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

/// What git says changed at one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum FileGitChange {
    Added,
    Modified,
    Deleted,
}

/// One changed path in a working copy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FileStatusEntry {
    /// Relative to the workspace root, with `/`.
    pub path: String,
    pub change: FileGitChange,
}

/// `GET {scope}/files/status`: the working copy's uncommitted changes. `git` is false
/// outside a git repository, where there are no changes to show rather than an error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FileStatusResponse {
    /// The workspace root, absolute.
    pub root: String,
    pub git: bool,
    pub entries: Vec<FileStatusEntry>,
}

/// `GET {scope}/files/search?q=&all=`: matching paths across the whole workspace, not
/// just the directories a client has loaded. At most 200; `truncated` says there were more.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FileSearchResponse {
    /// The workspace root, absolute.
    pub root: String,
    /// Paths relative to the root, with `/`, sorted.
    pub paths: Vec<String>,
    pub truncated: bool,
}

/// A terminal on the hub's machine (`GET /v1/terminals`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalInfo {
    pub id: String,
    /// What the client grouped it under when creating it; the hub only keeps this.
    pub project: Option<String>,
    pub cwd: String,
    pub shell: String,
    pub cols: u16,
    pub rows: u16,
    pub created_at: DateTime<Utc>,
    /// The shell finished; the output stays readable until the terminal is closed.
    pub exited: bool,
    pub exit_code: Option<i32>,
    /// Connections reading its output now.
    pub connections: u32,
    /// Which node it runs on; `None` is the hub's own machine, the only one yet.
    pub node: Option<String>,
}

/// `GET /v1/terminals`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListTerminalsResponse {
    pub terminals: Vec<TerminalInfo>,
}

/// `POST /v1/terminals`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateTerminalRequest {
    pub cwd: String,
    #[serde(default)]
    pub project: Option<String>,
    pub cols: u16,
    pub rows: u16,
    /// Reserved for cross-node terminals: anything but none is refused for now.
    #[serde(default)]
    pub node: Option<String>,
}

/// `POST /v1/terminals/{id}/input`: one batch of keystrokes. `seq` rises per connection; a batch
/// whose `seq` was already typed is not typed again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalInputRequest {
    pub connection: String,
    pub seq: u32,
    pub data: String,
}

/// The newest batch typed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalInputResponse {
    pub acked: u32,
}

/// `POST /v1/terminals/{id}/resize`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalResizeRequest {
    pub connection: String,
    pub cols: u16,
    pub rows: u16,
}

/// `POST /v1/terminals/{id}/takeover`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalTakeoverRequest {
    pub connection: String,
}

/// What `GET /v1/terminals/{id}/output` streams, one per SSE event. `data` is base64: output
/// comes in chunks that can end inside a UTF-8 character.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum TerminalFrame {
    /// Clear the terminal, then write this: the screen now. First on every connection, and again
    /// if a connection falls behind.
    Snapshot {
        data: String,
        cols: u16,
        rows: u16,
    },
    Output {
        data: String,
    },
    /// This connection types from now on; every other only watches.
    Writer {
        connection: String,
    },
    /// The shell finished, and this stream ends here.
    Exit {
        code: Option<i32>,
    },
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

/// One branch of a project, as `GET /v1/git/branches?path=` lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitBranch {
    /// The short name: `main`, `origin/main`.
    pub name: String,
    /// A remote-tracking branch (`refs/remotes/…`) rather than a local one.
    pub remote: bool,
    pub sha: String,
    /// What it tracks, by short name (`origin/main`).
    #[serde(default)]
    #[ts(optional)]
    pub upstream: Option<String>,
    /// Commits on the branch that its upstream does not have.
    #[serde(default)]
    pub ahead: u32,
    /// Commits the upstream has that the branch does not.
    #[serde(default)]
    pub behind: u32,
    /// HEAD points at this branch.
    #[serde(default)]
    pub current: bool,
}

/// `GET /v1/git/branches?path=`: a project's local and remote-tracking branches.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitBranchesResponse {
    /// The project directory, absolute, as the hub names it.
    pub root: String,
    pub branches: Vec<GitBranch>,
    /// The branch HEAD is on; absent when HEAD is detached or the repository is unborn.
    #[serde(default)]
    #[ts(optional)]
    pub head: Option<String>,
    /// The commit HEAD names; absent in a repository without commits.
    #[serde(default)]
    #[ts(optional)]
    pub head_sha: Option<String>,
}

/// One side of a file's state in `git status --porcelain=v2`: the index (`X`) or the
/// working tree (`Y`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum GitFileState {
    Unchanged,
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unmerged,
    Unknown,
}

/// One changed path in a project's working copy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitStatusEntry {
    /// Relative to the project root, with `/`.
    pub path: String,
    /// The path before a rename or copy.
    #[serde(default)]
    #[ts(optional)]
    pub orig_path: Option<String>,
    pub index: GitFileState,
    pub worktree: GitFileState,
    /// Git does not track the file (the `?` record).
    #[serde(default)]
    pub untracked: bool,
    /// An unresolved merge (the `u` record).
    #[serde(default)]
    pub conflicted: bool,
}

/// `GET /v1/git/status?path=`: a project's working copy, parsed from
/// `git status --porcelain=v2`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitStatusResponse {
    /// The project directory, absolute, as the hub names it.
    pub root: String,
    /// The commit HEAD is at; absent in a repository without commits.
    #[serde(default)]
    #[ts(optional)]
    pub head: Option<String>,
    /// The branch HEAD is on; absent when HEAD is detached.
    #[serde(default)]
    #[ts(optional)]
    pub branch: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub upstream: Option<String>,
    #[serde(default)]
    pub ahead: u32,
    #[serde(default)]
    pub behind: u32,
    pub entries: Vec<GitStatusEntry>,
}

/// `GET /v1/git/commits/{sha}?path=`: one commit's author, time, whole message and parents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitCommitResponse {
    /// The project directory, absolute, as the hub names it.
    pub root: String,
    pub sha: String,
    pub parents: Vec<String>,
    pub author: String,
    pub author_email: String,
    pub time: DateTime<Utc>,
    /// The subject line.
    pub subject: String,
    /// The whole message, subject line and body.
    pub message: String,
}

/// What a ref of the whole-repository graph is: HEAD, a local branch, a remote-tracking
/// branch, or a tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum GitRefKind {
    Head,
    Local,
    Remote,
    Tag,
}

/// A ref shown in the whole-repository graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitGraphRef {
    pub name: String,
    pub kind: GitRefKind,
    pub sha: String,
}

/// `GET /v1/git/graph?path=&limit=`: the whole repository (`git log --all`), newest first
/// (topological), at most `limit` (200), with every ref and what it points at.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GitGraph {
    /// The project directory, absolute, as the hub names it.
    pub root: String,
    pub refs: Vec<GitGraphRef>,
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
    /// When the run's branch reached its base (`run.landed`), for the swimlane's 落地 node.
    #[serde(default)]
    #[ts(optional)]
    pub landed: Option<crossagents_core::Landing>,
}

/// Tokens and dollars the agents reported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Spent {
    #[ts(type = "number")]
    pub tokens: u64,
    pub cost_usd: f64,
}

/// One dispatch of a task's worker: the accounts weighed for it, best first, and the one it
/// got (magpie-lancer §2.8).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DispatchTrace {
    pub at: DateTime<Utc>,
    /// The account the worker runs on; `None` when routing knew no account.
    pub account: Option<String>,
    /// The same vendor's accounts, best first, each with why it ranks there.
    pub candidates: Vec<crossagents_core::AccountCandidate>,
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
    /// Each dispatch of a worker for it (`task.routed`), with the accounts weighed and why.
    #[serde(default)]
    #[ts(as = "Option<Vec<DispatchTrace>>", optional)]
    pub dispatches: Vec<DispatchTrace>,
    /// Each time it changed hands (`task.handed_off`), oldest first.
    #[serde(default)]
    pub handoffs: Vec<Handoff>,
    /// The earliest thing that went wrong: a failed check, a failed command, a failed turn.
    pub first_failure: Option<String>,
    /// Each status change, oldest first (the swimlane's 提交, 重试, 合并 nodes).
    #[serde(default)]
    #[ts(as = "Option<Vec<StatusTrace>>", optional)]
    pub statuses: Vec<StatusTrace>,
}

/// A task moved from one status to another (`task.status_changed`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StatusTrace {
    pub at: DateTime<Utc>,
    pub from: crossagents_core::TaskStatus,
    pub to: crossagents_core::TaskStatus,
    /// The worker it was with.
    pub agent: Option<AgentId>,
}

/// Something that happened to a lane's session rather than its task (magpie-lancer §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum LaneMarkKind {
    /// The model or effort was switched (`agent.model_changed`).
    ModelChanged,
    /// The context shrank by much between two usage reports: it was compacted.
    Compacted,
    /// The agent asked the person (`human.asked`, a question).
    Asked,
    /// The person answered, or the default was taken.
    Answered,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LaneMark {
    pub at: DateTime<Utc>,
    pub agent: AgentId,
    pub kind: LaneMarkKind,
    /// In words: `sonnet → opus`, `180k → 40k`, the question, the answer.
    pub text: Option<String>,
}

/// The agents that worked on something, and what they did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LaneTrace {
    pub agents: Vec<AgentId>,
    pub turns: Vec<TurnTrace>,
    pub commands: Vec<CommandTrace>,
    pub spent: Spent,
    /// Model switches, compactions, questions to the person and their answers, oldest first.
    #[serde(default)]
    #[ts(as = "Option<Vec<LaneMark>>", optional)]
    pub marks: Vec<LaneMark>,
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

/// `GET /v1/background`: what the hub is keeping going behind the conversations: the
/// agents' reminders, their watches, and the tasks whose checks are running now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BackgroundResponse {
    /// Soonest first.
    pub reminders: Vec<BackgroundReminder>,
    /// Newest first.
    pub watches: Vec<BackgroundWatch>,
    /// Tasks in `verifying`, longest-running first.
    pub verifying: Vec<VerifyingTask>,
}

/// A reminder an agent set (`remind_set`), with what the board shows about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BackgroundReminder {
    pub reminder: crossagents_core::Reminder,
    /// The owner's name, or its id when it is gone.
    pub owner_name: String,
    /// The linked task's key, when there is one.
    pub task_key: Option<String>,
    /// The run it belongs to: the linked task's, else the owner's.
    pub run_id: Option<RunId>,
}

/// A watch an agent keeps (`watch`) on a task or a teammate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BackgroundWatch {
    pub watch: crossagents_core::Watch,
    pub owner_name: String,
    /// The watched task's `#key` or the watched agent's name; its id when it is gone.
    pub target_name: String,
    /// The run it belongs to: the watched task's, else the owner's.
    pub run_id: Option<RunId>,
}

/// A task whose acceptance checks the hub is running.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct VerifyingTask {
    pub task_id: TaskId,
    pub run_id: RunId,
    pub run_title: String,
    pub key: String,
    pub title: String,
    pub assignee: Option<AgentId>,
    pub assignee_name: Option<String>,
    /// How many checks it runs.
    pub checks: u32,
    /// When it entered `verifying` (its last update).
    pub since: DateTime<Utc>,
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
    /// What the work group's rounds left behind, including the ones since deleted
    /// (`RunHistory.deleted`): the work's history outlives its runs.
    #[serde(default)]
    pub history: Vec<crossagents_core::RunHistory>,
}

/// `GET /v1/history`: what runs left in a workspace, including the ones since deleted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListHistoryResponse {
    pub history: Vec<crossagents_core::RunHistory>,
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
    /// One row per task (its key; `run_id` says which run it is in).
    Task,
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
    #[serde(default)]
    pub estimated_cost_usd: Option<f64>,
    #[serde(default)]
    #[ts(type = "number")]
    pub cache_write_input: u64,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub requests: Option<u64>,
    /// Largest context (tokens) any session in the group reached.
    #[ts(type = "number")]
    pub peak_context: u64,
    /// For `by task`: the run the task is in.
    #[serde(default)]
    #[ts(optional)]
    pub run_id: Option<String>,
    /// Raw tokens the agents reported (`tokens.total`), and their kinds: input that missed the
    /// prompt cache, input served from it, and output (cache writes are `cache_write_input`).
    #[serde(default)]
    #[ts(type = "number", optional)]
    pub tokens: Option<u64>,
    #[serde(default)]
    #[ts(type = "number", optional)]
    pub new_input: Option<u64>,
    #[serde(default)]
    #[ts(type = "number", optional)]
    pub cached_input: Option<u64>,
    #[serde(default)]
    #[ts(type = "number", optional)]
    pub output: Option<u64>,
    /// Times the task was sent back for rework.
    #[serde(default)]
    #[ts(type = "number | null", optional)]
    pub rework: Option<u32>,
    /// Minutes from the task's first start to its end, or to now while it runs.
    #[serde(default)]
    #[ts(type = "number | null", optional)]
    pub minutes: Option<f64>,
    /// Seconds its acceptance checks and commands took.
    #[serde(default)]
    #[ts(type = "number | null", optional)]
    pub check_seconds: Option<f64>,
    /// For `by run`: the run's tasks, with their own figures.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<UsageRow>,
    /// Caps that do not apply, and why (an unpriced model, no request counts).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
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

/// A login already on this machine that can be copied into an account of its own
/// (docs/research/magpie-lancer.md §2.11). Importing reads the file and never changes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ImportableAccount {
    pub provider: String,
    /// The file the login is read from, as the person would look for it.
    pub source: String,
    /// What refreshing the copied token does to the tool the login came from.
    pub warning: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ImportAccountsResponse {
    /// The logins found; empty when the machine has none.
    pub candidates: Vec<ImportableAccount>,
}

/// Copies a login already on this machine into a new account's own home. `confirm` must be
/// true: the person has been told that refreshing the copied token may sign the original
/// tool out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ImportAccountRequest {
    pub provider: String,
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    pub confirm: bool,
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

/// One agent's spend on one model within a run: a segment of the run's cost bar
/// (docs/research/multica.md §十三).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunCostSlice {
    pub agent: AgentId,
    /// The agent's name as it reported, so the segment still reads after a rename.
    pub name: String,
    /// The model it ran on; `default` when it never said.
    pub model: String,
    #[ts(type = "number")]
    pub cached_input: u64,
    #[ts(type = "number")]
    pub new_input: u64,
    #[ts(type = "number")]
    pub output: u64,
    /// What the agent reported for this stretch; `null` when it reported tokens but no cost.
    pub cost_usd: Option<f64>,
}

/// `GET /v1/runs/{key}/cost`: what a run spent, split by agent and model — the run's cost
/// bar. An agent that switched models has one slice per model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunCostResponse {
    pub run_id: RunId,
    /// Biggest spender first; one agent's slices next to each other, its largest first.
    pub slices: Vec<RunCostSlice>,
    #[ts(type = "number")]
    pub tokens: u64,
    /// Sum of the costs the agents reported; `null` when none did.
    pub cost_usd: Option<f64>,
    /// In plain words: how the figures are counted, and what is missing.
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

// ---------------------------------------------------------------------------------------
// Projects (docs/tasks/LESSONS.md §4.2)

/// One checkout of a project on a node (paths are strings, for clients that cannot read
/// `PathBuf`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProjectPathInfo {
    pub node: String,
    pub path: String,
}

/// A project as clients see it: its stable id, and the checkouts and remotes that identify
/// it. A project's expertise profile is keyed by [`ProjectInfo::id`], which is what a client
/// finds here from the directory it works in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
    /// Normalised `host/owner/repo` remotes.
    pub remotes: Vec<String>,
    pub paths: Vec<ProjectPathInfo>,
    /// A remote seen at one of the project's checkouts that is not one of its own: the
    /// directory may hold another repository now, so the person decides (LESSONS §4.2).
    /// Nothing is bound automatically until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub unconfirmed_remote: Option<String>,
}

/// `POST /v1/projects/{id}/bind`: the person says a remote or a checkout belongs to this
/// project. A remote becomes an alias; a checkout moves to it from wherever it was bound.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BindProjectRequest {
    /// A remote URL, written any way git writes it (`git@host:owner/repo`,
    /// `https://host/owner/repo.git`); normalised before storing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub remote: Option<String>,
    /// A checkout on this node (canonicalised before storing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    /// With `path`: that checkout is another project, not this one. It is taken out of this
    /// project into a new one, which takes the remote that was waiting to be confirmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub split: Option<bool>,
}

/// `POST /v1/projects/merge`: the person says two projects are one. `from` keeps its id but
/// loses its checkouts, remotes and records; `into` gains them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MergeProjectsRequest {
    /// The project whose records move.
    pub from: String,
    /// The project that keeps them.
    pub into: String,
}

/// `GET /v1/projects`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListProjectsResponse {
    pub projects: Vec<ProjectInfo>,
}

// ---------------------------------------------------------------------------------------
// Troubleshooting records (docs/tasks/LESSONS.md L2)

pub use crossagents_core::{
    Evidence, IssueRef, Lesson, LessonOutcome, LessonStatus, RelatedLesson, Triage, TriageRecord,
};

/// `GET /v1/projects/{id}/lessons`: newest first, narrowed by `q` and `status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListLessonsResponse {
    pub lessons: Vec<Lesson>,
    /// Review clues (LESSONS §4.4): by lesson id, the record's files changed after its last
    /// check. A clue, not a verdict — the record may still hold.
    #[serde(default)]
    pub changed: std::collections::BTreeMap<String, Vec<String>>,
}

/// `PATCH /v1/lessons/{id}`: the fields given replace the stored ones; the rest is kept.
/// Evidence and sources are never set this way.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateLessonRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub symptom: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub keywords: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub conditions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub outcome: Option<LessonOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub status: Option<LessonStatus>,
    /// With `status: "superseded"`: the lesson that replaces it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub superseded_by: Option<String>,
}

/// A problem recorded in a run (a run note under `issue/`) and, once sorted, what became
/// of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunIssue {
    pub key: String,
    pub author: AgentId,
    pub text: String,
    pub updated_at: DateTime<Utc>,
    /// Absent while nobody has sorted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub triage: Option<TriageRecord>,
}

/// `GET /v1/runs/{key}/issues`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListRunIssuesResponse {
    pub run_id: RunId,
    pub issues: Vec<RunIssue>,
    /// How many of `issues` nobody has sorted yet.
    pub unsorted: u32,
}

// ---------------------------------------------------------------------------------------
// Expertise profiles (docs/tasks/EXPERTISE.md)

pub use crossagents_core::{
    Candidate, CandidateOrigin, CandidateStatus, EntryKind, EntryStats, EntryStatus, Example,
    ExpertiseEntry, OwnerKind, SourceRef,
};

/// `POST /v1/expertise`: writes an entry, replacing the one with the same `id` when there is
/// one. Fields the request leaves out keep what the stored entry had (evidence, stats,
/// sources, creation time).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SaveExpertiseRequest {
    /// The entry to replace; a new entry is made when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub id: Option<String>,
    pub owner_kind: OwnerKind,
    pub owner_id: String,
    /// The area it belongs to (`rust`, `gui`, …); the judgement budget is counted per domain.
    pub domain: String,
    pub kind: EntryKind,
    /// One line: the judgement, the case, or the limit.
    pub title: String,
    #[serde(default)]
    pub text: String,
    /// The worked example, for `kind: "example"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub example: Option<Example>,
    /// Where it applies; empty means anywhere.
    #[serde(default)]
    pub applies_when: String,
    /// Defaults to `active`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub status: Option<EntryStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub superseded_by: Option<String>,
}

/// `GET /v1/expertise`, `GET /v1/expertise/search`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListExpertiseResponse {
    pub entries: Vec<ExpertiseEntry>,
}

/// `GET /v1/expertise/candidates`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ListCandidatesResponse {
    pub candidates: Vec<Candidate>,
}

/// What the person decided about one entry a candidate conflicts with (§4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ConflictVerdict {
    /// The old entry stays as it is.
    KeepOld,
    /// The old entry is superseded by the accepted one.
    Replace,
    /// The accepted entry takes the old one's text over, and supersedes it.
    Merge,
    /// Both apply; the old entry stays active.
    Both,
}

/// One verdict of an accept request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConflictDecision {
    /// The conflicting entry's id (one of the candidate's `conflicts_with`).
    pub entry_id: String,
    pub verdict: ConflictVerdict,
}

/// `POST /v1/expertise/candidates/{id}/accept`: every id in `conflicts_with` needs a verdict,
/// or the call is refused with 400 and nothing changes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AcceptCandidateRequest {
    #[serde(default)]
    pub decisions: Vec<ConflictDecision>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AcceptCandidateResponse {
    pub candidate: Candidate,
    /// The entry the acceptance added.
    pub entry: ExpertiseEntry,
}

/// Two judgements in one domain that read alike, so they could be one entry
/// (`crossagents_orchestrator::expertise::maintain::MergeSuggestion`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MergeSuggestion {
    /// The two entry ids, in the order they appeared in the profile.
    pub a: String,
    pub b: String,
    /// Why they could be one entry.
    pub reason: String,
}

/// One domain's judgement budget (EXPERTISE §4.3). The fields are the ones
/// `maintain::budget_report` produces, narrowed to what the wire needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DomainBudget {
    pub domain: String,
    /// Sum of `estimate_tokens` over the domain's non-superseded judgements.
    pub judgment_tokens: u32,
    pub budget: u32,
    pub over: bool,
    /// By how much the domain is over, `0` when it is not.
    pub over_by: u32,
    pub judgment_count: u32,
    /// Pairs worth merging; only filled when the domain is over budget.
    pub merge_suggestions: Vec<MergeSuggestion>,
}

/// A suggestion to re-flag an entry (EXPERTISE §4.4). Nothing is changed by it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StaleHint {
    pub entry_id: String,
    pub domain: String,
    pub title: String,
    /// What the scan would set it to: `stale` or `review`.
    pub suggested: EntryStatus,
    pub reason: String,
    /// Days since it was last used, confirmed or created.
    pub days_unused: i32,
    /// `stats.reworked / stats.used`, `null` when it was never used.
    pub rework_rate: Option<f64>,
}

/// A pending candidate and one existing entry it may contradict (EXPERTISE §4.4): one pair
/// per conflicting entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConflictPair {
    pub candidate_id: String,
    /// The existing entry the candidate may conflict with.
    pub entry_id: String,
    pub domain: String,
    /// The proposed entry's title, so the pair can be shown side by side.
    pub title: String,
    pub note: String,
}

/// The card (EXPERTISE §4.8): how the profile is doing, not how an agent is doing. Computed
/// from decided usage, so a task that pushed three entries counts three times.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExpertiseCard {
    /// `merged_first_try / (merged_first_try + reworked + dropped)`, 0–1; `null` without a
    /// decided usage yet.
    pub first_try_rate: Option<f64>,
    /// The same figure over usages decided before 30 days ago.
    pub first_try_rate_30d_ago: Option<f64>,
    /// The two rates in percentage points, when both exist.
    pub delta_points: Option<f64>,
    /// Decided usages behind `first_try_rate`.
    pub sample: u32,
    /// The sample is under ten: the rate is shown, but not to be trusted yet.
    pub small_sample: bool,
    pub merged_first_try: u32,
    pub reworked: u32,
    pub dropped: u32,
    /// All entries, by kind.
    pub entries: u32,
    pub judgments: u32,
    pub examples: u32,
    pub limits: u32,
    /// Entries by status.
    pub active: u32,
    pub stale: u32,
    pub review: u32,
    pub superseded: u32,
    /// Judgement tokens over the whole profile, and the per-domain budget.
    pub judgment_tokens: u32,
    pub judgment_budget: u32,
    /// Domains whose judgements are over budget (shown red).
    pub over_budget_domains: u32,
}

/// `GET /v1/expertise/maintenance`: the three views of `maintain` in one answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExpertiseMaintenance {
    pub budgets: Vec<DomainBudget>,
    /// Suggested status changes; the person (or the GUI) applies them.
    pub stale: Vec<StaleHint>,
    pub conflicts: Vec<ConflictPair>,
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
    /// A request reached the hub but its outcome with the agent is unknown: a write timed
    /// out with no answer, so it may or may not have arrived. Clients must not offer to
    /// resend it blindly (docs/tasks/DELIVERY-AUDIT.md §3, §4).
    OutcomeUnknown,
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
    /// Why a message was refused, when the error is about delivering one
    /// (docs/tasks/DELIVERY-AUDIT.md §3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<QueuedReason>,
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
                reason: None,
            },
        }
    }

    /// The same error, saying why a message was refused.
    pub fn with_reason(mut self, reason: QueuedReason) -> Self {
        self.error.reason = Some(reason);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_roster_preserves_optional_unusable_reason() {
        let agent = Agent {
            id: crossagents_core::AgentId::new(),
            name: "Cursor".into(),
            name_source: Default::default(),
            plugin: "cursor".into(),
            driver: None,
            model: None,
            role: Role::Worker,
            home: Default::default(),
            cwd: Default::default(),
            config: serde_json::json!({}),
            status: crossagents_core::AgentStatus::Idle,
            created_at: chrono::Utc::now(),
            profile: Default::default(),
            topic: None,
            archived_at: None,
        };
        let response = ListAgentsResponse {
            agents: vec![agent.clone(), agent],
            unusable: vec![Some("unavailable".into()), None],
            unguarded: vec![true, false],
        };
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["agents"][0]["unusable"], "unavailable");
        assert!(value["agents"][1].get("unusable").is_none());
        let restored: ListAgentsResponse = serde_json::from_value(value).unwrap();
        assert_eq!(restored, response);
    }

    /// Core types are not `#[ts(export)]` themselves; export them (and everything they
    /// reference) from here so all bindings come from one place.
    #[test]
    fn export_core_bindings() {
        let cfg = ts_rs::Config::from_env();
        Event::export_all(&cfg).unwrap();
        crossagents_core::RoundSource::export_all(&cfg).unwrap();
        crossagents_core::RoundNotice::export_all(&cfg).unwrap();
        crossagents_core::CarriedNotice::export_all(&cfg).unwrap();
        Device::export_all(&cfg).unwrap();
        crossagents_core::Task::export_all(&cfg).unwrap();
        crossagents_core::Run::export_all(&cfg).unwrap();
        crossagents_core::RunHistory::export_all(&cfg).unwrap();
        crossagents_core::ExpertiseEntry::export_all(&cfg).unwrap();
        crossagents_core::Candidate::export_all(&cfg).unwrap();
        crossagents_core::PermissionPolicy::export_all(&cfg).unwrap();
        crossagents_core::TaskBudget::export_all(&cfg).unwrap();
    }

    /// The list `/v1/info` carries: a hub that predates the field omits it, and the GUI hides
    /// these entries rather than guessing (docs/TODO.md §二).
    #[test]
    fn hub_info_lists_the_entries_older_hubs_lacked() {
        let info = HubInfo {
            version: "0.2.6".into(),
            commit: None,
            debug: false,
            api: API_VERSION.into(),
            pid: 1,
            home: "/tmp".into(),
            started_at: Utc::now(),
            features: FEATURES.iter().map(|f| (*f).to_owned()).collect(),
            machine: Some(HubMachine {
                os: DeviceOs::Macos,
                hostname: "mini".into(),
            }),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["api"], "v2");
        assert_eq!(
            json["machine"],
            serde_json::json!({"os": "macos", "hostname": "mini"})
        );
        assert_eq!(
            json["features"],
            serde_json::json!(["git", "lessons", "work", "apply", "devices"])
        );
        // An older hub's `/v1/info`, without the field, still parses (that is the whole point).
        let old: HubInfo = serde_json::from_value(serde_json::json!({
            "version": "0.2.5",
            "api": "v1",
            "pid": 1,
            "home": "/tmp",
            "started_at": Utc::now(),
        }))
        .unwrap();
        assert!(old.features.is_empty());
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
    fn extension_names_follow_the_contract() {
        assert!(check_extension_name("standup").is_ok());
        assert!(check_extension_name("stand-up-2").is_ok());
        // No uppercase, no underscore (so `__` can never occur), no leading digit or dash.
        for bad in [
            "Standup", "stand_up", "2standup", "-standup", "", "stand up",
        ] {
            assert!(check_extension_name(bad).is_err(), "{bad:?}");
        }

        assert!(check_extension_tool_name("report").is_ok());
        assert!(check_extension_tool_name("report_2_days").is_ok());
        for bad in ["Report", "report-2", "_report", "", "report__x"] {
            assert!(check_extension_tool_name(bad).is_err(), "{bad:?}");
        }
        // The separator is called out by name, not just rejected as a bad character.
        assert!(
            check_extension_tool_name("report__x")
                .unwrap_err()
                .contains("__")
        );

        assert_eq!(
            extension_tool_name("standup", "report").unwrap(),
            "standup__report"
        );
        // 46 characters fit, 47 do not.
        let name = "a".repeat(EXTENSION_TOOL_NAME_MAX - "__x".len());
        assert_eq!(
            extension_tool_name(&name, "x").unwrap().len(),
            EXTENSION_TOOL_NAME_MAX
        );
        let too_long = extension_tool_name(&format!("{name}b"), "x").unwrap_err();
        assert!(
            too_long.contains(&EXTENSION_TOOL_NAME_MAX.to_string()),
            "{too_long}"
        );
    }

    #[test]
    fn extension_shapes_are_what_clients_read() {
        let json = serde_json::to_value(ExtensionInfo {
            name: "standup".into(),
            source: "/home/.crossagents/extensions/standup".into(),
            enabled: false,
            auto_trust: false,
            current: None,
            previous_fingerprint: None,
            pending: None,
            service: None,
            error: None,
        })
        .unwrap();
        // A never-approved extension carries nothing but its state.
        assert_eq!(json["enabled"], false);
        assert!(json.get("current").is_none());
        assert!(json.get("pending").is_none());

        assert_eq!(
            serde_json::to_value(ExtensionAccess::Write).unwrap(),
            "write"
        );
        assert_eq!(
            serde_json::to_value(ExtensionCallStatus::TimedOut).unwrap(),
            "timed_out"
        );
        assert_eq!(
            serde_json::to_value(ExtensionFileChangeKind::Modified).unwrap(),
            "modified"
        );
        // The call request takes an input and nothing else; a missing one is an empty input.
        let req: ExtensionCallRequest = serde_json::from_str("{}").unwrap();
        assert!(req.input.is_null());
    }

    #[test]
    fn api_error_shape() {
        let json = serde_json::to_value(ApiError::new(ErrorCode::NotFound, "nope")).unwrap();
        assert_eq!(json["error"]["code"], "not_found");
        assert!(json["error"].get("reason").is_none());
        let refused = serde_json::to_value(
            ApiError::new(ErrorCode::AgentError, "write failed")
                .with_reason(QueuedReason::SteerWriteFailed),
        )
        .unwrap();
        assert_eq!(refused["error"]["reason"], "steer_write_failed");
    }

    /// Every reason code reads as its snake_case name in the API and is exported for
    /// clients: a client maps each one to its own sentence, and shows an unknown one raw
    /// (docs/tasks/DELIVERY-AUDIT.md §3).
    #[test]
    fn queued_reasons_are_enumerated_everywhere() {
        let all = [
            (QueuedReason::TurnRunning, "turn_running"),
            (QueuedReason::OwnerQueueAhead, "owner_queue_ahead"),
            (QueuedReason::ExternalAgent, "external_agent"),
            (QueuedReason::RuntimeNoSteer, "runtime_no_steer"),
            (QueuedReason::NoActiveTurn, "no_active_turn"),
            (QueuedReason::SteerWriteFailed, "steer_write_failed"),
            (QueuedReason::OutcomeUnknown, "outcome_unknown"),
            (QueuedReason::Archived, "archived"),
            (QueuedReason::AlreadyStarted, "already_started"),
            (QueuedReason::AttachmentRefused, "attachment_refused"),
            (QueuedReason::Empty, "empty"),
        ];
        for (reason, name) in all {
            assert_eq!(serde_json::to_value(reason).unwrap(), name);
        }
        // The export names every one of them, so a client cannot silently miss a code.
        let binding = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../bindings/QueuedReason.ts"
        ))
        .expect("bindings/QueuedReason.ts is generated by export_core_bindings");
        for (_, name) in all {
            assert!(
                binding.contains(&format!("\"{name}\"")),
                "{name} in {binding}"
            );
        }
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
