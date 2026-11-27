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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DockEdge {
    #[default]
    None,
    Left,
    Right,
    Top,
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
    classic_edge_dock: bool,
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
    268.0
}

impl Default for StoredModePrefs {
    fn default() -> Self {
        Self {
            mode: WindowMode::Efficiency,
            efficiency_bounds: None,
            classic_bounds: None,
            mini_bounds: None,
            classic_always_on_top: false,
            classic_edge_dock: true,
            mini_always_on_top: true,
            classic_opacity: 1.0,
            mini_opacity: 0.94,
            mini_collapsed: false,
            mini_expanded_height: 268.0,
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
    pub edge_dock: bool,
    pub docked_edge: DockEdge,
    pub dock_hidden: bool,
}

impl StoredModePrefs {
    fn snapshot(&self, docked_edge: DockEdge, dock_hidden: bool) -> WindowModeSnapshot {
        match self.mode {
            WindowMode::Efficiency => WindowModeSnapshot {
                mode: WindowMode::Efficiency,
                always_on_top: false,
                opacity: 1.0,
                mini_collapsed: self.mini_collapsed,
                edge_dock: self.classic_edge_dock,
                docked_edge: DockEdge::None,
                dock_hidden: false,
            },
            WindowMode::Classic => WindowModeSnapshot {
                mode: WindowMode::Classic,
                always_on_top: self.classic_always_on_top,
                opacity: self.classic_opacity,
                mini_collapsed: self.mini_collapsed,
                edge_dock: self.classic_edge_dock,
                docked_edge,
                dock_hidden,
            },
            WindowMode::Mini => WindowModeSnapshot {
                mode: WindowMode::Mini,
                always_on_top: self.mini_always_on_top,
                opacity: self.mini_opacity,
                mini_collapsed: self.mini_collapsed,
                edge_dock: self.classic_edge_dock,
                docked_edge: DockEdge::None,
                dock_hidden: false,
            },
        }
    }
}

#[derive(Default)]
pub struct WindowModeState {
    inner: Mutex<WindowModeInner>,
    wake: std::sync::Arc<tokio::sync::Notify>,
}

#[derive(Default)]
struct WindowModeInner {
    loaded: bool,
    prefs: StoredModePrefs,
    listener: Option<Channel<WindowModeSnapshot>>,
    docked_edge: DockEdge,
    dock_hidden: bool,
    dock_hold: bool,
    /// Last expanded (visible) physical coordinates when tucked into an edge.
    dock_visible_pos: Option<(i32, i32)>,
}

impl WindowModeInner {
    fn snapshot(&self) -> WindowModeSnapshot {
        self.prefs.snapshot(self.docked_edge, self.dock_hidden)
    }
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
            let w = 368.0;
            let h = 780.0_f64.min((work_h - 80.0).max(520.0));
            (
                LogicalPosition::new((work_x + work_w - w - 16.0).max(work_x), work_y + 38.0),
                LogicalSize::new(w, h),
            )
        }
        WindowMode::Mini => {
            let w = if mini_collapsed { 300.0 } else { 332.0 };
            let h = if mini_collapsed { 46.0 } else { mini_h.clamp(160.0, 440.0) };
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PxRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl PxRect {
    fn width(self) -> i32 {
        (self.right - self.left).max(1)
    }

    fn height(self) -> i32 {
        (self.bottom - self.top).max(1)
    }

    fn contains(self, x: i32, y: i32, pad: i32) -> bool {
        x >= self.left - pad
            && x <= self.right + pad
            && y >= self.top - pad
            && y <= self.bottom + pad
    }
}

/// Detects whether a window rect is close enough to a monitor work-area edge to snap-dock.
/// Bottom edge is intentionally omitted so Windows taskbar stays unobstructed (classic QQ rule).
fn compute_snap_edge(win: PxRect, work: PxRect, threshold: i32) -> DockEdge {
    if win.left <= work.left + threshold {
        DockEdge::Left
    } else if win.right >= work.right - threshold {
        DockEdge::Right
    } else if win.top <= work.top + threshold {
        DockEdge::Top
    } else {
        DockEdge::None
    }
}

/// Computes the `(expanded_pos, hidden_pos)` physical top-left coordinates for a docked Classic
/// window, leaving `peek_px` visible along the docked screen edge as an interactive handle.
fn compute_dock_positions(
    win: PxRect,
    work: PxRect,
    edge: DockEdge,
    peek_px: i32,
) -> ((i32, i32), (i32, i32)) {
    let w = win.width();
    let h = win.height();
    let peek = peek_px.max(4);
    let clamp_y = win.top.clamp(work.top, (work.bottom - h).max(work.top));
    let clamp_x = win.left.clamp(work.left, (work.right - w).max(work.left));
    match edge {
        DockEdge::Left => (
            (work.left, clamp_y),
            (work.left - w + peek, clamp_y),
        ),
        DockEdge::Right => (
            (work.right - w, clamp_y),
            (work.right - peek, clamp_y),
        ),
        DockEdge::Top => (
            (clamp_x, work.top),
            (clamp_x, work.top - h + peek),
        ),
        DockEdge::None => ((win.left, win.top), (win.left, win.top)),
    }
}

#[cfg(windows)]
fn monitor_work_area(window: &Window) -> Option<PxRect> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    let raw = window.hwnd().ok()?;
    let hwnd = HWND(raw.0 as *mut _);
    unsafe {
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(hmon, &mut info).as_bool() {
            let r = info.rcWork;
            Some(PxRect {
                left: r.left,
                top: r.top,
                right: r.right,
                bottom: r.bottom,
            })
        } else {
            None
        }
    }
}

#[cfg(not(windows))]
fn monitor_work_area(window: &Window) -> Option<PxRect> {
    let m = window.current_monitor().ok()??;
    let pos = m.position();
    let size = m.size();
    Some(PxRect {
        left: pos.x,
        top: pos.y,
        right: pos.x + size.width as i32,
        bottom: pos.y + size.height as i32,
    })
}

#[cfg(windows)]
fn window_rect_px(window: &Window) -> Option<PxRect> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    let raw = window.hwnd().ok()?;
    let hwnd = HWND(raw.0 as *mut _);
    let mut r = RECT::default();
    unsafe {
        GetWindowRect(hwnd, &mut r).ok()?;
    }
    Some(PxRect {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    })
}

#[cfg(not(windows))]
fn window_rect_px(window: &Window) -> Option<PxRect> {
    let pos = window.outer_position().ok()?;
    let size = window.outer_size().ok()?;
    Some(PxRect {
        left: pos.x,
        top: pos.y,
        right: pos.x + size.width as i32,
        bottom: pos.y + size.height as i32,
    })
}

#[cfg(windows)]
fn cursor_pos_px() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut pt = POINT::default();
    unsafe {
        GetCursorPos(&mut pt).ok()?;
    }
    Some((pt.x, pt.y))
}

