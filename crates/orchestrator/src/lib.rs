//! Multi-agent orchestration: the fixed pipeline, collaboration tools, workspaces and
//! verification (docs/DESIGN.md §7–§9). Depends only on `crossagents-core`; the hub plugs
//! in agents through [`AgentHost`].

pub mod approval;
pub mod brief;
pub mod delete;
pub mod engine;
pub mod handoff;
pub mod host;
pub mod human;
pub mod landing;
pub mod messaging;
pub mod notes;
pub mod plan;
pub mod project;
pub mod protocol;
pub mod quota;
pub mod review;
pub mod screenshot;
pub mod tasks;
pub mod team;
pub mod tools;
pub mod verify;
pub mod work;
pub mod workspace;

pub use delete::{DeleteRun, DeletedRun};
pub use engine::{Approval, NewRun, Orchestrator, OrchestratorConfig};
pub use host::{AgentHost, Route};
pub use landing::LandOutcome;
pub use review::{Changes, Remark};
pub use verify::{ShellVerifier, Verifier};
pub use workspace::{GitWorkspaces, InPlaceWorkspaces, WorkspaceProvider};
