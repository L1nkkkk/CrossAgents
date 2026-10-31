//! Local HTTP API (docs/DESIGN.md §11). All business logic lives in the hub; clients only
//! call these routes.

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Extension;
use axum::extract::DefaultBodyLimit;
use axum::extract::{MatchedPath, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Utc};
use crossagents_api_types::{
    API_VERSION, AccountQuota, AccountUsageGroup, AccountUsageResponse, AgentCommandsResponse,
    AgentExecution, AgentExecutionResponse, AgentSettingsRequest, AnswerRequest, ApiError,
    ApproveRunRequest, ArchiveAgentRequest, AttachmentContent, AttachmentUpload, ChangesResponse,
    CliInfo, CliInstallAccepted, CommitGraph, ConnectInfo, CreateAccountRequest,
    CreateAgentRequest, CreateRunRequest, CreateWorkRequest, CreateWorkRunRequest,
    DeleteRunResponse, DiffSide, ErrorCode, FileContentResponse, FileEntry, FileKind,
    FileTreeResponse, FinishRequest, GraphCommit, GraphRef, GraphRefKind, HealthResponse, HubInfo,
    ImageContent, InitRepoRequest, LandRequest, ListAccountsResponse, ListAgentsResponse,
    ListNotesResponse, ListPluginsResponse, ListQuestionsResponse, ListRunsResponse,
    ListToolsResponse, ListWorksResponse, LoginStarted, PluginInfo, QuotaResponse,
    RejectRunRequest, RenameAgentRequest, ResolvePathsRequest, ResolvePathsResponse, ResolvedPath,
    RunDetail, RunTrace, SendMessageRequest, SendMessageResponse, SetModelRequest,
    SetPluginModelsRequest, SetTopicRequest, TaskReviewRequest, TaskTrace, Transcript,
    TurnDiffResponse, UpdateRunPolicyRequest, UsageGroup, UsageResponse, UsageSeriesGroup,
    UsageSeriesRange, UsageSeriesResponse, WorkDetail,
};
use crossagents_core::{
    Account, AccountStatus, Agent, AgentId, AgentKey, AgentProfile, AgentStatus, Attachment,
    AttachmentSupport, CoreError, DriverKind, Envelope, Event, EventBus, EventKind, HumanItem,
    LandMode, MessageId, MessageSource, Run, RunConfig, RunId, Store, Task, TaskId,
};
use crossagents_orchestrator::{
    Approval, DeleteRun, FileError, FilesScope, GitWorkspaces, NewRun, Orchestrator,
};

use crate::accounts::Accounts;
use crate::collab::AgentTokens;
use crate::runtime::Runtime;
use futures::Stream;
use serde::Deserialize;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const EVENT_PAGE: u32 = 500;
/// A message with its files, base64 (a third larger than the files themselves).
const MESSAGE_BODY_LIMIT: usize =
    crossagents_api_types::MAX_ATTACHMENTS_TOTAL / 3 * 4 + 1024 * 1024;

pub struct AppState {
    pub bus: Arc<EventBus>,
    pub runtime: Arc<Runtime>,
    pub orchestrator: Arc<Orchestrator>,
    pub accounts: Arc<Accounts>,
    pub models: Arc<crate::models::ModelChoice>,
    pub tokens: Arc<AgentTokens>,
    pub token: String,
    /// The read-only token (`<home>/token-read`).
    pub read_token: String,
    pub home: PathBuf,
    pub started_at: DateTime<Utc>,
    pub shutdown: CancellationToken,
    /// Set: stopping the hub stops every agent too. Clear: agents in runtime hosts are
    /// left running for the next hub.
    pub stop_agents: Arc<std::sync::atomic::AtomicBool>,
    /// How agents reach the hub's tools: the MCP command and the API base URL.
    pub mcp_command: Vec<String>,
    pub url: String,
    /// Runs the description's install and upgrade commands, one job per plugin at a time.
    pub installer: Arc<crate::install::Installer>,
}

impl AppState {
    fn store(&self) -> &Arc<dyn Store> {
        self.bus.store()
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    let authed = Router::new()
        .route("/v1/info", get(info))
        .route("/v1/agents", get(list_agents).post(create_agent))
        .route("/v1/session-candidates", get(session_candidates))
        .route("/v1/agents/{key}/chat", post(start_chat))
        .route("/v1/agents/{key}", get(get_agent).delete(delete_agent))
        .route("/v1/agents/{key}/start", post(start_agent))
        .route("/v1/agents/{key}/stop", post(stop_agent))
        .route("/v1/agents/{key}/cancel", post(cancel_agent))
        .route("/v1/agents/{key}/compact", post(compact_agent))
        .route("/v1/agents/{key}/commands", get(agent_commands))
        .route(
            "/v1/agents/{key}/messages",
            post(send_message).layer(DefaultBodyLimit::max(MESSAGE_BODY_LIMIT)),
        )
        .route("/v1/agents/{key}/attachments/{id}", get(get_attachment))
        .route(
            "/v1/agents/{key}/messages/{message_id}",
            delete(withdraw_message),
        )
        .route("/v1/agents/{key}/transcript", get(transcript))
        .route("/v1/agents/{key}/turns/{message_id}/diff", get(turn_diff))
        .route("/v1/agents/{key}/profile", post(update_profile))
        .route("/v1/agents/{key}/settings", post(agent_settings))
        .route("/v1/agents/{key}/model", post(set_model))
        .route("/v1/agents/{key}/name", post(rename_agent))
        .route("/v1/agents/{key}/topic", post(set_topic))
        .route("/v1/agents/{key}/archive", post(archive_agent))
        .route("/v1/agents/{key}/execution", get(agent_execution))
        .route(
            "/v1/agents/{key}/execution/interrupt",
            post(interrupt_execution),
        )
        .route("/v1/agents/{key}/resume-command", get(resume_command))
        .route("/v1/agents/{key}/connect", post(connect_agent))
        .route("/v1/notes", get(list_notes))
        .route("/v1/plugins", get(list_plugins))
        .route("/v1/plugins/{name}/models", post(set_plugin_models))
        .route("/v1/plugins/{name}/install", post(install_plugin))
        .route("/v1/plugins/{name}/upgrade", post(upgrade_plugin))
        .route("/v1/usage", get(usage))
        .route("/v1/usage/series", get(usage_series))
        .route("/v1/accounts", get(list_accounts).post(create_account))
        .route("/v1/quota", get(quota))
        .route("/v1/questions", get(list_questions))
        .route("/v1/questions/{id}/answer", post(answer_question))
        .route("/v1/questions/{id}/dismiss", post(dismiss_question))
        .route(
            "/v1/accounts/{id}",
            get(get_account).delete(delete_account_route),
        )
        .route("/v1/accounts/{id}/status", get(account_status))
        .route("/v1/accounts/{id}/usage", get(account_usage))
        .route("/v1/accounts/{id}/models", get(account_models))
        .route("/v1/accounts/{id}/login", post(account_login))
        .route("/v1/works", get(list_works).post(create_work))
        .route("/v1/works/{id}", get(get_work))
        .route("/v1/works/{id}/runs", post(create_work_run))
        .route("/v1/runs", get(list_runs).post(create_run))
        .route("/v1/runs/{key}", get(get_run).delete(delete_run))
        .route("/v1/runs/{key}/approve", post(approve_run))
        .route("/v1/runs/{key}/reject", post(reject_run))
        .route("/v1/runs/{key}/cancel", post(cancel_run))
        .route("/v1/runs/{key}/land", post(land_run))
        .route("/v1/runs/{key}/policy", post(update_run_policy))
        .route("/v1/runs/{key}/finish", post(finish_run))
        .route("/v1/runs/{key}/cleanup", post(cleanup_run))
        .route("/v1/runs/{key}/diff", get(run_diff))
        .route("/v1/runs/{key}/trace", get(run_trace))
        .route("/v1/runs/{key}/tasks/{task}/diff", get(task_diff))
        .route("/v1/runs/{key}/tasks/{task}/review", post(review_task))
        .route("/v1/agents/{key}/files/tree", get(agent_files_tree))
        .route("/v1/agents/{key}/files/content", get(agent_files_content))
        .route("/v1/agents/{key}/files/image", get(agent_files_image))
        .route("/v1/agents/{key}/files/resolve", post(agent_files_resolve))
        .route("/v1/runs/{key}/files/tree", get(run_files_tree))
        .route("/v1/runs/{key}/files/content", get(run_files_content))
        .route("/v1/runs/{key}/files/resolve", post(run_files_resolve))
        .route(
            "/v1/runs/{key}/tasks/{task}/files/tree",
            get(task_files_tree),
        )
        .route(
            "/v1/runs/{key}/tasks/{task}/files/content",
            get(task_files_content),
        )
        .route(
            "/v1/runs/{key}/tasks/{task}/files/resolve",
            post(task_files_resolve),
        )
        .route("/v1/runs/{key}/graph", get(run_graph))
        .route("/v1/agents/{key}/graph", get(agent_graph))
        .route("/v1/runs/{key}/commits/{sha}/diff", get(run_commit_diff))
        .route(
            "/v1/agents/{key}/commits/{sha}/diff",
            get(agent_commit_diff),
        )
        .route("/v1/repos/init", post(init_repo))
        .route("/v1/events", get(events))
        .route("/v1/shutdown", post(shutdown))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token));
    // Routes for agents, authenticated with their per-session token.
    let me = Router::new()
        .route("/v1/me", get(me))
        .route("/v1/me/tools", get(my_tools))
        .route("/v1/me/tools/{name}", post(call_tool))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_agent_token,
        ));
    Router::new()
        .route("/v1/health", get(health))
        .merge(authed)
        .merge(me)
        .with_state(state)
}