#[cfg(windows)]
fn left_mouse_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 }
}

#[cfg(windows)]
fn set_window_pos_no_activate(window: &Window, x: i32, y: i32, topmost: Option<bool>) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER,
        SetWindowPos,
    };
    let Ok(raw) = window.hwnd() else {
        return;
    };
    let hwnd = HWND(raw.0 as *mut _);
    let (insert_after, flags) = match topmost {
        Some(true) => (
            Some(HWND_TOPMOST),
            SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        ),
        Some(false) => (
            Some(HWND_NOTOPMOST),
            SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        ),
        None => (
            None,
            SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER,
        ),
    };
    unsafe {
        let _ = SetWindowPos(hwnd, insert_after, x, y, 0, 0, flags);
    }
}

#[cfg(not(windows))]
fn set_window_pos_no_activate(window: &Window, x: i32, y: i32, topmost: Option<bool>) {
    let _ = window.set_position(PhysicalPosition::new(x, y));
    if let Some(top) = topmost {
        let _ = window.set_always_on_top(top);
    }
}

/// Slides the window smoothly from `(from_x, from_y)` to `(to_x, to_y)` using a 60fps cubic
/// ease-out curve without stealing focus from games or other active applications.
async fn slide_window_no_activate(
    window: &Window,
    from: (i32, i32),
    to: (i32, i32),
    topmost: Option<bool>,
) {
    if (from.0 - to.0).abs() <= 2 && (from.1 - to.1).abs() <= 2 {
        set_window_pos_no_activate(window, to.0, to.1, topmost);
        return;
    }
    const STEPS: usize = 11;
    for step in 1..=STEPS {
        let t = step as f64 / STEPS as f64;
        // Cubic ease-out: quick departure, gentle landing against the screen edge.
        let eased = 1.0 - (1.0 - t).powi(3);
        let x = from.0 + ((to.0 - from.0) as f64 * eased).round() as i32;
        let y = from.1 + ((to.1 - from.1) as f64 * eased).round() as i32;
        let z = if step == 1 { topmost } else { None };
        set_window_pos_no_activate(window, x, y, z);
        if step < STEPS {
            tokio::time::sleep(Duration::from_millis(14)).await;
        }
    }
}

