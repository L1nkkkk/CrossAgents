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

    #[error("storage error: {0}")]
    Storage(String),

    /// The agent process or its protocol failed.
    #[error("agent error: {0}")]
    Agent(String),

    /// The driver cannot do this (check its capabilities first).
    #[error("unsupported: {0}")]
    Unsupported(String),
}
