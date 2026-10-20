use crate::state::TaskStatus;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("invalid name {0:?}: must match ^[a-z][a-z0-9_-]{{0,31}}$")]
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
}
