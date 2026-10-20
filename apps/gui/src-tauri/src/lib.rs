//! The GUI's Rust side is transport only (docs/tasks/V2-GUI.md §2): it finds the hub,
//! adds the bearer token, forwards HTTP requests and relays the SSE event stream. All
//! business logic stays in the hub.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, State};

const HOME_ENV: &str = "CROSSAGENTS_HOME";
const EVENT: &str = "hub://event";
const STATUS: &str = "hub://status";

#[derive(Default)]
struct Hub {
    client: reqwest::Client,
    events: Mutex<Option<JoinHandle<()>>>,
}

/// `$CROSSAGENTS_HOME`, else `~/.crossagents` — the same rule as the hub.
fn home() -> Result<PathBuf, String> {
    if let Some(home) = std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(home));
    }
    dirs::home_dir()
        .map(|h| h.join(".crossagents"))
        .ok_or_else(|| "cannot determine the user's home directory".to_owned())
}

#[derive(Deserialize)]
struct HubFile {
    port: u16,
}

/// Base URL and token of the running hub. Read on every call, so a restarted hub (new
/// port or token) is picked up without restarting the GUI.
fn connection() -> Result<(String, String), String> {
    let home = home()?;
    let hub_file = home.join("hub.json");
    let text = std::fs::read_to_string(&hub_file).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!(
                "hub is not running ({} not found); start it with `crossagents hub start`",
                hub_file.display()
            )
        } else {
            format!("cannot read {}: {e}", hub_file.display())
        }
    })?;
    let hub: HubFile =
        serde_json::from_str(&text).map_err(|e| format!("invalid {}: {e}", hub_file.display()))?;
    let token_file = home.join("token");
    let token = std::fs::read_to_string(&token_file)
        .map_err(|e| format!("cannot read {}: {e}", token_file.display()))?;
    Ok((
        format!("http://127.0.0.1:{}", hub.port),
        token.trim().to_owned(),
    ))
}

#[derive(Serialize)]
struct HubResponse {
    status: u16,
    /// Raw body; the frontend parses it with the generated API types.
    body: String,
}

#[tauri::command]
fn hub_home() -> Result<String, String> {
    Ok(home()?.display().to_string())
}

#[tauri::command]
async fn hub_request(
    hub: State<'_, Hub>,
    method: String,
    path: String,
    body: Option<String>,
) -> Result<HubResponse, String> {
    if !path.starts_with("/v1/") {
        return Err(format!("not an API path: {path}"));
    }
    let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
    let (base, token) = connection()?;
    let mut request = hub
        .client
        .request(method, format!("{base}{path}"))
        .bearer_auth(token)
        .timeout(Duration::from_secs(30));
    if let Some(body) = body {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("cannot reach the hub: {e}"))?;
    let status = response.status().as_u16();
    let body = response.text().await.map_err(|e| e.to_string())?;
    Ok(HubResponse { status, body })
}

/// (Re)starts relaying `GET /v1/events` from the beginning of history. Called once per
/// page load, after the page has subscribed to `hub://event`.
#[tauri::command]
fn hub_events_start(app: AppHandle, hub: State<'_, Hub>) {
    let client = hub.client.clone();
    let task = tauri::async_runtime::spawn(relay_events(app, client));
    if let Some(old) = hub.events.lock().unwrap().replace(task) {
        old.abort();
    }
}

async fn relay_events(app: AppHandle, client: reqwest::Client) {
    let mut last: Option<u64> = None;
    loop {
        let _ = app.emit(STATUS, "connecting");
        let err = match stream_once(&app, &client, &mut last).await {
            Ok(()) => "event stream ended".to_owned(),
            Err(err) => err,
        };
        let _ = app.emit(STATUS, format!("offline: {err}"));
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Reads one SSE connection, resuming after `last` with `Last-Event-ID`.
async fn stream_once(
    app: &AppHandle,
    client: &reqwest::Client,
    last: &mut Option<u64>,
) -> Result<(), String> {
    let (base, token) = connection()?;
    let mut request = client
        .get(format!("{base}/v1/events"))
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "text/event-stream");
    if let Some(seq) = last {
        request = request.header("Last-Event-ID", seq.to_string());
    }
    let response = request.send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("hub answered {}", response.status()));
    }
    let _ = app.emit(STATUS, "connected");

    let mut stream = response.bytes_stream();
    // Bytes, not text: a chunk may end inside a multi-byte character.
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        buf.extend(chunk.iter().filter(|&&b| b != b'\r'));
        while let Some(end) = buf.windows(2).position(|w| w == b"\n\n") {
            let block: Vec<u8> = buf.drain(..end + 2).collect();
            let message = parse_sse(&String::from_utf8_lossy(&block));
            if let Some(id) = message.id.and_then(|id| id.parse().ok()) {
                *last = Some(id);
            }
            if let Some(data) = message.data {
                let _ = app.emit(EVENT, data);
            }
        }
    }
    Ok(())
}

#[derive(Debug, Default, PartialEq)]
struct SseMessage {
    id: Option<String>,
    data: Option<String>,
}

/// Parses one SSE block. Comments (keep-alives) and the `event:` field are ignored: the
/// event type is also inside the JSON data.
fn parse_sse(block: &str) -> SseMessage {
    let mut message = SseMessage::default();
    for line in block.lines() {
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "id" => message.id = Some(value.to_owned()),
            "data" => match &mut message.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => message.data = Some(value.to_owned()),
            },
            _ => {}
        }
    }
    message
}

pub fn run() {
    tauri::Builder::default()
        .manage(Hub::default())
        .invoke_handler(tauri::generate_handler![
            hub_home,
            hub_request,
            hub_events_start
        ])
        .run(tauri::generate_context!())
        .expect("error while running the CrossAgents GUI");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sse_blocks() {
        let msg = parse_sse("id: 7\nevent: hub.started\ndata: {\"a\":1}\n\n");
        assert_eq!(msg.id.as_deref(), Some("7"));
        assert_eq!(msg.data.as_deref(), Some("{\"a\":1}"));
        assert_eq!(parse_sse(":\n\n"), SseMessage::default());
        assert_eq!(
            parse_sse("data: a\ndata: b\n").data.as_deref(),
            Some("a\nb")
        );
    }
}
