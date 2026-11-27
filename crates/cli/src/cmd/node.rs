//! `crossagents node`: run a lightweight cross-platform execution node (Windows/macOS/Linux)
//! connected to a hub, or inspect and control connected remote nodes from the command line.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use clap::Subcommand;
use crossagents_api_types::{
    ListNodesResponse, NodeEditFileRequest, NodeEditFileResponse, NodeExecRequest,
    NodeExecResponse, NodeFileEntry, NodeListFilesRequest, NodeListFilesResponse,
    NodeReadFileRequest, NodeReadFileResponse, NodeRpcCall, NodeRpcOp, NodeRpcReply,
    NodeSearchFilesRequest, NodeSearchFilesResponse, NodeSearchMatch, NodeWriteFileRequest,
    NodeWriteFileResponse,
};
use crossagents_core::process::NoWindow;
use crossagents_core::{CoreError, Driver, SessionSpec};
use crossagents_hub::{HubConfig, Paths};
use crossagents_node::{
    DriverSource, NodeService, PluginSnapshot, RegisterInfo, Transport, TransportError,
    TransportServer,
};
use crossagents_plugins::PluginRegistry;
use futures::StreamExt;
use tokio::process::Command;
use tokio::sync::{Mutex, mpsc, watch};

use super::Context;
use crate::output::{print_json, print_table};

const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_READ_BYTES: usize = 256 * 1024;
const MAX_SEARCH_FILE_BYTES: u64 = 1024 * 1024;
const BACKOFF_SECS: &[u64] = &[1, 2, 5, 10, 30];

