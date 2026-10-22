//! The part of the `codex app-server` protocol CrossAgents speaks, and its translation into
//! the hub's events. Verified against codex-cli 0.158.0, see
//! docs/research/codex-app-server.md. Everything here is JSON in, JSON or events out, so
//! unknown fields and notifications are simply ignored.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use crossagents_core::{
    Credits, FileChange, LimitGroup, McpServerSpec, OutputStream, PermissionDecision,
    PermissionRequest, RateLimits, RateWindow, ResetCredit, SessionEventKind, StopReason,
    TokenUsage, ToolDetail, ToolStatus,
};
use serde_json::{Map, Value, json};

/// The upstream version this module was checked against.
pub const VERIFIED_VERSION: &str = "0.158.0";

pub mod method {
    pub const INITIALIZE: &str = "initialize";
    pub const INITIALIZED: &str = "initialized";
    pub const THREAD_START: &str = "thread/start";
    pub const THREAD_RESUME: &str = "thread/resume";
    pub const THREAD_FORK: &str = "thread/fork";
    pub const THREAD_REVERT: &str = "thread/revert";
    pub const THREAD_UNSUBSCRIBE: &str = "thread/unsubscribe";
    pub const THREAD_COMPACT: &str = "thread/compact/start";
    pub const TURN_START: &str = "turn/start";
    pub const TURN_INTERRUPT: &str = "turn/interrupt";

    pub const TURN_STARTED: &str = "turn/started";
    pub const TURN_COMPLETED: &str = "turn/completed";
    pub const TURN_DIFF: &str = "turn/diff/updated";
    pub const ITEM_STARTED: &str = "item/started";
    pub const ITEM_COMPLETED: &str = "item/completed";
    pub const AGENT_MESSAGE_DELTA: &str = "item/agentMessage/delta";
    pub const REASONING_SUMMARY_DELTA: &str = "item/reasoning/summaryTextDelta";
    pub const REASONING_TEXT_DELTA: &str = "item/reasoning/textDelta";
    pub const TOKEN_USAGE: &str = "thread/tokenUsage/updated";
    pub const RATE_LIMITS: &str = "account/rateLimits/updated";
    pub const RATE_LIMITS_READ: &str = "account/rateLimits/read";
    pub const COMMAND_OUTPUT_DELTA: &str = "item/commandExecution/outputDelta";
    pub const ERROR: &str = "error";
    pub const WARNING: &str = "warning";

    pub const COMMAND_APPROVAL: &str = "item/commandExecution/requestApproval";
    pub const FILE_CHANGE_APPROVAL: &str = "item/fileChange/requestApproval";
    pub const PERMISSIONS_APPROVAL: &str = "item/permissions/requestApproval";
    pub const MCP_ELICITATION: &str = "mcpServer/elicitation/request";
    pub const LEGACY_PATCH_APPROVAL: &str = "applyPatchApproval";
    pub const LEGACY_EXEC_APPROVAL: &str = "execCommandApproval";
}

/// MCP servers CrossAgents mounts are trusted: under `approvalPolicy = "never"` Codex
/// rejects every MCP tool call unless the server says so.
const MCP_APPROVAL_MODE: &str = "approve";

pub fn initialize_params() -> Value {
    json!({
        // Becomes the rollout's `originator`, which M5 uses to attribute usage.
        "clientInfo": { "name": "crossagents", "title": "CrossAgents", "version": env!("CARGO_PKG_VERSION") },
        "capabilities": { "experimentalApi": false, "requestAttestation": false },
    })
}

/// Settings a thread needs on every start, resume and fork: Codex does not carry them
/// over (a fork falls back to config.toml defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadSettings {
    pub model: Option<String>,
    pub cwd: String,
    /// `never`, `on-request`, `untrusted`.
    pub approval_policy: String,
    /// `read-only`, `workspace-write`, `danger-full-access`.
    pub sandbox: String,
    pub mcp_servers: Vec<McpServerSpec>,
}

impl ThreadSettings {
    fn params(&self) -> Map<String, Value> {
        let mut params = Map::new();
        if let Some(model) = &self.model {
            params.insert("model".into(), model.clone().into());
        }
        params.insert("cwd".into(), self.cwd.clone().into());
        params.insert("approvalPolicy".into(), self.approval_policy.clone().into());
        params.insert("sandbox".into(), self.sandbox.clone().into());
        // The person's ChatGPT apps (Linear, GitHub, …) stay out of hub agents: their tool
        // list is tens of thousands of tokens and buries the hub's own tools.
        let mut config = json!({ "features": { "apps": false } });
        if !self.mcp_servers.is_empty() {
            config["mcp_servers"] = mcp_config(&self.mcp_servers);
        }
        params.insert("config".into(), config);
        params
    }
}

