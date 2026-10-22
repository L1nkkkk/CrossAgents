//! A scriptable ACP agent for tests. Each prompt is answered with `echo(model=<m>): <text>`
//! after acting on any of these markers:
//!
//! - `[tool]`  report a tool call, ask for permission, then echo the chosen option
//! - `[slow]`  keep the turn running until cancelled
//! - `[crash]` exit the process immediately
//! - `[write ...]` / `[call ...]` directives, see [`script`]. A prompt without directives
//!   that carries a `task_id:` line repeats the session's last directives (for retries).
//!
//! `crossagents-mock-agent app-server` speaks the `codex app-server` protocol instead, see
//! [`app_server`].

mod app_server;
mod script;

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use crossagents_drivers::jsonrpc::{RpcError, RpcHandler, RpcPeer};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify};

use crate::script::{Action, Directive};

struct MockAgent {
    peer: Mutex<Option<RpcPeer>>,
    model: Mutex<String>,
    mode: Mutex<String>,
    cancelled: Notify,
    cwd: Mutex<PathBuf>,
    mcp_servers: Mutex<Vec<Value>>,
    mcp: Mutex<Option<(RpcPeer, tokio::process::Child)>>,
    last_directives: Mutex<Vec<Directive>>,
    turns: Mutex<u32>,
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

impl MockAgent {
    /// Connects to the session's first MCP server on first use.
    async fn mcp(&self) -> Result<RpcPeer, String> {
        let mut slot = self.mcp.lock().await;
        if let Some((peer, _)) = slot.as_ref() {
            return Ok(peer.clone());
        }
        let server = self
            .mcp_servers
            .lock()
            .await
            .first()
            .cloned()
            .ok_or("no MCP server in this session")?;
        let mut cmd = tokio::process::Command::new(server["command"].as_str().unwrap_or_default());
        for arg in server["args"].as_array().into_iter().flatten() {
            cmd.arg(arg.as_str().unwrap_or_default());
        }
        for var in server["env"].as_array().into_iter().flatten() {
            cmd.env(
                var["name"].as_str().unwrap_or_default(),
                var["value"].as_str().unwrap_or_default(),
            );
        }
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

    /// Runs the directives that apply; returns a line per action for the transcript.
    async fn run_directives(&self, text: &str) -> Vec<String> {
        let (task_id, attempt) = script::context(text);
        let mut directives = script::parse(text, task_id.as_deref(), attempt);
        if directives.is_empty() && task_id.is_some() {
            directives = self.last_directives.lock().await.clone();
        } else if !directives.is_empty() {
            *self.last_directives.lock().await = directives.clone();
        }
        let model = self.model.lock().await.clone();
        let mut log = Vec::new();
        for directive in directives.iter().filter(|d| d.applies(attempt, &model)) {
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

    async fn peer(&self) -> RpcPeer {
        // `main` stores the peer right after spawning it; wait out that tiny window.
        loop {
            if let Some(peer) = self.peer.lock().await.clone() {
                return peer;
            }
            tokio::task::yield_now().await;
        }
    }

    async fn update(&self, session_id: &str, update: Value) {
        let _ = self.peer().await.notify(
            "session/update",
            json!({ "sessionId": session_id, "update": update }),
        );
    }

    async fn say(&self, session_id: &str, text: &str) {
        self.update(
            session_id,
            json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text } }),
        )
        .await;
    }

