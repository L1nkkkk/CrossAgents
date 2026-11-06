//! Multi-agent orchestration: the fixed pipeline, collaboration tools, workspaces and
//! verification (docs/DESIGN.md §7–§9). Depends only on `crossagents-core`; the hub plugs
//! in agents through [`AgentHost`].

pub mod approval;
pub mod attempts;
pub mod brief;
pub mod claims;
pub mod coauthor;
pub mod continuation;
pub mod delete;
pub mod engine;
pub mod expertise;
pub mod files;
pub mod gitview;
pub mod graph;
pub mod handoff;
pub mod history;
pub mod host;
pub mod human;
pub mod landing;
pub mod lessons;
pub mod machine;
pub mod messaging;
pub mod notes;
pub mod plan;
pub mod presets;
pub mod project;
pub mod projects;
pub mod protocol;
pub mod quota;
pub mod review;
pub mod running;
pub mod screenshot;
mod sessions;
pub mod setup;
pub mod tasks;
pub mod team;
pub mod tools;
pub mod verify;
pub mod work;
pub mod workspace;

#[cfg(test)]
mod tests_scheduling;
#[cfg(test)]
mod tests_verification;

pub use claims::Execution;
pub use delete::{DeleteRun, DeletedRun, DeletedRunAgents};
pub use engine::{Approval, DEFAULT_BUILD_CACHE_LIMIT, NewRun, Orchestrator, OrchestratorConfig};
pub use files::{FileError, FilesScope};
pub use gitview::GitError;
pub use graph::{Graph, RefEntry, RefKind};
pub use host::{AgentHost, ExpiringQuota, Route, RouteDecision, RouteMode};
pub use landing::LandOutcome;
pub use review::{Changes, Remark};
pub use verify::{ShellVerifier, Verifier};
pub use workspace::{GitWorkspaces, InPlaceWorkspaces, WorkspaceProvider};
