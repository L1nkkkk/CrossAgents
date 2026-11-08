//! What `with-cli chat -o stream-json` prints, and the arguments that make it print it.
//!
//! One JSON object per line, AG-UI style: `{"type": ..., "rawEvent": {...}}`. Seen on
//! with-cli v0.31.0 (docs/research/with-cli.md):
//!
//! - `RUN_STARTED` / `RUN_FINISHED` / `RUN_ERROR` frame the turn; `message_id` in every
//!   `rawEvent` is the request id `with-cli cancel` takes.
//! - `TEXT_MESSAGE_CONTENT` and `THINKING_TEXT_MESSAGE_CONTENT` carry text in `content`.
//! - `TOOL_CALL_START` names the tool; `need_result: false` marks with-cli's own bookkeeping
//!   calls ("任务规划中") nobody needs to see (`default_show: false` does not: real tools such
//!   as `read_file` carry it too). `TOOL_CALL_ARGS` streams the arguments, either
//!   whole (`document`) or as JSON-patch-like `patchs` where `add` to a string appends to
//!   it. `TOOL_CALL_RESULT` ends the call, with the result in `rawEvent.result` and its text
//!   in the top-level `content`.
//! - `STEP_FINISHED` carries one model call's `token_usage`.
//! - `HEARTBEAT`, `STEP_STARTED`, `TEXT_MESSAGE_START/END`, `CUSTOM` say nothing new.
//!
//! The process also prints plain lines: `requestID: ...` and `sessionID: ...` at the end,
//! and errors that stop it before the turn starts ("不支持该模型: ...", exit code 1).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crossagents_core::{
    FileChange, OutputStream, SessionEvent, SessionEventKind, TokenUsage, ToolDetail, ToolStatus,
};
use serde_json::{Map, Value};

use crate::acp::unified_diff;

/// Prompts longer than this (UTF-16 units) go in a file: a Windows command line holds
/// 32767, and the other arguments need room too.
pub const PROMPT_ARG_LIMIT: usize = 24_000;

/// A command's output kept in a tool call's detail, from its end.
const OUTPUT_TAIL: usize = 8_000;

/// How much of a `RUN_ERROR`'s own JSON is kept when it names no reason.
const ERROR_JSON: usize = 2_000;

/// The arguments of one turn, after the program.
#[derive(Debug)]
pub struct ChatArgs<'a> {
    pub session_id: &'a str,
    pub cwd: &'a Path,
    pub prompt: &'a str,
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    /// `--enable-thinking`; `None` leaves the model's default.
    pub thinking: Option<bool>,
    /// `--access-mode`: `default`, `workspace_write` or `full_access`.
    pub access_mode: Option<&'a str>,
    /// `-c`: a client configuration naming the session's MCP servers.
    pub config: Option<&'a Path>,
    /// `--user-rules`: the session's opening instructions.
    pub rules: Option<&'a Path>,
    pub images: &'a [PathBuf],
    pub files: &'a [PathBuf],
}

impl Default for ChatArgs<'_> {
    fn default() -> Self {
        Self {
            session_id: "",
            cwd: Path::new(""),
            prompt: "",
            model: None,
            effort: None,
            thinking: None,
            access_mode: None,
            config: None,
            rules: None,
            images: &[],
            files: &[],
        }
    }
}

/// `chat` and its flags. Values go as `--flag=value`, so a prompt that starts with `-` is
/// still a prompt.
pub fn chat_args(args: &ChatArgs<'_>) -> Vec<String> {
    let mut out = vec![
        "chat".to_owned(),
        "--no-update".to_owned(),
        "--output-format=stream-json".to_owned(),
        format!("--sessionId={}", args.session_id),
        format!("--workspace={}", args.cwd.display()),
    ];
    if let Some(config) = args.config {
        out.push(format!("--config={}", config.display()));
    }
    if let Some(model) = args.model.filter(|m| !m.is_empty()) {
        out.push(format!("--model={model}"));
    }
    if let Some(effort) = args.effort.filter(|e| !e.is_empty()) {
        out.push(format!("--reasoning-effort={effort}"));
    }
    if let Some(thinking) = args.thinking {
        out.push(format!("--enable-thinking={thinking}"));
    }
    if let Some(mode) = args.access_mode.filter(|m| !m.is_empty()) {
        out.push(format!("--access-mode={mode}"));
    }
    if let Some(rules) = args.rules {
        out.push(format!("--user-rules={}", rules.display()));
    }
    for image in args.images {
        out.push(format!("--images={}", image.display()));
    }
    for file in args.files {
        out.push(format!("--files={}", file.display()));
    }
    out.push(format!("--prompt={}", args.prompt));
    out
}

