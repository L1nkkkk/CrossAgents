//! HTTP routes and MCP tool handlers for cross-platform remote node control.
//!
//! Two router layers:
//! - [`node_channel_router`]: authenticated strictly with `DeviceKind::Node` tokens (`cad_...`).
//!   Hosts `GET /v1/node/stream` (SSE push of `NodeRpcCall`) and `POST /v1/node/results/{call_id}`.
//! - [`control_router`]: merged into the hub's normal `authed` router. Hosts `GET /v1/nodes`
//!   (`read`) and `POST /v1/nodes/{node}/...` (`admin`).
//!
//! In addition, [`mcp_tools`] and [`call_mcp_tool`] expose the same remote node operations to
//! agent sessions via `/v1/me/tools`, so a model running on this machine can inspect, edit, and
//! run commands on any connected Windows / macOS / Linux node.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Extension, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use crossagents_api_types::{
    ApiError, ErrorCode, FileContentResponse, FileEntry, FileGitChange, FileKind,
    FileSearchResponse, FileStatusEntry, FileStatusResponse, FileTreeResponse, ListNodesResponse,
    NodeDirEntry, NodeDirsResponse, NodeEditFileRequest, NodeEditFileResponse, NodeExecRequest,
    NodeExecResponse, NodeListFilesRequest, NodeListFilesResponse, NodeReadFileRequest,
    NodeReadFileResponse, NodeReadMediaRequest, NodeRpcOp, NodeRpcReply, NodeSearchFilesRequest,
    NodeSearchFilesResponse, NodeWriteFileRequest, NodeWriteFileResponse, RemoteNodeInfo,
    ResolvePathsRequest, ResolvePathsResponse, ResolvedPath, ToolInfo,
    MediaContent,
};
use crossagents_core::{Agent, CoreError, Device, DeviceKind, DeviceLevel, DeviceOs};
use futures::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::api::{ApiResult, AppError, AppState, VERSION};
use crate::remote_nodes::NodeStreamMsg;

const FILE_OP_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_EXEC_TIMEOUT_SECS: u64 = 60;
const MAX_EXEC_TIMEOUT_SECS: u64 = 120;
const NODE_FRAME_BODY_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Clone)]
struct AuthedNode {
    device: Device,
    cancel: CancellationToken,
}

/// Router for connected remote nodes (`DeviceKind::Node` tokens only).
pub fn node_channel_router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/node/stream", get(node_stream))
        .route("/v1/node/results/{call_id}", post(node_result))
        .route(
            "/v1/node/frames",
            post(node_frames).layer(DefaultBodyLimit::max(NODE_FRAME_BODY_LIMIT)),
        )
        .route_layer(middleware::from_fn_with_state(state, require_node_token))
}

/// Router for listing and controlling remote nodes (merged into the hub's `authed` router).
pub fn control_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/nodes", get(list_nodes))
        .route("/v1/nodes/{node}/dirs", get(node_dirs))
        .route("/v1/nodes/{node}/exec", post(node_exec))
        .route("/v1/nodes/{node}/files/read", post(node_read_file))
        .route("/v1/nodes/{node}/files/write", post(node_write_file))
        .route("/v1/nodes/{node}/files/edit", post(node_edit_file))
        .route("/v1/nodes/{node}/files/list", post(node_list_files))
        .route("/v1/nodes/{node}/files/search", post(node_search_files))
}

async fn require_node_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Response {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let Some(token) = presented else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiError::new(ErrorCode::Unauthorized, "missing node token")),
        )
            .into_response();
    };
    let os = headers
        .get("x-crossagents-client-os")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_device_os);
    let authed = state
        .devices
        .authenticate_reporting(state.bus.store().as_ref(), token, os)
        .await;
    match authed {
        Ok(Some((device, cancel)))
            if device.kind == DeviceKind::Node
                || (device.kind == DeviceKind::Client && device.level >= DeviceLevel::Write) =>
        {
            request
                .extensions_mut()
                .insert(AuthedNode { device, cancel });
            next.run(request).await
        }
        Ok(Some(_)) => (
            StatusCode::FORBIDDEN,
            Json(ApiError::new(
                ErrorCode::Forbidden,
                "this endpoint requires a node token or a write/admin client device token",
            )),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::UNAUTHORIZED,
            Json(ApiError::new(
                ErrorCode::Unauthorized,
                "invalid or revoked node token",
            )),
        )
            .into_response(),
        Err(err) => AppError::from(err).into_response(),
    }
}

fn parse_device_os(raw: &str) -> Option<DeviceOs> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "windows" => Some(DeviceOs::Windows),
        "macos" | "darwin" => Some(DeviceOs::Macos),
        "linux" => Some(DeviceOs::Linux),
        "ios" => Some(DeviceOs::Ios),
        "android" => Some(DeviceOs::Android),
        _ => None,
    }
}

