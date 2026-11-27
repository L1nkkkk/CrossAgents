//! Multi-Model Council execution engine: runs 2..=5 models or teammates in parallel on the
//! same question, with an optional anonymized cross-critique second round (`CouncilMode::Debate`),
//! persisting progress per seat into SQLite so callers (both MCP tools and GUI) see live status.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AgentKey, AgentProfile, AgentStatus, CoreError, CouncilMemberResponse,
    CouncilMemberSpec, CouncilMode, CouncilRecord, CouncilStatus, EventBus, EventKind,
    MessageSource, NameSource, OutputStream, Role, StopReason,
};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::runtime::Runtime;

const TURN_TIMEOUT: Duration = Duration::from_secs(300);
const SEATS: &[&str] = &["A", "B", "C", "D", "E"];

/// Everything needed to run a council consultation.
pub struct CouncilParams {
    pub question: String,
    pub context: Option<String>,
    pub mode: CouncilMode,
    pub members: Vec<CouncilMemberSpec>,
    pub cwd: Option<String>,
    pub requested_by: Option<AgentId>,
    pub home: PathBuf,
}

#[derive(Clone)]
struct ResolvedSeat {
    index: usize,
    seat: String,
    label: String,
    plugin: String,
    model: Option<String>,
    effort: Option<String>,
    perspective: Option<String>,
    cwd: PathBuf,
    profile: AgentProfile,
    base_config: serde_json::Value,
    init_error: Option<String>,
}

/// Validates and starts a council consultation, persisting the initial `Running` record
/// immediately and returning `(initial_record, join_handle)` so callers can either await
/// completion synchronously (MCP `council_consult`) or poll live progress (GUI).
pub async fn start_council(
    runtime: Arc<Runtime>,
    bus: Arc<EventBus>,
    params: CouncilParams,
) -> Result<(CouncilRecord, tokio::task::JoinHandle<CouncilRecord>), CoreError> {
    let question = params.question.trim().to_owned();
    if question.is_empty() {
        return Err(CoreError::InvalidName("question cannot be empty".into()));
    }
    if !(2..=5).contains(&params.members.len()) {
        return Err(CoreError::InvalidName(
            "council requires between 2 and 5 members".into(),
        ));
    }

    let default_cwd = params
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| params.home.clone()));

    let mut seats = Vec::with_capacity(params.members.len());
    for (idx, spec) in params.members.iter().enumerate() {
        let seat_code = SEATS.get(idx).copied().unwrap_or("X").to_owned();
        let resolved =
            resolve_seat(&runtime, &bus, idx, seat_code, spec, &default_cwd).await;
        seats.push(resolved);
    }

    let now = Utc::now();
    let short_id = Uuid::now_v7().to_string();
    let id = format!("council-{}", &short_id[..8]);

    let initial_members: Vec<CouncilMemberResponse> = seats
        .iter()
        .map(|s| CouncilMemberResponse {
            seat: s.seat.clone(),
            label: s.label.clone(),
            plugin: s.plugin.clone(),
            model: s.model.clone(),
            agent_id: None,
            status: if s.init_error.is_some() {
                "error".into()
            } else {
                "running".into()
            },
            round1: None,
            round2: None,
            error: s.init_error.clone(),
            duration_ms: None,
        })
        .collect();

    let record = CouncilRecord {
        id: id.clone(),
        question: question.clone(),
        context: params
            .context
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned),
        mode: params.mode,
        status: CouncilStatus::Running,
        requested_by: params.requested_by,
        cwd: Some(default_cwd.to_string_lossy().into_owned()),
        members: initial_members,
        summary: None,
        created_at: now,
        updated_at: now,
    };
    bus.store().upsert_council(&record).await?;

    let initial_clone = record.clone();
    let shared = Arc::new(Mutex::new(record));
    let handle = tokio::spawn(async move {
        execute_council(runtime, bus, shared, seats).await
    });
    Ok((initial_clone, handle))
}