/// `config.mcp_servers` for a thread: the same shape as `[mcp_servers.<name>]` in
/// config.toml.
pub fn mcp_config(servers: &[McpServerSpec]) -> Value {
    let map: Map<String, Value> = servers
        .iter()
        .map(|s| {
            (
                s.name.clone(),
                json!({
                    "command": path_str(&s.command),
                    "args": s.args,
                    "env": s.env,
                    "default_tools_approval_mode": MCP_APPROVAL_MODE,
                }),
            )
        })
        .collect();
    Value::Object(map)
}

fn path_str(path: &Path) -> String {
    path.display().to_string()
}

pub fn thread_start(settings: &ThreadSettings, instructions: Option<&str>) -> Value {
    let mut params = settings.params();
    if let Some(text) = instructions {
        params.insert("developerInstructions".into(), text.into());
    }
    Value::Object(params)
}

pub fn thread_resume(thread_id: &str, settings: &ThreadSettings) -> Value {
    let mut params = settings.params();
    params.insert("threadId".into(), thread_id.into());
    params.insert("excludeTurns".into(), true.into());
    Value::Object(params)
}

pub fn thread_fork(
    thread_id: &str,
    last_turn_id: Option<&str>,
    settings: &ThreadSettings,
    instructions: Option<&str>,
) -> Value {
    let mut params = settings.params();
    params.insert("threadId".into(), thread_id.into());
    if let Some(turn) = last_turn_id {
        params.insert("lastTurnId".into(), turn.into());
    }
    if let Some(text) = instructions {
        params.insert("developerInstructions".into(), text.into());
    }
    params.insert("excludeTurns".into(), true.into());
    Value::Object(params)
}

pub fn turn_start(thread_id: &str, text: &str, model: Option<&str>, effort: Option<&str>) -> Value {
    let mut params = Map::new();
    params.insert("threadId".into(), thread_id.into());
    params.insert(
        "input".into(),
        json!([{ "type": "text", "text": text, "text_elements": [] }]),
    );
    if let Some(model) = model {
        params.insert("model".into(), model.into());
    }
    if let Some(effort) = effort {
        params.insert("effort".into(), effort.into());
    }
    Value::Object(params)
}

/// The thread id in a start / resume / fork response.
pub fn thread_id_of(response: &Value) -> Option<&str> {
    response.pointer("/thread/id")?.as_str()
}

/// The thread a server message is about. Legacy approvals call it `conversationId`.
pub fn thread_of(params: &Value) -> Option<&str> {
    params
        .get("threadId")
        .or_else(|| params.get("conversationId"))
        .and_then(Value::as_str)
}

pub fn stop_reason(turn_status: &str) -> StopReason {
    match turn_status {
        "interrupted" => StopReason::Cancelled,
        "failed" => StopReason::Error,
        _ => StopReason::EndTurn,
    }
}

/// A finished turn, from `turn/completed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEnd {
    pub turn_id: String,
    pub status: String,
}

pub fn turn_end(params: &Value) -> Option<TurnEnd> {
    let turn = params.get("turn")?;
    Some(TurnEnd {
        turn_id: turn.get("id")?.as_str()?.to_owned(),
        status: turn
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("completed")
            .to_owned(),
    })
}

pub fn turn_started_id(params: &Value) -> Option<&str> {
    params.pointer("/turn/id")?.as_str()
}

