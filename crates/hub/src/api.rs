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
    ApproveRunRequest, ArchiveAgentRequest, AttachmentContent, AttachmentUpload,
    BackgroundReminder, BackgroundResponse, BackgroundWatch, ChangesResponse, CliInfo,
    CliInstallAccepted, CommitGraph, ConnectInfo, CreateAccountRequest, CreateAgentRequest,
    CreateRunRequest, CreateWorkRequest, CreateWorkRunRequest, DeleteRunResponse, DiffSide,
    ErrorCode, FEATURES, FileContentResponse, FileEntry, FileGitChange, FileKind,
    FileSearchResponse, FileStatusEntry, FileStatusResponse, FileTreeResponse, FinishRequest,
    GitBranch, GitBranchesResponse, GitCommitResponse, GitFileState, GitGraph, GitGraphRef,
    GitRefKind, GitStatusEntry, GitStatusResponse, GraphCommit, GraphRef, GraphRefKind,
    HealthResponse, HubInfo, ImageContent, InitRepoRequest, LandRequest, ListAccountsResponse,
    ListAgentsResponse, ListAvatarsResponse, ListHistoryResponse, ListNotesResponse,
    ListPluginsResponse, ListQuestionsResponse, ListRunsResponse, ListToolsResponse,
    ListWorksResponse, LoginStarted, PluginInfo, QueuedReason, QuotaResponse, RejectRunRequest,
    RenameAgentRequest, ResolvePathsRequest, ResolvePathsResponse, ResolvedPath, RunCostResponse,
    RunDetail, RunTrace, SendMessageRequest, SendMessageResponse, SetModelRequest,
    SetPluginModelsRequest, SetTopicRequest, TaskReviewRequest, TaskTrace, Transcript,
    TurnDiffResponse, UpdateRunPolicyRequest, UploadAvatarsRequest, UploadAvatarsResponse,
    UsageGroup, UsageResponse, UsageSeriesGroup, UsageSeriesRange, UsageSeriesResponse,
    VerifyingTask, WorkDetail,
};
use crossagents_core::{
    Account, AccountStatus, Agent, AgentId, AgentKey, AgentProfile, AgentStatus, Attachment,
    AttachmentSupport, CoreError, DriverKind, Envelope, Event, EventBus, EventKind, HumanItem,
    LandMode, MessageId, MessageSource, Run, RunConfig, RunId, Store, Task, TaskId,
};
use crossagents_orchestrator::{
    Approval, DeleteRun, FileError, FilesScope, GitError, GitWorkspaces, NewRun, Orchestrator,
    files as workspace_files, gitview,
};

use crate::accounts::Accounts;
use crate::collab::AgentTokens;
use crate::runtime::{Delivery, Runtime, SteerFailure};
use futures::Stream;
use serde::Deserialize;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const EVENT_PAGE: u32 = 500;
/// A full upload of avatars, base64.
const AVATAR_BODY_LIMIT: usize =
    crossagents_api_types::MAX_AVATAR_UPLOAD * crossagents_api_types::MAX_AVATAR_BYTES / 3 * 4
        + 1024 * 1024;

/// A message with its files, base64 (a third larger than the files themselves).
const MESSAGE_BODY_LIMIT: usize =
    crossagents_api_types::MAX_ATTACHMENTS_TOTAL / 3 * 4 + 1024 * 1024;