#[derive(Debug, Subcommand)]
pub enum NodeCommand {
    /// Connect this machine to a CrossAgents hub as a remote execution node.
    Run {
        /// Hub base URL (e.g. `http://127.0.0.1:8321` over SSH/iroh tunnel, or a LAN URL).
        #[arg(long, env = "CROSSAGENTS_HUB_URL")]
        hub: String,
        /// Node device token (`crossagents device add <name> --kind node`).
        #[arg(long, env = "CROSSAGENTS_NODE_TOKEN")]
        token: String,
        /// Default working directory for relative paths and commands (defaults to current dir).
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// List online nodes connected to the hub.
    List,
    /// Run a shell command on a connected remote node.
    Exec {
        /// Remote node name, id, or hostname.
        node: String,
        /// Working directory on the remote node.
        #[arg(long)]
        cwd: Option<String>,
        /// Timeout in seconds (1-120, default 60).
        #[arg(long)]
        timeout: Option<u64>,
        /// Command to run on the remote node.
        #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Read a UTF-8 file on a connected remote node.
    Read {
        node: String,
        path: String,
        #[arg(long)]
        start: Option<u32>,
        #[arg(long)]
        end: Option<u32>,
    },
    /// Write a UTF-8 file on a connected remote node (reads stdin if `--content` is omitted).
    Write {
        node: String,
        path: String,
        #[arg(long)]
        content: Option<String>,
        #[arg(long, default_value_t = true)]
        create_dirs: bool,
    },
    /// Find and replace exact text in a file on a connected remote node.
    Edit {
        node: String,
        path: String,
        #[arg(long)]
        target: String,
        #[arg(long)]
        replacement: String,
        #[arg(long, default_value_t = false)]
        all: bool,
    },
    /// List files in a directory on a connected remote node.
    Ls {
        node: String,
        #[arg(default_value = "")]
        path: String,
        #[arg(long)]
        max: Option<u32>,
    },
    /// Search for text in files on a connected remote node.
    Search {
        node: String,
        pattern: String,
        #[arg(default_value = "")]
        path: String,
        #[arg(long)]
        max: Option<u32>,
    },
}

pub async fn run(ctx: &Context, cmd: NodeCommand) -> Result<()> {
    match cmd {
        NodeCommand::Run { hub, token, cwd } => run_daemon(&hub, &token, cwd).await,
        NodeCommand::List => {
            let client = ctx.client()?;
            let list: ListNodesResponse = client.get("/v1/nodes").await?;
            if ctx.json {
                return print_json(&list);
            }
            let rows: Vec<Vec<String>> = list
                .nodes
                .iter()
                .map(|n| {
                    let plugins: Vec<&str> = n
                        .plugins
                        .iter()
                        .filter(|p| p.installed)
                        .map(|p| p.name.as_str())
                        .collect();
                    vec![
                        n.name.clone(),
                        if n.local {
                            "local".into()
                        } else if n.online {
                            "online".into()
                        } else {
                            "offline".into()
                        },
                        format!("{}/{}", n.os, n.arch),
                        if plugins.is_empty() {
                            "-".into()
                        } else {
                            plugins.join(",")
                        },
                        n.hostname.clone(),
                        n.default_cwd.clone(),
                    ]
                })
                .collect();
            print_table(
                &["NAME", "STATE", "PLATFORM", "PLUGINS", "HOST", "CWD"],
                &rows,
            );
            Ok(())
        }
        NodeCommand::Exec {
            node,
            cwd,
            timeout,
            command,
        } => {
            let client = ctx.client()?;
            let req = NodeExecRequest {
                command: command.join(" "),
                cwd,
                timeout_secs: timeout,
            };
            let resp: NodeExecResponse = client
                .post(&format!("/v1/nodes/{node}/exec"), &req)
                .await?;
            if ctx.json {
                return print_json(&resp);
            }
            if !resp.stdout.is_empty() {
                print!("{}", resp.stdout);
            }
            if !resp.stderr.is_empty() {
                eprint!("{}", resp.stderr);
            }
            if resp.timed_out {
                bail!("remote command timed out after {}ms", resp.duration_ms);
            }
            if let Some(code) = resp.exit_code
                && code != 0
            {
                bail!("remote command exited with status {code}");
            }
            Ok(())
        }
        NodeCommand::Read {
            node,
            path,
            start,
            end,
        } => {
            let client = ctx.client()?;
            let req = NodeReadFileRequest {
                path,
                start_line: start,
                end_line: end,
            };
            let resp: NodeReadFileResponse = client
                .post(&format!("/v1/nodes/{node}/files/read"), &req)
                .await?;
            if ctx.json {
                return print_json(&resp);
            }
            print!("{}", resp.content);
            Ok(())
        }
        NodeCommand::Write {
            node,
            path,
            content,
            create_dirs,
        } => {
            let client = ctx.client()?;
            let content = match content {
                Some(c) => c,
                None => {
                    let mut buf = String::new();
                    std::io::stdin()
                        .read_to_string(&mut buf)
                        .context("cannot read file content from stdin")?;
                    buf
                }
            };
            let req = NodeWriteFileRequest {
                path,
                content,
                create_dirs: Some(create_dirs),
            };
            let resp: NodeWriteFileResponse = client
                .post(&format!("/v1/nodes/{node}/files/write"), &req)
                .await?;
            if ctx.json {
                return print_json(&resp);
            }
            println!("wrote {} bytes to {}", resp.bytes, resp.path);
            Ok(())
        }
        NodeCommand::Edit {
            node,
            path,
            target,
            replacement,
            all,
        } => {
            let client = ctx.client()?;
            let req = NodeEditFileRequest {
                path,
                target,
                replacement,
                replace_all: Some(all),
            };
            let resp: NodeEditFileResponse = client
                .post(&format!("/v1/nodes/{node}/files/edit"), &req)
                .await?;
            if ctx.json {
                return print_json(&resp);
            }
            println!(
                "replaced {} occurrence(s) in {}",
                resp.replacements, resp.path
            );
            Ok(())
        }
        NodeCommand::Ls { node, path, max } => {
            let client = ctx.client()?;
            let req = NodeListFilesRequest {
                path,
                max_entries: max,
            };
            let resp: NodeListFilesResponse = client
                .post(&format!("/v1/nodes/{node}/files/list"), &req)
                .await?;
            if ctx.json {
                return print_json(&resp);
            }
            let rows: Vec<Vec<String>> = resp
                .entries
                .iter()
                .map(|e| {
                    vec![
                        if e.is_dir { "dir".into() } else { "file".into() },
                        e.size.to_string(),
                        e.name.clone(),
                    ]
                })
                .collect();
            print_table(&["TYPE", "SIZE", "NAME"], &rows);
            Ok(())
        }
        NodeCommand::Search {
            node,
            pattern,
            path,
            max,
        } => {
            let client = ctx.client()?;
            let req = NodeSearchFilesRequest {
                path,
                pattern,
                max_matches: max,
            };
            let resp: NodeSearchFilesResponse = client
                .post(&format!("/v1/nodes/{node}/files/search"), &req)
                .await?;
            if ctx.json {
                return print_json(&resp);
            }
            for m in &resp.matches {
                println!("{}:{}: {}", m.path, m.line, m.text);
            }
            Ok(())
        }
    }
}

struct LocalRegistrySource(PluginRegistry);

impl DriverSource for LocalRegistrySource {
    fn driver(&self, plugin: &str, driver: Option<&str>) -> Result<Arc<dyn Driver>, CoreError> {
        self.0.local_driver(plugin, driver)
    }
}

struct ChannelTransportServer {
    accepted: Mutex<mpsc::UnboundedReceiver<Arc<dyn Transport>>>,
}

#[async_trait]
impl TransportServer for ChannelTransportServer {
    async fn accept(&self) -> Option<Arc<dyn Transport>> {
        self.accepted.lock().await.recv().await
    }
}

struct HttpClientTransport {
    out_tx: mpsc::UnboundedSender<String>,
    in_rx: Mutex<mpsc::UnboundedReceiver<String>>,
    closed: watch::Receiver<bool>,
}

#[async_trait]
impl Transport for HttpClientTransport {
    async fn send_line(&self, line: String) -> Result<(), TransportError> {
        if *self.closed.borrow() {
            return Err(TransportError("node connection closed".into()));
        }
        self.out_tx
            .send(line)
            .map_err(|_| TransportError("node writer closed".into()))
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

async fn collect_plugin_snapshots(registry: &PluginRegistry) -> Vec<PluginSnapshot> {
    let drivers = registry.defaults();
    let mut handles = Vec::with_capacity(drivers.len());
    for d in &drivers {
        let d = d.clone();
        handles.push(tokio::spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(6), d.probe_cli()).await;
        }));
    }
    for h in handles {
        let _ = h.await;
    }

    let mut snapshots = Vec::with_capacity(drivers.len());
    for d in drivers {
        let name = d.name().to_owned();
        let status = d.cli_status().await;
        let version = status
            .as_ref()
            .and_then(|s| s.located.as_ref())
            .and_then(|l| l.version.clone());
        let installed = d.installed();
        let models = if installed { d.models() } else { Vec::new() };
        let efforts = registry
            .config(&name)
            .map(|c| c.efforts.clone())
            .unwrap_or_default();
        let efforts_by_model = models
            .iter()
            .filter_map(|m| d.model_efforts(m).map(|e| (m.clone(), e)))
            .collect();
        snapshots.push(PluginSnapshot {
            name,
            installed,
            version,
            driver: Some(d.kind().as_str().to_owned()),
            models,
            efforts,
            efforts_by_model,
            capabilities: Some(d.capabilities()),
        });
    }
    snapshots
}

/// On Unix (macOS/Linux), resolves a complete `PATH` combining the current environment, the
/// user's login/interactive shell `PATH`, and common CLI installation directories so Homebrew,
/// nvm, fnm, volta, bun, and npm-global binaries are found even when launched from a GUI app.
#[cfg(unix)]
async fn resolve_unix_path() -> Option<String> {
    const START: &str = "__CA_PATH_START__";
    const END: &str = "__CA_PATH_END__";
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let script = format!("printf '\\n{START}%s{END}\\n' \"$PATH\"");
    let mut shell_path: Option<String> = None;
    for flag in ["-lic", "-lc"] {
        let mut cmd = Command::new(&shell);
        cmd.args([flag, &script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        if let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(3), cmd.output()).await
            && out.status.success()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            if let Some(after) = text.split(START).nth(1)
                && let Some(raw) = after.split(END).next()
            {
                let trimmed = raw.trim();
                if !trimmed.is_empty() {
                    shell_path = Some(trimmed.to_owned());
                    break;
                }
            }
        }
    }

    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push_dir = |p: PathBuf| {
        if !p.as_os_str().is_empty() && !dirs.iter().any(|existing| existing == &p) {
            dirs.push(p);
        }
    };
    if let Some(current) = std::env::var_os("PATH") {
        for p in std::env::split_paths(&current) {
            push_dir(p);
        }
    }
    if let Some(raw) = shell_path {
        for p in std::env::split_paths(&raw) {
            push_dir(p);
        }
    }
    for fixed in [
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        "/usr/local/bin",
        "/usr/local/sbin",
    ] {
        let p = PathBuf::from(fixed);
        if p.is_dir() {
            push_dir(p);
        }
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for rel in [
            ".local/bin",
            ".cargo/bin",
            ".volta/bin",
            ".bun/bin",
            ".npm-global/bin",
            ".pnpm",
            "Library/pnpm",
            ".asdf/shims",
            ".mise/shims",
            ".opencode/bin",
        ] {
            let p = home.join(rel);
            if p.is_dir() {
                push_dir(p);
            }
        }
        for (root, suffix) in [
            (home.join(".nvm/versions/node"), "bin"),
            (home.join(".fnm/node-versions"), "installation/bin"),
        ] {
            if let Ok(entries) = std::fs::read_dir(&root) {
                let mut versions: Vec<PathBuf> = entries
                    .flatten()
                    .map(|e| e.path().join(suffix))
                    .filter(|p| p.is_dir())
                    .collect();
                versions.sort();
                versions.reverse();
                for p in versions {
                    push_dir(p);
                }
            }
        }
    }
    std::env::join_paths(dirs)
        .ok()
        .and_then(|os| os.into_string().ok())
        .filter(|s| !s.is_empty())
}

#[cfg(not(unix))]
async fn resolve_unix_path() -> Option<String> {
    None
}

fn rewrite_remote_spec(
    spec: &mut SessionSpec,
    hub_base: &str,
    default_cwd: &Path,
    extra_path: Option<&str>,
) {
    // Rewrite the hub's collaboration MCP entry so the agent on this node launches THIS machine's
    // `crossagents mcp` binary pointing at the node's reachable hub URL.
    if let Ok(exe) = std::env::current_exe() {
        for mcp in &mut spec.mcp_servers {
            if mcp.name == "crossagents" {
                mcp.command = exe.clone();
                mcp.args = vec!["mcp".into()];
                mcp.env
                    .insert("CROSSAGENTS_URL".into(), hub_base.to_owned());
            }
        }
    }
    // Ensure standalone chat directories (under `.crossagents/chats/`) exist on this machine.
    let cwd_str = spec.agent.cwd.to_string_lossy();
    if cwd_str.trim().is_empty() || cwd_str.trim() == "." {
        spec.agent.cwd = default_cwd.to_path_buf();
    } else if !spec.agent.cwd.exists() {
        let norm = cwd_str.replace('\\', "/");
        if norm.contains("/.crossagents/chats/") {
            let _ = std::fs::create_dir_all(&spec.agent.cwd);
        }
    }
    if let Some(path) = extra_path
        && !spec.env.iter().any(|(k, _)| k.eq_ignore_ascii_case("PATH"))
    {
        spec.env.push(("PATH".into(), path.to_owned()));
    }
}

/// Runs the outbound node daemon loop with automatic reconnect until Ctrl+C or token revocation.
async fn run_daemon(hub: &str, token: &str, cwd: Option<PathBuf>) -> Result<()> {
    let base = hub.trim().trim_end_matches('/').to_owned();
    if base.is_empty() {
        bail!("--hub URL cannot be empty");
    }
    let default_cwd = match cwd {
        Some(dir) => dir,
        None => std::env::current_dir().context("cannot determine current directory")?,
    };
    let paths = Paths::from_env()?;
    let _ = std::fs::create_dir_all(&paths.home);
    let hub_cfg =
        HubConfig::load(paths.clone()).unwrap_or_else(|_| HubConfig::new(paths.clone(), 0));
    let (plugins, warnings) = PluginRegistry::load(&hub_cfg.plugins, &paths.plugins());
    for w in warnings {
        eprintln!("warning: skipping plugin: {w}");
    }
    eprintln!("probing local CLI plugins on this machine...");
    let snapshots = collect_plugin_snapshots(&plugins).await;
    let ready: Vec<String> = snapshots
        .iter()
        .filter(|p| p.installed)
        .map(|p| format!("{} ({} models)", p.name, p.models.len()))
        .collect();
    if ready.is_empty() {
        eprintln!("no local agent CLIs detected (file/exec tools still available)");
    } else {
        eprintln!("ready local CLIs: {}", ready.join(", "));
    }

    let extra_path = resolve_unix_path().await;
    let hostname = node_hostname();
    let shell = default_shell();
    let chats_dir = paths.home.join("chats");
    let _ = std::fs::create_dir_all(&chats_dir);

    let mut register = RegisterInfo::local();
    register.hostname = hostname.clone();
    register.plugins = snapshots;

    let (accept_tx, accepted_rx) = mpsc::unbounded_channel::<Arc<dyn Transport>>();
    let server = Arc::new(ChannelTransportServer {
        accepted: Mutex::new(accepted_rx),
    });
    let rewriter_base = base.clone();
    let rewriter_cwd = default_cwd.clone();
    let rewriter_path = extra_path.clone();
    let service = NodeService::new(Arc::new(LocalRegistrySource(plugins)))
        .with_register(register)
        .with_spec_rewriter(Arc::new(move |spec| {
            rewrite_remote_spec(spec, &rewriter_base, &rewriter_cwd, rewriter_path.as_deref());
        }));
    let service_task = tokio::spawn(service.run(server));

    let http = reqwest::Client::builder()
        .no_proxy()
        .build()
        .context("cannot build HTTP client")?;
    let mut attempt = 0usize;

    loop {
        let cwd_str = default_cwd.to_string_lossy();
        let chats_str = chats_dir.to_string_lossy();
        let query = encode_query(&[
            ("os", std::env::consts::OS),
            ("arch", std::env::consts::ARCH),
            ("hostname", hostname.as_str()),
            ("shell", shell.as_str()),
            ("cwd", cwd_str.as_ref()),
            ("chats_dir", chats_str.as_ref()),
            ("version", env!("CARGO_PKG_VERSION")),
        ]);
        let resp = http
            .get(format!("{base}/v1/node/stream?{query}"))
            .bearer_auth(token)
            .header("x-crossagents-client-os", std::env::consts::OS)
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                attempt = 0;
                eprintln!(
                    "node connected to {base} ({}/{}, shell={shell}, cwd={})",
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                    default_cwd.display()
                );

                // Wire this SSE connection into the persistent `NodeService`.
                let (frame_in_tx, frame_in_rx) = mpsc::unbounded_channel::<String>();
                let (frame_out_tx, mut frame_out_rx) = mpsc::unbounded_channel::<String>();
                let (close_tx, close_rx) = watch::channel(false);
                let transport: Arc<dyn Transport> = Arc::new(HttpClientTransport {
                    out_tx: frame_out_tx,
                    in_rx: Mutex::new(frame_in_rx),
                    closed: close_rx.clone(),
                });
                let _ = accept_tx.send(transport);

                // Ordered HTTP writer: drains queued JSON-line frames into one POST batch per RTT.
                let writer_http = http.clone();
                let writer_url = format!("{base}/v1/node/frames");
                let writer_token = token.to_owned();
                let writer_close = close_tx.clone();
                let mut writer_closed = close_rx;
                let writer_task = tokio::spawn(async move {
                    loop {
                        let first = tokio::select! {
                            _ = writer_closed.wait_for(|c| *c) => break,
                            line = frame_out_rx.recv() => match line {
                                Some(l) => l,
                                None => break,
                            }
                        };
                        let mut batch = first;
                        if !batch.ends_with('\n') {
                            batch.push('\n');
                        }
                        while batch.len() < 128 * 1024 {
                            match frame_out_rx.try_recv() {
                                Ok(next) => {
                                    batch.push_str(&next);
                                    if !batch.ends_with('\n') {
                                        batch.push('\n');
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        match writer_http
                            .post(&writer_url)
                            .bearer_auth(&writer_token)
                            .header("content-type", "text/plain; charset=utf-8")
                            .body(batch)
                            .send()
                            .await
                        {
                            Ok(resp) if resp.status().is_success() => {}
                            Ok(resp) => {
                                eprintln!("node frame post failed with {}", resp.status());
                                let _ = writer_close.send(true);
                                break;
                            }
                            Err(err) => {
                                eprintln!("node frame post error: {err}");
                                let _ = writer_close.send(true);
                                break;
                            }
                        }
                    }
                });

                let mut stream = r.bytes_stream();
                let mut buf = String::new();
                let mut current_event = String::new();
                loop {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {
                            let _ = close_tx.send(true);
                            writer_task.abort();
                            service_task.abort();
                            eprintln!("node shutting down");
                            return Ok(());
                        }
                        chunk = stream.next() => {
                            let Some(chunk) = chunk else { break };
                            let Ok(bytes) = chunk else { break };
                            buf.push_str(&String::from_utf8_lossy(&bytes));
                            while let Some(pos) = buf.find('\n') {
                                let line = buf[..pos].trim_end_matches('\r').to_owned();
                                buf.drain(..=pos);
                                if line.is_empty() {
                                    current_event.clear();
                                    continue;
                                }
                                if let Some(ev) = line.strip_prefix("event:") {
                                    current_event = ev.trim().to_owned();
                                    continue;
                                }
                                if let Some(data) = line.strip_prefix("data:") {
                                    let raw = data.trim().to_owned();
                                    if current_event == "call" {
                                        let http = http.clone();
                                        let base = base.clone();
                                        let token = token.to_owned();
                                        let cwd = default_cwd.clone();
                                        tokio::spawn(async move {
                                            handle_call(http, base, token, cwd, raw).await;
                                        });
                                    } else if current_event == "frame" {
                                        let _ = frame_in_tx.send(raw);
                                    }
                                }
                            }
                        }
                    }
                }
                let _ = close_tx.send(true);
                writer_task.abort();
                eprintln!("disconnected from hub; reconnecting...");
            }
            Ok(r) if r.status().as_u16() == 401 || r.status().as_u16() == 403 => {
                service_task.abort();
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                bail!("hub rejected node token ({status}): {body}");
            }
            Ok(r) => {
                eprintln!("hub returned {}; retrying...", r.status());
            }
            Err(err) => {
                eprintln!("cannot reach hub {base}: {err}; retrying...");
            }
        }

        let wait = BACKOFF_SECS[attempt.min(BACKOFF_SECS.len() - 1)];
        attempt += 1;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                service_task.abort();
                return Ok(());
            }
            _ = tokio::time::sleep(Duration::from_secs(wait)) => {}
        }
    }
}

