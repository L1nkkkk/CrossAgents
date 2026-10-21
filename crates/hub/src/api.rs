//! Local HTTP API (docs/DESIGN.md §11). All business logic lives in the hub; clients only
//! call these routes.

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Extension;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use crossagents_api_types::{
    API_VERSION, AccountQuota, AccountUsageGroup, AccountUsageResponse, ApiError,
    CreateAccountRequest, CreateAgentRequest, CreateRunRequest, ErrorCode, HealthResponse, HubInfo,
    ListAccountsResponse, ListAgentsResponse, ListPluginsResponse, ListRunsResponse,
    ListToolsResponse, LoginStarted, PluginInfo, QuotaResponse, RejectRunRequest, RunDetail,
    SendMessageRequest, SendMessageResponse, SetModelRequest, Transcript, UsageGroup,
    UsageResponse,
};
use crossagents_core::{
    Account, AccountStatus, Agent, AgentId, AgentKey, AgentProfile, AgentStatus, CoreError,
    DriverKind, Event, EventBus, EventKind, MessageId, MessageSource, Run, RunConfig, RunId, Store,
    validate_agent_name,
};
use crossagents_orchestrator::{NewRun, Orchestrator};

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

pub struct AppState {
    pub bus: Arc<EventBus>,
    pub runtime: Arc<Runtime>,
    pub orchestrator: Arc<Orchestrator>,
    pub accounts: Arc<Accounts>,
    pub tokens: Arc<AgentTokens>,
    pub token: String,
    pub home: PathBuf,
    pub started_at: DateTime<Utc>,
    pub shutdown: CancellationToken,
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
        .route("/v1/agents/{key}", get(get_agent).delete(delete_agent))
        .route("/v1/agents/{key}/start", post(start_agent))
        .route("/v1/agents/{key}/stop", post(stop_agent))
        .route("/v1/agents/{key}/cancel", post(cancel_agent))
        .route("/v1/agents/{key}/compact", post(compact_agent))
        .route("/v1/agents/{key}/messages", post(send_message))
        .route(
            "/v1/agents/{key}/messages/{message_id}",
            delete(withdraw_message),
        )
        .route("/v1/agents/{key}/transcript", get(transcript))
        .route("/v1/agents/{key}/profile", post(update_profile))
        .route("/v1/agents/{key}/model", post(set_model))
        .route("/v1/plugins", get(list_plugins))
        .route("/v1/usage", get(usage))
        .route("/v1/accounts", get(list_accounts).post(create_account))
        .route("/v1/quota", get(quota))
        .route(
            "/v1/accounts/{id}",
            get(get_account).delete(delete_account_route),
        )
        .route("/v1/accounts/{id}/status", get(account_status))
        .route("/v1/accounts/{id}/usage", get(account_usage))
        .route("/v1/accounts/{id}/models", get(account_models))
        .route("/v1/accounts/{id}/login", post(account_login))
        .route("/v1/runs", get(list_runs).post(create_run))
        .route("/v1/runs/{key}", get(get_run))
        .route("/v1/runs/{key}/approve", post(approve_run))
        .route("/v1/runs/{key}/reject", post(reject_run))
        .route("/v1/runs/{key}/cancel", post(cancel_run))
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
            CoreError::Conflict(_) => (StatusCode::CONFLICT, ErrorCode::Conflict),
            CoreError::InvalidName(_)
            | CoreError::InvalidTransition { .. }
            | CoreError::Unsupported(_) => (StatusCode::BAD_REQUEST, ErrorCode::BadRequest),
            CoreError::Agent(_) => (StatusCode::BAD_GATEWAY, ErrorCode::AgentError),
            CoreError::Storage(_) => {
                tracing::error!(error = %err, "storage error");
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
    if presented.is_some_and(|t| constant_time_eq(t.as_bytes(), state.token.as_bytes())) {
        next.run(request).await
    } else {
        AppError::new(
            StatusCode::UNAUTHORIZED,
            ErrorCode::Unauthorized,
            "missing or invalid bearer token",
        )
        .into_response()
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

async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let uptime = (Utc::now() - state.started_at).num_seconds().max(0) as u64;
    Json(HealthResponse {
        status: "ok".into(),
        version: VERSION.into(),
        api: API_VERSION.into(),
        uptime_secs: uptime,
    })
}

async fn info(State(state): State<Arc<AppState>>) -> Json<HubInfo> {
    Json(HubInfo {
        version: VERSION.into(),
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

async fn create_agent(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateAgentRequest>,
) -> ApiResult<(StatusCode, Json<Agent>)> {
    validate_agent_name(&req.name)?;
    if !state.runtime.plugins().contains(&req.plugin) {
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
    let cwd = dunce::canonicalize(&req.cwd)
        .map_err(|e| AppError::bad_request(format!("cwd {:?}: {e}", req.cwd)))?;
    if !cwd.is_dir() {
        return Err(AppError::bad_request(format!(
            "cwd {:?} is not a directory",
            req.cwd
        )));
    }
    let agent = Agent {
        id: AgentId::new(),
        name: req.name,
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
    let mut fold = crate::transcript::Fold::default();
    loop {
        let page = state
            .store()
            .agent_events(agent.id, fold.last_seq, EVENT_PAGE)
            .await?;
        if page.is_empty() {
            break;
        }
        page.iter().for_each(|e| fold.push(e));
    }
    Ok(Json(Transcript {
        agent,
        driver: resolved.as_ref().map(|r| r.driver.kind()),
        capabilities: resolved.as_ref().map(|r| r.driver.capabilities()),
        turns: fold.turns,
        usage: fold.usage,
        last_seq: head.max(fold.last_seq),
    }))
}

async fn delete_agent(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<StatusCode> {
    let agent = state.store().get_agent(&AgentKey::parse(&key)).await?;
    state.runtime.stop(&AgentKey::Id(agent.id)).await?;
    state.store().delete_agent(agent.id).await?;
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

async fn set_model(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(req): Json<SetModelRequest>,
) -> ApiResult<Json<Agent>> {
    let model = req
        .model
        .map(|m| m.trim().to_owned())
        .filter(|m| !m.is_empty());
    Ok(Json(
        state
            .runtime
            .set_model(&AgentKey::parse(&key), model)
            .await?,
    ))
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
    if req.text.trim().is_empty() {
        return Err(AppError::bad_request("text must not be empty"));
    }
    let key = AgentKey::parse(&key);
    let (message_id, queued) = if req.now == Some(true) {
        state
            .runtime
            .send_now(&key, req.text, MessageSource::Human)
            .await?
    } else {
        state
            .runtime
            .send(&key, req.text, MessageSource::Human)
            .await?
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(SendMessageResponse { message_id, queued }),
    ))
}

async fn list_plugins(State(state): State<Arc<AppState>>) -> Json<ListPluginsResponse> {
    let plugins = state
        .runtime
        .plugins()
        .defaults()
        .iter()
        .map(|d| PluginInfo {
            name: d.name().to_owned(),
            models: d.models(),
            driver: d.kind(),
            capabilities: d.capabilities(),
        })
        .collect();
    Json(ListPluginsResponse { plugins })
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
    let new = NewRun {
        request: req.request,
        title: req.title,
        planner: agent_id(&state, &req.planner).await?,
        config: RunConfig {
            worker: agent_id(&state, &req.worker).await?,
            escalate,
            reviewer,
            auto_approve: req.auto_approve,
            max_parallel: req.max_parallel.unwrap_or(RunConfig::DEFAULT_MAX_PARALLEL),
            max_attempts: req.max_attempts.unwrap_or(RunConfig::DEFAULT_MAX_ATTEMPTS),
            team,
        },
    };
    let run = state.orchestrator.create_run(new).await?;
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
) -> ApiResult<Json<RunDetail>> {
    let id = resolve_run(&state, &key).await?;
    state.orchestrator.approve(id).await?;
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

async fn shutdown(State(state): State<Arc<AppState>>) -> StatusCode {
    tracing::info!("shutdown requested over the API");
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
