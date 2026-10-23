//! Reusable ways of connecting to agents (docs/DESIGN.md §9).
//!
//! - [`codex_native`]: `codex app-server`, one process per Profile, a thread per session.
//! - [`claude_native`]: Claude Code's stream-json control protocol, one process per session.
//! - [`codebuddy_native`]: WorkBuddy's CodeBuddy CLI on the same protocol.
//! - [`acp`]: `acp-generic`, any ACP agent, one process per session.

pub mod acp;
pub mod claude_native;
pub mod codebuddy_native;
pub mod codex_native;
pub mod jsonrpc;
pub mod process;