#[derive(Debug, Deserialize, Default)]
struct NodeStreamQuery {
    #[serde(default)]
    os: String,
    #[serde(default)]
    arch: String,
    #[serde(default)]
    hostname: String,
    #[serde(default)]
    shell: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    chats_dir: String,
    #[serde(default)]
    version: String,
}

async fn node_stream(
    State(state): State<Arc<AppState>>,
    Extension(authed): Extension<AuthedNode>,
    Query(q): Query<NodeStreamQuery>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let chats_dir = q.chats_dir.trim();
    let info = RemoteNodeInfo {
        id: authed.device.id.clone(),
        name: authed.device.name.clone(),
        os: if q.os.trim().is_empty() {
            "unknown".into()
        } else {
            q.os.trim().to_owned()
        },
        arch: if q.arch.trim().is_empty() {
            "unknown".into()
        } else {
            q.arch.trim().to_owned()
        },
        hostname: if q.hostname.trim().is_empty() {
            authed.device.name.clone()
        } else {
            q.hostname.trim().to_owned()
        },
        shell: if q.shell.trim().is_empty() {
            "sh".into()
        } else {
            q.shell.trim().to_owned()
        },
        default_cwd: q.cwd.trim().to_owned(),
        chats_dir: (!chats_dir.is_empty()).then(|| chats_dir.to_owned()),
        version: if q.version.trim().is_empty() {
            "unknown".into()
        } else {
            q.version.trim().to_owned()
        },
        connected_at: Utc::now(),
        local: false,
        online: true,
        plugins: Vec::new(),
    };

    let (mut rx, registration) = state.remote_nodes.register(info.clone()).await;
    let (out_tx, out_rx) = mpsc::channel::<Result<SseEvent, Infallible>>(256);
    let cancel = authed.cancel;
    let shutdown = state.shutdown.clone();

    tokio::spawn(async move {
        let _registration = registration;
        if let Ok(hello) = serde_json::to_string(&json!({ "connected": info })) {
            let _ = out_tx
                .send(Ok(SseEvent::default().event("hello").data(hello)))
                .await;
        }
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = shutdown.cancelled() => break,
                _ = out_tx.closed() => break,
                msg = rx.recv() => {
                    let Some(msg) = msg else { break };
                    let sse = match msg {
                        NodeStreamMsg::Call(call) => {
                            let Ok(data) = serde_json::to_string(&call) else { continue };
                            SseEvent::default().event("call").data(data)
                        }
                        NodeStreamMsg::Frame(line) => {
                            SseEvent::default()
                                .event("frame")
                                .data(line.trim_end_matches(['\r', '\n']))
                        }
                    };
                    if out_tx.send(Ok(sse)).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    Sse::new(ReceiverStream::new(out_rx)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

async fn node_frames(
    State(state): State<Arc<AppState>>,
    Extension(authed): Extension<AuthedNode>,
    body: String,
) -> StatusCode {
    if state.remote_nodes.feed_frames(&authed.device.id, &body).await {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::CONFLICT
    }
}

async fn node_result(
    State(state): State<Arc<AppState>>,
    Path(call_id): Path<String>,
    Json(reply): Json<NodeRpcReply>,
) -> StatusCode {
    if state.remote_nodes.complete(&call_id, reply).await {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn list_nodes(State(state): State<Arc<AppState>>) -> Json<ListNodesResponse> {
    Json(ListNodesResponse {
        nodes: state
            .remote_nodes
            .list(
                state.store().as_ref(),
                state.runtime.plugins(),
                state.started_at,
                VERSION,
            )
            .await,
    })
}

#[derive(Debug, Default, Deserialize)]
struct NodeDirsQuery {
    #[serde(default)]
    path: String,
}

const DRIVES_SENTINEL: &str = "__drives__";

async fn node_dirs(
    State(state): State<Arc<AppState>>,
    Path(node): Path<String>,
    Query(q): Query<NodeDirsQuery>,
) -> ApiResult<Json<NodeDirsResponse>> {
    let key = node.trim();
    if key.is_empty() || key.eq_ignore_ascii_case("local") {
        let requested = q.path.trim().to_owned();
        let resp = tokio::task::spawn_blocking(move || list_local_dirs(&requested))
            .await
            .map_err(|e| CoreError::Agent(format!("dir listing task failed: {e}")))??;
        return Ok(Json(resp));
    }

    let info = state.remote_nodes.resolve_online(key).await.ok_or_else(|| {
        CoreError::Agent(format!("节点 {key:?} 当前离线，无法浏览目录"))
    })?;
    let is_windows = info.os.eq_ignore_ascii_case("windows");
    let req_path = q.path.trim();
    let target = if req_path.is_empty() {
        info.default_cwd.clone()
    } else {
        req_path.to_owned()
    };
    let listed: NodeListFilesResponse = state
        .remote_nodes
        .call(
            &info.id,
            NodeRpcOp::ListFiles(NodeListFilesRequest {
                path: target,
                max_entries: Some(500),
            }),
            FILE_OP_TIMEOUT,
        )
        .await?;
    let mut dirs: Vec<NodeDirEntry> = listed
        .entries
        .into_iter()
        .filter(|e| e.is_dir)
        .map(|e| NodeDirEntry {
            path: join_remote_path(&listed.path, &e.name, is_windows),
            name: e.name,
        })
        .collect();
    sort_dir_entries(&mut dirs);
    let parent = parent_remote_path(&listed.path, is_windows);
    Ok(Json(NodeDirsResponse {
        node: info.name,
        os: info.os,
        path: listed.path,
        parent,
        dirs,
        truncated: listed.truncated,
    }))
}

fn sort_dir_entries(dirs: &mut [NodeDirEntry]) {
    dirs.sort_by(|a, b| {
        a.name
            .starts_with('.')
            .cmp(&b.name.starts_with('.'))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

fn list_local_dirs(requested: &str) -> Result<NodeDirsResponse, CoreError> {
    let os = std::env::consts::OS.to_owned();
    if cfg!(windows) && requested == DRIVES_SENTINEL {
        let mut dirs = Vec::new();
        for letter in b'A'..=b'Z' {
            let root = format!("{}:\\", letter as char);
            if std::path::Path::new(&root).is_dir() {
                dirs.push(NodeDirEntry {
                    name: root.clone(),
                    path: root,
                });
            }
        }
        return Ok(NodeDirsResponse {
            node: "local".into(),
            os,
            path: String::new(),
            parent: None,
            dirs,
            truncated: false,
        });
    }

    let dir = if requested.is_empty() {
        dirs::home_dir()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("."))
    } else {
        std::path::PathBuf::from(requested)
    };
    let rd = std::fs::read_dir(&dir)
        .map_err(|e| CoreError::Agent(format!("无法读取目录 {}: {e}", dir.display())))?;
    let mut dirs = Vec::new();
    for entry in rd.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path().to_string_lossy().into_owned();
        dirs.push(NodeDirEntry { name, path });
    }
    sort_dir_entries(&mut dirs);
    let truncated = dirs.len() > 500;
    dirs.truncate(500);
    let path_str = dir.to_string_lossy().into_owned();
    let parent = match dir.parent() {
        Some(p) if !p.as_os_str().is_empty() && p != dir => {
            Some(p.to_string_lossy().into_owned())
        }
        _ if cfg!(windows) => Some(DRIVES_SENTINEL.into()),
        _ => None,
    };
    Ok(NodeDirsResponse {
        node: "local".into(),
        os,
        path: path_str,
        parent,
        dirs,
        truncated,
    })
}

fn join_remote_path(base: &str, child: &str, windows: bool) -> String {
    let sep = if windows { '\\' } else { '/' };
    let child = child.trim_matches(['/', '\\']);
    if child.is_empty() {
        return base.to_owned();
    }
    let trimmed = base.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        if windows {
            child.to_owned()
        } else {
            format!("/{child}")
        }
    } else if windows {
        format!("{trimmed}{sep}{}", child.replace('/', "\\"))
    } else {
        format!("{trimmed}{sep}{}", child.replace('\\', "/"))
    }
}

fn parent_remote_path(path: &str, windows: bool) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    if windows {
        let clean = trimmed.trim_end_matches(['\\', '/']);
        if clean.len() <= 2 && clean.ends_with(':') {
            return None;
        }
        let idx = clean.rfind(['\\', '/'])?;
        let head = &clean[..idx];
        if head.len() == 2 && head.ends_with(':') {
            Some(format!("{head}\\"))
        } else if head.is_empty() {
            None
        } else {
            Some(head.to_owned())
        }
    } else {
        let clean = trimmed.trim_end_matches('/');
        if clean.is_empty() {
            return None;
        }
        match clean.rfind('/') {
            Some(0) => Some("/".into()),
            Some(idx) => Some(clean[..idx].to_owned()),
            None => None,
        }
    }
}

fn sanitize_rel_path(raw: &str) -> Result<String, AppError> {
    let trimmed = raw.trim().replace('\\', "/");
    let stripped = trimmed.trim_start_matches("./").trim_matches('/');
    if stripped.is_empty() {
        return Ok(String::new());
    }
    if trimmed.starts_with('/')
        || (trimmed.len() >= 2 && trimmed.as_bytes()[1] == b':')
    {
        return Err(AppError::bad_request("path must be relative to the workspace"));
    }
    let mut parts = Vec::new();
    for part in stripped.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err(AppError::bad_request("path cannot escape the workspace"));
        }
        parts.push(part);
    }
    Ok(parts.join("/"))
}

fn is_ignored_name(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | ".DS_Store" | "__pycache__" | ".next" | "dist" | ".venv" | "venv"
    )
}

async fn resolve_remote_agent_node(
    state: &AppState,
    agent: &Agent,
) -> Result<(RemoteNodeInfo, String, bool), AppError> {
    let node_key = agent
        .node
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::bad_request("agent has no remote node"))?;
    let info = state
        .remote_nodes
        .resolve_online(node_key)
        .await
        .ok_or_else(|| {
            CoreError::Agent(format!("节点 {node_key:?} 当前离线，无法查看工作区文件"))
        })?;
    let root = agent.cwd.to_string_lossy().into_owned();
    let is_windows = info.os.eq_ignore_ascii_case("windows");
    Ok((info, root, is_windows))
}

pub(crate) async fn remote_agent_files_tree(
    state: &AppState,
    agent: &Agent,
    rel_path: &str,
    all: bool,
) -> ApiResult<Json<FileTreeResponse>> {
    let (info, root, is_windows) = resolve_remote_agent_node(state, agent).await?;
    let rel = sanitize_rel_path(rel_path)?;
    let target = if rel.is_empty() {
        root.clone()
    } else {
        join_remote_path(&root, &rel, is_windows)
    };
    let listed: NodeListFilesResponse = state
        .remote_nodes
        .call(
            &info.id,
            NodeRpcOp::ListFiles(NodeListFilesRequest {
                path: target,
                max_entries: Some(1000),
            }),
            FILE_OP_TIMEOUT,
        )
        .await?;
    let mut entries = Vec::with_capacity(listed.entries.len());
    for entry in listed.entries {
        let ignored = is_ignored_name(&entry.name);
        if ignored && !all {
            continue;
        }
        let child_rel = if rel.is_empty() {
            entry.name.clone()
        } else {
            format!("{rel}/{}", entry.name)
        };
        entries.push(FileEntry {
            name: entry.name,
            path: child_rel,
            kind: if entry.is_dir {
                FileKind::Dir
            } else {
                FileKind::File
            },
            size: (!entry.is_dir).then_some(entry.size),
            ignored,
        });
    }
    Ok(Json(FileTreeResponse {
        root,
        path: rel,
        git_ref: None,
        entries,
    }))
}

pub(crate) async fn remote_agent_files_content(
    state: &AppState,
    agent: &Agent,
    rel_path: &str,
) -> ApiResult<Json<FileContentResponse>> {
    let (info, root, is_windows) = resolve_remote_agent_node(state, agent).await?;
    let rel = sanitize_rel_path(rel_path)?;
    if rel.is_empty() {
        return Err(AppError::bad_request("file path is required"));
    }
    let full = join_remote_path(&root, &rel, is_windows);
    match state
        .remote_nodes
        .call::<NodeReadFileResponse>(
            &info.id,
            NodeRpcOp::ReadFile(NodeReadFileRequest {
                path: full.clone(),
                start_line: None,
                end_line: None,
            }),
            FILE_OP_TIMEOUT,
        )
        .await
    {
        Ok(resp) => Ok(Json(FileContentResponse {
            path: rel,
            abs_path: resp.path,
            size: resp.content.len() as u64,
            binary: false,
            truncated: resp.truncated,
            text: Some(resp.content),
            git_ref: None,
        })),
        Err(CoreError::Agent(msg)) if msg.contains("not valid UTF-8") => {
            Ok(Json(FileContentResponse {
                path: rel,
                abs_path: full,
                size: 0,
                binary: true,
                truncated: false,
                text: None,
                git_ref: None,
            }))
        }
        Err(err) => Err(err.into()),
    }
}

pub(crate) async fn remote_agent_files_media(
    state: &AppState,
    agent: &Agent,
    raw_path: &str,
    offset: Option<u64>,
    limit: Option<u64>,
) -> ApiResult<Json<MediaContent>> {
    let (info, root, _is_windows) = resolve_remote_agent_node(state, agent).await?;
    let content: MediaContent = state
        .remote_nodes
        .call(
            &info.id,
            NodeRpcOp::ReadMedia(NodeReadMediaRequest {
                cwd: root,
                path: raw_path.to_owned(),
                offset,
                limit,
            }),
            FILE_OP_TIMEOUT,
        )
        .await?;
    Ok(Json(content))
}

pub(crate) async fn remote_agent_files_status(
    state: &AppState,
    agent: &Agent,
) -> ApiResult<Json<FileStatusResponse>> {
    let (info, root, _) = resolve_remote_agent_node(state, agent).await?;
    let resp: NodeExecResponse = state
        .remote_nodes
        .call(
            &info.id,
            NodeRpcOp::Exec(NodeExecRequest {
                command: "git status --porcelain=v1 -uall".into(),
                cwd: Some(root.clone()),
                timeout_secs: Some(8),
            }),
            Duration::from_secs(12),
        )
        .await?;
    if resp.exit_code != Some(0) {
        return Ok(Json(FileStatusResponse {
            root,
            git: false,
            entries: Vec::new(),
        }));
    }
    let mut entries = Vec::new();
    for line in resp.stdout.lines() {
        if line.len() < 4 {
            continue;
        }
        let code = &line[..2];
        let raw_path = line[3..].trim().trim_matches('"');
        let path = raw_path
            .rsplit_once(" -> ")
            .map_or(raw_path, |(_, next)| next)
            .replace('\\', "/");
        if path.is_empty() {
            continue;
        }
        let change = if code == "??" || code.contains('A') {
            FileGitChange::Added
        } else if code.contains('D') {
            FileGitChange::Deleted
        } else {
            FileGitChange::Modified
        };
        entries.push(FileStatusEntry { path, change });
    }
    Ok(Json(FileStatusResponse {
        root,
        git: true,
        entries,
    }))
}

pub(crate) async fn remote_agent_files_search(
    state: &AppState,
    agent: &Agent,
    query: &str,
    all: bool,
) -> ApiResult<Json<FileSearchResponse>> {
    let (info, root, is_windows) = resolve_remote_agent_node(state, agent).await?;
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return Ok(Json(FileSearchResponse {
            root,
            paths: Vec::new(),
            truncated: false,
        }));
    }

    let git_cmd = if all {
        "git ls-files --cached --others"
    } else {
        "git ls-files --cached --others --exclude-standard"
    };
    if let Ok(resp) = state
        .remote_nodes
        .call::<NodeExecResponse>(
            &info.id,
            NodeRpcOp::Exec(NodeExecRequest {
                command: git_cmd.into(),
                cwd: Some(root.clone()),
                timeout_secs: Some(8),
            }),
            Duration::from_secs(12),
        )
        .await
        && resp.exit_code == Some(0)
    {
        let mut paths = Vec::new();
        let mut truncated = false;
        for line in resp.stdout.lines() {
            let p = line.trim().replace('\\', "/");
            if p.is_empty() {
                continue;
            }
            if p.to_ascii_lowercase().contains(&needle) {
                if paths.len() >= 200 {
                    truncated = true;
                    break;
                }
                paths.push(p);
            }
        }
        return Ok(Json(FileSearchResponse {
            root,
            paths,
            truncated,
        }));
    }

    // Fallback for non-git remote folders: bounded BFS via ListFiles.
    let mut queue = std::collections::VecDeque::from([String::new()]);
    let mut visited = 0usize;
    let mut paths = Vec::new();
    let mut truncated = false;
    while let Some(dir) = queue.pop_front() {
        if visited >= 25 || paths.len() >= 200 {
            truncated = true;
            break;
        }
        visited += 1;
        let target = if dir.is_empty() {
            root.clone()
        } else {
            join_remote_path(&root, &dir, is_windows)
        };
        let Ok(listed) = state
            .remote_nodes
            .call::<NodeListFilesResponse>(
                &info.id,
                NodeRpcOp::ListFiles(NodeListFilesRequest {
                    path: target,
                    max_entries: Some(300),
                }),
                Duration::from_secs(10),
            )
            .await
        else {
            continue;
        };
        for entry in listed.entries {
            if !all && is_ignored_name(&entry.name) {
                continue;
            }
            let rel = if dir.is_empty() {
                entry.name.clone()
            } else {
                format!("{dir}/{}", entry.name)
            };
            if entry.is_dir {
                queue.push_back(rel);
            } else if rel.to_ascii_lowercase().contains(&needle) {
                if paths.len() >= 200 {
                    truncated = true;
                    break;
                }
                paths.push(rel);
            }
        }
    }
    Ok(Json(FileSearchResponse {
        root,
        paths,
        truncated,
    }))
}

pub(crate) async fn remote_agent_files_resolve(
    state: &AppState,
    agent: &Agent,
    req: ResolvePathsRequest,
) -> ApiResult<Json<ResolvePathsResponse>> {
    let (info, root, is_windows) = resolve_remote_agent_node(state, agent).await?;
    let norm_root = root.replace('\\', "/").trim_end_matches('/').to_owned();
    let mut candidates: Vec<Option<String>> = Vec::with_capacity(req.paths.len());
    let mut parents: Vec<String> = Vec::new();
    for raw in &req.paths {
        let norm = raw.trim().replace('\\', "/");
        let rel = if let Some(rest) = norm.strip_prefix(&format!("{norm_root}/")) {
            sanitize_rel_path(rest).ok()
        } else if norm == norm_root {
            Some(String::new())
        } else {
            sanitize_rel_path(&norm).ok()
        };
        if let Some(r) = &rel
            && !r.is_empty()
        {
            let parent = r.rsplit_once('/').map_or(String::new(), |(p, _)| p.to_owned());
            if !parents.contains(&parent) && parents.len() < 12 {
                parents.push(parent);
            }
        }
        candidates.push(rel);
    }

    let mut dir_cache: std::collections::HashMap<String, Vec<crossagents_api_types::NodeFileEntry>> =
        std::collections::HashMap::new();
    for parent in parents {
        let target = if parent.is_empty() {
            root.clone()
        } else {
            join_remote_path(&root, &parent, is_windows)
        };
        if let Ok(listed) = state
            .remote_nodes
            .call::<NodeListFilesResponse>(
                &info.id,
                NodeRpcOp::ListFiles(NodeListFilesRequest {
                    path: target,
                    max_entries: Some(1000),
                }),
                Duration::from_secs(10),
            )
            .await
        {
            dir_cache.insert(parent, listed.entries);
        }
    }

    let entries = req
        .paths
        .into_iter()
        .zip(candidates)
        .map(|(input, rel)| {
            let Some(rel) = rel else {
                return ResolvedPath {
                    input,
                    path: None,
                    abs_path: None,
                    kind: None,
                };
            };
            if rel.is_empty() {
                return ResolvedPath {
                    input,
                    path: Some(String::new()),
                    abs_path: Some(root.clone()),
                    kind: Some(FileKind::Dir),
                };
            }
            let (parent, name) = match rel.rsplit_once('/') {
                Some((p, n)) => (p.to_owned(), n),
                None => (String::new(), rel.as_str()),
            };
            let found = dir_cache.get(&parent).and_then(|list| {
                list.iter().find(|e| {
                    if is_windows {
                        e.name.eq_ignore_ascii_case(name)
                    } else {
                        e.name == name
                    }
                })
            });
            match found {
                Some(entry) => ResolvedPath {
                    input,
                    abs_path: Some(join_remote_path(&root, &rel, is_windows)),
                    path: Some(rel),
                    kind: Some(if entry.is_dir {
                        FileKind::Dir
                    } else {
                        FileKind::File
                    }),
                },
                None => ResolvedPath {
                    input,
                    path: None,
                    abs_path: None,
                    kind: None,
                },
            }
        })
        .collect();
    Ok(Json(ResolvePathsResponse { entries }))
}

async fn node_exec(
    State(state): State<Arc<AppState>>,
    Path(node): Path<String>,
    Json(mut req): Json<NodeExecRequest>,
) -> ApiResult<Json<NodeExecResponse>> {
    let secs = req
        .timeout_secs
        .unwrap_or(DEFAULT_EXEC_TIMEOUT_SECS)
        .clamp(1, MAX_EXEC_TIMEOUT_SECS);
    req.timeout_secs = Some(secs);
    let resp: NodeExecResponse = state
        .remote_nodes
        .call(&node, NodeRpcOp::Exec(req), Duration::from_secs(secs + 10))
        .await?;
    Ok(Json(resp))
}

async fn node_read_file(
    State(state): State<Arc<AppState>>,
    Path(node): Path<String>,
    Json(req): Json<NodeReadFileRequest>,
) -> ApiResult<Json<NodeReadFileResponse>> {
    let resp: NodeReadFileResponse = state
        .remote_nodes
        .call(&node, NodeRpcOp::ReadFile(req), FILE_OP_TIMEOUT)
        .await?;
    Ok(Json(resp))
}

async fn node_write_file(
    State(state): State<Arc<AppState>>,
    Path(node): Path<String>,
    Json(req): Json<NodeWriteFileRequest>,
) -> ApiResult<Json<NodeWriteFileResponse>> {
    let resp: NodeWriteFileResponse = state
        .remote_nodes
        .call(&node, NodeRpcOp::WriteFile(req), FILE_OP_TIMEOUT)
        .await?;
    Ok(Json(resp))
}

async fn node_edit_file(
    State(state): State<Arc<AppState>>,
    Path(node): Path<String>,
    Json(req): Json<NodeEditFileRequest>,
) -> ApiResult<Json<NodeEditFileResponse>> {
    let resp: NodeEditFileResponse = state
        .remote_nodes
        .call(&node, NodeRpcOp::EditFile(req), FILE_OP_TIMEOUT)
        .await?;
    Ok(Json(resp))
}

async fn node_list_files(
    State(state): State<Arc<AppState>>,
    Path(node): Path<String>,
    Json(req): Json<NodeListFilesRequest>,
) -> ApiResult<Json<NodeListFilesResponse>> {
    let resp: NodeListFilesResponse = state
        .remote_nodes
        .call(&node, NodeRpcOp::ListFiles(req), FILE_OP_TIMEOUT)
        .await?;
    Ok(Json(resp))
}

async fn node_search_files(
    State(state): State<Arc<AppState>>,
    Path(node): Path<String>,
    Json(req): Json<NodeSearchFilesRequest>,
) -> ApiResult<Json<NodeSearchFilesResponse>> {
    let resp: NodeSearchFilesResponse = state
        .remote_nodes
        .call(&node, NodeRpcOp::SearchFiles(req), FILE_OP_TIMEOUT)
        .await?;
    Ok(Json(resp))
}

// ---------------------------------------------------------------------------------------
// Agent MCP tools (brain here, hands on another Windows / macOS / Linux node)

pub fn mcp_tools() -> Vec<ToolInfo> {
    vec![
        ToolInfo {
            name: "node_list".into(),
            description: "List all machines (nodes) connected to the CrossAgents hub, including their OS, arch, default shell, and working directory.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        },
        ToolInfo {
            name: "node_exec".into(),
            description: "Run a shell command on a connected remote node (Windows/macOS/Linux). Check `node_list` first to know the target node's OS and shell.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "node": { "type": "string", "description": "Target remote node name, id, or hostname." },
                    "command": { "type": "string", "description": "Shell command to execute on the target node." },
                    "cwd": { "type": "string", "description": "Optional working directory on the target node." },
                    "timeout_secs": { "type": "integer", "description": "Optional timeout in seconds (1-120, default 60)." }
                },
                "required": ["node", "command"]
            }),
        },
        ToolInfo {
            name: "node_read".into(),
            description: "Read a UTF-8 text file on a connected remote node.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "node": { "type": "string", "description": "Target remote node name, id, or hostname." },
                    "path": { "type": "string", "description": "Absolute or node-cwd-relative path on the target node." },
                    "start_line": { "type": "integer", "description": "Optional 1-based start line." },
                    "end_line": { "type": "integer", "description": "Optional 1-based inclusive end line." }
                },
                "required": ["node", "path"]
            }),
        },
        ToolInfo {
            name: "node_write".into(),
            description: "Write or create a UTF-8 file on a connected remote node.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "node": { "type": "string", "description": "Target remote node name, id, or hostname." },
                    "path": { "type": "string", "description": "Absolute or node-cwd-relative file path." },
                    "content": { "type": "string", "description": "Full file contents to write." },
                    "create_dirs": { "type": "boolean", "description": "Create missing parent directories (default true)." }
                },
                "required": ["node", "path", "content"]
            }),
        },
        ToolInfo {
            name: "node_edit".into(),
            description: "Replace exact text inside a file on a connected remote node.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "node": { "type": "string", "description": "Target remote node name, id, or hostname." },
                    "path": { "type": "string", "description": "File path on the target node." },
                    "target": { "type": "string", "description": "Exact existing text to replace." },
                    "replacement": { "type": "string", "description": "Replacement text." },
                    "replace_all": { "type": "boolean", "description": "Replace all occurrences (default false: first match only)." }
                },
                "required": ["node", "path", "target", "replacement"]
            }),
        },
        ToolInfo {
            name: "node_list_files".into(),
            description: "List files and directories inside a folder on a connected remote node.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "node": { "type": "string", "description": "Target remote node name, id, or hostname." },
                    "path": { "type": "string", "description": "Directory path (empty uses the node's default cwd)." },
                    "max_entries": { "type": "integer", "description": "Maximum entries to return (default 200)." }
                },
                "required": ["node"]
            }),
        },
        ToolInfo {
            name: "node_search".into(),
            description: "Search for a text substring recursively in files on a connected remote node.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "node": { "type": "string", "description": "Target remote node name, id, or hostname." },
                    "pattern": { "type": "string", "description": "Substring to search for." },
                    "path": { "type": "string", "description": "Directory or file to search (empty uses the node's default cwd)." },
                    "max_matches": { "type": "integer", "description": "Maximum matching lines to return (default 100)." }
                },
                "required": ["node", "pattern"]
            }),
        },
    ]
}

