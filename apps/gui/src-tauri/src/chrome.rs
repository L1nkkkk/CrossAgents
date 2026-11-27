use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State as HttpState},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::{Notify, oneshot};

const PORT: u16 = 7422;
const ALIVE: Duration = Duration::from_secs(20);

#[derive(Clone, Deserialize)]
struct Reported {
    id: Option<u32>,
    url: Option<String>,
    title: Option<String>,
    #[serde(default)]
    active: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Tab {
    tab: String,
    raw_id: u32,
    url: String,
    title: String,
    active: bool,
}

#[derive(Deserialize)]
struct Poll {
    tabs: Vec<Reported>,
}

#[derive(Deserialize)]
struct Reply {
    id: u64,
    ok: bool,
    value: Option<Value>,
    error: Option<String>,
}

#[derive(Default)]
struct Queue {
    tabs: Vec<Tab>,
    last: Option<Instant>,
    commands: VecDeque<Value>,
    pending: HashMap<u64, oneshot::Sender<Result<Value, String>>>,
}

impl Queue {
    fn connected(&self) -> bool {
        self.last.is_some_and(|last| last.elapsed() < ALIVE)
    }
}

pub struct Chrome {
    directory: PathBuf,
    authorization: String,
    state: Mutex<Queue>,
    seq: AtomicU64,
    notify: Notify,
}

impl Chrome {
    pub fn setup(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
        let directory = app.path().app_data_dir()?.join("chrome-extension");
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&directory)?;
        let key_path = directory.join("bridge.key");
        let current = fs::read_to_string(&key_path).ok().filter(|key| {
            URL_SAFE_NO_PAD
                .decode(key)
                .is_ok_and(|bytes| bytes.len() == 32)
        });
        let key = match current {
            Some(key) => key,
            None => {
                let mut bytes = [0; 32];
                SystemRandom::new()
                    .fill(&mut bytes)
                    .map_err(|_| "无法创建 Chrome 桥接密钥")?;
                let key = URL_SAFE_NO_PAD.encode(bytes);
                fs::write(&key_path, &key)?;
                key
            }
        };
        let manifest = json!({
            "manifest_version": 3,
            "name": "CrossAgents Chrome Bridge",
            "version": "1.2.0",
            "permissions": ["tabs", "debugger", "alarms"],
            "host_permissions": ["http://127.0.0.1/*"],
            "background": { "service_worker": "background.js" },
        });
        fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest)?,
        )?;
        fs::write(
            directory.join("background.js"),
            format!(
                "const BRIDGE = \"http://127.0.0.1:{PORT}\";\nconst BRIDGE_KEY = {};\n{}",
                serde_json::to_string(&key)?,
                include_str!("chrome_worker.js")
            ),
        )?;
        let bridge = Arc::new(Self {
            directory,
            authorization: format!("Bearer {key}"),
            state: Default::default(),
            seq: AtomicU64::new(1),
            notify: Notify::new(),
        });
        app.manage(bridge.clone());
        tauri::async_runtime::spawn(async move {
            let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", PORT)).await else {
                return;
            };
            let router = Router::new()
                .route("/poll", post(poll).options(preflight))
                .route("/reply", post(reply).options(preflight))
                .layer(DefaultBodyLimit::max(16 * 1024 * 1024))
                .with_state(bridge);
            let _ = axum::serve(listener, router).await;
        });
        Ok(())
    }

    fn tab_id(&self, value: &Value) -> Result<u32, String> {
        let id = value
            .as_str()
            .and_then(|tab| tab.strip_prefix("chrome:"))
            .and_then(|id| id.parse::<u32>().ok())
            .ok_or("请选择已连接的 Chrome 标签页")?;
        let state = self.state.lock().expect("Chrome bridge");
        if !state.connected() || !state.tabs.iter().any(|tab| tab.raw_id == id) {
            return Err("Chrome 标签页已断开或已关闭".into());
        }
        Ok(id)
    }

    async fn command(&self, mut command: Value) -> Result<Value, String> {
        let id = self.seq.fetch_add(1, Ordering::Relaxed);
        let (send, receive) = oneshot::channel();
        {
            let mut state = self.state.lock().expect("Chrome bridge");
            if !state.connected() {
                return Err("请先在 Chrome 加载 CrossAgents 扩展".into());
            }
            if state.pending.len() >= 24 {
                return Err("Chrome 当前等待的操作过多".into());
            }
            command["id"] = json!(id);
            state.commands.push_back(command);
            state.pending.insert(id, send);
        }
        self.notify.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(18), receive).await;
        {
            let mut state = self.state.lock().expect("Chrome bridge");
            state.pending.remove(&id);
            state.commands.retain(|command| command["id"] != id);
        }
        result
            .map_err(|_| "等待 Chrome 扩展响应超时".to_owned())?
            .map_err(|_| "Chrome 扩展已断开".to_owned())?
    }
}