pub struct AppState {
    pub extensions: Arc<crate::extensions::Registry>,
    /// Runs extension tools and holds the tokens issued to them.
    pub runner: Arc<crate::extensions::Runner>,
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
    /// Looks for the CLIs again and asks their versions, in the background; one probe per
    /// plugin at a time.
    pub refresher: Arc<crate::refresh::Refresher>,
    /// The person's terminals on this machine.
    pub terminals: Arc<crate::terminal::Terminals>,
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
        .route(
            "/v1/agents/{key}/messages/{message_id}/send-now",
            post(send_now_message),
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
        .route(
            "/v1/avatars",
            get(list_avatars)
                .post(upload_avatars)
                .layer(DefaultBodyLimit::max(AVATAR_BODY_LIMIT)),
        )
        .route("/v1/avatars/{id}", get(get_avatar).delete(delete_avatar))
        .route("/v1/notes", get(list_notes))
        .route("/v1/background", get(background))
        .route("/v1/background/reminders/{id}", delete(cancel_reminder))
        .route("/v1/extensions", get(list_extensions))
        .route("/v1/extensions/{name}/logs", get(extension_logs))
        .route("/v1/extensions/{name}/enable", post(enable_extension))
        .route("/v1/extensions/{name}/disable", post(disable_extension))
        .route(
            "/v1/extensions/{name}/auto-trust",
            post(auto_trust_extension),
        )
        .route("/v1/extensions/{name}/pending/diff", get(extension_diff))
        .route(
            "/v1/extensions/{name}/tools/{tool}",
            post(call_extension_tool),
        )
        .route(
            "/v1/extensions/calls/{id}",
            get(get_extension_call).delete(cancel_extension_call),
        )
        .route("/v1/plugins", get(list_plugins))
        .route("/v1/plugins/refresh", post(refresh_plugins))
        .route("/v1/plugins/{name}/models", post(set_plugin_models))
        .route("/v1/plugins/{name}/install", post(install_plugin))
        .route("/v1/plugins/{name}/upgrade", post(upgrade_plugin))
        .route("/v1/usage", get(usage))
        .route("/v1/usage/series", get(usage_series))
        .route("/v1/accounts", get(list_accounts).post(create_account))
        .route(
            "/v1/accounts/import",
            get(crate::account_import::import_candidates)
                .post(crate::account_import::import_account),
        )
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
        .route("/v1/history", get(list_history))
        .route("/v1/runs", get(list_runs).post(create_run))
        .route("/v1/runs/{key}", get(get_run).delete(delete_run))
        .route("/v1/runs/{key}/approve", post(approve_run))
        .route("/v1/runs/{key}/reject", post(reject_run))
        .route("/v1/runs/{key}/cancel", post(cancel_run))
        .route("/v1/runs/{key}/land", post(land_run))
        .route("/v1/runs/{key}/policy", post(update_run_policy))
        .route("/v1/runs/{key}/finish", post(finish_run))
        .route("/v1/runs/{key}/apply", post(apply_run))
        .route("/v1/runs/{key}/integration", get(run_integration))
        .route("/v1/runs/{key}/cleanup", post(cleanup_run))
        .route("/v1/runs/{key}/diff", get(run_diff))
        .route("/v1/runs/{key}/trace", get(run_trace))
        .route("/v1/runs/{key}/retro", get(run_retro))
        .route("/v1/runs/{key}/cost", get(run_cost))
        .route("/v1/runs/{key}/tasks/{task}/diff", get(task_diff))
        .route("/v1/runs/{key}/tasks/{task}/review", post(review_task))
        .route("/v1/agents/{key}/files/tree", get(agent_files_tree))
        .route("/v1/agents/{key}/files/content", get(agent_files_content))
        .route("/v1/agents/{key}/files/status", get(agent_files_status))
        .route("/v1/agents/{key}/files/search", get(agent_files_search))
        .route("/v1/agents/{key}/files/image", get(agent_files_image))
        .route("/v1/agents/{key}/files/resolve", post(agent_files_resolve))
        .route("/v1/runs/{key}/files/tree", get(run_files_tree))
        .route("/v1/runs/{key}/files/content", get(run_files_content))
        .route("/v1/runs/{key}/files/status", get(run_files_status))
        .route("/v1/runs/{key}/files/search", get(run_files_search))
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
            "/v1/runs/{key}/tasks/{task}/files/status",
            get(task_files_status),
        )
        .route(
            "/v1/runs/{key}/tasks/{task}/files/search",
            get(task_files_search),
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
        .route("/v1/git/branches", get(git_branches))
        .route("/v1/git/status", get(git_status))
        .route("/v1/git/commits/{sha}", get(git_commit))
        .route("/v1/git/commits/{sha}/diff", get(git_commit_diff))
        .route("/v1/git/graph", get(git_graph))
        .route("/v1/repos/init", post(init_repo))
        .route("/v1/events", get(events))
        .route("/v1/shutdown", post(shutdown))
        .merge(crate::api_expertise::router())
        .merge(crate::api_projects::router())
        .merge(crate::lessons::router())
        .merge(crate::api_terminal::router())
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

    /// The same error, saying why a message was refused (docs/tasks/DELIVERY-AUDIT.md §3).
    fn with_reason(self, reason: QueuedReason) -> Self {
        Self(self.0, self.1.with_reason(reason))
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
            CoreError::OutcomeUnknown(_) => {
                (StatusCode::GATEWAY_TIMEOUT, ErrorCode::OutcomeUnknown)
            }
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

impl From<GitError> for AppError {
    fn from(err: GitError) -> Self {
        let (status, code) = match &err {
            // A directory the hub does not know is refused, not read.
            GitError::Outside(_) => (StatusCode::FORBIDDEN, ErrorCode::Forbidden),
            GitError::NotFound(_) => (StatusCode::NOT_FOUND, ErrorCode::NotFound),
            GitError::Conflict(_) => (StatusCode::CONFLICT, ErrorCode::Conflict),
            GitError::BadRequest(_) => (StatusCode::BAD_REQUEST, ErrorCode::BadRequest),
            GitError::Failed(_) => {
                tracing::error!(error = %err, "cannot read the git view");
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

pub(crate) type ApiResult<T> = Result<T, AppError>;

// ---------------------------------------------------------------------------------------
// Auth

async fn require_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut request: Request,
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
    // An extension's token is logged by name, so what it did can be traced to it.
    if let crate::access::Grant::Extension { name, level } = &grant {
        tracing::info!(extension = %name, ?level, method = %request.method(), route = %pattern, "extension request");
    }
    match crate::access::refusal(&grant, request.method(), &pattern) {
        None => {
            // Handlers that act as the caller read who it is from here, never from the body.
            request.extensions_mut().insert(grant);
            next.run(request).await
        }
        Some(why) => {
            AppError::new(StatusCode::FORBIDDEN, ErrorCode::Forbidden, why).into_response()
        }
    }
}

/// What a presented token may do: the hub's token everything, the read-only token read,
/// an agent's session token nothing here, an extension's token what its call was issued for.
async fn grant_of(state: &AppState, token: &str) -> Option<crate::access::Grant> {
    use crate::access::{Grant, Level};
    if constant_time_eq(token.as_bytes(), state.token.as_bytes()) {
        Some(Grant::Level(Level::Admin))
    } else if constant_time_eq(token.as_bytes(), state.read_token.as_bytes()) {
        Some(Grant::Level(Level::Read))
    } else if state.tokens.agent(token).await.is_some() {
        Some(Grant::Agent)
    } else {
        state
            .runner
            .token(token)
            .map(|(name, level)| Grant::Extension { name, level })
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
        features: FEATURES.iter().map(|f| (*f).to_owned()).collect(),
    })
}

#[derive(Deserialize)]
struct ListAgentsQuery {
    #[serde(default)]
    run: Option<String>,
}

async fn list_agents(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListAgentsQuery>,
) -> ApiResult<Json<ListAgentsResponse>> {
    let policy = if let Some(id) = query.run {
        let run = state
            .store()
            .list_runs()
            .await?
            .into_iter()
            .find(|r| r.id.to_string() == id)
            .ok_or_else(|| crossagents_core::CoreError::Conflict("no such run".into()))?;
        Some(run.config.policy)
    } else {
        None
    };
    let agents = state.store().list_agents().await?;
    let unusable = agents
        .iter()
        .map(|agent| crossagents_orchestrator::approval::unusable_for(agent, policy.as_ref()))
        .collect();
    Ok(Json(ListAgentsResponse { agents, unusable }))
}

#[derive(Deserialize)]
struct SessionCandidatesQuery {
    /// The workspace; absent: the standalone chats, each in its own `<home>/chats/<id>`.
    #[serde(default)]
    repo: Option<String>,
    /// Select a run when a workspace has more than one plan awaiting approval.
    #[serde(default)]
    run: Option<String>,
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
    // PlanDock currently asks by workspace. If exactly one plan there awaits approval,
    // its policy is the context in which these candidates would be assigned.
    let policy = if let Some(repo) = query.repo.as_deref().filter(|r| !r.is_empty()) {
        let runs: Vec<_> = state
            .store()
            .list_runs()
            .await?
            .into_iter()
            .filter(|r| {
                r.repo == std::path::Path::new(repo)
                    && r.status == crossagents_core::RunStatus::AwaitingApproval
                    && query.run.as_ref().is_none_or(|id| r.id.to_string() == *id)
            })
            .collect();
        if query.run.is_some() && runs.is_empty() {
            return Err(crossagents_core::CoreError::Conflict(
                "no matching plan awaiting approval in this workspace".into(),
            )
            .into());
        }
        if runs
            .iter()
            .skip(1)
            .any(|r| r.config.policy.approvals != runs[0].config.policy.approvals)
        {
            // A workspace-only request cannot identify which plan the GUI is showing.
            // Keep it working with chat context until it can supply the run identifier.
            None
        } else {
            runs.into_iter().next().map(|r| r.config.policy)
        }
    } else {
        None
    };
    let sessions = candidates
        .into_iter()
        .map(|candidate| crossagents_api_types::SessionCandidate {
            can_use: candidate.can_use(),
            reason: candidate.reason.map(|r| r.word().to_owned()),
            unusable: crossagents_orchestrator::approval::unusable_for(
                &candidate.agent,
                policy.as_ref(),
            ),
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
        return Err(
            AppError::bad_request("text must not be empty").with_reason(QueuedReason::Empty)
        );
    }
    let key = AgentKey::parse(&key);
    let agent = state.store().get_agent(&key).await?;
    state.orchestrator.chat_available(&agent).await?;
    if agent.archived_at.is_some() {
        if !req.restore {
            return Err(AppError::new(
                StatusCode::CONFLICT,
                ErrorCode::Conflict,
                format!(
                    "{} is archived: restore it first with restore: true",
                    agent.name
                ),
            )
            .with_reason(QueuedReason::Archived));
        }
        state.runtime.archive(&key, false).await?;
    }
    let text = req.text;
    let attachments = store_uploads(&state, &key, uploads).await?;
    let envelope = Envelope {
        attachments,
        ..Envelope::new(MessageSource::Human, text.clone())
    };
    let delivery = state.runtime.send_envelope(&key, envelope).await?;
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
    Ok((StatusCode::ACCEPTED, Json(message_response(delivery))))
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
    // A CLI older than the description's minimum does not register an agent
    // (docs/research/multica.md §2.9). A CLI that has not answered its version yet is
    // started as before: unknown is not too old.
    if req.plugin != crossagents_core::EXTERNAL_PLUGIN
        && let Some(driver) = state.runtime.plugins().get(&req.plugin)
        && let Some(status) = driver.cli_status().await
    {
        let version = status
            .located
            .as_ref()
            .and_then(|located| located.version.clone());
        if let Some(config) = state.runtime.plugins().config(&req.plugin)
            && config.too_old(version.as_deref())
        {
            return Err(AppError::bad_request(format!(
                "the {} CLI on this machine is too old (version {}, needs {} or newer): \
                 upgrade it on the Agents page",
                req.plugin,
                version.as_deref().unwrap_or("unknown"),
                config.min_version.as_deref().unwrap_or(""),
            )));
        }
    }
    if let Some(driver) = &req.driver {
        driver.parse::<DriverKind>()?;
    }
    let mut profile = req.profile.normalized()?;
    crate::avatars::assign_new(&state.home, state.store(), &mut profile).await?;
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

/// `POST /v1/agents/{key}/settings`: which of Claude Code's settings the agent loads, and
/// whether their hooks may run (docs/tasks/DOGFOOD.md §6, SELF.md §B5). The CLI reads both
/// only at launch, so an idle session is stopped and resumes with them on the next message; a
/// busy one keeps the old ones until it next starts.
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
    let hooks_before = agent.config.get("allow_hooks").cloned();
    // Off is the default, so it is stored as absence: one shape for "never chose".
    if let Some(allow) = req.allow_hooks {
        if allow {
            agent.config["allow_hooks"] = true.into();
        } else {
            agent.config.as_object_mut().unwrap().remove("allow_hooks");
        }
    }
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
    if (agent.config.get("setting_sources").cloned() != before
        || agent.config.get("allow_hooks").cloned() != hooks_before)
        && agent.status == AgentStatus::Idle
    {
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
    // What the session does with messages, from the hub's inbox: one answer for "busy" and
    // "next" (docs/tasks/DELIVERY-AUDIT.md §8 P1-4).
    let delivery = state.runtime.delivery_state(&agent).await;
    Ok(Json(Transcript {
        agent,
        driver: resolved.as_ref().map(|r| r.driver.kind()),
        capabilities,
        delivery,
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

/// The background board: every agent's reminders and watches, and the tasks being verified.
async fn background(State(state): State<Arc<AppState>>) -> ApiResult<Json<BackgroundResponse>> {
    use std::collections::HashMap;
    let store = state.store();
    let names: HashMap<AgentId, String> = store
        .list_agents()
        .await?
        .into_iter()
        .map(|a| (a.id, a.name))
        .collect();
    let name_of = |id: &AgentId| names.get(id).cloned().unwrap_or_else(|| id.to_string());

    let mut tasks: HashMap<TaskId, Task> = HashMap::new();
    let mut runs: HashMap<RunId, Run> = HashMap::new();
    // The run each agent works in: as a lead, a worker or a reviewer (the newest run wins).
    let mut agent_run: HashMap<AgentId, (DateTime<Utc>, RunId)> = HashMap::new();
    let mut note_agent = |agent: AgentId, at: DateTime<Utc>, run: RunId| {
        let slot = agent_run.entry(agent).or_insert((at, run));
        if at >= slot.0 {
            *slot = (at, run);
        }
    };
    for run in store.list_runs().await? {
        note_agent(run.planner, run.created_at, run.id);
        for task in store.list_tasks(run.id).await? {
            for agent in [task.assignee, task.reviewer].into_iter().flatten() {
                note_agent(agent, run.created_at, run.id);
            }
            tasks.insert(task.id, task);
        }
        runs.insert(run.id, run);
    }
    let run_of_agent = |id: &AgentId| agent_run.get(id).map(|(_, run)| *run);

    let reminders = store
        .list_reminders(None)
        .await?
        .into_iter()
        .map(|reminder| {
            let task = reminder.task.and_then(|id| tasks.get(&id));
            BackgroundReminder {
                owner_name: name_of(&reminder.owner),
                task_key: task.map(|t| t.key.clone()),
                run_id: task
                    .map(|t| t.run_id)
                    .or_else(|| run_of_agent(&reminder.owner)),
                reminder,
            }
        })
        .collect();

    let mut watches: Vec<BackgroundWatch> = store
        .list_watches(None)
        .await?
        .into_iter()
        .map(|watch| {
            let (target_name, run_id) = match &watch.target {
                crossagents_core::WatchTarget::Task { task_id } => match tasks.get(task_id) {
                    Some(t) => (format!("#{}", t.key), Some(t.run_id)),
                    None => (task_id.to_string(), None),
                },
                crossagents_core::WatchTarget::Agent { agent_id } => (name_of(agent_id), None),
            };
            BackgroundWatch {
                owner_name: name_of(&watch.owner),
                target_name,
                run_id: run_id.or_else(|| run_of_agent(&watch.owner)),
                watch,
            }
        })
        .collect();
    watches.sort_by_key(|a| std::cmp::Reverse(a.watch.created_at));

    let mut verifying: Vec<VerifyingTask> = tasks
        .values()
        .filter(|t| t.status == crossagents_core::TaskStatus::Verifying)
        .map(|t| VerifyingTask {
            task_id: t.id,
            run_id: t.run_id,
            run_title: runs
                .get(&t.run_id)
                .map(|r| r.title.clone())
                .unwrap_or_default(),
            key: t.key.clone(),
            title: t.title.clone(),
            assignee: t.assignee,
            assignee_name: t.assignee.as_ref().map(name_of),
            checks: t.spec.acceptance.len() as u32,
            since: t.updated_at,
        })
        .collect();
    verifying.sort_by_key(|a| a.since);

    Ok(Json(BackgroundResponse {
        reminders,
        watches,
        verifying,
    }))
}

/// Cancels an agent's reminder from the board, as `remind_cancel` does for its owner.
async fn cancel_reminder(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    // The same deletion `remind_cancel` makes, for any owner; an unknown id is a 404.
    state.store().delete_reminder(&id).await?;
    Ok(StatusCode::NO_CONTENT)
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
        return Err(
            AppError::bad_request("text must not be empty").with_reason(QueuedReason::Empty)
        );
    }
    let key = AgentKey::parse(&key);
    // Archived and refused files are said with their reason, before the message is made.
    let stored = state.store().get_agent(&key).await?;
    if stored.archived_at.is_some() {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            ErrorCode::Conflict,
            format!(
                "{} is archived; its conversation has to be restored before it gets messages",
                stored.name
            ),
        )
        .with_reason(QueuedReason::Archived));
    }
    let attachments = store_uploads(&state, &key, uploads).await?;
    let envelope = Envelope {
        attachments,
        ..Envelope::new(MessageSource::Human, req.text)
    };
    let client_message_id = req.client_message_id;
    let delivery = if req.now == Some(true) {
        state
            .runtime
            .send_now_envelope_from(&key, envelope, client_message_id)
            .await
            .map_err(AppError::from)?
    } else if req.steer == Some(true) {
        match state
            .runtime
            .steer_envelope_from(&key, envelope, client_message_id)
            .await
        {
            Ok(delivery) => delivery,
            Err(SteerFailure::Refused {
                reason, error: why, ..
            }) => {
                return Err(
                    AppError::new(StatusCode::BAD_GATEWAY, ErrorCode::AgentError, why)
                        .with_reason(reason),
                );
            }
            // The write timed out: it may or may not have arrived. The GUI shows this code
            // as "不确定发没发出去，先别重发" (docs/tasks/DELIVERY-AUDIT.md §3).
            Err(SteerFailure::Unknown { error: why, .. }) => {
                return Err(AppError::new(
                    StatusCode::GATEWAY_TIMEOUT,
                    ErrorCode::OutcomeUnknown,
                    why,
                ));
            }
            Err(SteerFailure::Error(err)) => return Err(err.into()),
        }
    } else {
        state
            .runtime
            .send_envelope_from(&key, envelope, client_message_id)
            .await
            .map_err(AppError::from)?
    };
    Ok((StatusCode::ACCEPTED, Json(message_response(delivery))))
}

/// The reply for a delivered message, saying why it waits when it does.
fn message_response(delivery: Delivery) -> SendMessageResponse {
    SendMessageResponse {
        message_id: delivery.message_id,
        queued: delivery.queued,
        steered: delivery.steered,
        queued_reason: delivery.queued_reason,
    }
}

/// `POST /v1/agents/{key}/messages/{id}/send-now`: moves a waiting message to the front of
/// the line and interrupts the running turn, keeping its id
/// (docs/tasks/DELIVERY-AUDIT.md §2). A message that already started, or one nobody knows,
/// is a conflict and leaves the queue as it was.
async fn send_now_message(
    State(state): State<Arc<AppState>>,
    Path((key, message_id)): Path<(String, String)>,
) -> ApiResult<(StatusCode, Json<SendMessageResponse>)> {
    let message_id: MessageId = message_id
        .parse()
        .map_err(|_| AppError::bad_request("invalid message id"))?;
    let key = AgentKey::parse(&key);
    let stored = state.store().get_agent(&key).await?;
    if stored.archived_at.is_some() {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            ErrorCode::Conflict,
            format!("{} is archived", stored.name),
        )
        .with_reason(QueuedReason::Archived));
    }
    let delivery = state
        .runtime
        .promote(&key, message_id)
        .await
        .map_err(|err| match err {
            CoreError::Conflict(why) => {
                AppError::new(StatusCode::CONFLICT, ErrorCode::Conflict, why)
                    .with_reason(QueuedReason::AlreadyStarted)
            }
            other => other.into(),
        })?;
    Ok((StatusCode::ACCEPTED, Json(message_response(delivery))))
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
    crate::attachments::check_support(&uploads, support, &agent.name, driver).map_err(
        |refused| AppError::bad_request(refused.0).with_reason(QueuedReason::AttachmentRefused),
    )?;
    crate::attachments::store(&state.home, agent.id, uploads).map_err(|refused| {
        AppError::bad_request(refused.0).with_reason(QueuedReason::AttachmentRefused)
    })
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

async fn list_avatars(State(state): State<Arc<AppState>>) -> Json<ListAvatarsResponse> {
    Json(ListAvatarsResponse {
        avatars: crate::avatars::list(&state.home),
    })
}

/// Adds pictures to the gallery, then gives them to every session without an avatar.
async fn upload_avatars(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UploadAvatarsRequest>,
) -> ApiResult<Json<UploadAvatarsResponse>> {
    let added = crate::avatars::store(&state.home, &req.images)
        .map_err(|refused| AppError::bad_request(refused.0))?;
    let assigned =
        crate::avatars::assign_missing(&state.home, state.store(), state.bus.as_ref()).await?;
    Ok(Json(UploadAvatarsResponse {
        added,
        assigned: assigned as u32,
    }))
}

async fn get_avatar(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<AttachmentContent>> {
    crate::avatars::read(&state.home, &id)
        .map(Json)
        .ok_or_else(|| CoreError::NotFound { kind: "avatar", id }.into())
}

async fn delete_avatar(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    crate::avatars::release(state.store(), state.bus.as_ref(), &id).await?;
    let removed = crate::avatars::remove(&state.home, &id).map_err(|e| {
        AppError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            format!("removing the picture: {e}"),
        )
    })?;
    if !removed {
        return Err(CoreError::NotFound { kind: "avatar", id }.into());
    }
    Ok(StatusCode::NO_CONTENT)
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
    // What this plugin's install or upgrade is doing now, if one is running (in memory only:
    // after a restart nothing is, so this is `None`).
    let installing = state
        .installer
        .installing(name)
        .await
        .map(|action| action.as_str().to_owned());
    let status = d.cli_status().await;
    let found = status
        .as_ref()
        .and_then(|status| status.located.as_ref())
        .and_then(|located| located.version.clone());
    let config = registry.config(name);
    // A CLI older than the description's minimum is not registered: no models are offered
    // for it, and the row says why (docs/research/multica.md §2.9).
    let below_minimum = config.is_some_and(|config| config.too_old(found.as_deref()));
    let all = match below_minimum {
        true => Vec::new(),
        false => crate::models::available(registry, &state.accounts, name).await,
    };
    // A CLI this machine does not have runs nothing: offer none of its models.
    let models = if d.installed() && !below_minimum {
        state.models.filter(name, all.clone())
    } else {
        Vec::new()
    };
    let cli = status.map(|status| CliInfo {
        installed: status.located.is_some(),
        probing: status.probing,
        path: status
            .located
            .as_ref()
            .map(|located| located.path.display().to_string()),
        source: status
            .located
            .as_ref()
            .map(|located| located.source.as_str().to_owned()),
        version: found.clone(),
        install_hint: status.install_hint,
        docs: status.docs,
        installed_via: status.installed_via,
        install_command: status.install_command,
        upgrade_command: status.upgrade_command,
        can_upgrade: status.can_upgrade,
        installing,
        described_by: status.described_by,
        min_version: config.and_then(|config| config.min_version.clone()),
        below_minimum,
        verified: config
            .map(|config| config.verified.clone())
            .filter(|series| !series.is_empty()),
        unverified: config.is_some_and(|config| config.unverified(found.as_deref())),
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

/// `POST /v1/plugins/refresh`: looks for the CLIs again and asks each its version, in the
/// background (docs/DESIGN.md §9.3). `202` at once, never waiting for a subprocess; every
/// probe that ends publishes `plugin.changed`, so a client re-reads `GET /v1/plugins` and
/// its rows turn into what the machine has now. `?plugin=<name>` looks for just that one.
///
/// Admin: it runs the CLIs' version commands here, as install and upgrade do.
async fn refresh_plugins(
    State(state): State<Arc<AppState>>,
    Query(query): Query<RefreshPluginsQuery>,
) -> ApiResult<StatusCode> {
    let registry = state.runtime.plugins();
    let drivers = match &query.plugin {
        Some(name) => vec![registry.get(name).ok_or_else(|| CoreError::NotFound {
            kind: "plugin",
            id: name.clone(),
        })?],
        None => registry.defaults(),
    };
    for driver in drivers {
        state.refresher.start(state.bus.clone(), driver).await;
    }
    Ok(StatusCode::ACCEPTED)
}

#[derive(Debug, Deserialize)]
struct RefreshPluginsQuery {
    /// One plugin to look for again; every plugin when absent.
    plugin: Option<String>,
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
pub(crate) async fn resolve_run(state: &AppState, key: &str) -> ApiResult<RunId> {
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
    let work = state
        .orchestrator
        .create_work_group(&req.title, lead.home().to_path_buf(), lead.id)
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
    // Its rounds that ended, including the ones since deleted: what the group did outlives
    // the runs in it.
    let history = state
        .store()
        .list_run_history(None)
        .await?
        .into_iter()
        .filter(|entry| entry.work_id == Some(id))
        .collect();
    Ok(Json(WorkDetail {
        work,
        runs,
        history,
    }))
}

/// `GET /v1/history?repo=<path>`: what runs left in a workspace, the deleted ones too. Without
/// `repo`, every workspace's.
#[derive(Debug, Deserialize)]
struct HistoryQuery {
    #[serde(default)]
    repo: Option<String>,
}

async fn list_history(
    State(state): State<Arc<AppState>>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<ListHistoryResponse>> {
    let repo = q
        .repo
        .map(|repo| repo.trim().to_owned())
        .filter(|repo| !repo.is_empty())
        .map(PathBuf::from);
    Ok(Json(ListHistoryResponse {
        history: state.store().list_run_history(repo.as_deref()).await?,
    }))
}

async fn create_work_run(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<CreateWorkRunRequest>,
) -> ApiResult<(StatusCode, Json<Run>)> {
    let id = id
        .parse::<crossagents_core::WorkId>()
        .map_err(|_| CoreError::Conflict("invalid work id".into()))?;
    let run = state
        .orchestrator
        .create_work_round(id, req.request, req.title, req.from.as_deref())
        .await?;
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
        lead_place: req
            .lead_place
            .unwrap_or(crossagents_core::LeadPlace::Worktree),
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
    let mut run = state
        .orchestrator
        .create_run_with_choice(new, req.fresh, req.restore)
        .await?;
    // Every run belongs to a work group: its next round continues from this one.
    state.orchestrator.work_of_run(&mut run).await?;
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
        // Re-arming also lifts a pause the automation breaker set: the person has looked at
        // the failing stretch and asked for it to run again, so the counts start over.
        continuation.health = Default::default();
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

/// `GET /v1/runs/{key}/retro`: the run's retro as Markdown (docs/tasks/SELF.md C1), folded
/// from its trace and its `issue/` notes. Only a run that ended has one. The caller writes
/// it where it belongs: the command line in the repository of the current directory, the
/// lead's `run_retro` in its own checkout.
async fn run_retro(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let id = resolve_run(&state, &key).await?;
    let run = state.store().get_run(id).await?;
    let doc = crate::retro::document(&**state.store(), &run).await?;
    Ok(Json(serde_json::json!({
        "run": run.id,
        "markdown": doc.markdown,
        "findings": doc.findings,
        "issues": doc.issues,
    })))
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

/// What the run spent, split by agent and model — its cost bar
/// (docs/research/multica.md §十三).
async fn run_cost(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Json<RunCostResponse>> {
    let id = resolve_run(&state, &key).await?;
    Ok(Json(
        crate::run_cost::compute(state.store().as_ref(), id).await?,
    ))
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
// Git view (read-only, docs/TODO.md §14)

/// What `GET /v1/git/*` takes: the project directory, and the graph's limit.
#[derive(Debug, Default, Deserialize)]
struct GitQuery {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

impl GitQuery {
    fn path(&self) -> &str {
        self.path.as_deref().unwrap_or("")
    }
}

fn git_file_state(state: gitview::FileState) -> GitFileState {
    use gitview::FileState;
    match state {
        FileState::Unchanged => GitFileState::Unchanged,
        FileState::Modified => GitFileState::Modified,
        FileState::Added => GitFileState::Added,
        FileState::Deleted => GitFileState::Deleted,
        FileState::Renamed => GitFileState::Renamed,
        FileState::Copied => GitFileState::Copied,
        FileState::TypeChanged => GitFileState::TypeChanged,
        FileState::Unmerged => GitFileState::Unmerged,
        FileState::Unknown => GitFileState::Unknown,
    }
}

fn git_ref_kind(kind: gitview::RepoRefKind) -> GitRefKind {
    use gitview::RepoRefKind;
    match kind {
        RepoRefKind::Head => GitRefKind::Head,
        RepoRefKind::Local => GitRefKind::Local,
        RepoRefKind::Remote => GitRefKind::Remote,
        RepoRefKind::Tag => GitRefKind::Tag,
    }
}

/// The local and remote-tracking branches of a project, with their upstreams.
async fn git_branches(
    State(state): State<Arc<AppState>>,
    Query(q): Query<GitQuery>,
) -> ApiResult<Json<GitBranchesResponse>> {
    let branches = state.orchestrator.git_branches(q.path()).await?;
    Ok(Json(GitBranchesResponse {
        root: branches.root.display().to_string(),
        branches: branches
            .entries
            .into_iter()
            .map(|branch| GitBranch {
                name: branch.name,
                remote: branch.remote,
                sha: branch.sha,
                upstream: branch.upstream,
                ahead: branch.ahead,
                behind: branch.behind,
                current: branch.current,
            })
            .collect(),
        head: branches.head,
        head_sha: branches.head_sha,
    }))
}

/// The working copy status of a project.
async fn git_status(
    State(state): State<Arc<AppState>>,
    Query(q): Query<GitQuery>,
) -> ApiResult<Json<GitStatusResponse>> {
    let status = state.orchestrator.git_status(q.path()).await?;
    Ok(Json(GitStatusResponse {
        root: status.root.display().to_string(),
        head: status.head,
        branch: status.branch,
        upstream: status.upstream,
        ahead: status.ahead,
        behind: status.behind,
        entries: status
            .entries
            .into_iter()
            .map(|entry| GitStatusEntry {
                path: entry.path,
                orig_path: entry.orig_path,
                index: git_file_state(entry.index),
                worktree: git_file_state(entry.worktree),
                untracked: entry.untracked,
                conflicted: entry.conflicted,
            })
            .collect(),
    }))
}

/// One commit of a project.
async fn git_commit(
    State(state): State<Arc<AppState>>,
    Path(sha): Path<String>,
    Query(q): Query<GitQuery>,
) -> ApiResult<Json<GitCommitResponse>> {
    let sha = commit_sha(&sha)?;
    let commit = state.orchestrator.git_commit(q.path(), sha).await?;
    Ok(Json(GitCommitResponse {
        root: commit.root.display().to_string(),
        sha: commit.sha,
        parents: commit.parents,
        author: commit.author,
        author_email: commit.author_email,
        time: commit.time,
        subject: commit.subject,
        message: commit.message,
    }))
}

/// What one commit of a project changed, against its first parent.
async fn git_commit_diff(
    State(state): State<Arc<AppState>>,
    Path(sha): Path<String>,
    Query(q): Query<GitQuery>,
) -> ApiResult<Json<ChangesResponse>> {
    let sha = commit_sha(&sha)?;
    let root = state.orchestrator.git_repo(q.path()).await?;
    Ok(changes_response(
        crossagents_orchestrator::graph::commit_diff(&root, sha).await?,
    ))
}

/// The whole repository's graph (`git log --all`).
async fn git_graph(
    State(state): State<Arc<AppState>>,
    Query(q): Query<GitQuery>,
) -> ApiResult<Json<GitGraph>> {
    let graph = state
        .orchestrator
        .git_graph(q.path(), q.limit.unwrap_or(0))
        .await?;
    Ok(Json(GitGraph {
        root: graph.root.display().to_string(),
        refs: graph
            .refs
            .into_iter()
            .map(|entry| GitGraphRef {
                name: entry.name,
                kind: git_ref_kind(entry.kind),
                sha: entry.sha,
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
    }))
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

/// What `GET {scope}/files/search` takes: the name to find, and whether to search what
/// .gitignore hides too.
#[derive(Debug, Default, Deserialize)]
struct FileSearchQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    all: bool,
}

fn file_change(change: workspace_files::Change) -> FileGitChange {
    match change {
        workspace_files::Change::Added => FileGitChange::Added,
        workspace_files::Change::Modified => FileGitChange::Modified,
        workspace_files::Change::Deleted => FileGitChange::Deleted,
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
    crate::paths::refuse_remote(&root, input)?;
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

async fn agent_files_status(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Json<FileStatusResponse>> {
    let agent = state.store().get_agent(&AgentKey::parse(&key)).await?;
    files_status(&state, FilesScope::Agent(&agent)).await
}

async fn run_files_status(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Json<FileStatusResponse>> {
    let run = resolve_run(&state, &key).await?;
    files_status(&state, FilesScope::Run(run)).await
}

async fn task_files_status(
    State(state): State<Arc<AppState>>,
    Path((key, task)): Path<(String, String)>,
) -> ApiResult<Json<FileStatusResponse>> {
    let run = resolve_run(&state, &key).await?;
    let task = resolve_task(&state, run, &task).await?;
    files_status(&state, FilesScope::Task { run, task }).await
}

async fn agent_files_search(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Query(q): Query<FileSearchQuery>,
) -> ApiResult<Json<FileSearchResponse>> {
    let agent = state.store().get_agent(&AgentKey::parse(&key)).await?;
    files_search(&state, FilesScope::Agent(&agent), q).await
}

async fn run_files_search(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Query(q): Query<FileSearchQuery>,
) -> ApiResult<Json<FileSearchResponse>> {
    let run = resolve_run(&state, &key).await?;
    files_search(&state, FilesScope::Run(run), q).await
}

async fn task_files_search(
    State(state): State<Arc<AppState>>,
    Path((key, task)): Path<(String, String)>,
    Query(q): Query<FileSearchQuery>,
) -> ApiResult<Json<FileSearchResponse>> {
    let run = resolve_run(&state, &key).await?;
    let task = resolve_task(&state, run, &task).await?;
    files_search(&state, FilesScope::Task { run, task }, q).await
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
    crate::paths::refuse_remote_in(&state.orchestrator, scope, q.path()).await?;
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

async fn files_status(
    state: &AppState,
    scope: FilesScope<'_>,
) -> ApiResult<Json<FileStatusResponse>> {
    let (root, changes) = state.orchestrator.files_status(scope).await?;
    let git = changes.is_some();
    Ok(Json(FileStatusResponse {
        root: root.display().to_string(),
        git,
        entries: changes
            .unwrap_or_default()
            .into_iter()
            .map(|entry| FileStatusEntry {
                path: entry.path,
                change: file_change(entry.change),
            })
            .collect(),
    }))
}

async fn files_search(
    state: &AppState,
    scope: FilesScope<'_>,
    q: FileSearchQuery,
) -> ApiResult<Json<FileSearchResponse>> {
    let (root, found) = state.orchestrator.files_search(scope, &q.q, q.all).await?;
    Ok(Json(FileSearchResponse {
        root: root.display().to_string(),
        paths: found.paths,
        truncated: found.truncated,
    }))
}

async fn files_content(
    state: &AppState,
    scope: FilesScope<'_>,
    q: FilesQuery,
) -> ApiResult<Json<FileContentResponse>> {
    crate::paths::refuse_remote_in(&state.orchestrator, scope, q.path()).await?;
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
    let inputs = crate::paths::without_remote(&state.orchestrator, scope, &req.paths).await?;
    let found = state.orchestrator.files_resolve(scope, &inputs).await?;
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

/// `POST /v1/runs/{key}/apply` — write the workers' results into the person's own directory
/// (docs/tasks/LEAD.md §6, §10): three-way, once, and never a commit there. Safe to call
/// again: a result already applied writes nothing.
async fn apply_run(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let id = resolve_run(&state, &key).await?;
    Ok(Json(state.orchestrator.apply_in_place(id).await?))
}

/// `GET /v1/runs/{key}/integration` — what the workers did, which the person's own directory
/// does not hold until an apply (docs/tasks/LEAD.md §6): the diff against the dispatch
/// snapshot, and the integration commit already applied, if any.
async fn run_integration(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let id = resolve_run(&state, &key).await?;
    Ok(Json(state.orchestrator.inplace_changes(id).await?))
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
        tools: state.orchestrator.tools_for(&agent).await,
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

async fn list_extensions(
    State(s): State<Arc<AppState>>,
) -> Json<crossagents_api_types::ExtensionsResponse> {
    Json(crossagents_api_types::ExtensionsResponse {
        extensions: s.extensions.list(),
    })
}
async fn extension_logs(
    State(s): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<Json<crossagents_api_types::ExtensionLogsResponse>> {
    Ok(Json(s.runner.logs(&name)?))
}
async fn enable_extension(
    State(s): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(req): Json<crossagents_api_types::EnableExtensionRequest>,
) -> ApiResult<Json<crossagents_api_types::ExtensionInfo>> {
    let r = s.extensions.clone();
    Ok(Json(
        tokio::task::spawn_blocking(move || r.enable(&name, &req.fingerprint))
            .await
            .map_err(|e| CoreError::Storage(e.to_string()))??,
    ))
}
async fn disable_extension(
    State(s): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<Json<crossagents_api_types::ExtensionInfo>> {
    Ok(Json(s.extensions.disable(&name)?))
}
async fn auto_trust_extension(
    State(s): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(req): Json<crossagents_api_types::SetExtensionAutoTrustRequest>,
) -> ApiResult<Json<crossagents_api_types::ExtensionInfo>> {
    let r = s.extensions.clone();
    Ok(Json(
        tokio::task::spawn_blocking(move || r.set_auto_trust(&name, req.auto_trust))
            .await
            .map_err(|e| CoreError::Storage(e.to_string()))??,
    ))
}
async fn extension_diff(
    State(s): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<Json<crossagents_api_types::ExtensionDiffResponse>> {
    let r = s.extensions.clone();
    Ok(Json(
        tokio::task::spawn_blocking(move || r.pending_diff(&name))
            .await
            .map_err(|e| CoreError::Storage(e.to_string()))??,
    ))
}

fn call_error(e: crate::extensions::CallError) -> AppError {
    use crate::extensions::CallError as E;
    let (status, code) = match &e {
        E::Unknown(_) => (StatusCode::NOT_FOUND, ErrorCode::NotFound),
        E::NotEnabled(_) => (StatusCode::CONFLICT, ErrorCode::Conflict),
        E::Forbidden(_) => (StatusCode::FORBIDDEN, ErrorCode::Forbidden),
        E::InvalidInput(_) => (StatusCode::BAD_REQUEST, ErrorCode::BadRequest),
        E::Busy(_) => (StatusCode::TOO_MANY_REQUESTS, ErrorCode::Conflict),
        E::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal),
    };
    AppError::new(status, code, e.to_string())
}

/// A person's GUI calls an extension tool. It answers at once with 202; the result is
/// fetched from `GET /v1/extensions/calls/{id}` (docs/EXTENSIONS.md §3).
///
/// The caller is a human, and only a person's credential is one: the policy point refuses an
/// extension's token on this route, and this checks again, because the one thing this route
/// must never do is let an extension call a `human` tool as a person.
async fn call_extension_tool(
    State(s): State<Arc<AppState>>,
    Extension(grant): Extension<crate::access::Grant>,
    Path((name, tool)): Path<(String, String)>,
    body: Option<Json<crossagents_api_types::ExtensionCallRequest>>,
) -> ApiResult<(
    StatusCode,
    Json<crossagents_api_types::ExtensionCallAccepted>,
)> {
    if !matches!(grant, crate::access::Grant::Level(_)) {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            ErrorCode::Forbidden,
            "only a person's credential calls an extension tool as a person",
        ));
    }
    let input = body.map(|Json(r)| r.input).unwrap_or_default();
    let accepted = s
        .runner
        .start(crate::extensions::Caller::Human, &name, &tool, input)
        .map_err(call_error)?;
    Ok((StatusCode::ACCEPTED, Json(accepted)))
}

fn no_such_call(id: &str) -> AppError {
    AppError::new(
        StatusCode::NOT_FOUND,
        ErrorCode::NotFound,
        format!("no extension call {id}: it never existed, or it ended more than ten minutes ago"),
    )
}

async fn get_extension_call(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<crossagents_api_types::ExtensionCallResponse>> {
    s.runner.get(&id).map(Json).ok_or_else(|| no_such_call(&id))
}

async fn cancel_extension_call(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<crossagents_api_types::ExtensionCallResponse>> {
    s.runner
        .cancel(&id)
        .await
        .map(Json)
        .ok_or_else(|| no_such_call(&id))
}
#[cfg(test)]
mod candidate_policy_tests {
    use chrono::Utc;
    use crossagents_core::{
        Agent, AgentId, AgentStatus, ApprovalPolicy, Role, Run, RunConfig, RunId, RunPolicy,
        RunStatus,
    };

    #[tokio::test]
    async fn conflicting_plans_without_run_query_return_candidates() {
        let home = tempfile::tempdir().unwrap();
        let repo = home.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let hub = crate::start(crate::HubConfig::new(crate::Paths::new(home.path()), 0))
            .await
            .unwrap();
        let now = Utc::now();
        let agent = Agent {
            id: AgentId::new(),
            name: "Cursor".into(),
            name_source: Default::default(),
            plugin: "cursor".into(),
            driver: None,
            model: None,
            role: Role::Worker,
            home: repo.clone(),
            cwd: repo.clone(),
            config: serde_json::json!({}),
            status: AgentStatus::Stopped,
            created_at: now,
            profile: Default::default(),
            topic: None,
            archived_at: None,
        };
        hub.bus.store().insert_agent(&agent).await.unwrap();
        let mut run = Run {
            origin_chat_id: None,
            work_id: None,
            continuation: None,
            id: RunId::new(),
            title: "plan".into(),
            request: "work".into(),
            planner: agent.id,
            status: RunStatus::AwaitingApproval,
            config: RunConfig {
                worker: agent.id,
                escalate: None,
                reviewer: None,
                auto_approve: false,
                max_parallel: 1,
                max_attempts: 1,
                team: vec![agent.id],
                policy: RunPolicy::default(),
            },
            repo: repo.clone(),
            base_branch: "main".into(),
            branch: "run/test".into(),
            in_place: false,
            mode: None,
            note: None,
            plan: Vec::new(),
            lead_note: None,
            landed: None,
            summary: None,
            checks: Vec::new(),
            checks_state: None,
            checks_snapshot: None,
            created_at: now,
            updated_at: now,
        };
        hub.bus.store().insert_run(&run).await.unwrap();
        run.id = RunId::new();
        run.config.policy.approvals = ApprovalPolicy::Off;
        hub.bus.store().insert_run(&run).await.unwrap();

        let mut url =
            reqwest::Url::parse(&format!("http://{}/v1/session-candidates", hub.addr)).unwrap();
        url.query_pairs_mut()
            .append_pair("repo", repo.to_str().unwrap());
        let response = reqwest::Client::new()
            .get(url)
            .bearer_auth(&hub.token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = response.json().await.unwrap();
        assert!(body.get("error").is_none(), "{body}");
        let candidates = body["sessions"].as_array().unwrap();
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].get("unusable").is_none());
        hub.shutdown();
        hub.wait().await.unwrap();
    }
}
