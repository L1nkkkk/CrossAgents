//! Static extension discovery and approval. Never executes extension code.
pub mod manifest;
pub mod registry;
pub mod runner;
pub mod snapshot;
pub use registry::{EnabledVersion, Registry, SnapshotLease};
pub use runner::{CallError, Caller, Limits, Runner};
