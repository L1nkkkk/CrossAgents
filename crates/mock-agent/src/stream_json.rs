//! `crossagents-mock-agent stream-json <claude flags>`: the core of Claude Code's stream-json
//! control protocol (`claude -p --input-format stream-json --output-format stream-json`),
//! which CodeBuddy speaks too, for testing `claude-native` and `codebuddy-native` without
//! either CLI. The flags the drivers add are accepted; `--model` and the session id
//! (`--session-id` / `--resume`) are used, the rest ignored.
//!
//! Each turn answers `echo(model=<m>): <text>` after acting on these markers:
//!
//! - `[slow]`      keep the turn running until interrupted
//! - `[sleep MS]`  keep the turn running for MS milliseconds
//! - `[pid]`       say this process's id
//! - `[crash]`     exit the process

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Stdout};
use tokio::sync::{Mutex, Notify};

struct Mock {
    out: Mutex<Stdout>,
    session_id: String,
    model: Mutex<String>,
    /// Woken by an `interrupt` control request.
    interrupt: Notify,
    /// One turn at a time, as Claude Code runs them.
    turn: Mutex<()>,
    counter: AtomicU64,
    turns: AtomicU64,
}

impl Mock {
    fn new_id(&self, kind: &str) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        format!("{kind}-{}-{n}", std::process::id())
    }

    async fn send(&self, message: Value) {
        let mut line = message.to_string();
        line.push('\n');
        let mut out = self.out.lock().await;
        let _ = out.write_all(line.as_bytes()).await;
        let _ = out.flush().await;
    }

    async fn answer(&self, request_id: &str, response: Value) {
        self.send(json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": request_id, "response": response },
        }))
        .await;
    }

    async fn say(&self, text: &str) {
        self.send(json!({
            "type": "assistant",
            "uuid": self.new_id("assistant"),
            "session_id": self.session_id,
            "parent_tool_use_id": null,
            "message": {
                "id": self.new_id("msg"), "role": "assistant",
                "content": [{ "type": "text", "text": text }],
            },
        }))
        .await;
    }

    async fn control(&self, message: &Value) {
        let id = message["request_id"].as_str().unwrap_or_default();
        let request = &message["request"];
        match request["subtype"].as_str().unwrap_or_default() {
            "initialize" => {
                let model = self.model.lock().await.clone();
                self.answer(id, json!({ "commands": [], "models": [{ "value": model }] }))
                    .await;
            }
            "interrupt" => {
                self.interrupt.notify_waiters();
                self.answer(id, json!({})).await;
            }
            "set_model" => {
                if let Some(model) = request["model"].as_str() {
                    *self.model.lock().await = model.to_owned();
                }
                self.answer(id, json!({})).await;
            }
            // `get_usage` and the rest: nothing to report.
            _ => self.answer(id, json!({})).await,
        }
    }

    async fn run_turn(&self, message: Value) {
        let _turn = self.turn.lock().await;
        let uuid = message["uuid"].as_str().unwrap_or_default().to_owned();
        let text = match &message["message"]["content"] {
            Value::String(text) => text.clone(),
            Value::Array(blocks) => blocks
                .iter()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if text.contains("[crash]") {
            std::process::exit(3);
        }
        let mut interrupted = false;
        if text.contains("[slow]") {
            self.say("working ").await;
            tokio::select! {
                () = self.interrupt.notified() => interrupted = true,
                () = tokio::time::sleep(Duration::from_secs(600)) => {}
            }
        }
        if !interrupted
            && let Some(ms) = text
                .split("[sleep ")
                .nth(1)
                .and_then(|rest| rest.split(']').next())
                .and_then(|ms| ms.trim().parse::<u64>().ok())
        {
            self.say("sleeping ").await;
            tokio::select! {
                () = self.interrupt.notified() => interrupted = true,
                () = tokio::time::sleep(Duration::from_millis(ms)) => {}
            }
        }
        if !interrupted {
            if text.contains("[pid]") {
                self.say(&format!("pid={} ", std::process::id())).await;
            }
            let model = self.model.lock().await.clone();
            self.say(&format!("echo(model={model}): {text}")).await;
        }
        // `total_cost_usd` counts from the start of the process.
        let turns = self.turns.fetch_add(1, Ordering::Relaxed) + 1;
        self.send(json!({
            "type": "result",
            "subtype": if interrupted { "error_during_execution" } else { "success" },
            "is_error": interrupted,
            "stop_reason": if interrupted { Value::Null } else { json!("end_turn") },
            "session_id": self.session_id,
            "user_message_uuid": uuid,
            "usage": { "input_tokens": 100, "output_tokens": 10 },
            "total_cost_usd": 0.01 * turns as f64,
        }))
        .await;
    }
}

/// The value after `flag` in `args`.
fn flag(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

pub async fn run(args: Vec<String>) {
    let session_id = flag(&args, "--session-id")
        .or_else(|| flag(&args, "--resume"))
        .unwrap_or_else(|| "mock-session".into());
    let mock = Arc::new(Mock {
        out: Mutex::new(tokio::io::stdout()),
        session_id,
        model: Mutex::new(flag(&args, "--model").unwrap_or_else(|| "mock-default".into())),
        interrupt: Notify::new(),
        turn: Mutex::new(()),
        counter: AtomicU64::new(0),
        turns: AtomicU64::new(0),
    });
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match message["type"].as_str() {
            Some("control_request") => {
                let mock = mock.clone();
                tokio::spawn(async move { mock.control(&message).await });
            }
            Some("user") => {
                let mock = mock.clone();
                tokio::spawn(async move { mock.run_turn(message).await });
            }
            _ => {}
        }
    }
    // Stdin closed: Claude Code finishes the running turn's transcript and exits.
    let _ = mock.turn.lock().await;
}