fn origin(headers: &HeaderMap) -> Option<&str> {
    let name = headers.get(header::ORIGIN)?.to_str().ok()?;
    let id = name.strip_prefix("chrome-extension://")?;
    (id.len() == 32 && id.bytes().all(|byte| (b'a'..=b'p').contains(&byte))).then_some(name)
}

fn cors(origin: &str, mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        origin.parse().expect("valid extension origin"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        "POST, OPTIONS".parse().unwrap(),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        "Authorization, Content-Type".parse().unwrap(),
    );
    headers.insert(
        "access-control-allow-private-network",
        "true".parse().unwrap(),
    );
    headers.insert(header::VARY, "Origin".parse().unwrap());
    response
}

async fn preflight(headers: HeaderMap) -> Response {
    origin(&headers).map_or_else(
        || StatusCode::FORBIDDEN.into_response(),
        |origin| cors(origin, StatusCode::NO_CONTENT.into_response()),
    )
}

fn authorized<'a>(bridge: &Chrome, headers: &'a HeaderMap) -> Option<&'a str> {
    let origin = origin(headers)?;
    let provided = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    constant_time_eq::constant_time_eq(provided.as_bytes(), bridge.authorization.as_bytes())
        .then_some(origin)
}

fn browser_url(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
    })
}

async fn poll(
    HttpState(bridge): HttpState<Arc<Chrome>>,
    headers: HeaderMap,
    Json(body): Json<Poll>,
) -> Response {
    let Some(origin) = authorized(&bridge, &headers) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let mut commands = {
        let mut state = bridge.state.lock().expect("Chrome bridge");
        state.last = Some(Instant::now());
        state.tabs = body
            .tabs
            .into_iter()
            .take(200)
            .filter_map(|tab| {
                let id = tab.id.filter(|id| *id > 0)?;
                let url = tab.url.filter(|url| browser_url(url))?;
                let title = tab
                    .title
                    .filter(|title| !title.trim().is_empty())
                    .unwrap_or_else(|| url.clone());
                Some(Tab {
                    tab: format!("chrome:{id}"),
                    raw_id: id,
                    url,
                    title,
                    active: tab.active,
                })
            })
            .collect();
        state.commands.drain(..).collect::<Vec<_>>()
    };
    if commands.is_empty() {
        let _ = tokio::time::timeout(Duration::from_secs(12), bridge.notify.notified()).await;
        commands = bridge
            .state
            .lock()
            .expect("Chrome bridge")
            .commands
            .drain(..)
            .collect();
    }
    cors(
        origin,
        Json(json!({ "commands": commands })).into_response(),
    )
}

async fn reply(
    HttpState(bridge): HttpState<Arc<Chrome>>,
    headers: HeaderMap,
    Json(reply): Json<Reply>,
) -> Response {
    let Some(origin) = authorized(&bridge, &headers) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let mut state = bridge.state.lock().expect("Chrome bridge");
    state.last = Some(Instant::now());
    if let Some(send) = state.pending.remove(&reply.id) {
        let result = if reply.ok {
            Ok(reply.value.unwrap_or(Value::Null))
        } else {
            Err(reply.error.unwrap_or_else(|| "Chrome 扩展执行失败".into()))
        };
        let _ = send.send(result);
    }
    cors(origin, Json(json!({ "ok": true })).into_response())
}

