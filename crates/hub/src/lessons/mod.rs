//! Troubleshooting record routes (docs/tasks/LESSONS.md L2): a project's lessons, listed,
//! searched, read, edited and exported as Markdown, and the problems recorded in a run with
//! what became of each. Mounted under the authed router in [`crate::api`].

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;
use crossagents_api_types::{
    Lesson, LessonStatus, ListLessonsResponse, ListRunIssuesResponse, RunIssue,
    UpdateLessonRequest,
};
use crossagents_core::{CoreError, EventKind, ISSUE_PREFIX, NoteScope};
use crossagents_orchestrator::lessons::{export_markdown, search_terms};
use serde::Deserialize;

use crate::api::{ApiResult, AppError, AppState, resolve_run};

/// Most lessons a search returns.
const SEARCH_LIMIT: usize = 50;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/projects/{id}/lessons", get(list_lessons))
        .route("/v1/projects/{id}/lessons/export", get(export))
        .route("/v1/lessons/{id}", get(get_lesson).patch(update_lesson))
        .route("/v1/runs/{key}/issues", get(run_issues))
}

#[derive(Debug, Default, Deserialize)]
struct ListQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    status: Option<LessonStatus>,
}

/// The project's lessons, newest first; with `q`, those matching any of its words, best
/// first; with `status`, only those in it.
async fn list_lessons(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<ListLessonsResponse>> {
    let store = state.bus.store();
    let terms = search_terms(query.q.as_deref().unwrap_or_default());
    let mut lessons = if terms.is_empty() {
        store.list_lessons(&id).await?
    } else {
        store
            .search_lessons(&id, &terms, true, SEARCH_LIMIT)
            .await?
    };
    if let Some(status) = query.status {
        lessons.retain(|l| l.status == status);
    }
    Ok(Json(ListLessonsResponse { lessons }))
}

async fn get_lesson(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Lesson>> {
    Ok(Json(state.bus.store().get_lesson(&id).await?))
}

fn non_empty(field: &str, text: Option<String>) -> ApiResult<Option<String>> {
    match text {
        Some(t) if t.trim().is_empty() => Err(AppError::from(CoreError::InvalidName(format!(
            "{field} cannot be empty"
        )))),
        other => Ok(other.map(|t| t.trim().to_owned())),
    }
}

/// Edits a lesson: the fields given replace the stored ones. Says so with `lesson.updated`.
async fn update_lesson(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateLessonRequest>,
) -> ApiResult<Json<Lesson>> {
    let store = state.bus.store();
    let mut lesson = store.get_lesson(&id).await?;
    if let Some(title) = non_empty("title", req.title)? {
        lesson.title = title;
    }
    if let Some(symptom) = non_empty("symptom", req.symptom)? {
        lesson.symptom = symptom;
    }
    if let Some(resolution) = non_empty("resolution", req.resolution)? {
        lesson.resolution = resolution;
    }
    if let Some(conditions) = req.conditions {
        lesson.conditions = conditions.trim().to_owned();
    }
    if let Some(keywords) = req.keywords {
        lesson.keywords = keywords
            .into_iter()
            .map(|k| k.trim().to_owned())
            .filter(|k| !k.is_empty())
            .collect();
    }
    if let Some(outcome) = req.outcome {
        lesson.outcome = outcome;
    }
    if let Some(status) = req.status {
        lesson.status = status;
        if status != LessonStatus::Superseded {
            lesson.superseded_by = None;
        }
    }
    if let Some(by) = req.superseded_by.filter(|b| !b.trim().is_empty()) {
        if by == lesson.id {
            return Err(AppError::from(CoreError::Conflict(
                "a lesson cannot supersede itself".into(),
            )));
        }
        store.get_lesson(&by).await?;
        lesson.status = LessonStatus::Superseded;
        lesson.superseded_by = Some(by);
    }
    lesson.updated_at = Utc::now();
    store.update_lesson(&lesson).await?;
    let _ = state
        .bus
        .publish(EventKind::LessonUpdated {
            lesson: lesson.clone(),
        })
        .await;
    Ok(Json(lesson))
}

/// The project's lessons as Markdown (`text/markdown`); empty when it has none.
async fn export(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let store = state.bus.store();
    let lessons = store.list_lessons(&id).await?;
    let name = store
        .list_projects()
        .await?
        .into_iter()
        .find(|p| p.id == id)
        .map(|p| p.name)
        .unwrap_or_else(|| id.clone());
    Ok((
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        export_markdown(&name, &lessons),
    )
        .into_response())
}

/// The problems recorded in a run, oldest first, each with how it was sorted if it was.
async fn run_issues(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Json<ListRunIssuesResponse>> {
    let run_id = resolve_run(&state, &key).await?;
    let store = state.bus.store();
    store.get_run(run_id).await?;
    let notes = store
        .list_notes(NoteScope::Run, Some(run_id), ISSUE_PREFIX)
        .await?;
    let sorted = store.list_triage(run_id).await?;
    let mut issues: Vec<RunIssue> = notes
        .into_iter()
        .map(|n| RunIssue {
            triage: sorted.iter().find(|t| t.issue.key == n.key).cloned(),
            key: n.key,
            author: n.author,
            text: n.text,
            updated_at: n.updated_at,
        })
        .collect();
    issues.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
    let unsorted = issues.iter().filter(|i| i.triage.is_none()).count() as u32;
    Ok(Json(ListRunIssuesResponse {
        run_id,
        issues,
        unsorted,
    }))
}