/// Handles a remote-node MCP tool call if `name` is one of ours; returns `None` for other tools.
pub async fn call_mcp_tool(
    state: &AppState,
    name: &str,
    input: Value,
) -> Option<Result<Value, CoreError>> {
    match name {
        "node_list" => {
            let nodes = state
                .remote_nodes
                .list(
                    state.store().as_ref(),
                    state.runtime.plugins(),
                    state.started_at,
                    VERSION,
                )
                .await;
            Some(Ok(json!({ "nodes": nodes })))
        }
        "node_exec" => Some(run_mcp_exec(state, input).await),
        "node_read" => Some(run_mcp_read(state, input).await),
        "node_write" => Some(run_mcp_write(state, input).await),
        "node_edit" => Some(run_mcp_edit(state, input).await),
        "node_list_files" => Some(run_mcp_list(state, input).await),
        "node_search" => Some(run_mcp_search(state, input).await),
        _ => None,
    }
}

fn require_node_arg(input: &Value) -> Result<String, CoreError> {
    input
        .get("node")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| CoreError::Agent("missing required string field `node`".into()))
}

async fn run_mcp_exec(state: &AppState, input: Value) -> Result<Value, CoreError> {
    let node = require_node_arg(&input)?;
    let mut req: NodeExecRequest = serde_json::from_value(input)
        .map_err(|e| CoreError::Agent(format!("invalid node_exec arguments: {e}")))?;
    let secs = req
        .timeout_secs
        .unwrap_or(DEFAULT_EXEC_TIMEOUT_SECS)
        .clamp(1, MAX_EXEC_TIMEOUT_SECS);
    req.timeout_secs = Some(secs);
    let resp: NodeExecResponse = state
        .remote_nodes
        .call(&node, NodeRpcOp::Exec(req), Duration::from_secs(secs + 10))
        .await?;
    serde_json::to_value(resp).map_err(|e| CoreError::Agent(e.to_string()))
}