async fn handle_call(
    http: reqwest::Client,
    base: String,
    token: String,
    default_cwd: PathBuf,
    raw: String,
) {
    let call: NodeRpcCall = match serde_json::from_str(&raw) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("ignoring malformed node call: {err}");
            return;
        }
    };
    let reply = execute_op(&default_cwd, call.op).await;
    let url = format!("{base}/v1/node/results/{}", call.call_id);
    if let Err(err) = http.post(url).bearer_auth(&token).json(&reply).send().await {
        eprintln!("failed to post result for {}: {err}", call.call_id);
    }
}

/// Executes one remote node operation locally on this machine.
pub async fn execute_op(default_cwd: &Path, op: NodeRpcOp) -> NodeRpcReply {
    match op {
        NodeRpcOp::Exec(req) => to_reply(exec_local(default_cwd, req).await),
        NodeRpcOp::ReadFile(req) => to_reply(read_local(default_cwd, req)),
        NodeRpcOp::ReadMedia(req) => to_reply(read_media_local(default_cwd, req)),
        NodeRpcOp::WriteFile(req) => to_reply(write_local(default_cwd, req)),
        NodeRpcOp::EditFile(req) => to_reply(edit_local(default_cwd, req)),
        NodeRpcOp::ListFiles(req) => to_reply(list_local(default_cwd, req)),
        NodeRpcOp::SearchFiles(req) => to_reply(search_local(default_cwd, req)),
    }
}

