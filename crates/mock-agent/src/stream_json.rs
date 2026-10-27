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
//! - `[cwd]`       say the process's working directory
//! - `[session]`   say the session id, and whether it was resumed
//! - `[crash]`     exit the process
//! - `[write ...]` / `[call ...]` directives, see [`crate::script`]; calls go to the first
//!   server of `--mcp-config`
//!
//! - `[remember X]` keep X in the session (with `--mock-sessions`)
//! - `[recall]`    say what the session keeps
//!
//! `--resume` fails (the process exits before answering `initialize`) in a directory that
//! has a file named `mock-no-resume`, for testing what happens when a session cannot be
//! carried into another directory. Any start fails in a directory with `mock-no-start`.
//!
//! `--mock-sessions DIR` keeps each session in `DIR/<id>.json` with the directory it was
//! started in, as CLIs that keep sessions per project do: `--resume` fails anywhere else,
//! and what `[remember]` kept comes back with the session.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Stdout};
use tokio::sync::{Mutex, Notify};

use crate::scripted::{McpLaunch, Scripted};

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
    resumed: bool,
    script: Scripted,
    /// `--mock-sessions`: where the session is kept.
    kept: Option<std::path::PathBuf>,
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
                self.answer(
                    id,
                    json!({ "commands": [], "models": [{ "value": model }] }),
                )
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
            if text.contains("[cwd]") {
                let cwd = std::env::current_dir().unwrap_or_default();
                self.say(&format!("cwd={} ", cwd.display())).await;
            }
            if text.contains("[session]") {
                let how = if self.resumed { "resumed" } else { "new" };
                self.say(&format!("session={} ({how}) ", self.session_id))
                    .await;
            }
            if let Some(kept) = &self.kept {
                let mut session = read_kept(kept).unwrap_or_else(|| json!({}));
                for word in text.split("[remember ").skip(1) {
                    if let Some(word) = word.split(']').next()
                        && let Some(memory) = session["memory"].as_array_mut()
                    {
                        memory.push(json!(word));
                    }
                }
                let _ = std::fs::write(kept, session.to_string());
                if text.contains("[recall]") {
                    let memory: Vec<&str> = session["memory"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    self.say(&format!("recall={} ", memory.join(","))).await;
                }
            }
            let model = self.model.lock().await.clone();
            for line in self.script.run(&text, &model).await {
                self.say(&format!(
                    "{line}
"
                ))
                .await;
            }
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

fn read_kept(path: &std::path::Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// The value after `flag` in `args`.
fn flag(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

pub async fn run(args: Vec<String>) {
    let resumed = flag(&args, "--resume").is_some() && flag(&args, "--fork-session").is_none();
    let cwd = std::env::current_dir().unwrap_or_default();
    if resumed && cwd.join("mock-no-resume").exists() {
        eprintln!("No conversation found to resume in {}", cwd.display());
        std::process::exit(1);
    }
    if cwd.join("mock-no-start").exists() {
        eprintln!("cannot start in {}", cwd.display());
        std::process::exit(1);
    }
    let session_id = flag(&args, "--session-id")
        .or_else(|| flag(&args, "--resume"))
        .unwrap_or_else(|| "mock-session".into());
    let here = std::fs::canonicalize(&cwd)
        .unwrap_or_else(|_| cwd.clone())
        .display()
        .to_string();
    let kept = flag(&args, "--mock-sessions")
        .map(|dir| std::path::PathBuf::from(dir).join(format!("{session_id}.json")));
    if let Some(kept) = &kept {
        if resumed {
            let found = read_kept(kept);
            if found.as_ref().and_then(|s| s["cwd"].as_str()) != Some(here.as_str()) {
                eprintln!("No conversation found with session ID: {session_id}");
                std::process::exit(1);
            }
        } else {
            let _ = std::fs::write(kept, json!({ "cwd": here, "memory": [] }).to_string());
        }
    }
    let script = Scripted::default();
    *script.cwd.lock().await = cwd;
    *script.server.lock().await = flag(&args, "--mcp-config")
        .and_then(|config| serde_json::from_str::<Value>(&config).ok())
        .and_then(|config| {
            let servers = config["mcpServers"].as_object()?;
            let server = servers.values().next()?;
            Some(McpLaunch {
                command: server["command"].as_str()?.to_owned(),
                args: server["args"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|a| a.as_str().unwrap_or_default().to_owned())
                    .collect(),
                env: server["env"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned()))
                    .collect(),
            })
        });
    let mock = Arc::new(Mock {
        out: Mutex::new(tokio::io::stdout()),
        session_id,
        model: Mutex::new(flag(&args, "--model").unwrap_or_else(|| "mock-default".into())),
        interrupt: Notify::new(),
        turn: Mutex::new(()),
        counter: AtomicU64::new(0),
        turns: AtomicU64::new(0),
        resumed,
        script,
        kept,
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
