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

    #[error("storage error: {0}")]
    Storage(String),

    /// The agent process or its protocol failed.
    #[error("agent error: {0}")]
    Agent(String),

    /// The driver cannot do this (check its capabilities first).
    #[error("unsupported: {0}")]
    Unsupported(String),
}
