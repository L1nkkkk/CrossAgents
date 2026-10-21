//! `acp-generic`: runs any ACP agent as a child process and drives it as the client.
//!
//! One process per session. The fallback driver for every agent without a native one.

pub mod wire;

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use crossagents_core::{
    Capabilities, CoreError, Driver, DriverKind, DriverSession, McpServerSpec, OutputStream,
    PermissionPolicy, SessionEvent, SessionEventKind, SessionSpec, StopReason, ToolStatus,
    TurnOutcome,
};
use serde_json::Value;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc};

use self::wire::{method, *};
use crate::jsonrpc::{RpcError, RpcHandler, RpcPeer};
use crate::process::{StderrTail, hide_console, resolve_program};

const INIT_TIMEOUT: Duration = Duration::from_secs(180);

/// A Profile whose agent speaks ACP.
pub struct AcpDriver {
    name: String,
    command: Vec<String>,
    models: Vec<String>,
    /// Environment variable that receives the session's model, e.g. `ANTHROPIC_MODEL`.
    model_env: Option<String>,
}

impl AcpDriver {
    pub fn new(
        name: impl Into<String>,
        command: Vec<String>,
        models: Vec<String>,
        model_env: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            command,
            models,
            model_env,
        }
    }

    pub fn command(&self) -> &[String] {
        &self.command
    }
}

#[async_trait]
impl Driver for AcpDriver {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> DriverKind {
        DriverKind::AcpGeneric
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            interrupt: true,
            mcp: true,
            usage: true,
            ..Capabilities::default()
        }
    }

    fn models(&self) -> Vec<String> {
        self.models.clone()
    }

    async fn open(&self, spec: SessionSpec) -> Result<Box<dyn DriverSession>, CoreError> {
        let command = spec.command.clone().unwrap_or_else(|| self.command.clone());
        let mut launch = AcpLaunch::from_command(&command, spec.agent.cwd.clone())?;
        if let (Some(var), Some(model)) = (&self.model_env, &spec.agent.model) {
            launch.env.push((var.clone(), model.clone()));
        }
        launch.env.extend(spec.env);
        let session = AcpSession::start(AcpOptions {
            launch,
            mcp_servers: spec.mcp_servers,
            permission: spec.permission,
            model: spec.agent.model,
            instructions: spec.instructions,
            events: spec.events,
        })
        .await?;
        Ok(Box::new(session))
    }
}

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
    events: mpsc::UnboundedSender<SessionEvent>,
    /// Set before we kill the process, so its exit is not reported as a failure.
    shutting_down: Arc<AtomicBool>,
    pub agent_info: Option<Implementation>,
}

