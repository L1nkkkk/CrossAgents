//! Read-only references to other sessions (`@session` / sidebar drag-and-drop).
//!
//! Each reference is resolved strictly by `AgentId` (display metadata is only a snapshot).
//! The hub materialises a concise inline `<referenced_sessions>` block for the model AND a
//! local read-only Markdown transcript snapshot under `<hub_home>/session-refs/<id>.md`, so
//! both MCP-enabled drivers (`session_read`/`session_search`) and non-MCP drivers (such as
//! `antigravity-native`) can read referenced session data immediately.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use crossagents_api_types::SessionRefInput;
use crossagents_core::{Agent, AgentKey, CoreError, EventKind, MessageId, SessionRef, Store};

/// Maximum distinct sessions referenced in a single message.
pub const MAX_SESSION_REFS: usize = 6;

const SUMMARY_MAX_TURNS: usize = 6;
const SUMMARY_MAX_CHARS: usize = 6_000;
const FULL_MAX_TURNS: usize = 20;
const FULL_MAX_CHARS: usize = 20_000;
const SNAPSHOT_MAX_TURNS: usize = 80;
const SNAPSHOT_MAX_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefMode {
    Summary,
    Pointer,
    Full,
}

impl RefMode {
    fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("pointer") => Self::Pointer,
            Some("full") => Self::Full,
            _ => Self::Summary,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Pointer => "pointer",
            Self::Full => "full",
        }
    }
}

struct TurnBuf {
    message_id: MessageId,
    sent_at: DateTime<Utc>,
    input: String,
    output: String,
    errors: Vec<String>,
}

/// Resolves `inputs` against `store`, writes read-only transcript snapshots to
/// `<hub_home>/session-refs/`, and returns `(recorded_refs, prompt_context_block)`.
pub async fn resolve_session_refs(
    store: &dyn Store,
    hub_home: &Path,
    inputs: &[SessionRefInput],
) -> Result<(Vec<SessionRef>, Option<String>), CoreError> {
    if inputs.is_empty() {
        return Ok((Vec::new(), None));
    }
    if inputs.len() > MAX_SESSION_REFS {
        return Err(CoreError::Unsupported(format!(
            "at most {MAX_SESSION_REFS} referenced sessions per message (got {})",
            inputs.len()
        )));
    }

    let chats_dir = hub_home.join("chats");
    let mut seen = HashSet::new();
    let mut refs = Vec::new();
    let mut sections = Vec::new();

    for item in inputs {
        let raw_key = item.agent.trim();
        if raw_key.is_empty() {
            continue;
        }
        let target = store.get_agent(&AgentKey::parse(raw_key)).await?;
        if !seen.insert(target.id) {
            continue;
        }
        let mode = RefMode::parse(item.mode.as_deref());
        let project = project_label(&target, &chats_dir);
        let turns = load_turns(store, target.id).await?;
        let snapshot_path = write_snapshot(hub_home, &target, &turns);

        refs.push(SessionRef {
            agent_id: target.id,
            name: target.name.clone(),
            topic: target.topic.clone(),
            project: project.clone(),
            mode: Some(mode.as_str().to_owned()),
        });

        sections.push(format_section(
            &target,
            project.as_deref(),
            mode,
            &turns,
            snapshot_path.as_deref(),
        ));
    }

    if refs.is_empty() {
        return Ok((Vec::new(), None));
    }

    let mut block = String::from(
        "<referenced_sessions>\n\
         The person attached the following read-only session reference(s) for context. \
         Do NOT message or control these sessions unless explicitly asked. \
         If you need earlier turns or keyword matches beyond the excerpt below, call \
         `session_read(agent=\"<agent_id>\")` / `session_search(agent=\"<agent_id>\", query=...)`, \
         or read the listed `snapshot_file` directly.\n",
    );
    for section in sections {
        block.push('\n');
        block.push_str(&section);
    }
    block.push_str("\n</referenced_sessions>");

    Ok((refs, Some(block)))
}

