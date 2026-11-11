//! What a node and the hub say to each other (docs/tasks/B1-NODE.md §4).
//!
//! One JSON object per line. The node numbers what it sends: `Event`, `Return` and `Ask`
//! all carry a `seq`, stay in the node's unacknowledged cache until the hub `Ack`s them, and
//! are replayed in order after a reconnect. The hub ignores a `seq` it has already seen. This
//! is the same shape as the runtime host's [`wire`](../../drivers/src/host/wire.rs), on a
//! transport that is a memory channel now and a QUIC stream in B1.3.

use std::path::PathBuf;
use std::sync::Arc;

use crossagents_core::{
    Agent, AgentCommand, Attachment, AttachmentSupport, CliSource, CliStatus, CoreError, Located,
    McpServerSpec, PermissionDecision, PermissionHandler, PermissionPolicy, PermissionRequest,
    Prompt, SessionEvent, SessionSpec, TaskStatus, TurnOutcome,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// Sent by the hub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ToNode {
    /// A `Driver` / `DriverSession` call; the answer is a [`ToHub::Return`] with this `id`.
    Call { id: String, op: Op },
    /// Everything up to `seq` arrived; the node may forget it.
    Ack { seq: u64 },
    /// The hub's answer to one of the node's [`ToHub::Ask`]s.
    Answer {
        id: String,
        decision: PermissionDecision,
    },
    /// Liveness; answered with [`ToHub::Pong`].
    Ping,
}

/// Sent by the node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ToHub {
    /// First frame of every connection, before anything is replayed.
    Register {
        /// Which run of the node this is; a new one means `seq` starts over.
        #[serde(default)]
        epoch: String,
        node_version: String,
        os: String,
        hostname: String,
        plugins: Vec<PluginSnapshot>,
    },
    /// The result of a [`ToNode::Call`].
    Return {
        seq: u64,
        id: String,
        result: WireResult,
        /// The session's commands, when they changed while the call ran (`prompt`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commands: Option<Vec<AgentCommand>>,
    },
    /// One `SessionEvent`, unchanged, for the session the node named `session`.
    Event {
        seq: u64,
        session: String,
        event: SessionEvent,
        /// The session's commands, when they changed with this event.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commands: Option<Vec<AgentCommand>>,
    },
    /// A permission request the node needs the hub to decide.
    Ask {
        seq: u64,
        id: String,
        session: String,
        request: PermissionRequest,
    },
    Pong,
}

impl ToHub {
    /// The sequence number, for the frames that are kept until acknowledged.
    pub fn seq(&self) -> Option<u64> {
        match self {
            Self::Return { seq, .. } | Self::Event { seq, .. } | Self::Ask { seq, .. } => {
                Some(*seq)
            }
            Self::Register { .. } | Self::Pong => None,
        }
    }

    /// The session a reliable frame belongs to (the one a dropped-frame warning names).
    pub fn session(&self) -> Option<&str> {
        match self {
            Self::Return { .. } => None,
            Self::Event { session, .. } | Self::Ask { session, .. } => Some(session),
            Self::Register { .. } | Self::Pong => None,
        }
    }
}

/// A node's report of one plugin, as [`ToHub::Register`] carries it. B1.4 fills it in; B1.1
/// sends an empty list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSnapshot {
    pub name: String,
    pub installed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// What a [`ToNode::Call`] asks for: every `Driver` / `DriverSession` method.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// `Driver::open`.
    Open {
        plugin: String,
        /// The `driver = "..."` name, when the session names one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<String>,
        spec: Box<WireSpec>,
    },
    /// `Driver::resume`.
    Resume {
        plugin: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<String>,
        session_id: String,
        spec: Box<WireSpec>,
    },
    /// `Driver::adopt`.
    Adopt {
        plugin: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<String>,
        session_id: String,
        turn_running: bool,
        spec: Box<WireSpec>,
    },
    /// `DriverSession::adopted_turn`.
    AdoptedTurn { session: String },
    /// `DriverSession::prompt_with`.
    Prompt { session: String, prompt: WirePrompt },
    /// `DriverSession::steer`.
    Steer {
        session: String,
        message_id: String,
        prompt: WirePrompt,
    },
    /// `DriverSession::interrupt`.
    Interrupt { session: String },
    /// `DriverSession::suspend`.
    Suspend { session: String },
    /// `DriverSession::fork`.
    Fork {
        session: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at_turn: Option<String>,
        plugin: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<String>,
        spec: Box<WireSpec>,
    },
    /// `DriverSession::rollback`.
    Rollback {
        session: String,
        before_turn: String,
        revert_files: bool,
    },
    /// `DriverSession::set_model`.
    SetModel {
        session: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
    },
    /// `DriverSession::compact`.
    Compact { session: String },
    /// `DriverSession::shutdown`.
    Shutdown { session: String },
    /// `Driver::probe_cli`.
    ProbeCli {
        plugin: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<String>,
    },
    /// `Driver::refresh_cli`.
    RefreshCli {
        plugin: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<String>,
    },
    /// `Driver::cli_status`.
    CliStatus {
        plugin: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver: Option<String>,
    },
}

