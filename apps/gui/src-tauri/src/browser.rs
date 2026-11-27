use std::sync::Mutex;
use std::time::Duration;

use crate::file_preview::{Previews, is_preview, same_preview};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, Webview, WebviewUrl, Window,
    ipc::Channel,
    webview::{NewWindowResponse, PageLoadEvent, WebviewBuilder},
};
use tauri_plugin_opener::OpenerExt;

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Event {
    Started { url: String },
    Finished { url: String },
    Title { title: String },
    Open { url: String },
}

fn label(id: u32) -> String {
    format!("browser-{id}")
}

fn allowed(url: &Url) -> bool {
    url.as_str() == "about:blank"
        || (matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some_and(|host| {
                !matches!(
                    host.trim_end_matches('.'),
                    "tauri.localhost"
                        | "ipc.localhost"
                        | "crossagents-ext.localhost"
                        | "crossagents-preview.localhost"
                )
            })
            && url.username().is_empty()
            && url.password().is_none())
}

fn parse(url: &str) -> Result<Url, String> {
    Url::parse(url)
        .ok()
        .filter(|url| allowed(url) || is_preview(url))
        .ok_or_else(|| "浏览器仅能打开 HTTP 或 HTTPS 地址".into())
}

fn tab(app: &AppHandle, id: u32) -> Result<Webview, String> {
    app.get_webview(&label(id))
        .ok_or_else(|| "浏览器标签已关闭".into())
}

#[tauri::command]
pub async fn browser_create(
    app: AppHandle,
    window: Window,
    id: u32,
    url: String,
    events: Channel<Event>,
) -> Result<(), String> {
    let url = parse(&url)?;
    if window.label() != "main" {
        return Err("浏览器只能从 CrossAgents 主窗口打开".into());
    }
    if let Some(existing) = app.get_webview(&label(id)) {
        existing.close().map_err(|err| err.to_string())?;
    }
    let states = events.clone();
    let titles = events.clone();
    let view_label = label(id);
    let previews = app.state::<Previews>();
    previews.unbind(&view_label);
    let preview = is_preview(&url);
    if preview {
        previews.bind(&view_label, &url)?;
    }
    let initial = url.clone();
    let builder = WebviewBuilder::new(&view_label, WebviewUrl::External(url))
        .initialization_script(include_str!("browser_console.js"))
        .on_navigation(move |url| {
            if preview {
                same_preview(&initial, url)
            } else {
                allowed(url)
            }
        })
        .on_new_window(move |url, _| {
            if allowed(&url) {
                let _ = events.send(Event::Open { url: url.into() });
            }
            NewWindowResponse::Deny
        })
        .on_page_load(move |_, event| {
            let url = event.url().as_str().to_owned();
            let _ = states.send(match event.event() {
                PageLoadEvent::Started => Event::Started { url },
                PageLoadEvent::Finished => Event::Finished { url },
            });
        })
        .on_document_title_changed(move |_, title| {
            let _ = titles.send(Event::Title { title });
        });
    #[cfg(windows)]
    let builder = builder.data_directory(
        app.path()
            .app_data_dir()
            .map_err(|err| err.to_string())?
            .join("browser"),
    );
    #[cfg(target_os = "macos")]
    let builder = builder.data_store_identifier(*b"CrossAgentsWeb01");
    let webview = window
        .add_child(
            builder,
            LogicalPosition::new(-2000.0, -2000.0),
            LogicalSize::new(1024.0, 768.0),
        )
        .map_err(|err| err.to_string())?;
    webview.hide().map_err(|err| err.to_string())
}

#[derive(Deserialize)]
pub struct Bounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[tauri::command]
pub fn browser_bounds(app: AppHandle, id: u32, bounds: Option<Bounds>) -> Result<(), String> {
    let tab = tab(&app, id)?;
    let result = if let Some(bounds) = bounds {
        if ![bounds.x, bounds.y, bounds.width, bounds.height]
            .into_iter()
            .all(|value| value.is_finite() && (0.0..=16000.0).contains(&value))
        {
            return Err("浏览器布局无效".into());
        }
        tab.set_position(LogicalPosition::new(bounds.x, bounds.y))
            .and_then(|()| tab.set_size(LogicalSize::new(bounds.width, bounds.height)))
            .and_then(|()| tab.show())
    } else {
        tab.hide()
    };
    result.map_err(|err| err.to_string())
}

