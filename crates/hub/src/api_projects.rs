//! Projects (docs/tasks/LESSONS.md §4.2): the ids a client needs to address a project's
//! expertise profile, with the checkouts and remotes that identify it. A client works in a
//! directory, so it finds the project whose `paths` hold that directory and uses its `id`.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use crossagents_api_types::{ListProjectsResponse, ProjectInfo, ProjectPathInfo};

use crate::api::{ApiResult, AppState};

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/v1/projects", get(list_projects))
}

async fn list_projects(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<ListProjectsResponse>> {
    let projects = state.bus.store().list_projects().await?;
    Ok(Json(ListProjectsResponse {
        projects: projects
            .into_iter()
            .map(|project| ProjectInfo {
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
            })
            .collect(),
    }))
}
