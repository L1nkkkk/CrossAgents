//! Reusable ways of connecting to agents (docs/DESIGN.md §9).
//!
//! - [`codex_native`]: `codex app-server`, one process per Profile, a thread per session.
//! - [`acp`]: `acp-generic`, any ACP agent, one process per session.

pub mod acp;
pub mod jsonrpc;
pub mod process;
