//! Running a session's [`script`] directives: files are written in the session's cwd, tool
//! calls go to the session's first MCP server. Shared by the ACP and stream-json mocks.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use crossagents_drivers::jsonrpc::{RpcError, RpcHandler, RpcPeer};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;

use crate::script::{self, Action, Directive};

/// How to start an MCP server.
#[derive(Debug, Clone, Default)]
pub struct McpLaunch {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// A client side that ignores whatever an MCP server sends unprompted.
struct Quiet;

#[async_trait]
impl RpcHandler for Quiet {
    async fn request(&self, method: &str, _: Value) -> Result<Value, RpcError> {
        Err(RpcError::method_not_found(method))
    }
    async fn notification(&self, _: &str, _: Value) {}
}

#[derive(Default)]
pub struct Scripted {
    pub cwd: Mutex<PathBuf>,
    /// The session's first MCP server.
    pub server: Mutex<Option<McpLaunch>>,
    mcp: Mutex<Option<McpProcess>>,
    last: Mutex<Vec<Directive>>,
}

struct McpProcess {
    peer: RpcPeer,
    child: tokio::process::Child,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_reader: tokio::task::JoinHandle<()>,
}

impl McpProcess {
    async fn diagnostic(&mut self) -> String {
        // EOF can precede termination and stderr drainage; bound diagnostics because a
        // server (or a descendant holding stderr) can continue running after stdout closes.
        let status =
            match tokio::time::timeout(std::time::Duration::from_millis(200), self.child.wait())
                .await
            {
                Ok(Ok(status)) => {
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_millis(200),
                        &mut self.stderr_reader,
                    )
                    .await;
                    format!("{status}")
                }
                Err(_) => "still running (exit status unavailable)".to_owned(),
                Ok(Err(err)) => format!("unavailable: {err}"),
            };
        let stderr = self.stderr.lock().await;
        format!(
            "MCP exit status: {status}; stderr: {}",
            String::from_utf8_lossy(&stderr)
        )
    }
}

