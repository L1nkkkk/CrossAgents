//! ACP driver: runs an ACP agent as a child process and drives it as the client.

pub mod wire;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use crossagents_core::{
    AgentSession, CoreError, McpServerSpec, OutputStream, PermissionPolicy, SessionEvent,
    StopReason, ToolStatus,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc};

use self::wire::{method, *};
use crate::jsonrpc::{RpcError, RpcHandler, RpcPeer};

const INIT_TIMEOUT: Duration = Duration::from_secs(180);
const STDERR_TAIL: usize = 40;

/// How to launch the agent process.
#[derive(Debug, Clone)]
pub struct AcpLaunch {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
}

impl AcpLaunch {
    /// Builds a launch from a command line (`["npx", "-y", "pkg"]`).
    pub fn from_command(command: &[String], cwd: PathBuf) -> Result<Self, CoreError> {
        let (program, args) = command
            .split_first()
            .ok_or_else(|| CoreError::Agent("empty launch command".into()))?;
        Ok(Self {
            program: program.clone(),
            args: args.to_vec(),
            env: Vec::new(),
            cwd,
        })
    }
}

pub struct AcpOptions {
    pub launch: AcpLaunch,
    pub mcp_servers: Vec<McpServerSpec>,
    pub permission: PermissionPolicy,
    /// Model to select through the session's `model` config option.
    pub model: Option<String>,
    /// Sent ahead of the first prompt.
    pub instructions: Option<String>,
    pub events: mpsc::UnboundedSender<SessionEvent>,
}

pub struct AcpSession {
    peer: RpcPeer,
    session_id: String,
    child: Mutex<Child>,
    pending_instructions: Mutex<Option<String>>,
    /// Set before we kill the process, so its exit is not reported as a failure.
    shutting_down: Arc<AtomicBool>,
    pub agent_info: Option<Implementation>,
}

impl AcpSession {
    /// Spawns the agent, performs the ACP handshake and opens a session.
    pub async fn start(opts: AcpOptions) -> Result<Self, CoreError> {
        let AcpOptions {
            launch,
            mcp_servers,
            permission,
            model,
            instructions,
            events,
        } = opts;

        let program = resolve_program(&launch.program);
        let mut command = Command::new(&program);
        command
            .args(&launch.args)
            .envs(launch.env.iter().cloned())
            .current_dir(&launch.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        hide_console(&mut command);
        let mut child = command
            .spawn()
            .map_err(|e| CoreError::Agent(format!("cannot start {}: {e}", program.display())))?;

        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        tokio::spawn(collect_stderr(stderr, tail.clone()));

        let handler = Arc::new(ClientHandler {
            events: events.clone(),
            permission,
        });
        let (peer, reader) = RpcPeer::spawn(stdout, stdin, handler);

        let shutting_down = Arc::new(AtomicBool::new(false));
        tokio::spawn({
            let events = events.clone();
            let tail = tail.clone();
            let shutting_down = shutting_down.clone();
            async move {
                let _ = reader.await;
                if !shutting_down.load(Ordering::SeqCst) {
                    let tail: Vec<String> = tail.lock().await.iter().cloned().collect();
                    let message = if tail.is_empty() {
                        "agent process exited".to_owned()
                    } else {
                        format!("agent process exited:\n{}", tail.join("\n"))
                    };
                    let _ = events.send(SessionEvent::Exited { message });
                }
            }
        });

        let handshake = async {
            let init: InitializeResponse = peer
                .request(
                    method::INITIALIZE,
                    InitializeRequest {
                        protocol_version: PROTOCOL_VERSION,
                        client_capabilities: ClientCapabilities::default(),
                        client_info: Implementation {
                            name: "crossagents".into(),
                            version: env!("CARGO_PKG_VERSION").into(),
                        },
                    },
                )
                .await?;
            let session: NewSessionResponse = peer
                .request(
                    method::SESSION_NEW,
                    NewSessionRequest {
                        cwd: launch.cwd.display().to_string(),
                        mcp_servers: mcp_servers.iter().map(to_wire_mcp).collect(),
                    },
                )
                .await?;
            Ok::<_, RpcError>((init, session))
        };
        let (init, session) = match tokio::time::timeout(INIT_TIMEOUT, handshake).await {
            Ok(Ok(result)) => result,
            Ok(Err(err)) => {
                let tail: Vec<String> = tail.lock().await.iter().cloned().collect();
                return Err(CoreError::Agent(format!(
                    "ACP handshake failed: {err}{}",
                    if tail.is_empty() {
                        String::new()
                    } else {
                        format!("\n{}", tail.join("\n"))
                    }
                )));
            }
            Err(_) => return Err(CoreError::Agent("ACP handshake timed out".into())),
        };
        if init.protocol_version != PROTOCOL_VERSION {
            tracing::warn!(
                version = init.protocol_version,
                "agent speaks a different ACP version"
            );
        }

        let this = Self {
            peer,
            session_id: session.session_id.clone(),
            child: Mutex::new(child),
            pending_instructions: Mutex::new(instructions),
            shutting_down,
            agent_info: init.agent_info,
        };
        this.configure(&session, permission, model.as_deref(), &events)
            .await;
        Ok(this)
    }

    async fn configure(
        &self,
        session: &NewSessionResponse,
        permission: PermissionPolicy,
        model: Option<&str>,
        events: &mpsc::UnboundedSender<SessionEvent>,
    ) {
        let warn = |message: String| {
            let _ = events.send(SessionEvent::Warning { message });
        };

        if permission == PermissionPolicy::Bypass
            && let Some(modes) = &session.modes
            && let Some(mode) = modes
                .available_modes
                .iter()
                .find(|m| m.id.to_lowercase().contains("bypass"))
            && mode.id != modes.current_mode_id
        {
            let request = SetSessionModeRequest {
                session_id: self.session_id.clone(),
                mode_id: mode.id.clone(),
            };
            if let Err(err) = self
                .peer
                .request::<_, Value>(method::SESSION_SET_MODE, request)
                .await
            {
                warn(format!("cannot switch to mode {}: {err}", mode.id));
            }
        }

        let Some(wanted) = model else { return };
        let options = session.config_options.as_deref().unwrap_or_default();
        let Some(option) =
            find_select_option(options, |id, cat| cat == Some("model") || id == "model")
        else {
            warn(format!(
                "agent offers no model option; relying on its default instead of {wanted:?}"
            ));
            return;
        };
        let Some(value) = option.resolve(wanted) else {
            let available: Vec<_> = option.values.iter().map(|v| v.value.as_str()).collect();
            warn(format!(
                "model {wanted:?} not offered (available: {})",
                available.join(", ")
            ));
            return;
        };
        if value.value == option.current {
            return;
        }
        let request = SetSessionConfigOptionRequest {
            session_id: self.session_id.clone(),
            config_id: option.id.clone(),
            value: value.value.clone(),
        };
        if let Err(err) = self
            .peer
            .request::<_, Value>(method::SESSION_SET_CONFIG_OPTION, request)
            .await
        {
            warn(format!("cannot select model {}: {err}", value.value));
        }
    }
}

#[async_trait]
impl AgentSession for AcpSession {
    async fn prompt(&self, text: &str) -> Result<StopReason, CoreError> {
        let mut prompt = Vec::new();
        if let Some(instructions) = self.pending_instructions.lock().await.take() {
            prompt.push(ContentBlock::Text { text: instructions });
        }
        prompt.push(ContentBlock::Text { text: text.into() });
        let response: PromptResponse = self
            .peer
            .request(
                method::SESSION_PROMPT,
                PromptRequest {
                    session_id: self.session_id.clone(),
                    prompt,
                },
            )
            .await
            .map_err(|e| CoreError::Agent(e.to_string()))?;
        Ok(stop_reason(&response.stop_reason))
    }