fn emit(events: &mpsc::UnboundedSender<SessionEvent>, kind: SessionEventKind) {
    let _ = events.send(kind.into());
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
        let tail = StderrTail::spawn(child.stderr.take().expect("piped stderr"));

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
                    let message = tail.describe("agent process exited").await;
                    emit(&events, SessionEventKind::Exited { message });
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
                return Err(CoreError::Agent(
                    tail.describe(&format!("ACP handshake failed: {err}")).await,
                ));
            }
            Err(_) => return Err(CoreError::Agent("ACP handshake timed out".into())),
        };
        if init.protocol_version != PROTOCOL_VERSION {
            tracing::warn!(
                version = init.protocol_version,
                "agent speaks a different ACP version"
            );
        }

        emit(
            &events,
            SessionEventKind::Started {
                session_id: session.session_id.clone(),
            },
        );
        let this = Self {
            peer,
            session_id: session.session_id.clone(),
            child: Mutex::new(child),
            pending_instructions: Mutex::new(instructions),
            events: events.clone(),
            shutting_down,
            agent_info: init.agent_info,
        };
        this.configure(&session, permission, model.as_deref()).await;
        Ok(this)
    }

    async fn configure(
        &self,
        session: &NewSessionResponse,
        permission: PermissionPolicy,
        model: Option<&str>,
    ) {
        let warn = |message: String| emit(&self.events, SessionEventKind::Warning { message });

        if permission == PermissionPolicy::Bypass
            && let Some(modes) = &session.modes
        {
            match bypass_mode(&modes.available_modes) {
                Some(mode) if mode.id == modes.current_mode_id => {}
                Some(mode) => {
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
                None if !modes.available_modes.is_empty() => warn(format!(
                    "no unrestricted mode among {}; staying in {:?}, so the agent may be \
                     sandboxed or ask for approvals",
                    modes
                        .available_modes
                        .iter()
                        .map(|m| m.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    modes.current_mode_id
                )),
                None => {}
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
impl DriverSession for AcpSession {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    async fn prompt(&self, text: &str) -> Result<TurnOutcome, CoreError> {
        let mut prompt = Vec::new();
        if let Some(instructions) = self.pending_instructions.lock().await.take() {
            prompt.push(ContentBlock::Text { text: instructions });
        }
        prompt.push(ContentBlock::Text { text: text.into() });
        emit(
            &self.events,
            SessionEventKind::TurnStarted { turn_id: None },
        );
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
        let stop_reason = stop_reason(&response.stop_reason);
        emit(
            &self.events,
            SessionEventKind::TurnCompleted {
                turn_id: None,
                stop_reason,
            },
        );
        Ok(TurnOutcome {
            stop_reason,
            turn_id: None,
        })
    }

    async fn interrupt(&self) -> Result<(), CoreError> {
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

/// The mode that lets the agent act without approvals or sandbox, by the names agents use:
/// Claude Code `bypassPermissions`, Codex `agent-full-access`, others `yolo`.
fn bypass_mode(modes: &[SessionMode]) -> Option<&SessionMode> {
    const KEYWORDS: [&str; 4] = ["bypass", "full-access", "full_access", "yolo"];
    KEYWORDS
        .iter()
        .find_map(|keyword| modes.iter().find(|m| m.id.to_lowercase().contains(keyword)))
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
        if let Some(kind) = parse_update(&note.update) {
            let _ = self.events.send(SessionEvent::with_raw(kind, note.update));
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

fn parse_update(update: &Value) -> Option<SessionEventKind> {
    let str_of = |key: &str| update.get(key).and_then(Value::as_str);
    match str_of("sessionUpdate")? {
        kind @ ("agent_message_chunk" | "agent_thought_chunk") => {
            let content = update.get("content")?;
            if content.get("type")?.as_str()? != "text" {
                return None;
            }
            Some(SessionEventKind::Output {
                stream: if kind == "agent_message_chunk" {
                    OutputStream::Text
                } else {
                    OutputStream::Thought
                },
                text: content.get("text")?.as_str()?.to_owned(),
            })
        }
        "tool_call" => Some(SessionEventKind::ToolCall {
            tool_call_id: str_of("toolCallId")?.to_owned(),
            title: str_of("title").unwrap_or_default().to_owned(),
            kind: str_of("kind").map(str::to_owned),
            status: tool_status(update.get("status")).unwrap_or(ToolStatus::Pending),
        }),
        "tool_call_update" => Some(SessionEventKind::ToolCallUpdated {
            tool_call_id: str_of("toolCallId")?.to_owned(),
            title: str_of("title").map(str::to_owned),
            status: tool_status(update.get("status")),
        }),
        "usage_update" => {
            let cost = update.get("cost");
            let cost_usd = cost
                .filter(|c| c.get("currency").and_then(Value::as_str) == Some("USD"))
                .and_then(|c| c.get("amount")?.as_f64());
            Some(SessionEventKind::Usage {
                used: update.get("used")?.as_u64()?,
                size: update.get("size")?.as_u64()?,
                cost_usd,
                tokens: None,
            })
        }
        _ => None,
    }
}

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
    fn finds_the_unrestricted_mode() {
        let modes = |ids: &[&str]| -> Vec<SessionMode> {
            ids.iter()
                .map(|id| SessionMode {
                    id: (*id).into(),
                    name: String::new(),
                })
                .collect()
        };
        let codex = modes(&["read-only", "workspace-write", "agent", "agent-full-access"]);
        assert_eq!(bypass_mode(&codex).unwrap().id, "agent-full-access");
        let claude = modes(&["default", "acceptEdits", "plan", "bypassPermissions"]);
        assert_eq!(bypass_mode(&claude).unwrap().id, "bypassPermissions");
        assert!(bypass_mode(&modes(&["default", "plan"])).is_none());
    }

    #[test]
    fn parses_updates() {
        let chunk = json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "hi" } });
        assert_eq!(
            parse_update(&chunk),
            Some(SessionEventKind::Output {
                stream: OutputStream::Text,
                text: "hi".into()
            })
        );
        let tool = json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Read x", "kind": "read", "status": "in_progress" });
        assert!(matches!(
            parse_update(&tool),
            Some(SessionEventKind::ToolCall {
                status: ToolStatus::InProgress,
                ..
            })
        ));
        let usage = json!({ "sessionUpdate": "usage_update", "used": 10, "size": 100, "cost": { "amount": 0.5, "currency": "USD" } });
        assert_eq!(
            parse_update(&usage),
            Some(SessionEventKind::Usage {
                used: 10,
                size: 100,
                cost_usd: Some(0.5),
                tokens: None,
            })
        );
        assert_eq!(
            parse_update(&json!({ "sessionUpdate": "plan", "entries": [] })),
            None
        );
    }
}
