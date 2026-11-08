//! Byte endpoints for extension assets and workspace files.
use crate::api::{AppError, AppState, resolve_run};
use axum::{
    Router,
    extract::{OriginalUri, Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use crossagents_core::{AgentKey, CoreError, RunId, TaskId};
use crossagents_orchestrator::files::{FileError, FilesScope, resolve_in_root};
use serde::Deserialize;
use std::{io::Read, path::Path as FsPath, sync::Arc};

pub const MAX_RAW_FILE_BYTES: u64 = 64 * 1024 * 1024;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/v1/extensions/{name}/assets/{fingerprint}/{*path}",
            get(asset),
        )
        .route("/v1/agents/{key}/files/raw", get(agent_raw))
        .route("/v1/runs/{key}/files/raw", get(run_raw))
        .route("/v1/runs/{key}/tasks/{task}/files/raw", get(task_raw))
}

async fn asset(
    State(state): State<Arc<AppState>>,
    Path((name, fingerprint, path)): Path<(String, String, String)>,
    OriginalUri(uri): OriginalUri,
) -> Response {
    let raw = uri.path().as_bytes();
    if raw.iter().enumerate().any(|(i, b)| {
        *b == b'%'
            && (i + 2 >= raw.len()
                || !raw[i + 1].is_ascii_hexdigit()
                || !raw[i + 2].is_ascii_hexdigit())
    }) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let valid_name = name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    let valid_fp = !fingerprint.is_empty()
        && fingerprint
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if !valid_name
        || !valid_fp
        || path.contains(['\\', ':', '\0'])
        || path.split('/').any(|p| p.is_empty() || p == "..")
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Ok(info) = state.extensions.get(&name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if info.current.as_ref().map(|v| v.fingerprint.as_str()) != Some(&fingerprint)
        && info.previous_fingerprint.as_deref() != Some(&fingerprint)
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let versions = state.home.join("extension-versions");
    let root = versions.join(name).join(fingerprint);
    let content_type = mime(&path);
    let result = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, StatusCode> {
        let missing = |_| StatusCode::NOT_FOUND;
        let versions = dunce::canonicalize(versions).map_err(missing)?;
        let root = dunce::canonicalize(root).map_err(missing)?;
        let file = dunce::canonicalize(root.join(&path)).map_err(missing)?;
        if root == versions
            || !root.starts_with(&versions)
            || file == root
            || !file.starts_with(&root)
        {
            return Err(StatusCode::NOT_FOUND);
        }
        let limit = crate::extensions::snapshot::MAX_BYTES;
        let meta = std::fs::metadata(&file).map_err(missing)?;
        if !meta.is_file() {
            return Err(StatusCode::NOT_FOUND);
        }
        if meta.len() > limit {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
        let input = std::fs::File::open(file).map_err(missing)?;
        let mut bytes = Vec::new();
        input
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(missing)?;
        if bytes.len() as u64 > limit {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
        Ok(bytes)
    })
    .await
    .unwrap_or(Err(StatusCode::NOT_FOUND));
    match result {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, content_type),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            bytes,
        )
            .into_response(),
        Err(status) => status.into_response(),
    }
}
fn mime(path: &str) -> &'static str {
    match FsPath::new(path)
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "html" => "text/html",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "wasm" => "application/wasm",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}
#[derive(Deserialize)]
struct RawQuery {
    path: Option<String>,
}
async fn agent_raw(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Query(q): Query<RawQuery>,
) -> Result<Response, AppError> {
    let agent = state.bus.store().get_agent(&AgentKey::parse(&key)).await?;
    raw(&state, FilesScope::Agent(&agent), q).await
}
async fn run_raw(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Query(q): Query<RawQuery>,
) -> Result<Response, AppError> {
    let run = resolve_run(&state, &key).await?;
    raw(&state, FilesScope::Run(run), q).await
}
async fn task_raw(
    State(state): State<Arc<AppState>>,
    Path((key, task)): Path<(String, String)>,
    Query(q): Query<RawQuery>,
) -> Result<Response, AppError> {
    let run = resolve_run(&state, &key).await?;
    let task = resolve_task(&state, run, &task).await?;
    raw(&state, FilesScope::Task { run, task }, q).await
}
async fn resolve_task(state: &AppState, run: RunId, key: &str) -> Result<TaskId, AppError> {
    if let Ok(id) = key.parse() {
        return Ok(id);
    }
    let key = key.trim().trim_start_matches('#');
    state
        .bus
        .store()
        .list_tasks(run)
        .await?
        .into_iter()
        .find(|t| t.key == key)
        .map(|t| t.id)
        .ok_or_else(|| {
            CoreError::NotFound {
                kind: "task",
                id: key.to_owned(),
            }
            .into()
        })
}
async fn raw(state: &AppState, scope: FilesScope<'_>, q: RawQuery) -> Result<Response, AppError> {
    let path = q.path.unwrap_or_default();
    crate::paths::refuse_remote_in(&state.orchestrator, scope, &path).await?;
    let root = state.orchestrator.files_root(scope).await?;
    tokio::task::spawn_blocking(move || read_raw(&root, &path))
        .await
        .map_err(|e| AppError::from(FileError::Failed(format!("file read task failed: {e}"))))?
}

fn read_raw(root: &FsPath, path: &str) -> Result<Response, AppError> {
    let (abs, rel) = resolve_in_root(root, path)?;
    let meta = std::fs::metadata(&abs).map_err(|_| FileError::NotFound(rel.clone()))?;
    if !meta.is_file() {
        return Err(FileError::BadRequest(format!("{rel} is not a regular file")).into());
    }
    if meta.len() > MAX_RAW_FILE_BYTES {
        return Ok(StatusCode::PAYLOAD_TOO_LARGE.into_response());
    }
    let mut file = std::fs::File::open(abs)
        .map_err(|e| FileError::Failed(format!("cannot read {rel}: {e}")))?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_RAW_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| FileError::Failed(format!("cannot read {rel}: {e}")))?;
    if bytes.len() as u64 > MAX_RAW_FILE_BYTES {
        return Ok(StatusCode::PAYLOAD_TOO_LARGE.into_response());
    }
    let name = rel.rsplit('/').next().unwrap_or("");
    let encoded: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename*=UTF-8''{encoded}"),
            ),
        ],
        bytes,
    )
        .into_response())
}
