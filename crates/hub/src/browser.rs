//! The built-in browser's GUI side: the routes the CrossAgents window uses to take an
//! agent's request and answer it (docs/BROWSER.md, run note `browser/contract` §2).
//!
//! The requests themselves, their timeout and what the model sees are the orchestrator's
//! (`crossagents_orchestrator::browser`); these routes only move a claim and an answer into
//! that table. Mounted under the authed router in [`crate::api`].

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::post;
use axum::{Extension, Json, Router};
use crossagents_api_types::{BrowserClaimResponse, BrowserResultRequest};
use crossagents_orchestrator::browser::Answer;

use crate::access::Grant;
use crate::api::{ApiResult, AppState};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/browser/requests/{id}/claim", post(claim))
        .route("/v1/browser/requests/{id}/result", post(result))
}

/// Who took the request, for the 409 that names it: the GUI's device, or the person.
fn claimer(grant: &Grant) -> Option<String> {
    match grant {
        Grant::Device { id, name, .. } => Some(format!("{name} ({id})")),
        Grant::Level(_) => Some("the person".into()),
        Grant::Agent | Grant::Extension { .. } => None,
    }
}

async fn claim(
    State(state): State<Arc<AppState>>,
    Extension(grant): Extension<Grant>,
    Path(id): Path<String>,
) -> ApiResult<Json<BrowserClaimResponse>> {
    state.orchestrator.browser_claim(&id, claimer(&grant))?;
    Ok(Json(BrowserClaimResponse { ok: true }))
}

async fn result(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<BrowserResultRequest>,
) -> ApiResult<Json<BrowserClaimResponse>> {
    state.orchestrator.browser_answer(
        &id,
        Answer {
            ok: req.ok,
            output: req.output,
            error: req.error,
        },
    )?;
    Ok(Json(BrowserClaimResponse { ok: true }))
}