/// The model ids in what `with-cli model list` prints: one per line, indented, the id
/// first.
pub fn parse_model_list(text: &str) -> Vec<String> {
    let mut models = Vec::new();
    for line in text.lines() {
        if !line.starts_with(' ') || line.contains("[flags]") {
            continue;
        }
        let Some(id) = line.split_whitespace().next() else {
            continue;
        };
        let is_id = id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            && !id.ends_with(".exe");
        if is_id && !models.iter().any(|m| m == id) {
            models.push(id.to_owned());
        }
    }
    models
}

/// One line of output, translated.
#[derive(Debug, PartialEq)]
pub enum Line {
    Events(Vec<SessionEvent>),
    /// Not JSON: an error the CLI printed instead of starting the turn, say.
    Text(String),
    Skip,
}

/// What one turn's output said so far.
#[derive(Debug, Default)]
pub struct Turn {
    /// `message_id`: what `with-cli cancel --request-id` takes.
    pub request_id: Option<String>,
    /// `RUN_FINISHED` arrived.
    pub finished: bool,
    /// What `RUN_ERROR` said.
    pub error: Option<String>,
    hidden: HashSet<String>,
    tools: HashMap<String, Tool>,
}

#[derive(Debug)]
struct Tool {
    name: String,
    args: Value,
}

impl Turn {
    /// Translates one line; `totals` is the session's token count, which `STEP_FINISHED`
    /// adds to.
    pub fn line(&mut self, line: &str, totals: &mut TokenUsage) -> Line {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            return Line::Skip;
        }
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            if line.starts_with("requestID:") || line.starts_with("sessionID:") {
                return Line::Skip;
            }
            return Line::Text(line.to_owned());
        };
        let Some(kind) = message.get("type").and_then(Value::as_str) else {
            return Line::Skip;
        };
        let raw = message.get("rawEvent").cloned().unwrap_or(Value::Null);
        if self.request_id.is_none()
            && let Some(id) = str_of(&raw, "message_id")
        {
            self.request_id = Some(id.to_owned());
        }
        let kinds = self.translate(kind, &raw, &message, totals);
        if kinds.is_empty() {
            return Line::Skip;
        }
        Line::Events(
            kinds
                .into_iter()
                .map(|kind| SessionEvent::with_raw(kind, message.clone()))
                .collect(),
        )
    }

    fn translate(
        &mut self,
        kind: &str,
        raw: &Value,
        message: &Value,
        totals: &mut TokenUsage,
    ) -> Vec<SessionEventKind> {
        match kind {
            "TEXT_MESSAGE_CONTENT" => text_of(raw)
                .map(|text| SessionEventKind::Output {
                    stream: OutputStream::Text,
                    text,
                })
                .into_iter()
                .collect(),
            "THINKING_TEXT_MESSAGE_CONTENT" => text_of(raw)
                .map(|text| SessionEventKind::Output {
                    stream: OutputStream::Thought,
                    text,
                })
                .into_iter()
                .collect(),
            "TOOL_CALL_START" => self.tool_start(raw),
            "TOOL_CALL_ARGS" => {
                self.tool_args(raw);
                Vec::new()
            }
            "TOOL_CALL_RESULT" => self.tool_result(raw, message),
            "STEP_FINISHED" => usage(raw, totals).into_iter().collect(),
            "RUN_FINISHED" => {
                self.finished = true;
                Vec::new()
            }
            "RUN_ERROR" => {
                let message = run_error_text(message, raw);
                self.error = Some(message.clone());
                vec![SessionEventKind::Error { message }]
            }
            _ => Vec::new(),
        }
    }

    fn tool_start(&mut self, raw: &Value) -> Vec<SessionEventKind> {
        let Some(id) = str_of(raw, "tool_call_id") else {
            return Vec::new();
        };
        let name = str_of(raw, "name").unwrap_or_default().to_owned();
        if bookkeeping(raw, &name) {
            self.hidden.insert(id.to_owned());
            return Vec::new();
        }
        let title = tool_title(&name).map(str::to_owned).unwrap_or_else(|| {
            str_of(raw, "ch_name")
                .or_else(|| str_of(raw, "display_name"))
                .unwrap_or(&name)
                .to_owned()
        });
        let kind = tool_kind(&name).to_owned();
        self.tools.insert(
            id.to_owned(),
            Tool {
                name,
                args: Value::Object(Map::new()),
            },
        );
        vec![SessionEventKind::ToolCall {
            tool_call_id: id.to_owned(),
            title,
            kind: Some(kind),
            status: ToolStatus::InProgress,
        }]
    }

    fn tool_args(&mut self, raw: &Value) {
        let Some(id) = str_of(raw, "tool_call_id") else {
            return;
        };
        if self.hidden.contains(id) {
            return;
        }
        let tool = self.tools.entry(id.to_owned()).or_insert_with(|| Tool {
            name: String::new(),
            args: Value::Object(Map::new()),
        });
        if let Some(Value::Object(document)) = raw.get("document") {
            if !tool.args.is_object() {
                tool.args = Value::Object(Map::new());
            }
            let args = tool.args.as_object_mut().expect("object");
            for (key, value) in document {
                args.insert(key.clone(), value.clone());
            }
        }
        let patches = raw.get("patchs").or_else(|| raw.get("patches"));
        for patch in patches.and_then(Value::as_array).into_iter().flatten() {
            apply_patch(&mut tool.args, patch);
        }
    }

    fn tool_result(&mut self, raw: &Value, message: &Value) -> Vec<SessionEventKind> {
        let Some(id) = str_of(raw, "tool_call_id") else {
            return Vec::new();
        };
        if self.hidden.remove(id) {
            return Vec::new();
        }
        let tool = self.tools.remove(id).unwrap_or(Tool {
            name: String::new(),
            args: Value::Null,
        });
        let failure = failure(raw);
        let detail = detail(&tool.name, &tool.args, raw, message, failure.as_deref());
        let mut events = Vec::new();
        if detail != ToolDetail::default() {
            events.push(SessionEventKind::ToolDetail {
                tool_call_id: id.to_owned(),
                detail,
            });
        }
        events.push(SessionEventKind::ToolCallUpdated {
            tool_call_id: id.to_owned(),
            title: subject(&tool.name, &tool.args),
            status: Some(if failure.is_some() {
                ToolStatus::Failed
            } else {
                ToolStatus::Completed
            }),
        });
        events
    }
}

