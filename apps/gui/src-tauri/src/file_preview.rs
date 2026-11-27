use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use percent_encoding::percent_decode_str;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Manager, State,
    http::{Method, Request, Response, StatusCode},
};
use uuid::Uuid;

use crate::{
    desktop::{resolve_path, valid_scope},
    remote::{Remote, client_headers},
};

const LIMIT: usize = 16 * 1024 * 1024;
const ROOTS: usize = 128;

#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
enum RootSource {
    Local {
        local_root: PathBuf,
    },
    Workspace {
        scope: String,
        directory: String,
        workspace: String,
    },
}

#[derive(Clone, Deserialize, Serialize)]
struct Root {
    id: Uuid,
    #[serde(flatten)]
    source: RootSource,
}

impl Root {
    fn valid(&self) -> bool {
        match &self.source {
            RootSource::Local { local_root } => local_root.is_absolute() && local_root.is_dir(),
            RootSource::Workspace {
                scope, directory, ..
            } => {
                valid_scope(scope)
                    && (directory.is_empty() || directory.split('/').all(valid_segment))
            }
        }
    }
}

struct Store {
    roots: VecDeque<Root>,
    attached: HashMap<String, Uuid>,
}

pub struct Previews {
    file: PathBuf,
    store: Mutex<Store>,
}

impl Previews {
    pub fn new(app: &AppHandle) -> tauri::Result<Self> {
        let file = app.path().app_data_dir()?.join("html-previews.json");
        let roots: VecDeque<Root> = fs::read(&file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<Root>>(&bytes).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(Root::valid)
            .take(ROOTS)
            .collect();
        Ok(Self {
            file,
            store: Mutex::new(Store {
                roots,
                attached: HashMap::new(),
            }),
        })
    }

    fn remember(&self, root: Root) -> Result<(), String> {
        let mut entries = self.store.lock().expect("file previews");
        let mut roots = entries.roots.clone();
        let removed = if roots.len() >= ROOTS {
            roots.pop_front()
        } else {
            None
        };
        roots.push_back(root);
        let write = || -> Result<(), Box<dyn std::error::Error>> {
            let dir = self.file.parent().ok_or("html preview directory")?;
            fs::create_dir_all(dir)?;
            let mut file = tempfile::NamedTempFile::new_in(dir)?;
            serde_json::to_writer(&mut file, &roots)?;
            file.persist(&self.file)?;
            Ok(())
        };
        write().map_err(|err| format!("无法保存文件预览：{err}"))?;
        entries.roots = roots;
        if let Some(old) = removed {
            entries.attached.retain(|_, id| *id != old.id);
        }
        Ok(())
    }

    pub fn bind(&self, view: &str, url: &Url) -> Result<(), String> {
        let (id, _) = parse(url.path()).ok_or("无效的文件预览地址")?;
        let mut entries = self.store.lock().expect("file previews");
        if !entries.roots.iter().any(|root| root.id == id) {
            return Err("工作区文件预览已过期，请从文件面板重新打开".into());
        }
        entries.attached.insert(view.to_owned(), id);
        Ok(())
    }

    pub fn unbind(&self, view: &str) {
        self.store
            .lock()
            .expect("file previews")
            .attached
            .remove(view);
    }
}

pub fn is_preview(url: &Url) -> bool {
    url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(
            (url.scheme(), url.host_str()),
            ("http", Some("crossagents-preview.localhost"))
                | ("crossagents-preview", Some("localhost"))
        )
}

pub fn same_preview(left: &Url, right: &Url) -> bool {
    is_preview(left)
        && is_preview(right)
        && parse(left.path())
            .zip(parse(right.path()))
            .is_some_and(|((a, _), (b, _))| a == b)
}

fn parse(path: &str) -> Option<(Uuid, String)> {
    let (id, file) = path.strip_prefix('/')?.split_once('/')?;
    let id = Uuid::parse_str(id).ok()?;
    let file = file
        .split('/')
        .map(|part| {
            let part = percent_decode_str(part).decode_utf8().ok()?;
            valid_segment(&part).then(|| part.into_owned())
        })
        .collect::<Option<Vec<_>>>()?
        .join("/");
    Some((id, file))
}

fn valid_segment(part: &str) -> bool {
    !matches!(part, "" | "." | "..") && !part.contains(['/', '\\', ':', '\0'])
}

fn preview_url(id: Uuid, name: &str) -> Result<String, String> {
    let origin = if cfg!(windows) {
        "http://crossagents-preview.localhost/"
    } else {
        "crossagents-preview://localhost/"
    };
    let mut url = Url::parse(origin).map_err(|err| err.to_string())?;
    url.path_segments_mut()
        .map_err(|_| "无法构造文件预览地址")?
        .clear()
        .push(&id.to_string())
        .push(name);
    Ok(url.into())
}

#[tauri::command]
pub async fn browser_file_url(
    client: State<'_, Client>,
    remote: State<'_, Remote>,
    previews: State<'_, Previews>,
    scope: String,
    path: String,
) -> Result<String, String> {
    let workspace = remote.workspace_key().ok_or("工作区不可用")?;
    let hub = remote.hub().await?;
    let relative = resolve_path(&client, &remote, &hub, &scope, &path)
        .await?
        .path
        .ok_or("文件不存在或不在工作区内")?;
    if mime(&relative) != "text/html; charset=utf-8" {
        return Err("这里只预览 HTML 文件".into());
    }
    let (directory, name) = relative.rsplit_once('/').unwrap_or(("", &relative));
    let id = Uuid::new_v4();
    let url = preview_url(id, name)?;
    if remote.workspace_key().as_deref() != Some(&workspace) {
        return Err("连接已切换，请重新打开文件".into());
    }
    previews.remember(Root {
        id,
        source: RootSource::Workspace {
            scope,
            directory: directory.into(),
            workspace,
        },
    })?;
    Ok(url)
}

#[tauri::command]
pub fn browser_local_file_url(
    remote: State<'_, Remote>,
    previews: State<'_, Previews>,
    path: String,
) -> Result<String, String> {
    if remote.is_remote() {
        return Err("远程模式下不能直接打开本机绝对路径文件".into());
    }
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("没有文件路径".into());
    }
    let target = Path::new(trimmed);
    if !target.is_absolute() {
        return Err("需要文件的绝对路径".into());
    }
    let real = fs::canonicalize(target).map_err(|_| "文件不存在".to_owned())?;
    if !real.is_file() {
        return Err("不是一个文件".into());
    }
    let local_root = real.parent().ok_or("无法确定文件所在目录")?.to_path_buf();
    let name = real
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| valid_segment(n))
        .ok_or("文件名无效")?;
    let id = Uuid::new_v4();
    let url = preview_url(id, name)?;
    previews.remember(Root {
        id,
        source: RootSource::Local { local_root },
    })?;
    Ok(url)
}

