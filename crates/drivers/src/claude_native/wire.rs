//! Claude Code's stream-json protocol: launch arguments, the messages we send, and the
//! translation of what it prints into hub events. Pure functions, so the protocol is tested
//! without a `claude` process. Shapes: docs/research/claude-stream-json.md.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use crossagents_core::{
    Credits, FileChange, LimitGroup, McpServerSpec, OutputStream, PermissionDecision,
    PermissionRequest, RateLimits, RateWindow, SessionEventKind, StopReason, TokenUsage,
    ToolDetail, ToolStatus,
};
use serde_json::{Map, Value, json};

/// Tool results longer than this are cut (the end is kept for output, the start for results).
const RESULT_LIMIT: usize = 16 * 1024;

/// How to launch `claude` for a session; the same for every process of the session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Launch {
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Run without asking (`--dangerously-skip-permissions`); otherwise every ask goes to
    /// the hub through `can_use_tool`.
    pub bypass: bool,
    /// `--permission-mode` (e.g. `acceptEdits`), from the agent's `config.permission_mode`.
    pub permission_mode: Option<String>,
    pub mcp_servers: Vec<McpServerSpec>,
    /// Appended to Claude Code's system prompt.
    pub instructions: Option<String>,
    /// Which settings files Claude Code reads. Not the user's own by default: their
    /// plugins, hooks and connectors are not the agent's.
    pub setting_sources: String,
}

/// Which conversation the process continues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// A new session with the id we chose.
    New(String),
    Resume(String),
    /// A copy of `from` under `id`, up to and including message `at` (all of it when `None`).
    Fork {
        from: String,
        id: String,
        at: Option<String>,
    },
}

/// Which CLI is driven: Claude Code, or a fork that keeps the protocol but not every flag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Flavor {
    #[default]
    Claude,
    /// WorkBuddy's CodeBuddy CLI: it routes permission asks over the stream-json channel by
    /// itself and rejects `--permission-prompt-tool`.
    Codebuddy,
}

/// `CODEBUDDY_*` variables a session keeps when started from inside another CodeBuddy or
/// WorkBuddy session: the ones that say how to sign in.
const CODEBUDDY_KEPT_ENV: [&str; 3] = [
    "CODEBUDDY_API_KEY",
    "CODEBUDDY_AUTH_TOKEN",
    "CODEBUDDY_INTERNET_ENVIRONMENT",
];

impl Flavor {
    /// The CLI's name, for messages.
    pub fn program(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codebuddy => "codebuddy",
        }
    }

    /// Whether an inherited variable is withheld from the CLI. A hub started from a
    /// WorkBuddy terminal inherits that session's `CODEBUDDY_*` (its MCP config with the
    /// connector proxy's token, its session id, host, service proxy and `CONFIG_DIR`, the
    /// folder WorkBuddy keeps its own login in), which would tie the worker to WorkBuddy;
    /// everything but the sign-in is dropped. An agent's own `env` is applied after this
    /// and can set any of them again.
    pub fn withholds(self, key: &str) -> bool {
        self == Self::Codebuddy
            && key.to_ascii_uppercase().starts_with("CODEBUDDY_")
            && !CODEBUDDY_KEPT_ENV
                .iter()
                .any(|kept| kept.eq_ignore_ascii_case(key))
    }
}

pub const DEFAULT_SETTING_SOURCES: &str = "project,local";

pub fn args(launch: &Launch, start: &Start, flavor: Flavor) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--strict-mcp-config",
        "--setting-sources",
    ]
    .map(String::from)
    .to_vec();
    args.push(launch.setting_sources.clone());
    if flavor == Flavor::Claude {
        args.extend(["--permission-prompt-tool".into(), "stdio".into()]);
    }
    if !launch.mcp_servers.is_empty() {
        args.push("--mcp-config".into());
        args.push(mcp_config(&launch.mcp_servers).to_string());
    }
    if launch.bypass {
        args.push("--dangerously-skip-permissions".into());
    }
    for (flag, value) in [
        ("--model", &launch.model),
        ("--effort", &launch.effort),
        ("--permission-mode", &launch.permission_mode),
    ] {
        if let Some(value) = value {
            args.push(flag.into());
            args.push(value.clone());
        }
    }
    if let Some(instructions) = &launch.instructions {
        args.push("--append-system-prompt".into());
        args.push(instructions.clone());
    }
    match start {
        Start::New(id) => args.extend(["--session-id".into(), id.clone()]),
        Start::Resume(id) => args.extend(["--resume".into(), id.clone()]),
        Start::Fork { from, id, at } => {
            args.extend([
                "--resume".into(),
                from.clone(),
                "--fork-session".into(),
                "--session-id".into(),
                id.clone(),
            ]);
            if let Some(at) = at {
                args.extend(["--resume-session-at".into(), at.clone()]);
            }
        }
    }
    args
}

fn mcp_config(servers: &[McpServerSpec]) -> Value {
    let servers: Map<String, Value> = servers
        .iter()
        .map(|s| {
            (
                s.name.clone(),
                json!({
                    "type": "stdio",
                    "command": s.command.display().to_string(),
                    "args": s.args,
                    "env": s.env,
                }),
            )
        })
        .collect();
    json!({ "mcpServers": servers })
}

/// A user message; `uuid` names it for rewinding and forking.
pub fn user_message(session_id: &str, uuid: &str, text: &str) -> Value {
    json!({
        "type": "user",
        "uuid": uuid,
        "session_id": session_id,
        "parent_tool_use_id": null,
        "message": { "role": "user", "content": text },
    })
}

