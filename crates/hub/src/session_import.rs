//! Importing an external CLI session (starting with Codex) into CrossAgents so a person can
//! pick up a conversation they started in the terminal and continue it here.
//!
//! `GET /v1/sessions/import` scans local Codex rollout files (`<CODEX_HOME>/sessions/**/rollout-*.jsonl`)
//! across the default Codex home and any registered Codex accounts.
//! `POST /v1/sessions/import` creates a CrossAgents agent bound to that `session_id` (and account
//! home when applicable) and backfills its text conversation history into the event log so the
//! transcript is immediately readable before the next turn resumes via `thread/resume`.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use crossagents_api_types::{ImportSessionRequest, ImportSessionsResponse, ImportableSession};
use crossagents_core::{
    Agent, AgentId, AgentProfile, AgentStatus, CoreError, EventKind, MessageId, MessageSource,
    OutputStream, Role, StopReason, Store,
};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::api::{ApiResult, AppError, AppState};

/// Maximum number of recent rollout files inspected during listing so `GET /v1/sessions/import`
/// stays fast even with thousands of historical sessions.
const MAX_SCAN_FILES: usize = 250;
/// Maximum candidates returned to the GUI.
const MAX_CANDIDATES: usize = 40;
/// Maximum title length (in chars).
const MAX_TITLE_CHARS: usize = 120;

#[derive(Debug, Default, Deserialize)]
pub struct ImportSessionsQuery {
    #[serde(default)]
    pub cwd: Option<String>,
}

/// One `CODEX_HOME` directory to scan, plus the registered CrossAgents account id if any.
#[derive(Debug, Clone)]
struct CodexHome {
    dir: PathBuf,
    account: Option<String>,
}

/// A parsed historical turn from a Codex rollout file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedTurn {
    user: String,
    assistant: String,
}

/// Parsed summary (and optionally full turns) of one Codex rollout file.
#[derive(Debug, Clone)]
struct ParsedRollout {
    session_id: String,
    cwd: String,
    title: String,
    updated_at: DateTime<Utc>,
    turns: Vec<ParsedTurn>,
    model: Option<String>,
    effort: Option<String>,
}

/// `GET /v1/sessions/import`: local external Codex sessions, newest first.
pub async fn import_candidates(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ImportSessionsQuery>,
) -> ApiResult<Json<ImportSessionsResponse>> {
    let homes = codex_homes(state.store().as_ref()).await?;
    let filter_cwd = query
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    let existing = existing_codex_sessions(state.store().as_ref()).await?;
    let scanned = tokio::task::spawn_blocking(move || scan_codex_homes(&homes, filter_cwd.as_deref()))
        .await
        .map_err(|err| CoreError::Storage(format!("session scan task failed: {err}")))?;

    let mut candidates: Vec<ImportableSession> = scanned
        .into_iter()
        .map(|(account, r)| {
            let (existing_agent_id, existing_agent_name) = existing
                .get(&r.session_id)
                .map(|(id, name)| (Some(*id), Some(name.clone())))
                .unwrap_or((None, None));
            ImportableSession {
                plugin: "codex".into(),
                session_id: r.session_id,
                cwd: r.cwd,
                title: r.title,
                updated_at: r.updated_at,
                turns: r.turns.len() as u32,
                model: r.model,
                effort: r.effort,
                account,
                existing_agent_id,
                existing_agent_name,
            }
        })
        .collect();
    candidates.truncate(MAX_CANDIDATES);

    Ok(Json(ImportSessionsResponse { candidates }))
}