fn notify_snapshot(app: &AppHandle) {
    let state = app.state::<WindowModeState>();
    let (snapshot, listener) = {
        let inner = state.inner.lock().expect("window mode state");
        (inner.snapshot(), inner.listener.clone())
    };
    if let Some(channel) = listener {
        let _ = channel.send(snapshot);
    }
}

/// Immediately restores a tucked-in Classic window back onto the visible work area.
fn reveal_docked_sync(app: &AppHandle, clear_dock: bool) {
    let Some(window) = app.get_window("main") else {
        return;
    };
    let state = app.state::<WindowModeState>();
    let (restore_pos, restore_topmost, changed) = {
        let mut inner = state.inner.lock().expect("window mode state");
        let was_hidden = inner.dock_hidden;
        let was_docked = inner.docked_edge != DockEdge::None;
        let pos = inner.dock_visible_pos;
        if clear_dock {
            inner.docked_edge = DockEdge::None;
            inner.dock_hidden = false;
            inner.dock_visible_pos = None;
        } else if was_hidden {
            inner.dock_hidden = false;
        }
        let topmost = if clear_dock {
            Some(inner.snapshot().always_on_top)
        } else {
            None
        };
        (
            if was_hidden { pos } else { None },
            topmost,
            was_hidden || (clear_dock && was_docked),
        )
    };
    if let Some((x, y)) = restore_pos {
        set_window_pos_no_activate(&window, x, y, restore_topmost);
    } else if let Some(top) = restore_topmost {
        let _ = window.set_always_on_top(top);
    }
    if changed {
        notify_snapshot(app);
    }
}

