use std::{
    path::Path,
    sync::{LazyLock, Mutex},
    time::Duration,
};

use regex::Regex;
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State, Theme, WebviewWindow, ipc::Channel};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;

use crate::{
    Hub,
    remote::{Remote, client_headers},
};

#[tauri::command]
pub fn write_clipboard(app: AppHandle, text: String) -> Result<(), String> {
    app.clipboard()
        .write_text(text)
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub fn set_theme(window: WebviewWindow, dark: bool) -> Result<(), String> {
    window
        .set_theme(Some(if dark { Theme::Dark } else { Theme::Light }))
        .map_err(|err| err.to_string())
}

#[derive(Default)]
pub struct NoticeListener(Mutex<Option<Channel<String>>>);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    title: String,
    body: String,
    agent_id: Option<String>,
}

#[tauri::command]
pub fn listen_for_notice(
    window: WebviewWindow,
    listener: State<'_, NoticeListener>,
    channel: Channel<String>,
) {
    if window.label() == "main" {
        *listener.0.lock().expect("notice listener") = Some(channel);
    }
}

#[tauri::command]
pub fn notify(window: WebviewWindow, notice: Notice) -> Result<(), String> {
    if window.is_focused().unwrap_or(true) {
        return Ok(());
    }
    let mut toast = window
        .notification()
        .builder()
        .title(notice.title.chars().take(120).collect::<String>())
        .body(notice.body.chars().take(300).collect::<String>());
    if let Some(agent) = notice.agent_id {
        toast = toast.extra("agentId", agent);
    }
    toast.show().map_err(|err| err.to_string())
}

pub fn install_notice_actions(app: &AppHandle) -> tauri_plugin_notification::Result<()> {
    let handle = app.clone();
    app.notification().on_action(move |action| {
        if action.action_id() != "tap" {
            return;
        }
        show_window(&handle);
        if let Some(agent) = action
            .notification()
            .and_then(|toast| toast.extra().get("agentId"))
            .and_then(serde_json::Value::as_str)
            && let Some(channel) = &*handle
                .state::<NoticeListener>()
                .0
                .lock()
                .expect("notice listener")
        {
            let _ = channel.send(agent.to_owned());
        }
    })
}

#[derive(Default)]
pub struct Zoom(Mutex<i32>);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WindowAction {
    Drag,
    Minimize,
    Maximize,
    Close,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Reload,
    Devtools,
    Fullscreen,
    Quit,
}

#[tauri::command]
pub fn window_command(
    window: WebviewWindow,
    zoom: State<'_, Zoom>,
    action: WindowAction,
) -> Result<(), String> {
    let result = match action {
        WindowAction::Drag => window.start_dragging(),
        WindowAction::Minimize => window.minimize(),
        WindowAction::Maximize => window.is_maximized().and_then(|max| {
            if max {
                window.unmaximize()
            } else {
                window.maximize()
            }
        }),
        WindowAction::Close => window.close(),
        WindowAction::Reload => window.reload(),
        WindowAction::Fullscreen => window
            .is_fullscreen()
            .and_then(|full| window.set_fullscreen(!full)),
        WindowAction::Quit => {
            window.app_handle().exit(0);
            Ok(())
        }
        WindowAction::Devtools => {
            #[cfg(debug_assertions)]
            if window.is_devtools_open() {
                window.close_devtools();
            } else {
                window.open_devtools();
            }
            Ok(())
        }
        action => {
            let mut step = zoom.0.lock().expect("window zoom");
            let next = match action {
                WindowAction::ZoomIn => (*step + 1).min(8),
                WindowAction::ZoomOut => (*step - 1).max(-6),
                _ => 0,
            };
            window
                .set_zoom(1.2_f64.powf(f64::from(next) * 0.5))
                .map_err(|err| err.to_string())?;
            *step = next;
            Ok(())
        }
    };
    result.map_err(|err| err.to_string())
}

