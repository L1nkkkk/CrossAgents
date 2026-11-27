//! Reusable ways of connecting to agents (docs/DESIGN.md §9).
//!
//! - [`codex_native`]: `codex app-server`, one process per Profile, a thread per session.
//! - [`claude_native`]: Claude Code's stream-json control protocol, one process per session.
//! - [`codebuddy_native`]: WorkBuddy's CodeBuddy CLI on the same protocol.
//! - [`with_native`]: With's `with-cli chat -o stream-json`, one process per turn.
//! - [`cursor_native`]: Cursor's `agent -p`, one process per turn, resumed by chat id.
//! - [`gemini_native`]: Gemini CLI's `gemini -p -o stream-json`, one process per turn,
//!   resumed by its session id.
//! - [`antigravity_native`]: Antigravity CLI's `agy` stream-json, one process per session,
//!   resumed by its conversation id.
//! - [`qoder_native`]: Qoder CLI's `qodercli` on the stream-json control protocol.
//! - [`acp`]: `acp-generic`, any ACP agent, one process per session.
//! - [`mcp_config`]: what the CLI's own MCP configuration says, beside what the hub passes.
//! - [`hooks`]: whether a session may run the CLI's own hooks (off unless the agent allows).
//!
//! [`launch`] starts agent processes, directly or in a [`host`] that outlives the hub
//! (docs/PROCESS-SUPERVISION.md).

pub mod acp;
pub mod antigravity_native;
mod argv;
pub mod claude_native;
pub mod codebuddy_native;
pub mod codex_native;
#[cfg(windows)]
mod conpty;
pub mod cursor_native;
pub mod gemini_native;
pub mod hooks;
pub mod host;
pub mod install;
pub mod jsonrpc;
pub mod launch;
pub mod locate;
pub mod mcp_config;
pub mod process;
pub mod qoder_native;
pub mod with_native;