/// Spawns the background watcher that powers classic QQ-style screen-edge snapping and smooth
/// auto-hide drawer reveal/collapse. Sleeps completely idle unless Classic mode + edge-dock
/// are both active.
pub fn start_edge_dock_watcher(app: &AppHandle) {
    #[cfg(not(windows))]
    {
        let _ = app;
        return;
    }
    #[cfg(windows)]
    {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            use std::time::Instant;
            let wake = app.state::<WindowModeState>().wake.clone();
            let mut was_dragging = false;
            let mut hover_reveal_since: Option<Instant> = None;
            let mut outside_hide_since: Option<Instant> = None;

            loop {
                let (active, docked_edge, dock_hidden, dock_hold, pref_topmost) = {
                    let state = app.state::<WindowModeState>();
                    let inner = state.inner.lock().expect("window mode state");
                    (
                        inner.prefs.mode == WindowMode::Classic && inner.prefs.classic_edge_dock,
                        inner.docked_edge,
                        inner.dock_hidden,
                        inner.dock_hold,
                        inner.prefs.classic_always_on_top,
                    )
                };

                if !active {
                    was_dragging = false;
                    hover_reveal_since = None;
                    outside_hide_since = None;
                    wake.notified().await;
                    continue;
                }

                tokio::time::sleep(Duration::from_millis(36)).await;

                let Some(window) = app.get_window("main") else {
                    continue;
                };
                if !window.is_visible().unwrap_or(false) || window.is_minimized().unwrap_or(false) {
                    hover_reveal_since = None;
                    outside_hide_since = None;
                    continue;
                }
                let (Some(win_rect), Some(work), Some((cx, cy))) =
                    (window_rect_px(&window), monitor_work_area(&window), cursor_pos_px())
                else {
                    continue;
                };
                let scale = window.scale_factor().unwrap_or(1.0);
                let snap_px = (16.0 * scale).round() as i32;
                let detach_px = (26.0 * scale).round() as i32;
                let peek_px = (6.0 * scale).round() as i32;
                let lbtn = left_mouse_down();

                // While the person is dragging, never yank the window away.
                if lbtn {
                    if win_rect.contains(cx, cy, 24) {
                        was_dragging = true;
                        hover_reveal_since = None;
                        outside_hide_since = None;
                    }
                    continue;
                }

                // Drag just finished (or initial Classic placement): check whether we snapped to
                // a screen edge or pulled away into open space.
                if was_dragging || docked_edge == DockEdge::None {
                    was_dragging = false;
                    let snapped = compute_snap_edge(win_rect, work, snap_px);
                    if snapped != DockEdge::None && !dock_hidden {
                        let (expanded, _) = compute_dock_positions(win_rect, work, snapped, peek_px);
                        if (win_rect.left - expanded.0).abs() > 1
                            || (win_rect.top - expanded.1).abs() > 1
                        {
                            slide_window_no_activate(
                                &window,
                                (win_rect.left, win_rect.top),
                                expanded,
                                Some(true),
                            )
                            .await;
                        } else {
                            set_window_pos_no_activate(&window, expanded.0, expanded.1, Some(true));
                        }
                        let changed = {
                            let state = app.state::<WindowModeState>();
                            let mut inner = state.inner.lock().expect("window mode state");
                            let diff = inner.docked_edge != snapped || inner.dock_hidden;
                            inner.docked_edge = snapped;
                            inner.dock_hidden = false;
                            inner.dock_visible_pos = Some(expanded);
                            diff
                        };
                        if changed {
                            notify_snapshot(&app);
                        }
                        outside_hide_since = None;
                        hover_reveal_since = None;
                        continue;
                    } else if docked_edge != DockEdge::None
                        && !dock_hidden
                        && compute_snap_edge(win_rect, work, detach_px) == DockEdge::None
                    {
                        // User dragged it back away from the screen edge -> undock cleanly.
                        {
                            let state = app.state::<WindowModeState>();
                            let mut inner = state.inner.lock().expect("window mode state");
                            inner.docked_edge = DockEdge::None;
                            inner.dock_hidden = false;
                            inner.dock_visible_pos = None;
                        }
                        let _ = window.set_always_on_top(pref_topmost);
                        notify_snapshot(&app);
                        outside_hide_since = None;
                        hover_reveal_since = None;
                        continue;
                    }
                }

                if docked_edge == DockEdge::None {
                    continue;
                }

                if dock_hidden {
                    outside_hide_since = None;
                    // Check if cursor is resting on the exposed 6px edge peek strip.
                    let on_strip = win_rect.contains(cx, cy, 4);
                    if on_strip {
                        let start = *hover_reveal_since.get_or_insert_with(Instant::now);
                        // 160ms anti-accidental-brush hover dwell before sliding out.
                        if start.elapsed() >= Duration::from_millis(160) {
                            let (expanded, _) =
                                compute_dock_positions(win_rect, work, docked_edge, peek_px);
                            {
                                let state = app.state::<WindowModeState>();
                                let mut inner = state.inner.lock().expect("window mode state");
                                inner.dock_hidden = false;
                                inner.dock_visible_pos = Some(expanded);
                            }
                            notify_snapshot(&app);
                            slide_window_no_activate(
                                &window,
                                (win_rect.left, win_rect.top),
                                expanded,
                                Some(true),
                            )
                            .await;
                            hover_reveal_since = None;
                        }
                    } else {
                        hover_reveal_since = None;
                    }
                } else {
                    hover_reveal_since = None;
                    // Keep expanded if cursor is inside window or user is actively typing.
                    let inside = win_rect.contains(cx, cy, 8);
                    if inside || dock_hold {
                        outside_hide_since = None;
                    } else {
                        let start = *outside_hide_since.get_or_insert_with(Instant::now);
                        // 360ms grace delay before smoothly gliding back into the screen edge.
                        if start.elapsed() >= Duration::from_millis(360) {
                            let (expanded, hidden) =
                                compute_dock_positions(win_rect, work, docked_edge, peek_px);
                            {
                                let state = app.state::<WindowModeState>();
                                let mut inner = state.inner.lock().expect("window mode state");
                                inner.dock_visible_pos = Some(expanded);
                                inner.dock_hidden = true;
                            }
                            notify_snapshot(&app);
                            slide_window_no_activate(
                                &window,
                                (win_rect.left, win_rect.top),
                                hidden,
                                Some(true),
                            )
                            .await;
                            outside_hide_since = None;
                        }
                    }
                }
            }
        });
    }
}