async fn resolve_seat(
    runtime: &Runtime,
    bus: &EventBus,
    index: usize,
    seat: String,
    spec: &CouncilMemberSpec,
    default_cwd: &PathBuf,
) -> ResolvedSeat {
    let mut plugin = spec
        .plugin
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    let mut model = spec
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    let effort = spec
        .effort
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    let perspective = spec
        .perspective
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    let mut label = spec
        .label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    let mut cwd = default_cwd.clone();
    let mut profile = AgentProfile::default();
    let mut base_config = serde_json::json!({});

    // Optional template teammate/session
    if let Some(agent_ref) = spec.agent.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        match bus.store().get_agent(&AgentKey::parse(agent_ref)).await {
            Ok(tmpl) => {
                if plugin.is_none() {
                    plugin = Some(tmpl.plugin.clone());
                }
                if model.is_none() {
                    model = tmpl.model.clone();
                }
                if label.is_none() {
                    label = Some(tmpl.name.clone());
                }
                if !tmpl.cwd.as_os_str().is_empty() {
                    cwd = tmpl.cwd.clone();
                }
                profile = tmpl.profile.clone();
                if tmpl.config.is_object() {
                    base_config = tmpl.config.clone();
                }
            }
            Err(err) => {
                return ResolvedSeat {
                    index,
                    seat: seat.clone(),
                    label: label.unwrap_or_else(|| agent_ref.to_owned()),
                    plugin: plugin.unwrap_or_default(),
                    model,
                    effort,
                    perspective,
                    cwd,
                    profile,
                    base_config,
                    init_error: Some(format!("teammate {agent_ref:?} not found: {err}")),
                };
            }
        }
    }

    // Resolve plugin from model if plugin was omitted (strict match — never silently substitutes).
    if plugin.is_none()
        && let Some(ref wanted_model) = model
    {
        for driver in runtime.plugins().defaults() {
            if driver.models().iter().any(|m| m == wanted_model) {
                plugin = Some(driver.name().to_owned());
                break;
            }
        }
    }

    let Some(plugin_name) = plugin else {
        let fallback_label = label
            .or_else(|| model.clone())
            .unwrap_or_else(|| format!("Seat {seat}"));
        return ResolvedSeat {
            index,
            seat,
            label: fallback_label,
            plugin: String::new(),
            model: model.clone(),
            effort,
            perspective,
            cwd,
            profile,
            base_config,
            init_error: Some(match model {
                Some(m) => format!("no installed plugin offers model {m:?}; specify `plugin` explicitly"),
                None => "member must specify `agent`, `plugin`, or a recognized `model`".into(),
            }),
        };
    };

    if !runtime.plugins().contains(&plugin_name) {
        return ResolvedSeat {
            index,
            seat: seat.clone(),
            label: label
                .or_else(|| model.clone())
                .unwrap_or_else(|| plugin_name.clone()),
            plugin: plugin_name.clone(),
            model,
            effort,
            perspective,
            cwd,
            profile,
            base_config,
            init_error: Some(format!("unknown plugin {plugin_name:?}")),
        };
    }

    let final_label = label
        .or_else(|| model.clone())
        .unwrap_or_else(|| format!("{plugin_name} ({seat})"));

    ResolvedSeat {
        index,
        seat,
        label: final_label,
        plugin: plugin_name,
        model,
        effort,
        perspective,
        cwd,
        profile,
        base_config,
        init_error: None,
    }
}

