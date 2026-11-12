use super::{WirePrompt, WireSpec};
use serde::{Deserialize, Serialize};

/// What a [`super::ToNode::Call`] asks for: every `Driver` / `DriverSession` method.
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
    /// An operation unknown to this version.
    #[serde(other)]
    Unknown,
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
            Self::Unknown
            | Self::Open { .. }
            | Self::Resume { .. }
            | Self::Adopt { .. }
            | Self::ProbeCli { .. }
            | Self::RefreshCli { .. }
            | Self::CliStatus { .. } => None,
        }
    }
}
