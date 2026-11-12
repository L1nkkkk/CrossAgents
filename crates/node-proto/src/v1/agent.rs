use crossagents_core::{Prompt, SessionSpec};
use serde::{Deserialize, Serialize};

/// `SessionSpec` as it crosses the wire: everything but the event sender and the handler,
/// which the node builds itself (`permissions` becomes a boolean).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireSpec {
    pub agent: WireAgent,
    pub mcp_servers: Vec<WireMcpServerSpec>,
    pub permission: WirePermissionPolicy,
    pub command: Option<Vec<String>>,
    pub instructions: Option<String>,
    pub env: Vec<(String, String)>,
    /// The hub gave a [`crossagents_core::PermissionHandler`]; the node answers with [`super::ToHub::Ask`]s.
    pub permissions: bool,
}

impl WireSpec {
    pub fn from_spec(spec: &SessionSpec) -> Self {
        Self {
            agent: (&spec.agent).into(),
            mcp_servers: spec.mcp_servers.iter().map(Into::into).collect(),
            permission: (&spec.permission).into(),
            command: spec.command.clone(),
            instructions: spec.instructions.clone(),
            env: spec.env.clone(),
            permissions: spec.permissions.is_some(),
        }
    }
}

/// A [`Prompt`] as it crosses the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WirePrompt {
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<WireAttachment>,
}

impl From<&Prompt> for WirePrompt {
    fn from(prompt: &Prompt) -> Self {
        Self {
            text: prompt.text.clone(),
            attachments: prompt.attachments.iter().map(Into::into).collect(),
        }
    }
}

impl From<WirePrompt> for Prompt {
    fn from(prompt: WirePrompt) -> Self {
        Self {
            text: prompt.text,
            attachments: prompt.attachments.into_iter().map(Into::into).collect(),
        }
    }
}

/// What the hub needs to know about a session the node just handed it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// The node's handle for the session; every later call names it.
    pub handle: String,
    /// The vendor's session id ([`crossagents_core::SessionEventKind::Started`]).
    pub session_id: String,
    pub attachment_support: Option<WireAttachmentSupport>,
    pub outlives_hub: bool,
    /// The commands the CLI reported at start, when it did.
    pub commands: Option<Vec<WireAgentCommand>>,
}

