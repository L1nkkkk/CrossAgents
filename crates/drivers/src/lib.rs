//! Reusable ways of connecting to agents (docs/DESIGN.md §9).
//!
//! - [`codex_native`]: `codex app-server`, one process per Profile, a thread per session.
//! - [`claude_native`]: Claude Code's stream-json control protocol, one process per session.
//! - [`codebuddy_native`]: WorkBuddy's CodeBuddy CLI on the same protocol.
//! - [`with_native`]: With's `with-cli chat -o stream-json`, one process per turn.
//! - [`cursor_native`]: Cursor's `agent -p`, one process per turn, resumed by chat id.
//! - [`acp`]: `acp-generic`, any ACP agent, one process per session.
//! - [`mcp_config`]: what the CLI's own MCP configuration says, beside what the hub passes.
//!
//! [`launch`] starts agent processes, directly or in a [`host`] that outlives the hub
//! (docs/PROCESS-SUPERVISION.md).

pub mod acp;
pub mod claude_native;
pub mod codebuddy_native;
pub mod codex_native;
pub mod cursor_native;
pub mod host;
pub mod install;
pub mod jsonrpc;
pub mod launch;
pub mod locate;
pub mod mcp_config;
pub mod process;
pub mod with_native;