fn build_tray_menu(
    app: &AppHandle,
    stopping: bool,
) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};

    let remote_summary = app.state::<Remote>().remote_summary();
    let status_label = match &remote_summary {
        Some((target, connected)) => format!(
            "远程：{target}（{}）",
            if *connected { "已连接" } else { "未连接" }
        ),
        None if stopping => "正在停止后台…".into(),
        None => {
            let running = crate::hub_home()
                .map(|home| home.join("hub.json").exists())
                .unwrap_or(false);
            if running {
                "后台运行中"
            } else {
                "后台未运行"
            }
            .into()
        }
    };

    let menu = Menu::new(app)?;
    let status = MenuItem::with_id(app, "status", status_label, false, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let show = MenuItem::with_id(app, "show", "打开 CrossAgents", !stopping, None::<&str>)?;
    menu.append(&status)?;
    menu.append(&sep1)?;
    menu.append(&show)?;

    let update_state = crate::updates::current_state(app);
    let update_kind = update_state["state"].as_str().unwrap_or("idle");
    if update_kind != "manual" {
        let (id, label, enabled) = match update_kind {
            "ready" => {
                let version = update_state["version"].as_str().unwrap_or("");
                (
                    "update-install",
                    format!("重启并更新到 {version}"),
                    !stopping,
                )
            }
            "downloading" => {
                let version = update_state["version"].as_str().unwrap_or("");
                let percent = update_state["percent"].as_u64().unwrap_or(0);
                (
                    "update-status",
                    format!("正在下载 {version}（{percent}%）"),
                    false,
                )
            }
            "checking" => ("update-status", "正在检查更新…".into(), false),
            _ => {
                let current = update_state["current"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| app.package_info().version.to_string());
                (
                    "update-check",
                    format!("检查更新（当前 {current}）"),
                    !stopping,
                )
            }
        };
        let item = MenuItem::with_id(app, id, label, enabled, None::<&str>)?;
        menu.append(&item)?;
    }

    let safe_mode = MenuItem::with_id(app, "safe-mode", "以安全模式重启", !stopping, None::<&str>)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    menu.append(&safe_mode)?;
    menu.append(&sep2)?;

    if remote_summary.is_some() {
        let quit = MenuItem::with_id(app, "quit", "退出界面", true, None::<&str>)?;
        menu.append(&quit)?;
    } else {
        let quit = MenuItem::with_id(
            app,
            "quit",
            "退出界面（后台继续运行）",
            !stopping,
            None::<&str>,
        )?;
        menu.append(&quit)?;
        #[cfg(feature = "bundled-hub")]
        {
            let stop = MenuItem::with_id(
                app,
                "stop-background",
                "停止后台并退出",
                !stopping,
                None::<&str>,
            )?;
            menu.append(&stop)?;
        }
    }
    Ok(menu)
}

pub fn refresh_tray(app: &AppHandle) {
    if let Some(tray) = app.tray_by_id("main") {
        if let Ok(menu) = build_tray_menu(app, false) {
            let _ = tray.set_menu(Some(menu));
        }
        let update_state = crate::updates::current_state(app);
        if update_state["state"].as_str() == Some("ready")
            && let Some(version) = update_state["version"].as_str()
        {
            let _ = tray.set_tooltip(Some(&format!("CrossAgents：{version} 已下载，退出时安装")));
        }
    }
}

fn restart_safe_mode(app: &AppHandle) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg("--safe-mode").spawn();
        app.exit(0);
    }
}