#[tauri::command]
pub fn browser_navigate(app: AppHandle, id: u32, url: String) -> Result<(), String> {
    let view = tab(&app, id)?;
    let next = parse(&url)?;
    let current = view.url().map_err(|err| err.to_string())?;
    if (is_preview(&next) || is_preview(&current)) && !same_preview(&current, &next) {
        return Err("请在新标签页打开其他地址".into());
    }
    view.navigate(next).map_err(|err| err.to_string())
}

#[tauri::command]
pub fn browser_step(app: AppHandle, id: u32, action: String) -> Result<(), String> {
    let script = match action.as_str() {
        "back" => "history.back()",
        "forward" => "history.forward()",
        "reload" => "location.reload()",
        "stop" => "window.stop()",
        _ => return Err("未知的浏览器操作".into()),
    };
    tab(&app, id)?.eval(script).map_err(|err| err.to_string())
}

#[tauri::command]
pub fn browser_style(app: AppHandle, id: u32, css: String) -> Result<(), String> {
    let css = serde_json::to_string(&css).map_err(|err| err.to_string())?;
    tab(&app, id)?.eval(format!("(() => {{ const style = document.createElement('style'); style.textContent = {css}; document.head?.append(style); }})()"))
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub fn browser_open_external(app: AppHandle, url: String) -> Result<Value, String> {
    let parsed = Url::parse(url.trim())
        .ok()
        .filter(|parsed| matches!(parsed.scheme(), "http" | "https") && allowed(parsed))
        .ok_or_else(|| "只允许在外部浏览器打开 http 或 https 链接".to_owned())?;
    app.opener()
        .open_url(parsed.as_str(), None::<&str>)
        .map_err(|err| err.to_string())?;
    Ok(serde_json::json!({ "ok": true }))
}

#[tauri::command]
pub fn browser_close(app: AppHandle, id: u32) -> Result<(), String> {
    app.state::<Previews>().unbind(&label(id));
    if let Some(tab) = app.get_webview(&label(id)) {
        tab.close().map_err(|err| err.to_string())?;
    }
    Ok(())
}

pub fn close_all(app: &AppHandle) {
    for (label, view) in app.webviews() {
        if label.starts_with("browser-") {
            app.state::<Previews>().unbind(&label);
            let _ = view.close();
        }
    }
}

pub fn hide_all(app: &AppHandle) {
    for (label, view) in app.webviews() {
        if label.starts_with("browser-") {
            let _ = view.hide();
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum Action {
    Snapshot {
        #[serde(default)]
        selector: Option<String>,
        #[serde(default)]
        max_chars: Option<u32>,
    },
    Console,
    Click {
        reference: u32,
    },
    Type {
        reference: u32,
        text: String,
        submit: bool,
        #[serde(default = "default_true")]
        clear: bool,
    },
    Select {
        reference: u32,
        value: String,
    },
    Press {
        key: String,
    },
    Scroll {
        dy: f64,
        #[serde(default)]
        reference: Option<u32>,
    },
    Probe {
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        text_gone: Option<String>,
        #[serde(default)]
        selector: Option<String>,
        #[serde(default)]
        url: Option<String>,
    },
}

#[tauri::command]
pub async fn browser_action(app: AppHandle, id: u32, action: Action) -> Result<Value, String> {
    let tab = tab(&app, id)?;
    if !tab
        .url()
        .as_ref()
        .is_ok_and(|url| allowed(url) || is_preview(url))
    {
        return Err("浏览器页面不可用".into());
    }
    let action = serde_json::to_string(&action).map_err(|err| err.to_string())?;
    let source = format!("({})({action})", include_str!("browser_page.js"));
    let (send, receive) = tokio::sync::oneshot::channel();
    let send = Mutex::new(Some(send));
    tab.eval_with_callback(source, move |raw| {
        if let Some(send) = send.lock().expect("browser evaluation").take() {
            let _ = send.send(raw);
        }
    })
    .map_err(|err| err.to_string())?;
    let raw = tokio::time::timeout(Duration::from_secs(8), receive)
        .await
        .map_err(|_| "浏览器页面没有回应".to_owned())?
        .map_err(|err| err.to_string())?;
    let result: Value =
        serde_json::from_str(&raw).map_err(|_| "浏览器返回了无效的结果".to_owned())?;
    if let Some(error) = result["error"].as_str() {
        return Err(error.to_owned());
    }
    if result.is_null() {
        return Err("浏览器页面没有返回结果".into());
    }
    Ok(result)
}

#[tauri::command]
pub async fn browser_screenshot(app: AppHandle, id: u32) -> Result<Value, String> {
    let tab = tab(&app, id)?;
    #[cfg(any(windows, target_os = "macos"))]
    {
        use base64::{Engine, engine::general_purpose::STANDARD};
        #[cfg(windows)]
        let bytes = capture_windows(tab).await?;
        #[cfg(target_os = "macos")]
        let bytes = capture_macos(tab).await?;
        return Ok(serde_json::json!({ "base64": STANDARD.encode(bytes), "mime": "image/png" }));
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = tab;
        Err("这个平台的浏览器截图还未接入".into())
    }
}

#[derive(Deserialize)]
struct BrowserSession {
    origin: String,
    cookies: Vec<SessionCookie>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionCookie {
    name: String,
    value: String,
    path: String,
    http_only: bool,
    same_site: String,
    expiration_date: Option<f64>,
}

#[tauri::command]
pub async fn browser_import_session(
    app: AppHandle,
    id: u32,
    file: String,
    key: String,
) -> Result<Value, String> {
    use tauri::webview::cookie::{Cookie, SameSite};
    let view = tab(&app, id)?;
    let url = view.url().map_err(|err| err.to_string())?;
    if url.scheme() != "https" || !allowed(&url) {
        return Err("请先在内置浏览器打开相同的 HTTPS 网站".into());
    }
    let data: BrowserSession = crate::transfer::open(
        &file,
        &key,
        "crossagents-browser-session-v1",
        2 * 1024 * 1024,
    )?;
    if url.origin().ascii_serialization() != data.origin || data.cookies.len() > 200 {
        return Err("迁移文件与当前网站不匹配".into());
    }
    let domain = url.host_str().ok_or("无效的网站地址")?.to_owned();
    let mut cookies = Vec::new();
    for cookie in data.cookies {
        if cookie.name.is_empty()
            || cookie.name.len() + cookie.value.len() > 4096
            || !cookie
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
            || !cookie
                .value
                .bytes()
                .all(|b| (0x21..=0x7e).contains(&b) && !b"\";,\\".contains(&b))
            || !cookie.path.starts_with('/')
            || cookie.path.contains([';', '\0', '\r', '\n'])
            || (cookie.name.starts_with("__Host-") && cookie.path != "/")
        {
            return Err("迁移文件含有无效的 Cookie".into());
        }
        let same_site = match cookie.same_site.as_str() {
            "strict" => SameSite::Strict,
            "lax" | "unspecified" => SameSite::Lax,
            "no_restriction" => SameSite::None,
            _ => return Err("不支持的 Cookie 设置".into()),
        };
        let mut cookie_builder = Cookie::build((cookie.name, cookie.value))
            .domain(domain.clone())
            .path(cookie.path)
            .secure(true)
            .http_only(cookie.http_only)
            .same_site(same_site);
        if let Some(expires) = cookie.expiration_date {
            let date = time::OffsetDateTime::from_unix_timestamp(expires as i64)
                .map_err(|_| "无效的过期时间")?;
            if date <= time::OffsetDateTime::now_utc() {
                continue;
            }
            cookie_builder = cookie_builder.expires(date);
        }
        cookies.push(cookie_builder.build());
    }
    if view
        .url()
        .as_ref()
        .is_ok_and(|current| current.origin() == url.origin())
    {
        let count = cookies.len();
        for cookie in cookies {
            view.set_cookie(cookie).map_err(|err| err.to_string())?;
        }
        view.eval("location.reload()")
            .map_err(|err| err.to_string())?;
        Ok(serde_json::json!({ "count": count }))
    } else {
        Err("网站已经切换，请重新打开后导入".into())
    }
}

#[cfg(target_os = "macos")]
async fn capture_macos(tab: Webview) -> Result<Vec<u8>, String> {
    use block2::RcBlock;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage};
    use objc2_foundation::{NSDictionary, NSError};
    use objc2_web_kit::WKWebView;
    use std::cell::RefCell;

    let (send, receive) = tokio::sync::oneshot::channel();
    tab.with_webview(move |view| {
        let send = RefCell::new(Some(send));
        let callback = RcBlock::new(move |image: *mut NSImage, _error: *mut NSError| {
            // WebKit calls back on the main thread. The image is borrowed for this callback.
            let png = unsafe { image.as_ref() }
                .and_then(|image| image.TIFFRepresentation())
                .and_then(|tiff| NSBitmapImageRep::imageRepWithData(&tiff))
                .and_then(|bitmap| unsafe {
                    bitmap.representationUsingType_properties(
                        NSBitmapImageFileType::PNG,
                        &NSDictionary::new(),
                    )
                })
                .map(|data| data.to_vec())
                .filter(|bytes| bytes.len() <= 32 * 1024 * 1024)
                .ok_or_else(|| "系统未能生成浏览器截图".to_owned());
            if let Some(send) = send.borrow_mut().take() {
                let _ = send.send(png);
            }
        });
        // Tauri exposes this WKWebView pointer only on the UI thread.
        let webview = unsafe { &*view.inner().cast::<WKWebView>() };
        unsafe { webview.takeSnapshotWithConfiguration_completionHandler(None, &callback) };
    })
    .map_err(|err| err.to_string())?;
    tokio::time::timeout(Duration::from_secs(8), receive)
        .await
        .map_err(|_| "浏览器截图超时".to_owned())?
        .map_err(|err| err.to_string())?
}

#[cfg(windows)]
async fn capture_windows(tab: Webview) -> Result<Vec<u8>, String> {
    use std::sync::Arc;
    use webview2_com::{
        CapturePreviewCompletedHandler,
        Microsoft::Web::WebView2::Win32::COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
    };
    use windows::Win32::System::Com::{
        IStream, STREAM_SEEK_SET, StructuredStorage::CreateStreamOnHGlobal,
    };

    fn read(stream: &IStream) -> windows::core::Result<Vec<u8>> {
        let mut output = Vec::new();
        let mut chunk = [0_u8; 16384];
        // The callback runs on the WebView2 apartment that owns the stream.
        unsafe { stream.Seek(0, STREAM_SEEK_SET, None)? };
        loop {
            let mut count = 0;
            unsafe {
                stream
                    .Read(
                        chunk.as_mut_ptr().cast(),
                        chunk.len() as u32,
                        Some(&mut count),
                    )
                    .ok()?
            };
            if count == 0 {
                break;
            }
            if output.len() + count as usize > 32 * 1024 * 1024 {
                return Err(windows::core::Error::from_hresult(
                    windows::Win32::Foundation::E_OUTOFMEMORY,
                ));
            }
            output.extend_from_slice(&chunk[..count as usize]);
        }
        Ok(output)
    }

    let (send, receive) = tokio::sync::oneshot::channel();
    let send = Arc::new(Mutex::new(Some(send)));
    tab.with_webview(move |webview| {
        let callback_send = send.clone();
        // Both COM objects are created and used on the WebView2 UI thread.
        let capture = unsafe {
            (|| -> windows::core::Result<()> {
                let browser = webview.controller().CoreWebView2()?;
                let stream = CreateStreamOnHGlobal(Default::default(), true)?;
                let callback_stream = stream.clone();
                let handler = CapturePreviewCompletedHandler::create(Box::new(move |status| {
                    let result = status
                        .and_then(|()| read(&callback_stream))
                        .map_err(|err| err.to_string());
                    if let Some(send) = callback_send.lock().expect("browser capture").take() {
                        let _ = send.send(result);
                    }
                    Ok(())
                }));
                browser.CapturePreview(
                    COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
                    &stream,
                    &handler,
                )
            })()
        };
        if let Err(error) = capture
            && let Some(send) = send.lock().expect("browser capture").take()
        {
            let _ = send.send(Err(error.to_string()));
        }
    })
    .map_err(|err| err.to_string())?;
    tokio::time::timeout(Duration::from_secs(8), receive)
        .await
        .map_err(|_| "浏览器截图超时".to_owned())?
        .map_err(|err| err.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_serializes_new_fields_and_defaults_clear_to_true() {
        let typed: Action = serde_json::from_value(serde_json::json!({
            "action": "type",
            "reference": 2,
            "text": "hello",
            "submit": true
        }))
        .unwrap();
        let json = serde_json::to_value(typed).unwrap();
        assert_eq!(json["action"], "type");
        assert_eq!(json["clear"], true);

        let selected: Action = serde_json::from_value(serde_json::json!({
            "action": "select",
            "reference": 3,
            "value": "pro"
        }))
        .unwrap();
        assert_eq!(serde_json::to_value(selected).unwrap()["action"], "select");

        let probe: Action = serde_json::from_value(serde_json::json!({
            "action": "probe",
            "text": "Done",
            "selector": "#app"
        }))
        .unwrap();
        let probe_json = serde_json::to_value(probe).unwrap();
        assert_eq!(probe_json["action"], "probe");
        assert_eq!(probe_json["text"], "Done");
        assert_eq!(probe_json["selector"], "#app");
    }

    #[test]
    fn parse_allows_http_https_blank_and_rejects_internal_hosts() {
        assert!(parse("https://example.com/a").is_ok());
        assert!(parse("about:blank").is_ok());
        assert!(parse("http://tauri.localhost/index.html").is_err());
        assert!(parse("file:///C:/Windows/win.ini").is_err());
    }
}