fn read_media_local(
    default_cwd: &Path,
    req: crossagents_api_types::NodeReadMediaRequest,
) -> Result<crossagents_api_types::MediaContent> {
    let root = if req.cwd.trim().is_empty() {
        default_cwd.to_path_buf()
    } else {
        resolve_path(default_cwd, &req.cwd)
    };
    let normalized = crossagents_hub::media::normalize_input_path(&req.path);
    crossagents_hub::paths::refuse_remote(&root, &normalized)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let abs = crossagents_hub::media::locate(&root, None, &req.path)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    crossagents_hub::media::load_chunk(&abs, &req.path, req.offset, req.limit)
        .map_err(|e| anyhow::anyhow!("{e}"))
}

fn to_reply<T: serde::Serialize>(res: Result<T>) -> NodeRpcReply {
    match res {
        Ok(val) => match serde_json::to_value(val) {
            Ok(payload) => NodeRpcReply {
                ok: true,
                payload: Some(payload),
                error: None,
            },
            Err(err) => NodeRpcReply {
                ok: false,
                payload: None,
                error: Some(err.to_string()),
            },
        },
        Err(err) => NodeRpcReply {
            ok: false,
            payload: None,
            error: Some(err.to_string()),
        },
    }
}

fn resolve_path(default_cwd: &Path, raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return default_cwd.to_path_buf();
    }
    let p = PathBuf::from(trimmed);
    if p.is_absolute() {
        p
    } else {
        default_cwd.join(p)
    }
}