impl Op {
    /// The session the call names, when it names one. Its `Return` goes out behind that
    /// session's events, so a failed call cannot overtake what the driver already reported.
    pub fn session(&self) -> Option<&str> {
        match self {
            Self::AdoptedTurn { session }
            | Self::Prompt { session, .. }
            | Self::Steer { session, .. }
            | Self::Interrupt { session }
            | Self::Suspend { session }
            | Self::Fork { session, .. }
            | Self::Rollback { session, .. }
            | Self::SetModel { session, .. }
            | Self::Compact { session }
            | Self::Shutdown { session } => Some(session),
            Self::Open { .. }
            | Self::Resume { .. }
            | Self::Adopt { .. }
            | Self::ProbeCli { .. }
            | Self::RefreshCli { .. }
            | Self::CliStatus { .. } => None,
        }
    }
}

/// `SessionSpec` as it crosses the wire: everything but the event sender and the handler,
/// which the node builds itself (`permissions` becomes a boolean).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireSpec {
    pub agent: Agent,
    pub mcp_servers: Vec<McpServerSpec>,
    pub permission: PermissionPolicy,
    pub command: Option<Vec<String>>,
    pub instructions: Option<String>,
    pub env: Vec<(String, String)>,
    /// The hub gave a [`PermissionHandler`]; the node answers with [`ToHub::Ask`]s.
    pub permissions: bool,
}

impl WireSpec {
    pub fn from_spec(spec: &SessionSpec) -> Self {
        Self {
            agent: spec.agent.clone(),
            mcp_servers: spec.mcp_servers.clone(),
            permission: spec.permission,
            command: spec.command.clone(),
            instructions: spec.instructions.clone(),
            env: spec.env.clone(),
            permissions: spec.permissions.is_some(),
        }
    }

    /// The spec a driver opens with: the node's own event channel, and the handler that
    /// turns permission requests into `Ask` frames when the hub asked for them.
    pub fn into_spec(
        self,
        events: mpsc::UnboundedSender<SessionEvent>,
        permissions: Option<Arc<dyn PermissionHandler>>,
    ) -> SessionSpec {
        SessionSpec {
            agent: self.agent,
            mcp_servers: self.mcp_servers,
            permission: self.permission,
            permissions: if self.permissions { permissions } else { None },
            command: self.command,
            instructions: self.instructions,
            env: self.env,
            events,
        }
    }
}

/// A [`Prompt`] as it crosses the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WirePrompt {
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

impl From<&Prompt> for WirePrompt {
    fn from(prompt: &Prompt) -> Self {
        Self {
            text: prompt.text.clone(),
            attachments: prompt.attachments.clone(),
        }
    }
}

impl From<WirePrompt> for Prompt {
    fn from(prompt: WirePrompt) -> Self {
        Self {
            text: prompt.text,
            attachments: prompt.attachments,
        }
    }
}

/// What a call answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum WireReply {
    /// Nothing to report (`interrupt`, `shutdown`, `probe_cli`, ...).
    Unit,
    /// A session was opened, resumed, adopted or forked.
    Session { session: SessionInfo },
    /// `adopt` found no process to take over.
    NoSession,
    /// A turn ended.
    Turn { outcome: TurnOutcome },
    /// `adopted_turn`: the turn the previous hub started, when there was one.
    MaybeTurn { outcome: Option<TurnOutcome> },
    /// `cli_status`: what the driver says, when it says anything.
    CliStatus { status: Option<WireCliStatus> },
}

/// What the hub needs to know about a session the node just handed it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// The node's handle for the session; every later call names it.
    pub handle: String,
    /// The vendor's session id ([`crossagents_core::SessionEventKind::Started`]).
    pub session_id: String,
    pub attachment_support: Option<AttachmentSupport>,
    pub outlives_hub: bool,
    /// The commands the CLI reported at start, when it did.
    pub commands: Option<Vec<AgentCommand>>,
}

/// A call's result: the reply, or the error in a form that rebuilds the same `CoreError`
/// variant on the hub (`NotFound.kind` goes back to a known literal).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum WireResult {
    Ok { reply: WireReply },
    Err { error: WireError },
}