/// The control request that changes a running session's reasoning effort: Claude Code's
/// `apply_flag_settings`; CodeBuddy has none and takes `set_config` (for this session only,
/// its settings file is left alone).
pub fn set_effort(flavor: Flavor, effort: &str) -> Value {
    match flavor {
        Flavor::Claude => {
            json!({ "subtype": "apply_flag_settings", "settings": { "effortLevel": effort } })
        }
        Flavor::Codebuddy => {
            json!({ "subtype": "set_config", "config": { "reasoningEffort": effort } })
        }
    }
}

pub fn control_request(id: &str, request: Value) -> Value {
    json!({ "type": "control_request", "request_id": id, "request": request })
}

pub fn control_success(id: &str, response: Value) -> Value {
    json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": id, "response": response },
    })
}

pub fn control_error(id: &str, error: &str) -> Value {
    json!({
        "type": "control_response",
        "response": { "subtype": "error", "request_id": id, "error": error },
    })
}

/// A `control_response`: its request id and the answer, or the error text.
pub fn control_answer(message: &Value) -> Option<(String, Result<Value, String>)> {
    let response = message.get("response")?;
    let id = response.get("request_id")?.as_str()?.to_owned();
    let answer = match response.get("subtype").and_then(Value::as_str) {
        Some("success") => Ok(response.get("response").cloned().unwrap_or(Value::Null)),
        _ => Err(response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("control request failed")
            .to_owned()),
    };
    Some((id, answer))
}

/// `(title, kind)` of a tool call, in the hub's kinds (`execute`, `edit`, `read`, ...).
pub fn tool_call(name: &str, input: &Value) -> (String, &'static str) {
    let text = |key: &str| input.get(key).and_then(Value::as_str).unwrap_or_default();
    let (title, kind) = match name {
        "Bash" | "PowerShell" => (
            text("command")
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned(),
            "execute",
        ),
        "Write" => (format!("Write {}", text("file_path")), "edit"),
        "Edit" | "MultiEdit" => (format!("Edit {}", text("file_path")), "edit"),
        "NotebookEdit" => (format!("Edit {}", text("notebook_path")), "edit"),
        "Read" => (format!("Read {}", text("file_path")), "read"),
        "Glob" => (format!("Find {}", text("pattern")), "search"),
        "Grep" => (format!("Search {}", text("pattern")), "search"),
        "WebSearch" => (format!("Web search {}", text("query")), "search"),
        "WebFetch" => (format!("Fetch {}", text("url")), "read"),
        "Task" | "Agent" => (text("description").to_owned(), "agent"),
        "TodoWrite" => ("Update the plan".to_owned(), "other"),
        _ => match name.strip_prefix("mcp__").and_then(|n| n.split_once("__")) {
            Some((server, tool)) => (format!("{server}.{tool}"), "mcp"),
            None => (name.to_owned(), "tool"),
        },
    };
    let title = title.trim();
    (if title.is_empty() { name } else { title }.to_owned(), kind)
}

/// What a finished tool call did. `result` is the message's structured `tool_use_result`,
/// `text` the result as the model saw it.
pub fn tool_detail(
    name: &str,
    input: &Value,
    result: Option<&Value>,
    text: &str,
    is_error: bool,
) -> ToolDetail {
    let (_, kind) = tool_call(name, input);
    let error = || is_error.then(|| text.to_owned());
    match kind {
        "execute" => {
            let mut detail = ToolDetail {
                command: input
                    .get("command")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                ..ToolDetail::default()
            };
            let streams: Vec<&str> = ["stdout", "stderr"]
                .into_iter()
                .filter_map(|k| result?.get(k)?.as_str())
                .filter(|s| !s.is_empty())
                .collect();
            detail.output = Some(tail(&if streams.is_empty() {
                text.to_owned()
            } else {
                streams.join("\n")
            }));
            detail.exit_code = if is_error {
                exit_code(text)
            } else {
                result.filter(|r| r.get("stdout").is_some()).map(|_| 0)
            };
            if is_error && detail.exit_code.is_none() {
                detail.error = Some(text.to_owned());
            }
            detail
        }
        "edit" => ToolDetail {
            changes: result
                .and_then(file_change)
                .or_else(|| (!is_error).then(|| input_change(name, input, text))?)
                .into_iter()
                .collect(),
            error: error(),
            ..ToolDetail::default()
        },
        // The file itself is not worth keeping.
        "read" if name == "Read" => ToolDetail {
            error: error(),
            ..ToolDetail::default()
        },
        _ => ToolDetail {
            arguments: Some(serde_json::to_string_pretty(input).unwrap_or_default()),
            result: (!is_error).then(|| head(text)),
            error: error(),
            ..ToolDetail::default()
        },
    }
}

/// "Exit code 2" in a failed shell call's result.
fn exit_code(text: &str) -> Option<i32> {
    let rest = &text[text.find("Exit code ")? + "Exit code ".len()..];
    let digits: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse().ok()
}

/// The file an Edit / Write changed, with a unified diff.
fn file_change(result: &Value) -> Option<FileChange> {
    let path = result.get("filePath")?.as_str()?.to_owned();
    let created = result.get("type").and_then(Value::as_str) == Some("create");
    let diff = if created {
        let content = result
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let lines: Vec<&str> = content.lines().collect();
        let mut diff = format!("@@ -0,0 +1,{} @@\n", lines.len());
        for line in lines {
            diff.push('+');
            diff.push_str(line);
            diff.push('\n');
        }
        diff
    } else {
        patch_diff(result.get("structuredPatch")?)
    };
    Some(FileChange {
        path,
        kind: if created { "add" } else { "update" }.to_owned(),
        diff,
    })
}