    async fn prompt(&self, params: Value) -> Result<Value, RpcError> {
        let session_id = params["sessionId"].as_str().unwrap_or_default().to_owned();
        let blocks = params["prompt"].as_array().cloned().unwrap_or_default();
        let text = blocks
            .last()
            .and_then(|b| b["text"].as_str())
            .unwrap_or_default()
            .to_owned();

        if text.contains("[crash]") {
            std::process::exit(3);
        }
        if text.contains("[slow]") {
            self.say(&session_id, "working").await;
            tokio::select! {
                _ = self.cancelled.notified() => return Ok(json!({ "stopReason": "cancelled" })),
                _ = tokio::time::sleep(Duration::from_secs(600)) => {}
            }
        }
        if text.contains("[tool]") {
            self.update(
                &session_id,
                json!({
                    "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Run mock tool",
                    "kind": "execute", "status": "pending"
                }),
            )
            .await;
            let answer: Value = self
                .peer()
                .await
                .request(
                    "session/request_permission",
                    json!({
                        "sessionId": session_id,
                        "toolCall": { "toolCallId": "t1" },
                        "options": [
                            { "optionId": "yes", "name": "Allow", "kind": "allow_once" },
                            { "optionId": "no", "name": "Reject", "kind": "reject_once" }
                        ]
                    }),
                )
                .await?;
            let chosen = answer["outcome"]["optionId"]
                .as_str()
                .unwrap_or("cancelled");
            self.update(
                &session_id,
                json!({
                    "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed"
                }),
            )
            .await;
            self.say(&session_id, &format!("permission={chosen} "))
                .await;
        }

        // `[env NAME]` reports an environment variable (tests check account binding).
        if let Some(rest) = text.split("[env ").nth(1)
            && let Some(name) = rest.split(']').next()
        {
            let value = std::env::var(name).unwrap_or_default();
            self.say(&session_id, &format!("env {name}={value}\n"))
                .await;
        }
        let all_text: Vec<&str> = blocks.iter().filter_map(|b| b["text"].as_str()).collect();
        for line in self.run_directives(&all_text.join("\n")).await {
            self.say(&session_id, &format!("{line}\n")).await;
        }

        let model = self.model.lock().await.clone();
        let mode = self.mode.lock().await.clone();
        // Every session gets who it is and its role's guide; only a profile card (a role on
        // the team, or a persona) is noted, so tests can see a persona arrive.
        let card = blocks.len() > 1
            && blocks[0]["text"].as_str().is_some_and(|t| {
                t.starts_with("You are ")
                    && (t.contains("Your role on the team") || t.contains("About you: "))
            });
        if card {
            self.say(&session_id, "[instructions received] ").await;
        }
        self.say(&session_id, &format!("echo(model={model},mode={mode}): "))
            .await;
        self.say(&session_id, &text).await;
        // Cumulative session cost, as ACP agents report it: opus costs 10x haiku per turn.
        let turns = {
            let mut turns = self.turns.lock().await;
            *turns += 1;
            *turns
        };
        let rate = if model.contains("haiku") { 0.001 } else { 0.01 };
        self.update(
            &session_id,
            json!({
                "sessionUpdate": "usage_update", "used": 42, "size": 1000,
                "cost": { "amount": rate * f64::from(turns), "currency": "USD" }
            }),
        )
        .await;
        Ok(json!({ "stopReason": "end_turn" }))
    }
}

#[async_trait]
impl RpcHandler for MockAgent {
    async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => Ok(json!({
                "protocolVersion": 1,
                "agentCapabilities": { "loadSession": false, "mcpCapabilities": { "http": false, "sse": false } },
                "agentInfo": { "name": "crossagents-mock-agent", "version": "0" }
            })),
            "session/new" => {
                *self.cwd.lock().await = PathBuf::from(params["cwd"].as_str().unwrap_or("."));
                *self.mcp_servers.lock().await =
                    params["mcpServers"].as_array().cloned().unwrap_or_default();
                Ok(json!({
                "sessionId": "mock-session",
                "modes": {
                    "currentModeId": "default",
                    "availableModes": [ { "id": "default", "name": "Default" }, { "id": "bypassPermissions", "name": "Bypass" } ]
                },
                "configOptions": [{
                    "id": "model", "name": "Model", "category": "model", "type": "select",
                    "currentValue": "mock-opus",
                    "options": [ { "value": "mock-opus", "name": "Mock Opus" }, { "value": "mock-haiku", "name": "Mock Haiku" } ]
                }]
                }))
            }
            "session/set_mode" => {
                *self.mode.lock().await = params["modeId"].as_str().unwrap_or_default().into();
                Ok(json!({}))
            }
            "session/set_config_option" => {
                *self.model.lock().await = params["value"].as_str().unwrap_or_default().into();
                Ok(json!({ "configOptions": [] }))
            }
            "session/prompt" => self.prompt(params).await,
            _ => Err(RpcError::method_not_found(method)),
        }
    }

    async fn notification(&self, method: &str, _params: Value) {
        if method == "session/cancel" {
            self.cancelled.notify_waiters();
        }
    }
}

#[tokio::main]
async fn main() {
    if std::env::args().nth(1).as_deref() == Some("app-server") {
        app_server::run().await;
        return;
    }
    let agent = Arc::new(MockAgent {
        peer: Mutex::new(None),
        model: Mutex::new("mock-opus".into()),
        mode: Mutex::new("default".into()),
        cancelled: Notify::new(),
        cwd: Mutex::new(PathBuf::from(".")),
        mcp_servers: Mutex::default(),
        mcp: Mutex::default(),
        last_directives: Mutex::default(),
        turns: Mutex::default(),
    });
    let (peer, reader) = RpcPeer::spawn(tokio::io::stdin(), tokio::io::stdout(), agent.clone());
    *agent.peer.lock().await = Some(peer);
    let _ = reader.await;
}
