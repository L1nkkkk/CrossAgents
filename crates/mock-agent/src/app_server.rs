//! `crossagents-mock-agent app-server`: the core of the `codex app-server` protocol, for
//! testing the `codex-native` driver without Codex. Threads persist as JSON files under
//! `$CODEX_HOME/mock-threads/`, so a new process can resume them.
//!
//! Each turn answers `echo(model=<m>,effort=<e>): <text>` after acting on these markers:
//!
//! - `[slow]`      keep the turn running until interrupted
//! - `[sleep MS]`  keep the turn running for MS milliseconds
//! - `[tool]`      ask for command approval, then say `permission=<decision>`
//! - `[write P=C]` write file P (relative to the thread's cwd), report a file change and diff
//! - `[history]`   say `history=<earlier inputs joined by |>`
//! - `[mcp]`       say the MCP servers the thread was given and their approval mode
//! - `[instructions]` say the thread's developer instructions
//! - `[env NAME]`  say an environment variable
//! - `[pid]`       say this process's id
//! - `[fail]`      fail the turn with an error
//! - `[crash]`     exit the process
//! - `[limit N]`   report the account's quota as N% used (at 100, the limit as reached)

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use crossagents_drivers::jsonrpc::{RpcError, RpcHandler, RpcPeer};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify};

struct Active {
    turn_id: String,
    interrupt: Arc<Notify>,
}

struct MockAppServer {
    peer: Mutex<Option<RpcPeer>>,
    home: PathBuf,
    threads: Mutex<HashMap<String, Value>>,
    active: Mutex<HashMap<String, Active>>,
    counter: AtomicU64,
}

fn rpc_error(message: impl Into<String>) -> RpcError {
    RpcError::new(-32600, message)
}

fn sandbox_policy(mode: &str) -> Value {
    match mode {
        "danger-full-access" => json!({ "type": "dangerFullAccess" }),
        "read-only" => json!({ "type": "readOnly", "networkAccess": false }),
        _ => json!({ "type": "workspaceWrite", "writableRoots": [], "networkAccess": false }),
    }
}