    async fn cancel(&self) -> Result<(), CoreError> {
        self.peer
            .notify(
                method::SESSION_CANCEL,
                CancelNotification {
                    session_id: self.session_id.clone(),
                },
            )
            .map_err(|e| CoreError::Agent(e.to_string()))
    }

    async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        let mut child = self.child.lock().await;
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    }
}

fn stop_reason(raw: &str) -> StopReason {
    match raw {
        "end_turn" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "max_turn_requests" => StopReason::MaxTurnRequests,
        "refusal" => StopReason::Refusal,
        "cancelled" => StopReason::Cancelled,
        _ => StopReason::EndTurn,
    }
}

fn to_wire_mcp(spec: &McpServerSpec) -> McpServerStdio {
    McpServerStdio {
        name: spec.name.clone(),
        command: spec.command.display().to_string(),
        args: spec.args.clone(),
        env: spec
            .env
            .iter()
            .map(|(name, value)| EnvVariable {
                name: name.clone(),
                value: value.clone(),
            })
            .collect(),
    }
}

async fn collect_stderr(stderr: tokio::process::ChildStderr, tail: Arc<Mutex<VecDeque<String>>>) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::debug!(target: "crossagents::agent_stderr", "{line}");
        let mut tail = tail.lock().await;
        if tail.len() == STDERR_TAIL {
            tail.pop_front();
        }
        tail.push_back(line);
    }
}

/// Answers the agent's requests and turns its updates into [`SessionEvent`]s.
struct ClientHandler {
    events: mpsc::UnboundedSender<SessionEvent>,
    permission: PermissionPolicy,
}

#[async_trait]
impl RpcHandler for ClientHandler {
    async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        match method {
            method::SESSION_REQUEST_PERMISSION => {
                let req: RequestPermissionRequest =
                    serde_json::from_value(params).map_err(RpcError::invalid_params)?;
                let outcome = choose_permission(self.permission, &req.options);
                serde_json::to_value(RequestPermissionResponse { outcome })
                    .map_err(|e| RpcError::new(crate::jsonrpc::INTERNAL_ERROR, e.to_string()))
            }
            _ => Err(RpcError::method_not_found(method)),
        }
    }

    async fn notification(&self, method: &str, params: Value) {
        if method != method::SESSION_UPDATE {
            return;
        }
        let Ok(note) = serde_json::from_value::<SessionNotification>(params) else {
            return;
        };
        if let Some(event) = parse_update(&note.update) {
            let _ = self.events.send(event);
        }
    }
}