/// Translates one notification into hub events (none for the ones the hub ignores).
pub fn translate(method: &str, params: &Value) -> Vec<SessionEventKind> {
    let str_at = |pointer: &str| params.pointer(pointer).and_then(Value::as_str);
    let turn_id = || str_at("/turnId").map(str::to_owned);
    let event = match method {
        method::AGENT_MESSAGE_DELTA => SessionEventKind::Output {
            stream: OutputStream::Text,
            text: str_at("/delta").unwrap_or_default().to_owned(),
        },
        method::REASONING_SUMMARY_DELTA | method::REASONING_TEXT_DELTA => {
            SessionEventKind::Output {
                stream: OutputStream::Thought,
                text: str_at("/delta").unwrap_or_default().to_owned(),
            }
        }
        method::TURN_STARTED => SessionEventKind::TurnStarted {
            turn_id: turn_started_id(params).map(str::to_owned),
        },
        method::TURN_COMPLETED => {
            let Some(end) = turn_end(params) else {
                return Vec::new();
            };
            SessionEventKind::TurnCompleted {
                stop_reason: stop_reason(&end.status),
                turn_id: Some(end.turn_id),
            }
        }
        method::TURN_DIFF => SessionEventKind::TurnDiff {
            turn_id: turn_id(),
            diff: str_at("/diff").unwrap_or_default().to_owned(),
        },
        method::COMMAND_OUTPUT_DELTA => {
            let (Some(id), Some(text)) = (str_at("/itemId"), str_at("/delta")) else {
                return Vec::new();
            };
            SessionEventKind::ToolOutput {
                tool_call_id: id.to_owned(),
                text: text.to_owned(),
            }
        }
        method::ITEM_STARTED => {
            let Some(item) = params.get("item") else {
                return Vec::new();
            };
            let Some((id, title, kind)) = tool_call(item) else {
                return Vec::new();
            };
            let mut events = vec![SessionEventKind::ToolCall {
                tool_call_id: id.clone(),
                title,
                kind: Some(kind.to_owned()),
                status: item_status(item).unwrap_or(ToolStatus::InProgress),
            }];
            // File changes carry their diffs before they are applied.
            if let Some(detail) = tool_detail(item).filter(|d| !d.changes.is_empty()) {
                events.push(SessionEventKind::ToolDetail {
                    tool_call_id: id,
                    detail,
                });
            }
            return events;
        }
        method::ITEM_COMPLETED => {
            let Some(item) = params.get("item") else {
                return Vec::new();
            };
            let Some((id, _, _)) = tool_call(item) else {
                return Vec::new();
            };
            let mut events = Vec::new();
            if let Some(detail) = tool_detail(item) {
                events.push(SessionEventKind::ToolDetail {
                    tool_call_id: id.clone(),
                    detail,
                });
            }
            events.push(SessionEventKind::ToolCallUpdated {
                tool_call_id: id,
                title: None,
                status: Some(item_status(item).unwrap_or(ToolStatus::Completed)),
            });
            return events;
        }
        method::TOKEN_USAGE => {
            let usage = params.get("tokenUsage").unwrap_or(&Value::Null);
            let num = |v: &Value, key: &str| v.get(key).and_then(Value::as_u64).unwrap_or(0);
            let last = usage.get("last").unwrap_or(&Value::Null);
            let total = usage.get("total").unwrap_or(&Value::Null);
            SessionEventKind::Usage {
                used: num(last, "totalTokens"),
                size: usage
                    .get("modelContextWindow")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                cost_usd: None,
                tokens: Some(TokenUsage {
                    input: num(total, "inputTokens"),
                    cached_input: num(total, "cachedInputTokens"),
                    output: num(total, "outputTokens"),
                    reasoning_output: num(total, "reasoningOutputTokens"),
                    total: num(total, "totalTokens"),
                }),
            }
        }
        method::RATE_LIMITS => SessionEventKind::RateLimits {
            limits: rate_limits(params.get("rateLimits").unwrap_or(&Value::Null)),
        },
        method::ERROR => {
            let message = error_message(params.get("error").unwrap_or(&Value::Null));
            if params.get("willRetry").and_then(Value::as_bool) == Some(true) {
                SessionEventKind::Warning {
                    message: format!("{message} (retrying)"),
                }
            } else {
                SessionEventKind::Error { message }
            }
        }
        method::WARNING => SessionEventKind::Warning {
            message: str_at("/message").unwrap_or_default().to_owned(),
        },
        _ => return Vec::new(),
    };
    vec![event]
}

