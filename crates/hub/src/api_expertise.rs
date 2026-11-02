//! Expertise profile routes (docs/tasks/EXPERTISE.md §6.6): reading, writing and deleting
//! entries, searching them, the candidates waiting for the person, the card, the maintenance
//! suggestions and the Markdown export. Mounted under the authed router in [`crate::api`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use crossagents_api_types::{
    AcceptCandidateRequest, AcceptCandidateResponse, ApiError, ConflictDecision, ConflictPair,
    ConflictVerdict, DomainBudget, EntryCounts, EntryKind, EntryPair, EntryStatus, ErrorCode,
    ExpertiseCard, ExpertiseEntry, ExpertiseMaintenance, ListCandidatesResponse,
    ListExpertiseResponse, SaveExpertiseRequest, StaleHint,
};
use crossagents_core::{
    Candidate, CandidateStatus, CoreError, OwnerKind, PERSON_OWNER_ID, estimate_tokens,
};
use crossagents_orchestrator::lessons::search_terms;
use serde::Deserialize;

use crate::api::AppState;

/// The judgement budget of one domain (EXPERTISE §4.3).
const JUDGMENT_BUDGET: u32 = 1500;
/// Entries not used for this long are a `stale` suggestion (EXPERTISE §4.4).
const STALE_DAYS: i64 = 60;
/// Below this many results the card says the rate is built on little.
const LOW_SAMPLE: u32 = 10;
/// The window the card compares against (EXPERTISE §4.8).
const COMPARE_DAYS: i64 = 30;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/expertise", get(list_expertise).post(save_expertise))
        .route("/v1/expertise/search", get(search_expertise))
        .route("/v1/expertise/card", get(card))
        .route("/v1/expertise/maintenance", get(maintenance))
        .route("/v1/expertise/export", get(export))
        .route("/v1/expertise/candidates", get(list_candidates))
        .route(
            "/v1/expertise/candidates/{id}/accept",
            post(accept_candidate),
        )
        .route(
            "/v1/expertise/candidates/{id}/reject",
            post(reject_candidate),
        )
        .route(
            "/v1/expertise/{id}",
            get(get_expertise).delete(delete_expertise),
        )
}

// ---------------------------------------------------------------------------------------
// Errors, shaped like the rest of the API

#[derive(Debug)]
struct Refusal(StatusCode, ApiError);
impl Refusal {
    fn bad_request(message: impl Into<String>) -> Self {
        Self(
            StatusCode::BAD_REQUEST,
            ApiError::new(ErrorCode::BadRequest, message),
        )
    }
}