/// [`CoreError`], one variant each, so callers can keep matching variants across the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum WireError {
    InvalidName { message: String },
    InvalidTransition { from: String, to: String },
    NotFound { kind: String, id: String },
    Conflict { message: String },
    ConflictFiles { message: String, files: Vec<String> },
    Busy { message: String },
    Storage { message: String },
    Agent { message: String },
    Unsupported { message: String },
    OutcomeUnknown { message: String },
}

/// The `NotFound` kinds the workspace's drivers and services use (`grep 'kind: "'`); a
/// `&'static str` cannot be deserialized, so the wire carries the word and this table gives
/// the literal back. Every literal a driver hands out has to be here, or the hub's message
/// says "record" instead of what was missing.
const NOT_FOUND_KINDS: [&str; 34] = [
    "account",
    "add",
    "agent",
    "attachment",
    "avatar",
    "browser request",
    "codebuddy session",
    "commit",
    "device",
    "example",
    "expertise candidate",
    "expertise entry",
    "extension",
    "git repository",
    "human",
    "issue",
    "lesson",
    "login file",
    "message",
    "note",
    "plugin",
    "project",
    "question",
    "reminder",
    "run",
    "session",
    "task",
    "terminal",
    "thinking",
    "tool",
    "turn",
    "update",
    "watch",
    "work",
];

/// The word a kind this build does not know becomes; it reads as "the thing asked for".
const UNKNOWN_KIND: &str = "record";

fn not_found_kind(kind: &str) -> &'static str {
    NOT_FOUND_KINDS
        .into_iter()
        .find(|known| *known == kind)
        .unwrap_or(UNKNOWN_KIND)
}

fn status_name(status: TaskStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{status:?}").to_lowercase())
}

fn status_from(name: &str) -> TaskStatus {
    serde_json::from_value::<TaskStatus>(serde_json::Value::String(name.to_owned()))
        .unwrap_or(TaskStatus::Pending)
}

impl From<&CoreError> for WireError {
    fn from(err: &CoreError) -> Self {
        match err {
            CoreError::InvalidName(message) => Self::InvalidName {
                message: message.clone(),
            },
            CoreError::InvalidTransition { from, to } => Self::InvalidTransition {
                from: status_name(*from),
                to: status_name(*to),
            },
            CoreError::NotFound { kind, id } => Self::NotFound {
                kind: (*kind).to_owned(),
                id: id.clone(),
            },
            CoreError::Conflict(message) => Self::Conflict {
                message: message.clone(),
            },
            CoreError::ConflictFiles { message, files } => Self::ConflictFiles {
                message: message.clone(),
                files: files.clone(),
            },
            CoreError::Busy(message) => Self::Busy {
                message: message.clone(),
            },
            CoreError::Storage(message) => Self::Storage {
                message: message.clone(),
            },
            CoreError::Agent(message) => Self::Agent {
                message: message.clone(),
            },
            CoreError::Unsupported(message) => Self::Unsupported {
                message: message.clone(),
            },
            CoreError::OutcomeUnknown(message) => Self::OutcomeUnknown {
                message: message.clone(),
            },
        }
    }
}

impl From<WireError> for CoreError {
    fn from(err: WireError) -> Self {
        match err {
            WireError::InvalidName { message } => Self::InvalidName(message),
            WireError::InvalidTransition { from, to } => Self::InvalidTransition {
                from: status_from(&from),
                to: status_from(&to),
            },
            WireError::NotFound { kind, id } => Self::NotFound {
                kind: not_found_kind(&kind),
                id,
            },
            WireError::Conflict { message } => Self::Conflict(message),
            WireError::ConflictFiles { message, files } => Self::ConflictFiles { message, files },
            WireError::Busy { message } => Self::Busy(message),
            WireError::Storage { message } => Self::Storage(message),
            WireError::Agent { message } => Self::Agent(message),
            WireError::Unsupported { message } => Self::Unsupported(message),
            WireError::OutcomeUnknown { message } => Self::OutcomeUnknown(message),
        }
    }
}

/// [`CliStatus`] as it crosses the wire: the core type is not serializable (it is local
/// state, not a message), so the fields travel in their own shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireCliStatus {
    pub located: Option<WireLocated>,
    pub probing: bool,
    pub install_hint: Option<String>,
    pub docs: Option<String>,
    pub installed_via: Option<String>,
    pub install_command: Option<Vec<String>>,
    pub upgrade_command: Option<Vec<String>>,
    pub can_upgrade: bool,
    pub described_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireLocated {
    pub argv: Vec<String>,
    pub path: PathBuf,
    pub source: String,
    pub version: Option<String>,
}

