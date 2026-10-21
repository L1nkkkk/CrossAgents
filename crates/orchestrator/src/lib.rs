//! Multi-agent orchestration: the fixed pipeline, collaboration tools, workspaces and
//! verification (docs/DESIGN.md §7–§9). Depends only on `crossagents-core`; the hub plugs
//! in agents through [`AgentHost`].

pub mod engine;
pub mod host;
pub mod messaging;
pub mod plan;
pub mod prompts;
pub mod tools;
pub mod verify;
pub mod workspace;

pub use engine::{NewRun, Orchestrator, OrchestratorConfig};
pub use host::AgentHost;
pub use verify::{ShellVerifier, Verifier};
pub use workspace::{GitWorkspaces, WorkspaceProvider};