async fn exec_local(default_cwd: &Path, req: NodeExecRequest) -> Result<NodeExecResponse> {
    let cwd = match req.cwd.as_deref() {
        Some(dir) if !dir.trim().is_empty() => resolve_path(default_cwd, dir),
        _ => default_cwd.to_path_buf(),
    };
    let timeout = Duration::from_secs(req.timeout_secs.unwrap_or(60).clamp(1, 120));
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("powershell");
        c.args(["-NoProfile", "-NonInteractive", "-Command", &req.command]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", &req.command]);
        c
    };
    cmd.current_dir(&cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    cmd.no_window();

    let start = Instant::now();
    let child = cmd
        .spawn()
        .with_context(|| format!("cannot spawn command in {}", cwd.display()))?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => Ok(NodeExecResponse {
            exit_code: output.status.code(),
            stdout: clamp_utf8(&output.stdout, MAX_OUTPUT_BYTES),
            stderr: clamp_utf8(&output.stderr, MAX_OUTPUT_BYTES),
            timed_out: false,
            duration_ms: start.elapsed().as_millis() as u64,
        }),
        Ok(Err(err)) => Err(err.into()),
        Err(_) => Ok(NodeExecResponse {
            exit_code: None,
            stdout: String::new(),
            stderr: format!("command timed out after {}s", timeout.as_secs()),
            timed_out: true,
            duration_ms: start.elapsed().as_millis() as u64,
        }),
    }
}