fn project_label(agent: &Agent, chats_dir: &Path) -> Option<String> {
    let home = agent.home();
    if home.as_os_str().is_empty() || home.starts_with(chats_dir) {
        return None;
    }
    let norm = home.to_string_lossy().replace('\\', "/");
    if norm.contains("/.crossagents/chats/") {
        return None;
    }
    home.file_name()
        .and_then(|s| s.to_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

async fn load_turns(store: &dyn Store, agent_id: crossagents_core::AgentId) -> Result<Vec<TurnBuf>, CoreError> {
    let mut turns: Vec<TurnBuf> = Vec::new();
    let mut by_msg: HashMap<MessageId, usize> = HashMap::new();
    let mut active: Option<usize> = None;
    let mut after = 0u64;

    loop {
        let batch = store.agent_events(agent_id, after, 1000).await?;
        if batch.is_empty() {
            break;
        }
        for ev in &batch {
            after = ev.seq;
            match &ev.kind {
                EventKind::AgentInput {
                    message_id, text, ..
                } => {
                    let idx = turns.len();
                    turns.push(TurnBuf {
                        message_id: *message_id,
                        sent_at: ev.ts,
                        input: text.clone(),
                        output: String::new(),
                        errors: Vec::new(),
                    });
                    by_msg.insert(*message_id, idx);
                    active.get_or_insert(idx);
                }
                EventKind::AgentInputUpdated {
                    message_id, text, ..
                } => {
                    if let Some(&idx) = by_msg.get(message_id) {
                        turns[idx].input.clone_from(text);
                    }
                }
                EventKind::AgentTurnStarted { message_id, .. } => {
                    if let Some(&idx) = by_msg.get(message_id) {
                        active = Some(idx);
                    }
                }
                EventKind::AgentOutput { stream, text, .. }
                    if matches!(stream, crossagents_core::OutputStream::Text) =>
                {
                    if let Some(idx) = active
                        && let Some(turn) = turns.get_mut(idx)
                    {
                        turn.output.push_str(text);
                    }
                }
                EventKind::AgentError { message, .. } => {
                    if let Some(idx) = active
                        && let Some(turn) = turns.get_mut(idx)
                    {
                        turn.errors.push(message.clone());
                    }
                }
                EventKind::AgentTurnCompleted { .. } => {
                    active = None;
                }
                _ => {}
            }
        }
        if batch.len() < 1000 {
            break;
        }
    }

    Ok(turns)
}

fn write_snapshot(hub_home: &Path, agent: &Agent, turns: &[TurnBuf]) -> Option<PathBuf> {
    let dir = hub_home.join("session-refs");
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }
    let path = dir.join(format!("{}.md", agent.id));
    let mut md = format!(
        "# Session Snapshot: {}\n\n- agent_id: `{}`\n- topic: {}\n- project_cwd: `{}`\n- model: {} ({})\n- archived: {}\n- total_turns: {}\n\n",
        agent.name,
        agent.id,
        agent.topic.as_deref().unwrap_or("(none)"),
        agent.cwd.display(),
        agent.model.as_deref().unwrap_or("default"),
        agent.plugin,
        agent.archived_at.is_some(),
        turns.len(),
    );
    let start = turns.len().saturating_sub(SNAPSHOT_MAX_TURNS);
    if start > 0 {
        md.push_str(&format!("_(showing latest {} of {} turns)_\n\n", turns.len() - start, turns.len()));
    }
    for (offset, turn) in turns[start..].iter().enumerate() {
        let num = start + offset + 1;
        let entry = format_turn_markdown(num, turn, 8_000, 12_000);
        if md.len() + entry.len() > SNAPSHOT_MAX_BYTES {
            md.push_str("\n_(snapshot truncated at 256 KB limit)_\n");
            break;
        }
        md.push_str(&entry);
    }
    if std::fs::write(&path, md).is_ok() {
        Some(path)
    } else {
        None
    }
}

fn format_turn_markdown(num: usize, turn: &TurnBuf, max_in: usize, max_out: usize) -> String {
    let mut s = format!(
        "## Turn {num} ({}, message_id: `{}`)\n\n### User\n{}\n",
        turn.sent_at.format("%Y-%m-%d %H:%M UTC"),
        turn.message_id,
        truncate_chars(turn.input.trim(), max_in),
    );
    if !turn.output.trim().is_empty() {
        s.push_str("\n### Assistant\n");
        s.push_str(&truncate_chars(turn.output.trim(), max_out));
        s.push('\n');
    }
    if !turn.errors.is_empty() {
        s.push_str("\n### Errors\n");
        for err in &turn.errors {
            s.push_str("- ");
            s.push_str(&truncate_chars(err.trim(), 500));
            s.push('\n');
        }
    }
    s.push('\n');
    s
}

fn format_section(
    agent: &Agent,
    project: Option<&str>,
    mode: RefMode,
    turns: &[TurnBuf],
    snapshot_path: Option<&Path>,
) -> String {
    let mut out = format!(
        "<session agent_id=\"{}\" name=\"{}\" mode=\"{}\" total_turns=\"{}\" archived=\"{}\">\n",
        agent.id,
        escape_attr(&agent.name),
        mode.as_str(),
        turns.len(),
        agent.archived_at.is_some(),
    );
    if let Some(topic) = agent.topic.as_deref().filter(|t| !t.trim().is_empty()) {
        out.push_str(&format!("topic: {topic}\n"));
    }
    if let Some(proj) = project {
        out.push_str(&format!("project: {proj}\n"));
    }
    out.push_str(&format!("cwd: {}\n", agent.cwd.display()));
    out.push_str(&format!(
        "model: {} ({})\n",
        agent.model.as_deref().unwrap_or("default"),
        agent.plugin
    ));
    if let Some(path) = snapshot_path {
        out.push_str(&format!("snapshot_file: {}\n", path.display()));
    }

    match mode {
        RefMode::Pointer => {
            if let Some(last) = turns.last() {
                out.push_str(&format!(
                    "latest_turn_at: {}\nlatest_user_prompt: {}\n",
                    last.sent_at.format("%Y-%m-%d %H:%M UTC"),
                    truncate_chars(last.input.trim(), 200),
                ));
            }
        }
        RefMode::Summary | RefMode::Full => {
            let (max_turns, max_chars) = match mode {
                RefMode::Summary => (SUMMARY_MAX_TURNS, SUMMARY_MAX_CHARS),
                RefMode::Full => (FULL_MAX_TURNS, FULL_MAX_CHARS),
                RefMode::Pointer => unreachable!(),
            };
            if turns.is_empty() {
                out.push_str("(no messages recorded yet)\n");
            } else {
                let start = turns.len().saturating_sub(max_turns);
                if start > 0 {
                    out.push_str(&format!(
                        "[showing latest {} of {} turns; use session_read/session_search or snapshot_file for earlier context]\n",
                        turns.len() - start,
                        turns.len()
                    ));
                }
                let per_input = if mode == RefMode::Full { 2_000 } else { 700 };
                let per_output = if mode == RefMode::Full { 4_000 } else { 1_200 };
                let mut used = 0usize;
                for (offset, turn) in turns[start..].iter().enumerate() {
                    let num = start + offset + 1;
                    let chunk = format_turn_markdown(num, turn, per_input, per_output);
                    let chunk_chars = chunk.chars().count();
                    if used > 0 && used + chunk_chars > max_chars {
                        out.push_str("[excerpt truncated at character budget; read snapshot_file or call session_read for full details]\n");
                        break;
                    }
                    out.push_str(&chunk);
                    used += chunk_chars;
                }
            }
        }
    }

    out.push_str("</session>");
    out
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;")
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossagents_core::{AgentId, AgentProfile, AgentStatus, NameSource, OutputStream, Role};
    use crossagents_store_sqlite::SqliteStore;

    #[tokio::test]
    async fn resolves_session_refs_builds_context_and_writes_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open_in_memory().await.unwrap();
        let target_id = AgentId::new();
        let target = Agent {
            id: target_id,
            name: "CA-Nyx-1".into(),
            name_source: NameSource::Generated,
            plugin: "codex".into(),
            driver: None,
            model: Some("gpt-6.2-sol".into()),
            role: Role::Worker,
            home: PathBuf::from("C:/Dev/CrossAgents"),
            cwd: PathBuf::from("C:/Dev/CrossAgents"),
            config: serde_json::json!({}),
            status: AgentStatus::Idle,
            created_at: Utc::now(),
            profile: AgentProfile::default(),
            topic: Some("修复并发锁问题".into()),
            archived_at: None,
            node: None,
        };
        store.insert_agent(&target).await.unwrap();

        let msg_id = MessageId::new();
        let _ = store
            .append_event(EventKind::AgentInput {
                id: target_id,
                message_id: msg_id,
                text: "请排查 continuation 测试超时原因".into(),
                queued: false,
                from: Some(crossagents_core::MessageSource::Human),
                reply_to: None,
                thread: None,
                reply_excerpt: None,
                attachments: Vec::new(),
                session_refs: Vec::new(),
                notices: Vec::new(),
                queued_reason: None,
            })
            .await
            .unwrap();
        let _ = store
            .append_event(EventKind::AgentTurnStarted {
                id: target_id,
                message_id: msg_id,
            })
            .await
            .unwrap();
        let _ = store
            .append_event(EventKind::AgentOutput {
                id: target_id,
                stream: OutputStream::Text,
                text: "根因是 update_run_policy 缺少 run 锁".into(),
            })
            .await
            .unwrap();
        let _ = store
            .append_event(EventKind::AgentTurnCompleted {
                id: target_id,
                message_id: msg_id,
                stop_reason: crossagents_core::StopReason::EndTurn,
            })
            .await
            .unwrap();

        // Duplicate inputs collapse cleanly to one reference.
        let (refs, block) = resolve_session_refs(
            &store,
            dir.path(),
            &[
                SessionRefInput {
                    agent: target_id.to_string(),
                    mode: None,
                },
                SessionRefInput {
                    agent: "CA-Nyx-1".into(),
                    mode: Some("summary".into()),
                },
            ],
        )
        .await
        .unwrap();

        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].agent_id, target_id);
        assert_eq!(refs[0].name, "CA-Nyx-1");
        assert_eq!(refs[0].project.as_deref(), Some("CrossAgents"));
        let block = block.expect("context block");
        assert!(block.contains("<referenced_sessions>"), "{block}");
        assert!(block.contains(&target_id.to_string()), "{block}");
        assert!(block.contains("请排查 continuation 测试超时原因"), "{block}");
        assert!(block.contains("根因是 update_run_policy 缺少 run 锁"), "{block}");
        let snap = dir.path().join("session-refs").join(format!("{target_id}.md"));
        assert!(snap.is_file(), "snapshot file should exist at {}", snap.display());
        let snap_text = std::fs::read_to_string(&snap).unwrap();
        assert!(snap_text.contains("修复并发锁问题"));
    }
}
