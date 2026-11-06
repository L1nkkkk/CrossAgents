use crate::state::TaskStatus;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// The bad name and what a valid one looks like.
    #[error("invalid name {0}")]
    InvalidName(String),

    #[error("invalid task transition {from:?} -> {to:?}")]
    InvalidTransition { from: TaskStatus, to: TaskStatus },

    #[error("{kind} not found: {id}")]
    NotFound { kind: &'static str, id: String },

    #[error("conflict: {0}")]
    Conflict(String),

    /// A conflict that names what is in the way: the files a run's checkout changed and
    /// deleting the run would throw away (docs/tasks/R1.md §3.2). Clients list them.
    #[error("conflict: {message}")]
    ConflictFiles { message: String, files: Vec<String> },

    /// The agent is busy: a turn is running, or messages wait for it (docs/tasks/S1.md
    /// §3.3). Try again later, or pick someone else.
    #[error("busy: {0}")]
    Busy(String),

    #[error("storage error: {0}")]
    Storage(String),

    /// The agent process or its protocol failed.
    #[error("agent error: {0}")]
    Agent(String),

    /// The driver cannot do this (check its capabilities first).
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// Whether the agent received this cannot be told: the write timed out with no answer,
    /// so it may or may not have arrived. Never retried on its own; the sender is told the
    /// outcome is unknown rather than failed (docs/tasks/DELIVERY-AUDIT.md §3).
    #[error("outcome unknown: {0}")]
    OutcomeUnknown(String),
}
