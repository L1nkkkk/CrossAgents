//! Expertise profile routes (docs/tasks/EXPERTISE.md §6.6): reading, writing and deleting
//! entries, searching them, the candidates waiting for the person, the card, the maintenance
//! suggestions and the Markdown export. Mounted under the authed router in [`crate::api`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use crossagents_api_types::{
    AcceptCandidateRequest, AcceptCandidateResponse, ApiError, ConflictDecision, ConflictPair,
    ConflictVerdict, DomainBudget, EntryStatus, ErrorCode, ExpertiseCard, ExpertiseEntry,
    ExpertiseMaintenance, ListCandidatesResponse, ListExpertiseResponse, MergeSuggestion,
    SaveExpertiseRequest, StaleHint,
};
use crossagents_core::{Candidate, CandidateStatus, CoreError, OwnerKind, PERSON_OWNER_ID};
use crossagents_orchestrator::expertise::maintain;
use crossagents_orchestrator::lessons::search_terms;
use serde::Deserialize;

use crate::api::AppState;

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
            CoreError::OutcomeUnknown(_) => {
                (StatusCode::GATEWAY_TIMEOUT, ErrorCode::OutcomeUnknown)
            }
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
// Card, maintenance, export (the views of `crossagents_orchestrator::expertise::maintain`)

async fn card(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ScopeQuery>,
) -> ApiResult<Json<ExpertiseCard>> {
    let (kind, id) = scope(&query)?;
    let entries = profile(&state, kind, &id, query.domain.as_deref()).await?;
    let usages = usages_of(&state, &entries).await?;
    Ok(Json(card_from(maintain::expertise_card(
        &entries,
        &usages,
        Utc::now(),
    ))))
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
        budgets: maintain::budget_report(&entries)
            .into_iter()
            .map(budget_from)
            .collect(),
        stale: maintain::stale_scan(&entries, &usages, Utc::now())
            .into_iter()
            .map(hint_from)
            .collect(),
        conflicts: maintain::conflict_list(&pending)
            .into_iter()
            .map(conflict_from)
            .collect(),
    }))
}

/// The profile as Markdown, for a person to read or paste elsewhere. Empty when the profile
/// has no entries: `export_markdown` says so by returning nothing.
async fn export(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ScopeQuery>,
) -> ApiResult<Response> {
    let (kind, id) = scope(&query)?;
    let entries = profile(&state, kind, &id, query.domain.as_deref()).await?;
    Ok((
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        maintain::export_markdown(&entries),
    )
        .into_response())
}

// The maintain types belong to the orchestrator and carry no bindings derives, so the hub
// carries them onto the API response shapes field by field, in the same order. Not `impl
// From`: both types are foreign to this crate, so the orphan rule rules it out. The counts
// are narrowed to `u32`, which is all a profile of this size needs.

fn budget_from(budget: maintain::DomainBudget) -> DomainBudget {
    DomainBudget {
        domain: budget.domain,
        judgment_tokens: budget.judgment_tokens as u32,
        budget: budget.budget as u32,
        over: budget.over,
        over_by: budget.over_by as u32,
        judgment_count: budget.judgment_count as u32,
        merge_suggestions: budget
            .merge_suggestions
            .into_iter()
            .map(|pair| MergeSuggestion {
                a: pair.a,
                b: pair.b,
                reason: pair.reason,
            })
            .collect(),
    }
}

fn hint_from(hint: maintain::StaleHint) -> StaleHint {
    StaleHint {
        entry_id: hint.entry_id,
        domain: hint.domain,
        title: hint.title,
        suggested: hint.suggested,
        reason: hint.reason,
        days_unused: hint.days_unused as i32,
        rework_rate: hint.rework_rate,
    }
}

fn conflict_from(pair: maintain::ConflictPair) -> ConflictPair {
    ConflictPair {
        candidate_id: pair.candidate_id,
        entry_id: pair.entry_id,
        domain: pair.domain,
        title: pair.title,
        note: pair.note,
    }
}

fn card_from(card: maintain::ExpertiseCard) -> ExpertiseCard {
    ExpertiseCard {
        first_try_rate: card.first_try_rate,
        first_try_rate_30d_ago: card.first_try_rate_30d_ago,
        delta_points: card.delta_points,
        sample: card.sample as u32,
        small_sample: card.small_sample,
        merged_first_try: card.merged_first_try as u32,
        reworked: card.reworked as u32,
        dropped: card.dropped as u32,
        entries: card.entries as u32,
        judgments: card.judgments as u32,
        examples: card.examples as u32,
        limits: card.limits as u32,
        active: card.active as u32,
        stale: card.stale as u32,
        review: card.review as u32,
        superseded: card.superseded as u32,
        judgment_tokens: card.judgment_tokens as u32,
        judgment_budget: card.judgment_budget as u32,
        over_budget_domains: card.over_budget_domains as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossagents_core::{EntryKind, EntryStats, SourceRef};

    fn candidate(conflicts: &[&str]) -> Candidate {
        let now = Utc::now();
        Candidate {
            id: "c1".into(),
            owner_kind: OwnerKind::Person,
            owner_id: PERSON_OWNER_ID.into(),
            domain: "rust".into(),
            proposed: ExpertiseEntry {
                id: "p".into(),
                owner_kind: OwnerKind::Person,
                owner_id: PERSON_OWNER_ID.into(),
                domain: "rust".into(),
                kind: EntryKind::Judgment,
                title: "t".into(),
                text: "body".into(),
                example: None,
                applies_when: String::new(),
                evidence: 0,
                last_confirmed_at: None,
                status: EntryStatus::Active,
                superseded_by: None,
                stats: EntryStats::default(),
                sources: vec![SourceRef::Run {
                    run_id: crossagents_core::RunId::new(),
                }],
                created_at: now,
                updated_at: now,
            },
            conflicts_with: conflicts.iter().map(|c| (*c).to_owned()).collect(),
            origin: crossagents_core::CandidateOrigin::Manual,
            status: CandidateStatus::Pending,
            suggested_by_lead: false,
            note: String::new(),
            sources: Vec::new(),
            created_at: now,
            decided_at: None,
        }
    }

    #[test]
    fn expertise_every_conflict_needs_a_verdict() {
        let candidate = candidate(&["e1", "e2"]);
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
        // The candidate order, not the body order.
        assert_eq!(all[0].0, "e1");
        assert_eq!(all[1].0, "e2");
    }
}
