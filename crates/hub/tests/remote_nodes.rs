//! End-to-end integration test for cross-platform remote node control:
//! - Issuing a `DeviceKind::Node` token
//! - Enforcing that node tokens are rejected on normal routes (`403`) and accepted on `/v1/node/*`
//! - Connecting a remote node over SSE (`GET /v1/node/stream`)
//! - Listing online nodes (`GET /v1/nodes`)
//! - Executing remote commands & file operations (`exec`, `write`, `read`, `edit`, `list`, `search`)
//! - Revoking the node device and verifying it disconnects cleanly.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use crossagents_api_types::{
    Agent, CreateAgentRequest, CreateDeviceRequest, CreateDeviceResponse, DeviceKind,
    ListNodesResponse, ListPluginsResponse, NodeEditFileRequest, NodeEditFileResponse,
    NodeExecRequest, NodeExecResponse, NodeListFilesRequest, NodeListFilesResponse,
    NodeReadFileRequest, NodeReadFileResponse, NodeRpcCall, NodeRpcOp, NodeRpcReply,
    NodeSearchFilesRequest, NodeSearchFilesResponse, NodeWriteFileRequest, NodeWriteFileResponse,
    SendMessageRequest, Transcript,
};
use crossagents_core::{
    Capabilities, CoreError, Driver, DriverKind, DriverSession, OutputStream, Prompt,
    SessionEvent, SessionEventKind, SessionSpec, StopReason, TurnOutcome,
};
use crossagents_hub::{HubConfig, Paths};
use crossagents_node::{
    DriverSource, NodeService, PluginSnapshot, RegisterInfo, Transport, TransportError,
    TransportServer,
};
use futures::StreamExt;
use serde_json::json;
use tokio::sync::{Mutex, mpsc, watch};