async fn execute_council(
    runtime: Arc<Runtime>,
    bus: Arc<EventBus>,
    shared: Arc<Mutex<CouncilRecord>>,
    seats: Vec<ResolvedSeat>,
) -> CouncilRecord {
    let (council_id, question, context, mode) = {
        let guard = shared.lock().await;
        (
            guard.id.clone(),
            guard.question.clone(),
            guard.context.clone(),
            guard.mode,
        )
    };

    // Spawn clean temporary agents + run Round 1 in parallel across all valid seats.
    let mut r1_handles = Vec::new();
    for seat in seats.iter().cloned() {
        if seat.init_error.is_some() {
            continue;
        }
        let runtime = runtime.clone();
        let bus = bus.clone();
        let shared = shared.clone();
        let council_id = council_id.clone();
        let question = question.clone();
        let context = context.clone();
        r1_handles.push(tokio::spawn(async move {
            run_seat_round1(
                runtime,
                bus,
                shared,
                &council_id,
                &seat,
                &question,
                context.as_deref(),
            )
            .await
        }));
    }

    // Track spawned temporary agents so every one of them is archived in cleanup.
    let mut spawned: Vec<(usize, AgentId, String)> = Vec::new();
    for h in r1_handles {
        if let Ok(Some((idx, aid, answer))) = h.await {
            spawned.push((idx, aid, answer));
        }
    }

    // Round 2 (Debate mode): share anonymized Round 1 answers so each seat critiques blind spots.
    if mode == CouncilMode::Debate && spawned.len() >= 2 {
        let r1_snapshot: Vec<(usize, String, String)> = {
            let guard = shared.lock().await;
            spawned
                .iter()
                .filter_map(|(idx, _, ans)| {
                    guard
                        .members
                        .get(*idx)
                        .map(|m| (*idx, m.seat.clone(), ans.clone()))
                })
                .collect()
        };

        let mut r2_handles = Vec::new();
        for (idx, agent_id, _) in &spawned {
            let idx = *idx;
            let agent_id = *agent_id;
            let prompt = build_round2_prompt(&question, idx, &r1_snapshot);
            let runtime = runtime.clone();
            let bus = bus.clone();
            let shared = shared.clone();
            r2_handles.push(tokio::spawn(async move {
                let started = Instant::now();
                match run_single_turn(&runtime, &bus, agent_id, prompt).await {
                    Ok(reply) => {
                        let elapsed = started.elapsed().as_millis() as u64;
                        let snapshot = {
                            let mut guard = shared.lock().await;
                            if let Some(m) = guard.members.get_mut(idx) {
                                m.round2 = Some(reply);
                                m.duration_ms = Some(m.duration_ms.unwrap_or(0) + elapsed);
                            }
                            guard.updated_at = Utc::now();
                            guard.clone()
                        };
                        let _ = bus.store().upsert_council(&snapshot).await;
                    }
                    Err(err) => {
                        let snapshot = {
                            let mut guard = shared.lock().await;
                            if let Some(m) = guard.members.get_mut(idx) {
                                m.error = Some(format!("round 2 failed: {err}"));
                            }
                            guard.updated_at = Utc::now();
                            guard.clone()
                        };
                        let _ = bus.store().upsert_council(&snapshot).await;
                    }
                }
            }));
        }
        for h in r2_handles {
            let _ = h.await;
        }
    }

    // Archive every temporary council agent so the active sidebar stays tidy.
    let agent_ids: Vec<AgentId> = {
        let guard = shared.lock().await;
        guard.members.iter().filter_map(|m| m.agent_id).collect()
    };
    for aid in agent_ids {
        let _ = runtime.archive(&AgentKey::Id(aid), true).await;
    }

    // Finalize overall status and persist.
    let final_record = {
        let mut guard = shared.lock().await;
        let ok_count = guard.members.iter().filter(|m| m.status == "ok").count();
        guard.status = if ok_count > 0 {
            CouncilStatus::Completed
        } else {
            CouncilStatus::Failed
        };
        guard.updated_at = Utc::now();
        guard.clone()
    };
    let _ = bus.store().upsert_council(&final_record).await;
    final_record
}