/// `(id, title, kind)` for items that are tool calls.
fn tool_call(item: &Value) -> Option<(String, String, &'static str)> {
    let id = item.get("id")?.as_str()?.to_owned();
    let str_of = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or_default();
    let (title, kind) = match item.get("type")?.as_str()? {
        // `commandActions` holds what the model asked for, without the shell wrapper
        // (`powershell.exe -Command '...'`) that `command` carries.
        "commandExecution" => {
            let actions: Vec<&str> = item
                .get("commandActions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|a| a.get("command")?.as_str())
                .collect();
            let title = if actions.is_empty() {
                str_of("command").to_owned()
            } else {
                actions.join(" && ")
            };
            (title, "execute")
        }
        "fileChange" => {
            let paths: Vec<&str> = item
                .get("changes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|c| c.get("path")?.as_str())
                .collect();
            (format!("Edit {}", paths.join(", ")), "edit")
        }
        "mcpToolCall" => (format!("{}.{}", str_of("server"), str_of("tool")), "mcp"),
        "dynamicToolCall" => (str_of("tool").to_owned(), "tool"),
        "collabAgentToolCall" => (str_of("tool").to_owned(), "agent"),
        "webSearch" => (format!("Web search {}", str_of("query")), "search"),
        "imageView" => (format!("View {}", str_of("path")), "read"),
        "imageGeneration" => ("Generate image".to_owned(), "other"),
        _ => return None,
    };
    Some((id, title.trim().to_owned(), kind))
}

/// What an item did, for the item types that say more than their title.
fn tool_detail(item: &Value) -> Option<ToolDetail> {
    let text = |key: &str| item.get(key).and_then(Value::as_str).map(str::to_owned);
    let json_text = |key: &str| {
        item.get(key)
            .filter(|v| !v.is_null())
            .map(|v| match v.as_str() {
                Some(s) => s.to_owned(),
                None => serde_json::to_string_pretty(v).unwrap_or_default(),
            })
    };
    let detail = match item.get("type")?.as_str()? {
        "commandExecution" => ToolDetail {
            command: text("command"),
            cwd: text("cwd"),
            exit_code: item
                .get("exitCode")
                .and_then(Value::as_i64)
                .map(|c| c as i32),
            duration_ms: item.get("durationMs").and_then(Value::as_u64),
            output: text("aggregatedOutput"),
            ..ToolDetail::default()
        },
        "fileChange" => ToolDetail {
            changes: item
                .get("changes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|c| {
                    Some(FileChange {
                        path: c.get("path")?.as_str()?.to_owned(),
                        kind: change_kind(c.get("kind")),
                        diff: c
                            .get("diff")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    })
                })
                .collect(),
            ..ToolDetail::default()
        },
        "mcpToolCall" | "dynamicToolCall" | "collabAgentToolCall" => ToolDetail {
            arguments: json_text("arguments"),
            result: json_text("result"),
            error: item
                .get("error")
                .and_then(|e| e.get("message").or(Some(e)))
                .and_then(Value::as_str)
                .map(str::to_owned),
            duration_ms: item.get("durationMs").and_then(Value::as_u64),
            ..ToolDetail::default()
        },
        _ => return None,
    };
    (detail != ToolDetail::default()).then_some(detail)
}

/// `{"type": "update", ...}` or a plain string -> `update`.
fn change_kind(kind: Option<&Value>) -> String {
    match kind {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("update")
            .to_owned(),
        None => "update".to_owned(),
    }
}

fn item_status(item: &Value) -> Option<ToolStatus> {
    match item.get("status")?.as_str()? {
        "inProgress" => Some(ToolStatus::InProgress),
        "completed" => Some(ToolStatus::Completed),
        "failed" | "declined" => Some(ToolStatus::Failed),
        _ => None,
    }
}

/// The readable message of a `TurnError`. Upstream errors often arrive as a JSON string
/// with the real message nested inside.
pub fn error_message(error: &Value) -> String {
    let raw = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| raw.to_owned())
}

fn windows_of(snapshot: &Value) -> Vec<RateWindow> {
    ["primary", "secondary"]
        .into_iter()
        .filter_map(|name| {
            let w = snapshot.get(name).filter(|w| w.is_object())?;
            Some(RateWindow {
                name: name.into(),
                used_percent: w.get("usedPercent").and_then(Value::as_f64).unwrap_or(0.0),
                window_seconds: w
                    .get("windowDurationMins")
                    .and_then(Value::as_u64)
                    .map(|m| m * 60),
                resets_at: w
                    .get("resetsAt")
                    .and_then(Value::as_i64)
                    .and_then(|s| DateTime::<Utc>::from_timestamp(s, 0)),
            })
        })
        .collect()
}

/// One limit of a snapshot: its id, name, model and windows.
fn limit_group(snapshot: &Value) -> LimitGroup {
    let text = |key: &str| snapshot.get(key).and_then(Value::as_str).map(str::to_owned);
    LimitGroup {
        id: text("limitId").unwrap_or_else(|| "default".into()),
        name: text("limitName"),
        model: text("normalModelSlug"),
        windows: windows_of(snapshot),
        reached: text("rateLimitReachedType").is_some()
            || snapshot.get("spendControlReached").and_then(Value::as_bool) == Some(true),
    }
}

/// The per-call quota update (`account/rateLimits/updated`): the limit in use.
fn rate_limits(snapshot: &Value) -> RateLimits {
    let windows = windows_of(snapshot);
    let credits = snapshot
        .get("credits")
        .filter(|c| c.is_object())
        .map(|c| Credits {
            has_credits: c
                .get("hasCredits")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            unlimited: c.get("unlimited").and_then(Value::as_bool).unwrap_or(false),
            balance: c.get("balance").and_then(Value::as_str).map(str::to_owned),
        });
    let text = |key: &str| snapshot.get(key).and_then(Value::as_str).map(str::to_owned);
    RateLimits {
        windows,
        credits,
        plan: text("planType"),
        reached: text("rateLimitReachedType"),
        by_limit: vec![limit_group(snapshot)],
        reset_credits: None,
    }
}