/// `POST /v1/sessions/import`: imports a local Codex session as a CrossAgents agent.
pub async fn import_session(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ImportSessionRequest>,
) -> ApiResult<(StatusCode, Json<Agent>)> {
    if req.plugin != "codex" {
        return Err(AppError::bad_request(format!(
            "session import currently supports only \"codex\", not {:?}",
            req.plugin
        )));
    }
    let session_id = req.session_id.trim().to_owned();
    if session_id.is_empty() {
        return Err(AppError::bad_request("session_id must not be empty"));
    }

    let Some(driver) = state.runtime.plugins().get(&req.plugin) else {
        return Err(AppError::bad_request(format!(
            "unknown plugin {:?} (available: {})",
            req.plugin,
            state.runtime.plugins().names().join(", ")
        )));
    };
    if !driver.capabilities().resume {
        return Err(AppError::bad_request(format!(
            "plugin {:?} does not support resuming sessions",
            req.plugin
        )));
    }

    if let Some(account) = req.account.as_deref().filter(|s| !s.is_empty()) {
        state.accounts.check_binding(&req.plugin, account).await?;
    }

    let existing = existing_codex_sessions(state.store().as_ref()).await?;
    if let Some((_, existing_name)) = existing.get(&session_id) {
        return Err(CoreError::Conflict(format!(
            "这个 Codex 会话已经在 CrossAgents 里了（{existing_name}）"
        ))
        .into());
    }

    let homes = codex_homes(state.store().as_ref()).await?;
    let requested_account = req
        .account
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    let sid_clone = session_id.clone();
    let found = tokio::task::spawn_blocking(move || {
        find_codex_rollout(&homes, &sid_clone, requested_account.as_deref())
    })
    .await
    .map_err(|err| CoreError::Storage(format!("session lookup task failed: {err}")))?;

    let Some((matched_account, parsed)) = found else {
        return Err(CoreError::NotFound {
            kind: "codex session",
            id: session_id,
        }
        .into());
    };

    let mut profile = AgentProfile::default().normalized()?;
    crate::avatars::assign_new(&state.home, state.store(), &mut profile).await?;

    let id = AgentId::new();
    let desired_cwd = req
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| (!parsed.cwd.trim().is_empty()).then_some(parsed.cwd.as_str()));

    let dir: PathBuf = match desired_cwd {
        Some(path) if Path::new(path).is_dir() => PathBuf::from(path),
        Some(path) if req.cwd.as_deref().is_some_and(|s| !s.trim().is_empty()) => {
            return Err(AppError::bad_request(format!(
                "cwd {path:?} is not a directory"
            )));
        }
        _ => {
            let fallback = state.home.join("chats").join(id.to_string());
            std::fs::create_dir_all(&fallback).map_err(|e| {
                CoreError::Storage(format!("creating {}: {e}", fallback.display()))
            })?;
            fallback
        }
    };
    let cwd = dunce::canonicalize(&dir)
        .map_err(|e| AppError::bad_request(format!("cwd {}: {e}", dir.display())))?;

    let chats = state.home.join("chats");
    let (name, name_source) = crate::identity::free_name_in(
        state.store().as_ref(),
        req.name.as_deref(),
        false,
        &crate::identity::denied_words(state.runtime.plugins()),
        Some(&cwd),
        Some(&chats),
    )
    .await?;

    let mut config = Map::new();
    if let Some(account) = req
        .account
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .or(matched_account)
    {
        config.insert("account".into(), Value::String(account));
    }
    if let Some(effort) = &parsed.effort {
        config.insert("effort".into(), Value::String(effort.clone()));
    }
    if let Some(permission) = req
        .permission
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        config.insert("permission".into(), Value::String(permission.to_owned()));
    }
    config.insert("imported_from".into(), Value::String("codex".into()));

    let topic = (!parsed.title.is_empty()).then(|| parsed.title.clone());
    let model = req
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .or(parsed.model);

    let agent = Agent {
        node: None,
        id,
        name_source,
        topic,
        archived_at: None,
        home: cwd.clone(),
        name,
        plugin: req.plugin,
        driver: None,
        model,
        role: Role::default(),
        cwd,
        config: Value::Object(config),
        status: AgentStatus::Stopped,
        created_at: Utc::now(),
        profile,
    };

    state.store().insert_agent(&agent).await?;
    backfill_history(state.store().as_ref(), id, &session_id, &parsed.turns).await?;
    state
        .bus
        .publish(EventKind::AgentCreated {
            agent: agent.clone(),
        })
        .await?;

    Ok((StatusCode::CREATED, Json(agent)))
}

