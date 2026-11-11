//! Binding and merging projects (docs/tasks/LESSONS.md §4.2, L3): the two decisions a person
//! makes when the hub cannot — a checkout or a remote that belongs to a project it did not
//! recognise, and two projects that turn out to be one. Both answer with the project that
//! keeps the records, so a client can redraw without a second call.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::post;
use axum::{Json, Router};
use crossagents_api_types::{
    BindProjectRequest, MergeProjectsRequest, ProjectInfo, ProjectPathInfo,
};
use crossagents_core::Project;
use crossagents_orchestrator::lessons::{bind_project, merge_projects};

use crate::api::{ApiResult, AppState};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/projects/{id}/bind", post(bind))
        .route("/v1/projects/merge", post(merge))
}

pub(crate) fn info(project: Project) -> ProjectInfo {
    ProjectInfo {
        id: project.id,
        name: project.name,
        remotes: project.remotes,
        paths: project
            .paths
            .into_iter()
            .map(|at| ProjectPathInfo {
                node: at.node,
                path: at.path.to_string_lossy().into_owned(),
            })
            .collect(),
        unconfirmed_remote: project.unconfirmed_remote,
    }
}

/// A person settles what a project is: a remote it now has, or the checkout that is bound to
/// it — or that the checkout is another project and is split out. An identity the hub was
/// unsure of stops being unsure.
async fn bind(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<BindProjectRequest>,
) -> ApiResult<Json<ProjectInfo>> {
    let project = bind_project(
        &**state.bus.store(),
        &id,
        req.remote.as_deref(),
        req.path.as_deref(),
        req.split.unwrap_or(false),
    )
    .await?;
    Ok(Json(info(project)))
}

/// Two projects are one: `from`'s records, checkouts and remotes join `into`, and `from` is
/// left with none.
async fn merge(
    State(state): State<Arc<AppState>>,
    Json(req): Json<MergeProjectsRequest>,
) -> ApiResult<Json<ProjectInfo>> {
    let project = merge_projects(&**state.bus.store(), &req.from, &req.into).await?;
    Ok(Json(info(project)))
}