/// A full read (`account/rateLimits/read`): every limit, and the reset credits.
pub fn rate_limits_read(result: &Value) -> Option<RateLimits> {
    let main = result.get("rateLimits").filter(|v| v.is_object())?;
    let mut limits = rate_limits(main);
    if let Some(by_id) = result.get("rateLimitsByLimitId").and_then(Value::as_object) {
        limits.by_limit = by_id.values().map(limit_group).collect();
        // The main limit first, the rest by id.
        let main_id = main
            .get("limitId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        limits
            .by_limit
            .sort_by_key(|g| (g.id != main_id, g.id.clone()));
    }
    let credits = result
        .pointer("/rateLimitResetCredits/credits")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|c| {
                    Some(ResetCredit {
                        id: c.get("id")?.as_str()?.to_owned(),
                        title: c
                            .get("title")
                            .and_then(Value::as_str)
                            .unwrap_or("reset")
                            .to_owned(),
                        description: c
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        status: c
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("available")
                            .to_owned(),
                        expires_at: c
                            .get("expiresAt")
                            .and_then(Value::as_i64)
                            .and_then(|s| DateTime::<Utc>::from_timestamp(s, 0)),
                    })
                })
                .collect()
        });
    limits.reset_credits = Some(credits.unwrap_or_default());
    Some(limits)
}

/// The request as the hub sees it, or `None` for requests that are not permission asks.
pub fn permission_request(method: &str, params: &Value) -> Option<PermissionRequest> {
    let str_of = |key: &str| params.get(key).and_then(Value::as_str);
    let (kind, title) = match method {
        method::COMMAND_APPROVAL | method::LEGACY_EXEC_APPROVAL => {
            let command = match params.get("command") {
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
                Some(Value::String(command)) => command.clone(),
                _ => str_of("reason").unwrap_or("run a command").to_owned(),
            };
            ("command", command)
        }
        method::FILE_CHANGE_APPROVAL | method::LEGACY_PATCH_APPROVAL => (
            "file_change",
            str_of("reason").unwrap_or("apply file changes").to_owned(),
        ),
        method::PERMISSIONS_APPROVAL => (
            "permissions",
            str_of("reason")
                .unwrap_or("grant additional permissions")
                .to_owned(),
        ),
        method::MCP_ELICITATION => (
            "mcp_elicitation",
            str_of("message").unwrap_or("MCP server input").to_owned(),
        ),
        _ => return None,
    };
    Some(PermissionRequest {
        id: ["approvalId", "itemId", "callId", "elicitationId"]
            .into_iter()
            .find_map(str_of)
            .unwrap_or_default()
            .to_owned(),
        turn_id: str_of("turnId").map(str::to_owned),
        kind: kind.to_owned(),
        title,
    })
}

/// The reply to a permission request, and the decision actually given (an MCP form cannot
/// be filled in on the hub's behalf, so elicitations are always declined).
pub fn permission_response(
    method: &str,
    params: &Value,
    decision: PermissionDecision,
) -> (Value, PermissionDecision) {
    use PermissionDecision::*;
    match method {
        method::LEGACY_EXEC_APPROVAL | method::LEGACY_PATCH_APPROVAL => {
            let answer = match decision {
                Allow => json!("approved"),
                AllowForSession => json!("approved_for_session"),
                Deny => json!({ "denied": { "rejection": "declined by CrossAgents" } }),
                Cancel => json!("abort"),
            };
            (json!({ "decision": answer }), decision)
        }
        method::PERMISSIONS_APPROVAL => {
            let (permissions, scope) = match decision {
                Allow | AllowForSession => {
                    let requested = params.get("permissions").cloned().unwrap_or(json!({}));
                    let granted: Map<String, Value> = requested
                        .as_object()
                        .into_iter()
                        .flatten()
                        .filter(|(_, v)| !v.is_null())
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    let scope = if decision == AllowForSession {
                        "session"
                    } else {
                        "turn"
                    };
                    (Value::Object(granted), scope)
                }
                Deny | Cancel => (json!({}), "turn"),
            };
            (
                json!({ "permissions": permissions, "scope": scope }),
                decision,
            )
        }
        method::MCP_ELICITATION => (
            json!({ "action": "decline", "content": null, "_meta": null }),
            Deny,
        ),
        _ => {
            let answer = match decision {
                Allow => "accept",
                AllowForSession => "acceptForSession",
                Deny => "decline",
                Cancel => "cancel",
            };
            (json!({ "decision": answer }), decision)
        }
    }
}