impl Scripted {
    /// Connects to the session's MCP server on first use.
    async fn mcp(&self) -> Result<RpcPeer, String> {
        let mut slot = self.mcp.lock().await;
        if let Some(process) = slot.as_ref() {
            return Ok(process.peer.clone());
        }
        let server = self
            .server
            .lock()
            .await
            .clone()
            .ok_or("no MCP server in this session")?;
        let mut cmd = tokio::process::Command::new(&server.command);
        cmd.args(&server.args).envs(server.env.iter().cloned());
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("cannot start MCP server: {e}"))?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let mut stderr = child.stderr.take().expect("piped");
        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink = captured.clone();
        let stderr_reader = tokio::spawn(async move {
            let mut buf = [0; 4096];
            while let Ok(n) = stderr.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                sink.lock().await.extend_from_slice(&buf[..n]);
            }
        });
        let (peer, _) = RpcPeer::spawn(stdout, stdin, Arc::new(Quiet));
        let mut process = McpProcess {
            peer: peer.clone(),
            child,
            stderr: captured,
            stderr_reader,
        };
        let initialized: Result<Value, _> = peer
            .request(
                "initialize",
                json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "mock", "version": "0" } }),
            )
            .await;
        if let Err(err) = initialized {
            return Err(format!("{err}; {}", process.diagnostic().await));
        }
        let _ = peer.notify("notifications/initialized", json!({}));
        *slot = Some(process);
        Ok(peer)
    }

    /// Runs the directives in `text` that apply to `model`; returns a line per action for
    /// the transcript. A prompt without directives that carries a `task_id:` line repeats
    /// the session's last directives (for retries).
    pub async fn run(&self, text: &str, model: &str) -> Vec<String> {
        let (task_id, attempt) = script::context(text);
        let mut directives = script::parse(text, task_id.as_deref(), attempt);
        if directives.is_empty() && task_id.is_some() {
            directives = self.last.lock().await.clone();
        } else if !directives.is_empty() {
            *self.last.lock().await = directives.clone();
        }
        let mut log = Vec::new();
        for directive in directives.iter().filter(|d| d.applies(attempt, model)) {
            match &directive.action {
                Action::Write { path, content } => {
                    let full = self.cwd.lock().await.join(path);
                    if let Some(parent) = full.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    match std::fs::write(&full, content) {
                        Ok(()) => log.push(format!("wrote {path}")),
                        Err(e) => log.push(format!("cannot write {path}: {e}")),
                    }
                }
                Action::Call { tool, args } => {
                    // Remembered directives were parsed for an earlier prompt; fill in this
                    // prompt's task id. Without one, placeholders pass through untouched.
                    let args = match &task_id {
                        Some(id) => {
                            serde_json::from_str(&args.to_string().replace("{task_id}", id))
                                .unwrap_or(args.clone())
                        }
                        None => args.clone(),
                    };
                    let result = match self.mcp().await {
                        Ok(peer) => peer
                            .request::<_, Value>(
                                "tools/call",
                                json!({ "name": tool, "arguments": args }),
                            )
                            .await
                            .map(|r| {
                                r["content"][0]["text"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_owned()
                            })
                            .map_err(|e| e.to_string()),
                        Err(e) => Err(e),
                    };
                    let result = match result {
                        Err(err) => {
                            let mut slot = self.mcp.lock().await;
                            match slot.as_mut() {
                                Some(process) => {
                                    Err(format!("{err}; {}", process.diagnostic().await))
                                }
                                None => Err(err),
                            }
                        }
                        ok => ok,
                    };
                    match result {
                        Ok(text) => log.push(format!("called {tool}: {text}")),
                        Err(e) => log.push(format!("call {tool} failed: {e}")),
                    }
                }
            }
        }
        log
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reports_stderr_and_exit_status_when_initialization_fails() {
        #[cfg(windows)]
        let (command, args) = (
            "cmd.exe",
            vec!["/C", "echo startup marker 1>&2 & exit /b 43"],
        );
        #[cfg(not(windows))]
        let (command, args) = ("sh", vec!["-c", "echo startup marker >&2; exit 43"]);
        let script = Scripted::default();
        *script.server.lock().await = Some(McpLaunch {
            command: command.into(),
            args: args.into_iter().map(str::to_owned).collect(),
            env: vec![],
        });
        let output = script.run("[call plan_submit {}]", "test").await.join("\n");
        assert!(output.contains("connection closed"), "{output}");
        assert!(output.contains("43"), "{output}");
        assert!(output.contains("startup marker"), "{output}");
    }

    #[tokio::test]
    async fn reports_stderr_and_exit_status_after_call_connection_closes() {
        #[cfg(windows)]
        let (command, args) = (
            "powershell.exe",
            vec![
                "-NoProfile",
                "-Command",
                r#"$null = [Console]::ReadLine(); [Console]::WriteLine('{"jsonrpc":"2.0","id":1,"result":{}}'); $null = [Console]::ReadLine(); $null = [Console]::ReadLine(); [Console]::Error.WriteLine('diagnostic marker'); exit 42"#,
            ],
        );
        #[cfg(not(windows))]
        let (command, args) = (
            "sh",
            vec![
                "-c",
                "read line; echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}'; read line; read line; echo 'diagnostic marker' >&2; exit 42",
            ],
        );
        let script = Scripted::default();
        *script.server.lock().await = Some(McpLaunch {
            command: command.into(),
            args: args.into_iter().map(str::to_owned).collect(),
            env: vec![],
        });
        let output = script.run("[call plan_submit {}]", "test").await.join("\n");
        assert!(output.contains("connection closed"), "{output}");
        assert!(output.contains("42"), "{output}");
        assert!(output.contains("diagnostic marker"), "{output}");
    }
}