fn read_local(default_cwd: &Path, req: NodeReadFileRequest) -> Result<NodeReadFileResponse> {
    let path = resolve_path(default_cwd, &req.path);
    let raw = std::fs::read(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let text = String::from_utf8(raw)
        .with_context(|| format!("{} is not valid UTF-8 text", path.display()))?;
    let all_lines: Vec<&str> = text.lines().collect();
    let total_lines = all_lines.len() as u32;
    let start = req.start_line.unwrap_or(1).max(1) as usize;
    let end = req
        .end_line
        .map(|e| (e as usize).min(all_lines.len()))
        .unwrap_or(all_lines.len());
    let mut out = if start <= end && start <= all_lines.len() {
        all_lines[start - 1..end].join("\n")
    } else {
        String::new()
    };
    let mut truncated = false;
    if out.len() > MAX_READ_BYTES {
        let mut cut = MAX_READ_BYTES;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        truncated = true;
    }
    Ok(NodeReadFileResponse {
        path: path.to_string_lossy().into_owned(),
        content: out,
        total_lines,
        truncated,
    })
}

fn write_local(default_cwd: &Path, req: NodeWriteFileRequest) -> Result<NodeWriteFileResponse> {
    let path = resolve_path(default_cwd, &req.path);
    if req.create_dirs.unwrap_or(true)
        && let Some(parent) = path.parent()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create directory {}", parent.display()))?;
    }
    let bytes = req.content.len() as u64;
    std::fs::write(&path, req.content.as_bytes())
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(NodeWriteFileResponse {
        path: path.to_string_lossy().into_owned(),
        bytes,
    })
}