pub fn apply_window_mode(app: &AppHandle, next_mode: WindowMode) -> Result<WindowModeSnapshot, String> {
    let window = app
        .get_window("main")
        .ok_or_else(|| "找不到主窗口".to_owned())?;
    // If Classic was tucked into a screen edge, bring it back before saving bounds or switching.
    reveal_docked_sync(app, true);

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
                                (f64::from(bounds.height) / scale).clamp(160.0, 440.0);
                        }
                        inner.prefs.mini_bounds = Some(bounds);
                    }
                }
            }
            inner.prefs.mode = next_mode;
            inner.docked_edge = DockEdge::None;
            inner.dock_hidden = false;
            inner.dock_hold = false;
            inner.dock_visible_pos = None;
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
            inner.snapshot(),
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
            WindowMode::Classic => LogicalSize::new(300.0, 460.0),
            WindowMode::Mini => LogicalSize::new(240.0, 44.0),
        };
        let _ = window.set_min_size(Some(min_size));

        if let Some(bounds) = saved_target {
            if next_mode == WindowMode::Efficiency && bounds.maximized {
                let _ = window.maximize();
            } else {
                let scale = window.scale_factor().unwrap_or(1.0);
                let target_height = if next_mode == WindowMode::Mini {
                    if mini_collapsed {
                        (46.0 * scale).round() as u32
                    } else {
                        (mini_expanded_height * scale).round() as u32
                    }
                } else {
                    bounds.height
                };
                let _ = window.set_size(PhysicalSize::new(bounds.width.max(240), target_height.max(44)));
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
    state.wake.notify_one();
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
    Ok(inner.snapshot())
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
    state.wake.notify_one();
    Ok(inner.snapshot())
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
        inner.snapshot()
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
        inner.snapshot()
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
                    (f64::from(size.height) / scale).clamp(160.0, 440.0);
            }
        }
        inner.prefs.mini_collapsed = collapsed;
        save_prefs(&app, &inner.prefs);
        let h = if collapsed {
            46.0
        } else {
            inner.prefs.mini_expanded_height
        };
        (h, inner.snapshot())
    };
    if snapshot.mode == WindowMode::Mini
        && let Ok(size) = window.inner_size()
    {
        let scale = window.scale_factor().unwrap_or(1.0);
        let width_logical = (f64::from(size.width) / scale).max(280.0);
        let _ = window.set_size(LogicalSize::new(width_logical, target_h));
    }
    Ok(snapshot)
}