/// Writes the native `session_id` and historical text turns directly into the event log so
/// `last_session_id` immediately resolves for `driver.resume` and `transcript` shows the past
/// conversation without flooding live SSE subscribers.
async fn backfill_history(
    store: &dyn Store,
    id: AgentId,
    session_id: &str,
    turns: &[ParsedTurn],
) -> Result<(), CoreError> {
    store
        .append_event(EventKind::AgentSessionStarted {
            id,
            session_id: session_id.to_owned(),
        })
        .await?;
    for turn in turns {
        let message_id = MessageId::new();
        store
            .append_event(EventKind::AgentInput {
                id,
                message_id,
                text: turn.user.clone(),
                queued: false,
                from: Some(MessageSource::Human),
                reply_to: None,
                thread: None,
                reply_excerpt: None,
                attachments: Vec::new(),
                session_refs: Vec::new(),
                notices: Vec::new(),
                queued_reason: None,
            })
            .await?;
        store
            .append_event(EventKind::AgentTurnStarted { id, message_id })
            .await?;
        if !turn.assistant.is_empty() {
            store
                .append_event(EventKind::AgentOutput {
                    id,
                    stream: OutputStream::Text,
                    text: turn.assistant.clone(),
                })
                .await?;
        }
        store
            .append_event(EventKind::AgentTurnCompleted {
                id,
                message_id,
                stop_reason: StopReason::EndTurn,
            })
            .await?;
    }
    Ok(())
}

/// Maps `session_id -> (agent_id, agent_name)` for existing `codex` agents in CrossAgents.
async fn existing_codex_sessions(
    store: &dyn Store,
) -> Result<HashMap<String, (AgentId, String)>, CoreError> {
    let mut map = HashMap::new();
    for agent in store.list_agents().await? {
        if agent.plugin != "codex" {
            continue;
        }
        if let Some(sid) = store.last_session_id(agent.id).await? {
            map.insert(sid, (agent.id, agent.name));
        }
    }
    Ok(map)
}

/// All `CODEX_HOME` directories on this machine: registered Codex accounts plus `$CODEX_HOME` /
/// `~/.codex`, deduplicated by canonical path. Registered accounts come first so a default home
/// that is also registered as an account carries its account id.
async fn codex_homes(store: &dyn Store) -> Result<Vec<CodexHome>, CoreError> {
    let mut homes = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for account in store.list_accounts().await? {
        if account.provider != "codex" {
            continue;
        }
        let key = norm_path_key(&account.home);
        if seen.insert(key) {
            homes.push(CodexHome {
                dir: account.home,
                account: Some(account.id),
            });
        }
    }
    if let Some(default) = default_codex_home() {
        let key = norm_path_key(&default);
        if seen.insert(key) {
            homes.push(CodexHome {
                dir: default,
                account: None,
            });
        }
    }
    Ok(homes)
}

fn default_codex_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("CODEX_HOME")
        && !home.is_empty()
    {
        return Some(PathBuf::from(home));
    }
    dirs::home_dir().map(|h| h.join(".codex"))
}

