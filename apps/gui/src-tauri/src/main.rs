#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{path::PathBuf, sync::Mutex, time::Duration};

use eventsource_stream::Eventsource;
use futures_util::{
    StreamExt,
    future::{AbortHandle, Abortable},
};
use reqwest::{Client, Method, Url, redirect::Policy};
use serde::{Deserialize, Serialize};
use tauri::{
    Manager, State, WebviewUrl, WebviewWindowBuilder, ipc::Channel, webview::NewWindowResponse,
};
use tauri_plugin_opener::OpenerExt;

mod browser;
mod browser_bookmarks;
mod chrome;
mod desktop;
mod extensions;
mod file_preview;
mod remote;
mod routes;
#[cfg(feature = "bundled-hub")]
mod startup;
mod terminal;
mod transfer;
mod updates;

struct Hub {
    base: Url,
    token: String,
    remote_generation: Option<u64>,
}

#[derive(Deserialize)]
struct HubFile {
    port: u16,
}

fn hub_home() -> Result<PathBuf, String> {
    std::env::var_os("CROSSAGENTS_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".crossagents")))
        .ok_or_else(|| "Cannot determine the CrossAgents home directory".into())
}

impl Hub {
    async fn discover() -> Result<Self, String> {
        let home = hub_home()?;
        let file = tauri::async_runtime::spawn_blocking(move || {
            let hub = std::fs::read(home.join("hub.json"))
                .map_err(|_| "Local Hub is unavailable. Start it with `crossagents hub start`.")?;
            let token = std::fs::read_to_string(home.join("token"))
                .map_err(|_| "Cannot read the local Hub token")?;
            let hub: HubFile =
                serde_json::from_slice(&hub).map_err(|_| "Cannot read the local Hub port")?;
            let base = Url::parse(&format!("http://127.0.0.1:{}", hub.port))
                .map_err(|_| "Invalid local Hub port")?;
            if token.trim().is_empty() {
                return Err("The local Hub token is empty");
            }
            Ok(Self {
                base,
                token: token.trim().into(),
                remote_generation: None,
            })
        })
        .await
        .map_err(|_| "Cannot read the local Hub configuration")?;
        file.map_err(str::to_owned)
    }

    fn endpoint(&self, method: &Method, path: &str) -> Result<Url, String> {
        let url = self.base.join(path).map_err(|_| "Invalid Hub API path")?;
        if !path.starts_with("/v1/")
            || url.origin() != self.base.origin()
            || url.fragment().is_some()
        {
            return Err("Only local Hub API routes are available".into());
        }
        if !routes::allowed(method, url.path()) {
            return Err("This operation is not available in the desktop client".into());
        }
        Ok(url)
    }
}

#[derive(Serialize)]
struct HubResponse {
    status: u16,
    body: String,
}

#[tauri::command]
async fn hub_request(
    client: State<'_, Client>,
    remote: State<'_, remote::Remote>,
    method: String,
    path: String,
    body: Option<String>,
) -> Result<HubResponse, String> {
    let method = Method::from_bytes(method.as_bytes()).map_err(|_| "Invalid HTTP method")?;
    let hub = remote.hub().await?;
    let url = hub.endpoint(&method, &path)?;
    let mut request = client
        .request(method.clone(), url.clone())
        .bearer_auth(&hub.token)
        .timeout(routes::timeout(&method, url.path()));
    if let Some(body) = body {
        request = request
            .header("content-type", "application/json")
            .body(body);
    }
    let response = remote::client_headers(request, &hub)
        .send()
        .await
        .map_err(|err| err.without_url().to_string())?;
    let status = response.status().as_u16();
    if status == 401 {
        remote.unauthorized(&hub);
    }
    let body = response
        .text()
        .await
        .map_err(|err| err.without_url().to_string())?;
    Ok(HubResponse { status, body })
}

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StreamFrame {
    Connected,
    Event { data: String, id: String },
}

#[derive(Default)]
struct HubRelay {
    abort: Mutex<Option<AbortHandle>>,
    #[cfg(feature = "bundled-hub")]
    startup: startup::BundledHub,
}

#[tauri::command]
async fn dev_hub(
    app: tauri::AppHandle,
    remote: State<'_, remote::Remote>,
) -> Result<Option<serde_json::Value>, String> {
    #[cfg(feature = "bundled-hub")]
    if !remote.is_remote() {
        return Ok(startup::development_hub(&app.package_info().version).await);
    }
    let _ = (app, remote);
    Ok(None)
}

#[tauri::command]
async fn use_shipped_hub(
    app: tauri::AppHandle,
    relay: State<'_, HubRelay>,
    remote: State<'_, remote::Remote>,
) -> Result<(), String> {
    #[cfg(feature = "bundled-hub")]
    if !remote.is_remote() {
        return relay.startup.use_shipped().await;
    }
    let _ = (app, relay, remote);
    Err("当前模式没有可切换的随附 Hub".into())
}

#[tauri::command]
async fn hub_events(
    app: tauri::AppHandle,
    client: State<'_, Client>,
    relay: State<'_, HubRelay>,
    remote: State<'_, remote::Remote>,
    last_id: Option<String>,
    frames: Channel<StreamFrame>,
) -> Result<(), String> {
    let (abort, registration) = AbortHandle::new_pair();
    if let Some(previous) = relay.abort.lock().expect("Hub relay").replace(abort) {
        previous.abort();
    }
    #[cfg(feature = "bundled-hub")]
    if !remote.is_remote() {
        relay
            .startup
            .ensure_running(&client, &app.package_info().version)
            .await?;
    }
    let _ = app;
    Abortable::new(
        stream_hub_events(&client, &remote, last_id, frames),
        registration,
    )
    .await
    .unwrap_or(Ok(()))
}

async fn stream_hub_events(
    client: &Client,
    remote: &remote::Remote,
    last_id: Option<String>,
    frames: Channel<StreamFrame>,
) -> Result<(), String> {
    let hub = remote.hub().await?;
    let mut url = hub.base.join("/v1/events").map_err(|err| err.to_string())?;
    if last_id.is_none() {
        url.set_query(Some("live=true"));
    }
    let mut request = client
        .get(url)
        .bearer_auth(&hub.token)
        .header("accept", "text/event-stream");
    if let Some(id) = last_id {
        request = request.header("last-event-id", id);
    }
    // Bound the wait for headers while letting the SSE response body stay open.
    let request = remote::client_headers(request, &hub);
    let response = tokio::time::timeout(Duration::from_secs(10), request.send())
        .await
        .map_err(|_| "Timed out opening the Hub event stream")?
        .map_err(|err| err.without_url().to_string())?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        remote.unauthorized(&hub);
    }
    let response = response
        .error_for_status()
        .map_err(|err| err.without_url().to_string())?;
    frames
        .send(StreamFrame::Connected)
        .map_err(|err| err.to_string())?;
    let mut events = response.bytes_stream().eventsource();
    while let Some(event) = events.next().await {
        let event = event.map_err(|err| err.to_string())?;
        if frames
            .send(StreamFrame::Event {
                data: event.data,
                id: event.id,
            })
            .is_err()
        {
            break;
        }
    }
    Ok(())
}