// ---------------------------------------------------------------------------------------
// Errors

pub struct AppError(StatusCode, ApiError);

impl AppError {
    fn new(status: StatusCode, code: ErrorCode, message: impl Into<String>) -> Self {
        Self(status, ApiError::new(code, message))
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, message)
    }
}

impl From<CoreError> for AppError {
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
        let mut body = ApiError::new(code, err.to_string());
        // What is in the way, for a client that lists it (a run's checkout with changes).
        if let CoreError::ConflictFiles { files, .. } = &err {
            body.error.files = Some(files.clone());
        }
        Self(status, body)
    }
}

impl From<FileError> for AppError {
    fn from(err: FileError) -> Self {
        let (status, code) = match &err {
            // A path that leads out of the workspace is refused, not looked for.
            FileError::Outside(_) => (StatusCode::FORBIDDEN, ErrorCode::Forbidden),
            FileError::NotFound(_) => (StatusCode::NOT_FOUND, ErrorCode::NotFound),
            FileError::BadRequest(_) => (StatusCode::BAD_REQUEST, ErrorCode::BadRequest),
            FileError::Failed(_) => {
                tracing::error!(error = %err, "cannot read a workspace file");
                (StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal)
            }
        };
        Self::new(status, code, err.to_string())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

type ApiResult<T> = Result<T, AppError>;

// ---------------------------------------------------------------------------------------
// Auth

async fn require_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let grant = match presented {
        Some(token) => grant_of(&state, token).await,
        None => None,
    };
    let Some(grant) = grant else {
        return AppError::new(
            StatusCode::UNAUTHORIZED,
            ErrorCode::Unauthorized,
            "missing or invalid bearer token",
        )
        .into_response();
    };
    // The policy point (docs/tasks/DOGFOOD.md §7): the route's level against the grant's.
    let pattern = request
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| request.uri().path().to_owned());
    match crate::access::refusal(grant, request.method(), &pattern) {
        None => next.run(request).await,
        Some(why) => {
            AppError::new(StatusCode::FORBIDDEN, ErrorCode::Forbidden, why).into_response()
        }
    }
}

/// What a presented token may do: the hub's token everything, the read-only token read,
/// an agent's session token nothing here.
async fn grant_of(state: &AppState, token: &str) -> Option<crate::access::Grant> {
    use crate::access::{Grant, Level};
    if constant_time_eq(token.as_bytes(), state.token.as_bytes()) {
        Some(Grant::Level(Level::Admin))
    } else if constant_time_eq(token.as_bytes(), state.read_token.as_bytes()) {
        Some(Grant::Level(Level::Read))
    } else if state.tokens.agent(token).await.is_some() {
        Some(Grant::Agent)
    } else {
        None
    }
}

/// Resolves the agent behind a per-session token and passes it on as an extension.
async fn require_agent_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Response {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let agent = match presented {
        Some(token) => match state.tokens.agent(token).await {
            Some(id) => state.store().get_agent(&AgentKey::Id(id)).await.ok(),
            None => None,
        },
        None => None,
    };
    match agent {
        Some(agent) => {
            request.extensions_mut().insert(agent);
            next.run(request).await
        }
        None => AppError::new(
            StatusCode::UNAUTHORIZED,
            ErrorCode::Unauthorized,
            "missing or invalid agent token",
        )
        .into_response(),
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---------------------------------------------------------------------------------------
// Handlers

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".into(),
        api: API_VERSION.into(),
    })
}

async fn info(State(state): State<Arc<AppState>>) -> Json<HubInfo> {
    Json(HubInfo {
        version: VERSION.into(),
        commit: option_env!("CROSSAGENTS_COMMIT").map(str::to_owned),
        debug: cfg!(debug_assertions),
        api: API_VERSION.into(),
        pid: std::process::id(),
        home: state.home.display().to_string(),
        started_at: state.started_at,
    })
}

async fn list_agents(State(state): State<Arc<AppState>>) -> ApiResult<Json<ListAgentsResponse>> {
    let agents = state.store().list_agents().await?;
    Ok(Json(ListAgentsResponse { agents }))
}

#[derive(Deserialize)]
struct SessionCandidatesQuery {
    /// The workspace; absent: the standalone chats, each in its own `<home>/chats/<id>`.
    #[serde(default)]
    repo: Option<String>,
}

async fn session_candidates(
    State(state): State<Arc<AppState>>,
    Query(query): Query<SessionCandidatesQuery>,
) -> ApiResult<Json<crossagents_api_types::ListSessionCandidatesResponse>> {
    let candidates = match query.repo.as_deref().filter(|r| !r.is_empty()) {
        Some(repo) => {
            state
                .orchestrator
                .session_candidates(std::path::Path::new(repo))
                .await?
        }
        None => {
            state
                .orchestrator
                .chat_candidates(&state.home.join("chats"))
                .await?
        }
    };
    let sessions = candidates
        .into_iter()
        .map(|candidate| crossagents_api_types::SessionCandidate {
            can_use: candidate.can_use(),
            reason: candidate.reason.map(|r| r.word().to_owned()),
            agent: candidate.agent,
            state: candidate.state,
            busy_with: candidate.busy_with,
            last_execution: candidate.last_execution,
        })
        .collect();
    Ok(Json(crossagents_api_types::ListSessionCandidatesResponse {
        sessions,
    }))
}

/// The claim lookup is advisory: a run may claim after it; only claim_agent excludes
/// competing runs atomically. Chat itself does not claim.
async fn start_chat(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(req): Json<crossagents_api_types::StartChatRequest>,
) -> ApiResult<(StatusCode, Json<SendMessageResponse>)> {
    let uploads = req.attachments.unwrap_or_default();
    if req.text.trim().is_empty() && uploads.is_empty() {
        return Err(AppError::bad_request("text must not be empty"));
    }
    let key = AgentKey::parse(&key);
    let agent = state.store().get_agent(&key).await?;
    state.orchestrator.chat_available(&agent).await?;
    if agent.archived_at.is_some() {
        if !req.restore {
            return Err(CoreError::Conflict(format!(
                "{} is archived: restore it first with restore: true",
                agent.name
            ))
            .into());
        }
        state.runtime.archive(&key, false).await?;
    }
    let text = req.text;
    let attachments = store_uploads(&state, &key, uploads).await?;
    let envelope = Envelope {
        attachments,
        ..Envelope::new(MessageSource::Human, text.clone())
    };
    let (message_id, queued) = state.runtime.send_envelope(&key, envelope).await?;
    // Files alone leave the topic as it was.
    if !text.trim().is_empty() {
        let topic = text
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(60)
            .collect();
        state.runtime.set_topic(&key, Some(topic)).await?;
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(SendMessageResponse { message_id, queued }),
    ))
}

async fn create_agent(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateAgentRequest>,
) -> ApiResult<(StatusCode, Json<Agent>)> {
    let (name, name_source) = crate::identity::free_name(
        state.store().as_ref(),
        req.name.as_deref(),
        false,
        &crate::identity::denied_words(state.runtime.plugins()),
    )
    .await?;
    if req.plugin != crossagents_core::EXTERNAL_PLUGIN
        && !state.runtime.plugins().contains(&req.plugin)
    {
        return Err(AppError::bad_request(format!(
            "unknown plugin {:?} (available: {})",
            req.plugin,
            state.runtime.plugins().names().join(", ")
        )));
    }
    if let Some(driver) = &req.driver {
        driver.parse::<DriverKind>()?;
    }
    let profile = req.profile.normalized()?;
    if let Some(account) = req.config.get("account").and_then(|v| v.as_str()) {
        state.accounts.check_binding(&req.plugin, account).await?;
    }
    let id = AgentId::new();
    // No directory: a chat outside any project gets its own under the hub home (by id: the
    // name can change).
    let dir = if req.cwd.trim().is_empty() {
        let dir = state.home.join("chats").join(id.to_string());
        std::fs::create_dir_all(&dir).map_err(|e| {
            AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::Internal,
                format!("creating {}: {e}", dir.display()),
            )
        })?;
        dir
    } else {
        req.cwd.clone().into()
    };
    let cwd = dunce::canonicalize(&dir)
        .map_err(|e| AppError::bad_request(format!("cwd {:?}: {e}", req.cwd)))?;
    if !cwd.is_dir() {
        return Err(AppError::bad_request(format!(
            "cwd {:?} is not a directory",
            req.cwd
        )));
    }
    let topic = req
        .topic
        .map(|t| t.trim().chars().take(120).collect::<String>())
        .filter(|t| !t.is_empty());
    let agent = Agent {
        id,
        name_source,
        topic,
        archived_at: None,
        home: cwd.clone(),
        name,
        plugin: req.plugin,
        driver: req.driver,
        model: req.model,
        role: req.role,
        cwd,
        config: req.config,
        status: AgentStatus::Stopped,
        created_at: Utc::now(),
        profile,
    };
    state.store().insert_agent(&agent).await?;
    state
        .bus
        .publish(EventKind::AgentCreated {
            agent: agent.clone(),
        })
        .await?;
    Ok((StatusCode::CREATED, Json(agent)))
}

