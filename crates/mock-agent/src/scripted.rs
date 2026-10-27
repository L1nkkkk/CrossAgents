//! Running a session's [`script`] directives: files are written in the session's cwd, tool
//! calls go to the session's first MCP server. Shared by the ACP and stream-json mocks.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use crossagents_drivers::jsonrpc::{RpcError, RpcHandler, RpcPeer};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::script::{self, Action, Directive};

/// How to start an MCP server.
#[derive(Debug, Clone, Default)]
pub struct McpLaunch {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// A client side that ignores whatever an MCP server sends unprompted.
struct Quiet;

#[async_trait]
impl RpcHandler for Quiet {
    async fn request(&self, method: &str, _: Value) -> Result<Value, RpcError> {
        Err(RpcError::method_not_found(method))
    }
    async fn notification(&self, _: &str, _: Value) {}
}

#[derive(Default)]
pub struct Scripted {
    pub cwd: Mutex<PathBuf>,
    /// The session's first MCP server.
    pub server: Mutex<Option<McpLaunch>>,
    mcp: Mutex<Option<(RpcPeer, tokio::process::Child)>>,
    last: Mutex<Vec<Directive>>,
}

impl Scripted {
    /// Connects to the session's MCP server on first use.
    async fn mcp(&self) -> Result<RpcPeer, String> {
        let mut slot = self.mcp.lock().await;
        if let Some((peer, _)) = slot.as_ref() {
            return Ok(peer.clone());
        }
        let server = self
            .server
            .lock()
            .await
            .clone()
            .ok_or("no MCP server in this session")?;
        let mut cmd = tokio::process::Command::new(&server.command);
        cmd.args(&server.args).envs(server.env.iter().cloned());
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("cannot start MCP server: {e}"))?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let (peer, _) = RpcPeer::spawn(stdout, stdin, Arc::new(Quiet));
        let _: Value = peer
            .request(
                "initialize",
                json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "mock", "version": "0" } }),
            )
            .await
            .map_err(|e| e.to_string())?;
        let _ = peer.notify("notifications/initialized", json!({}));
        *slot = Some((peer.clone(), child));
        Ok(peer)
    }

    /// Runs the directives in `text` that apply to `model`; returns a line per action for
    /// the transcript. A prompt without directives that carries a `task_id:` line repeats
    /// the session's last directives (for retries).
    pub async fn run(&self, text: &str, model: &str) -> Vec<String> {
        let (task_id, attempt) = script::context(text);
        let mut directives = script::parse(text, task_id.as_deref(), attempt);
        if directives.is_empty() && task_id.is_some() {
            directives = self.last.lock().await.clone();
        } else if !directives.is_empty() {
            *self.last.lock().await = directives.clone();
        }
        let mut log = Vec::new();
        for directive in directives.iter().filter(|d| d.applies(attempt, model)) {
            match &directive.action {
                Action::Write { path, content } => {
                    let full = self.cwd.lock().await.join(path);
                    if let Some(parent) = full.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    match std::fs::write(&full, content) {
                        Ok(()) => log.push(format!("wrote {path}")),
                        Err(e) => log.push(format!("cannot write {path}: {e}")),
                    }
                }
                Action::Call { tool, args } => {
                    // Remembered directives were parsed for an earlier prompt; fill in this
                    // prompt's task id. Without one, placeholders pass through untouched.
                    let args = match &task_id {
                        Some(id) => {
                            serde_json::from_str(&args.to_string().replace("{task_id}", id))
                                .unwrap_or(args.clone())
                        }
                        None => args.clone(),
                    };
                    let result = match self.mcp().await {
                        Ok(peer) => peer
                            .request::<_, Value>(
                                "tools/call",
                                json!({ "name": tool, "arguments": args }),
                            )
                            .await
                            .map(|r| {
                                r["content"][0]["text"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_owned()
                            })
                            .map_err(|e| e.to_string()),
                        Err(e) => Err(e),
                    };
                    match result {
                        Ok(text) => log.push(format!("called {tool}: {text}")),
                        Err(e) => log.push(format!("call {tool} failed: {e}")),
                    }
                }
            }
        }
        log
    }
}
