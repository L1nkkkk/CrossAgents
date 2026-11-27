use std::{
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
    time::Duration,
};

use regex::Regex;
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, PhysicalPosition, PhysicalSize, State, Theme,
    Webview, Window, ipc::Channel,
};
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

// Commands below intentionally accept `Window` / `Webview` (and `app.get_window("main")`) rather
// than `WebviewWindow` / `app.get_webview_window("main")`: once a built-in browser tab attaches a
// child webview via `window.add_child(...)`, Tauri's `Window::is_webview_window()` becomes `false`
// and `WebviewWindow` extraction fails with "current webview is not a WebviewWindow".

#[tauri::command]
pub fn set_theme(window: Window, dark: bool) -> Result<(), String> {
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
    webview: Webview,
    listener: State<'_, NoticeListener>,
    channel: Channel<String>,
) {
    if webview.label() == "main" {
        *listener.0.lock().expect("notice listener") = Some(channel);
    }
}

#[tauri::command]
pub fn notify(app: AppHandle, window: Window, notice: Notice) -> Result<(), String> {
    if window.is_focused().unwrap_or(true) {
        return Ok(());
    }
    let mut toast = app
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
    window: Window,
    webview: Webview,
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
        WindowAction::Reload => webview.reload(),
        WindowAction::Fullscreen => window
            .is_fullscreen()
            .and_then(|full| window.set_fullscreen(!full)),
        WindowAction::Quit => {
            window.app_handle().exit(0);
            Ok(())
        }
        WindowAction::Devtools => {
            #[cfg(debug_assertions)]
            if webview.is_devtools_open() {
                webview.close_devtools();
            } else {
                webview.open_devtools();
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
            webview
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
    let mode_eff = MenuItem::with_id(app, "mode-efficiency", "效率模式（完整窗口）", !stopping, None::<&str>)?;
    let mode_classic = MenuItem::with_id(app, "mode-classic", "经典模式（侧边窄面板）", !stopping, None::<&str>)?;
    let mode_mini = MenuItem::with_id(app, "mode-mini", "迷你窗（悬浮置顶）", !stopping, None::<&str>)?;
    let sep_modes = PredefinedMenuItem::separator(app)?;
    menu.append(&status)?;
    menu.append(&sep1)?;
    menu.append(&show)?;
    menu.append(&mode_eff)?;
    menu.append(&mode_classic)?;
    menu.append(&mode_mini)?;
    menu.append(&sep_modes)?;

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
            "mode-efficiency" => {
                show_window(app);
                let _ = apply_window_mode(app, WindowMode::Efficiency);
            }
            "mode-classic" => {
                show_window(app);
                let _ = apply_window_mode(app, WindowMode::Classic);
            }
            "mode-mini" => {
                show_window(app);
                let _ = apply_window_mode(app, WindowMode::Mini);
            }
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WindowMode {
    #[default]
    Efficiency,
    Classic,
    Mini,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedBounds {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    maximized: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredModePrefs {
    #[serde(default)]
    mode: WindowMode,
    #[serde(default)]
    efficiency_bounds: Option<SavedBounds>,
    #[serde(default)]
    classic_bounds: Option<SavedBounds>,
    #[serde(default)]
    mini_bounds: Option<SavedBounds>,
    #[serde(default)]
    classic_always_on_top: bool,
    #[serde(default = "default_true_bool")]
    mini_always_on_top: bool,
    #[serde(default = "default_one")]
    classic_opacity: f64,
    #[serde(default = "default_mini_opacity")]
    mini_opacity: f64,
    #[serde(default)]
    mini_collapsed: bool,
    #[serde(default = "default_mini_expanded_height")]
    mini_expanded_height: f64,
}

fn default_true_bool() -> bool {
    true
}

fn default_one() -> f64 {
    1.0
}

fn default_mini_opacity() -> f64 {
    0.94
}

fn default_mini_expanded_height() -> f64 {
    248.0
}

impl Default for StoredModePrefs {
    fn default() -> Self {
        Self {
            mode: WindowMode::Efficiency,
            efficiency_bounds: None,
            classic_bounds: None,
            mini_bounds: None,
            classic_always_on_top: false,
            mini_always_on_top: true,
            classic_opacity: 1.0,
            mini_opacity: 0.94,
            mini_collapsed: false,
            mini_expanded_height: 248.0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowModeSnapshot {
    pub mode: WindowMode,
    pub always_on_top: bool,
    pub opacity: f64,
    pub mini_collapsed: bool,
}

impl StoredModePrefs {
    fn snapshot(&self) -> WindowModeSnapshot {
        match self.mode {
            WindowMode::Efficiency => WindowModeSnapshot {
                mode: WindowMode::Efficiency,
                always_on_top: false,
                opacity: 1.0,
                mini_collapsed: self.mini_collapsed,
            },
            WindowMode::Classic => WindowModeSnapshot {
                mode: WindowMode::Classic,
                always_on_top: self.classic_always_on_top,
                opacity: self.classic_opacity,
                mini_collapsed: self.mini_collapsed,
            },
            WindowMode::Mini => WindowModeSnapshot {
                mode: WindowMode::Mini,
                always_on_top: self.mini_always_on_top,
                opacity: self.mini_opacity,
                mini_collapsed: self.mini_collapsed,
            },
        }
    }
}

#[derive(Default)]
pub struct WindowModeState {
    inner: Mutex<WindowModeInner>,
}

#[derive(Default)]
struct WindowModeInner {
    loaded: bool,
    prefs: StoredModePrefs,
    listener: Option<Channel<WindowModeSnapshot>>,
}

fn prefs_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|dir| dir.join("window-modes.json"))
}

fn load_prefs(app: &AppHandle, inner: &mut WindowModeInner) {
    if inner.loaded {
        return;
    }
    inner.loaded = true;
    if let Some(path) = prefs_path(app)
        && let Ok(bytes) = std::fs::read(path)
        && let Ok(parsed) = serde_json::from_slice::<StoredModePrefs>(&bytes)
    {
        inner.prefs = parsed;
    }
}

fn save_prefs(app: &AppHandle, prefs: &StoredModePrefs) {
    if let Some(path) = prefs_path(app) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(prefs) {
            let _ = std::fs::write(path, bytes);
        }
    }
}

fn capture_bounds(window: &Window) -> Option<SavedBounds> {
    let pos = window.outer_position().ok()?;
    let size = window.inner_size().ok()?;
    let maximized = window.is_maximized().unwrap_or(false);
    Some(SavedBounds {
        x: pos.x,
        y: pos.y,
        width: size.width,
        height: size.height,
        maximized,
    })
}

fn default_bounds_for(window: &Window, mode: WindowMode, mini_collapsed: bool, mini_h: f64) -> (LogicalPosition<f64>, LogicalSize<f64>) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let (work_x, work_y, work_w, work_h) = window
        .current_monitor()
        .ok()
        .flatten()
        .map(|m| {
            let pos = m.position();
            let size = m.size();
            (
                f64::from(pos.x) / scale,
                f64::from(pos.y) / scale,
                f64::from(size.width) / scale,
                f64::from(size.height) / scale,
            )
        })
        .unwrap_or((0.0, 0.0, 1440.0, 900.0));
    match mode {
        WindowMode::Efficiency => {
            let w = 1280.0_f64.min((work_w - 60.0).max(800.0));
            let h = 900.0_f64.min((work_h - 80.0).max(600.0));
            (
                LogicalPosition::new(work_x + (work_w - w) / 2.0, work_y + (work_h - h) / 2.0),
                LogicalSize::new(w, h),
            )
        }
        WindowMode::Classic => {
            let w = 356.0;
            let h = 760.0_f64.min((work_h - 90.0).max(520.0));
            (
                LogicalPosition::new((work_x + work_w - w - 18.0).max(work_x), work_y + 42.0),
                LogicalSize::new(w, h),
            )
        }
        WindowMode::Mini => {
            let w = if mini_collapsed { 286.0 } else { 312.0 };
            let h = if mini_collapsed { 52.0 } else { mini_h.clamp(160.0, 420.0) };
            (
                LogicalPosition::new((work_x + work_w - w - 18.0).max(work_x), work_y + 24.0),
                LogicalSize::new(w, h),
            )
        }
    }
}

fn apply_opacity_native(window: &Window, opacity: f64) -> Result<(), String> {
    let clamped = opacity.clamp(0.35, 1.0);
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{COLORREF, HWND};
        use windows::Win32::UI::WindowsAndMessaging::{
            GWL_EXSTYLE, GetWindowLongW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongW,
            WS_EX_LAYERED,
        };
        let raw = window.hwnd().map_err(|err| err.to_string())?;
        let hwnd = HWND(raw.0 as *mut _);
        unsafe {
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE);
            let layered = WS_EX_LAYERED.0 as i32;
            if (ex & layered) == 0 {
                SetWindowLongW(hwnd, GWL_EXSTYLE, ex | layered);
            }
            let alpha = (clamped * 255.0).round().clamp(90.0, 255.0) as u8;
            SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA)
                .map_err(|err| err.to_string())?;
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (window, clamped);
    }
    Ok(())
}

pub fn apply_window_mode(app: &AppHandle, next_mode: WindowMode) -> Result<WindowModeSnapshot, String> {
    let window = app
        .get_window("main")
        .ok_or_else(|| "找不到主窗口".to_owned())?;
    let state = app.state::<WindowModeState>();
    let (saved_target, prev_mode, snapshot, mini_collapsed, mini_expanded_height, listener) = {
        let mut inner = state.inner.lock().expect("window mode state");
        load_prefs(app, &mut inner);
        let prev = inner.prefs.mode;
        if prev != next_mode {
            if let Some(bounds) = capture_bounds(&window) {
                match prev {
                    WindowMode::Efficiency => inner.prefs.efficiency_bounds = Some(bounds),
                    WindowMode::Classic => inner.prefs.classic_bounds = Some(bounds),
                    WindowMode::Mini => {
                        if !inner.prefs.mini_collapsed {
                            let scale = window.scale_factor().unwrap_or(1.0);
                            inner.prefs.mini_expanded_height =
                                (f64::from(bounds.height) / scale).clamp(160.0, 420.0);
                        }
                        inner.prefs.mini_bounds = Some(bounds);
                    }
                }
            }
            inner.prefs.mode = next_mode;
            save_prefs(app, &inner.prefs);
        }
        let target = match next_mode {
            WindowMode::Efficiency => inner.prefs.efficiency_bounds,
            WindowMode::Classic => inner.prefs.classic_bounds,
            WindowMode::Mini => inner.prefs.mini_bounds,
        };
        (
            target,
            prev,
            inner.prefs.snapshot(),
            inner.prefs.mini_collapsed,
            inner.prefs.mini_expanded_height,
            inner.listener.clone(),
        )
    };

    if next_mode != WindowMode::Efficiency {
        crate::browser::hide_all(app);
    }

    if prev_mode != next_mode {
        if window.is_maximized().unwrap_or(false) {
            let _ = window.unmaximize();
        }
        let min_size = match next_mode {
            WindowMode::Efficiency => LogicalSize::new(640.0, 480.0),
            WindowMode::Classic => LogicalSize::new(280.0, 440.0),
            WindowMode::Mini => LogicalSize::new(220.0, 44.0),
        };
        let _ = window.set_min_size(Some(min_size));

        if let Some(bounds) = saved_target {
            if next_mode == WindowMode::Efficiency && bounds.maximized {
                let _ = window.maximize();
            } else {
                let scale = window.scale_factor().unwrap_or(1.0);
                let target_height = if next_mode == WindowMode::Mini {
                    if mini_collapsed {
                        (52.0 * scale).round() as u32
                    } else {
                        (mini_expanded_height * scale).round() as u32
                    }
                } else {
                    bounds.height
                };
                let _ = window.set_size(PhysicalSize::new(bounds.width.max(220), target_height.max(44)));
                let _ = window.set_position(PhysicalPosition::new(bounds.x, bounds.y));
            }
        } else {
            let (pos, size) = default_bounds_for(&window, next_mode, mini_collapsed, mini_expanded_height);
            let _ = window.set_size(size);
            let _ = window.set_position(pos);
        }
    }

    let _ = window.set_always_on_top(snapshot.always_on_top);
    let _ = apply_opacity_native(&window, snapshot.opacity);
    if let Some(channel) = listener {
        let _ = channel.send(snapshot.clone());
    }
    Ok(snapshot)
}

#[tauri::command]
pub fn window_mode_get(app: AppHandle) -> Result<WindowModeSnapshot, String> {
    let state = app.state::<WindowModeState>();
    let mut inner = state.inner.lock().expect("window mode state");
    load_prefs(&app, &mut inner);
    Ok(inner.prefs.snapshot())
}

#[tauri::command]
pub fn window_mode_set(app: AppHandle, mode: WindowMode) -> Result<WindowModeSnapshot, String> {
    apply_window_mode(&app, mode)
}

#[tauri::command]
pub fn window_mode_listen(
    app: AppHandle,
    webview: Webview,
    channel: Channel<WindowModeSnapshot>,
) -> Result<WindowModeSnapshot, String> {
    let state = app.state::<WindowModeState>();
    let mut inner = state.inner.lock().expect("window mode state");
    load_prefs(&app, &mut inner);
    if webview.label() == "main" {
        inner.listener = Some(channel);
    }
    Ok(inner.prefs.snapshot())
}

#[tauri::command]
pub fn window_always_on_top(
    app: AppHandle,
    window: Window,
    enabled: bool,
) -> Result<WindowModeSnapshot, String> {
    let state = app.state::<WindowModeState>();
    let snapshot = {
        let mut inner = state.inner.lock().expect("window mode state");
        load_prefs(&app, &mut inner);
        match inner.prefs.mode {
            WindowMode::Classic => inner.prefs.classic_always_on_top = enabled,
            WindowMode::Mini => inner.prefs.mini_always_on_top = enabled,
            WindowMode::Efficiency => {}
        }
        save_prefs(&app, &inner.prefs);
        inner.prefs.snapshot()
    };
    window
        .set_always_on_top(enabled)
        .map_err(|err| err.to_string())?;
    Ok(WindowModeSnapshot {
        always_on_top: enabled,
        ..snapshot
    })
}

#[tauri::command]
pub fn window_opacity(
    app: AppHandle,
    window: Window,
    opacity: f64,
    persist: Option<bool>,
) -> Result<WindowModeSnapshot, String> {
    let clamped = opacity.clamp(0.35, 1.0);
    apply_opacity_native(&window, clamped)?;
    let state = app.state::<WindowModeState>();
    let snapshot = {
        let mut inner = state.inner.lock().expect("window mode state");
        load_prefs(&app, &mut inner);
        if persist.unwrap_or(true) {
            match inner.prefs.mode {
                WindowMode::Classic => inner.prefs.classic_opacity = clamped,
                WindowMode::Mini => inner.prefs.mini_opacity = clamped,
                WindowMode::Efficiency => {}
            }
            save_prefs(&app, &inner.prefs);
        }
        inner.prefs.snapshot()
    };
    Ok(WindowModeSnapshot {
        opacity: clamped,
        ..snapshot
    })
}

#[tauri::command]
pub fn window_mini_resize(
    app: AppHandle,
    window: Window,
    collapsed: bool,
) -> Result<WindowModeSnapshot, String> {
    let state = app.state::<WindowModeState>();
    let (target_h, snapshot) = {
        let mut inner = state.inner.lock().expect("window mode state");
        load_prefs(&app, &mut inner);
        if inner.prefs.mode == WindowMode::Mini && !inner.prefs.mini_collapsed && collapsed {
            if let Ok(size) = window.inner_size() {
                let scale = window.scale_factor().unwrap_or(1.0);
                inner.prefs.mini_expanded_height =
                    (f64::from(size.height) / scale).clamp(160.0, 420.0);
            }
        }
        inner.prefs.mini_collapsed = collapsed;
        save_prefs(&app, &inner.prefs);
        let h = if collapsed {
            52.0
        } else {
            inner.prefs.mini_expanded_height
        };
        (h, inner.prefs.snapshot())
    };
    if snapshot.mode == WindowMode::Mini
        && let Ok(size) = window.inner_size()
    {
        let scale = window.scale_factor().unwrap_or(1.0);
        let width_logical = (f64::from(size.width) / scale).max(260.0);
        let _ = window.set_size(LogicalSize::new(width_logical, target_h));
    }
    Ok(snapshot)
}

pub fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[tauri::command]
pub async fn pick_folder(
    app: AppHandle,
    window: Window,
    remote: State<'_, Remote>,
) -> Result<Option<String>, String> {
    if remote.is_remote() {
        return Err("远程模式下不能选择本机目录".into());
    }
    let (send, receive) = tokio::sync::oneshot::channel();
    app.dialog()
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

#[cfg(test)]
mod tests {
    use super::{WindowAction, WindowMode};

    #[test]
    fn window_modes_roundtrip_camel_case() {
        for (raw, expected) in [
            ("\"efficiency\"", WindowMode::Efficiency),
            ("\"classic\"", WindowMode::Classic),
            ("\"mini\"", WindowMode::Mini),
        ] {
            let parsed: WindowMode = serde_json::from_str(raw).unwrap();
            assert_eq!(parsed, expected);
            assert_eq!(serde_json::to_string(&parsed).unwrap(), raw);
        }
    }

    #[test]
    fn window_actions_deserialize_from_frontend_names() {
        for name in [
            "drag",
            "minimize",
            "maximize",
            "close",
            "zoomIn",
            "zoomOut",
            "zoomReset",
            "reload",
            "devtools",
            "fullscreen",
            "quit",
        ] {
            let parsed: Result<WindowAction, _> = serde_json::from_str(&format!("\"{name}\""));
            assert!(parsed.is_ok(), "failed to parse window action {name}");
        }
    }

    #[test]
    fn desktop_commands_avoid_webview_window_so_child_webviews_never_break_controls() {
        // Once a built-in browser tab attaches via `window.add_child`, `Window::is_webview_window`
        // returns false; commands and tray restore must use `Window`/`Webview`/`get_window`.
        let src = include_str!("desktop.rs");
        let prod = src.split("#[cfg(test)]").next().unwrap_or(src);
        let code: String = prod
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains("WebviewWindow"),
            "desktop.rs must not take WebviewWindow (breaks when child browser webviews exist)"
        );
        assert!(
            !code.contains("get_webview_window"),
            "desktop.rs must use get_window instead of get_webview_window"
        );
    }
}