impl MockAppServer {
    fn new_id(&self, kind: &str) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        format!("{kind}-{}-{n}", std::process::id())
    }

    fn path(&self, thread_id: &str) -> PathBuf {
        self.home
            .join("mock-threads")
            .join(format!("{thread_id}.json"))
    }

    async fn save(&self, thread: &Value) {
        let id = thread["id"].as_str().unwrap_or_default();
        let path = self.path(id);
        let _ = std::fs::create_dir_all(path.parent().expect("has parent"));
        let _ = std::fs::write(path, thread.to_string());
        self.threads.lock().await.insert(id.into(), thread.clone());
    }

    async fn load(&self, thread_id: &str) -> Result<Value, RpcError> {
        if let Some(thread) = self.threads.lock().await.get(thread_id) {
            return Ok(thread.clone());
        }
        let text = std::fs::read_to_string(self.path(thread_id))
            .map_err(|_| rpc_error(format!("no rollout found for thread id {thread_id}")))?;
        let thread: Value =
            serde_json::from_str(&text).map_err(|e| rpc_error(format!("bad rollout: {e}")))?;
        self.threads
            .lock()
            .await
            .insert(thread_id.into(), thread.clone());
        Ok(thread)
    }

    async fn peer(&self) -> RpcPeer {
        loop {
            if let Some(peer) = self.peer.lock().await.clone() {
                return peer;
            }
            tokio::task::yield_now().await;
        }
    }

    async fn notify(&self, method: &str, params: Value) {
        let _ = self.peer().await.notify(method, params);
    }

    /// Applies `thread/start`-style settings from `params` to `thread`. Like Codex, settings
    /// not given fall back to defaults, never to another thread's.
    fn apply_settings(thread: &mut Value, params: &Value) {
        for (key, param) in [
            ("model", "model"),
            ("cwd", "cwd"),
            ("sandbox", "sandbox"),
            ("approvalPolicy", "approvalPolicy"),
        ] {
            if let Some(value) = params.get(param).filter(|v| !v.is_null()) {
                thread[key] = value.clone();
            }
        }
        if let Some(text) = params.get("developerInstructions").filter(|v| !v.is_null()) {
            thread["instructions"] = text.clone();
        }
        if let Some(servers) = params.pointer("/config/mcp_servers") {
            thread["mcp"] = servers.clone();
        }
    }

    fn new_thread(&self, id: String) -> Value {
        json!({
            "id": id, "cwd": ".", "model": "mock-default", "effort": "medium",
            "sandbox": "read-only", "approvalPolicy": "on-request",
            "instructions": null, "mcp": {}, "turns": [], "forkedFromId": null,
        })
    }

    fn thread_response(thread: &Value) -> Value {
        json!({
            "thread": {
                "id": thread["id"], "sessionId": thread["id"], "forkedFromId": thread["forkedFromId"],
                "cwd": thread["cwd"], "model": thread["model"], "status": { "type": "idle" },
                "historyMode": "paginated", "turns": [],
            },
            "model": thread["model"], "modelProvider": "mock", "cwd": thread["cwd"],
            "approvalPolicy": thread["approvalPolicy"],
            "sandbox": sandbox_policy(thread["sandbox"].as_str().unwrap_or_default()),
            "reasoningEffort": thread["effort"],
        })
    }

    async fn thread_start(&self, params: Value) -> Result<Value, RpcError> {
        let mut thread = self.new_thread(self.new_id("thread"));
        Self::apply_settings(&mut thread, &params);
        self.save(&thread).await;
        Ok(Self::thread_response(&thread))
    }

    async fn thread_resume(&self, params: Value) -> Result<Value, RpcError> {
        let id = params["threadId"].as_str().unwrap_or_default();
        let mut thread = self.load(id).await?;
        Self::apply_settings(&mut thread, &params);
        self.save(&thread).await;
        Ok(Self::thread_response(&thread))
    }

    async fn thread_fork(&self, params: Value) -> Result<Value, RpcError> {
        let source = self
            .load(params["threadId"].as_str().unwrap_or_default())
            .await?;
        let mut turns = Vec::new();
        for turn in source["turns"].as_array().into_iter().flatten() {
            turns.push(turn.clone());
            if params.get("lastTurnId") == turn.get("id") {
                break;
            }
        }
        let mut thread = self.new_thread(self.new_id("thread"));
        thread["turns"] = Value::Array(turns);
        thread["forkedFromId"] = source["id"].clone();
        thread["instructions"] = source["instructions"].clone();
        Self::apply_settings(&mut thread, &params);
        self.save(&thread).await;
        Ok(Self::thread_response(&thread))
    }

    async fn thread_revert(&self, params: Value) -> Result<Value, RpcError> {
        let mut thread = self
            .load(params["threadId"].as_str().unwrap_or_default())
            .await?;
        let before = params["beforeTurnId"].clone();
        let turns = thread["turns"].as_array().cloned().unwrap_or_default();
        let Some(cut) = turns.iter().position(|t| t["id"] == before) else {
            return Err(rpc_error(format!("turn {before} not found")));
        };
        thread["turns"] = Value::Array(turns[..cut].to_vec());
        self.save(&thread).await;
        Ok(
            json!({ "thread": Self::thread_response(&thread)["thread"], "turnsBackwardsCursor": null, "itemsBackwardsCursor": null }),
        )
    }

    async fn turn_start(self: &Arc<Self>, params: Value) -> Result<Value, RpcError> {
        let thread_id = params["threadId"].as_str().unwrap_or_default().to_owned();
        if !self.threads.lock().await.contains_key(&thread_id) {
            return Err(rpc_error(format!("thread {thread_id} is not loaded")));
        }
        let text = params
            .pointer("/input/0/text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let mut active = self.active.lock().await;
        if let Some(running) = active.get(&thread_id) {
            // Like Codex: input for a busy thread steers the running turn.
            return Ok(
                json!({ "turn": { "id": running.turn_id, "status": "inProgress", "items": [] } }),
            );
        }
        {
            let mut threads = self.threads.lock().await;
            let thread = threads.get_mut(&thread_id).expect("checked above");
            for key in ["model", "effort"] {
                if let Some(value) = params.get(key).filter(|v| !v.is_null()) {
                    thread[key] = value.clone();
                }
            }
        }
        let turn_id = self.new_id("turn");
        let interrupt = Arc::new(Notify::new());
        active.insert(
            thread_id.clone(),
            Active {
                turn_id: turn_id.clone(),
                interrupt: interrupt.clone(),
            },
        );
        drop(active);
        tokio::spawn({
            let this = self.clone();
            let turn_id = turn_id.clone();
            async move { this.run_turn(thread_id, turn_id, text, interrupt).await }
        });
        Ok(json!({ "turn": { "id": turn_id, "status": "inProgress", "items": [] } }))
    }

    async fn say(&self, thread_id: &str, turn_id: &str, text: &str) {
        self.notify(
            "item/agentMessage/delta",
            json!({ "threadId": thread_id, "turnId": turn_id, "itemId": format!("msg-{turn_id}"), "delta": text }),
        )
        .await;
    }

    async fn run_turn(
        &self,
        thread_id: String,
        turn_id: String,
        text: String,
        interrupt: Arc<Notify>,
    ) {
        let tid = thread_id.as_str();
        let uid = turn_id.as_str();
        self.notify(
            "thread/status/changed",
            json!({ "threadId": tid, "status": { "type": "active", "activeFlags": [] } }),
        )
        .await;
        self.notify(
            "turn/started",
            json!({ "threadId": tid, "turn": { "id": uid, "status": "inProgress", "items": [] } }),
        )
        .await;
        if text.contains("[crash]") {
            std::process::exit(3);
        }
        let thread = self
            .threads
            .lock()
            .await
            .get(tid)
            .cloned()
            .unwrap_or_default();
        let mut status = "completed";

        if text.contains("[slow]") {
            self.say(tid, uid, "working ").await;
            tokio::select! {
                _ = interrupt.notified() => status = "interrupted",
                _ = tokio::time::sleep(Duration::from_secs(600)) => {}
            }
        }
        if let Some(ms) = text
            .split("[sleep ")
            .nth(1)
            .and_then(|rest| rest.split(']').next())
            .and_then(|ms| ms.trim().parse::<u64>().ok())
        {
            self.say(tid, uid, "sleeping ").await;
            tokio::select! {
                _ = interrupt.notified() => status = "interrupted",
                _ = tokio::time::sleep(Duration::from_millis(ms)) => {}
            }
        }
        if status == "completed" && text.contains("[tool]") {
            let item = json!({ "type": "commandExecution", "id": format!("exec-{uid}"), "command": "mock-tool --go",
                "cwd": thread["cwd"], "status": "inProgress" });
            self.notify(
                "item/started",
                json!({ "threadId": tid, "turnId": uid, "item": item }),
            )
            .await;
            let answer: Result<Value, RpcError> = self
                .peer()
                .await
                .request(
                    "item/commandExecution/requestApproval",
                    json!({ "kind": "command", "threadId": tid, "turnId": uid, "itemId": format!("exec-{uid}"),
                        "command": "mock-tool --go", "cwd": thread["cwd"], "startedAtMs": 0,
                        "availableDecisions": ["accept", "acceptForSession", "decline", "cancel"] }),
                )
                .await;
            let decision = answer
                .ok()
                .and_then(|a| a["decision"].as_str().map(str::to_owned))
                .unwrap_or_else(|| "error".into());
            let mut item = item;
            item["status"] = if decision.starts_with("accept") {
                "completed"
            } else {
                "declined"
            }
            .into();
            self.notify(
                "item/completed",
                json!({ "threadId": tid, "turnId": uid, "item": item }),
            )
            .await;
            self.say(tid, uid, &format!("permission={decision} ")).await;
        }
        if status == "completed" {
            self.directives(tid, uid, &text, &thread).await;
        }
        if status == "completed" && text.contains("[fail]") {
            let error = json!({ "message": "mock failure", "codexErrorInfo": "other", "additionalDetails": null });
            self.notify(
                "error",
                json!({ "threadId": tid, "turnId": uid, "willRetry": false, "error": error }),
            )
            .await;
            status = "failed";
        }
        if status == "completed" {
            let reply = format!(
                "echo(model={},effort={}): {text}",
                thread["model"].as_str().unwrap_or_default(),
                thread["effort"].as_str().unwrap_or_default()
            );
            let (a, b) = reply.split_at(reply.len() / 2);
            self.say(tid, uid, a).await;
            self.say(tid, uid, b).await;
            self.notify(
                "item/completed",
                json!({ "threadId": tid, "turnId": uid,
                "item": { "type": "agentMessage", "id": format!("msg-{uid}"), "text": reply } }),
            )
            .await;
        }

        // Usage and quota, as Codex reports them after each model call.
        let turns = thread["turns"].as_array().map_or(0, Vec::len) as u64 + 1;
        let total = json!({ "totalTokens": 1100 * turns, "inputTokens": 1000 * turns, "cachedInputTokens": 400 * turns,
            "cacheWriteInputTokens": 0, "outputTokens": 100 * turns, "reasoningOutputTokens": 10 * turns });
        self.notify("thread/tokenUsage/updated", json!({ "threadId": tid, "turnId": uid, "tokenUsage": {
            "total": total, "last": { "totalTokens": 1100, "inputTokens": 1000, "cachedInputTokens": 400,
            "cacheWriteInputTokens": 0, "outputTokens": 100, "reasoningOutputTokens": 10 }, "modelContextWindow": 200000 } })).await;
        let used = text
            .split("[limit ")
            .nth(1)
            .and_then(|rest| rest.split(']').next())
            .and_then(|n| n.trim().parse::<u64>().ok())
            .unwrap_or(7);
        let reached = (used >= 100).then_some("rate_limit_reached");
        self.notify("account/rateLimits/updated", json!({ "rateLimits": { "limitId": "codex", "primary": {
            "usedPercent": used, "windowDurationMins": 300, "resetsAt": 1795131666 }, "secondary": null,
            "credits": null, "planType": "pro", "rateLimitReachedType": reached } })).await;

        {
            let mut threads = self.threads.lock().await;
            if let Some(thread) = threads.get_mut(tid) {
                if let Some(turns) = thread["turns"].as_array_mut() {
                    turns.push(json!({ "id": uid, "input": text, "status": status }));
                }
                let thread = thread.clone();
                drop(threads);
                self.save(&thread).await;
            }
        }
        self.active.lock().await.remove(tid);
        self.notify(
            "thread/status/changed",
            json!({ "threadId": tid, "status": { "type": "idle" } }),
        )
        .await;
        self.notify(
            "turn/completed",
            json!({ "threadId": tid, "turn": { "id": uid, "status": status, "items": [] } }),
        )
        .await;
    }

    async fn directives(&self, tid: &str, uid: &str, text: &str, thread: &Value) {
        if let Some(rest) = text.split("[write ").nth(1)
            && let Some(body) = rest.split(']').next()
            && let Some((path, content)) = body.split_once('=')
        {
            let full = PathBuf::from(thread["cwd"].as_str().unwrap_or(".")).join(path);
            let _ = std::fs::write(&full, content);
            let item = json!({ "type": "fileChange", "id": format!("patch-{uid}"),
                "changes": [{ "path": full.display().to_string(), "kind": { "type": "add" }, "diff": format!("{content}\n") }],
                "status": "inProgress" });
            self.notify(
                "item/started",
                json!({ "threadId": tid, "turnId": uid, "item": item }),
            )
            .await;
            let mut done = item;
            done["status"] = "completed".into();
            self.notify(
                "item/completed",
                json!({ "threadId": tid, "turnId": uid, "item": done }),
            )
            .await;
            let diff = format!(
                "diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1 @@\n+{content}\n"
            );
            self.notify(
                "turn/diff/updated",
                json!({ "threadId": tid, "turnId": uid, "diff": diff }),
            )
            .await;
            self.say(tid, uid, &format!("wrote {path} ")).await;
        }
        if text.contains("[history]") {
            let inputs: Vec<&str> = thread["turns"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| t["input"].as_str())
                .collect();
            self.say(tid, uid, &format!("history={} ", inputs.join("|")))
                .await;
        }
        if text.contains("[mcp]") {
            let servers: Vec<String> = thread["mcp"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(name, s)| {
                    format!(
                        "{name}:{}:{}",
                        s["default_tools_approval_mode"].as_str().unwrap_or("-"),
                        s["env"].as_object().map(|e| e.len()).unwrap_or(0)
                    )
                })
                .collect();
            self.say(tid, uid, &format!("mcp={} ", servers.join(",")))
                .await;
        }
        if text.contains("[instructions]") {
            let instructions = thread["instructions"].as_str().unwrap_or("-");
            self.say(tid, uid, &format!("instructions={instructions} "))
                .await;
        }
        if let Some(rest) = text.split("[env ").nth(1)
            && let Some(name) = rest.split(']').next()
        {
            let value = std::env::var(name).unwrap_or_default();
            self.say(tid, uid, &format!("env {name}={value} ")).await;
        }
        if text.contains("[pid]") {
            self.say(tid, uid, &format!("pid={} ", std::process::id()))
                .await;
        }
    }
}

