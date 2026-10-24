//! Multi-agent orchestration: the fixed pipeline, collaboration tools, workspaces and
//! verification (docs/DESIGN.md §7–§9). Depends only on `crossagents-core`; the hub plugs
//! in agents through [`AgentHost`].

pub mod approval;
pub mod brief;
pub mod engine;
pub mod host;
pub mod human;
pub mod landing;
pub mod messaging;
pub mod notes;
pub mod plan;
pub mod protocol;
pub mod quota;
pub mod review;
pub mod screenshot;
pub mod tasks;
pub mod team;
pub mod tools;
pub mod verify;
pub mod workspace;

pub use engine::{NewRun, Orchestrator, OrchestratorConfig};
pub use landing::LandOutcome;
pub use host::{AgentHost, Route};
pub use review::{Changes, Remark};
pub use verify::{ShellVerifier, Verifier};
pub use workspace::{GitWorkspaces, InPlaceWorkspaces, WorkspaceProvider};
