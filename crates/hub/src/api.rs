//! Local HTTP API (docs/DESIGN.md §11). All business logic lives in the hub; clients only
//! call these routes.

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use crossagents_api_types::{
    API_VERSION, ApiError, CreateAgentRequest, ErrorCode, HealthResponse, HubInfo,
    ListAgentsResponse,
};
use crossagents_core::{
    Agent, AgentId, AgentKey, AgentStatus, CoreError, Event, EventBus, EventKind, Store,
    validate_agent_name,
};
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
        .route("/v1/events", get(events))
        .route("/v1/shutdown", post(shutdown))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token));
    Router::new()
        .route("/v1/health", get(health))
        .merge(authed)
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
            CoreError::InvalidName(_) | CoreError::InvalidTransition { .. } => {
                (StatusCode::BAD_REQUEST, ErrorCode::BadRequest)
            }
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
    if req.plugin.trim().is_empty() {
        return Err(AppError::bad_request("plugin must not be empty"));
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

async fn delete_agent(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<StatusCode> {
    let agent = state.store().get_agent(&AgentKey::parse(&key)).await?;
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
}

async fn events(
    State(state): State<Arc<AppState>>,
    Query(query): Query<EventsQuery>,
    headers: HeaderMap,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let last_event_id = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let since = last_event_id.or(query.since).unwrap_or(0);

    let (tx, rx) = mpsc::channel(256);
    let follow = query.follow.unwrap_or(true);
    tokio::spawn(stream_events(state, since, follow, tx));
    let stream = ReceiverStream::new(rx).map(|event| Ok(to_sse(&event)));
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
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
async fn stream_events(state: Arc<AppState>, since: u64, follow: bool, tx: mpsc::Sender<Event>) {
    let mut live = state.bus.subscribe();
    let mut last = since;
    if !replay(&state, &mut last, &tx).await {
        return;
    }
    if !follow {
        return;
    }
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