struct Handler(Arc<MockAppServer>);

#[async_trait]
impl RpcHandler for Handler {
    async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let server = &self.0;
        match method {
            "initialize" => Ok(json!({
                "userAgent": "crossagents-mock-app-server/0", "codexHome": server.home.display().to_string(),
                "platformFamily": std::env::consts::FAMILY, "platformOs": std::env::consts::OS,
            })),
            "thread/start" => server.thread_start(params).await,
            "thread/resume" => server.thread_resume(params).await,
            "thread/fork" => server.thread_fork(params).await,
            "thread/revert" => server.thread_revert(params).await,
            "thread/unsubscribe" => Ok(json!({ "status": "unsubscribed" })),
            "thread/loaded/list" => {
                let ids: Vec<String> = server.threads.lock().await.keys().cloned().collect();
                Ok(json!({ "data": ids }))
            }
            "thread/compact/start" => Ok(json!({})),
            "turn/start" => server.turn_start(params).await,
            "turn/interrupt" => {
                let thread_id = params["threadId"].as_str().unwrap_or_default();
                if let Some(active) = server.active.lock().await.get(thread_id)
                    && params["turnId"] == active.turn_id.as_str()
                {
                    active.interrupt.notify_one();
                }
                Ok(json!({}))
            }
            "model/list" => Ok(json!({ "data": [
                { "id": "mock-codex", "model": "mock-codex", "isDefault": true, "supportedReasoningEfforts": [] },
                { "id": "mock-codex-mini", "model": "mock-codex-mini", "isDefault": false, "supportedReasoningEfforts": [] }
            ], "nextCursor": null })),
            _ => Err(RpcError::method_not_found(method)),
        }
    }

    async fn notification(&self, _method: &str, _params: Value) {}
}

pub async fn run() {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("crossagents-mock-codex"));
    let server = Arc::new(MockAppServer {
        peer: Mutex::new(None),
        home,
        threads: Mutex::default(),
        active: Mutex::default(),
        counter: AtomicU64::new(0),
    });
    let (peer, reader) = RpcPeer::spawn(
        tokio::io::stdin(),
        tokio::io::stdout(),
        Arc::new(Handler(server.clone())),
    );
    *server.peer.lock().await = Some(peer);
    let _ = reader.await;
}
