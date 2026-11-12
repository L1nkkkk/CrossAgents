use crossagents_core::{
    Agent, AgentCommand, Attachment, AttachmentSupport, McpServerSpec, PermissionPolicy, Prompt,
    SessionSpec,
};
use serde::{Deserialize, Serialize};

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
    /// The hub gave a [`crossagents_core::PermissionHandler`]; the node answers with [`super::ToHub::Ask`]s.
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
