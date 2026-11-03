//! The terminal routes (`crate::terminal`). All admin (`crate::access`): whoever reaches them runs
//! commands as this machine's user. Output is one long SSE stream and input ordinary requests, so
//! they pass the same token check and the same relay as everything else, with no socket of their
//! own. Mounted under the authed router in [`crate::api`].

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use crossagents_api_types::{
    CreateTerminalRequest, ListTerminalsResponse, TerminalFrame, TerminalInfo,
    TerminalInputRequest, TerminalInputResponse, TerminalResizeRequest, TerminalTakeoverRequest,
};
use crossagents_core::CoreError;
use futures::Stream;
use serde::Deserialize;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::ReceiverStream;

use crate::api::{ApiResult, AppState};
use crate::terminal::{Attached, Terminals};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/terminals", get(list_terminals).post(create_terminal))
        .route("/v1/terminals/{id}", delete(close_terminal))
        .route("/v1/terminals/{id}/output", get(terminal_output))
        .route("/v1/terminals/{id}/input", post(terminal_input))
        .route("/v1/terminals/{id}/resize", post(terminal_resize))
        .route("/v1/terminals/{id}/takeover", post(terminal_takeover))
}

/// Runs what may block on a process or a PTY away from the server's threads.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, CoreError> + Send + 'static,
) -> Result<T, CoreError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| CoreError::Agent(format!("terminal task failed: {err}")))?
}

async fn list_terminals(State(state): State<Arc<AppState>>) -> Json<ListTerminalsResponse> {
    Json(ListTerminalsResponse {
        terminals: state.terminals.list(),
    })
}

async fn create_terminal(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateTerminalRequest>,
) -> ApiResult<(StatusCode, Json<TerminalInfo>)> {
    let terminals = state.terminals.clone();
    let info = blocking(move || terminals.create(&req)).await?;
    Ok((StatusCode::CREATED, Json(info)))
}

async fn close_terminal(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let terminals = state.terminals.clone();
    blocking(move || terminals.close(&id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct OutputQuery {
    /// Who is reading: one window's terminal, across its reconnects.
    connection: String,
}

async fn terminal_output(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<OutputQuery>,
) -> ApiResult<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>> {
    let terminals = state.terminals.clone();
    let attached = blocking(move || terminals.attach(&id, &query.connection)).await?;
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(forward(attached, tx, state.shutdown.clone()));
    let stream = ReceiverStream::new(rx);
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

/// Sends a connection the screen, then everything after it, until the shell or the connection ends.
async fn forward(
    attached: Attached,
    tx: mpsc::Sender<Result<SseEvent, Infallible>>,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let Attached {
        terminal,
        snapshot,
        mut live,
        exit,
        guard: _guard,
    } = attached;
    if !send(&tx, &snapshot).await {
        return;
    }
    if let Some(exit) = exit {
        send(&tx, &exit).await;
        return;
    }
    loop {
        let frame = tokio::select! {
            () = shutdown.cancelled() => return,
            () = tx.closed() => return,
            frame = live.recv() => frame,
        };
        let frame = match frame {
            Ok(frame) => frame,
            // It fell behind: what it missed is in the screen as it is now.
            Err(broadcast::error::RecvError::Lagged(_)) => Terminals::resnapshot(&terminal),
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if !send(&tx, &frame).await || matches!(frame, TerminalFrame::Exit { .. }) {
            return;
        }
    }
}

/// Whether the connection is still there.
async fn send(tx: &mpsc::Sender<Result<SseEvent, Infallible>>, frame: &TerminalFrame) -> bool {
    match SseEvent::default().json_data(frame) {
        Ok(event) => tx.send(Ok(event)).await.is_ok(),
        Err(err) => {
            tracing::warn!(error = %err, "cannot encode a terminal frame");
            true
        }
    }
}

async fn terminal_input(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<TerminalInputRequest>,
) -> ApiResult<Json<TerminalInputResponse>> {
    let terminals = state.terminals.clone();
    let acked = blocking(move || terminals.input(&id, &req.connection, req.seq, &req.data)).await?;
    Ok(Json(TerminalInputResponse { acked }))
}

async fn terminal_resize(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<TerminalResizeRequest>,
) -> ApiResult<StatusCode> {
    let terminals = state.terminals.clone();
    blocking(move || terminals.resize(&id, &req.connection, req.cols, req.rows)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn terminal_takeover(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<TerminalTakeoverRequest>,
) -> ApiResult<StatusCode> {
    let terminals = state.terminals.clone();
    blocking(move || terminals.takeover(&id, &req.connection)).await?;
    Ok(StatusCode::NO_CONTENT)
}