use super::WireStopReason;
use crossagents_core as core;
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WireNameSource {
    #[default]
    Chosen,
    Generated,
}
impl From<&core::NameSource> for WireNameSource {
    fn from(v: &core::NameSource) -> Self {
        match v {
            core::NameSource::Chosen => Self::Chosen,
            core::NameSource::Generated => Self::Generated,
        }
    }
}
impl From<WireNameSource> for core::NameSource {
    fn from(v: WireNameSource) -> Self {
        match v {
            WireNameSource::Chosen => Self::Chosen,
            WireNameSource::Generated => Self::Generated,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WireRole {
    Planner,
    #[default]
    Worker,
    Reviewer,
}
impl From<&core::Role> for WireRole {
    fn from(v: &core::Role) -> Self {
        match v {
            core::Role::Planner => Self::Planner,
            core::Role::Worker => Self::Worker,
            core::Role::Reviewer => Self::Reviewer,
        }
    }
}
impl From<WireRole> for core::Role {
    fn from(v: WireRole) -> Self {
        match v {
            WireRole::Planner => Self::Planner,
            WireRole::Worker => Self::Worker,
            WireRole::Reviewer => Self::Reviewer,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireAgentStatus {
    Stopped,
    Starting,
    Idle,
    Running,
    Error,
}
impl From<&core::AgentStatus> for WireAgentStatus {
    fn from(v: &core::AgentStatus) -> Self {
        match v {
            core::AgentStatus::Stopped => Self::Stopped,
            core::AgentStatus::Starting => Self::Starting,
            core::AgentStatus::Idle => Self::Idle,
            core::AgentStatus::Running => Self::Running,
            core::AgentStatus::Error => Self::Error,
        }
    }
}
impl From<WireAgentStatus> for core::AgentStatus {
    fn from(v: WireAgentStatus) -> Self {
        match v {
            WireAgentStatus::Stopped => Self::Stopped,
            WireAgentStatus::Starting => Self::Starting,
            WireAgentStatus::Idle => Self::Idle,
            WireAgentStatus::Running => Self::Running,
            WireAgentStatus::Error => Self::Error,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WirePermissionPolicy {
    Bypass,
    Deny,
    Guard,
}
impl From<&core::PermissionPolicy> for WirePermissionPolicy {
    fn from(v: &core::PermissionPolicy) -> Self {
        match v {
            core::PermissionPolicy::Bypass => Self::Bypass,
            core::PermissionPolicy::Deny => Self::Deny,
            core::PermissionPolicy::Guard => Self::Guard,
        }
    }
}
impl From<WirePermissionPolicy> for core::PermissionPolicy {
    fn from(v: WirePermissionPolicy) -> Self {
        match v {
            WirePermissionPolicy::Bypass => Self::Bypass,
            WirePermissionPolicy::Deny => Self::Deny,
            WirePermissionPolicy::Guard => Self::Guard,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireAgentCommandKind {
    Command,
    Skill,
}
impl From<&core::AgentCommandKind> for WireAgentCommandKind {
    fn from(v: &core::AgentCommandKind) -> Self {
        match v {
            core::AgentCommandKind::Command => Self::Command,
            core::AgentCommandKind::Skill => Self::Skill,
        }
    }
}
impl From<WireAgentCommandKind> for core::AgentCommandKind {
    fn from(v: WireAgentCommandKind) -> Self {
        match v {
            WireAgentCommandKind::Command => Self::Command,
            WireAgentCommandKind::Skill => Self::Skill,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireAgentProfile {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub avatar_image: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub persona: Option<String>,
}
impl From<&core::AgentProfile> for WireAgentProfile {
    fn from(v: &core::AgentProfile) -> Self {
        Self {
            display_name: v.display_name.clone(),
            title: v.title.clone(),
            avatar: v.avatar.clone(),
            avatar_image: v.avatar_image.clone(),
            color: v.color.clone(),
            persona: v.persona.clone(),
        }
    }
}
impl From<WireAgentProfile> for core::AgentProfile {
    fn from(v: WireAgentProfile) -> Self {
        Self {
            display_name: v.display_name,
            title: v.title,
            avatar: v.avatar,
            avatar_image: v.avatar_image,
            color: v.color,
            persona: v.persona,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireAgent {
    pub id: uuid::Uuid,
    pub name: String,
    #[serde(default)]
    pub name_source: WireNameSource,
    pub plugin: String,
    pub driver: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub role: WireRole,
    #[serde(default)]
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub config: serde_json::Value,
    pub status: WireAgentStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub profile: WireAgentProfile,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub archived_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub node: Option<String>,
}
impl From<&core::Agent> for WireAgent {
    fn from(v: &core::Agent) -> Self {
        Self {
            id: v.id.as_uuid(),
            name: v.name.clone(),
            name_source: (&v.name_source).into(),
            plugin: v.plugin.clone(),
            driver: v.driver.clone(),
            model: v.model.clone(),
            role: (&v.role).into(),
            home: v.home.clone(),
            cwd: v.cwd.clone(),
            config: v.config.clone(),
            status: (&v.status).into(),
            created_at: v.created_at.clone(),
            profile: (&v.profile).into(),
            topic: v.topic.clone(),
            archived_at: v.archived_at.clone(),
            node: v.node.clone(),
        }
    }
}
impl From<WireAgent> for core::Agent {
    fn from(v: WireAgent) -> Self {
        Self {
            id: v.id.into(),
            name: v.name,
            name_source: v.name_source.into(),
            plugin: v.plugin,
            driver: v.driver,
            model: v.model,
            role: v.role.into(),
            home: v.home,
            cwd: v.cwd,
            config: v.config,
            status: v.status.into(),
            created_at: v.created_at,
            profile: v.profile.into(),
            topic: v.topic,
            archived_at: v.archived_at,
            node: v.node,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireMcpServerSpec {
    pub name: String,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}
impl From<&core::McpServerSpec> for WireMcpServerSpec {
    fn from(v: &core::McpServerSpec) -> Self {
        Self {
            name: v.name.clone(),
            command: v.command.clone(),
            args: v.args.clone(),
            env: v.env.clone(),
        }
    }
}
impl From<WireMcpServerSpec> for core::McpServerSpec {
    fn from(v: WireMcpServerSpec) -> Self {
        Self {
            name: v.name,
            command: v.command,
            args: v.args,
            env: v.env,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireAttachment {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub path: PathBuf,
}
impl From<&core::Attachment> for WireAttachment {
    fn from(v: &core::Attachment) -> Self {
        Self {
            id: v.id.clone(),
            name: v.name.clone(),
            mime: v.mime.clone(),
            size: v.size.clone(),
            path: v.path.clone(),
        }
    }
}
impl From<WireAttachment> for core::Attachment {
    fn from(v: WireAttachment) -> Self {
        Self {
            id: v.id,
            name: v.name,
            mime: v.mime,
            size: v.size,
            path: v.path,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireAttachmentSupport {
    pub image: bool,
    pub pdf: bool,
    pub text: bool,
}
impl From<&core::AttachmentSupport> for WireAttachmentSupport {
    fn from(v: &core::AttachmentSupport) -> Self {
        Self {
            image: v.image.clone(),
            pdf: v.pdf.clone(),
            text: v.text.clone(),
        }
    }
}
impl From<WireAttachmentSupport> for core::AttachmentSupport {
    fn from(v: WireAttachmentSupport) -> Self {
        Self {
            image: v.image,
            pdf: v.pdf,
            text: v.text,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireAgentCommand {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub kind: WireAgentCommandKind,
}
impl From<&core::AgentCommand> for WireAgentCommand {
    fn from(v: &core::AgentCommand) -> Self {
        Self {
            name: v.name.clone(),
            description: v.description.clone(),
            kind: (&v.kind).into(),
        }
    }
}
impl From<WireAgentCommand> for core::AgentCommand {
    fn from(v: WireAgentCommand) -> Self {
        Self {
            name: v.name,
            description: v.description,
            kind: v.kind.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireTurnOutcome {
    pub stop_reason: WireStopReason,
    pub turn_id: Option<String>,
}
impl From<&core::TurnOutcome> for WireTurnOutcome {
    fn from(v: &core::TurnOutcome) -> Self {
        Self {
            stop_reason: (&v.stop_reason).into(),
            turn_id: v.turn_id.clone(),
        }
    }
}
impl From<WireTurnOutcome> for core::TurnOutcome {
    fn from(v: WireTurnOutcome) -> Self {
        Self {
            stop_reason: v.stop_reason.into(),
            turn_id: v.turn_id,
        }
    }
}
