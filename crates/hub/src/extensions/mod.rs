//! Static extension discovery and approval. Never executes extension code.
pub mod manifest;
pub mod registry;
pub mod runner;
pub mod snapshot;
pub use registry::{EnabledVersion, Registry, SnapshotLease};
pub use runner::{CallError, Caller, Limits, Runner};

pub mod creator;
/// Event hooks (`[[on]]`): one process per event, observe only.
pub mod hooks;
/// Resident processes (`[service]`): one per enabled extension, supervised.
pub mod service;
/// Stream rules (`[[rules]]`): rule files read into a session, as the project's own are.
pub mod rules;
pub use hooks::{HookRate, Hooks};
pub use service::Services;