fn str_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// A call with-cli makes for itself ("任务规划中"), which nobody needs to see. Not
/// `default_show: false`: with-cli v0.31.0 marks `read_file` and other real tools so too,
/// and hiding them left reads, searches and MCP calls out of the transcript. Its own calls
/// want no result.
fn bookkeeping(raw: &Value, name: &str) -> bool {
    name == "task_planning" || raw.get("need_result") == Some(&Value::Bool(false))
}

fn text_of(raw: &Value) -> Option<String> {
    str_of(raw, "content")
        .or_else(|| str_of(raw, "delta"))
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

/// ACP's tool kinds, which clients already know how to show.
fn tool_kind(name: &str) -> &'static str {
    match name {
        "terminal" => "execute",
        "write_to_file" | "replace_in_file" | "multi_replace" | "edit_file" => "edit",
        "delete_file" => "delete",
        "read_file" | "list_dir" | "read_image" => "read",
        "grep_search" | "search_files" | "web_search" | "codebase_search" => "search",
        "web_fetch" => "fetch",
        _ if name.starts_with("browser_") => "fetch",
        _ => "other",
    }
}

/// The title a call starts with: named by what it does, not by what with-cli calls it
/// (`ch_name` says "查找文件" for a search, which reads as the wrong thing). `None` for
/// `other`: those keep with-cli's name.
fn tool_title(name: &str) -> Option<&'static str> {
    match tool_kind(name) {
        "search" => Some("搜索"),
        "read" if name == "list_dir" => Some("列出目录"),
        "read" => Some("读取文件"),
        "execute" => Some("执行命令"),
        "edit" => Some("修改文件"),
        "delete" => Some("删除文件"),
        "fetch" => Some("获取网页"),
        _ => None,
    }
}

/// The file a file tool works on.
fn file_path(args: &Value) -> Option<String> {
    ["file_path", "path", "target_file"]
        .iter()
        .find_map(|key| str_of(args, key))
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
}

/// A title naming what the call worked on, once its arguments are known.
fn subject(name: &str, args: &Value) -> Option<String> {
    match name {
        "terminal" => str_of(args, "command").map(str::to_owned),
        "write_to_file" | "replace_in_file" | "multi_replace" | "edit_file" | "delete_file"
        | "read_file" => file_path(args),
        _ => None,
    }
}

/// Why the call failed, if it did.
fn failure(raw: &Value) -> Option<String> {
    if let Some(error) = raw.get("error").filter(|e| !e.is_null()) {
        return Some(value_text(error));
    }
    let result = raw.get("result")?;
    if let Some(error) = result.get("error").filter(|e| !e.is_null()) {
        return Some(value_text(error));
    }
    let status = str_of(result, "status")?.to_ascii_uppercase();
    matches!(status.as_str(), "FAIL" | "FAILED" | "FAILURE" | "ERROR").then(|| {
        str_of(result, "message")
            .unwrap_or("tool failed")
            .to_owned()
    })
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Object(map) => ["content", "message", "error"]
            .iter()
            .find_map(|key| map.get(*key).and_then(Value::as_str))
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string()),
        _ => value.to_string(),
    }
}

fn error_text(raw: &Value) -> Option<String> {
    ["message", "error", "content", "detail"]
        .iter()
        .find_map(|key| raw.get(*key).filter(|v| !v.is_null()).map(value_text))
}