/// The change a Write / Edit / MultiEdit asked for, when no structured result says what it
/// did (CodeBuddy sends only the text). Hunks carry no line numbers, and a Write over an
/// existing file shows only the new content.
fn input_change(name: &str, input: &Value, text: &str) -> Option<FileChange> {
    let path = input.get("file_path")?.as_str()?.to_owned();
    let str_of = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
    let lines = |prefix: char, s: &str| -> String {
        s.lines().map(|line| format!("{prefix}{line}\n")).collect()
    };
    let (kind, diff) = match name {
        "Write" => {
            let content = str_of(input, "content")?;
            let created = text.to_ascii_lowercase().contains("created");
            let header = if created {
                format!("@@ -0,0 +1,{} @@\n", content.lines().count())
            } else {
                "@@ @@\n".to_owned()
            };
            (
                if created { "add" } else { "update" },
                header + &lines('+', &content),
            )
        }
        "Edit" | "MultiEdit" => {
            let edits = match input.get("edits").and_then(Value::as_array) {
                Some(edits) => edits.iter().collect(),
                None => vec![input],
            };
            let diff: String = edits
                .into_iter()
                .filter_map(|edit| {
                    let old = str_of(edit, "old_string")?;
                    let new = str_of(edit, "new_string")?;
                    Some(format!("@@ @@\n{}{}", lines('-', &old), lines('+', &new)))
                })
                .collect();
            if diff.is_empty() {
                return None;
            }
            ("update", diff)
        }
        _ => return None,
    };
    Some(FileChange {
        path,
        kind: kind.to_owned(),
        diff,
    })
}

/// Claude Code's `structuredPatch` hunks as unified diff text.
fn patch_diff(hunks: &Value) -> String {
    let mut diff = String::new();
    for hunk in hunks.as_array().into_iter().flatten() {
        let num = |key: &str| hunk.get(key).and_then(Value::as_u64).unwrap_or(0);
        diff.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            num("oldStart"),
            num("oldLines"),
            num("newStart"),
            num("newLines")
        ));
        for line in hunk
            .get("lines")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            diff.push_str(line);
            diff.push('\n');
        }
    }
    diff
}

fn tail(text: &str) -> String {
    if text.len() <= RESULT_LIMIT {
        return text.to_owned();
    }
    let mut start = text.len() - RESULT_LIMIT;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &text[start..])
}