fn edit_local(default_cwd: &Path, req: NodeEditFileRequest) -> Result<NodeEditFileResponse> {
    if req.target.is_empty() {
        bail!("edit target cannot be empty");
    }
    let path = resolve_path(default_cwd, &req.path);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let (next, count) = if req.replace_all.unwrap_or(false) {
        let count = text.matches(&req.target).count() as u32;
        (text.replace(&req.target, &req.replacement), count)
    } else if text.contains(&req.target) {
        (text.replacen(&req.target, &req.replacement, 1), 1)
    } else {
        (text, 0)
    };
    if count == 0 {
        bail!("target text not found in {}", path.display());
    }
    std::fs::write(&path, next.as_bytes())
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(NodeEditFileResponse {
        path: path.to_string_lossy().into_owned(),
        replacements: count,
    })
}

fn list_local(default_cwd: &Path, req: NodeListFilesRequest) -> Result<NodeListFilesResponse> {
    let path = resolve_path(default_cwd, &req.path);
    let max_entries = req.max_entries.unwrap_or(200).clamp(1, 2000) as usize;
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&path)
        .with_context(|| format!("cannot list {}", path.display()))?
    {
        let entry = entry?;
        let meta = entry.metadata()?;
        entries.push(NodeFileEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
        });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let truncated = entries.len() > max_entries;
    entries.truncate(max_entries);
    Ok(NodeListFilesResponse {
        path: path.to_string_lossy().into_owned(),
        entries,
        truncated,
    })
}