/// Why a `RUN_ERROR` ended the turn. AG-UI puts `code` and `message` on the event itself
/// (`{"type": "RUN_ERROR", "code": ..., "message": ...}`), and with-cli's rawEvent may
/// repeat or elaborate them; when neither says anything, the event's own JSON goes in
/// instead of a fixed line, so the transcript shows what actually arrived.
fn run_error_text(message: &Value, raw: &Value) -> String {
    let text = str_of(message, "message").filter(|t| !t.is_empty());
    let code = str_of(message, "code").filter(|c| !c.is_empty());
    match (code, text) {
        (Some(code), Some(text)) => format!("{code}: {text}"),
        (Some(code), None) => code.to_owned(),
        (None, Some(text)) => text.to_owned(),
        (None, None) => error_text(raw).unwrap_or_else(|| {
            format!(
                "with-cli reported an error: {}",
                head(&message.to_string(), ERROR_JSON)
            )
        }),
    }
}

/// The first `max` bytes of `text`, on a character boundary.
fn head(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn detail(
    name: &str,
    args: &Value,
    raw: &Value,
    message: &Value,
    failure: Option<&str>,
) -> ToolDetail {
    let mut detail = ToolDetail::default();
    let result = raw.get("result").filter(|r| !r.is_null());
    match name {
        "terminal" => {
            detail.command = str_of(args, "command").map(str::to_owned);
            detail.cwd = str_of(args, "cwd").map(str::to_owned);
            let content = result.and_then(|r| r.get("content"));
            detail.exit_code = content
                .and_then(|c| c.get("exit_code"))
                .and_then(Value::as_i64)
                .and_then(|c| i32::try_from(c).ok());
            let output: Vec<&str> = ["stdout", "stderr"]
                .iter()
                .filter_map(|key| content.and_then(|c| str_of(c, key)))
                .map(str::trim_end)
                .filter(|s| !s.is_empty())
                .collect();
            if !output.is_empty() {
                detail.output = Some(tail(&output.join("\n"), OUTPUT_TAIL));
            }
        }
        "write_to_file" | "replace_in_file" | "multi_replace" | "edit_file" | "delete_file" => {
            if let Some(path) = file_path(args) {
                let (kind, diff) = match name {
                    "delete_file" => ("delete", String::new()),
                    "write_to_file" => ("write", String::new()),
                    _ => match (str_of(args, "old_string"), str_of(args, "new_string")) {
                        (Some(old), Some(new)) => ("update", unified_diff(old, new)),
                        _ => ("update", String::new()),
                    },
                };
                detail.changes.push(FileChange {
                    path,
                    kind: kind.to_owned(),
                    diff,
                });
            }
        }
        _ => {
            let args = without_repeated_query(args);
            if args.as_object().is_some_and(|a| !a.is_empty()) {
                detail.arguments = serde_json::to_string_pretty(&args).ok();
            }
            detail.result = str_of(message, "content")
                .filter(|c| !c.is_empty())
                .map(|c| tail(c, OUTPUT_TAIL))
                .or_else(|| result.map(|r| tail(&value_text(r), OUTPUT_TAIL)));
        }
    }
    detail.error = failure.map(str::to_owned);
    detail
}

/// The arguments with `sub_content` / `sub_content_tip` dropped when they only repeat
/// `query` word for word: the panel needs one copy, not three.
fn without_repeated_query(args: &Value) -> Value {
    let mut kept = args.clone();
    let Some(query) = str_of(args, "query").map(str::to_owned) else {
        return kept;
    };
    if let Some(map) = kept.as_object_mut() {
        map.retain(|key, value| {
            !matches!(key.as_str(), "sub_content" | "sub_content_tip")
                || value.as_str() != Some(query.as_str())
        });
    }
    kept
}

/// The last `max` bytes of `text`, on a character boundary.
fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &text[start..])
}