fn head(text: &str) -> String {
    if text.len() <= RESULT_LIMIT {
        return text.to_owned();
    }
    let mut end = RESULT_LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// A `tool_result` block's content: a string or text blocks.
fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Turns one session's output into hub events. Keeps what later messages refer back to:
/// the tool calls, which messages were already streamed, the running totals.
#[derive(Default)]
pub struct Translator {
    tools: HashMap<String, (String, Value)>,
    /// Messages whose text arrived as stream deltas: their full copy is not shown again.
    streamed: HashSet<String>,
    streaming: Option<String>,
    tokens: TokenUsage,
    /// `total_cost_usd` counts from the start of the process.
    cost_so_far: f64,
}

impl Translator {
    /// Events for one printed message. `result` messages go through [`Self::turn_end`].
    pub fn translate(&mut self, message: &Value) -> Vec<SessionEventKind> {
        // A subagent's own steps stay inside its tool call.
        if message
            .get("parent_tool_use_id")
            .is_some_and(|p| !p.is_null())
        {
            return Vec::new();
        }
        match message.get("type").and_then(Value::as_str) {
            Some("stream_event") => self.stream_event(message.get("event").unwrap_or(&Value::Null)),
            Some("assistant") => self.assistant(message.get("message").unwrap_or(&Value::Null)),
            Some("user") => self.tool_results(message),
            Some("rate_limit_event") => message
                .get("rate_limit_info")
                .map(|info| {
                    vec![SessionEventKind::RateLimits {
                        limits: rate_limit_event(info),
                    }]
                })
                .unwrap_or_default(),
            Some("system") => match message.get("subtype").and_then(Value::as_str) {
                Some("api_retry") => vec![SessionEventKind::Warning {
                    message: format!(
                        "API request failed ({}); retrying, attempt {}",
                        message
                            .get("error")
                            .and_then(Value::as_str)
                            .or_else(|| message.get("error_status").and_then(Value::as_str))
                            .unwrap_or("error"),
                        message.get("attempt").and_then(Value::as_u64).unwrap_or(1)
                    ),
                }],
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    fn stream_event(&mut self, event: &Value) -> Vec<SessionEventKind> {
        match event.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                self.streaming = event
                    .pointer("/message/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                Vec::new()
            }
            Some("content_block_delta") => {
                let delta = event.get("delta").unwrap_or(&Value::Null);
                let (stream, text) = match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => (OutputStream::Text, delta.get("text")),
                    Some("thinking_delta") => (OutputStream::Thought, delta.get("thinking")),
                    _ => return Vec::new(),
                };
                let Some(text) = text.and_then(Value::as_str) else {
                    return Vec::new();
                };
                if let Some(id) = &self.streaming {
                    self.streamed.insert(id.clone());
                }
                vec![SessionEventKind::Output {
                    stream,
                    text: text.to_owned(),
                }]
            }
            _ => Vec::new(),
        }
    }

    fn assistant(&mut self, message: &Value) -> Vec<SessionEventKind> {
        let streamed = message
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| self.streamed.contains(id));
        let mut events = Vec::new();
        for block in message
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let text = |key: &str| block.get(key).and_then(Value::as_str).map(str::to_owned);
            match block.get("type").and_then(Value::as_str) {
                Some("text") if !streamed => {
                    events.extend(text("text").map(|text| SessionEventKind::Output {
                        stream: OutputStream::Text,
                        text,
                    }))
                }
                Some("thinking") if !streamed => {
                    events.extend(text("thinking").map(|text| SessionEventKind::Output {
                        stream: OutputStream::Thought,
                        text,
                    }))
                }
                Some("tool_use") => {
                    let (Some(id), Some(name)) = (text("id"), text("name")) else {
                        continue;
                    };
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    let (title, kind) = tool_call(&name, &input);
                    self.tools.insert(id.clone(), (name, input));
                    events.push(SessionEventKind::ToolCall {
                        tool_call_id: id,
                        title,
                        kind: Some(kind.to_owned()),
                        status: ToolStatus::InProgress,
                    });
                }
                _ => {}
            }
        }
        events
    }

    fn tool_results(&mut self, message: &Value) -> Vec<SessionEventKind> {
        let blocks: Vec<&Value> = message
            .pointer("/message/content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            .collect();
        // The structured result belongs to the message's only tool result.
        let structured = (blocks.len() == 1)
            .then(|| message.get("tool_use_result"))
            .flatten()
            .filter(|r| r.is_object());
        let mut events = Vec::new();
        for block in blocks {
            let Some(id) = block.get("tool_use_id").and_then(Value::as_str) else {
                continue;
            };
            let is_error = block.get("is_error").and_then(Value::as_bool) == Some(true);
            if let Some((name, input)) = self.tools.remove(id) {
                let text = result_text(block.get("content"));
                let detail = tool_detail(&name, &input, structured, &text, is_error);
                if detail != ToolDetail::default() {
                    events.push(SessionEventKind::ToolDetail {
                        tool_call_id: id.to_owned(),
                        detail,
                    });
                }
            }
            events.push(SessionEventKind::ToolCallUpdated {
                tool_call_id: id.to_owned(),
                title: None,
                status: Some(if is_error {
                    ToolStatus::Failed
                } else {
                    ToolStatus::Completed
                }),
            });
        }
        events
    }

    /// The end of a turn (`result`): how it stopped, and the usage and error events.
    pub fn turn_end(
        &mut self,
        result: &Value,
        interrupted: bool,
    ) -> (StopReason, Vec<SessionEventKind>) {
        let mut events = Vec::new();
        let usage = result.get("usage").unwrap_or(&Value::Null);
        let num = |v: &Value, key: &str| v.get(key).and_then(Value::as_u64).unwrap_or(0);
        let input = num(usage, "input_tokens")
            + num(usage, "cache_read_input_tokens")
            + num(usage, "cache_creation_input_tokens");
        let output = num(usage, "output_tokens");
        self.tokens.input += input;
        self.tokens.cached_input += num(usage, "cache_read_input_tokens");
        self.tokens.output += output;
        self.tokens.reasoning_output += usage
            .pointer("/output_tokens_details/thinking_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        self.tokens.total += input + output;
        // The context in use: what the last request of the turn sent and got back.
        let last = usage
            .get("iterations")
            .and_then(Value::as_array)
            .and_then(|i| i.last())
            .unwrap_or(usage);
        let used = num(last, "input_tokens")
            + num(last, "cache_read_input_tokens")
            + num(last, "cache_creation_input_tokens")
            + num(last, "output_tokens");
        let size = result
            .get("modelUsage")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|m| m.values())
            .filter_map(|m| m.get("contextWindow").and_then(Value::as_u64))
            .max()
            .unwrap_or(0);
        let total_cost = result.get("total_cost_usd").and_then(Value::as_f64);
        let cost_usd = total_cost.map(|total| {
            let turn = (total - self.cost_so_far).max(0.0);
            self.cost_so_far = total;
            turn
        });
        if input + output > 0 {
            events.push(SessionEventKind::Usage {
                used,
                size,
                cost_usd,
                tokens: Some(self.tokens.clone()),
            });
        }
        let subtype = result.get("subtype").and_then(Value::as_str).unwrap_or("");
        let stop_reason = if interrupted {
            StopReason::Cancelled
        } else {
            match (subtype, result.get("stop_reason").and_then(Value::as_str)) {
                ("success", Some("max_tokens")) => StopReason::MaxTokens,
                ("success", Some("refusal")) => StopReason::Refusal,
                ("success", _) => StopReason::EndTurn,
                ("error_max_turns", _) => StopReason::MaxTurnRequests,
                _ => StopReason::Error,
            }
        };
        if stop_reason == StopReason::Error {
            let errors: Vec<&str> = result
                .get("errors")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            events.push(SessionEventKind::Error {
                message: if errors.is_empty() {
                    result
                        .get("result")
                        .and_then(Value::as_str)
                        .unwrap_or(subtype)
                        .to_owned()
                } else {
                    errors.join("\n")
                },
            });
        }
        events.push(SessionEventKind::TurnCompleted {
            turn_id: result
                .get("user_message_uuid")
                .and_then(Value::as_str)
                .map(str::to_owned),
            stop_reason,
        });
        (stop_reason, events)
    }
}

/// A `can_use_tool` request as the hub sees it.
pub fn permission_request(request_id: &str, request: &Value) -> PermissionRequest {
    let name = request
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let input = request.get("input").unwrap_or(&Value::Null);
    let (title, kind) = tool_call(name, input);
    PermissionRequest {
        id: request
            .get("tool_use_id")
            .and_then(Value::as_str)
            .unwrap_or(request_id)
            .to_owned(),
        turn_id: None,
        kind: match kind {
            "execute" => "command",
            "edit" => "file_change",
            other => other,
        }
        .to_owned(),
        title,
    }
}

/// The answer to `can_use_tool`.
pub fn permission_response(decision: PermissionDecision, request: &Value) -> Value {
    let input = request.get("input").cloned().unwrap_or(json!({}));
    match decision {
        PermissionDecision::Allow => json!({ "behavior": "allow", "updatedInput": input }),
        PermissionDecision::AllowForSession => {
            let mut answer = json!({ "behavior": "allow", "updatedInput": input });
            // Claude Code's own suggestion of a rule that stops this from asking again.
            if let Some(rules) = request
                .get("permission_suggestions")
                .filter(|s| s.is_array())
            {
                answer["updatedPermissions"] = rules.clone();
            }
            answer
        }
        PermissionDecision::Deny => {
            json!({ "behavior": "deny", "message": "Declined by CrossAgents." })
        }
        PermissionDecision::Cancel => json!({
            "behavior": "deny",
            "message": "Declined by CrossAgents; the turn is stopped.",
            "interrupt": true,
        }),
    }
}

const FIVE_HOURS: u64 = 5 * 3600;
const SEVEN_DAYS: u64 = 7 * 86_400;

fn window(name: &str, seconds: u64, percent: f64, resets_at: Option<DateTime<Utc>>) -> RateWindow {
    RateWindow {
        name: name.into(),
        used_percent: percent,
        window_seconds: Some(seconds),
        resets_at,
        remaining: None,
        total: None,
    }
}

fn seconds_window_length(name: &str) -> Option<u64> {
    match name {
        "five_hour" => Some(FIVE_HOURS),
        n if n.starts_with("seven_day") => Some(SEVEN_DAYS),
        _ => None,
    }
}

/// The per-turn quota report (`rate_limit_event`): utilization is 0..1, times are seconds.
pub fn rate_limit_event(info: &Value) -> RateLimits {
    let mut windows: Vec<RateWindow> = info
        .get("unifiedWindows")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(name, w)| {
            Some(window(
                name,
                seconds_window_length(name)?,
                w.get("utilization").and_then(Value::as_f64)? * 100.0,
                w.get("resetsAt")
                    .and_then(Value::as_i64)
                    .and_then(|s| DateTime::from_timestamp(s, 0)),
            ))
        })
        .collect();
    if windows.is_empty()
        && let (Some(name), Some(used)) = (
            info.get("rateLimitType").and_then(Value::as_str),
            info.get("utilization").and_then(Value::as_f64),
        )
        && let Some(seconds) = seconds_window_length(name)
    {
        windows.push(window(
            name,
            seconds,
            used * 100.0,
            info.get("resetsAt")
                .and_then(Value::as_i64)
                .and_then(|s| DateTime::from_timestamp(s, 0)),
        ));
    }
    windows.sort_by_key(|w| w.window_seconds);
    let rejected = info.get("status").and_then(Value::as_str) == Some("rejected");
    RateLimits {
        windows,
        reached: rejected.then(|| {
            info.get("rateLimitType")
                .and_then(Value::as_str)
                .unwrap_or("rate_limited")
                .to_owned()
        }),
        ..RateLimits::default()
    }
}

