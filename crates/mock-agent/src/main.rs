//! A scriptable ACP agent for tests. Each prompt is answered with `echo(model=<m>): <text>`
//! unless it contains a directive:
//!
//! - `[tool]`  report a tool call, ask for permission, then echo the chosen option
//! - `[slow]`  keep the turn running until cancelled
//! - `[crash]` exit the process immediately

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use crossagents_drivers::jsonrpc::{RpcError, RpcHandler, RpcPeer};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify};

struct MockAgent {
    peer: Mutex<Option<RpcPeer>>,
    model: Mutex<String>,
    mode: Mutex<String>,
    cancelled: Notify,
}

impl MockAgent {
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

        let model = self.model.lock().await.clone();
        let mode = self.mode.lock().await.clone();
        if blocks.len() > 1 {
            self.say(&session_id, "[instructions received] ").await;
        }
        self.say(&session_id, &format!("echo(model={model},mode={mode}): "))
            .await;
        self.say(&session_id, &text).await;
        self.update(
            &session_id,
            json!({ "sessionUpdate": "usage_update", "used": 42, "size": 1000 }),
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
            "session/new" => Ok(json!({
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
            })),
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
    let agent = Arc::new(MockAgent {
        peer: Mutex::new(None),
        model: Mutex::new("mock-opus".into()),
        mode: Mutex::new("default".into()),
        cancelled: Notify::new(),
    });
    let (peer, reader) = RpcPeer::spawn(tokio::io::stdin(), tokio::io::stdout(), agent.clone());
    *agent.peer.lock().await = Some(peer);
    let _ = reader.await;
}