fn usage(raw: &Value, totals: &mut TokenUsage) -> Option<SessionEventKind> {
    if raw.get("step_name").and_then(Value::as_str) != Some("call_llm") {
        return None;
    }
    let usage = raw.get("token_usage").filter(|u| u.is_object())?;
    let count = |value: Option<&Value>, key: &str| {
        value
            .and_then(|v| v.get(key))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let input = count(Some(usage), "prompt_tokens");
    let output = count(Some(usage), "completion_tokens");
    let total = match count(Some(usage), "total_tokens") {
        0 => input + output,
        total => total,
    };
    totals.input += input;
    totals.requests = Some(totals.requests.unwrap_or(0) + 1);
    totals.cached_input += count(usage.get("prompt_tokens_details"), "cached_tokens");
    totals.output += output;
    totals.reasoning_output += count(usage.get("completion_tokens_details"), "reasoning_tokens");
    totals.total += total;
    Some(SessionEventKind::Usage {
        used: input + output,
        size: 0,
        cost_usd: None,
        tokens: Some(totals.clone()),
    })
}

/// Applies one `{op, path, value}` to `target`. `add` to a string appends: that is how
/// with-cli streams an argument's text.
fn apply_patch(target: &mut Value, patch: &Value) {
    let Some(op) = str_of(patch, "op") else {
        return;
    };
    let Some(path) = str_of(patch, "path") else {
        return;
    };
    let keys: Vec<String> = path
        .split('/')
        .skip(1)
        .map(|k| k.replace("~1", "/").replace("~0", "~"))
        .collect();
    let value = patch.get("value").cloned().unwrap_or(Value::Null);
    let Some((last, parents)) = keys.split_last() else {
        if op != "remove" {
            *target = value;
        }
        return;
    };
    let mut node = target;
    for key in parents {
        node = child(node, key);
    }
    if let Value::Array(items) = node {
        let index = if last == "-" {
            Some(items.len())
        } else {
            last.parse::<usize>().ok()
        };
        match (op, index) {
            ("remove", Some(i)) if i < items.len() => {
                items.remove(i);
            }
            ("add", Some(i)) if i <= items.len() => items.insert(i, value),
            ("replace", Some(i)) if i < items.len() => items[i] = value,
            _ => {}
        }
        return;
    }
    if !node.is_object() {
        *node = Value::Object(Map::new());
    }
    let map = node.as_object_mut().expect("object");
    match op {
        "remove" => {
            map.remove(last);
        }
        "add" => match (map.get_mut(last), value) {
            (Some(Value::String(existing)), Value::String(more)) => existing.push_str(&more),
            (_, value) => {
                map.insert(last.clone(), value);
            }
        },
        _ => {
            map.insert(last.clone(), value);
        }
    }
}

fn child<'a>(node: &'a mut Value, key: &str) -> &'a mut Value {
    if let (Some(i), true) = (key.parse::<usize>().ok(), node.is_array()) {
        let items = node.as_array_mut().expect("array");
        if i >= items.len() {
            items.resize(i + 1, Value::Null);
        }
        return &mut items[i];
    }
    if !node.is_object() {
        *node = Value::Object(Map::new());
    }
    node.as_object_mut()
        .expect("object")
        .entry(key.to_owned())
        .or_insert_with(|| Value::Object(Map::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(turn: &mut Turn, line: &str) -> Vec<SessionEventKind> {
        match turn.line(line, &mut TokenUsage::default()) {
            Line::Events(events) => events.into_iter().map(|e| e.kind).collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn values_go_with_their_flags() {
        let images = [PathBuf::from(r"C:\a.png")];
        let args = chat_args(&ChatArgs {
            session_id: "s1",
            cwd: Path::new(r"C:\work"),
            prompt: "-not a flag",
            model: Some("claude-haiku-5.5"),
            effort: Some("high"),
            access_mode: Some("full_access"),
            images: &images,
            ..ChatArgs::default()
        });
        assert_eq!(
            args,
            [
                "chat",
                "--no-update",
                "--output-format=stream-json",
                "--sessionId=s1",
                r"--workspace=C:\work",
                "--model=claude-haiku-5.5",
                "--reasoning-effort=high",
                "--access-mode=full_access",
                r"--images=C:\a.png",
                "--prompt=-not a flag",
            ]
        );
    }

    #[test]
    fn model_list_keeps_the_ids() {
        let text = "List available models\n\nUsage:\n  with-cli.exe model list [flags]\n\nFlags:\n  -a, --agentId string   specify\n  claude-haiku-5.5              supports-thinking  context=100K,1M\n  kimi-k2.5\n  tokenhub_deepseek-v4-pro      supports-thinking\n";
        assert_eq!(
            parse_model_list(text),
            ["claude-haiku-5.5", "kimi-k2.5", "tokenhub_deepseek-v4-pro"]
        );
    }

    #[test]
    fn text_and_the_end_of_the_run() {
        let mut turn = Turn::default();
        assert_eq!(
            events(
                &mut turn,
                r#"{"type":"RUN_STARTED","rawEvent":{"message_id":"r1","conversation_id":"s1"}}"#
            ),
            []
        );
        assert_eq!(turn.request_id.as_deref(), Some("r1"));
        assert_eq!(
            events(
                &mut turn,
                r#"{"type":"TEXT_MESSAGE_CONTENT","rawEvent":{"message_id":"r1","content":"hi"}}"#
            ),
            [SessionEventKind::Output {
                stream: OutputStream::Text,
                text: "hi".into()
            }]
        );
        assert_eq!(
            events(
                &mut turn,
                r#"{"type":"THINKING_TEXT_MESSAGE_CONTENT","rawEvent":{"content":"hmm"}}"#
            ),
            [SessionEventKind::Output {
                stream: OutputStream::Thought,
                text: "hmm".into()
            }]
        );
        assert!(!turn.finished);
        events(&mut turn, r#"{"type":"RUN_FINISHED","rawEvent":{}}"#);
        assert!(turn.finished);
        assert_eq!(
            turn.line("requestID: r1", &mut TokenUsage::default()),
            Line::Skip
        );
        assert_eq!(
            turn.line("不支持该模型: x", &mut TokenUsage::default()),
            Line::Text("不支持该模型: x".into())
        );
    }

    #[test]
    fn bookkeeping_calls_are_hidden() {
        let mut turn = Turn::default();
        for line in [
            r#"{"type":"TOOL_CALL_START","rawEvent":{"tool_call_id":"p","name":"task_planning","ch_name":"任务规划中","default_show":false,"need_result":false}}"#,
            r#"{"type":"TOOL_CALL_END","rawEvent":{"tool_call_id":"p"}}"#,
            r#"{"type":"TOOL_CALL_RESULT","rawEvent":{"tool_call_id":"p","display_name":"任务规划完成"}}"#,
            r#"{"type":"CUSTOM","rawEvent":{"type":"remove-tool","tool_call_id":"p"}}"#,
        ] {
            assert_eq!(events(&mut turn, line), []);
        }
    }

    /// As with-cli v0.31.0 printed a read: `default_show: false` on a real tool.
    #[test]
    fn reads_marked_not_shown_are_still_reported() {
        let mut turn = Turn::default();
        assert_eq!(
            events(
                &mut turn,
                r#"{"type":"TOOL_CALL_START","rawEvent":{"tool_call_id":"r","name":"read_file","ch_name":"读取文件","display_name":"读取文件中","render_type":"None","default_show":false,"need_result":true}}"#
            ),
            [SessionEventKind::ToolCall {
                tool_call_id: "r".into(),
                title: "读取文件".into(),
                kind: Some("read".into()),
                status: ToolStatus::InProgress,
            }]
        );
        events(
            &mut turn,
            r#"{"type":"TOOL_CALL_ARGS","rawEvent":{"tool_call_id":"r","document":{"path":"C:\\ws\\a.txt"}}}"#,
        );
        let done = events(
            &mut turn,
            r#"{"type":"TOOL_CALL_RESULT","rawEvent":{"tool_call_id":"r","result":{},"display_name":"读取文件"},"content":"{\"content\": \"hello\"}"}"#,
        );
        assert!(matches!(
            done.last(),
            Some(SessionEventKind::ToolCallUpdated {
                title: Some(title),
                status: Some(ToolStatus::Completed),
                ..
            }) if title == r"C:\ws\a.txt"
        ));
    }

    /// As with-cli printed it for `echo hi` (arguments shortened).
    #[test]
    fn a_command_reports_what_it_ran_and_printed() {
        let mut turn = Turn::default();
        assert_eq!(
            events(
                &mut turn,
                r#"{"type":"TOOL_CALL_START","rawEvent":{"tool_call_id":"t1","name":"terminal","ch_name":"执行命令","default_show":true}}"#
            ),
            [SessionEventKind::ToolCall {
                tool_call_id: "t1".into(),
                title: "执行命令".into(),
                kind: Some("execute".into()),
                status: ToolStatus::InProgress,
            }]
        );
        events(
            &mut turn,
            r#"{"type":"TOOL_CALL_ARGS","rawEvent":{"tool_call_id":"t1","document":{"command":"echo hi","cwd":"C:\\ws","shell_type":"powershell"}}}"#,
        );
        let result = events(
            &mut turn,
            r#"{"type":"TOOL_CALL_RESULT","rawEvent":{"tool_call_id":"t1","result":{"content":{"exit_code":0,"stdout":"hi\r","stderr":"","status":"completed"},"status":"SUCCESS"}}}"#,
        );
        let [
            SessionEventKind::ToolDetail { detail, .. },
            SessionEventKind::ToolCallUpdated { title, status, .. },
        ] = result.as_slice()
        else {
            panic!("{result:?}");
        };
        assert_eq!(detail.command.as_deref(), Some("echo hi"));
        assert_eq!(detail.cwd.as_deref(), Some(r"C:\ws"));
        assert_eq!(detail.exit_code, Some(0));
        assert_eq!(detail.output.as_deref(), Some("hi"));
        assert_eq!(title.as_deref(), Some("echo hi"));
        assert_eq!(*status, Some(ToolStatus::Completed));
    }

    /// The arguments arrive in pieces: `add` to a string appends to it.
    #[test]
    fn streamed_arguments_are_put_together() {
        let mut turn = Turn::default();
        for line in [
            r#"{"type":"TOOL_CALL_START","rawEvent":{"tool_call_id":"w","name":"replace_in_file","default_show":true}}"#,
            r#"{"type":"TOOL_CALL_ARGS","rawEvent":{"tool_call_id":"w","document":{"apply_mode":"apply_only"},"patchs":[{"op":"add","path":"/path","value":"C:\\"}]}}"#,
            r#"{"type":"TOOL_CALL_ARGS","rawEvent":{"tool_call_id":"w","patchs":[{"op":"add","path":"/path","value":"a.txt"}]}}"#,
            r#"{"type":"TOOL_CALL_ARGS","rawEvent":{"tool_call_id":"w","patchs":[{"op":"add","path":"/old_string","value":"hel"},{"op":"add","path":"/new_string","value":"hello"}]}}"#,
            r#"{"type":"TOOL_CALL_ARGS","rawEvent":{"tool_call_id":"w","patchs":[{"op":"add","path":"/old_string","value":"lo"},{"op":"add","path":"/new_string","value":" world"}]}}"#,
        ] {
            events(&mut turn, line);
        }
        let result = events(
            &mut turn,
            r#"{"type":"TOOL_CALL_RESULT","rawEvent":{"tool_call_id":"w","result":{"commit_id":["",""]}},"content":"ok"}"#,
        );
        let SessionEventKind::ToolDetail { detail, .. } = &result[0] else {
            panic!("{result:?}");
        };
        assert_eq!(detail.changes.len(), 1);
        assert_eq!(detail.changes[0].path, r"C:\a.txt");
        assert_eq!(detail.changes[0].kind, "update");
        assert!(detail.changes[0].diff.contains("-hello\n+hello world"));
    }

    /// with-cli's own names say the wrong thing ("查找文件" for a search); a call is titled
    /// by what it does. Only an `other` call keeps with-cli's name.
    #[test]
    fn a_call_is_titled_by_what_it_does() {
        let cases = [
            ("read_file", "读取文件", "读取文件"),
            ("list_dir", "读取文件", "列出目录"),
            ("grep_search", "查找文件", "搜索"),
            ("web_search", "搜索网页", "搜索"),
            ("terminal", "执行命令", "执行命令"),
            ("replace_in_file", "编辑文件", "修改文件"),
            ("delete_file", "删除文件", "删除文件"),
            ("web_fetch", "抓取网页", "获取网页"),
        ];
        for (i, (name, ch_name, title)) in cases.into_iter().enumerate() {
            let mut turn = Turn::default();
            let line = format!(
                r#"{{"type":"TOOL_CALL_START","rawEvent":{{"tool_call_id":"t{i}","name":"{name}","ch_name":"{ch_name}","display_name":"{ch_name}中","default_show":true}}}}"#
            );
            let started = events(&mut turn, &line);
            let [SessionEventKind::ToolCall { title: shown, .. }] = started.as_slice() else {
                panic!("{name}: {started:?}");
            };
            assert_eq!(shown.as_str(), title, "{name}");
        }
        let mut turn = Turn::default();
        assert_eq!(
            events(
                &mut turn,
                r#"{"type":"TOOL_CALL_START","rawEvent":{"tool_call_id":"m","name":"use_mcp_tool","ch_name":"调用 MCP","display_name":"调用中","default_show":true}}"#
            ),
            [SessionEventKind::ToolCall {
                tool_call_id: "m".into(),
                title: "调用 MCP".into(),
                kind: Some("other".into()),
                status: ToolStatus::InProgress,
            }]
        );
    }

    /// with-cli repeats a search's query in `sub_content` and `sub_content_tip`; the argument
    /// panel shows it once. Text that says something new is kept.
    #[test]
    fn a_search_shows_its_query_once() {
        fn arguments(args: &str) -> Value {
            let mut turn = Turn::default();
            events(
                &mut turn,
                r#"{"type":"TOOL_CALL_START","rawEvent":{"tool_call_id":"s","name":"grep_search","default_show":true}}"#,
            );
            events(
                &mut turn,
                &format!(
                    r#"{{"type":"TOOL_CALL_ARGS","rawEvent":{{"tool_call_id":"s","document":{args}}}}}"#
                ),
            );
            let result = events(
                &mut turn,
                r#"{"type":"TOOL_CALL_RESULT","rawEvent":{"tool_call_id":"s","result":{"status":"SUCCESS"}}}"#,
            );
            let SessionEventKind::ToolDetail { detail, .. } = &result[0] else {
                panic!("{result:?}");
            };
            serde_json::from_str(detail.arguments.as_deref().expect("arguments")).expect("json")
        }

        let same = arguments(
            r#"{"query":"relativeTo","sub_content":"relativeTo","sub_content_tip":"relativeTo","search_directory":"apps/gui/src/lib"}"#,
        );
        let mut keys: Vec<&str> = same
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["query", "search_directory"]);
        assert_eq!(same["query"], "relativeTo");

        let different =
            arguments(r#"{"query":"q","sub_content":"other","sub_content_tip":"hint"}"#);
        assert_eq!(different["sub_content"], "other");
        assert_eq!(different["sub_content_tip"], "hint");
    }

    #[test]
    fn failures_and_mcp_results() {
        let mut turn = Turn::default();
        events(
            &mut turn,
            r#"{"type":"TOOL_CALL_START","rawEvent":{"tool_call_id":"m","name":"use_mcp_tool","default_show":true}}"#,
        );
        events(
            &mut turn,
            r#"{"type":"TOOL_CALL_ARGS","rawEvent":{"tool_call_id":"m","document":{"serverName":"probe","toolName":"probe_ping"}}}"#,
        );
        let ok = events(
            &mut turn,
            r#"{"type":"TOOL_CALL_RESULT","rawEvent":{"tool_call_id":"m","result":{"result":"x","approved":true}},"content":"secret=banana77"}"#,
        );
        let SessionEventKind::ToolDetail { detail, .. } = &ok[0] else {
            panic!("{ok:?}");
        };
        assert_eq!(detail.result.as_deref(), Some("secret=banana77"));
        assert!(detail.arguments.as_deref().unwrap().contains("probe_ping"));

        events(
            &mut turn,
            r#"{"type":"TOOL_CALL_START","rawEvent":{"tool_call_id":"f","name":"terminal","default_show":true}}"#,
        );
        let failed = events(
            &mut turn,
            r#"{"type":"TOOL_CALL_RESULT","rawEvent":{"tool_call_id":"f","error":{"type":"text","content":"工具执行失败"}}}"#,
        );
        assert!(matches!(
            failed.last(),
            Some(SessionEventKind::ToolCallUpdated {
                status: Some(ToolStatus::Failed),
                ..
            })
        ));
        let SessionEventKind::ToolDetail { detail, .. } = &failed[0] else {
            panic!("{failed:?}");
        };
        assert_eq!(detail.error.as_deref(), Some("工具执行失败"));
    }

    #[test]
    fn usage_adds_up_over_the_session() {
        let mut totals = TokenUsage::default();
        let line = r#"{"type":"STEP_FINISHED","rawEvent":{"step_name":"call_llm","token_usage":{"completion_tokens":7,"prompt_tokens":100,"total_tokens":107,"completion_tokens_details":{"reasoning_tokens":2},"prompt_tokens_details":{"cached_tokens":80}}}}"#;
        let mut turn = Turn::default();
        turn.line(line, &mut totals);
        let Line::Events(events) = turn.line(line, &mut totals) else {
            panic!();
        };
        assert_eq!(
            events[0].kind,
            SessionEventKind::Usage {
                used: 107,
                size: 0,
                cost_usd: None,
                tokens: Some(TokenUsage {
                    input: 200,
                    cached_input: 160,
                    output: 14,
                    reasoning_output: 4,
                    total: 214,
                    requests: Some(2),
                    ..TokenUsage::default()
                }),
            }
        );
    }

    #[test]
    fn run_errors_are_reported() {
        let mut turn = Turn::default();
        // AG-UI puts the reason at the top level of the event, beside `type`.
        assert_eq!(
            events(
                &mut turn,
                r#"{"type":"RUN_ERROR","message":"quota exceeded"}"#
            ),
            [SessionEventKind::Error {
                message: "quota exceeded".into()
            }]
        );
        assert_eq!(turn.error.as_deref(), Some("quota exceeded"));
        // A code is kept too, ahead of the message.
        assert_eq!(
            events(
                &mut turn,
                r#"{"type":"RUN_ERROR","code":"rate_limit","message":"quota exceeded"}"#
            ),
            [SessionEventKind::Error {
                message: "rate_limit: quota exceeded".into()
            }]
        );
        // Some events only say it inside rawEvent.
        assert_eq!(
            events(
                &mut turn,
                r#"{"type":"RUN_ERROR","rawEvent":{"message":"quota exceeded"}}"#
            ),
            [SessionEventKind::Error {
                message: "quota exceeded".into()
            }]
        );
        assert_eq!(turn.error.as_deref(), Some("quota exceeded"));
        // Saying nothing at all still shows what arrived, not a fixed line.
        let bare = r#"{"type":"RUN_ERROR","rawEvent":{"unknown":7}}"#;
        let error = events(&mut turn, bare);
        let [SessionEventKind::Error { message }] = error.as_slice() else {
            panic!("{error:?}");
        };
        assert!(message.contains(r#""unknown":7"#), "{message}");
        assert!(message.contains("RUN_ERROR"), "{message}");
        assert_ne!(message, "with-cli reported an error");
    }

    /// A long event is kept from its head, cut on a character boundary.
    #[test]
    fn run_error_json_is_truncated() {
        let mut turn = Turn::default();
        let filler = "错".repeat(2_000);
        let line = format!(r#"{{"type":"RUN_ERROR","rawEvent":{{"blob":"{filler}"}}}}"#);
        events(&mut turn, &line);
        let error = turn.error.expect("an error");
        let kept = error
            .strip_prefix("with-cli reported an error: ")
            .and_then(|text| text.strip_suffix('…'))
            .expect("the raw JSON, cut");
        let json = serde_json::from_str::<Value>(&line).unwrap().to_string();
        assert!(json.starts_with(kept), "{kept}");
        assert!(
            kept.len() <= ERROR_JSON && kept.len() > ERROR_JSON - 4,
            "{}",
            kept.len()
        );
    }
}