async fn run_mcp_read(state: &AppState, input: Value) -> Result<Value, CoreError> {
    let node = require_node_arg(&input)?;
    let req: NodeReadFileRequest = serde_json::from_value(input)
        .map_err(|e| CoreError::Agent(format!("invalid node_read arguments: {e}")))?;
    let resp: NodeReadFileResponse = state
        .remote_nodes
        .call(&node, NodeRpcOp::ReadFile(req), FILE_OP_TIMEOUT)
        .await?;
    serde_json::to_value(resp).map_err(|e| CoreError::Agent(e.to_string()))
}

async fn run_mcp_write(state: &AppState, input: Value) -> Result<Value, CoreError> {
    let node = require_node_arg(&input)?;
    let req: NodeWriteFileRequest = serde_json::from_value(input)
        .map_err(|e| CoreError::Agent(format!("invalid node_write arguments: {e}")))?;
    let resp: NodeWriteFileResponse = state
        .remote_nodes
        .call(&node, NodeRpcOp::WriteFile(req), FILE_OP_TIMEOUT)
        .await?;
    serde_json::to_value(resp).map_err(|e| CoreError::Agent(e.to_string()))
}

async fn run_mcp_edit(state: &AppState, input: Value) -> Result<Value, CoreError> {
    let node = require_node_arg(&input)?;
    let req: NodeEditFileRequest = serde_json::from_value(input)
        .map_err(|e| CoreError::Agent(format!("invalid node_edit arguments: {e}")))?;
    let resp: NodeEditFileResponse = state
        .remote_nodes
        .call(&node, NodeRpcOp::EditFile(req), FILE_OP_TIMEOUT)
        .await?;
    serde_json::to_value(resp).map_err(|e| CoreError::Agent(e.to_string()))
}

async fn run_mcp_list(state: &AppState, input: Value) -> Result<Value, CoreError> {
    let node = require_node_arg(&input)?;
    let req: NodeListFilesRequest = serde_json::from_value(input)
        .map_err(|e| CoreError::Agent(format!("invalid node_list_files arguments: {e}")))?;
    let resp: NodeListFilesResponse = state
        .remote_nodes
        .call(&node, NodeRpcOp::ListFiles(req), FILE_OP_TIMEOUT)
        .await?;
    serde_json::to_value(resp).map_err(|e| CoreError::Agent(e.to_string()))
}

async fn run_mcp_search(state: &AppState, input: Value) -> Result<Value, CoreError> {
    let node = require_node_arg(&input)?;
    let req: NodeSearchFilesRequest = serde_json::from_value(input)
        .map_err(|e| CoreError::Agent(format!("invalid node_search arguments: {e}")))?;
    let resp: NodeSearchFilesResponse = state
        .remote_nodes
        .call(&node, NodeRpcOp::SearchFiles(req), FILE_OP_TIMEOUT)
        .await?;
    serde_json::to_value(resp).map_err(|e| CoreError::Agent(e.to_string()))
}