#[tokio::test]
async fn remote_node_channel_exec_and_file_ops_end_to_end() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let cfg = HubConfig::new(Paths::new(home.path().to_path_buf()), 0);
    let hub = crossagents_hub::start(cfg).await.unwrap();
    let base = format!("http://{}", hub.addr);
    let http = reqwest::Client::builder().no_proxy().build().unwrap();

    // 1. Issue a node device credential.
    let created: CreateDeviceResponse = http
        .post(format!("{base}/v1/devices"))
        .bearer_auth(&hub.token)
        .json(&CreateDeviceRequest {
            name: "linux-bot".into(),
            kind: Some(DeviceKind::Node),
            level: None,
            os: None,
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let node_token = created.token;
    let device_id = created.device.id;

    // 2. Node token must still be refused (403) on ordinary API routes.
    let denied = http
        .get(format!("{base}/v1/agents"))
        .bearer_auth(&node_token)
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status().as_u16(), 403);

    // 3. Connect a simulated remote node over SSE (`/v1/node/stream`).
    let node_cwd = work.path().to_path_buf();
    let stream_url = format!(
        "{base}/v1/node/stream?os=linux&arch=x86_64&hostname=bot-vps&shell=bash&version=0.3.0"
    );
    let resp = http
        .get(stream_url)
        .bearer_auth(&node_token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let node_task = {
        let http = http.clone();
        let base = base.clone();
        let token = node_token.clone();
        let cwd = node_cwd.clone();
        tokio::spawn(async move {
            let mut stream = resp.bytes_stream();
            let mut buf = String::new();
            let mut event = String::new();
            while let Some(Ok(bytes)) = stream.next().await {
                buf.push_str(&String::from_utf8_lossy(&bytes));
                while let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim_end_matches('\r').to_owned();
                    buf.drain(..=pos);
                    if line.is_empty() {
                        event.clear();
                        continue;
                    }
                    if let Some(ev) = line.strip_prefix("event:") {
                        event = ev.trim().to_owned();
                        continue;
                    }
                    if let Some(data) = line.strip_prefix("data:")
                        && event == "call"
                    {
                        let call: NodeRpcCall = serde_json::from_str(data.trim()).unwrap();
                        let reply = handle_mock_node_op(&cwd, call.op);
                        let _ = http
                            .post(format!("{base}/v1/node/results/{}", call.call_id))
                            .bearer_auth(&token)
                            .json(&reply)
                            .send()
                            .await;
                    }
                }
            }
        })
    };

    // Wait briefly for the node registration to appear in `GET /v1/nodes`.
    let mut found = false;
    for _ in 0..30 {
        let list: ListNodesResponse = http
            .get(format!("{base}/v1/nodes"))
            .bearer_auth(&hub.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if list.nodes.iter().any(|n| n.name == "linux-bot" && !n.local) {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(found, "remote node did not register in GET /v1/nodes");

    // 4. Write -> Read -> Edit -> List -> Search -> Exec on remote node.
    let written: NodeWriteFileResponse = http
        .post(format!("{base}/v1/nodes/linux-bot/files/write"))
        .bearer_auth(&hub.token)
        .json(&NodeWriteFileRequest {
            path: "src/bot.py".into(),
            content: "print('hello old')\n".into(),
            create_dirs: Some(true),
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(written.bytes, 19);

    let edited: NodeEditFileResponse = http
        .post(format!("{base}/v1/nodes/linux-bot/files/edit"))
        .bearer_auth(&hub.token)
        .json(&NodeEditFileRequest {
            path: "src/bot.py".into(),
            target: "hello old".into(),
            replacement: "hello v0.3.0".into(),
            replace_all: Some(false),
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(edited.replacements, 1);

    let read: NodeReadFileResponse = http
        .post(format!("{base}/v1/nodes/linux-bot/files/read"))
        .bearer_auth(&hub.token)
        .json(&NodeReadFileRequest {
            path: "src/bot.py".into(),
            start_line: Some(1),
            end_line: Some(5),
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(read.content, "print('hello v0.3.0')");

    let listed: NodeListFilesResponse = http
        .post(format!("{base}/v1/nodes/linux-bot/files/list"))
        .bearer_auth(&hub.token)
        .json(&NodeListFilesRequest {
            path: "src".into(),
            max_entries: Some(50),
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.entries.len(), 1);
    assert_eq!(listed.entries[0].name, "bot.py");

    let searched: NodeSearchFilesResponse = http
        .post(format!("{base}/v1/nodes/linux-bot/files/search"))
        .bearer_auth(&hub.token)
        .json(&NodeSearchFilesRequest {
            path: "".into(),
            pattern: "v0.3.0".into(),
            max_matches: Some(20),
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(searched.matches.len(), 1);
    assert_eq!(searched.matches[0].line, 1);

    let exec: NodeExecResponse = http
        .post(format!("{base}/v1/nodes/linux-bot/exec"))
        .bearer_auth(&hub.token)
        .json(&NodeExecRequest {
            command: "echo ok".into(),
            cwd: None,
            timeout_secs: Some(10),
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(exec.exit_code, Some(0));
    assert!(exec.stdout.contains("ok"));

    // 5. Revoking the device drops the remote node connection.
    http.delete(format!("{base}/v1/devices/{device_id}"))
        .bearer_auth(&hub.token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), node_task).await;

    hub.shutdown();
    let _ = hub.wait().await;
}

fn handle_mock_node_op(root: &std::path::Path, op: NodeRpcOp) -> NodeRpcReply {
    match op {
        NodeRpcOp::WriteFile(req) => {
            let p = root.join(&req.path);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&p, req.content.as_bytes()).unwrap();
            NodeRpcReply {
                ok: true,
                payload: Some(json!({
                    "path": p.to_string_lossy(),
                    "bytes": req.content.len() as u64
                })),
                error: None,
            }
        }
        NodeRpcOp::EditFile(req) => {
            let p = root.join(&req.path);
            let text = std::fs::read_to_string(&p).unwrap();
            let next = text.replacen(&req.target, &req.replacement, 1);
            std::fs::write(&p, next.as_bytes()).unwrap();
            NodeRpcReply {
                ok: true,
                payload: Some(json!({
                    "path": p.to_string_lossy(),
                    "replacements": 1
                })),
                error: None,
            }
        }
        NodeRpcOp::ReadFile(req) => {
            let p = root.join(&req.path);
            let text = std::fs::read_to_string(&p).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            NodeRpcReply {
                ok: true,
                payload: Some(json!({
                    "path": p.to_string_lossy(),
                    "content": lines.join("\n"),
                    "total_lines": lines.len(),
                    "truncated": false
                })),
                error: None,
            }
        }
        NodeRpcOp::ListFiles(req) => {
            let p = root.join(&req.path);
            let mut entries = Vec::new();
            for e in std::fs::read_dir(&p).unwrap() {
                let e = e.unwrap();
                let m = e.metadata().unwrap();
                entries.push(json!({
                    "name": e.file_name().to_string_lossy(),
                    "is_dir": m.is_dir(),
                    "size": if m.is_dir() { 0 } else { m.len() }
                }));
            }
            NodeRpcReply {
                ok: true,
                payload: Some(json!({
                    "path": p.to_string_lossy(),
                    "entries": entries,
                    "truncated": false
                })),
                error: None,
            }
        }
        NodeRpcOp::SearchFiles(req) => {
            let mut matches = Vec::new();
            let p = root.join("src/bot.py");
            let text = std::fs::read_to_string(&p).unwrap();
            for (i, line) in text.lines().enumerate() {
                if line.contains(&req.pattern) {
                    matches.push(json!({
                        "path": "src/bot.py",
                        "line": i + 1,
                        "text": line
                    }));
                }
            }
            NodeRpcReply {
                ok: true,
                payload: Some(json!({
                    "path": root.to_string_lossy(),
                    "matches": matches,
                    "truncated": false
                })),
                error: None,
            }
        }
        NodeRpcOp::Exec(req) => {
            if req.command.starts_with("git ") {
                return NodeRpcReply {
                    ok: true,
                    payload: Some(json!({
                        "exit_code": 128,
                        "stdout": "",
                        "stderr": "fatal: not a git repository\n",
                        "timed_out": false,
                        "duration_ms": 2
                    })),
                    error: None,
                };
            }
            NodeRpcReply {
                ok: true,
                payload: Some(json!({
                    "exit_code": 0,
                    "stdout": format!("{}\n", req.command),
                    "stderr": "",
                    "timed_out": false,
                    "duration_ms": 5
                })),
                error: None,
            }
        }
        NodeRpcOp::ReadMedia(req) => {
            match crossagents_hub::media::locate(std::path::Path::new(&req.cwd), None, &req.path)
                .and_then(|abs| {
                    crossagents_hub::media::load_chunk(&abs, &req.path, req.offset, req.limit)
                })
            {
                Ok(media) => NodeRpcReply {
                    ok: true,
                    payload: serde_json::to_value(media).ok(),
                    error: None,
                },
                Err(err) => NodeRpcReply {
                    ok: false,
                    payload: None,
                    error: Some(err.to_string()),
                },
            }
        }
    }
}

struct EchoDriver;
struct EchoSession {
    events: mpsc::UnboundedSender<SessionEvent>,
    cwd: String,
}

#[async_trait]
impl Driver for EchoDriver {
    fn name(&self) -> &str {
        "mac-codex"
    }
    fn kind(&self) -> DriverKind {
        DriverKind::CodexNative
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            resume: true,
            interrupt: true,
            set_model: true,
            ..Capabilities::default()
        }
    }
    fn models(&self) -> Vec<String> {
        vec!["gpt-5.3-codex".into()]
    }
    async fn open(&self, spec: SessionSpec) -> Result<Box<dyn DriverSession>, CoreError> {
        let _ = spec.events.send(SessionEvent::from(SessionEventKind::Started {
            session_id: "remote-sess-1".into(),
        }));
        Ok(Box::new(EchoSession {
            events: spec.events,
            cwd: spec.agent.cwd.to_string_lossy().into_owned(),
        }))
    }
    async fn resume(
        &self,
        _session_id: &str,
        spec: SessionSpec,
    ) -> Result<Box<dyn DriverSession>, CoreError> {
        self.open(spec).await
    }
}

#[async_trait]
impl DriverSession for EchoSession {
    fn session_id(&self) -> &str {
        "remote-sess-1"
    }
    async fn prompt(&self, text: &str) -> Result<TurnOutcome, CoreError> {
        self.prompt_with(&Prompt::text(text)).await
    }
    async fn prompt_with(&self, prompt: &Prompt) -> Result<TurnOutcome, CoreError> {
        let _ = self.events.send(SessionEvent::from(SessionEventKind::Output {
            stream: OutputStream::Text,
            text: format!("mac-echo[{}]: {}", self.cwd, prompt.text),
        }));
        Ok(TurnOutcome {
            stop_reason: StopReason::EndTurn,
            turn_id: Some("t1".into()),
        })
    }
    async fn interrupt(&self) -> Result<(), CoreError> {
        Ok(())
    }
    async fn shutdown(&self) {}
}

struct TestDriverSource;
impl DriverSource for TestDriverSource {
    fn driver(&self, plugin: &str, _driver: Option<&str>) -> Result<Arc<dyn Driver>, CoreError> {
        if plugin == "mac-codex" {
            Ok(Arc::new(EchoDriver))
        } else {
            Err(CoreError::NotFound {
                kind: "plugin",
                id: plugin.to_owned(),
            })
        }
    }
}

struct TestTransportServer(Mutex<mpsc::UnboundedReceiver<Arc<dyn Transport>>>);
#[async_trait]
impl TransportServer for TestTransportServer {
    async fn accept(&self) -> Option<Arc<dyn Transport>> {
        self.0.lock().await.recv().await
    }
}

struct TestClientTransport {
    out_tx: mpsc::UnboundedSender<String>,
    in_rx: Mutex<mpsc::UnboundedReceiver<String>>,
    closed: watch::Receiver<bool>,
}
#[async_trait]
impl Transport for TestClientTransport {
    async fn send_line(&self, line: String) -> Result<(), TransportError> {
        self.out_tx
            .send(line)
            .map_err(|_| TransportError("closed".into()))
    }
    async fn recv_line(&self) -> Option<String> {
        let mut rx = self.in_rx.lock().await;
        let mut closed = self.closed.clone();
        tokio::select! {
            line = rx.recv() => line,
            _ = closed.wait_for(|c| *c) => None,
        }
    }
}

#[tokio::test]
async fn remote_node_executes_agent_session_and_reports_plugins() {
    let home = tempfile::tempdir().unwrap();
    let cfg = HubConfig::new(Paths::new(home.path().to_path_buf()), 0);
    let hub = crossagents_hub::start(cfg).await.unwrap();
    let base = format!("http://{}", hub.addr);
    let http = reqwest::Client::builder().no_proxy().build().unwrap();

    // 1. Issue a node token for "mac-studio".
    let created: CreateDeviceResponse = http
        .post(format!("{base}/v1/devices"))
        .bearer_auth(&hub.token)
        .json(&CreateDeviceRequest {
            name: "mac-studio".into(),
            kind: Some(DeviceKind::Node),
            level: None,
            os: None,
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let node_token = created.token;
    let node_id = created.device.id;

    // 2. Start a real NodeService reporting `mac-codex` and bridge it onto SSE + POST /v1/node/frames.
    let mut reg = RegisterInfo::local();
    reg.os = "macos".into();
    reg.hostname = "Mac-Studio.local".into();
    reg.plugins = vec![PluginSnapshot {
        name: "mac-codex".into(),
        installed: true,
        version: Some("0.170.0".into()),
        driver: Some("codex-native".into()),
        models: vec!["gpt-5.3-codex".into()],
        efforts: vec!["medium".into(), "high".into()],
        efforts_by_model: BTreeMap::new(),
        capabilities: Some(EchoDriver.capabilities()),
    }];

    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let service = NodeService::new(Arc::new(TestDriverSource)).with_register(reg);
    let service_task = tokio::spawn(service.run(Arc::new(TestTransportServer(Mutex::new(accept_rx)))));

    let resp = http
        .get(format!(
            "{base}/v1/node/stream?os=macos&arch=aarch64&hostname=Mac-Studio.local&shell=zsh&cwd=/Users/link/Dev&chats_dir=/Users/link/.crossagents/chats&version=0.3.6"
        ))
        .bearer_auth(&node_token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let (in_tx, in_rx) = mpsc::unbounded_channel::<String>();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let (close_tx, close_rx) = watch::channel(false);
    accept_tx
        .send(Arc::new(TestClientTransport {
            out_tx,
            in_rx: Mutex::new(in_rx),
            closed: close_rx.clone(),
        }))
        .unwrap();

    let writer_task = {
        let http = http.clone();
        let base = base.clone();
        let token = node_token.clone();
        tokio::spawn(async move {
            while let Some(line) = out_rx.recv().await {
                let _ = http
                    .post(format!("{base}/v1/node/frames"))
                    .bearer_auth(&token)
                    .body(line)
                    .send()
                    .await;
            }
        })
    };
    let reader_task = tokio::spawn(async move {
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut ev = String::new();
        while let Some(Ok(bytes)) = stream.next().await {
            buf.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim_end_matches('\r').to_owned();
                buf.drain(..=pos);
                if line.is_empty() {
                    ev.clear();
                    continue;
                }
                if let Some(e) = line.strip_prefix("event:") {
                    ev = e.trim().to_owned();
                } else if let Some(d) = line.strip_prefix("data:")
                    && ev == "frame"
                {
                    let _ = in_tx.send(d.trim().to_owned());
                }
            }
        }
    });

    // 3. Wait until GET /v1/plugins?node=mac-studio returns `mac-codex`.
    let mut plugins = Vec::new();
    for _ in 0..40 {
        let resp: ListPluginsResponse = http
            .get(format!("{base}/v1/plugins?node=mac-studio"))
            .bearer_auth(&hub.token)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        if !resp.plugins.is_empty() {
            plugins = resp.plugins;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0].name, "mac-codex");
    assert_eq!(plugins[0].models, vec!["gpt-5.3-codex".to_owned()]);

    // 4. Create an agent on `mac-studio` with a macOS path that doesn't exist on Windows!
    let agent: Agent = http
        .post(format!("{base}/v1/agents"))
        .bearer_auth(&hub.token)
        .json(&CreateAgentRequest {
            name: Some("MacCodex".into()),
            topic: Some("Remote test".into()),
            plugin: "mac-codex".into(),
            driver: None,
            model: Some("gpt-5.3-codex".into()),
            role: None,
            cwd: "/Users/link/Dev/MyApp".into(),
            config: serde_json::json!({}),
            profile: Default::default(),
            worktree: None,
            node: Some("mac-studio".into()),
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(agent.node.as_deref(), Some(node_id.as_str()));
    assert_eq!(agent.cwd.to_string_lossy(), "/Users/link/Dev/MyApp");

    // 5. Send a message and confirm the remote node runs it and the transcript records the reply!
    http.post(format!("{base}/v1/agents/{}/messages", agent.id))
        .bearer_auth(&hub.token)
        .json(&SendMessageRequest {
            text: "hello from windows".into(),
            now: None,
            steer: None,
            attachments: None,
            client_message_id: None,
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let mut replied = false;
    for _ in 0..60 {
        let tr: Transcript = http
            .get(format!("{base}/v1/agents/{}/transcript", agent.id))
            .bearer_auth(&hub.token)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let text = serde_json::to_string(&tr).unwrap();
        if text.contains("mac-echo[/Users/link/Dev/MyApp]: hello from windows") {
            replied = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(replied, "remote node reply never appeared in hub transcript");

    // 6. Disconnect the node and confirm it still shows in GET /v1/nodes as offline (`online: false`).
    let _ = close_tx.send(true);
    reader_task.abort();
    writer_task.abort();
    service_task.abort();

    let mut offline_seen = false;
    for _ in 0..40 {
        let list: ListNodesResponse = http
            .get(format!("{base}/v1/nodes"))
            .bearer_auth(&hub.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if list
            .nodes
            .iter()
            .any(|n| n.id == node_id && !n.online && !n.local)
        {
            offline_seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(offline_seen, "disconnected node should still be listed as offline");

    hub.shutdown();
    let _ = hub.wait().await;
}

#[tokio::test]
async fn write_client_device_can_connect_as_node_and_read_client_is_forbidden() {
    use crossagents_api_types::DeviceLevel;

    let home = tempfile::tempdir().unwrap();
    let cfg = HubConfig::new(Paths::new(home.path().to_path_buf()), 0);
    let hub = crossagents_hub::start(cfg).await.unwrap();
    let base = format!("http://{}", hub.addr);
    let http = reqwest::Client::builder().no_proxy().build().unwrap();

    // A read-only client device must NOT be allowed to register as an execution node.
    let read_dev: CreateDeviceResponse = http
        .post(format!("{base}/v1/devices"))
        .bearer_auth(&hub.token)
        .json(&CreateDeviceRequest {
            name: "mac-readonly".into(),
            kind: Some(DeviceKind::Client),
            level: Some(DeviceLevel::Read),
            os: None,
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let denied = http
        .get(format!("{base}/v1/node/stream?os=macos&arch=aarch64"))
        .bearer_auth(&read_dev.token)
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status().as_u16(), 403);

    // A normal paired desktop client (`kind: client, level: write`) can connect directly to
    // `/v1/node/stream` so pairing the GUI once automatically brings its local CLIs online.
    let client_dev: CreateDeviceResponse = http
        .post(format!("{base}/v1/devices"))
        .bearer_auth(&hub.token)
        .json(&CreateDeviceRequest {
            name: "Link-MacBook".into(),
            kind: Some(DeviceKind::Client),
            level: Some(DeviceLevel::Write),
            os: None,
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let resp = http
        .get(format!(
            "{base}/v1/node/stream?os=macos&arch=aarch64&hostname=Link-MacBook.local&shell=zsh&version=0.3.8"
        ))
        .bearer_auth(&client_dev.token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    let mut seen_online = false;
    for _ in 0..30 {
        let list: ListNodesResponse = http
            .get(format!("{base}/v1/nodes"))
            .bearer_auth(&hub.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if list
            .nodes
            .iter()
            .any(|n| n.id == client_dev.device.id && n.name == "Link-MacBook" && n.online)
        {
            seen_online = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(seen_online, "write client device should appear online in GET /v1/nodes");

    // Dropping the stream marks the client node offline while keeping it listed.
    drop(resp);
    let mut seen_offline = false;
    for _ in 0..40 {
        let list: ListNodesResponse = http
            .get(format!("{base}/v1/nodes"))
            .bearer_auth(&hub.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if list
            .nodes
            .iter()
            .any(|n| n.id == client_dev.device.id && !n.online)
        {
            seen_offline = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(seen_offline, "disconnected client node should remain listed as offline");

    hub.shutdown();
    let _ = hub.wait().await;
}

#[tokio::test]
async fn user_profile_accepts_raw_base64_and_data_url_avatars() {
    use crossagents_api_types::UserProfile;

    let home = tempfile::tempdir().unwrap();
    let cfg = HubConfig::new(Paths::new(home.path().to_path_buf()), 0);
    let hub = crossagents_hub::start(cfg).await.unwrap();
    let base = format!("http://{}", hub.addr);
    let http = reqwest::Client::builder().no_proxy().build().unwrap();

    // Minimal valid 1x1 PNG header + bytes.
    const PNG_B64: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

    // Legacy GUI clients send raw base64 without the `data:image/...;base64,` prefix.
    let saved: UserProfile = http
        .post(format!("{base}/v1/profile"))
        .bearer_auth(&hub.token)
        .json(&UserProfile {
            name: "Link".into(),
            avatar: Some(PNG_B64.into()),
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(saved.name, "Link");
    assert_eq!(
        saved.avatar.as_deref(),
        Some(format!("data:image/png;base64,{PNG_B64}").as_str())
    );

    // Full data URLs (including PNG fallback from macOS WKWebView) also normalize cleanly.
    let saved2: UserProfile = http
        .post(format!("{base}/v1/profile"))
        .bearer_auth(&hub.token)
        .json(&UserProfile {
            name: "Link".into(),
            avatar: Some(format!("data:image/png;base64,{PNG_B64}")),
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        saved2.avatar.as_deref(),
        Some(format!("data:image/png;base64,{PNG_B64}").as_str())
    );

    // Non-image / SVG payloads must still be rejected.
    let bad = http
        .post(format!("{base}/v1/profile"))
        .bearer_auth(&hub.token)
        .json(&UserProfile {
            name: "Link".into(),
            avatar: Some("data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=".into()),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status().as_u16(), 400);

    hub.shutdown();
    let _ = hub.wait().await;
}

#[tokio::test]
async fn remote_agent_files_and_node_dirs_end_to_end() {
    use crossagents_api_types::{
        FileContentResponse, FileKind, FileSearchResponse, FileTreeResponse, NodeDirsResponse,
        ResolvePathsRequest, ResolvePathsResponse,
    };

    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(work.path().join("src")).unwrap();
    std::fs::write(work.path().join("README.md"), "# Hello Mac\n").unwrap();
    std::fs::write(work.path().join("src").join("main.rs"), "fn main() {}\n").unwrap();

    let cfg = HubConfig::new(Paths::new(home.path().to_path_buf()), 0);
    let hub = crossagents_hub::start(cfg).await.unwrap();
    let base = format!("http://{}", hub.addr);
    let http = reqwest::Client::builder().no_proxy().build().unwrap();

    // 1. Browse local hub dirs (`GET /v1/nodes/local/dirs`).
    let local_dirs: NodeDirsResponse = http
        .get(format!(
            "{base}/v1/nodes/local/dirs?path={}",
            work.path().to_string_lossy()
        ))
        .bearer_auth(&hub.token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(local_dirs.node, "local");
    assert!(local_dirs.dirs.iter().any(|d| d.name == "src"));

    // 2. Connect a simulated remote node handling file RPCs against `work`.
    let created: CreateDeviceResponse = http
        .post(format!("{base}/v1/devices"))
        .bearer_auth(&hub.token)
        .json(&CreateDeviceRequest {
            name: "mac-files".into(),
            kind: Some(DeviceKind::Node),
            level: None,
            os: None,
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let node_token = created.token;
    let node_id = created.device.id;
    let work_str = work.path().to_string_lossy().replace('\\', "/");
    let resp = http
        .get(format!(
            "{base}/v1/node/stream?os=macos&arch=aarch64&hostname=mac-files.local&shell=zsh&cwd={work_str}&version=0.3.9"
        ))
        .bearer_auth(&node_token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let node_task = {
        let http = http.clone();
        let base = base.clone();
        let token = node_token.clone();
        let root = work.path().to_path_buf();
        tokio::spawn(async move {
            let mut stream = resp.bytes_stream();
            let mut buf = String::new();
            let mut event = String::new();
            while let Some(Ok(bytes)) = stream.next().await {
                buf.push_str(&String::from_utf8_lossy(&bytes));
                while let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim_end_matches('\r').to_owned();
                    buf.drain(..=pos);
                    if line.is_empty() {
                        event.clear();
                        continue;
                    }
                    if let Some(ev) = line.strip_prefix("event:") {
                        event = ev.trim().to_owned();
                    } else if let Some(data) = line.strip_prefix("data:")
                        && event == "call"
                        && let Ok(call) = serde_json::from_str::<NodeRpcCall>(data.trim())
                    {
                        let reply = handle_mock_node_op(&root, call.op);
                        let _ = http
                            .post(format!("{base}/v1/node/results/{}", call.call_id))
                            .bearer_auth(&token)
                            .json(&reply)
                            .send()
                            .await;
                    }
                }
            }
        })
    };

    for _ in 0..30 {
        let list: ListNodesResponse = http
            .get(format!("{base}/v1/nodes"))
            .bearer_auth(&hub.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if list.nodes.iter().any(|n| n.id == node_id && n.online) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 3. Browse remote node dirs (`GET /v1/nodes/{node}/dirs`).
    let remote_dirs: NodeDirsResponse = http
        .get(format!("{base}/v1/nodes/{node_id}/dirs"))
        .bearer_auth(&hub.token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(remote_dirs.node, "mac-files");
    assert!(remote_dirs.dirs.iter().any(|d| d.name == "src"));

    // 4. Create a remote agent and verify all read-only file endpoints route to the remote node.
    let agent: Agent = http
        .post(format!("{base}/v1/agents"))
        .bearer_auth(&hub.token)
        .json(&CreateAgentRequest {
            name: Some("mac-browser".into()),
            plugin: "codex".into(),
            driver: None,
            model: None,
            role: None,
            cwd: work_str.clone(),
            node: Some(node_id.clone()),
            worktree: None,
            config: serde_json::json!({}),
            profile: Default::default(),
            topic: None,
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();

    let tree: FileTreeResponse = http
        .get(format!("{base}/v1/agents/{}/files/tree", agent.id))
        .bearer_auth(&hub.token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(tree.entries.iter().any(|e| e.path == "src" && e.kind == FileKind::Dir));
    assert!(tree.entries.iter().any(|e| e.path == "README.md" && e.kind == FileKind::File));

    let content: FileContentResponse = http
        .get(format!(
            "{base}/v1/agents/{}/files/content?path=src/main.rs",
            agent.id
        ))
        .bearer_auth(&hub.token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!content.binary);
    assert_eq!(content.text.as_deref(), Some("fn main() {}"));

    let search: FileSearchResponse = http
        .get(format!("{base}/v1/agents/{}/files/search?q=main", agent.id))
        .bearer_auth(&hub.token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(search.paths.iter().any(|p| p == "src/main.rs"));

    let resolved: ResolvePathsResponse = http
        .post(format!("{base}/v1/agents/{}/files/resolve", agent.id))
        .bearer_auth(&hub.token)
        .json(&ResolvePathsRequest {
            paths: vec!["src/main.rs".into(), "nope.txt".into()],
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resolved.entries[0].path.as_deref(), Some("src/main.rs"));
    assert_eq!(resolved.entries[0].kind, Some(FileKind::File));
    assert_eq!(resolved.entries[1].path, None);

    // Path traversal must be refused.
    let bad_tree = http
        .get(format!("{base}/v1/agents/{}/files/tree?path=../escape", agent.id))
        .bearer_auth(&hub.token)
        .send()
        .await
        .unwrap();
    assert_eq!(bad_tree.status().as_u16(), 400);

    node_task.abort();
    hub.shutdown();
    let _ = hub.wait().await;
}

#[tokio::test]
async fn device_level_promotion_takes_effect_immediately_without_reconnect() {
    use crossagents_api_types::{Device, DeviceLevel, SetDeviceLevelRequest};

    let home = tempfile::tempdir().unwrap();
    let cfg = HubConfig::new(Paths::new(home.path().to_path_buf()), 0);
    let hub = crossagents_hub::start(cfg).await.unwrap();
    let base = format!("http://{}", hub.addr);
    let http = reqwest::Client::builder().no_proxy().build().unwrap();

    // 1. Issue a write-level client device (same as default pairing code).
    let created: CreateDeviceResponse = http
        .post(format!("{base}/v1/devices"))
        .bearer_auth(&hub.token)
        .json(&CreateDeviceRequest {
            name: "travel-mac".into(),
            kind: Some(DeviceKind::Client),
            level: Some(DeviceLevel::Write),
            os: None,
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = created.token;
    let id = created.device.id;

    // 2. While write-level, admin routes like GET /v1/terminals and level-change are 403.
    let denied_term = http
        .get(format!("{base}/v1/terminals"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(denied_term.status().as_u16(), 403);
    let denied_self_promote = http
        .post(format!("{base}/v1/devices/{id}/level"))
        .bearer_auth(&token)
        .json(&SetDeviceLevelRequest {
            level: DeviceLevel::Admin,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(denied_self_promote.status().as_u16(), 403);

    // 3. Hub admin promotes the device to `admin`.
    let promoted: Device = http
        .post(format!("{base}/v1/devices/{id}/level"))
        .bearer_auth(&hub.token)
        .json(&SetDeviceLevelRequest {
            level: DeviceLevel::Admin,
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(promoted.level, DeviceLevel::Admin);

    // 4. The SAME token immediately accesses admin routes without waiting for cache TTL.
    let allowed_term = http
        .get(format!("{base}/v1/terminals"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(allowed_term.status().as_u16(), 200);

    // 5. Demoting back to read immediately blocks write/admin routes.
    let demoted: Device = http
        .post(format!("{base}/v1/devices/{id}/level"))
        .bearer_auth(&hub.token)
        .json(&SetDeviceLevelRequest {
            level: DeviceLevel::Read,
        })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(demoted.level, DeviceLevel::Read);
    let denied_after = http
        .get(format!("{base}/v1/terminals"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(denied_after.status().as_u16(), 403);

    hub.shutdown();
    let _ = hub.wait().await;
}