impl From<CliStatus> for WireCliStatus {
    fn from(status: CliStatus) -> Self {
        Self {
            located: status.located.map(|located| WireLocated {
                argv: located.argv,
                path: located.path,
                source: located.source.as_str().to_owned(),
                version: located.version,
            }),
            probing: status.probing,
            install_hint: status.install_hint,
            docs: status.docs,
            installed_via: status.installed_via,
            install_command: status.install_command,
            upgrade_command: status.upgrade_command,
            can_upgrade: status.can_upgrade,
            described_by: status.described_by,
        }
    }
}

impl From<WireCliStatus> for CliStatus {
    fn from(status: WireCliStatus) -> Self {
        Self {
            located: status.located.map(|located| Located {
                argv: located.argv,
                path: located.path,
                source: match located.source.as_str() {
                    "config" => CliSource::Config,
                    "env" => CliSource::Env,
                    "path" => CliSource::Path,
                    "native" => CliSource::Native,
                    "bundled" => CliSource::Bundled,
                    _ => CliSource::Path,
                },
                version: located.version,
            }),
            probing: status.probing,
            install_hint: status.install_hint,
            docs: status.docs,
            installed_via: status.installed_via,
            install_command: status.install_command,
            upgrade_command: status.upgrade_command,
            can_upgrade: status.can_upgrade,
            described_by: status.described_by,
        }
    }
}

/// Who the node is, as [`ToHub::Register`] says it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterInfo {
    /// This run of the node. A restart makes a new one, so the hub knows `seq` starts over.
    pub epoch: String,
    pub node_version: String,
    pub os: String,
    pub hostname: String,
    pub plugins: Vec<PluginSnapshot>,
}

impl RegisterInfo {
    /// This process's answer: this run's epoch, the crate version and the machine's name.
    pub fn local() -> Self {
        Self {
            epoch: new_epoch(),
            node_version: env!("CARGO_PKG_VERSION").to_owned(),
            os: std::env::consts::OS.to_owned(),
            hostname: std::env::var("COMPUTERNAME")
                .or_else(|_| std::env::var("HOSTNAME"))
                .unwrap_or_else(|_| "unknown".to_owned()),
            plugins: Vec::new(),
        }
    }
}

/// A name for this run of the node: the process, the time it was built up, and a counter, so
/// two nodes started in the same nanosecond still differ.
fn new_epoch() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let nth = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    format!("{}-{nanos}-{nth}", std::process::id())
}

/// One frame as a line, newline included.
pub fn encode<T: Serialize>(frame: &T) -> String {
    let mut line = serde_json::to_string(frame).expect("frames serialize");
    line.push('\n');
    line
}

