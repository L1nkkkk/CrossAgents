use super::{SessionInfo, WireCliStatus, WireError, WireTurnOutcome};
use serde::{Deserialize, Serialize};

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
    Turn { outcome: WireTurnOutcome },
    /// `adopted_turn`: the turn the previous hub started, when there was one.
    MaybeTurn { outcome: Option<WireTurnOutcome> },
    /// `cli_status`: what the driver says, when it says anything.
    CliStatus { status: Option<WireCliStatus> },
}

/// A call's result: the reply, or the error in a form that rebuilds the same `CoreError`
/// variant on the hub (`NotFound.kind` goes back to a known literal).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum WireResult {
    Ok { reply: WireReply },
    Err { error: WireError },
}