/// Scans rollout files across `homes`, deduplicating by `session_id` (newest file wins) and
/// optionally filtering by `cwd`.
fn scan_codex_homes(
    homes: &[CodexHome],
    filter_cwd: Option<&str>,
) -> Vec<(Option<String>, ParsedRollout)> {
    let filter_key = filter_cwd.map(|c| norm_path_key(Path::new(c)));
    let mut files: Vec<(SystemTime, Option<String>, PathBuf)> = Vec::new();
    for home in homes {
        let sessions_dir = home.dir.join("sessions");
        collect_rollout_files(&sessions_dir, &home.account, &mut files);
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files.truncate(MAX_SCAN_FILES);

    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for (mtime, account, path) in files {
        let Some(parsed) = parse_rollout(&path, mtime, false) else {
            continue;
        };
        if !seen_ids.insert(parsed.session_id.clone()) {
            continue;
        }
        if parsed.turns.is_empty() {
            continue;
        }
        if let Some(ref wanted) = filter_key
            && norm_path_key(Path::new(&parsed.cwd)) != *wanted
        {
            continue;
        }
        out.push((account, parsed));
    }
    out.sort_by(|a, b| b.1.updated_at.cmp(&a.1.updated_at));
    out
}

/// Locates the newest rollout file matching `session_id` and parses its full turn history.
fn find_codex_rollout(
    homes: &[CodexHome],
    session_id: &str,
    account_filter: Option<&str>,
) -> Option<(Option<String>, ParsedRollout)> {
    let mut files: Vec<(SystemTime, Option<String>, PathBuf)> = Vec::new();
    for home in homes {
        if let Some(wanted) = account_filter
            && home.account.as_deref() != Some(wanted)
        {
            continue;
        }
        let sessions_dir = home.dir.join("sessions");
        collect_rollout_files(&sessions_dir, &home.account, &mut files);
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));

    // Fast path: Codex names rollout files `rollout-<ts>-<threadId>[_<revertId>].jsonl`.
    for (mtime, account, path) in &files {
        let matches_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains(session_id));
        if matches_name
            && let Some(parsed) = parse_rollout(path, *mtime, true)
            && parsed.session_id == session_id
        {
            return Some((account.clone(), parsed));
        }
    }
    // Fallback: inspect `session_meta.id` of recent files in case a file was renamed.
    for (mtime, account, path) in files.into_iter().take(MAX_SCAN_FILES) {
        if let Some(parsed) = parse_rollout(&path, mtime, true)
            && parsed.session_id == session_id
        {
            return Some((account, parsed));
        }
    }
    None
}

fn collect_rollout_files(
    dir: &Path,
    account: &Option<String>,
    out: &mut Vec<(SystemTime, Option<String>, PathBuf)>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            collect_rollout_files(&path, account, out);
        } else if meta.is_file()
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
        {
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            out.push((mtime, account.clone(), path));
        }
    }
}