pub fn decode<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, serde_json::Error> {
    serde_json::from_str(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossagents_core::{OutputStream, PermissionDecision, StopReason};

    #[test]
    fn events_round_trip() {
        let event = SessionEvent::from(crossagents_core::SessionEventKind::Output {
            stream: OutputStream::Text,
            text: "hi".into(),
        });
        let frame = ToHub::Event {
            seq: 7,
            session: "s1".into(),
            event,
            commands: None,
        };
        let line = encode(&frame);
        assert_eq!(decode::<ToHub>(&line).unwrap(), frame);
        assert_eq!(frame.seq(), Some(7));
    }

    #[test]
    fn ask_round_trips() {
        let frame = ToHub::Ask {
            seq: 9,
            id: "a1".into(),
            session: "s1".into(),
            request: PermissionRequest {
                id: "r1".into(),
                turn_id: Some("t1".into()),
                kind: "command".into(),
                title: "rm -rf /tmp/x".into(),
            },
        };
        let line = encode(&frame);
        assert_eq!(decode::<ToHub>(&line).unwrap(), frame);
    }

    #[test]
    fn calls_round_trip() {
        let spec = WireSpec {
            agent: agent(),
            mcp_servers: vec![],
            permission: PermissionPolicy::Deny,
            command: Some(vec!["agent".into(), "acp".into()]),
            instructions: Some("be kind".into()),
            env: vec![("A".into(), "B".into())],
            permissions: true,
        };
        let call = ToNode::Call {
            id: "c1".into(),
            op: Op::Open {
                plugin: "mock".into(),
                driver: Some("acp-generic".into()),
                spec: Box::new(spec),
            },
        };
        let line = encode(&call);
        assert_eq!(decode::<ToNode>(&line).unwrap(), call);
    }

    #[test]
    fn returns_round_trip() {
        let frame = ToHub::Return {
            seq: 12,
            id: "c1".into(),
            result: WireResult::Ok {
                reply: WireReply::Turn {
                    outcome: TurnOutcome {
                        stop_reason: StopReason::EndTurn,
                        turn_id: None,
                    },
                },
            },
            commands: None,
        };
        let line = encode(&frame);
        assert_eq!(decode::<ToHub>(&line).unwrap(), frame);
    }

    #[test]
    fn answers_round_trip() {
        let frame = ToNode::Answer {
            id: "a1".into(),
            decision: PermissionDecision::AllowForSession,
        };
        let line = encode(&frame);
        assert_eq!(decode::<ToNode>(&line).unwrap(), frame);
    }

    #[test]
    fn errors_keep_their_variant() {
        let cases = [
            CoreError::InvalidName("x".into()),
            CoreError::InvalidTransition {
                from: TaskStatus::Running,
                to: TaskStatus::Merged,
            },
            CoreError::NotFound {
                kind: "agent",
                id: "a1".into(),
            },
            CoreError::Conflict("why".into()),
            CoreError::ConflictFiles {
                message: "why".into(),
                files: vec!["a".into()],
            },
            CoreError::Busy("turn running".into()),
            CoreError::Storage("disk".into()),
            CoreError::Agent("died".into()),
            CoreError::Unsupported("no pivot".into()),
            CoreError::OutcomeUnknown("timeout".into()),
        ];
        for err in cases {
            let wire = WireError::from(&err);
            let line = encode(&wire);
            let back: CoreError = decode::<WireError>(&line).unwrap().into();
            match (&err, &back) {
                (
                    CoreError::InvalidTransition { from, to },
                    CoreError::InvalidTransition { from: f2, to: t2 },
                ) => {
                    assert_eq!(from, f2);
                    assert_eq!(to, t2);
                }
                _ => assert_eq!(err.to_string(), back.to_string()),
            }
        }
        // An unknown kind becomes a literal rather than being lost.
        let back: CoreError = WireError::NotFound {
            kind: "whatever".into(),
            id: "x".into(),
        }
        .into();
        assert!(matches!(back, CoreError::NotFound { kind: "record", .. }));
    }

    #[test]
    fn not_found_kinds_keep_the_words_drivers_use() {
        // These are what drivers actually return (`grep 'kind: "'`); a missing entry
        // would turn the hub's message into "record".
        for kind in [
            "turn",
            "codebuddy session",
            "attachment",
            "tool",
            "login file",
            "git repository",
            "thinking",
            "terminal",
        ] {
            let wire = WireError::from(&CoreError::NotFound {
                kind,
                id: "x".into(),
            });
            let back: CoreError = decode::<WireError>(&encode(&wire)).unwrap().into();
            match back {
                CoreError::NotFound { kind: back, .. } => assert_eq!(back, kind),
                other => panic!("{kind} came back as {other:?}"),
            }
        }
    }

    #[test]
    fn register_carries_the_epoch() {
        let frame = ToHub::Register {
            epoch: "run-1".into(),
            node_version: "0.2.20".into(),
            os: "linux".into(),
            hostname: "box".into(),
            plugins: vec![PluginSnapshot {
                name: "mock".into(),
                installed: true,
                version: Some("1.0.0".into()),
            }],
        };
        let line = encode(&frame);
        assert_eq!(decode::<ToHub>(&line).unwrap(), frame);
    }

    #[test]
    fn cli_status_round_trips() {
        let status = CliStatus {
            located: Some(Located {
                argv: vec!["node".into(), "cli.js".into()],
                path: PathBuf::from("/usr/bin/node"),
                source: CliSource::Path,
                version: Some("1.2.3".into()),
            }),
            probing: false,
            install_hint: Some("npm i -g x".into()),
            docs: None,
            installed_via: Some("npm".into()),
            install_command: Some(vec!["npm".into()]),
            upgrade_command: None,
            can_upgrade: true,
            described_by: Some("built-in".into()),
        };
        let wire = WireCliStatus::from(status.clone());
        assert_eq!(CliStatus::from(wire), status);
    }

    fn agent() -> Agent {
        use crossagents_core::{AgentId, AgentStatus, NameSource, Role};
        Agent {
            id: AgentId::new(),
            name: "mocky".into(),
            name_source: NameSource::Chosen,
            plugin: "mock".into(),
            driver: None,
            model: None,
            role: Role::Worker,
            home: PathBuf::from("/tmp"),
            cwd: PathBuf::from("/tmp"),
            config: serde_json::Value::Null,
            status: AgentStatus::Idle,
            created_at: chrono::Utc::now(),
            profile: Default::default(),
            topic: None,
            archived_at: None,
        }
    }
}