#[tauri::command]
pub fn chrome_status(bridge: State<'_, Arc<Chrome>>) -> Value {
    let state = bridge.state.lock().expect("Chrome bridge");
    let connected = state.connected();
    json!({ "connected": connected, "extensionDir": bridge.directory, "port": PORT, "tabs": if connected { &state.tabs[..] } else { &[] } })
}

#[tauri::command]
pub fn chrome_open_extension(
    app: AppHandle,
    bridge: State<'_, Arc<Chrome>>,
) -> Result<Value, String> {
    let path = bridge.directory.to_str().ok_or("扩展目录路径无效")?;
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|err| err.to_string())?;
    Ok(json!({ "dir": path }))
}

#[tauri::command]
pub async fn chrome_activate(bridge: State<'_, Arc<Chrome>>, tab: String) -> Result<Value, String> {
    let id = bridge.tab_id(&json!(tab))?;
    bridge
        .command(json!({ "kind": "activate", "tabId": id }))
        .await
}

#[tauri::command]
pub async fn chrome_action(bridge: State<'_, Arc<Chrome>>, input: Value) -> Result<Value, String> {
    let tool = input["tool"].as_str().ok_or("缺少 Chrome 操作")?;
    if tool == "browser_open" {
        let url = input["url"]
            .as_str()
            .filter(|url| browser_url(url))
            .ok_or("请填写 HTTP 或 HTTPS 地址")?;
        let id = match input["tab"].as_str() {
            None | Some("chrome" | "chrome:new") => None,
            _ => Some(bridge.tab_id(&input["tab"])?),
        };
        return bridge
            .command(json!({ "kind": "open", "url": url, "tabId": id }))
            .await;
    }
    let id = bridge.tab_id(&input["tab"])?;
    if tool == "browser_navigate" {
        let action = input["action"]
            .as_str()
            .filter(|action| matches!(*action, "back" | "forward" | "reload"))
            .ok_or("无效的导航操作")?;
        return bridge
            .command(json!({ "kind": "navigate", "tabId": id, "action": action }))
            .await;
    }
    if tool == "browser_screenshot" {
        return bridge
            .command(json!({ "kind": "screenshot", "tabId": id }))
            .await;
    }
    if tool == "browser_console" {
        return bridge
            .command(json!({ "kind": "console", "tabId": id }))
            .await;
    }
    if tool == "browser_close" {
        return bridge
            .command(json!({ "kind": "close", "tabId": id }))
            .await;
    }
    let script = match tool {
        "browser_snapshot" => json!({
            "action": "snapshot",
            "selector": input.get("selector"),
            "max_chars": input.get("max_chars"),
        }),
        "browser_click" => json!({ "action": "click", "reference": input["ref"] }),
        "browser_type" => json!({
            "action": "type",
            "reference": input["ref"],
            "text": input["text"],
            "submit": input["submit"],
            "clear": input.get("clear").and_then(Value::as_bool).unwrap_or(true),
        }),
        "browser_select" => json!({
            "action": "select",
            "reference": input["ref"],
            "value": input["value"],
        }),
        "browser_press" => json!({ "action": "press", "key": input["key"] }),
        "browser_scroll" => json!({
            "action": "scroll",
            "dy": input["dy"],
            "reference": input.get("ref"),
        }),
        "browser_probe" => json!({
            "action": "probe",
            "text": input.get("text"),
            "text_gone": input.get("text_gone"),
            "selector": input.get("selector"),
            "url": input.get("url"),
        }),
        _ => return Err("这个 Chrome 操作还未接入".into()),
    };
    let expression = format!("({})({script})", include_str!("browser_page.js"));
    bridge
        .command(json!({ "kind": "evaluate", "tabId": id, "expression": expression }))
        .await
}