/// Parses one Codex rollout `.jsonl` file.
/// When `include_assistant` is false, assistant text is skipped to make listing faster.
fn parse_rollout(path: &Path, mtime: SystemTime, include_assistant: bool) -> Option<ParsedRollout> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);

    let mut session_id: Option<String> = None;
    let mut cwd = String::new();
    let mut model: Option<String> = None;
    let mut effort: Option<String> = None;
    let mut turns: Vec<ParsedTurn> = Vec::new();
    let mut seen_item_ids: HashSet<String> = HashSet::new();
    let mut saw_event_user = false;
    let mut saw_event_assistant = false;
    let mut fallback_turns: Vec<ParsedTurn> = Vec::new();

    for line in reader.lines().map_while(Result::ok) {
        if line.is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(kind) = val.get("type").and_then(Value::as_str) else {
            continue;
        };
        let payload = val.get("payload");
        match kind {
            "session_meta" => {
                if let Some(p) = payload {
                    if session_id.is_none() {
                        session_id = p
                            .get("id")
                            .or_else(|| p.get("session_id"))
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned);
                    }
                    if cwd.is_empty()
                        && let Some(c) = p.get("cwd").and_then(Value::as_str)
                    {
                        cwd = c.to_owned();
                    }
                }
            }
            "turn_context" => {
                if let Some(p) = payload {
                    if let Some(m) = p.get("model").and_then(Value::as_str)
                        && !m.is_empty()
                    {
                        model = Some(m.to_owned());
                    }
                    if let Some(e) = p.get("effort").and_then(Value::as_str)
                        && !e.is_empty()
                    {
                        effort = Some(e.to_owned());
                    }
                    if let Some(c) = p.get("cwd").and_then(Value::as_str)
                        && !c.is_empty()
                    {
                        cwd = c.to_owned();
                    }
                }
            }
            "event_msg" => {
                let Some(p) = payload else { continue };
                let Some(pt) = p.get("type").and_then(Value::as_str) else {
                    continue;
                };
                match pt {
                    "item_completed" => {
                        let Some(item) = p.get("item") else { continue };
                        let Some(it) = item.get("type").and_then(Value::as_str) else {
                            continue;
                        };
                        let item_id = item.get("id").and_then(Value::as_str);
                        if let Some(id) = item_id
                            && !seen_item_ids.insert(id.to_owned())
                        {
                            continue;
                        }
                        match it {
                            "UserMessage" => {
                                let text = extract_content_text(item.get("content"));
                                if !text.is_empty() {
                                    saw_event_user = true;
                                    turns.push(ParsedTurn {
                                        user: text,
                                        assistant: String::new(),
                                    });
                                }
                            }
                            "AgentMessage" if include_assistant => {
                                let text = extract_content_text(item.get("content"));
                                if !text.is_empty() {
                                    saw_event_assistant = true;
                                    append_assistant(&mut turns, &text);
                                }
                            }
                            _ => {}
                        }
                    }
                    "user_message" => {
                        if let Some(text) = p
                            .get("message")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                        {
                            saw_event_user = true;
                            turns.push(ParsedTurn {
                                user: text.to_owned(),
                                assistant: String::new(),
                            });
                        }
                    }
                    "agent_message" if include_assistant => {
                        if let Some(text) = p
                            .get("message")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                        {
                            saw_event_assistant = true;
                            append_assistant(&mut turns, text);
                        }
                    }
                    _ => {}
                }
            }
            "response_item" => {
                // Fallback for older rollouts that only recorded `response_item/message`.
                let Some(p) = payload else { continue };
                if p.get("type").and_then(Value::as_str) != Some("message") {
                    continue;
                }
                let role = p.get("role").and_then(Value::as_str).unwrap_or("");
                let msg_id = p.get("id").and_then(Value::as_str);
                if let Some(id) = msg_id
                    && seen_item_ids.contains(id)
                {
                    continue;
                }
                match role {
                    "user" if !saw_event_user => {
                        let text = extract_content_text(p.get("content"));
                        if !text.is_empty() && !is_codex_injected_context(&text) {
                            fallback_turns.push(ParsedTurn {
                                user: text,
                                assistant: String::new(),
                            });
                        }
                    }
                    "assistant" if include_assistant && !saw_event_assistant => {
                        let text = extract_content_text(p.get("content"));
                        if !text.is_empty() {
                            append_assistant(&mut fallback_turns, &text);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let final_turns = if !turns.is_empty() {
        turns
    } else {
        fallback_turns
    };
    let session_id = session_id?;
    let title = final_turns
        .first()
        .map(|t| summarize_title(&t.user))
        .unwrap_or_default();

    Some(ParsedRollout {
        session_id,
        cwd,
        title,
        updated_at: DateTime::<Utc>::from(mtime),
        turns: final_turns,
        model,
        effort,
    })
}

fn append_assistant(turns: &mut [ParsedTurn], text: &str) {
    if let Some(last) = turns.last_mut() {
        if !last.assistant.is_empty() {
            last.assistant.push_str("\n\n");
        }
        last.assistant.push_str(text);
    }
}

fn extract_content_text(content: Option<&Value>) -> String {
    let Some(items) = content.and_then(Value::as_array) else {
        return String::new();
    };
    let mut parts = Vec::new();
    for item in items {
        if let Some(text) = item.get("text").and_then(Value::as_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed);
            }
        }
    }
    parts.join("\n")
}

/// Codex injects `<environment_context>` / `<AGENTS.md>` / `# AGENTS.md` as synthetic user
/// `response_item` messages in older rollouts; skip those when building a fallback title/turn list.
fn is_codex_injected_context(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("<environment_context>")
        || trimmed.starts_with("<INSTRUCTIONS>")
        || trimmed.starts_with("# AGENTS.md instructions")
}

fn summarize_title(text: &str) -> String {
    let single_line: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    single_line.chars().take(MAX_TITLE_CHARS).collect()
}

fn norm_path_key(path: &Path) -> String {
    let resolved = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let s = resolved.to_string_lossy().replace('\\', "/");
    let trimmed = s.trim_end_matches('/');
    if cfg!(windows) {
        trimmed.to_ascii_lowercase()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn write_rollout(dir: &Path, rel: &str, lines: &[Value]) -> PathBuf {
        let path = dir.join("sessions").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = lines
            .iter()
            .map(|v| serde_json::to_string(v).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn parses_modern_rollout_without_duplicating_response_items() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_rollout(
            tmp.path(),
            "2026/11/17/rollout-2026-11-17T10-00-00-tid-1.jsonl",
            &[
                serde_json::json!({
                    "type": "session_meta",
                    "payload": { "id": "tid-1", "cwd": "/repo/app" }
                }),
                serde_json::json!({
                    "type": "turn_context",
                    "payload": { "model": "gpt-6-sol", "effort": "medium", "cwd": "/repo/app" }
                }),
                serde_json::json!({
                    "type": "event_msg",
                    "payload": {
                        "type": "item_completed",
                        "item": {
                            "type": "UserMessage",
                            "id": "u1",
                            "content": [{ "type": "text", "text": "Fix the login bug" }]
                        }
                    }
                }),
                serde_json::json!({
                    "type": "event_msg",
                    "payload": {
                        "type": "item_completed",
                        "item": {
                            "type": "AgentMessage",
                            "id": "a1",
                            "content": [{ "type": "Text", "text": "I fixed it." }]
                        }
                    }
                }),
                // Duplicate response_item with same id must not double-append.
                serde_json::json!({
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "id": "a1",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": "I fixed it." }]
                    }
                }),
            ],
        );

        let parsed = parse_rollout(&path, SystemTime::UNIX_EPOCH, true).unwrap();
        assert_eq!(parsed.session_id, "tid-1");
        assert_eq!(parsed.cwd, "/repo/app");
        assert_eq!(parsed.model.as_deref(), Some("gpt-6-sol"));
        assert_eq!(parsed.effort.as_deref(), Some("medium"));
        assert_eq!(parsed.title, "Fix the login bug");
        assert_eq!(
            parsed.turns,
            vec![ParsedTurn {
                user: "Fix the login bug".into(),
                assistant: "I fixed it.".into(),
            }]
        );
    }

    #[test]
    fn scan_deduplicates_reverted_rollouts_keeping_newest() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("my-repo");
        std::fs::create_dir_all(&repo).unwrap();
        let repo_str = repo.display().to_string();

        let old_path = write_rollout(
            tmp.path(),
            "2026/11/16/rollout-2026-11-16T10-00-00-tid-same.jsonl",
            &[
                serde_json::json!({ "type": "session_meta", "payload": { "id": "tid-same", "cwd": repo_str } }),
                serde_json::json!({
                    "type": "event_msg",
                    "payload": { "type": "user_message", "message": "Old version" }
                }),
            ],
        );
        // Ensure newer mtime on the second file.
        std::thread::sleep(Duration::from_millis(20));
        let _new_path = write_rollout(
            tmp.path(),
            "2026/11/17/rollout-2026-11-17T10-00-00-tid-same_rev2.jsonl",
            &[
                serde_json::json!({ "type": "session_meta", "payload": { "id": "tid-same", "cwd": repo_str } }),
                serde_json::json!({
                    "type": "event_msg",
                    "payload": { "type": "user_message", "message": "Newer reverted version" }
                }),
            ],
        );
        assert!(old_path.exists());

        let homes = vec![CodexHome {
            dir: tmp.path().to_path_buf(),
            account: None,
        }];
        let found = scan_codex_homes(&homes, Some(&repo_str));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1.title, "Newer reverted version");
    }

    #[tokio::test]
    async fn backfill_history_populates_last_session_id_and_transcript() {
        let store = crossagents_store_sqlite::SqliteStore::open_in_memory().await.unwrap();
        let id = AgentId::new();
        let turns = vec![ParsedTurn {
            user: "Hello Codex".into(),
            assistant: "Hi there!".into(),
        }];
        backfill_history(&store, id, "thread-xyz", &turns)
            .await
            .unwrap();

        assert_eq!(
            store.last_session_id(id).await.unwrap().as_deref(),
            Some("thread-xyz")
        );
        let events = store.agent_events(id, 0, 100).await.unwrap();
        let mut fold = crate::transcript::Fold::default();
        for ev in &events {
            fold.push(ev);
        }
        assert_eq!(fold.turns.len(), 1);
        assert_eq!(fold.turns[0].input, "Hello Codex");
        assert_eq!(fold.turns[0].stop_reason, Some(StopReason::EndTurn));
    }
}