/// A full read (`get_usage`): the plan, the five-hour and weekly windows, the per-model weekly
/// limits and extra usage. `None` when the account has no plan limits (an API key).
pub fn usage_limits(usage: &Value) -> Option<RateLimits> {
    let limits = usage.get("rate_limits").filter(|l| l.is_object())?;
    let window_at = |key: &str, name: &str, seconds: u64| {
        let w = limits.get(key).filter(|w| w.is_object())?;
        Some(window(
            name,
            seconds,
            w.get("utilization").and_then(Value::as_f64)?,
            w.get("resets_at")
                .and_then(Value::as_str)
                .and_then(|s| s.parse().ok()),
        ))
    };
    let windows: Vec<RateWindow> = [
        window_at("five_hour", "five_hour", FIVE_HOURS),
        window_at("seven_day", "seven_day", SEVEN_DAYS),
    ]
    .into_iter()
    .flatten()
    .collect();
    let reached = windows.iter().any(|w| w.used_percent >= 100.0);
    let mut groups = vec![LimitGroup {
        id: "all".into(),
        name: None,
        model: None,
        windows: windows.clone(),
        reached,
    }];
    for (key, name) in [("seven_day_opus", "Opus"), ("seven_day_sonnet", "Sonnet")] {
        if let Some(w) = window_at(key, "seven_day", SEVEN_DAYS) {
            groups.push(LimitGroup {
                id: name.to_lowercase(),
                name: Some(name.into()),
                model: Some(name.to_lowercase()),
                reached: w.used_percent >= 100.0,
                windows: vec![w],
            });
        }
    }
    for scoped in limits
        .get("model_scoped")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(name) = scoped.get("display_name").and_then(Value::as_str) else {
            continue;
        };
        let id = name.to_lowercase();
        if groups.iter().any(|g| g.id == id) {
            continue;
        }
        let used = scoped
            .get("utilization")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        groups.push(LimitGroup {
            id,
            name: Some(name.into()),
            model: None,
            reached: used >= 100.0,
            windows: vec![window(
                "seven_day",
                SEVEN_DAYS,
                used,
                scoped
                    .get("resets_at")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse().ok()),
            )],
        });
    }
    let credits = limits
        .get("extra_usage")
        .filter(|e| e.get("is_enabled").and_then(Value::as_bool) == Some(true))
        .map(|extra| {
            let amount = |key: &str| extra.get(key).and_then(Value::as_f64);
            Credits {
                has_credits: true,
                unlimited: amount("monthly_limit").is_none(),
                balance: amount("monthly_limit")
                    .map(|limit| format!("{:.2}", limit - amount("used_credits").unwrap_or(0.0))),
            }
        });
    Some(RateLimits {
        windows,
        credits,
        plan: usage
            .get("subscription_type")
            .and_then(Value::as_str)
            .map(str::to_owned),
        reached: reached.then(|| "rate_limited".to_owned()),
        by_limit: groups,
        reset_credits: None,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn launch_args_for_new_resumed_and_forked_sessions() {
        let launch = Launch {
            model: Some("haiku".into()),
            effort: Some("high".into()),
            bypass: true,
            permission_mode: None,
            mcp_servers: vec![McpServerSpec {
                name: "crossagents".into(),
                command: "/bin/crossagents".into(),
                args: vec!["mcp".into()],
                env: BTreeMap::from([("TOKEN".into(), "t".into())]),
            }],
            instructions: Some("You are w1.".into()),
            setting_sources: DEFAULT_SETTING_SOURCES.into(),
        };
        let new = args(&launch, &Start::New("s1".into()), Flavor::Claude);
        let after = |flag: &str| {
            let i = new.iter().position(|a| a == flag).expect(flag);
            new[i + 1].clone()
        };
        assert_eq!(after("--input-format"), "stream-json");
        assert_eq!(after("--permission-prompt-tool"), "stdio");
        assert_eq!(after("--setting-sources"), "project,local");
        assert_eq!(after("--model"), "haiku");
        assert_eq!(after("--effort"), "high");
        assert_eq!(after("--append-system-prompt"), "You are w1.");
        assert_eq!(after("--session-id"), "s1");
        assert!(new.contains(&"--dangerously-skip-permissions".to_owned()));
        let mcp: Value = serde_json::from_str(&after("--mcp-config")).unwrap();
        assert_eq!(mcp["mcpServers"]["crossagents"]["env"]["TOKEN"], "t");

        let resumed = args(
            &Launch::default(),
            &Start::Resume("s1".into()),
            Flavor::Claude,
        );
        assert!(resumed.windows(2).any(|w| w == ["--resume", "s1"]));
        assert!(!resumed.contains(&"--mcp-config".to_owned()));
        assert!(!resumed.contains(&"--dangerously-skip-permissions".to_owned()));

        let fork = args(
            &Launch::default(),
            &Start::Fork {
                from: "s1".into(),
                id: "s2".into(),
                at: Some("m9".into()),
            },
            Flavor::Claude,
        );
        for pair in [
            ["--resume", "s1"],
            ["--session-id", "s2"],
            ["--resume-session-at", "m9"],
        ] {
            assert!(fork.windows(2).any(|w| w == pair), "{pair:?}");
        }
        assert!(fork.contains(&"--fork-session".to_owned()));
    }

    #[test]
    fn codebuddy_gets_the_protocol_without_claude_only_flags() {
        let args = args(
            &Launch::default(),
            &Start::New("s1".into()),
            Flavor::Codebuddy,
        );
        assert!(!args.contains(&"--permission-prompt-tool".to_owned()));
        for flag in [
            "--input-format",
            "--output-format",
            "--include-partial-messages",
            "--strict-mcp-config",
            "--setting-sources",
            "--session-id",
        ] {
            assert!(args.contains(&flag.to_owned()), "{flag}");
        }
    }

    #[test]
    fn edits_without_a_structured_result_are_read_from_their_input() {
        let write = tool_detail(
            "Write",
            &json!({ "file_path": "/w/note.txt", "content": "v1\nv2" }),
            None,
            "Successfully created and wrote to new file: /w/note.txt",
            false,
        );
        assert_eq!(write.changes[0].kind, "add");
        assert_eq!(write.changes[0].diff, "@@ -0,0 +1,2 @@\n+v1\n+v2\n");
        let edit = tool_detail(
            "MultiEdit",
            &json!({ "file_path": "/w/a.rs", "edits": [
                { "old_string": "a", "new_string": "b" },
                { "old_string": "c", "new_string": "d\ne" },
            ] }),
            None,
            "ok",
            false,
        );
        assert_eq!(edit.changes[0].kind, "update");
        assert_eq!(edit.changes[0].diff, "@@ @@\n-a\n+b\n@@ @@\n-c\n+d\n+e\n");
        // A failed edit changed nothing.
        let failed = tool_detail(
            "Edit",
            &json!({ "file_path": "/w/a.rs", "old_string": "x", "new_string": "y" }),
            None,
            "String not found",
            true,
        );
        assert!(failed.changes.is_empty());
    }

    #[test]
    fn effort_changes_use_each_cli_s_own_request() {
        assert_eq!(
            set_effort(Flavor::Claude, "high"),
            json!({ "subtype": "apply_flag_settings", "settings": { "effortLevel": "high" } })
        );
        assert_eq!(
            set_effort(Flavor::Codebuddy, "minimal"),
            json!({ "subtype": "set_config", "config": { "reasoningEffort": "minimal" } })
        );
    }

    #[test]
    fn codebuddy_withholds_an_enclosing_session_but_keeps_sign_in() {
        for key in [
            "CODEBUDDY_MCP_CONFIG",
            "CODEBUDDY_SESSION_ID",
            "CODEBUDDY_HOST",
            "CODEBUDDY_SERVICE_PROXY_URL",
            "codebuddy_force_lite_wb_bundle",
        ] {
            assert!(Flavor::Codebuddy.withholds(key), "{key}");
        }
        for key in [
            "CODEBUDDY_API_KEY",
            "CODEBUDDY_INTERNET_ENVIRONMENT",
            "HTTPS_PROXY",
        ] {
            assert!(!Flavor::Codebuddy.withholds(key), "{key}");
        }
        assert!(!Flavor::Claude.withholds("CODEBUDDY_MCP_CONFIG"));
    }

    fn stream(event: Value) -> Value {
        json!({ "type": "stream_event", "event": event, "parent_tool_use_id": null })
    }

    #[test]
    fn streamed_text_is_not_repeated_and_tools_come_with_details() {
        let mut t = Translator::default();
        assert!(
            t.translate(&stream(
                json!({ "type": "message_start", "message": { "id": "m1" } })
            ))
            .is_empty()
        );
        assert_eq!(
            t.translate(&stream(json!({ "type": "content_block_delta", "delta": { "type": "text_delta", "text": "Hel" } }))),
            [SessionEventKind::Output { stream: OutputStream::Text, text: "Hel".into() }]
        );
        let assistant = json!({ "type": "assistant", "parent_tool_use_id": null, "message": { "id": "m1", "content": [
            { "type": "text", "text": "Hello" },
            { "type": "tool_use", "id": "t1", "name": "PowerShell", "input": { "command": "git status\nmore" } },
        ]}});
        assert_eq!(
            t.translate(&assistant),
            [SessionEventKind::ToolCall {
                tool_call_id: "t1".into(),
                title: "git status".into(),
                kind: Some("execute".into()),
                status: ToolStatus::InProgress,
            }]
        );
        let result = json!({ "type": "user", "parent_tool_use_id": null,
            "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "t1", "content": "fatal: nope\nExit code 128", "is_error": true }] },
            "tool_use_result": "Error: Exit code 128" });
        let events = t.translate(&result);
        let SessionEventKind::ToolDetail { detail, .. } = &events[0] else {
            panic!("{events:?}")
        };
        assert_eq!(detail.exit_code, Some(128));
        assert_eq!(detail.command.as_deref(), Some("git status\nmore"));
        assert_eq!(
            events[1],
            SessionEventKind::ToolCallUpdated {
                tool_call_id: "t1".into(),
                title: None,
                status: Some(ToolStatus::Failed)
            }
        );

        // A message that was not streamed shows its text; a subagent's steps are skipped.
        let plain = json!({ "type": "assistant", "message": { "id": "m2", "content": [{ "type": "text", "text": "Done" }] }});
        assert_eq!(
            t.translate(&plain),
            [SessionEventKind::Output {
                stream: OutputStream::Text,
                text: "Done".into()
            }]
        );
        let sub = json!({ "type": "assistant", "parent_tool_use_id": "t9", "message": { "id": "m3", "content": [{ "type": "text", "text": "x" }] }});
        assert!(t.translate(&sub).is_empty());
    }

    #[test]
    fn shell_output_and_file_diffs_from_structured_results() {
        let shell = tool_detail(
            "Bash",
            &json!({ "command": "echo hi" }),
            Some(&json!({ "stdout": "hi", "stderr": "", "interrupted": false })),
            "hi",
            false,
        );
        assert_eq!(
            (shell.output.as_deref(), shell.exit_code),
            (Some("hi"), Some(0))
        );

        let edit = tool_detail(
            "Edit",
            &json!({ "file_path": "/w/a.txt" }),
            Some(&json!({ "filePath": "/w/a.txt", "structuredPatch": [
                { "oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1, "lines": ["-v1", "+v2"] }] })),
            "ok",
            false,
        );
        assert_eq!(edit.changes[0].kind, "update");
        assert_eq!(edit.changes[0].diff, "@@ -1,1 +1,1 @@\n-v1\n+v2\n");

        let create = tool_detail(
            "Write",
            &json!({ "file_path": "/w/n.txt" }),
            Some(&json!({ "type": "create", "filePath": "/w/n.txt", "content": "a\nb" })),
            "ok",
            false,
        );
        assert_eq!(
            (
                create.changes[0].kind.as_str(),
                create.changes[0].diff.as_str()
            ),
            ("add", "@@ -0,0 +1,2 @@\n+a\n+b\n")
        );

        let mcp = tool_detail(
            "mcp__crossagents__task_list",
            &json!({ "all": true }),
            None,
            "[]",
            false,
        );
        assert_eq!(
            tool_call("mcp__crossagents__task_list", &json!({})),
            ("crossagents.task_list".into(), "mcp")
        );
        assert_eq!(mcp.result.as_deref(), Some("[]"));
        assert!(mcp.arguments.unwrap().contains("\"all\": true"));
    }

    #[test]
    fn turn_end_reports_usage_cost_per_turn_and_stop_reason() {
        let mut t = Translator::default();
        let result = |cost: f64, subtype: &str| {
            json!({ "type": "result", "subtype": subtype, "is_error": subtype != "success",
            "user_message_uuid": "u1", "total_cost_usd": cost, "stop_reason": "end_turn", "errors": ["boom"],
            "usage": { "input_tokens": 2, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 10, "output_tokens": 5,
                "output_tokens_details": { "thinking_tokens": 3 },
                "iterations": [{ "input_tokens": 1, "cache_read_input_tokens": 50, "cache_creation_input_tokens": 0, "output_tokens": 4 }] },
            "modelUsage": { "claude-haiku-5-5": { "contextWindow": 1000000 } } })
        };
        let (stop, events) = t.turn_end(&result(0.5, "success"), false);
        assert_eq!(stop, StopReason::EndTurn);
        let SessionEventKind::Usage {
            used,
            size,
            cost_usd,
            tokens,
        } = &events[0]
        else {
            panic!()
        };
        assert_eq!((*used, *size, *cost_usd), (55, 1_000_000, Some(0.5)));
        assert_eq!(tokens.as_ref().unwrap().cached_input, 100);
        assert_eq!(
            events[1],
            SessionEventKind::TurnCompleted {
                turn_id: Some("u1".into()),
                stop_reason: StopReason::EndTurn
            }
        );

        let (_, events) = t.turn_end(&result(0.75, "success"), false);
        let SessionEventKind::Usage {
            cost_usd, tokens, ..
        } = &events[0]
        else {
            panic!()
        };
        assert_eq!(*cost_usd, Some(0.25));
        assert_eq!(tokens.as_ref().unwrap().input, 224);

        assert_eq!(
            t.turn_end(&result(0.75, "error_during_execution"), true).0,
            StopReason::Cancelled
        );
        let (stop, events) = t.turn_end(&result(0.75, "error_during_execution"), false);
        assert_eq!(stop, StopReason::Error);
        assert!(events.contains(&SessionEventKind::Error {
            message: "boom".into()
        }));
    }

    #[test]
    fn permission_requests_and_answers() {
        let request = json!({ "subtype": "can_use_tool", "tool_name": "PowerShell", "tool_use_id": "t1",
            "input": { "command": "Remove-Item x" },
            "permission_suggestions": [{ "type": "addRules", "rules": [], "behavior": "allow", "destination": "session" }] });
        let asked = permission_request("r1", &request);
        assert_eq!(
            (asked.id.as_str(), asked.kind.as_str(), asked.title.as_str()),
            ("t1", "command", "Remove-Item x")
        );
        let allow = permission_response(PermissionDecision::Allow, &request);
        assert_eq!(allow["updatedInput"]["command"], "Remove-Item x");
        assert!(allow.get("updatedPermissions").is_none());
        assert!(permission_response(PermissionDecision::AllowForSession, &request)["updatedPermissions"].is_array());
        assert_eq!(
            permission_response(PermissionDecision::Deny, &request)["behavior"],
            "deny"
        );
        assert_eq!(
            permission_response(PermissionDecision::Cancel, &request)["interrupt"],
            true
        );
    }

    #[test]
    fn quota_from_rate_limit_events_and_get_usage() {
        // As seen live on a Pro account (docs/research/claude-stream-json.md).
        let event = rate_limit_event(
            &json!({ "status": "allowed_warning", "rateLimitType": "seven_day", "utilization": 0.65,
            "unifiedWindows": { "seven_day": { "utilization": 0.65, "resetsAt": 1793084400 }, "five_hour": { "utilization": 0.19, "resetsAt": 1792656600 } } }),
        );
        assert_eq!(
            event
                .windows
                .iter()
                .map(|w| w.name.as_str())
                .collect::<Vec<_>>(),
            ["five_hour", "seven_day"]
        );
        assert_eq!(event.windows[1].used_percent.round(), 65.0);
        assert_eq!(event.windows[0].window_seconds, Some(FIVE_HOURS));
        assert!(event.reached.is_none());
        assert!(
            rate_limit_event(&json!({ "status": "rejected", "rateLimitType": "five_hour" }))
                .reached
                .is_some()
        );

        let usage = json!({ "subscription_type": "pro", "rate_limits_available": true, "rate_limits": {
            "five_hour": { "utilization": 20, "resets_at": "2026-10-22T08:09:59.854928+00:00" },
            "seven_day": { "utilization": 66, "resets_at": "2026-10-27T06:59:59.854948+00:00" },
            "seven_day_opus": null, "seven_day_sonnet": { "utilization": 10, "resets_at": null },
            "model_scoped": [{ "display_name": "Fable", "utilization": 5, "resets_at": null }],
            "extra_usage": { "is_enabled": false } } });
        let limits = usage_limits(&usage).unwrap();
        assert_eq!(limits.plan.as_deref(), Some("pro"));
        assert_eq!(limits.windows.len(), 2);
        assert_eq!(limits.windows[0].used_percent, 20.0);
        assert!(limits.windows[0].resets_at.is_some());
        assert_eq!(
            limits
                .by_limit
                .iter()
                .map(|g| g.id.as_str())
                .collect::<Vec<_>>(),
            ["all", "sonnet", "fable"]
        );
        assert!(limits.credits.is_none());
        assert!(
            usage_limits(&json!({ "rate_limits_available": false, "rate_limits": null })).is_none()
        );
    }

    #[test]
    fn control_messages_round_trip() {
        let ok = json!({ "type": "control_response", "response": { "subtype": "success", "request_id": "r1", "response": { "a": 1 } } });
        assert_eq!(
            control_answer(&ok),
            Some(("r1".into(), Ok(json!({ "a": 1 }))))
        );
        let err = control_error("r2", "nope");
        assert_eq!(
            control_answer(&err),
            Some(("r2".into(), Err("nope".into())))
        );
        assert_eq!(
            control_success("r3", json!(1))["response"]["request_id"],
            "r3"
        );
        assert_eq!(user_message("s", "u", "hi")["message"]["content"], "hi");
    }
}