fn choose_permission(policy: PermissionPolicy, options: &[PermissionOption]) -> PermissionOutcome {
    let pick = |kinds: &[&str]| {
        kinds
            .iter()
            .find_map(|kind| options.iter().find(|o| o.kind == *kind))
            .map(|o| PermissionOutcome::Selected {
                option_id: o.option_id.clone(),
            })
    };
    let choice = match policy {
        PermissionPolicy::Bypass => pick(&["allow_always", "allow_once"]),
        PermissionPolicy::Deny => pick(&["reject_once", "reject_always"]),
    };
    choice.unwrap_or(PermissionOutcome::Cancelled)
}

fn tool_status(value: Option<&Value>) -> Option<ToolStatus> {
    match value?.as_str()? {
        "pending" => Some(ToolStatus::Pending),
        "in_progress" => Some(ToolStatus::InProgress),
        "completed" => Some(ToolStatus::Completed),
        "failed" => Some(ToolStatus::Failed),
        _ => None,
    }
}

fn parse_update(update: &Value) -> Option<SessionEvent> {
    let str_of = |key: &str| update.get(key).and_then(Value::as_str);
    match str_of("sessionUpdate")? {
        kind @ ("agent_message_chunk" | "agent_thought_chunk") => {
            let content = update.get("content")?;
            if content.get("type")?.as_str()? != "text" {
                return None;
            }
            Some(SessionEvent::Output {
                stream: if kind == "agent_message_chunk" {
                    OutputStream::Text
                } else {
                    OutputStream::Thought
                },
                text: content.get("text")?.as_str()?.to_owned(),
            })
        }
        "tool_call" => Some(SessionEvent::ToolCall {
            tool_call_id: str_of("toolCallId")?.to_owned(),
            title: str_of("title").unwrap_or_default().to_owned(),
            kind: str_of("kind").map(str::to_owned),
            status: tool_status(update.get("status")).unwrap_or(ToolStatus::Pending),
        }),
        "tool_call_update" => Some(SessionEvent::ToolCallUpdated {
            tool_call_id: str_of("toolCallId")?.to_owned(),
            title: str_of("title").map(str::to_owned),
            status: tool_status(update.get("status")),
        }),
        "usage_update" => {
            let cost = update.get("cost");
            let cost_usd = cost
                .filter(|c| c.get("currency").and_then(Value::as_str) == Some("USD"))
                .and_then(|c| c.get("amount")?.as_f64());
            Some(SessionEvent::Usage {
                used: update.get("used")?.as_u64()?,
                size: update.get("size")?.as_u64()?,
                cost_usd,
            })
        }
        _ => None,
    }
}

/// On Windows a bare program name does not find `npx.cmd` and friends; search `PATH`.
fn resolve_program(program: &str) -> PathBuf {
    let path = PathBuf::from(program);
    if !cfg!(windows) || path.extension().is_some() || path.components().count() > 1 {
        return path;
    }
    let Some(dirs) = std::env::var_os("PATH") else {
        return path;
    };
    for dir in std::env::split_paths(&dirs) {
        for ext in ["exe", "cmd", "bat"] {
            let candidate = dir.join(program).with_extension(ext);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    path
}

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_: &mut Command) {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn bypass_prefers_allow_always_and_deny_rejects() {
        let options: Vec<PermissionOption> = serde_json::from_value(json!([
            { "optionId": "a1", "name": "Allow", "kind": "allow_once" },
            { "optionId": "a2", "name": "Always", "kind": "allow_always" },
            { "optionId": "r1", "name": "Reject", "kind": "reject_once" },
        ]))
        .unwrap();
        assert_eq!(
            choose_permission(PermissionPolicy::Bypass, &options),
            PermissionOutcome::Selected {
                option_id: "a2".into()
            }
        );
        assert_eq!(
            choose_permission(PermissionPolicy::Deny, &options),
            PermissionOutcome::Selected {
                option_id: "r1".into()
            }
        );
        assert_eq!(
            choose_permission(PermissionPolicy::Deny, &options[..2]),
            PermissionOutcome::Cancelled
        );
    }

    #[test]
    fn parses_updates() {
        let chunk = json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "hi" } });
        assert_eq!(
            parse_update(&chunk),
            Some(SessionEvent::Output {
                stream: OutputStream::Text,
                text: "hi".into()
            })
        );
        let tool = json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Read x", "kind": "read", "status": "in_progress" });
        assert!(matches!(
            parse_update(&tool),
            Some(SessionEvent::ToolCall {
                status: ToolStatus::InProgress,
                ..
            })
        ));
        let usage = json!({ "sessionUpdate": "usage_update", "used": 10, "size": 100, "cost": { "amount": 0.5, "currency": "USD" } });
        assert_eq!(
            parse_update(&usage),
            Some(SessionEvent::Usage {
                used: 10,
                size: 100,
                cost_usd: Some(0.5)
            })
        );
        assert_eq!(
            parse_update(&json!({ "sessionUpdate": "plan", "entries": [] })),
            None
        );
    }
}