async fn run_seat_round1(
    runtime: Arc<Runtime>,
    bus: Arc<EventBus>,
    shared: Arc<Mutex<CouncilRecord>>,
    council_id: &str,
    seat: &ResolvedSeat,
    question: &str,
    context: Option<&str>,
) -> Option<(usize, AgentId, String)> {
    let started = Instant::now();
    let agent_id = AgentId::new();
    let unique_name = format!(
        "council-{}-{}-{}",
        council_id.trim_start_matches("council-"),
        seat.seat.to_lowercase(),
        &agent_id.to_string()[..4]
    );
    let mut config = if seat.base_config.is_object() {
        seat.base_config.clone()
    } else {
        serde_json::json!({})
    };
    if let Some(ref eff) = seat.effort
        && let Some(obj) = config.as_object_mut()
    {
        obj.insert("effort".into(), serde_json::Value::String(eff.clone()));
    }
    let mut profile = seat.profile.clone();
    profile.title = Some(format!("Council Seat {} · {}", seat.seat, seat.label));

    let agent = Agent {
        id: agent_id,
        name: unique_name,
        name_source: NameSource::default(),
        plugin: seat.plugin.clone(),
        driver: None,
        model: seat.model.clone(),
        role: Role::Worker,
        cwd: seat.cwd.clone(),
        home: seat.cwd.clone(),
        node: None,
        config,
        status: AgentStatus::Stopped,
        profile,
        topic: Some(format!("Council: {}", first_chars(question, 48))),
        archived_at: None,
        created_at: Utc::now(),
    };

    if let Err(err) = bus.store().insert_agent(&agent).await {
        record_seat_error(&bus, &shared, seat.index, None, format!("create seat agent failed: {err}"), started).await;
        return None;
    }

    // Record spawned agent_id immediately so cleanup always archives it even on failure/timeout.
    {
        let mut guard = shared.lock().await;
        if let Some(m) = guard.members.get_mut(seat.index) {
            m.agent_id = Some(agent_id);
        }
    }

    let prompt = build_round1_prompt(question, context, seat.perspective.as_deref());
    match run_single_turn(&runtime, &bus, agent_id, prompt).await {
        Ok(reply) => {
            let elapsed = started.elapsed().as_millis() as u64;
            let snapshot = {
                let mut guard = shared.lock().await;
                if let Some(m) = guard.members.get_mut(seat.index) {
                    m.status = "ok".into();
                    m.round1 = Some(reply.clone());
                    m.duration_ms = Some(elapsed);
                }
                guard.updated_at = Utc::now();
                guard.clone()
            };
            let _ = bus.store().upsert_council(&snapshot).await;
            Some((seat.index, agent_id, reply))
        }
        Err(err) => {
            record_seat_error(&bus, &shared, seat.index, Some(agent_id), err, started).await;
            None
        }
    }
}

async fn record_seat_error(
    bus: &EventBus,
    shared: &Mutex<CouncilRecord>,
    idx: usize,
    agent_id: Option<AgentId>,
    error: String,
    started: Instant,
) {
    let snapshot = {
        let mut guard = shared.lock().await;
        if let Some(m) = guard.members.get_mut(idx) {
            if let Some(id) = agent_id {
                m.agent_id = Some(id);
            }
            m.status = "error".into();
            m.error = Some(error);
            m.duration_ms = Some(started.elapsed().as_millis() as u64);
        }
        guard.updated_at = Utc::now();
        guard.clone()
    };
    let _ = bus.store().upsert_council(&snapshot).await;
}