impl From<CoreError> for Refusal {
    fn from(err: CoreError) -> Self {
        let (status, code) = match &err {
            CoreError::NotFound { .. } => (StatusCode::NOT_FOUND, ErrorCode::NotFound),
            CoreError::Conflict(_) | CoreError::ConflictFiles { .. } | CoreError::Busy(_) => {
                (StatusCode::CONFLICT, ErrorCode::Conflict)
            }
            CoreError::InvalidName(_)
            | CoreError::InvalidTransition { .. }
            | CoreError::Unsupported(_) => (StatusCode::BAD_REQUEST, ErrorCode::BadRequest),
            CoreError::Agent(_) => (StatusCode::BAD_GATEWAY, ErrorCode::AgentError),
            CoreError::Storage(_) => {
                tracing::error!(error = %err, "storage error");
                (StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal)
            }
        };
        Self(status, ApiError::new(code, err.to_string()))
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

type ApiResult<T> = Result<T, Refusal>;

// ---------------------------------------------------------------------------------------
// Which profile

/// `owner_kind`/`owner_id` on every route; the person's profile when both are absent.
#[derive(Debug, Default, Deserialize)]
struct ScopeQuery {
    #[serde(default)]
    owner_kind: Option<OwnerKind>,
    #[serde(default)]
    owner_id: Option<String>,
    #[serde(default)]
    domain: Option<String>,
}

fn scope(query: &ScopeQuery) -> ApiResult<(OwnerKind, String)> {
    let kind = query.owner_kind.unwrap_or(OwnerKind::Person);
    match query.owner_id.as_deref().map(str::trim) {
        Some(id) if !id.is_empty() => Ok((kind, id.to_owned())),
        _ => match kind {
            OwnerKind::Person => Ok((OwnerKind::Person, PERSON_OWNER_ID.to_owned())),
            OwnerKind::Project => Err(Refusal::bad_request(
                "a project's profile needs owner_id (the project id)",
            )),
        },
    }
}

/// The entries of the profile, newest first; `domain` narrows it.
async fn profile(
    state: &AppState,
    kind: OwnerKind,
    id: &str,
    domain: Option<&str>,
) -> ApiResult<Vec<ExpertiseEntry>> {
    Ok(state.bus.store().expertise_list(kind, id, domain).await?)
}

/// The usage records of these entries only.
async fn usages_of(
    state: &AppState,
    entries: &[ExpertiseEntry],
) -> ApiResult<Vec<crossagents_core::UsageRecord>> {
    let ids: HashSet<&str> = entries.iter().map(|e| e.id.as_str()).collect();
    let all = state.bus.store().usage_list(None, None).await?;
    Ok(all
        .into_iter()
        .filter(|u| ids.contains(u.entry_id.as_str()))
        .collect())
}

// ---------------------------------------------------------------------------------------
// Entries

async fn list_expertise(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ScopeQuery>,
) -> ApiResult<Json<ListExpertiseResponse>> {
    let (kind, id) = scope(&query)?;
    let entries = profile(&state, kind, &id, query.domain.as_deref()).await?;
    Ok(Json(ListExpertiseResponse { entries }))
}

async fn get_expertise(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<ExpertiseEntry>> {
    Ok(Json(state.bus.store().expertise_get(&id).await?))
}

/// Writes an entry: a new id when the body has none, else the stored entry with that id is
/// replaced. Counters the body cannot set (evidence, stats, sources) are carried over.
async fn save_expertise(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SaveExpertiseRequest>,
) -> ApiResult<(StatusCode, Json<ExpertiseEntry>)> {
    let domain = req.domain.trim().to_owned();
    let title = req.title.trim().to_owned();
    if domain.is_empty() {
        return Err(Refusal::bad_request("domain is required"));
    }
    if title.is_empty() {
        return Err(Refusal::bad_request("title is required"));
    }
    if req.owner_id.trim().is_empty() {
        return Err(Refusal::bad_request("owner_id is required"));
    }

    let now = Utc::now();
    let store = state.bus.store();
    let existing = match &req.id {
        Some(id) => store.expertise_get(id).await.ok(),
        None => None,
    };
    let entry = ExpertiseEntry {
        id: match (&req.id, &existing) {
            (Some(id), _) => id.clone(),
            (None, _) => uuid::Uuid::new_v4().to_string(),
        },
        owner_kind: req.owner_kind,
        owner_id: req.owner_id.trim().to_owned(),
        domain,
        kind: req.kind,
        title,
        text: req.text,
        example: req.example,
        applies_when: req.applies_when,
        evidence: existing.as_ref().map(|e| e.evidence).unwrap_or_default(),
        last_confirmed_at: existing.as_ref().and_then(|e| e.last_confirmed_at),
        status: req
            .status
            .or(existing.as_ref().map(|e| e.status))
            .unwrap_or(EntryStatus::Active),
        superseded_by: req
            .superseded_by
            .or_else(|| existing.as_ref().and_then(|e| e.superseded_by.clone())),
        stats: existing.as_ref().map(|e| e.stats).unwrap_or_default(),
        sources: existing
            .as_ref()
            .map(|e| e.sources.clone())
            .unwrap_or_default(),
        created_at: existing.as_ref().map(|e| e.created_at).unwrap_or(now),
        updated_at: now,
    };
    store.expertise_put(&entry).await?;
    Ok((StatusCode::OK, Json(entry)))
}

async fn delete_expertise(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    state.bus.store().expertise_delete(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    owner_kind: Option<OwnerKind>,
    #[serde(default)]
    owner_id: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

/// Full-text search in one profile; superseded entries are left out by the store.
async fn search_expertise(
    State(state): State<Arc<AppState>>,
    Query(query): Query<SearchQuery>,
) -> ApiResult<Json<ListExpertiseResponse>> {
    let owner = ScopeQuery {
        owner_kind: query.owner_kind,
        owner_id: query.owner_id.clone(),
        domain: None,
    };
    let (kind, id) = scope(&owner)?;
    let terms = search_terms(&query.q);
    if terms.is_empty() {
        return Err(Refusal::bad_request("q is required"));
    }
    let limit = query.limit.unwrap_or(10).clamp(1, 50);
    let entries = state
        .bus
        .store()
        .expertise_search(&[(kind, id)], &terms, limit)
        .await?;
    Ok(Json(ListExpertiseResponse { entries }))
}

// ---------------------------------------------------------------------------------------
// Candidates

#[derive(Debug, Deserialize)]
struct CandidatesQuery {
    #[serde(default)]
    status: Option<CandidateStatus>,
    #[serde(default)]
    owner_kind: Option<OwnerKind>,
    #[serde(default)]
    owner_id: Option<String>,
}

async fn list_candidates(
    State(state): State<Arc<AppState>>,
    Query(query): Query<CandidatesQuery>,
) -> ApiResult<Json<ListCandidatesResponse>> {
    let owner = match (query.owner_kind, &query.owner_id) {
        (Some(kind), Some(id)) if !id.trim().is_empty() => Some((kind, id.trim().to_owned())),
        (Some(OwnerKind::Person), None) => Some((OwnerKind::Person, PERSON_OWNER_ID.to_owned())),
        (None, Some(id)) if !id.trim().is_empty() => {
            Some((OwnerKind::Person, id.trim().to_owned()))
        }
        _ => None,
    };
    let candidates = state
        .bus
        .store()
        .candidate_list(owner, query.status)
        .await?;
    Ok(Json(ListCandidatesResponse { candidates }))
}

async fn find_candidate(state: &AppState, id: &str) -> ApiResult<Candidate> {
    let candidates = state.bus.store().candidate_list(None, None).await?;
    candidates.into_iter().find(|c| c.id == id).ok_or_else(|| {
        Refusal::from(CoreError::NotFound {
            kind: "expertise candidate",
            id: id.to_owned(),
        })
    })
}

/// Accepts a candidate: the proposed entry is added as a new entry, and every entry in
/// `conflicts_with` follows the verdict the person gave. Any conflict without a verdict
/// refuses the whole call with 400 and changes nothing (EXPERTISE §4.4).
async fn accept_candidate(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<AcceptCandidateRequest>,
) -> ApiResult<Json<AcceptCandidateResponse>> {
    let store = state.bus.store();
    let mut candidate = find_candidate(&state, &id).await?;
    if candidate.status != CandidateStatus::Pending {
        return Err(Refusal::from(CoreError::Conflict(format!(
            "candidate {id} was already decided"
        ))));
    }
    let verdicts = verdicts(&candidate, &req.decisions)?;

    let now = Utc::now();
    let mut entry = candidate.proposed.clone();
    entry.id = uuid::Uuid::new_v4().to_string();
    entry.owner_kind = candidate.owner_kind;
    entry.owner_id = candidate.owner_id.clone();
    entry.domain = candidate.domain.clone();
    entry.status = EntryStatus::Active;
    entry.superseded_by = None;
    entry.created_at = now;
    entry.updated_at = now;
    let sources = merged_sources(&candidate, &entry);
    entry.sources = sources;
    entry.stats = Default::default();
    entry.evidence = 0;
    entry.last_confirmed_at = None;

    for (conflict_id, verdict) in &verdicts {
        let Ok(mut old) = store.expertise_get(conflict_id).await else {
            // A conflict that is already gone has nothing to rule on.
            continue;
        };
        match verdict {
            ConflictVerdict::KeepOld | ConflictVerdict::Both => {}
            ConflictVerdict::Replace | ConflictVerdict::Merge => {
                if *verdict == ConflictVerdict::Merge && !old.text.trim().is_empty() {
                    let kept = old.text.trim().to_owned();
                    if !entry.text.contains(&kept) {
                        entry.text = format!("{}\n\n{kept}", entry.text.trim_end());
                    }
                }
                old.status = EntryStatus::Superseded;
                old.superseded_by = Some(entry.id.clone());
                old.updated_at = now;
                store.expertise_put(&old).await?;
            }
        }
    }

    store.expertise_put(&entry).await?;
    store
        .candidate_set_status(&candidate.id, CandidateStatus::Accepted)
        .await?;
    candidate.status = CandidateStatus::Accepted;
    candidate.decided_at = Some(now);
    Ok(Json(AcceptCandidateResponse { candidate, entry }))
}

/// One verdict per conflicting entry, in the candidate's order; a missing one is a 400.
fn verdicts(
    candidate: &Candidate,
    decisions: &[ConflictDecision],
) -> ApiResult<Vec<(String, ConflictVerdict)>> {
    let given: HashMap<&str, ConflictVerdict> = decisions
        .iter()
        .map(|d| (d.entry_id.as_str(), d.verdict))
        .collect();
    let mut ordered = Vec::new();
    for conflict in &candidate.conflicts_with {
        match given.get(conflict.as_str()) {
            Some(verdict) => ordered.push((conflict.clone(), *verdict)),
            None => {
                return Err(Refusal::bad_request(format!(
                    "no verdict for conflict {conflict}: one of keep_old, replace, merge, both is required"
                )));
            }
        }
    }
    Ok(ordered)
}

/// Where the new entry came from: the candidate's sources and the proposed entry's, once each.
fn merged_sources(
    candidate: &Candidate,
    proposed: &ExpertiseEntry,
) -> Vec<crossagents_core::SourceRef> {
    let mut sources = candidate.sources.clone();
    for source in &proposed.sources {
        if !sources.contains(source) {
            sources.push(source.clone());
        }
    }
    sources
}

async fn reject_candidate(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Candidate>> {
    let store = state.bus.store();
    let mut candidate = find_candidate(&state, &id).await?;
    if candidate.status != CandidateStatus::Pending {
        return Err(Refusal::from(CoreError::Conflict(format!(
            "candidate {id} was already decided"
        ))));
    }
    store
        .candidate_set_status(&id, CandidateStatus::Rejected)
        .await?;
    candidate.status = CandidateStatus::Rejected;
    candidate.decided_at = Some(Utc::now());
    Ok(Json(candidate))
}

// ---------------------------------------------------------------------------------------
// Card, maintenance, export

async fn card(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ScopeQuery>,
) -> ApiResult<Json<ExpertiseCard>> {
    let (kind, id) = scope(&query)?;
    let entries = profile(&state, kind, &id, query.domain.as_deref()).await?;
    let usages = usages_of(&state, &entries).await?;
    Ok(Json(expertise_card(&entries, &usages, Utc::now())))
}

async fn maintenance(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ScopeQuery>,
) -> ApiResult<Json<ExpertiseMaintenance>> {
    let (kind, id) = scope(&query)?;
    let entries = profile(&state, kind, &id, query.domain.as_deref()).await?;
    let usages = usages_of(&state, &entries).await?;
    let pending = state
        .bus
        .store()
        .candidate_list(Some((kind, id.clone())), Some(CandidateStatus::Pending))
        .await?;
    Ok(Json(ExpertiseMaintenance {
        budgets: budget_report(&entries),
        stale: stale_scan(&entries, &usages, Utc::now()),
        conflicts: conflict_list(&pending),
    }))
}

/// The profile as Markdown, for a person to read or paste elsewhere.
async fn export(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ScopeQuery>,
) -> ApiResult<Response> {
    let (kind, id) = scope(&query)?;
    let entries = profile(&state, kind, &id, query.domain.as_deref()).await?;
    Ok((
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        export_markdown(&entries),
    )
        .into_response())
}

// ---------------------------------------------------------------------------------------
// Maintenance views
//
// Temporary: `crates/orchestrator/src/expertise/maintain.rs` (task exp-maintain, EXPERTISE
// §6.5) is still empty, so the same five functions live here, over the API's own result
// types. When exp-maintain merges, these go and the handlers call
// `crossagents_orchestrator::expertise::maintain` instead.

/// Judgement tokens per domain, and pairs that look like they say the same thing.
fn budget_report(entries: &[ExpertiseEntry]) -> Vec<DomainBudget> {
    let mut domains: BTreeMap<&str, Vec<&ExpertiseEntry>> = BTreeMap::new();
    for entry in entries
        .iter()
        .filter(|e| e.kind == EntryKind::Judgment && e.status != EntryStatus::Superseded)
    {
        domains
            .entry(entry.domain.as_str())
            .or_default()
            .push(entry);
    }
    domains
        .into_iter()
        .map(|(domain, list)| {
            let tokens: u32 = list
                .iter()
                .map(|e| (estimate_tokens(&e.title) + estimate_tokens(&e.text)) as u32)
                .sum();
            let mut pairs = Vec::new();
            for (i, a) in list.iter().enumerate() {
                for b in &list[i + 1..] {
                    if looks_alike(a, b) {
                        pairs.push(EntryPair {
                            a: a.id.clone(),
                            b: b.id.clone(),
                        });
                    }
                }
            }
            DomainBudget {
                domain: domain.to_owned(),
                tokens,
                budget: JUDGMENT_BUDGET,
                over: tokens.saturating_sub(JUDGMENT_BUDGET),
                merge_candidates: pairs,
            }
        })
        .collect()
}

/// The same `applies_when` (or one covering the other), and a word the titles share.
fn looks_alike(a: &ExpertiseEntry, b: &ExpertiseEntry) -> bool {
    let (one, other) = (a.applies_when.trim(), b.applies_when.trim());
    let reach = one.is_empty() || other.is_empty() || one.contains(other) || other.contains(one);
    reach && shares_word(&a.title, &b.title)
}

fn shares_word(a: &str, b: &str) -> bool {
    let words = |text: &str| -> Vec<String> {
        text.split(|c: char| {
            c.is_whitespace()
                || c.is_ascii_punctuation()
                || matches!(c, '，' | '。' | '、' | '：' | '；' | '（' | '）')
        })
        .filter(|w| w.chars().count() >= 2)
        .map(str::to_owned)
        .collect()
    };
    let theirs = words(b);
    words(a).iter().any(|w| theirs.contains(w))
}

/// Entries unused for a long time, or whose results got worse; nothing is changed here.
fn stale_scan(
    entries: &[ExpertiseEntry],
    usages: &[crossagents_core::UsageRecord],
    now: DateTime<Utc>,
) -> Vec<StaleHint> {
    let mut hints = Vec::new();
    for entry in entries.iter().filter(|e| e.status == EntryStatus::Active) {
        let used = entry.stats.used;
        let reworked = entry.stats.reworked;
        if used > 0 && reworked * 10 >= used * 4 {
            hints.push(StaleHint {
                entry_id: entry.id.clone(),
                suggested: EntryStatus::Review,
                reason: format!("used {used} times, reworked {reworked}"),
            });
            continue;
        }
        let last = usages
            .iter()
            .filter(|u| u.entry_id == entry.id)
            .map(|u| u.at)
            .chain(entry.last_confirmed_at)
            .max()
            .unwrap_or(entry.updated_at);
        let idle = now.signed_duration_since(last).num_days();
        if idle >= STALE_DAYS {
            hints.push(StaleHint {
                entry_id: entry.id.clone(),
                suggested: EntryStatus::Stale,
                reason: format!("not used for {idle} days"),
            });
        }
    }
    hints
}

/// Pending candidates that need a verdict per conflicting entry.
fn conflict_list(candidates: &[Candidate]) -> Vec<ConflictPair> {
    candidates
        .iter()
        .filter(|c| c.status == CandidateStatus::Pending && !c.conflicts_with.is_empty())
        .map(|c| ConflictPair {
            candidate_id: c.id.clone(),
            entry_ids: c.conflicts_with.clone(),
        })
        .collect()
}

fn expertise_card(
    entries: &[ExpertiseEntry],
    usages: &[crossagents_core::UsageRecord],
    now: DateTime<Utc>,
) -> ExpertiseCard {
    let live: Vec<&ExpertiseEntry> = entries
        .iter()
        .filter(|e| e.status != EntryStatus::Superseded)
        .collect();
    let count = |kind: EntryKind| live.iter().filter(|e| e.kind == kind).count() as u32;
    let budgets = budget_report(entries);
    let judgment_tokens: u32 = live
        .iter()
        .filter(|e| e.kind == EntryKind::Judgment)
        .map(|e| (estimate_tokens(&e.title) + estimate_tokens(&e.text)) as u32)
        .sum();
    let sum = |pick: fn(&crossagents_core::EntryStats) -> u32| -> u32 {
        entries.iter().map(|e| pick(&e.stats)).sum()
    };
    let (merged, reworked, dropped) = (
        sum(|s| s.merged_first_try),
        sum(|s| s.reworked),
        sum(|s| s.dropped),
    );
    let first_try_rate = rate(merged, reworked, dropped);

    // The same count over the usage that already existed 30 days ago (EXPERTISE §4.8).
    let before = now - Duration::days(COMPARE_DAYS);
    let (mut old_merged, mut old_reworked, mut old_dropped) = (0, 0, 0);
    for usage in usages.iter().filter(|u| u.at < before) {
        match usage.outcome {
            Some(crossagents_core::UsageOutcome::MergedFirstTry) => old_merged += 1,
            Some(crossagents_core::UsageOutcome::Reworked) => old_reworked += 1,
            Some(crossagents_core::UsageOutcome::Dropped) => old_dropped += 1,
            None => {}
        }
    }
    let rate_30d = rate(old_merged, old_reworked, old_dropped);
    let delta_points = match (first_try_rate, rate_30d) {
        (Some(now), Some(then)) => Some((now - then) * 100.0),
        _ => None,
    };

    ExpertiseCard {
        entries: EntryCounts {
            judgment: count(EntryKind::Judgment),
            example: count(EntryKind::Example),
            limit: count(EntryKind::Limit),
        },
        judgment_tokens,
        judgment_budget: JUDGMENT_BUDGET,
        over_budget_domains: budgets.iter().filter(|b| b.over > 0).count() as u32,
        stale: live
            .iter()
            .filter(|e| e.status == EntryStatus::Stale)
            .count() as u32,
        review: live
            .iter()
            .filter(|e| e.status == EntryStatus::Review)
            .count() as u32,
        used: sum(|s| s.used),
        merged_first_try: merged,
        reworked,
        dropped,
        first_try_rate,
        first_try_rate_30d_ago: rate_30d,
        delta_points,
        low_sample: merged + reworked + dropped < LOW_SAMPLE,
    }
}

fn rate(merged: u32, reworked: u32, dropped: u32) -> Option<f64> {
    let total = merged + reworked + dropped;
    (total > 0).then(|| f64::from(merged) / f64::from(total))
}

/// The profile as Markdown: a heading per domain, one section per kind.
fn export_markdown(entries: &[ExpertiseEntry]) -> String {
    let mut out = String::new();
    match entries.first() {
        Some(first) => out.push_str(&format!(
            "# Expertise profile: {} {}\n",
            owner_word(first.owner_kind),
            first.owner_id
        )),
        None => out.push_str("# Expertise profile\n"),
    }
    let live: Vec<&ExpertiseEntry> = entries
        .iter()
        .filter(|e| e.status != EntryStatus::Superseded)
        .collect();
    if live.is_empty() {
        out.push_str("\n(no entries yet)\n");
        return out;
    }

    let mut domains: BTreeMap<&str, Vec<&ExpertiseEntry>> = BTreeMap::new();
    for entry in &live {
        domains
            .entry(entry.domain.as_str())
            .or_default()
            .push(entry);
    }
    for (domain, list) in domains {
        out.push_str(&format!("\n## {domain}\n"));
        let mut tokens = 0u32;
        for (kind, heading) in [
            (EntryKind::Judgment, "Judgments"),
            (EntryKind::Example, "Examples"),
            (EntryKind::Limit, "Limits"),
        ] {
            let group: Vec<&&ExpertiseEntry> = list.iter().filter(|e| e.kind == kind).collect();
            if group.is_empty() {
                continue;
            }
            out.push_str(&format!("\n### {heading}\n\n"));
            for entry in group {
                if kind == EntryKind::Judgment {
                    tokens += (estimate_tokens(&entry.title) + estimate_tokens(&entry.text)) as u32;
                }
                out.push_str(&format!("- **{}**", entry.title));
                if let Some(note) = status_note(entry) {
                    out.push_str(&format!(" ({note})"));
                }
                out.push('\n');
                if !entry.applies_when.trim().is_empty() {
                    out.push_str(&format!(
                        "  - applies when: {}\n",
                        entry.applies_when.trim()
                    ));
                }
                match &entry.example {
                    Some(example) if kind == EntryKind::Example => {
                        for (label, value) in [
                            ("situation", &example.situation),
                            ("rejected", &example.rejected),
                            ("why", &example.rejected_why),
                            ("chosen", &example.chosen),
                        ] {
                            if !value.trim().is_empty() {
                                out.push_str(&format!("  - {label}: {}\n", value.trim()));
                            }
                        }
                    }
                    _ => {
                        if !entry.text.trim().is_empty() {
                            out.push_str(&format!("  - {}\n", one_line(&entry.text)));
                        }
                    }
                }
            }
        }
        if tokens > 0 {
            out.push_str(&format!(
                "\n_judgement budget: {tokens} / {JUDGMENT_BUDGET} tokens_\n"
            ));
        }
    }
    out
}

fn owner_word(kind: OwnerKind) -> &'static str {
    match kind {
        OwnerKind::Person => "person",
        OwnerKind::Project => "project",
    }
}

/// The warning a person needs next to the title, if any.
fn status_note(entry: &ExpertiseEntry) -> Option<String> {
    let mut notes = Vec::new();
    match entry.status {
        EntryStatus::Stale => notes.push("stale".to_owned()),
        EntryStatus::Review => notes.push("under review".to_owned()),
        EntryStatus::Active | EntryStatus::Superseded => {}
    }
    if entry.stats.used > 0 {
        notes.push(format!(
            "used {}, merged first try {}, reworked {}, dropped {}",
            entry.stats.used,
            entry.stats.merged_first_try,
            entry.stats.reworked,
            entry.stats.dropped
        ));
    }
    (!notes.is_empty()).then(|| notes.join("; "))
}

/// The body on one line, for a bullet.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossagents_core::{EntryStats, SourceRef, TaskId};

    fn entry(
        id: &str,
        kind: EntryKind,
        status: EntryStatus,
        title: &str,
        text: &str,
    ) -> ExpertiseEntry {
        let now = Utc::now();
        ExpertiseEntry {
            id: id.into(),
            owner_kind: OwnerKind::Person,
            owner_id: PERSON_OWNER_ID.into(),
            domain: "rust".into(),
            kind,
            title: title.into(),
            text: text.into(),
            example: None,
            applies_when: String::new(),
            evidence: 0,
            last_confirmed_at: None,
            status,
            superseded_by: None,
            stats: EntryStats::default(),
            sources: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    fn candidate(id: &str, conflicts: &[&str]) -> Candidate {
        let now = Utc::now();
        Candidate {
            id: id.into(),
            owner_kind: OwnerKind::Person,
            owner_id: PERSON_OWNER_ID.into(),
            domain: "rust".into(),
            proposed: entry("p", EntryKind::Judgment, EntryStatus::Active, "t", "body"),
            conflicts_with: conflicts.iter().map(|c| (*c).to_owned()).collect(),
            origin: crossagents_core::CandidateOrigin::Manual,
            status: CandidateStatus::Pending,
            suggested_by_lead: false,
            note: String::new(),
            sources: vec![SourceRef::Run {
                run_id: crossagents_core::RunId::new(),
            }],
            created_at: now,
            decided_at: None,
        }
    }

    #[test]
    fn expertise_budget_adds_up_per_domain_and_flags_the_overflow() {
        let long = "先写一个会失败的测试".repeat(90); // 900 tokens
        let mut entries = vec![
            entry(
                "e1",
                EntryKind::Judgment,
                EntryStatus::Active,
                "测试先于实现",
                &long,
            ),
            entry(
                "e2",
                EntryKind::Judgment,
                EntryStatus::Active,
                "别过度设计",
                &long,
            ),
            // Examples do not count against the judgement budget.
            entry(
                "e3",
                EntryKind::Example,
                EntryStatus::Active,
                "某个例子",
                &long,
            ),
            entry(
                "e4",
                EntryKind::Judgment,
                EntryStatus::Active,
                "旧判断",
                &long,
            ),
        ];
        entries[3].status = EntryStatus::Superseded;
        let budgets = budget_report(&entries);
        assert_eq!(budgets.len(), 1);
        assert_eq!(budgets[0].domain, "rust");
        assert_eq!(budgets[0].budget, JUDGMENT_BUDGET);
        assert!(budgets[0].tokens > JUDGMENT_BUDGET);
        assert_eq!(
            budgets[0].over,
            budgets[0].tokens.saturating_sub(JUDGMENT_BUDGET)
        );
    }

    #[test]
    fn expertise_stale_scan_flags_long_unused_and_reworked() {
        let mut old = entry(
            "e1",
            EntryKind::Judgment,
            EntryStatus::Active,
            "旧判断",
            "正文",
        );
        old.updated_at = Utc::now() - Duration::days(STALE_DAYS + 1);
        let hints = stale_scan(&[old], &[], Utc::now());
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].suggested, EntryStatus::Stale);

        let mut bad = entry(
            "e2",
            EntryKind::Judgment,
            EntryStatus::Active,
            "常返工",
            "正文",
        );
        bad.stats = EntryStats {
            used: 5,
            reworked: 3,
            ..EntryStats::default()
        };
        let hints = stale_scan(&[bad], &[], Utc::now());
        assert_eq!(hints[0].suggested, EntryStatus::Review);
    }

    #[test]
    fn expertise_every_conflict_needs_a_verdict() {
        let candidate = candidate("c1", &["e1", "e2"]);
        let missing = verdicts(
            &candidate,
            &[ConflictDecision {
                entry_id: "e1".into(),
                verdict: ConflictVerdict::KeepOld,
            }],
        );
        assert!(missing.is_err());

        let all = verdicts(
            &candidate,
            &[
                ConflictDecision {
                    entry_id: "e2".into(),
                    verdict: ConflictVerdict::Merge,
                },
                ConflictDecision {
                    entry_id: "e1".into(),
                    verdict: ConflictVerdict::Replace,
                },
            ],
        )
        .unwrap();
        // The candidate's order, not the body's.
        assert_eq!(all[0].0, "e1");
        assert_eq!(all[1].0, "e2");
    }

    #[test]
    fn expertise_conflict_list_is_pending_with_conflicts() {
        let mut settled = candidate("c2", &["e1"]);
        settled.status = CandidateStatus::Accepted;
        let pairs = conflict_list(&[
            candidate("c1", &["e1", "e2"]),
            settled,
            candidate("c3", &[]),
        ]);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].entry_ids, vec!["e1", "e2"]);
    }

    #[test]
    fn expertise_card_is_null_without_results_and_compares_30_days_ago() {
        let mut used = entry(
            "e1",
            EntryKind::Judgment,
            EntryStatus::Active,
            "判断",
            "正文",
        );
        used.stats = EntryStats {
            used: 3,
            merged_first_try: 2,
            reworked: 1,
            dropped: 0,
        };
        let card = expertise_card(&[used.clone()], &[], Utc::now());
        assert_eq!(card.entries.judgment, 1);
        assert_eq!(card.first_try_rate, Some(2.0 / 3.0));
        assert_eq!(card.first_try_rate_30d_ago, None);
        assert_eq!(card.delta_points, None);
        assert!(card.low_sample);

        let old = Utc::now() - Duration::days(COMPARE_DAYS + 1);
        let usage = |outcome: crossagents_core::UsageOutcome| crossagents_core::UsageRecord {
            id: uuid::Uuid::new_v4().to_string(),
            entry_id: "e1".into(),
            run_id: None,
            task_id: Some(TaskId::new()),
            agent: None,
            source: crossagents_core::UsageSource::Declared,
            outcome: Some(outcome),
            at: old,
        };
        let usages = vec![
            usage(crossagents_core::UsageOutcome::MergedFirstTry),
            usage(crossagents_core::UsageOutcome::Reworked),
        ];
        let card = expertise_card(&[used], &usages, Utc::now());
        assert_eq!(card.first_try_rate_30d_ago, Some(0.5));
        assert_eq!(card.delta_points, Some((2.0 / 3.0 - 0.5) * 100.0));
    }

    #[test]
    fn expertise_export_sections_by_domain_and_kind() {
        let mut example = entry(
            "e2",
            EntryKind::Example,
            EntryStatus::Active,
            "一次取舍",
            "",
        );
        example.applies_when = "crates/hub/**".into();
        example.example = Some(crossagents_core::Example {
            situation: "要给路由加鉴权".into(),
            rejected: "每个 handler 各写一遍".into(),
            rejected_why: "漏掉一个就开口子".into(),
            chosen: "中间件".into(),
        });
        let mut gone = entry(
            "e3",
            EntryKind::Limit,
            EntryStatus::Superseded,
            "旧边界",
            "x",
        );
        gone.superseded_by = Some("e2".into());
        let markdown = export_markdown(&[
            entry(
                "e1",
                EntryKind::Judgment,
                EntryStatus::Active,
                "测试先行",
                "先红后绿",
            ),
            example,
            gone,
        ]);
        assert!(markdown.starts_with("# Expertise profile: person me"));
        assert!(markdown.contains("## rust"));
        assert!(markdown.contains("### Judgments"));
        assert!(markdown.contains("- **测试先行**"));
        assert!(markdown.contains("### Examples"));
        assert!(markdown.contains("chosen: 中间件"));
        assert!(!markdown.contains("旧边界"));

        assert!(export_markdown(&[]).contains("(no entries yet)"));
    }
}