/// Sandbox and approval policy from the session's `config` (`sandbox`, `approval`) or, by
/// default, from its permission policy: bypass runs unrestricted (isolation comes from
/// the worktree, docs/DESIGN.md §4.4), deny keeps Codex's workspace sandbox and asks.
pub fn policies(
    config: &Value,
    permission: crossagents_core::PermissionPolicy,
) -> (String, String) {
    let pick = |key: &str| config.get(key).and_then(Value::as_str).map(str::to_owned);
    let (sandbox, approval) = match permission {
        crossagents_core::PermissionPolicy::Bypass => ("danger-full-access", "never"),
        crossagents_core::PermissionPolicy::Deny => ("workspace-write", "on-request"),
    };
    (
        pick("sandbox").unwrap_or_else(|| sandbox.into()),
        pick("approval").unwrap_or_else(|| approval.into()),
    )
}

/// Environment variables as a sorted list (process identity).
pub fn sorted_env(env: &[(String, String)]) -> Vec<(String, String)> {
    let map: BTreeMap<_, _> = env.iter().cloned().collect();
    map.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_params_carry_settings_and_trusted_mcp() {
        let settings = ThreadSettings {
            model: Some("gpt-6-luna".into()),
            cwd: "/w/a".into(),
            approval_policy: "never".into(),
            sandbox: "danger-full-access".into(),
            mcp_servers: vec![McpServerSpec {
                name: "crossagents".into(),
                command: "/bin/crossagents".into(),
                args: vec!["mcp".into()],
                env: BTreeMap::from([("CROSSAGENTS_AGENT_TOKEN".into(), "t".into())]),
            }],
        };
        let start = thread_start(&settings, Some("You are w1."));
        assert_eq!(start["model"], "gpt-6-luna");
        assert_eq!(start["developerInstructions"], "You are w1.");
        let server = &start["config"]["mcp_servers"]["crossagents"];
        assert_eq!(server["command"], "/bin/crossagents");
        assert_eq!(server["env"]["CROSSAGENTS_AGENT_TOKEN"], "t");
        assert_eq!(server["default_tools_approval_mode"], "approve");
        assert_eq!(start["config"]["features"]["apps"], false);

        let fork = thread_fork("t1", Some("turn-1"), &settings, None);
        assert_eq!(fork["lastTurnId"], "turn-1");
        assert_eq!(fork["sandbox"], "danger-full-access");
        assert!(fork["config"]["mcp_servers"]["crossagents"].is_object());
        assert!(fork.get("developerInstructions").is_none());

        let resume = thread_resume(
            "t1",
            &ThreadSettings {
                model: None,
                ..settings
            },
        );
        assert_eq!(resume["threadId"], "t1");
        assert!(resume.get("model").is_none());
    }

    #[test]
    fn translates_a_turn() {
        let delta = json!({ "threadId": "t", "turnId": "u", "itemId": "m", "delta": "PO" });
        assert_eq!(
            translate(method::AGENT_MESSAGE_DELTA, &delta),
            [SessionEventKind::Output {
                stream: OutputStream::Text,
                text: "PO".into()
            }]
        );
        let started = json!({ "threadId": "t", "turn": { "id": "u", "status": "inProgress" } });
        assert_eq!(
            translate(method::TURN_STARTED, &started),
            [SessionEventKind::TurnStarted {
                turn_id: Some("u".into())
            }]
        );
        let done = json!({ "threadId": "t", "turn": { "id": "u", "status": "interrupted" } });
        assert_eq!(
            translate(method::TURN_COMPLETED, &done),
            [SessionEventKind::TurnCompleted {
                turn_id: Some("u".into()),
                stop_reason: StopReason::Cancelled
            }]
        );
        let usage = json!({ "threadId": "t", "turnId": "u", "tokenUsage": {
            "total": { "totalTokens": 45269, "inputTokens": 45203, "cachedInputTokens": 29696, "cacheWriteInputTokens": 0, "outputTokens": 66, "reasoningOutputTokens": 0 },
            "last": { "totalTokens": 15167, "inputTokens": 15161, "cachedInputTokens": 14848, "cacheWriteInputTokens": 0, "outputTokens": 6, "reasoningOutputTokens": 0 },
            "modelContextWindow": 258400 } });
        let [
            SessionEventKind::Usage {
                used, size, tokens, ..
            },
        ] = &translate(method::TOKEN_USAGE, &usage)[..]
        else {
            panic!("no usage")
        };
        assert_eq!((*used, *size), (15167, 258400));
        assert_eq!(tokens.as_ref().unwrap().cached_input, 29696);
    }

    #[test]
    fn translates_tool_items_and_diffs() {
        let cmd = json!({ "threadId": "t", "turnId": "u", "item": {
            "type": "commandExecution", "id": "exec-1", "command": "ls", "status": "inProgress" } });
        assert_eq!(
            translate(method::ITEM_STARTED, &cmd),
            [SessionEventKind::ToolCall {
                tool_call_id: "exec-1".into(),
                title: "ls".into(),
                kind: Some("execute".into()),
                status: ToolStatus::InProgress
            }]
        );
        let edit = json!({ "threadId": "t", "turnId": "u", "item": {
            "type": "fileChange", "id": "exec-2", "status": "declined",
            "changes": [{ "path": "/w/a.txt", "kind": { "type": "add" }, "diff": "x\n" }] } });
        let change = FileChange {
            path: "/w/a.txt".into(),
            kind: "add".into(),
            diff: "x
"
            .into(),
        };
        assert_eq!(
            translate(method::ITEM_COMPLETED, &edit),
            [
                SessionEventKind::ToolDetail {
                    tool_call_id: "exec-2".into(),
                    detail: ToolDetail {
                        changes: vec![change],
                        ..ToolDetail::default()
                    }
                },
                SessionEventKind::ToolCallUpdated {
                    tool_call_id: "exec-2".into(),
                    title: None,
                    status: Some(ToolStatus::Failed)
                }
            ]
        );
        let message = json!({ "threadId": "t", "turnId": "u", "item": { "type": "agentMessage", "id": "m", "text": "hi" } });
        assert!(translate(method::ITEM_COMPLETED, &message).is_empty());
        let diff = json!({ "threadId": "t", "turnId": "u", "diff": "diff --git a/x b/x\n" });
        assert_eq!(
            translate(method::TURN_DIFF, &diff),
            [SessionEventKind::TurnDiff {
                turn_id: Some("u".into()),
                diff: "diff --git a/x b/x\n".into()
            }]
        );
    }

    #[test]
    fn translates_command_output_and_result() {
        // As codex-cli 0.163 sends them.
        let delta = json!({ "threadId": "t", "turnId": "u", "itemId": "exec-9", "delta": "On branch master
" });
        assert_eq!(
            translate(method::COMMAND_OUTPUT_DELTA, &delta),
            [SessionEventKind::ToolOutput {
                tool_call_id: "exec-9".into(),
                text: "On branch master
"
                .into()
            }]
        );
        let done = json!({ "threadId": "t", "turnId": "u", "item": {
            "type": "commandExecution", "id": "exec-9", "command": "git status", "cwd": "/w",
            "status": "completed", "aggregatedOutput": "On branch master
", "exitCode": 0, "durationMs": 25 } });
        let events = translate(method::ITEM_COMPLETED, &done);
        let SessionEventKind::ToolDetail { detail, .. } = &events[0] else {
            panic!("no detail: {events:?}")
        };
        assert_eq!(detail.command.as_deref(), Some("git status"));
        assert_eq!(detail.exit_code, Some(0));
        assert_eq!(detail.duration_ms, Some(25));
        assert_eq!(
            detail.output.as_deref(),
            Some(
                "On branch master
"
            )
        );
        let mcp = json!({ "threadId": "t", "turnId": "u", "item": {
            "type": "mcpToolCall", "id": "m1", "server": "crossagents", "tool": "agent_list", "status": "completed",
            "arguments": { "x": 1 }, "result": { "content": [] } } });
        let events = translate(method::ITEM_COMPLETED, &mcp);
        let SessionEventKind::ToolDetail { detail, .. } = &events[0] else {
            panic!("no detail: {events:?}")
        };
        assert!(detail.arguments.as_deref().unwrap().contains("\"x\": 1"));
        assert!(detail.result.is_some());
    }

    #[test]
    fn translates_limits_and_errors() {
        let limits = json!({ "rateLimits": { "limitId": "codex", "primary": { "usedPercent": 5, "windowDurationMins": 43200, "resetsAt": 1795131666 },
            "secondary": null, "credits": { "hasCredits": true, "unlimited": false, "balance": "53035.29" }, "planType": "free", "rateLimitReachedType": null } });
        let [SessionEventKind::RateLimits { limits }] =
            &translate(method::RATE_LIMITS, &limits)[..]
        else {
            panic!("no limits")
        };
        assert_eq!(limits.windows.len(), 1);
        assert_eq!(limits.windows[0].window_seconds, Some(43200 * 60));
        assert!(limits.windows[0].resets_at.is_some());
        assert_eq!(limits.plan.as_deref(), Some("free"));
        assert_eq!(
            limits.credits.as_ref().unwrap().balance.as_deref(),
            Some("53035.29")
        );

        let error = json!({ "threadId": "t", "turnId": "u", "willRetry": false, "error": {
            "message": "{\"type\":\"error\",\"status\":400,\"error\":{\"type\":\"invalid_request_error\",\"message\":\"The 'gpt-6-astra' model is not supported when using Codex with a ChatGPT account.\"}}" } });
        assert_eq!(
            translate(method::ERROR, &error),
            [SessionEventKind::Error {
                message: "The 'gpt-6-astra' model is not supported when using Codex with a ChatGPT account.".into()
            }]
        );
        let retry = json!({ "threadId": "t", "willRetry": true, "error": { "message": "stream disconnected" } });
        assert!(matches!(
            &translate(method::ERROR, &retry)[..],
            [SessionEventKind::Warning { .. }]
        ));
    }

    #[test]
    fn permission_requests_and_answers() {
        let params = json!({ "kind": "command", "threadId": "t", "turnId": "u", "itemId": "exec-8",
            "command": "powershell -Command 'echo probe > made.txt'", "cwd": "/w",
            "availableDecisions": ["accept", "cancel"] });
        let request = permission_request(method::COMMAND_APPROVAL, &params).unwrap();
        assert_eq!(request.kind, "command");
        assert_eq!(request.id, "exec-8");
        assert_eq!(request.turn_id.as_deref(), Some("u"));
        assert!(request.title.contains("made.txt"));
        for (decision, answer) in [
            (PermissionDecision::Allow, "accept"),
            (PermissionDecision::AllowForSession, "acceptForSession"),
            (PermissionDecision::Deny, "decline"),
            (PermissionDecision::Cancel, "cancel"),
        ] {
            let (value, given) = permission_response(method::COMMAND_APPROVAL, &params, decision);
            assert_eq!(value["decision"], answer);
            assert_eq!(given, decision);
        }

        let perms = json!({ "threadId": "t", "turnId": "u", "itemId": "p", "cwd": "/w", "reason": "network",
            "permissions": { "network": { "enabled": true }, "fileSystem": null } });
        let (value, _) = permission_response(
            method::PERMISSIONS_APPROVAL,
            &perms,
            PermissionDecision::AllowForSession,
        );
        assert_eq!(value["scope"], "session");
        assert_eq!(
            value["permissions"],
            json!({ "network": { "enabled": true } })
        );

        let (value, given) = permission_response(
            method::MCP_ELICITATION,
            &json!({}),
            PermissionDecision::Allow,
        );
        assert_eq!(value["action"], "decline");
        assert_eq!(given, PermissionDecision::Deny);
        assert!(permission_request("item/tool/call", &json!({})).is_none());
    }

    #[test]
    fn policies_follow_permission_and_config() {
        use crossagents_core::PermissionPolicy;
        assert_eq!(
            policies(&json!({}), PermissionPolicy::Bypass),
            ("danger-full-access".into(), "never".into())
        );
        assert_eq!(
            policies(&json!({}), PermissionPolicy::Deny),
            ("workspace-write".into(), "on-request".into())
        );
        assert_eq!(
            policies(
                &json!({ "sandbox": "read-only", "approval": "untrusted" }),
                PermissionPolicy::Bypass
            ),
            ("read-only".into(), "untrusted".into())
        );
    }
}

#[cfg(test)]
mod limit_tests {
    use super::*;

    #[test]
    fn full_reads_list_every_limit_and_the_reset_credits() {
        // As codex-cli 0.163 answers account/rateLimits/read, with a second, model-specific limit.
        let window = |used: u32| json!({ "usedPercent": used, "windowDurationMins": 300, "resetsAt": 1795131666 });
        let limit = |id: &str, name: Value, model: Value, used| {
            json!({ "limitId": id, "limitName": name, "normalModelSlug": model,
                    "primary": window(used), "secondary": null, "planType": "plus",
                    "rateLimitReachedType": null, "spendControlReached": false })
        };
        let result = json!({
            "rateLimits": limit("codex", Value::Null, Value::Null, 8),
            "rateLimitsByLimitId": {
                "zz-luna": limit("zz-luna", json!("GPT-6-Luna"), json!("gpt-6-luna"), 40),
                "codex": limit("codex", Value::Null, Value::Null, 8)
            },
            "rateLimitResetCredits": { "availableCount": 1, "credits": [
                { "id": "R1", "title": "Full reset (Monthly)", "status": "available", "expiresAt": 1792700868 }
            ]}
        });
        let limits = rate_limits_read(&result).unwrap();
        let ids: Vec<&str> = limits.by_limit.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["codex", "zz-luna"], "the main limit first");
        assert_eq!(limits.by_limit[1].model.as_deref(), Some("gpt-6-luna"));
        assert_eq!(limits.by_limit[1].windows[0].used_percent, 40.0);
        let credits = limits.reset_credits.unwrap();
        assert_eq!(credits.len(), 1);
        assert_eq!(credits[0].title, "Full reset (Monthly)");
        assert!(credits[0].expires_at.is_some());
        // A per-call update carries only the limit in use, and no credits.
        let update = rate_limits(&limit("codex", Value::Null, Value::Null, 9));
        assert_eq!(update.by_limit.len(), 1);
        assert!(update.reset_credits.is_none());
    }
}