/// Sends one message to `agent_id` and collects its full text reply until the turn completes,
/// errors, or times out. Subscribes to `EventBus` BEFORE sending so no chunks are lost.
async fn run_single_turn(
    runtime: &Runtime,
    bus: &EventBus,
    agent_id: AgentId,
    prompt: String,
) -> Result<String, String> {
    let mut rx = bus.subscribe();
    let delivery = runtime
        .send(&AgentKey::Id(agent_id), prompt, MessageSource::Human)
        .await
        .map_err(|e| e.to_string())?;
    let target_msg = delivery.message_id;

    let result = tokio::time::timeout(TURN_TIMEOUT, async {
        let mut out = String::new();
        let mut last_error: Option<String> = None;
        loop {
            match rx.recv().await {
                Ok(ev) => match ev.kind {
                    EventKind::AgentOutput {
                        id,
                        stream: OutputStream::Text,
                        text,
                    } if id == agent_id => {
                        out.push_str(&text);
                    }
                    EventKind::AgentError { id, message } if id == agent_id => {
                        last_error = Some(message);
                    }
                    EventKind::AgentTurnCompleted {
                        id,
                        message_id,
                        stop_reason,
                    } if id == agent_id && message_id == target_msg => {
                        let trimmed = out.trim().to_owned();
                        if matches!(stop_reason, StopReason::Error | StopReason::Cancelled)
                            && trimmed.is_empty()
                        {
                            return Err(last_error.unwrap_or_else(|| {
                                format!("turn ended with {:?}", stop_reason)
                            }));
                        }
                        if trimmed.is_empty() {
                            return Err(last_error
                                .unwrap_or_else(|| "model returned an empty response".into()));
                        }
                        return Ok(trimmed);
                    }
                    _ => {}
                },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err("event bus closed while waiting for council reply".into());
                }
            }
        }
    })
    .await;

    match result {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(err)) => Err(err),
        Err(_) => Err(format!(
            "timed out after {}s waiting for reply",
            TURN_TIMEOUT.as_secs()
        )),
    }
}

pub fn build_round1_prompt(
    question: &str,
    context: Option<&str>,
    perspective: Option<&str>,
) -> String {
    let mut p = String::from(
        "You are participating in an independent Multi-Model Council consultation.\n\
         Give a clear, candid, self-contained answer based on your own analysis. \
         Highlight key trade-offs, blind spots or risks, and end with a concrete recommendation.\n\
         Do NOT modify files or run destructive commands; answer analytically in text.",
    );
    if let Some(angle) = perspective {
        p.push_str("\n\nAssigned focus / perspective:\n");
        p.push_str(angle);
    }
    if let Some(ctx) = context {
        p.push_str("\n\nShared context:\n");
        p.push_str(ctx);
    }
    p.push_str("\n\nQuestion:\n");
    p.push_str(question);
    p
}

pub fn build_round2_prompt(
    question: &str,
    own_idx: usize,
    round1: &[(usize, String, String)],
) -> String {
    let mut p = format!(
        "Round 2 (Cross-Critique) of the Multi-Model Council.\n\
         Original question:\n{question}\n\n\
         Below are the anonymized Round 1 responses from the other council seats:\n"
    );
    for (idx, seat, answer) in round1 {
        if *idx == own_idx {
            continue;
        }
        p.push_str(&format!("\n--- Seat {seat} ---\n{answer}\n"));
    }
    p.push_str(
        "\nReview these peer responses critically:\n\
         1. Where do they catch valid points or risks you missed?\n\
         2. Where do you disagree and why?\n\
         3. State your refined final recommendation concisely.",
    );
    p
}

fn first_chars(s: &str, max: usize) -> String {
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        collapsed
    } else {
        let head: String = collapsed.chars().take(max).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round1_prompt_includes_optional_perspective_and_context() {
        let p = build_round1_prompt(
            "SQLite FTS5 vs tantivy?",
            Some("Desktop app with embedded DB"),
            Some("Focus on binary size & CJK search"),
        );
        assert!(p.contains("SQLite FTS5 vs tantivy?"));
        assert!(p.contains("Desktop app with embedded DB"));
        assert!(p.contains("Focus on binary size & CJK search"));
    }

    #[test]
    fn round2_prompt_anonymizes_and_excludes_own_seat() {
        let r1 = vec![
            (0, "A".into(), "Answer from seat A".into()),
            (1, "B".into(), "Answer from seat B".into()),
            (2, "C".into(), "Answer from seat C".into()),
        ];
        let p = build_round2_prompt("Which architecture?", 1, &r1);
        assert!(p.contains("Seat A"));
        assert!(p.contains("Answer from seat A"));
        assert!(p.contains("Seat C"));
        assert!(!p.contains("Seat B"));
        assert!(!p.contains("Answer from seat B"));
    }
}