pub async fn serve(app: AppHandle, view: String, request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let response = match read(&app, &view, request).await {
        Ok((content_type, body)) => Response::builder()
            .status(200)
            .header("content-type", content_type)
            .body(body),
        Err(status) => Response::builder().status(status).body(Vec::new()),
    };
    let mut response = response.expect("valid preview response");
    response
        .headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    response
        .headers_mut()
        .insert("referrer-policy", "no-referrer".parse().unwrap());
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response.headers_mut().insert("content-security-policy",
        "default-src 'self' data: blob:; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; connect-src 'none'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'".parse().unwrap());
    response
}

fn read_local(
    local_root: &Path,
    name: &str,
    head: bool,
) -> Result<(&'static str, Vec<u8>), StatusCode> {
    let real_root = fs::canonicalize(local_root).map_err(|_| StatusCode::FORBIDDEN)?;
    let candidate = real_root.join(name);
    let real = fs::canonicalize(&candidate).map_err(|_| StatusCode::NOT_FOUND)?;
    if !real.starts_with(&real_root) {
        return Err(StatusCode::FORBIDDEN);
    }
    let meta = fs::metadata(&real).map_err(|_| StatusCode::NOT_FOUND)?;
    if !meta.is_file() {
        return Err(StatusCode::NOT_FOUND);
    }
    if meta.len() > LIMIT as u64 {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let bytes = if head {
        Vec::new()
    } else {
        fs::read(&real).map_err(|_| StatusCode::NOT_FOUND)?
    };
    Ok((mime(name), bytes))
}

async fn read(
    app: &AppHandle,
    view: &str,
    request: Request<Vec<u8>>,
) -> Result<(&'static str, Vec<u8>), StatusCode> {
    let head = match *request.method() {
        Method::GET => false,
        Method::HEAD => true,
        _ => return Err(StatusCode::METHOD_NOT_ALLOWED),
    };
    let (id, name) = parse(request.uri().path()).ok_or(StatusCode::NOT_FOUND)?;
    let root = {
        let previews = app.state::<Previews>();
        let entries = previews.store.lock().expect("file previews");
        if entries.attached.get(view) != Some(&id) {
            return Err(StatusCode::FORBIDDEN);
        }
        entries
            .roots
            .iter()
            .find(|root| root.id == id)
            .cloned()
            .ok_or(StatusCode::NOT_FOUND)?
    };
    match root.source {
        RootSource::Local { local_root } => {
            tokio::task::spawn_blocking(move || read_local(&local_root, &name, head))
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        }
        RootSource::Workspace {
            scope,
            directory,
            workspace,
        } => {
            let remote = app.state::<Remote>();
            let hub = remote
                .hub()
                .await
                .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
            if remote.workspace_key().as_deref() != Some(&workspace) {
                return Err(StatusCode::GONE);
            }
            let mut url = hub
                .base
                .join(&format!("{scope}/files/raw"))
                .map_err(|_| StatusCode::NOT_FOUND)?;
            let file = if directory.is_empty() {
                name.clone()
            } else {
                format!("{directory}/{name}")
            };
            url.query_pairs_mut().append_pair("path", &file);
            let client = app.state::<Client>();
            let raw = client
                .request(request.method().clone(), url)
                .bearer_auth(&hub.token)
                .timeout(Duration::from_secs(30));
            let mut reply = client_headers(raw, &hub)
                .send()
                .await
                .map_err(|_| StatusCode::BAD_GATEWAY)?;
            if reply.status() == StatusCode::UNAUTHORIZED {
                remote.unauthorized(&hub);
            }
            if !reply.status().is_success() {
                return Err(reply.status());
            }
            if reply
                .content_length()
                .is_some_and(|length| length > LIMIT as u64)
            {
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = reply.chunk().await.map_err(|_| StatusCode::BAD_GATEWAY)? {
                if bytes.len() + chunk.len() > LIMIT {
                    return Err(StatusCode::PAYLOAD_TOO_LARGE);
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok((mime(&name), bytes))
        }
    }
}

fn mime(path: &str) -> &'static str {
    match path
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("txt" | "md") => "text/plain; charset=utf-8",
        Some("xml") => "application/xml; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("pdf") => "application/pdf",
        Some("wasm") => "application/wasm",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_file_preview_reads_inside_root_and_blocks_traversal() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("index.html"), "<h1>ok</h1>").unwrap();
        let (ct, body) = read_local(dir.path(), "index.html", false).unwrap();
        assert_eq!(ct, "text/html; charset=utf-8");
        assert_eq!(body, b"<h1>ok</h1>");
        let (_, empty) = read_local(dir.path(), "index.html", true).unwrap();
        assert!(empty.is_empty());
        assert_eq!(
            read_local(dir.path(), "missing.html", false).unwrap_err(),
            StatusCode::NOT_FOUND
        );
        assert!(parse("/not-a-uuid/index.html").is_none());
        let id = Uuid::new_v4();
        assert!(parse(&format!("/{id}/../secret.txt")).is_none());
    }

    #[test]
    fn root_source_deserializes_legacy_workspace_and_local_entries() {
        let id = Uuid::new_v4();
        let workspace_json = serde_json::json!({
            "id": id,
            "scope": "/v1/agents/Rex",
            "directory": "docs",
            "workspace": "local"
        });
        let parsed: Root = serde_json::from_value(workspace_json).unwrap();
        assert!(matches!(parsed.source, RootSource::Workspace { .. }));
        assert!(parsed.valid());

        let dir = tempfile::tempdir().unwrap();
        let local_json = serde_json::json!({
            "id": id,
            "local_root": dir.path()
        });
        let parsed: Root = serde_json::from_value(local_json).unwrap();
        assert!(matches!(parsed.source, RootSource::Local { .. }));
        assert!(parsed.valid());
    }
}