async fn get_agent(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Json<Agent>> {
    Ok(Json(state.store().get_agent(&AgentKey::parse(&key)).await?))
}

/// Replaces the agent's card. A new persona reaches the agent when its next session opens.
async fn update_profile(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(profile): Json<AgentProfile>,
) -> ApiResult<Json<Agent>> {
    let profile = profile.normalized()?;
    let mut agent = state.store().get_agent(&AgentKey::parse(&key)).await?;
    state
        .store()
        .update_agent_profile(agent.id, &profile)
        .await?;
    agent.profile = profile;
    state
        .bus
        .publish(EventKind::AgentUpdated {
            agent: agent.clone(),
        })
        .await?;
    Ok(Json(agent))
}

/// `POST /v1/agents/{key}/settings`: which of Claude Code's settings the agent loads
/// (docs/tasks/DOGFOOD.md §6). The CLI reads them only at launch, so an idle session is
/// stopped and resumes with them on the next message; a busy one keeps the old ones until it
/// next starts.
async fn agent_settings(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(req): Json<AgentSettingsRequest>,
) -> ApiResult<Json<Agent>> {
    let mut agent = state.store().get_agent(&AgentKey::parse(&key)).await?;
    if !agent.config.is_object() {
        agent.config = serde_json::json!({});
    }
    let before = agent.config.get("setting_sources").cloned();
    match req.setting_sources {
        None => {
            agent
                .config
                .as_object_mut()
                .unwrap()
                .remove("setting_sources");
        }
        Some(sources) => {
            let sources: Vec<&str> = sources
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            if let Some(bad) = sources
                .iter()
                .find(|s| !["user", "project", "local"].contains(s))
            {
                return Err(AppError::bad_request(format!(
                    "unknown setting source {bad:?} (user, project, local)"
                )));
            }
            agent.config["setting_sources"] = sources.join(",").into();
        }
    }
    state
        .store()
        .update_agent_config(agent.id, &agent.config)
        .await?;
    if agent.config.get("setting_sources").cloned() != before && agent.status == AgentStatus::Idle {
        state.runtime.stop(&AgentKey::Id(agent.id)).await?;
        agent.status = AgentStatus::Stopped;
    }
    state
        .bus
        .publish(EventKind::AgentUpdated {
            agent: agent.clone(),
        })
        .await?;
    Ok(Json(agent))
}

/// Folds every stored event of `agent` up to now.
async fn fold_agent(state: &AppState, agent: AgentId) -> Result<crate::transcript::Fold, AppError> {
    let mut fold = crate::transcript::Fold::default();
    loop {
        let page = state
            .store()
            .agent_events(agent, fold.last_seq, EVENT_PAGE)
            .await?;
        if page.is_empty() {
            break;
        }
        page.iter().for_each(|e| fold.push(e));
    }
    Ok(fold)
}

/// Rebuilds the agent's conversation from its stored events.
async fn transcript(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Json<Transcript>> {
    let agent = state.store().get_agent(&AgentKey::parse(&key)).await?;
    let resolved = state
        .runtime
        .plugins()
        .driver(&agent.plugin, agent.driver.as_deref())
        .ok();
    // Clients follow the global stream from `last_seq`. Read the head first: every event
    // of this agent up to it is then folded in, and later ones arrive on the stream.
    let head = state.store().latest_event_seq().await?;
    let fold = fold_agent(&state, agent.id).await?;
    // The conversation's spend includes its recruits': nothing is spent out of sight.
    let mut costs = vec![fold.usage.as_ref().and_then(|u| u.cost_usd)];
    for recruit in state.store().list_agents().await? {
        if recruit.recruiter() == Some(agent.id) {
            costs.push(
                fold_agent(&state, recruit.id)
                    .await?
                    .usage
                    .and_then(|u| u.cost_usd),
            );
        }
    }
    let conversation_cost_usd = costs
        .iter()
        .any(Option::is_some)
        .then(|| costs.iter().flatten().sum());
    let in_error = agent.status == crossagents_core::AgentStatus::Error;
    let error = in_error.then_some(fold.last_error).flatten();
    let exit = in_error.then_some(fold.last_exit).flatten();
    let capabilities = state.runtime.capabilities(&agent).await;
    Ok(Json(Transcript {
        agent,
        driver: resolved.as_ref().map(|r| r.driver.kind()),
        capabilities,
        turns: fold.turns,
        usage: fold.usage,
        switches: fold.switches,
        conversation_cost_usd,
        error,
        exit,
        last_seq: head.max(fold.last_seq),
    }))
}

/// What one turn changed, as a unified diff.
async fn turn_diff(
    State(state): State<Arc<AppState>>,
    Path((key, message_id)): Path<(String, String)>,
) -> ApiResult<Json<TurnDiffResponse>> {
    let agent = state.store().get_agent(&AgentKey::parse(&key)).await?;
    let message_id: MessageId = message_id
        .parse()
        .map_err(|_| AppError::bad_request(format!("invalid message id: {message_id}")))?;
    let mut fold = fold_agent(&state, agent.id).await?;
    let not_found = || {
        AppError::new(
            StatusCode::NOT_FOUND,
            ErrorCode::NotFound,
            format!("turn {message_id} of {} changed nothing", agent.name),
        )
    };
    let stat = fold
        .turns
        .iter()
        .find(|t| t.message_id == message_id)
        .and_then(|t| t.diff.clone())
        .ok_or_else(not_found)?;
    let diff = fold.diffs.remove(&message_id).ok_or_else(not_found)?;
    Ok(Json(TurnDiffResponse {
        message_id,
        diff,
        stat,
    }))
}

async fn delete_agent(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<StatusCode> {
    let agent = state.store().get_agent(&AgentKey::parse(&key)).await?;
    state.runtime.stop(&AgentKey::Id(agent.id)).await?;
    // A lead keeping an ended run's checkout: the checkout goes with it.
    if let Err(err) = state.orchestrator.lead_left(&agent).await {
        tracing::warn!(agent = %agent.name, error = %err, "cannot remove the run checkout");
    }
    state.store().delete_agent(agent.id).await?;
    if let Err(err) = crate::attachments::remove(&state.home, agent.id) {
        tracing::warn!(agent = %agent.name, error = %err, "cannot remove the agent's attachments");
    }
    state
        .bus
        .publish(EventKind::AgentRemoved {
            id: agent.id,
            name: agent.name,
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn start_agent(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Json<Agent>> {
    let agent = state.runtime.start(&AgentKey::parse(&key)).await?;
    Ok(Json(
        state.store().get_agent(&AgentKey::Id(agent.id)).await?,
    ))
}

async fn rename_agent(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(req): Json<RenameAgentRequest>,
) -> ApiResult<Json<Agent>> {
    Ok(Json(
        state
            .runtime
            .rename(&AgentKey::parse(&key), &req.name)
            .await?,
    ))
}

async fn set_topic(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(req): Json<SetTopicRequest>,
) -> ApiResult<Json<Agent>> {
    Ok(Json(
        state
            .runtime
            .set_topic(&AgentKey::parse(&key), req.topic)
            .await?,
    ))
}

/// What the session works on for a run now (docs/tasks/S1.md §3.3).
async fn agent_execution(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Json<AgentExecutionResponse>> {
    let agent = state.store().get_agent(&AgentKey::parse(&key)).await?;
    let execution = state
        .orchestrator
        .execution_of(agent.id)
        .await?
        .map(|e| AgentExecution {
            run_id: e.claim.run_id,
            run_title: e.run_title,
            task_id: e.claim.task_id,
            task_key: e.task_key,
            role: e.claim.role,
            fresh: e.claim.fresh,
            since: e.claim.since,
        });
    Ok(Json(AgentExecutionResponse { execution }))
}

/// The command that carries the session on in its own CLI (docs/TODO.md §十五). Always 200
/// for a known agent: why it cannot be done is part of the answer.
async fn resume_command(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Json<crossagents_api_types::ResumeCommandResponse>> {
    Ok(Json(
        state.runtime.resume_command(&AgentKey::parse(&key)).await?,
    ))
}

/// The person stops what the session does on a task: the execution ends as interrupted, the
/// session goes back to its workspace (or is archived, an execution session), and the task's
/// issuer is told. Returns the task.
async fn interrupt_execution(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Json<Task>> {
    let agent = state.store().get_agent(&AgentKey::parse(&key)).await?;
    Ok(Json(
        state.orchestrator.interrupt_execution(agent.id).await?,
    ))
}

async fn archive_agent(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(req): Json<ArchiveAgentRequest>,
) -> ApiResult<Json<Agent>> {
    let agent = state
        .runtime
        .archive(&AgentKey::parse(&key), req.archived)
        .await?;
    // A lead keeping an ended run's checkout gives it up while archived (docs/tasks/W1.md §11).
    let checkout = if req.archived {
        state.orchestrator.lead_left(&agent).await
    } else {
        state.orchestrator.lead_returned(&agent).await
    };
    if let Err(err) = checkout {
        tracing::warn!(agent = %agent.name, error = %err, "cannot update the run checkout");
    }
    Ok(Json(agent))
}

async fn set_model(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(req): Json<SetModelRequest>,
) -> ApiResult<Json<Agent>> {
    let model = req
        .model
        .map(|m| m.trim().to_owned())
        .filter(|m| !m.is_empty());
    let effort = req.effort.map(|e| e.trim().to_owned());
    Ok(Json(
        state
            .runtime
            .set_model(&AgentKey::parse(&key), model, effort)
            .await?,
    ))
}

/// Lets an agent that runs outside the hub join: a fresh token (the previous one stops
/// working) and the MCP configuration to paste into it.
async fn connect_agent(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Json<ConnectInfo>> {
    let agent = state.store().get_agent(&AgentKey::parse(&key)).await?;
    if agent.plugin != crossagents_core::EXTERNAL_PLUGIN {
        return Err(AppError::bad_request(format!(
            "{} is run by the hub; only external agents connect themselves",
            agent.name
        )));
    }
    let token = state.tokens.issue(agent.id).await;
    let (command, args) = state
        .mcp_command
        .split_first()
        .map(|(c, a)| (c.clone(), a.to_vec()))
        .unwrap_or_default();
    let env = serde_json::json!({
        crate::collab::URL_ENV: state.url,
        crate::collab::TOKEN_ENV: token,
    });
    let json_config = serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": { "crossagents": { "command": command, "args": args, "env": env } }
    }))
    .unwrap_or_default();
    let toml_args = args
        .iter()
        .map(|a| format!("{a:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let toml_config = format!(
        "[mcp_servers.crossagents]\ncommand = {command:?}\nargs = [{toml_args}]\nenv = {{ {} = {:?}, {} = {:?} }}\n",
        crate::collab::URL_ENV,
        state.url,
        crate::collab::TOKEN_ENV,
        token
    );
    Ok(Json(ConnectInfo {
        agent: agent.name,
        token,
        json_config,
        toml_config,
        instructions: crossagents_orchestrator::protocol::guide(agent.role),
    }))
}

#[derive(Debug, Deserialize)]
struct NotesQuery {
    #[serde(default)]
    scope: crossagents_core::NoteScope,
    #[serde(default)]
    run: Option<String>,
}

async fn list_notes(
    State(state): State<Arc<AppState>>,
    Query(query): Query<NotesQuery>,
) -> ApiResult<Json<ListNotesResponse>> {
    let run_id = match (&query.scope, &query.run) {
        (crossagents_core::NoteScope::Run, Some(key)) => Some(resolve_run(&state, key).await?),
        _ => None,
    };
    Ok(Json(ListNotesResponse {
        notes: state.store().list_notes(query.scope, run_id, "").await?,
    }))
}

async fn stop_agent(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<StatusCode> {
    state.runtime.stop(&AgentKey::parse(&key)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn cancel_agent(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<StatusCode> {
    state.runtime.cancel(&AgentKey::parse(&key)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Has the agent summarise its history to free context.
async fn compact_agent(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<StatusCode> {
    state.runtime.compact(&AgentKey::parse(&key)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The slash commands and skills the agent's own CLI brings, when it reports them.
async fn agent_commands(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Json<AgentCommandsResponse>> {
    Ok(Json(state.runtime.commands(&AgentKey::parse(&key)).await?))
}

/// Takes back a message that is still waiting for its turn.
async fn withdraw_message(
    State(state): State<Arc<AppState>>,
    Path((key, message_id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let message_id: MessageId = message_id
        .parse()
        .map_err(|_| AppError::bad_request("invalid message id"))?;
    state
        .runtime
        .withdraw(&AgentKey::parse(&key), message_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn send_message(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(req): Json<SendMessageRequest>,
) -> ApiResult<(StatusCode, Json<SendMessageResponse>)> {
    let uploads = req.attachments.unwrap_or_default();
    if req.text.trim().is_empty() && uploads.is_empty() {
        return Err(AppError::bad_request("text must not be empty"));
    }
    let key = AgentKey::parse(&key);
    let attachments = store_uploads(&state, &key, uploads).await?;
    let envelope = Envelope {
        attachments,
        ..Envelope::new(MessageSource::Human, req.text)
    };
    let (message_id, queued) = if req.now == Some(true) {
        state.runtime.send_now_envelope(&key, envelope).await?
    } else {
        state.runtime.send_envelope(&key, envelope).await?
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(SendMessageResponse { message_id, queued }),
    ))
}

/// Checks `uploads` against what the agent takes and stores them for it; nothing to do
/// without uploads.
async fn store_uploads(
    state: &AppState,
    key: &AgentKey,
    uploads: Vec<AttachmentUpload>,
) -> ApiResult<Vec<Attachment>> {
    if uploads.is_empty() {
        return Ok(Vec::new());
    }
    let agent = state.store().get_agent(key).await?;
    // Refuse a kind the agent does not take, before storing anything (docs/TODO.md §7); an
    // ACP agent declares its kinds when its session starts.
    let support = state
        .runtime
        .attachment_support(&agent)
        .await?
        .unwrap_or(AttachmentSupport::TEXT_ONLY);
    let driver = state
        .runtime
        .plugins()
        .driver(&agent.plugin, agent.driver.as_deref())
        .map(|resolved| resolved.driver.kind().as_str())
        .unwrap_or("unknown");
    crate::attachments::check_support(&uploads, support, &agent.name, driver)
        .map_err(|refused| AppError::bad_request(refused.0))?;
    crate::attachments::store(&state.home, agent.id, uploads)
        .map_err(|refused| AppError::bad_request(refused.0))
}

/// One file sent to the agent, for clients to show.
async fn get_attachment(
    State(state): State<Arc<AppState>>,
    Path((key, id)): Path<(String, String)>,
) -> ApiResult<Json<AttachmentContent>> {
    let agent = state.store().get_agent(&AgentKey::parse(&key)).await?;
    crate::attachments::read(&state.home, agent.id, &id)
        .map(Json)
        .ok_or_else(|| {
            CoreError::NotFound {
                kind: "attachment",
                id,
            }
            .into()
        })
}

async fn list_plugins(State(state): State<Arc<AppState>>) -> Json<ListPluginsResponse> {
    let registry = state.runtime.plugins();
    let mut plugins = Vec::new();
    for d in registry.defaults() {
        plugins.push(plugin_info(&state, d.as_ref()).await);
    }
    Json(ListPluginsResponse { plugins })
}

async fn plugin_info(state: &AppState, d: &dyn crossagents_core::Driver) -> PluginInfo {
    let registry = state.runtime.plugins();
    let name = d.name();
    let all = crate::models::available(registry, &state.accounts, name).await;
    // A CLI this machine does not have runs nothing: offer none of its models.
    let models = if d.installed() {
        state.models.filter(name, all.clone())
    } else {
        Vec::new()
    };
    let cli = d.cli_status().await.map(|status| CliInfo {
        installed: status.located.is_some(),
        path: status
            .located
            .as_ref()
            .map(|located| located.path.display().to_string()),
        source: status
            .located
            .as_ref()
            .map(|located| located.source.as_str().to_owned()),
        version: status
            .located
            .as_ref()
            .and_then(|located| located.version.clone()),
        install_hint: status.install_hint,
        docs: status.docs,
        installed_via: status.installed_via,
        install_command: status.install_command,
        upgrade_command: status.upgrade_command,
        can_upgrade: status.can_upgrade,
        described_by: status.described_by,
    });
    PluginInfo {
        name: name.to_owned(),
        models,
        all_models: all,
        shown: state.models.shown(name),
        models_pinned: registry.models_pinned(name),
        driver: d.kind(),
        capabilities: d.capabilities(),
        // The ACP driver does not pass an effort on, so none is offered there.
        efforts: match d.kind() {
            DriverKind::AcpGeneric => Vec::new(),
            _ => registry
                .config(name)
                .map(|c| c.efforts.clone())
                .unwrap_or_default(),
        },
        cli,
    }
}

/// Picks the models a plugin offers (the settings page); `null` offers them all.
async fn set_plugin_models(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(req): Json<SetPluginModelsRequest>,
) -> ApiResult<Json<PluginInfo>> {
    let driver = state
        .runtime
        .plugins()
        .get(&name)
        .ok_or_else(|| CoreError::NotFound {
            kind: "plugin",
            id: name.clone(),
        })?;
    state.models.set(&name, req.shown)?;
    Ok(Json(plugin_info(&state, driver.as_ref()).await))
}

/// `POST /v1/plugins/{name}/install`: runs the description's install command on this machine
/// (admin: it executes a program here). `202` once it starts; the event stream says how it
/// went (docs/DESIGN.md §9.3).
async fn install_plugin(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<(StatusCode, Json<CliInstallAccepted>)> {
    run_cli_job(&state, &name, crate::install::Action::Install).await
}

/// `POST /v1/plugins/{name}/upgrade`: the same for the upgrade command, when the description
/// has one and the CLI is one of its own installs.
async fn upgrade_plugin(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<(StatusCode, Json<CliInstallAccepted>)> {
    run_cli_job(&state, &name, crate::install::Action::Upgrade).await
}

async fn run_cli_job(
    state: &AppState,
    name: &str,
    action: crate::install::Action,
) -> ApiResult<(StatusCode, Json<CliInstallAccepted>)> {
    let registry = state.runtime.plugins();
    let not_found = || CoreError::NotFound {
        kind: "plugin",
        id: name.to_owned(),
    };
    let config = registry.config(name).ok_or_else(not_found)?;
    let driver = registry.get(name).ok_or_else(not_found)?;
    let status = driver.cli_status().await.ok_or_else(|| {
        AppError::bad_request(format!("{name} launches no CLI that this machine installs"))
    })?;
    let argv = match action {
        crate::install::Action::Install => status.install_command.clone(),
        crate::install::Action::Upgrade => status.upgrade_command.clone(),
    }
    .ok_or_else(|| match action {
        crate::install::Action::Install => AppError::bad_request(format!(
            "the description of {name} says no way to install it on this machine"
        )),
        crate::install::Action::Upgrade => AppError::from(CoreError::Conflict(format!(
            "{name} cannot be upgraded here: {}",
            why_not_upgradable(&status)
        ))),
    })?;
    let job = crate::install::JobSpec {
        plugin: name.to_owned(),
        action,
        argv: argv.clone(),
        described_by: config.described_by.clone(),
    };
    state
        .installer
        .start(state.bus.clone(), driver.clone(), job)
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(CliInstallAccepted {
            plugin: name.to_owned(),
            action: action.as_str().to_owned(),
            argv,
        }),
    ))
}

/// Why the GUI (and this API) does not offer an upgrade for what was found.
fn why_not_upgradable(status: &crossagents_core::CliStatus) -> String {
    match status.located.as_ref().map(|found| found.source.as_str()) {
        Some("bundled") => {
            "it is the copy the desktop app ships; update the desktop app instead".to_owned()
        }
        Some("config") | Some("env") => {
            "it is the program your configuration names, so the hub does not touch it".to_owned()
        }
        _ => "no install method in its description matches where it was found".to_owned(),
    }
}

// ---------------------------------------------------------------------------------------
// Runs

/// A run by full id, or by a unique prefix or suffix of it (the suffix appears in branch
/// names).
async fn resolve_run(state: &AppState, key: &str) -> ApiResult<RunId> {
    if let Ok(id) = key.parse::<RunId>() {
        return Ok(id);
    }
    let key = key.trim().to_lowercase().replace('-', "");
    if key.len() < 4 {
        return Err(AppError::bad_request(
            "give at least 4 characters of the run id",
        ));
    }
    let matches: Vec<RunId> = state
        .store()
        .list_runs()
        .await?
        .into_iter()
        .map(|r| r.id)
        .filter(|id| {
            let hex = id.to_string().replace('-', "");
            hex.starts_with(&key) || hex.ends_with(&key)
        })
        .collect();
    match matches.as_slice() {
        [id] => Ok(*id),
        [] => Err(CoreError::NotFound {
            kind: "run",
            id: key,
        }
        .into()),
        _ => Err(AppError::bad_request(format!(
            "{key:?} matches several runs"
        ))),
    }
}

async fn agent_id(state: &AppState, key: &str) -> ApiResult<AgentId> {
    Ok(state.store().get_agent(&AgentKey::parse(key)).await?.id)
}

async fn create_work(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateWorkRequest>,
) -> ApiResult<(StatusCode, Json<crossagents_core::Work>)> {
    if req.title.trim().is_empty() {
        return Err(CoreError::Conflict("the work title is empty".into()).into());
    }
    let lead = state.store().get_agent(&AgentKey::parse(&req.lead)).await?;
    let now = Utc::now();
    let work = crossagents_core::Work {
        id: crossagents_core::WorkId::new(),
        title: req.title,
        repo: lead.home().to_path_buf(),
        lead: lead.id,
        created_at: now,
        updated_at: now,
    };
    state.store().insert_work(&work).await?;
    state
        .bus
        .publish(EventKind::WorkCreated { work: work.clone() })
        .await?;
    Ok((StatusCode::CREATED, Json(work)))
}

async fn list_works(State(state): State<Arc<AppState>>) -> ApiResult<Json<ListWorksResponse>> {
    Ok(Json(ListWorksResponse {
        works: state.store().list_works().await?,
    }))
}

async fn get_work(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<WorkDetail>> {
    let id = id
        .parse::<crossagents_core::WorkId>()
        .map_err(|_| CoreError::Conflict("invalid work id".into()))?;
    let work = state.store().get_work(id).await?;
    let runs = state
        .store()
        .list_runs()
        .await?
        .into_iter()
        .filter(|r| r.work_id == Some(id))
        .collect();
    Ok(Json(WorkDetail { work, runs }))
}

async fn create_work_run(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<CreateWorkRunRequest>,
) -> ApiResult<(StatusCode, Json<Run>)> {
    let id = id
        .parse::<crossagents_core::WorkId>()
        .map_err(|_| CoreError::Conflict("invalid work id".into()))?;
    let mut work = state.store().get_work(id).await?;
    let previous = state
        .store()
        .list_runs()
        .await?
        .into_iter()
        .find(|r| r.work_id == Some(id));
    let config = previous.map(|r| r.config).unwrap_or(RunConfig {
        worker: work.lead,
        escalate: None,
        reviewer: None,
        auto_approve: false,
        max_parallel: RunConfig::DEFAULT_MAX_PARALLEL,
        max_attempts: RunConfig::DEFAULT_MAX_ATTEMPTS,
        team: Vec::new(),
        policy: Default::default(),
    });
    let new = NewRun {
        origin_chat_id: None,
        request: req.request,
        title: req.title,
        planner: work.lead,
        config,
        repo: Some(work.repo.clone()),
        base_branch: None,
        attachments: Vec::new(),
    };
    let run = state
        .orchestrator
        .create_run_for_work(new, None, false, Some(id))
        .await?;
    work.updated_at = Utc::now();
    state.store().update_work(&work).await?;
    state.bus.publish(EventKind::WorkUpdated { work }).await?;
    Ok((StatusCode::CREATED, Json(run)))
}

async fn create_run(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateRunRequest>,
) -> ApiResult<(StatusCode, Json<Run>)> {
    let escalate = match &req.escalate {
        Some(key) => Some(agent_id(&state, key).await?),
        None => None,
    };
    let reviewer = match &req.reviewer {
        Some(key) => Some(agent_id(&state, key).await?),
        None => None,
    };
    let mut team = Vec::new();
    for name in req.team.iter().flatten() {
        team.push(agent_id(&state, name).await?);
    }
    let planner = agent_id(&state, &req.planner).await?;
    let worker = match &req.worker {
        Some(key) => agent_id(&state, key).await?,
        None => planner,
    };
    // The planner's workspace, wherever it works now (an ended run's checkout, say), from
    // the branch it is on (docs/tasks/S1.md §3.2).
    let planner_agent = state.store().get_agent(&AgentKey::Id(planner)).await?;
    let (repo, base_branch) = (planner_agent.home().to_path_buf(), None);
    let attachments = store_uploads(
        &state,
        &AgentKey::Id(planner),
        req.attachments.unwrap_or_default(),
    )
    .await?;
    let new = NewRun {
        attachments,
        origin_chat_id: None,
        request: req.request,
        title: req.title,
        planner,
        repo: Some(repo),
        base_branch,
        config: RunConfig {
            worker,
            escalate,
            reviewer,
            auto_approve: req.auto_approve,
            max_parallel: req.max_parallel.unwrap_or(RunConfig::DEFAULT_MAX_PARALLEL),
            max_attempts: req.max_attempts.unwrap_or(RunConfig::DEFAULT_MAX_ATTEMPTS),
            team,
            policy: req.policy.unwrap_or_default(),
        },
    };
    let run = state
        .orchestrator
        .create_run_with_choice(new, req.fresh, req.restore)
        .await?;
    Ok((StatusCode::CREATED, Json(run)))
}

async fn list_runs(State(state): State<Arc<AppState>>) -> ApiResult<Json<ListRunsResponse>> {
    Ok(Json(ListRunsResponse {
        runs: state.store().list_runs().await?,
    }))
}

async fn run_detail(state: &AppState, id: RunId) -> ApiResult<Json<RunDetail>> {
    Ok(Json(RunDetail {
        run: state.store().get_run(id).await?,
        tasks: state.store().list_tasks(id).await?,
    }))
}

async fn get_run(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Json<RunDetail>> {
    let id = resolve_run(&state, &key).await?;
    run_detail(&state, id).await
}

async fn approve_run(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    req: Option<Json<ApproveRunRequest>>,
) -> ApiResult<Json<RunDetail>> {
    let id = resolve_run(&state, &key).await?;
    let req = req.map(|Json(r)| r).unwrap_or_default();
    let mut assignees = Vec::new();
    for (task, worker) in req.assignees.into_iter().flatten() {
        let worker = match worker {
            Some(key) => Some(agent_id(&state, &key).await?),
            None => None,
        };
        assignees.push((task, worker));
    }
    let approval = Approval {
        policy: req.policy,
        max_parallel: req.max_parallel,
        assignees,
    };
    state.orchestrator.approve(id, approval).await?;
    run_detail(&state, id).await
}

async fn reject_run(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(req): Json<RejectRunRequest>,
) -> ApiResult<Json<RunDetail>> {
    let id = resolve_run(&state, &key).await?;
    state.orchestrator.reject(id, req.reason).await?;
    run_detail(&state, id).await
}

async fn cancel_run(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Json<RunDetail>> {
    let id = resolve_run(&state, &key).await?;
    state.orchestrator.cancel(id).await?;
    run_detail(&state, id).await
}

/// What `DELETE /v1/runs/{key}` takes: delete the run's branches too, and delete it even with
/// uncommitted work in its checkout (docs/tasks/R1.md §3.2).
#[derive(Debug, Default, Deserialize)]
struct DeleteRunQuery {
    #[serde(default)]
    branches: bool,
    #[serde(default)]
    force: bool,
}

/// Deletes a run that ended: its row, its tasks, the agents it started, its working copies
/// and, by choice, its branches. A checkout with unlanded work answers 409 with the files.
async fn delete_run(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Query(q): Query<DeleteRunQuery>,
) -> ApiResult<Json<DeleteRunResponse>> {
    let id = resolve_run(&state, &key).await?;
    let done = state
        .orchestrator
        .delete_run(
            id,
            DeleteRun {
                branches: q.branches,
                force: q.force,
            },
        )
        .await?;
    Ok(Json(DeleteRunResponse {
        agents: crossagents_api_types::DeletedRunAgents {
            returned: done.agents.returned,
            archived: done.agents.archived,
        },
        worktrees: done.worktrees,
        branches: done.branches,
        warnings: done.warnings,
    }))
}

/// The person lands a run (whatever the run's policy says about the lead).
async fn land_run(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(req): Json<LandRequest>,
) -> ApiResult<Json<RunDetail>> {
    let id = resolve_run(&state, &key).await?;
    state
        .orchestrator
        .land(id, req.mode.unwrap_or(LandMode::Merge), "person", None)
        .await?;
    run_detail(&state, id).await
}

/// The person changes how a run that is already going continues by itself, and may hand its
/// allowance out again (a run whose wakeups ran out otherwise stays where it is until the
/// person writes to the lead). The clock reads the policy back on every tick, so the write
/// is all it takes; `null` on `continuation` in the run is what the clock checks.
async fn update_run_policy(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(req): Json<UpdateRunPolicyRequest>,
) -> ApiResult<Json<RunDetail>> {
    if req.continuation.is_none() && !req.reset_attempts {
        return Err(AppError::bad_request(
            "say what to change: continuation (on or off, with its limits) or reset_attempts",
        ));
    }
    let id = resolve_run(&state, &key).await?;
    let mut run = state.store().get_run(id).await?;
    if let Some(update) = req.continuation {
        if update.enabled {
            // Kept as a value and edited field by field: a limit the request does not
            // mention keeps what the run had (or the default when it had no policy), and a
            // limit added to the policy later does not have to be named here.
            let mut policy = run.config.policy.continuation.take().unwrap_or_default();
            if let Some(max_attempts) = update.max_attempts {
                policy.max_attempts = max_attempts;
            }
            if let Some(interval_seconds) = update.interval_seconds {
                policy.interval_seconds = interval_seconds;
            }
            // Stated or not is the question here: `null` turns the stuck report off, while
            // saying nothing about it keeps the run's own setting.
            if let Some(stuck_minutes) = update.stuck_minutes {
                policy.stuck_minutes = stuck_minutes;
            }
            run.config.policy.continuation = Some(policy);
        } else {
            // Off leaves nothing behind for the clock (or a client) to read as still on.
            run.config.policy.continuation = None;
            run.continuation = None;
        }
    }
    if req.reset_attempts
        && let Some(mut continuation) = run.continuation.take()
    {
        // A fresh allowance: the cap is counted from zero, and the fact that the person
        // was told the old one ran out is forgotten, so the next exhaustion tells again.
        continuation.attempts = 0;
        continuation.exhausted_notified = false;
        // The clock arms the next wakeup from the interval it reads; it does not fire
        // one immediately.
        continuation.next_at = None;
        run.continuation = Some(continuation);
    }
    run.updated_at = Utc::now();
    state.store().update_run(&run).await?;
    if let Some(continuation) = run.continuation.as_deref()
        && let Err(error) = state
            .bus
            .publish(EventKind::RunContinuationChanged {
                id,
                state: continuation.clone(),
            })
            .await
    {
        tracing::error!(%error, run = %id, "cannot publish a policy change");
    }
    run_detail(&state, id).await
}

/// What happened in a run, by task, from its events (docs/tasks/DOGFOOD.md §8).
async fn run_trace(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Json<RunTrace>> {
    let id = resolve_run(&state, &key).await?;
    let mut tracer = crate::trace::Tracer::new(id);
    let mut after = 0;
    loop {
        let page = state.store().events_since(after, EVENT_PAGE).await?;
        let Some(last) = page.last().map(|e| e.seq) else {
            break;
        };
        after = last;
        for event in &page {
            tracer.push(event);
        }
    }
    let mut trace = tracer.finish();
    // The tasks as stored: titles, the latest status, failures; and tasks with no events.
    for task in state.store().list_tasks(id).await? {
        let found = trace.tasks.iter_mut().find(|t| t.task_id == Some(task.id));
        let entry = match found {
            Some(entry) => entry,
            None => {
                trace.tasks.push(TaskTrace {
                    task_id: Some(task.id),
                    key: task.key.clone(),
                    ..TaskTrace::default()
                });
                trace.tasks.last_mut().unwrap()
            }
        };
        entry.title = Some(task.title.clone());
        entry.status = Some(task.status);
        entry.failure = task.report.failure.clone();
    }
    Ok(Json(trace))
}

/// A task of `run` by id or key (`auth` or `#auth`).
async fn resolve_task(state: &AppState, run: RunId, key: &str) -> ApiResult<TaskId> {
    if let Ok(id) = key.parse::<TaskId>() {
        return Ok(id);
    }
    let key = key.trim().trim_start_matches('#');
    state
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

fn changes_response(changes: crossagents_orchestrator::Changes) -> Json<ChangesResponse> {
    Json(ChangesResponse {
        base: changes.base,
        head: changes.head,
        diff: changes.diff,
        truncated: changes.truncated,
    })
}

/// Everything a task changed, for the person's review (docs/tasks/DOGFOOD.md §2).
async fn task_diff(
    State(state): State<Arc<AppState>>,
    Path((key, task)): Path<(String, String)>,
) -> ApiResult<Json<ChangesResponse>> {
    let run = resolve_run(&state, &key).await?;
    let task = resolve_task(&state, run, &task).await?;
    Ok(changes_response(
        state.orchestrator.task_changes(run, task).await?,
    ))
}

/// What landing the run would bring into its base branch.
async fn run_diff(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Json<ChangesResponse>> {
    let run = resolve_run(&state, &key).await?;
    Ok(changes_response(state.orchestrator.run_changes(run).await?))
}

// ---------------------------------------------------------------------------------------
// Commit graph (read-only)

/// What `GET .../graph` takes: how many commits to hand out (200 when absent, at most 500).
#[derive(Debug, Default, Deserialize)]
struct GraphQuery {
    #[serde(default)]
    limit: Option<usize>,
}

fn graph_kind(kind: crossagents_orchestrator::graph::RefKind) -> GraphRefKind {
    use crossagents_orchestrator::graph::RefKind;
    match kind {
        RefKind::Base => GraphRefKind::Base,
        RefKind::Run => GraphRefKind::Run,
        RefKind::Task => GraphRefKind::Task,
        RefKind::Head => GraphRefKind::Head,
        RefKind::Other => GraphRefKind::Other,
    }
}

fn graph_response(graph: crossagents_orchestrator::Graph) -> Json<CommitGraph> {
    Json(CommitGraph {
        base_branch: graph.base_branch,
        run_branch: graph.run_branch,
        refs: graph
            .refs
            .into_iter()
            .map(|entry| GraphRef {
                name: entry.name,
                kind: graph_kind(entry.kind),
                sha: entry.sha,
                task_key: entry.task_key,
                task_title: entry.task_title,
            })
            .collect(),
        commits: graph
            .commits
            .into_iter()
            .map(|commit| GraphCommit {
                sha: commit.sha,
                parents: commit.parents,
                subject: commit.subject,
                author: commit.author,
                time: commit.time,
                refs: commit.refs,
            })
            .collect(),
        truncated: graph.truncated,
    })
}

/// The commits of a run's base, run and task branches.
async fn run_graph(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Query(q): Query<GraphQuery>,
) -> ApiResult<Json<CommitGraph>> {
    let run = resolve_run(&state, &key).await?;
    Ok(graph_response(
        state
            .orchestrator
            .run_graph(run, q.limit.unwrap_or(0))
            .await?,
    ))
}

/// The commits of the repository an agent works in: its HEAD and local branches.
async fn agent_graph(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Query(q): Query<GraphQuery>,
) -> ApiResult<Json<CommitGraph>> {
    let agent = state.store().get_agent(&AgentKey::parse(&key)).await?;
    Ok(graph_response(
        state
            .orchestrator
            .agent_graph(&agent, q.limit.unwrap_or(0))
            .await?,
    ))
}

/// A commit sha as the route carries it; anything else is refused before git sees it.
fn commit_sha(sha: &str) -> ApiResult<&str> {
    if !crossagents_orchestrator::graph::valid_sha(sha) {
        return Err(AppError::bad_request(format!(
            "{sha:?} is not a commit sha (4 to 40 lowercase hex digits)"
        )));
    }
    Ok(sha)
}

/// What one commit of a run changed.
async fn run_commit_diff(
    State(state): State<Arc<AppState>>,
    Path((key, sha)): Path<(String, String)>,
) -> ApiResult<Json<ChangesResponse>> {
    let run = resolve_run(&state, &key).await?;
    let sha = commit_sha(&sha)?;
    Ok(changes_response(
        state.orchestrator.run_commit_diff(run, sha).await?,
    ))
}

/// What one commit of an agent's repository changed.
async fn agent_commit_diff(
    State(state): State<Arc<AppState>>,
    Path((key, sha)): Path<(String, String)>,
) -> ApiResult<Json<ChangesResponse>> {
    let agent = state.store().get_agent(&AgentKey::parse(&key)).await?;
    let sha = commit_sha(&sha)?;
    Ok(changes_response(
        state.orchestrator.agent_commit_diff(&agent, sha).await?,
    ))
}

// ---------------------------------------------------------------------------------------
// Files (read-only)

/// What `GET {scope}/files/*` takes: the directory or file, a git ref to read it at, and
/// whether to list what .gitignore hides too.
#[derive(Debug, Default, Deserialize)]
struct FilesQuery {
    #[serde(default)]
    path: Option<String>,
    /// Read at this ref instead of the working copy (`ref` in the query string).
    #[serde(default, rename = "ref")]
    git_ref: Option<String>,
    /// List what .gitignore hides too, marked ignored.
    #[serde(default)]
    all: bool,
}

impl FilesQuery {
    fn path(&self) -> &str {
        self.path.as_deref().unwrap_or("")
    }

    fn git_ref(&self) -> Option<&str> {
        self.git_ref
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
    }
}

fn file_kind(kind: crossagents_orchestrator::files::Kind) -> FileKind {
    match kind {
        crossagents_orchestrator::files::Kind::Dir => FileKind::Dir,
        crossagents_orchestrator::files::Kind::File => FileKind::File,
        crossagents_orchestrator::files::Kind::Symlink => FileKind::Symlink,
    }
}

async fn agent_files_tree(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Query(q): Query<FilesQuery>,
) -> ApiResult<Json<FileTreeResponse>> {
    let agent = state.store().get_agent(&AgentKey::parse(&key)).await?;
    files_tree(&state, FilesScope::Agent(&agent), q).await
}

async fn run_files_tree(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Query(q): Query<FilesQuery>,
) -> ApiResult<Json<FileTreeResponse>> {
    let run = resolve_run(&state, &key).await?;
    files_tree(&state, FilesScope::Run(run), q).await
}

async fn task_files_tree(
    State(state): State<Arc<AppState>>,
    Path((key, task)): Path<(String, String)>,
    Query(q): Query<FilesQuery>,
) -> ApiResult<Json<FileTreeResponse>> {
    let run = resolve_run(&state, &key).await?;
    let task = resolve_task(&state, run, &task).await?;
    files_tree(&state, FilesScope::Task { run, task }, q).await
}

async fn agent_files_content(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Query(q): Query<FilesQuery>,
) -> ApiResult<Json<FileContentResponse>> {
    let agent = state.store().get_agent(&AgentKey::parse(&key)).await?;
    files_content(&state, FilesScope::Agent(&agent), q).await
}

/// `GET /v1/agents/{key}/files/image?path=`: an image a tool call named by path, base64 like
/// an attachment, so a client can show it as a `data:` URL (crate::image says what is
/// allowed; anything else is the usual error body with the reason).
async fn agent_files_image(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Query(q): Query<FilesQuery>,
) -> ApiResult<Json<ImageContent>> {
    let agent = state.store().get_agent(&AgentKey::parse(&key)).await?;
    let input = q.path();
    if input.trim().is_empty() {
        return Err(AppError::bad_request(
            "path is missing: `?path=<an image inside the agent's working directory or the system temp directory>`",
        ));
    }
    let root = state
        .orchestrator
        .files_root(FilesScope::Agent(&agent))
        .await?;
    let abs = crate::image::locate(&root, input)?;
    let (kind, bytes) = crate::image::load(&abs, input).await?;
    Ok(Json(ImageContent {
        path: abs.display().to_string(),
        mime: kind.content_type().to_owned(),
        size: bytes.len() as u64,
        data: STANDARD.encode(&bytes),
    }))
}

async fn run_files_content(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Query(q): Query<FilesQuery>,
) -> ApiResult<Json<FileContentResponse>> {
    let run = resolve_run(&state, &key).await?;
    files_content(&state, FilesScope::Run(run), q).await
}

async fn task_files_content(
    State(state): State<Arc<AppState>>,
    Path((key, task)): Path<(String, String)>,
    Query(q): Query<FilesQuery>,
) -> ApiResult<Json<FileContentResponse>> {
    let run = resolve_run(&state, &key).await?;
    let task = resolve_task(&state, run, &task).await?;
    files_content(&state, FilesScope::Task { run, task }, q).await
}

async fn agent_files_resolve(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(req): Json<ResolvePathsRequest>,
) -> ApiResult<Json<ResolvePathsResponse>> {
    let agent = state.store().get_agent(&AgentKey::parse(&key)).await?;
    files_resolve(&state, FilesScope::Agent(&agent), req).await
}

async fn run_files_resolve(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(req): Json<ResolvePathsRequest>,
) -> ApiResult<Json<ResolvePathsResponse>> {
    let run = resolve_run(&state, &key).await?;
    files_resolve(&state, FilesScope::Run(run), req).await
}

async fn task_files_resolve(
    State(state): State<Arc<AppState>>,
    Path((key, task)): Path<(String, String)>,
    Json(req): Json<ResolvePathsRequest>,
) -> ApiResult<Json<ResolvePathsResponse>> {
    let run = resolve_run(&state, &key).await?;
    let task = resolve_task(&state, run, &task).await?;
    files_resolve(&state, FilesScope::Task { run, task }, req).await
}

async fn files_tree(
    state: &AppState,
    scope: FilesScope<'_>,
    q: FilesQuery,
) -> ApiResult<Json<FileTreeResponse>> {
    let git_ref = q.git_ref().map(str::to_owned);
    let tree = state
        .orchestrator
        .files_tree(scope, q.path(), git_ref.as_deref(), q.all)
        .await?;
    Ok(Json(FileTreeResponse {
        root: tree.root.display().to_string(),
        path: tree.path,
        git_ref,
        entries: tree
            .entries
            .into_iter()
            .map(|entry| FileEntry {
                name: entry.name,
                path: entry.path,
                kind: file_kind(entry.kind),
                size: entry.size,
                ignored: entry.ignored,
            })
            .collect(),
    }))
}

async fn files_content(
    state: &AppState,
    scope: FilesScope<'_>,
    q: FilesQuery,
) -> ApiResult<Json<FileContentResponse>> {
    let git_ref = q.git_ref().map(str::to_owned);
    let file = state
        .orchestrator
        .files_content(scope, q.path(), git_ref.as_deref())
        .await?;
    Ok(Json(FileContentResponse {
        path: file.path,
        abs_path: file.abs_path.display().to_string(),
        size: file.contents.size,
        binary: file.contents.binary,
        truncated: file.contents.truncated,
        text: file.contents.text,
        git_ref,
    }))
}

/// Which of the strings name something inside the workspace: missing and outside paths are
/// `None`, in their order, and the call itself does not fail for them.
async fn files_resolve(
    state: &AppState,
    scope: FilesScope<'_>,
    req: ResolvePathsRequest,
) -> ApiResult<Json<ResolvePathsResponse>> {
    let found = state.orchestrator.files_resolve(scope, &req.paths).await?;
    Ok(Json(ResolvePathsResponse {
        entries: req
            .paths
            .into_iter()
            .zip(found)
            .map(|(input, hit)| ResolvedPath {
                input,
                path: hit.as_ref().map(|found| found.path.clone()),
                abs_path: hit
                    .as_ref()
                    .map(|found| found.abs_path.display().to_string()),
                kind: hit.map(|found| file_kind(found.kind)),
            })
            .collect(),
    }))
}

/// The person's comments on a task, to its worker; optionally sends the task back.
async fn review_task(
    State(state): State<Arc<AppState>>,
    Path((key, task)): Path<(String, String)>,
    Json(req): Json<TaskReviewRequest>,
) -> ApiResult<Json<Task>> {
    let run = resolve_run(&state, &key).await?;
    let task = resolve_task(&state, run, &task).await?;
    let remarks = req
        .comments
        .into_iter()
        .map(|c| crossagents_orchestrator::Remark {
            path: c.path,
            line: c.line,
            old_side: c.side == Some(DiffSide::Old),
            text: c.text,
        })
        .collect();
    Ok(Json(
        state
            .orchestrator
            .review_task(run, task, remarks, req.note, req.rework)
            .await?,
    ))
}

/// The person puts a run's directory under git, for the runs after it.
async fn init_repo(Json(req): Json<InitRepoRequest>) -> ApiResult<Json<InitRepoRequest>> {
    GitWorkspaces.init(std::path::Path::new(&req.path)).await?;
    Ok(Json(req))
}

/// Explicitly cleans a finished run's managed workspaces while retaining its history.
async fn cleanup_run(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Query(q): Query<DeleteRunQuery>,
) -> ApiResult<Json<crossagents_orchestrator::DeletedRun>> {
    let id = resolve_run(&state, &key).await?;
    Ok(Json(state.orchestrator.cleanup_run(id, q.force).await?))
}

/// The person closes a run; open tasks are cancelled.
async fn finish_run(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(req): Json<FinishRequest>,
) -> ApiResult<Json<RunDetail>> {
    let id = resolve_run(&state, &key).await?;
    state.orchestrator.finish(id, req.summary, true).await?;
    run_detail(&state, id).await
}

// ---------------------------------------------------------------------------------------
// Agent-facing routes

async fn me(Extension(agent): Extension<Agent>) -> Json<Agent> {
    Json(agent)
}

async fn my_tools(
    State(state): State<Arc<AppState>>,
    Extension(agent): Extension<Agent>,
) -> Json<ListToolsResponse> {
    Json(ListToolsResponse {
        tools: state.orchestrator.tools_for(&agent),
    })
}

async fn call_tool(
    State(state): State<Arc<AppState>>,
    Extension(agent): Extension<Agent>,
    Path(name): Path<String>,
    body: Option<Json<serde_json::Value>>,
) -> ApiResult<Json<serde_json::Value>> {
    let input = body.map(|Json(v)| v).unwrap_or(serde_json::Value::Null);
    tracing::info!(agent = %agent.name, tool = %name, "tool call");
    // Detached: a tool may end the caller's own session (e.g. a reviewer's verdict removes
    // the reviewer), which drops this request. The state change must still finish.
    let orchestrator = state.orchestrator.clone();
    let call = tokio::spawn(async move { orchestrator.call_tool(&agent, &name, input).await });
    let result = call
        .await
        .map_err(|e| AppError::from(CoreError::Agent(format!("tool call panicked: {e}"))))?;
    Ok(Json(result?))
}

// ---------------------------------------------------------------------------------------
// Accounts

async fn list_accounts(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<ListAccountsResponse>> {
    Ok(Json(ListAccountsResponse {
        accounts: state.store().list_accounts().await?,
    }))
}

#[derive(Debug, Deserialize)]
struct QuestionsQuery {
    /// Only what still waits for the person (default true).
    #[serde(default)]
    all: bool,
}

/// What agents put in front of the person: questions and notices.
async fn list_questions(
    State(state): State<Arc<AppState>>,
    Query(query): Query<QuestionsQuery>,
) -> ApiResult<Json<ListQuestionsResponse>> {
    Ok(Json(ListQuestionsResponse {
        items: state.store().list_human_items(!query.all).await?,
    }))
}

async fn answer_question(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<AnswerRequest>,
) -> ApiResult<Json<HumanItem>> {
    let answer = req.answer.trim().to_owned();
    if answer.is_empty() {
        return Err(AppError::bad_request("the answer is empty"));
    }
    Ok(Json(state.orchestrator.answer_human(&id, answer).await?))
}

async fn dismiss_question(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    state.orchestrator.dismiss_human(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Every account with its status, models and agents, read concurrently.
async fn quota(State(state): State<Arc<AppState>>) -> ApiResult<Json<QuotaResponse>> {
    let accounts = state.store().list_accounts().await?;
    let agents = state.store().list_agents().await?;
    let mut owners: Vec<(String, Option<String>)> = Vec::new();
    for agent in &agents {
        owners.push((agent.name.clone(), state.accounts.account_for(agent).await));
    }
    let views = futures::future::join_all(accounts.into_iter().map(|account| {
        let state = state.clone();
        let users: Vec<String> = owners
            .iter()
            .filter(|(_, id)| id.as_deref() == Some(account.id.as_str()))
            .map(|(name, _)| name.clone())
            .collect();
        async move {
            let status = state.accounts.status(&account.id).await?;
            let models = state.accounts.models(&account.id).await?;
            Ok::<_, CoreError>(AccountQuota {
                account,
                status,
                models,
                agents: users,
            })
        }
    }))
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(QuotaResponse {
        accounts: views,
        providers: state.accounts.provider_names(),
    }))
}

async fn create_account(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateAccountRequest>,
) -> ApiResult<(StatusCode, Json<Account>)> {
    Ok((StatusCode::CREATED, Json(state.accounts.create(req).await?)))
}

async fn get_account(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Account>> {
    Ok(Json(state.store().get_account(&id).await?))
}

async fn delete_account_route(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    state.accounts.remove(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn account_status(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<AccountStatus>> {
    Ok(Json(state.accounts.status(&id).await?))
}

#[derive(Debug, Deserialize)]
struct AccountUsageQuery {
    #[serde(default)]
    by: AccountUsageGroup,
}

async fn account_usage(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<AccountUsageQuery>,
) -> ApiResult<Json<AccountUsageResponse>> {
    Ok(Json(state.accounts.usage(&id, query.by).await?))
}

async fn account_models(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<crossagents_core::ModelInfo>>> {
    Ok(Json(state.accounts.models(&id).await?))
}

async fn account_login(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<LoginStarted>> {
    Ok(Json(state.accounts.login(&id).await?))
}

#[derive(Debug, Deserialize)]
struct UsageQuery {
    #[serde(default)]
    by: UsageGroup,
}

async fn usage(
    State(state): State<Arc<AppState>>,
    Query(query): Query<UsageQuery>,
) -> ApiResult<Json<UsageResponse>> {
    Ok(Json(
        crate::usage::compute(state.store().as_ref(), query.by).await?,
    ))
}

#[derive(Debug, Deserialize)]
struct UsageSeriesParams {
    #[serde(default)]
    range: Option<String>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

/// `GET /v1/usage/series`: token use and cost day by day, across accounts (see
/// [`crate::usage_series`]). Defaults: the last 30 days, one total, every account.
async fn usage_series(
    State(state): State<Arc<AppState>>,
    Query(params): Query<UsageSeriesParams>,
) -> ApiResult<Json<UsageSeriesResponse>> {
    let range = match params.range.as_deref().unwrap_or("30d") {
        "7d" => UsageSeriesRange::Days7,
        "30d" => UsageSeriesRange::Days30,
        "90d" => UsageSeriesRange::Days90,
        "all" => UsageSeriesRange::All,
        other => {
            return Err(AppError::bad_request(format!(
                "range must be 7d, 30d, 90d or all, not {other:?}"
            )));
        }
    };
    let group = match params.group.as_deref().unwrap_or("total") {
        "total" => UsageSeriesGroup::Total,
        "model" => UsageSeriesGroup::Model,
        "origin" => UsageSeriesGroup::Origin,
        "agent" => UsageSeriesGroup::Agent,
        "run" => UsageSeriesGroup::Run,
        other => {
            return Err(AppError::bad_request(format!(
                "group must be total, model, origin, agent or run, not {other:?}"
            )));
        }
    };
    let query = crate::usage_series::SeriesQuery {
        range,
        group,
        source: params.source.unwrap_or_else(|| "all".into()),
    };
    Ok(Json(state.accounts.usage_series(query).await?))
}

#[derive(Debug, Default, serde::Deserialize)]
struct ShutdownQuery {
    /// Stop the agents too, instead of leaving those in runtime hosts running.
    #[serde(default)]
    stop_agents: bool,
}

async fn shutdown(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ShutdownQuery>,
) -> StatusCode {
    tracing::info!(
        stop_agents = query.stop_agents,
        "shutdown requested over the API"
    );
    if query.stop_agents {
        state
            .stop_agents
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    state.shutdown.cancel();
    StatusCode::ACCEPTED
}

// ---------------------------------------------------------------------------------------
// Event stream

#[derive(Debug, Deserialize)]
struct EventsQuery {
    since: Option<u64>,
    /// `false` ends the stream once history has been sent. Defaults to `true`.
    follow: Option<bool>,
    /// `true` skips history and sends only events published after the response starts.
    live: Option<bool>,
    /// Only events about this agent (name or id), see `EventKind::agent_id`.
    agent: Option<String>,
}

async fn events(
    State(state): State<Arc<AppState>>,
    Query(query): Query<EventsQuery>,
    headers: HeaderMap,
) -> ApiResult<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>> {
    let last_event_id = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let since = last_event_id.or(query.since).unwrap_or(0);
    let agent = match &query.agent {
        Some(key) => Some(state.store().get_agent(&AgentKey::parse(key)).await?.id),
        None => None,
    };

    // Subscribe before responding: once the client sees the response headers, every
    // later event is guaranteed to reach it.
    let live = state.bus.subscribe();
    let (tx, rx) = mpsc::channel(256);
    let follow = query.follow.unwrap_or(true);
    if query.live.unwrap_or(false) {
        let latest = match state.store().latest_event_seq().await {
            Ok(seq) => seq,
            Err(err) => {
                tracing::error!(error = %err, "cannot read latest event");
                0
            }
        };
        tokio::spawn(stream_live(state, live, latest, tx));
    } else {
        tokio::spawn(stream_events(state, live, since, follow, tx));
    }
    let stream = ReceiverStream::new(rx)
        .filter(move |event| agent.is_none() || event.kind.agent_id() == agent)
        .map(|event| Ok(to_sse(&event)));
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

fn to_sse(event: &Event) -> SseEvent {
    SseEvent::default()
        .id(event.seq.to_string())
        .event(event.kind.type_name())
        .data(serde_json::to_string(event).unwrap_or_default())
}

/// Sends every event with `seq > since`, then follows live events, without gaps or
/// duplicates: subscribe first, replay history from the store, then skip broadcast events
/// already sent. If the subscriber lags behind, catch up from the store again.
async fn stream_events(
    state: Arc<AppState>,
    live: broadcast::Receiver<Event>,
    since: u64,
    follow: bool,
    tx: mpsc::Sender<Event>,
) {
    let mut last = since;
    if !replay(&state, &mut last, &tx).await || !follow {
        return;
    }
    stream_live(state, live, last, tx).await;
}

/// Forwards live events with `seq > last`, catching up from the store after lagging.
async fn stream_live(
    state: Arc<AppState>,
    mut live: broadcast::Receiver<Event>,
    mut last: u64,
    tx: mpsc::Sender<Event>,
) {
    loop {
        tokio::select! {
            _ = state.shutdown.cancelled() => return,
            _ = tx.closed() => return,
            received = live.recv() => match received {
                Ok(event) if event.seq > last => {
                    last = event.seq;
                    if tx.send(event).await.is_err() {
                        return;
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if !replay(&state, &mut last, &tx).await {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
        }
    }
}

/// Sends stored events after `*last`. Returns `false` if the client went away.
async fn replay(state: &AppState, last: &mut u64, tx: &mpsc::Sender<Event>) -> bool {
    loop {
        let page = match state.store().events_since(*last, EVENT_PAGE).await {
            Ok(page) => page,
            Err(err) => {
                tracing::error!(error = %err, "cannot read events");
                return false;
            }
        };
        if page.is_empty() {
            return true;
        }
        for event in page {
            *last = event.seq;
            if tx.send(event).await.is_err() {
                return false;
            }
        }
    }
}