fn search_local(default_cwd: &Path, req: NodeSearchFilesRequest) -> Result<NodeSearchFilesResponse> {
    if req.pattern.is_empty() {
        bail!("search pattern cannot be empty");
    }
    let root = resolve_path(default_cwd, &req.path);
    let max_matches = req.max_matches.unwrap_or(100).clamp(1, 500) as usize;
    let mut matches = Vec::new();
    let mut truncated = false;
    search_walk(&root, &root, &req.pattern, max_matches, &mut matches, &mut truncated);
    Ok(NodeSearchFilesResponse {
        path: root.to_string_lossy().into_owned(),
        matches,
        truncated,
    })
}

fn search_walk(
    root: &Path,
    dir: &Path,
    pattern: &str,
    max_matches: usize,
    out: &mut Vec<NodeSearchMatch>,
    truncated: &mut bool,
) {
    if out.len() >= max_matches {
        *truncated = true;
        return;
    }
    if dir.is_file() {
        search_file(root, dir, pattern, max_matches, out, truncated);
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for p in paths {
        if out.len() >= max_matches {
            *truncated = true;
            return;
        }
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if p.is_dir() {
            if matches!(name, ".git" | "node_modules" | "target" | ".next" | "dist") {
                continue;
            }
            search_walk(root, &p, pattern, max_matches, out, truncated);
        } else if p.is_file() {
            search_file(root, &p, pattern, max_matches, out, truncated);
        }
    }
}

fn search_file(
    root: &Path,
    file: &Path,
    pattern: &str,
    max_matches: usize,
    out: &mut Vec<NodeSearchMatch>,
    truncated: &mut bool,
) {
    let Ok(meta) = std::fs::metadata(file) else {
        return;
    };
    if meta.len() > MAX_SEARCH_FILE_BYTES {
        return;
    }
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };
    let rel = file
        .strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/");
    for (idx, line) in text.lines().enumerate() {
        if line.contains(pattern) {
            if out.len() >= max_matches {
                *truncated = true;
                return;
            }
            out.push(NodeSearchMatch {
                path: rel.clone(),
                line: (idx + 1) as u32,
                text: line.chars().take(240).collect(),
            });
        }
    }
}

fn encode_query(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", pct_encode(k), pct_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn clamp_utf8(bytes: &[u8], max: usize) -> String {
    if bytes.len() <= max {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        let mut s = String::from_utf8_lossy(&bytes[..max]).into_owned();
        s.push_str("\n... [output truncated]");
        s
    }
}

fn node_hostname() -> String {
    if let Ok(v) = std::env::var(if cfg!(windows) {
        "COMPUTERNAME"
    } else {
        "HOSTNAME"
    }) {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }
    #[cfg(target_os = "macos")]
    if let Ok(out) = std::process::Command::new("scutil")
        .args(["--get", "LocalHostName"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        && out.status.success()
    {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !s.is_empty() {
            return s;
        }
    }
    #[cfg(unix)]
    if let Ok(out) = std::process::Command::new("hostname")
        .arg("-s")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        && out.status.success()
    {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !s.is_empty() {
            return s;
        }
    }
    "node".into()
}

fn default_shell() -> String {
    if cfg!(windows) {
        "powershell".into()
    } else {
        std::env::var("SHELL")
            .ok()
            .and_then(|s| {
                Path::new(&s)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "sh".into())
    }
}