fn main() {
    // The updater enables TLS on the shared reqwest build without selecting a process-wide provider.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .build()
        .expect("build local Hub client");
    let commands: fn(tauri::ipc::Invoke<tauri::Wry>) -> bool = tauri::generate_handler![
        hub_request,
        hub_events,
        desktop::pick_folder,
        desktop::open_path,
        desktop::write_clipboard,
        desktop::set_theme,
        desktop::window_command,
        desktop::window_mode_get,
        desktop::window_mode_set,
        desktop::window_mode_listen,
        desktop::window_always_on_top,
        desktop::window_opacity,
        desktop::window_mini_resize,
        desktop::notify,
        desktop::listen_for_notice,
        extensions::extension_safe_mode,
        extensions::extension_loading,
        extensions::extension_ready,
        remote::connection_get,
        remote::connection_pair,
        remote::connection_pair_code,
        remote::connection_use,
        remote::connection_set_share_node,
        remote::connection_reconnect,
        remote::connection_forget,
        remote::connection_import_pairing,
        remote::tailscale_status,
        dev_hub,
        use_shipped_hub,
        terminal::terminal_start,
        terminal::terminal_stop,
        browser::browser_create,
        browser::browser_bounds,
        browser::browser_navigate,
        browser::browser_step,
        browser::browser_style,
        browser::browser_close,
        browser::browser_action,
        browser::browser_screenshot,
        browser::browser_import_session,
        browser::browser_open_external,
        browser_bookmarks::browser_bookmarks,
        file_preview::browser_file_url,
        file_preview::browser_local_file_url,
        chrome::chrome_status,
        chrome::chrome_open_extension,
        chrome::chrome_activate,
        chrome::chrome_action,
        updates::updates_get,
        updates::updates_check,
        updates::updates_install,
    ];
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            desktop::show_window(app)
        }))
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::POSITION
                        | tauri_plugin_window_state::StateFlags::SIZE
                        | tauri_plugin_window_state::StateFlags::MAXIMIZED,
                )
                .build(),
        )
        .manage(client)
        .manage(HubRelay::default())
        .manage(terminal::TerminalRelays::default())
        .manage(desktop::Zoom::default())
        .manage(desktop::WindowModeState::default())
        .manage(desktop::OpenCopies::default())
        .manage(desktop::NoticeListener::default())
        .manage(updates::Updates::default())
        .register_asynchronous_uri_scheme_protocol(
            "crossagents-preview",
            |context, request, reply| {
                let app = context.app_handle().clone();
                let view = context.webview_label().to_owned();
                tauri::async_runtime::spawn(async move {
                    reply.respond(file_preview::serve(app, view, request).await);
                });
            },
        )
        .register_asynchronous_uri_scheme_protocol("crossagents-ext", |context, request, reply| {
            if context.webview_label() != "main" {
                reply.respond(
                    tauri::http::Response::builder()
                        .status(403)
                        .body(Vec::new())
                        .unwrap(),
                );
                return;
            }
            let app = context.app_handle().clone();
            tauri::async_runtime::spawn(async move {
                reply.respond(extensions::serve(app, request).await);
            });
        })
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_opener::Builder::new()
                .open_js_links_on_click(false)
                .build(),
        )
        .invoke_handler(move |invoke: tauri::ipc::Invoke<tauri::Wry>| {
            if invoke.message.webview_ref().label() == "main" {
                commands(invoke)
            } else {
                invoke
                    .resolver
                    .reject("This browser page cannot access CrossAgents");
                true
            }
        })
        .setup(|app| {
            let app_handle = app.handle().clone();
            app.manage(extensions::Extensions::new(&app_handle)?);
            app.manage(remote::Remote::new(&app_handle)?);
            app.manage(file_preview::Previews::new(&app_handle)?);
            chrome::Chrome::setup(&app_handle)?;
            remote::Remote::start(&app_handle);
            let builder =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                    .title("CrossAgents")
                    .inner_size(1280.0, 900.0)
                    .visible(false)
                    .disable_drag_drop_handler();
            #[cfg(target_os = "macos")]
            let builder = builder
                .title_bar_style(tauri::TitleBarStyle::Overlay)
                .hidden_title(true);
            #[cfg(not(target_os = "macos"))]
            let builder = builder.decorations(false);
            builder
                .on_page_load(|window, event| {
                    if event.event() == tauri::webview::PageLoadEvent::Started {
                        browser::close_all(window.app_handle());
                    }
                })
                .on_navigation(|url| {
                    #[cfg(debug_assertions)]
                    if url.scheme() == "http"
                        && url.host_str() == Some("localhost")
                        && url.port() == Some(1420)
                        && url.username().is_empty()
                        && url.password().is_none()
                    {
                        return true;
                    }
                    url.port().is_none()
                        && url.username().is_empty()
                        && url.password().is_none()
                        && matches!(
                            (url.scheme(), url.host_str()),
                            ("http", Some("tauri.localhost")) | ("tauri", Some("localhost"))
                        )
                })
                .on_new_window(move |url, _| {
                    if matches!(url.scheme(), "http" | "https")
                        && url.username().is_empty()
                        && url.password().is_none()
                        && !matches!(
                            url.host_str(),
                            Some(
                                "tauri.localhost"
                                    | "ipc.localhost"
                                    | "crossagents-ext.localhost"
                                    | "crossagents-preview.localhost"
                            )
                        )
                    {
                        let _ = app_handle.opener().open_url(url.as_str(), None::<&str>);
                    }
                    NewWindowResponse::Deny
                })
                .build()?;
            desktop::install_tray(app.handle())?;
            desktop::install_notice_actions(app.handle())?;
            desktop::show_window(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("build CrossAgents Tauri")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                app.state::<remote::Remote>().stop();
            }
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                desktop::show_window(app);
            }
        });
}