#[tauri::command]
pub fn window_edge_dock(app: AppHandle, enabled: bool) -> Result<WindowModeSnapshot, String> {
    if !enabled {
        reveal_docked_sync(&app, true);
    }
    let state = app.state::<WindowModeState>();
    let snapshot = {
        let mut inner = state.inner.lock().expect("window mode state");
        load_prefs(&app, &mut inner);
        inner.prefs.classic_edge_dock = enabled;
        save_prefs(&app, &inner.prefs);
        inner.snapshot()
    };
    state.wake.notify_one();
    Ok(snapshot)
}

#[tauri::command]
pub fn window_dock_hold(app: AppHandle, held: bool) -> Result<(), String> {
    let state = app.state::<WindowModeState>();
    let mut inner = state.inner.lock().expect("window mode state");
    inner.dock_hold = held;
    Ok(())
}

#[tauri::command]
pub async fn window_dock_now(
    app: AppHandle,
    window: Window,
    edge: Option<DockEdge>,
) -> Result<WindowModeSnapshot, String> {
    let target_edge = edge.unwrap_or(DockEdge::Right);
    let Some(win_rect) = window_rect_px(&window) else {
        return window_mode_get(app);
    };
    let Some(work) = monitor_work_area(&window) else {
        return window_mode_get(app);
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let peek_px = (6.0 * scale).round() as i32;
    if target_edge == DockEdge::None {
        reveal_docked_sync(&app, true);
        return window_mode_get(app);
    }
    let (expanded, hidden) = compute_dock_positions(win_rect, work, target_edge, peek_px);
    let state = app.state::<WindowModeState>();
    let snapshot = {
        let mut inner = state.inner.lock().expect("window mode state");
        load_prefs(&app, &mut inner);
        inner.prefs.classic_edge_dock = true;
        inner.docked_edge = target_edge;
        inner.dock_hidden = true;
        inner.dock_visible_pos = Some(expanded);
        save_prefs(&app, &inner.prefs);
        inner.snapshot()
    };
    notify_snapshot(&app);
    slide_window_no_activate(&window, (win_rect.left, win_rect.top), hidden, Some(true)).await;
    state.wake.notify_one();
    Ok(snapshot)
}

pub fn show_window(app: &AppHandle) {
    reveal_docked_sync(app, false);
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
    use super::{
        DockEdge, PxRect, WindowAction, WindowMode, compute_dock_positions, compute_snap_edge,
    };

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
    fn dock_edge_snap_and_drawer_positions() {
        let work = PxRect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        let near_right = PxRect {
            left: 1550,
            top: 80,
            right: 1912,
            bottom: 840,
        };
        assert_eq!(compute_snap_edge(near_right, work, 16), DockEdge::Right);
        let (exp, hid) = compute_dock_positions(near_right, work, DockEdge::Right, 6);
        assert_eq!(exp, (1920 - 362, 80));
        assert_eq!(hid, (1920 - 6, 80));

        let near_left = PxRect {
            left: 10,
            top: 100,
            right: 370,
            bottom: 860,
        };
        assert_eq!(compute_snap_edge(near_left, work, 16), DockEdge::Left);
        let (exp_l, hid_l) = compute_dock_positions(near_left, work, DockEdge::Left, 6);
        assert_eq!(exp_l, (0, 100));
        assert_eq!(hid_l, (-360 + 6, 100));

        let near_top = PxRect {
            left: 700,
            top: 8,
            right: 1060,
            bottom: 768,
        };
        assert_eq!(compute_snap_edge(near_top, work, 16), DockEdge::Top);
        let (exp_t, hid_t) = compute_dock_positions(near_top, work, DockEdge::Top, 6);
        assert_eq!(exp_t, (700, 0));
        assert_eq!(hid_t, (700, -760 + 6));

        let center = PxRect {
            left: 600,
            top: 120,
            right: 960,
            bottom: 880,
        };
        assert_eq!(compute_snap_edge(center, work, 16), DockEdge::None);
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
