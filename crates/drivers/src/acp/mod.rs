//! `acp-generic`: runs any ACP agent as a child process and drives it as the client.
//!
//! One process per session. The fallback driver for every agent without a native one.

pub mod wire;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use crossagents_core::{
    AttachmentKind, AttachmentSupport, Capabilities, CoreError, Driver, DriverKind, DriverSession,
    FileChange, McpServerSpec, OutputStream, PermissionPolicy, Prompt, SessionEvent,
    SessionEventKind, SessionSpec, StopReason, ToolDetail, ToolStatus, TurnOutcome,
};
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};

use self::wire::{method, *};
use crate::jsonrpc::{RpcError, RpcHandler, RpcPeer};
use crate::process::resolve_program;

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
            // Unknown until a session says: the agent declares `promptCapabilities` at
            // `initialize` (docs/TODO.md §7).
            attachments: None,
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
    process: crate::launch::ProcessHandle,
    pending_instructions: Mutex<Option<String>>,
    events: mpsc::UnboundedSender<SessionEvent>,
    /// Set before we kill the process, so its exit is not reported as a failure.
    shutting_down: Arc<AtomicBool>,
    /// What the agent said it takes at `initialize` (docs/TODO.md §7).
    attachments: AttachmentSupport,
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

        let launched = crate::launch::launch(crate::launch::LaunchSpec {
            program: resolve_program(&launch.program),
            args: launch.args.clone(),
            env: launch.env.clone(),
            cwd: launch.cwd.clone(),
            owner: None,
            label: launch.program.clone(),
            ..crate::launch::LaunchSpec::default()
        })
        .await?;
        let crate::launch::Launched {
            stdin,
            stdout,
            stderr: tail,
            process,
        } = launched;

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
            let exit = process.watch();
            async move {
                let _ = reader.await;
                if !shutting_down.load(Ordering::SeqCst) {
                    let info = exit.within(Duration::from_secs(2)).await;
                    let what = match info {
                        Some(info) => info.describe("agent process"),
                        None => "agent process exited".to_owned(),
                    };
                    let message = tail.describe(&what).await;
                    emit(
                        &events,
                        SessionEventKind::Exited {
                            message,
                            cause: info.map(|i| i.cause),
                            code: info.and_then(|i| i.code),
                        },
                    );
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
            process,
            pending_instructions: Mutex::new(instructions),
            events: events.clone(),
            shutting_down,
            // PDFs would need `embeddedContext` and a resource block, which is not wired;
            // only images are sent as such.
            attachments: AttachmentSupport {
                image: init.agent_capabilities.prompt_capabilities.image,
                pdf: false,
                text: true,
            },
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

impl AcpSession {
    /// The prompt blocks for one turn: instructions once, the message, then each file as
    /// what the agent takes (docs/TODO.md §7).
    async fn prompt_blocks(&self, prompt: &Prompt) -> Vec<ContentBlock> {
        let mut blocks = Vec::new();
        if let Some(instructions) = self.pending_instructions.lock().await.take() {
            blocks.push(ContentBlock::Text { text: instructions });
        }
        blocks.push(ContentBlock::Text {
            text: prompt.text.clone(),
        });
        for attachment in &prompt.attachments {
            blocks.push(match attachment.kind() {
                Some(AttachmentKind::Image) if self.attachments.image => {
                    match std::fs::read(&attachment.path) {
                        Ok(bytes) => ContentBlock::Image {
                            data: STANDARD.encode(bytes),
                            mime_type: attachment.mime.clone(),
                        },
                        // Unreadable now: named, so the agent still hears about it.
                        Err(_) => ContentBlock::Text {
                            text: attachment.reference(),
                        },
                    }
                }
                Some(AttachmentKind::Text) => match attachment.inline_text() {
                    Some(text) => ContentBlock::Text {
                        text: attachment.as_file_block(&text),
                    },
                    None => ContentBlock::Text {
                        text: attachment.reference(),
                    },
                },
                // A kind this agent did not say it takes (or one this client does not
                // send as such): named, never dropped.
                _ => ContentBlock::Text {
                    text: attachment.reference(),
                },
            });
        }
        blocks
    }

    /// Sends one turn's blocks and waits for it to end.
    async fn run_prompt(&self, prompt: Vec<ContentBlock>) -> Result<TurnOutcome, CoreError> {
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
}

#[async_trait]
impl DriverSession for AcpSession {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    async fn prompt(&self, text: &str) -> Result<TurnOutcome, CoreError> {
        self.run_prompt(self.prompt_blocks(&Prompt::text(text)).await)
            .await
    }

    /// Images go as ACP image blocks when the agent declared them; text is inlined or
    /// named by path (docs/TODO.md §7).
    async fn prompt_with(&self, prompt: &Prompt) -> Result<TurnOutcome, CoreError> {
        self.run_prompt(self.prompt_blocks(prompt).await).await
    }

    fn attachment_support(&self) -> Option<AttachmentSupport> {
        Some(self.attachments)
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
        self.process.start_kill();
        let _ = self.process.exit_within(Duration::from_secs(5)).await;
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
        let detail = parse_detail(&note.update);
        if let Some(kind) = parse_update(&note.update) {
            let _ = self
                .events
                .send(SessionEvent::with_raw(kind, note.update.clone()));
        }
        if let Some(kind) = detail {
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
        // ACP agents do not reach the hub's guard yet: guarded ones are let through.
        PermissionPolicy::Bypass | PermissionPolicy::Guard => pick(&["allow_always", "allow_once"]),
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

/// What a tool call did, from an ACP `tool_call` / `tool_call_update`: its text content
/// as output, its diffs as file changes, `rawInput` / `rawOutput` as arguments and result.
fn parse_detail(update: &Value) -> Option<SessionEventKind> {
    let kind = update.get("sessionUpdate")?.as_str()?;
    if kind != "tool_call" && kind != "tool_call_update" {
        return None;
    }
    let id = update.get("toolCallId")?.as_str()?.to_owned();
    let mut detail = ToolDetail::default();
    let mut texts = Vec::new();
    for item in update
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match item.get("type").and_then(Value::as_str) {
            Some("content") => {
                if let Some(text) = item.pointer("/content/text").and_then(Value::as_str) {
                    texts.push(text.to_owned());
                }
            }
            Some("diff") => {
                let Some(path) = item.get("path").and_then(Value::as_str) else {
                    continue;
                };
                let old = item.get("oldText").and_then(Value::as_str);
                let new = item
                    .get("newText")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                detail.changes.push(FileChange {
                    path: path.to_owned(),
                    kind: if old.is_none() { "add" } else { "update" }.to_owned(),
                    diff: unified_diff(old.unwrap_or_default(), new),
                });
            }
            _ => {}
        }
    }
    if !texts.is_empty() {
        detail.output = Some(texts.join("\n"));
    }
    let raw_input = update.get("rawInput").filter(|v| !v.is_null());
    if let Some(input) = raw_input {
        // Shell tools put the command in their input.
        detail.command = match input.get("command") {
            Some(Value::String(c)) => Some(c.clone()),
            Some(Value::Array(parts)) => Some(
                parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => None,
        };
        detail.arguments = Some(serde_json::to_string_pretty(input).unwrap_or_default());
    }
    if let Some(output) = update.get("rawOutput").filter(|v| !v.is_null()) {
        detail.result = Some(match output.as_str() {
            Some(s) => s.to_owned(),
            None => serde_json::to_string_pretty(output).unwrap_or_default(),
        });
    }
    (detail != ToolDetail::default()).then_some(SessionEventKind::ToolDetail {
        tool_call_id: id,
        detail,
    })
}

/// Lines compared up to this many cells; bigger files are shown as replaced whole.
const DIFF_CELLS: usize = 4_000_000;

/// A one-hunk unified diff of `old` → `new` (longest common subsequence of lines).
fn unified_diff(old: &str, new: &str) -> String {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let mut lines: Vec<(char, &str)> = Vec::new();
    if a.len() * b.len() <= DIFF_CELLS {
        // lcs[i][j]: common lines of a[i..] and b[j..].
        let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                lcs[i][j] = if a[i] == b[j] {
                    lcs[i + 1][j + 1] + 1
                } else {
                    lcs[i + 1][j].max(lcs[i][j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < a.len() || j < b.len() {
            if i < a.len() && j < b.len() && a[i] == b[j] {
                lines.push((' ', a[i]));
                i += 1;
                j += 1;
            } else if j < b.len() && (i == a.len() || lcs[i][j + 1] > lcs[i + 1][j]) {
                lines.push(('+', b[j]));
                j += 1;
            } else {
                lines.push(('-', a[i]));
                i += 1;
            }
        }
    } else {
        lines.extend(a.iter().map(|l| ('-', *l)));
        lines.extend(b.iter().map(|l| ('+', *l)));
    }
    let mut out = format!("@@ -1,{} +1,{} @@\n", a.len(), b.len());
    for (sign, line) in lines {
        out.push(sign);
        out.push_str(line);
        out.push('\n');
    }
    out
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

    #[test]
    fn tool_details_from_content_and_raw_io() {
        // As Claude Code's ACP adapter reports an edit and a shell command.
        let edit = json!({ "sessionUpdate": "tool_call_update", "toolCallId": "e1", "status": "completed",
            "content": [{ "type": "diff", "path": "/w/a.txt", "oldText": "one\ntwo\n", "newText": "one\nthree\n" }] });
        let Some(SessionEventKind::ToolDetail { detail, .. }) = parse_detail(&edit) else {
            panic!("no detail")
        };
        assert_eq!(detail.changes[0].kind, "update");
        assert_eq!(
            detail.changes[0].diff,
            "@@ -1,2 +1,2 @@\n one\n-two\n+three\n"
        );
        let shell = json!({ "sessionUpdate": "tool_call", "toolCallId": "s1", "title": "ls", "kind": "execute",
            "rawInput": { "command": "ls -la" },
            "content": [{ "type": "content", "content": { "type": "text", "text": "a.txt" } }] });
        let Some(SessionEventKind::ToolDetail { detail, .. }) = parse_detail(&shell) else {
            panic!("no detail")
        };
        assert_eq!(detail.command.as_deref(), Some("ls -la"));
        assert_eq!(detail.output.as_deref(), Some("a.txt"));
        assert!(
            parse_detail(&json!({ "sessionUpdate": "tool_call", "toolCallId": "x" })).is_none()
        );
    }
}