pub fn install_tray(app: &AppHandle) -> tauri::Result<()> {
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let menu = build_tray_menu(app, false)?;
    let mut tray = TrayIconBuilder::with_id("main")
        .menu(&menu)
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .tooltip("CrossAgents")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_window(app),
            "update-check" => crate::updates::check_from_tray(app.clone()),
            "update-install" => crate::updates::install_from_tray(app.clone()),
            "safe-mode" => restart_safe_mode(app),
            "quit" => app.exit(0),
            #[cfg(feature = "bundled-hub")]
            "stop-background" => stop_background(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if cfg!(not(target_os = "macos"))
                && matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                )
            {
                show_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

#[cfg(feature = "bundled-hub")]
fn stop_background(app: &AppHandle) {
    use tauri_plugin_dialog::{MessageDialogButtons, MessageDialogKind};
    if app.state::<Remote>().is_remote() {
        return;
    }
    let app = app.clone();
    app.clone()
        .dialog()
        .message("停止本机 Hub 和所有正在运行的智能体，并退出 CrossAgents？")
        .title("停止后台")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancel)
        .show(move |confirmed| {
            if !confirmed {
                return;
            }
            tauri::async_runtime::spawn(async move {
                if app.state::<Remote>().is_remote() {
                    return;
                }
                match app.state::<crate::HubRelay>().startup.stop_all().await {
                    Ok(()) => app.exit(0),
                    Err(error) => app
                        .dialog()
                        .message(error)
                        .title("停止后台失败")
                        .show(|_| {}),
                }
            });
        });
}

pub fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[tauri::command]
pub async fn pick_folder(
    window: WebviewWindow,
    remote: State<'_, Remote>,
) -> Result<Option<String>, String> {
    if remote.is_remote() {
        return Err("远程模式下不能选择本机目录".into());
    }
    let (send, receive) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_parent(&window)
        .pick_folder(move |folder| {
            let _ = send.send(folder);
        });
    receive
        .await
        .map_err(|err| err.to_string())?
        .map(|folder| {
            folder
                .into_path()
                .map_err(|err| err.to_string())?
                .into_os_string()
                .into_string()
                .map_err(|_| "路径无法用 Unicode 表示".to_owned())
        })
        .transpose()
}

#[derive(Deserialize)]
struct ResolvedPaths {
    entries: Vec<ResolvedPath>,
}

#[derive(Deserialize)]
pub(crate) struct ResolvedPath {
    pub path: Option<String>,
    pub abs_path: Option<String>,
}

#[derive(Serialize)]
pub struct OpenedPath {
    copy: bool,
}

#[derive(Default)]
pub struct OpenCopies(Mutex<Vec<tempfile::TempDir>>);

#[tauri::command]
pub async fn open_path(
    app: AppHandle,
    client: State<'_, Client>,
    remote: State<'_, Remote>,
    copies: State<'_, OpenCopies>,
    scope: String,
    path: String,
) -> Result<OpenedPath, String> {
    let hub = remote.hub().await?;
    let entry = resolve_path(&client, &remote, &hub, &scope, &path).await?;
    let relative = entry.path.ok_or("文件不存在或不在工作区内")?;
    if hub.remote_generation.is_some() {
        let (directory, path) = download_copy(&client, &remote, &hub, &scope, &relative).await?;
        app.opener()
            .open_path(path, None::<&str>)
            .map_err(|err| err.to_string())?;
        copies.0.lock().expect("remote copies").push(directory);
        return Ok(OpenedPath { copy: true });
    }
    let target = entry.abs_path.ok_or("文件不存在或不在工作区内")?;
    let file = Path::new(&target);
    if !file.is_absolute() || runs_when_opened(file) {
        return Err("可执行文件或脚本不会用默认程序打开，请在查看器里阅读".into());
    }
    app.opener()
        .open_path(target, None::<&str>)
        .map_err(|err| err.to_string())?;
    Ok(OpenedPath { copy: false })
}

pub(crate) async fn resolve_path(
    client: &Client,
    remote: &Remote,
    hub: &Hub,
    scope: &str,
    path: &str,
) -> Result<ResolvedPath, String> {
    if path.is_empty() || !valid_scope(scope) {
        return Err("需要工作区中的文件路径".into());
    }
    let url = hub.endpoint(&Method::POST, &format!("{scope}/files/resolve"))?;
    // The Hub checks the resolved path against the workspace root in both connection modes.
    let request = client
        .post(url)
        .bearer_auth(&hub.token)
        .timeout(Duration::from_secs(30))
        .json(&serde_json::json!({ "paths": [path] }));
    let response = client_headers(request, hub)
        .send()
        .await
        .map_err(|err| err.without_url().to_string())?;
    if !response.status().is_success() {
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            remote.unauthorized(hub);
        }
        let error: serde_json::Value = response.json().await.unwrap_or_default();
        return Err(error["error"]["message"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Hub 返回 {status}")));
    }
    let resolved = response
        .json::<ResolvedPaths>()
        .await
        .map_err(|err| err.to_string())?;
    resolved
        .entries
        .into_iter()
        .next()
        .ok_or_else(|| "文件不存在或不在工作区内".into())
}

pub(crate) fn valid_scope(scope: &str) -> bool {
    static FILE_SCOPE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\A/v1/(agents/[^/?#\\\p{C}]+|runs/[^/?#\\\p{C}]+(/tasks/[^/?#\\\p{C}]+)?)\z")
            .unwrap()
    });
    FILE_SCOPE.is_match(scope)
}

async fn download_copy(
    client: &Client,
    remote: &Remote,
    hub: &Hub,
    scope: &str,
    relative: &str,
) -> Result<(tempfile::TempDir, String), String> {
    use tokio::io::AsyncWriteExt;
    const LIMIT: usize = 64 * 1024 * 1024;
    let base = relative.rsplit(['/', '\\']).next().unwrap_or_default();
    let name: String = base
        .chars()
        .take(160)
        .map(|character| {
            if character.is_control() || "<>:\"/\\|?*".contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect();
    let name = format!("download-{}", name.trim_end_matches(['.', ' ']));
    if runs_when_opened(Path::new(relative)) || runs_when_opened(Path::new(&name)) {
        return Err("可执行文件或脚本不会用默认程序打开，请在查看器里阅读".into());
    }
    let mut url = hub
        .base
        .join(&format!("{scope}/files/raw"))
        .map_err(|err| err.to_string())?;
    if url.origin() != hub.base.origin() || url.fragment().is_some() {
        return Err("无效的文件下载地址".into());
    }
    url.query_pairs_mut().append_pair("path", relative);
    let request = client
        .get(url)
        .bearer_auth(&hub.token)
        .timeout(Duration::from_secs(600));
    let mut response = client_headers(request, hub)
        .send()
        .await
        .map_err(|err| err.without_url().to_string())?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        remote.unauthorized(hub);
    }
    if !response.status().is_success() {
        return Err(format!("无法下载文件：{}", response.status()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > LIMIT as u64)
    {
        return Err("文件超过 64 MiB".into());
    }
    let directory = tempfile::Builder::new()
        .prefix("crossagents-open-")
        .tempdir()
        .map_err(|err| err.to_string())?;
    let path = directory.path().join(name);
    let mut file = tokio::fs::File::create_new(&path)
        .await
        .map_err(|err| err.to_string())?;
    let mut total = 0;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| err.without_url().to_string())?
    {
        total += chunk.len();
        if total > LIMIT {
            return Err("文件超过 64 MiB".into());
        }
        file.write_all(&chunk)
            .await
            .map_err(|err| err.to_string())?;
    }
    file.flush().await.map_err(|err| err.to_string())?;
    drop(file);
    let path = path
        .into_os_string()
        .into_string()
        .map_err(|_| "下载路径无法用 Unicode 表示".to_owned())?;
    #[cfg(windows)]
    tokio::fs::write(
        format!("{path}:Zone.Identifier"),
        "[ZoneTransfer]\r\nZoneId=3\r\n",
    )
    .await
    .map_err(|err| err.to_string())?;
    #[cfg(target_os = "macos")]
    {
        let quarantine = format!(
            "0081;{:x};CrossAgents;",
            time::OffsetDateTime::now_utc().unix_timestamp()
        );
        let status = tokio::process::Command::new("/usr/bin/xattr")
            .args(["-w", "com.apple.quarantine", &quarantine, &path])
            .status()
            .await
            .map_err(|err| err.to_string())?;
        if !status.success() {
            return Err("无法标记远程下载文件".into());
        }
    }
    Ok((directory, path))
}

fn runs_when_opened(path: &Path) -> bool {
    static EXECUTABLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\.(exe|com|bat|cmd|ps1|psm1|vbs|vbe|js|jse|wsf|wsh|msi|msp|scr|pif|cpl|hta|lnk|url|reg|jar|app|command|sh|desktop|appimage|terminal|fileloc)$").unwrap()
    });
    path.components().any(|part| {
        EXECUTABLE.is_match(
            part.as_os_str()
                .to_string_lossy()
                .trim_end_matches(['.', ' ']),
        )
    })
}
