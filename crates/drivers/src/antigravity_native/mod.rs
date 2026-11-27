//! `antigravity-native`: Google Antigravity CLI's headless stream-json
//! (docs/research/antigravity-cli.md).
//!
//! One process per session: `agy --input-format stream-json --output-format stream-json`
//! stays up and takes one `user` message per turn on stdin, answering in NDJSON. That is
//! cheaper than restarting it per turn, and the process holds the conversation; when it is
//! gone (an interrupt, a crash), the next turn starts a new one with
//! `--conversation <id>`, which is also what [`Driver::resume`] does.
//! Image turns use `agy acp` with the same conversation id because headless input
//! supports only text blocks.
//!
//! - The conversation id is the CLI's: it arrives in `init`/`result`, and the session
//!   reports it as [`SessionEventKind::Started`] once it has it. A new session therefore
//!   has a provisional id until its first turn.
//! - Headless mode has no approval channel, but it soft-denies instead of failing: a tool
//!   that would ask is refused, the turn carries on and exits 0. A bypassing session runs
//!   `--dangerously-skip-permissions`, a guarding one `--mode accept-edits`, a denying one
//!   leaves the default mode (everything that asks is refused).
//! - Collaboration MCP servers cannot be mounted: the CLI reads `mcp_config.json` from
//!   `~/.gemini/config/` only (`agy mcp add` writes there, for every session), and
//!   `--settings` rejects `mcpServers`. The session says so rather than pretending.

pub mod login;
pub mod wire;

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crossagents_core::{
    AttachmentSupport, Capabilities, CliStatus, CoreError, Driver, DriverKind, DriverSession,
    OutputStream, PermissionPolicy, Prompt, SessionEvent, SessionEventKind, SessionSpec,
    StopReason, TokenUsage, ToolDetail, ToolStatus, TurnOutcome,
};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Notify, mpsc};

use crate::acp::{AcpLaunch, AcpOptions, AcpSession, ImageTurnOptions};
use crate::launch::{BoxWrite, LaunchSpec, Launched, ProcessHandle};
use crate::locate::Cli;
use crate::process::{StderrTail, hide_console, resolve_program, system_proxy_env};
use wire::{Event, Step};

const ATTACHMENTS: AttachmentSupport = AttachmentSupport {
    image: true,
    pdf: false,
    text: true,
};

/// How long `agy models` gets.
const MODEL_LIST_TIMEOUT: Duration = Duration::from_secs(20);
/// How long the process gets to exit once its output has ended.
const EXIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Windows' `agy` updater launches its own visible `agy --version` console.
/// Skip that updater for background queries while still letting the query run.
pub fn prepare_query(command: &mut Command) {
    #[cfg(windows)]
    command.env("AGY_CLI_DISABLE_AUTO_UPDATE", "true");
    hide_console(command);
}

/// Canonical order for Antigravity `--effort` levels.
const EFFORT_ORDER: &[&str] = &["low", "medium", "high", "xhigh", "max"];

fn effort_unsupported_model(model: &str) -> bool {
    let lower = model.trim().to_ascii_lowercase();
    lower == "auto"
        || lower == "gemini-pro-orchestrator"
        || lower.starts_with("gemini-pro-orchestrator-")
}

/// Effort levels `listed` offers as suffixed variants of a bare base model, in canonical order.
fn listed_efforts_for_base(listed: &[String], base: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for slug in listed {
        if let Some((slug_base, effort)) = crate::acp::wire::split_effort_suffix(slug)
            && slug_base.eq_ignore_ascii_case(base)
        {
            let effort = effort.to_ascii_lowercase();
            if !found.iter().any(|existing| existing == &effort) {
                found.push(effort);
            }
        }
    }
    found.sort_by_key(|e| {
        EFFORT_ORDER
            .iter()
            .position(|k| *k == e.as_str())
            .unwrap_or(EFFORT_ORDER.len())
    });
    found
}

/// Resolves `(model, effort)` into CLI flags `agy` accepts without conflict:
/// - `agy models` lists effort-suffixed slugs (`gemini-4-neon-xhigh`) whose effort is already
///   encoded in `--model`; passing any conflicting `--effort` fails with
///   `--model <slug> conflicts with --effort=<level>`.
/// - `auto` and `gemini-pro-orchestrator*` reject `--effort`.
/// - Bare base models (`gemini-4-neon`) only accept the effort levels their listed variants
///   support (e.g. `gemini-4-neon` has no `max`, `claude-opus-5-5` has no `xhigh`).
pub(crate) fn resolve_model_effort(
    model: Option<&str>,
    effort: Option<&str>,
    listed: &[String],
) -> (Option<String>, Option<String>) {
    let model = model.map(str::trim).filter(|m| !m.is_empty());
    let effort = effort.map(str::trim).filter(|e| !e.is_empty());
    let Some(model) = model else {
        return (None, effort.map(str::to_owned));
    };
    let Some(effort) = effort else {
        return (Some(model.to_owned()), None);
    };
    if effort_unsupported_model(model) || crate::acp::wire::split_effort_suffix(model).is_some() {
        return (Some(model.to_owned()), None);
    }
    if !listed.is_empty() {
        let allowed = listed_efforts_for_base(listed, model);
        if !allowed.is_empty() {
            let supported = allowed.iter().any(|a| a.eq_ignore_ascii_case(effort));
            return (
                Some(model.to_owned()),
                supported.then(|| effort.to_owned()),
            );
        }
        if listed.iter().any(|s| s.eq_ignore_ascii_case(model)) {
            return (Some(model.to_owned()), None);
        }
    }
    (Some(model.to_owned()), Some(effort.to_owned()))
}

pub struct AntigravityDriver {
    name: String,
    cli: Cli,
    models: Vec<String>,
    /// What `agy models` answered, once it has.
    listed: Arc<Mutex<Vec<String>>>,
}

impl AntigravityDriver {
    pub fn new(name: impl Into<String>, cli: Cli, models: Vec<String>) -> Self {
        Self {
            name: name.into(),
            cli,
            models,
            listed: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn command(&self, spec: &SessionSpec) -> Result<Vec<String>, CoreError> {
        if let Some(command) = spec.command.clone().filter(|c| !c.is_empty()) {
            return Ok(command);
        }
        self.cli.command().ok_or_else(|| {
            CoreError::Agent(
                self.cli
                    .missing_message(&self.name)
                    .unwrap_or_else(|| format!("the {} CLI is not installed", self.name)),
            )
        })
    }

    async fn session(
        &self,
        id: String,
        spec: SessionSpec,
        resume: Option<String>,
    ) -> Result<Box<dyn DriverSession>, CoreError> {
        let command = self.command(&spec)?;
        Ok(Box::new(AntigravitySession::new(
            id,
            command,
            spec,
            resume,
            Arc::clone(&self.listed),
        )))
    }

    /// Asks the CLI which models the signed-in account may use. Best effort: the
    /// description's list stands in until it answers (and it needs a sign-in to answer).
    async fn list_models(&self) {
        let Some(command) = self.cli.command() else {
            return;
        };
        let Some((program, extra)) = command.split_first() else {
            return;
        };
        let output = if cfg!(windows) {
            let mut args: Vec<String> = extra.to_vec();
            args.push("models".into());
            let mut env = system_proxy_env();
            env.push(("AGY_CLI_DISABLE_AUTO_UPDATE".into(), "true".into()));
            let Ok(mut launched) = crate::launch::launch(LaunchSpec {
                program: resolve_program(program),
                args,
                cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                env,
                label: "agy models".into(),
                ..LaunchSpec::default()
            })
            .await
            else {
                tracing::debug!("agy models failed to start");
                return;
            };
            drop(launched.stdin);
            let mut buf = Vec::new();
            let read = tokio::time::timeout(
                MODEL_LIST_TIMEOUT,
                tokio::io::AsyncReadExt::read_to_end(&mut launched.stdout, &mut buf),
            )
            .await;
            launched.process.start_kill();
            if read.is_err() {
                tracing::debug!("agy models did not answer");
                return;
            }
            buf
        } else {
            let mut child = Command::new(resolve_program(program));
            child
                .args(extra)
                .arg("models")
                .envs(system_proxy_env())
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            prepare_query(&mut child);
            let Ok(Ok(output)) = tokio::time::timeout(MODEL_LIST_TIMEOUT, child.output()).await
            else {
                tracing::debug!("agy models did not answer");
                return;
            };
            output.stdout
        };
        let models = wire::parse_models(&String::from_utf8_lossy(&output));
        if !models.is_empty() {
            *self.listed.lock().expect("model list") = models;
        }
    }
}

#[async_trait]
impl Driver for AntigravityDriver {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> DriverKind {
        DriverKind::AntigravityNative
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            interrupt: true,
            resume: true,
            set_model: true,
            usage: true,
            attachments: Some(ATTACHMENTS),
            // Conversations are stored per project directory.
            move_cwd: false,
            ..Capabilities::default()
        }
    }

    fn models(&self) -> Vec<String> {
        let listed = self.listed.lock().expect("model list");
        if !listed.is_empty() {
            return listed.clone();
        }
        drop(listed);
        self.models.clone()
    }

    fn model_efforts(&self, model: &str) -> Option<Vec<String>> {
        let model = model.trim();
        if model.is_empty() {
            return None;
        }
        // `agy models` returns effort-suffixed slugs (`gemini-4-neon-xhigh`) whose effort is
        // already encoded in `--model`, and `auto` / orchestrator models reject `--effort`.
        // Reporting `Some(vec![])` keeps pickers from offering a conflicting `--effort` flag.
        if effort_unsupported_model(model)
            || crate::acp::wire::split_effort_suffix(model).is_some()
        {
            return Some(Vec::new());
        }
        let listed = self.listed.lock().expect("model list");
        if listed.is_empty() {
            return None;
        }
        let levels = listed_efforts_for_base(&listed, model);
        if !levels.is_empty() {
            return Some(levels);
        }
        if listed.iter().any(|s| s.eq_ignore_ascii_case(model)) {
            return Some(Vec::new());
        }
        None
    }

    fn installed(&self) -> bool {
        self.cli.installed()
    }

    async fn cli_status(&self) -> Option<CliStatus> {
        Some(self.cli.status().await)
    }

    /// Asked at hub startup, in the background: the version, then the model list.
    async fn probe_cli(&self) {
        let _ = self.cli.version().await;
        self.list_models().await;
    }

    fn refresh_cli(&self) {
        self.cli.forget();
    }

    fn cli_places(&self) -> Vec<String> {
        self.cli.searched()
    }

    async fn open(&self, spec: SessionSpec) -> Result<Box<dyn DriverSession>, CoreError> {
        self.session(uuid::Uuid::new_v4().to_string(), spec, None)
            .await
    }

    async fn resume(
        &self,
        session_id: &str,
        spec: SessionSpec,
    ) -> Result<Box<dyn DriverSession>, CoreError> {
        self.session(session_id.to_owned(), spec, Some(session_id.to_owned()))
            .await
    }
}

/// The live CLI process of a session.
struct Live {
    stdin: BoxWrite,
    lines: mpsc::UnboundedReceiver<String>,
    process: Arc<ProcessHandle>,
    stderr: StderrTail,
}

/// Best-known context window (in tokens) for an Antigravity model slug, or `None` when the
/// model family is unknown so the UI does not fabricate a percentage.
pub fn context_window_for(model: Option<&str>) -> Option<u64> {
    let m = model?.trim().to_ascii_lowercase();
    if m.is_empty() || m == "auto" {
        return None;
    }
    if m.contains("gemini") {
        return Some(1_000_000);
    }
    if m.contains("claude") {
        return Some(if m.contains("1m") { 1_000_000 } else { 200_000 });
    }
    if m.starts_with("gpt-4.1") {
        return Some(1_000_000);
    }
    if m.starts_with("gpt-5") || m.contains("codex") {
        return Some(272_000);
    }
    if m.starts_with("o3") || m.starts_with("o4") || m.starts_with("gpt-4") {
        return Some(200_000);
    }
    None
}

pub struct AntigravitySession {
    id: String,
    /// The CLI's conversation id, once it has said it.
    conversation: Mutex<Option<String>>,
    started: AtomicBool,
    command: Vec<String>,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    model: Mutex<Option<String>>,
    /// The concrete model the CLI reported in `init` (resolves `"auto"`).
    resolved_model: Mutex<Option<String>>,
    effort: Mutex<Option<String>>,
    /// Shared catalog from `agy models`, used to avoid passing unsupported `--effort`.
    listed: Arc<Mutex<Vec<String>>>,
    permission: PermissionPolicy,
    instructions: Mutex<Option<String>>,
    tokens: Mutex<TokenUsage>,
    /// Single-step prompt/context size from the latest `step_update.usage`.
    context_used: std::sync::atomic::AtomicU64,
    /// Tool calls already announced, so a `DONE` step updates instead of adding another.
    tools: Mutex<HashSet<String>>,
    events: mpsc::UnboundedSender<SessionEvent>,
    turn: tokio::sync::Mutex<()>,
    child: tokio::sync::Mutex<Option<Live>>,
    running: Mutex<Option<Arc<ProcessHandle>>>,
    cancelled: AtomicBool,
    interrupt_image: Notify,
    closed: AtomicBool,
}

impl AntigravitySession {
    fn new(
        id: String,
        command: Vec<String>,
        spec: SessionSpec,
        resume: Option<String>,
        listed: Arc<Mutex<Vec<String>>>,
    ) -> Self {
        if spec.permissions.is_some() {
            let _ = spec.events.send(
                SessionEventKind::Warning {
                    message: "Antigravity CLI headless has no per-action approval channel: the session's permission policy decides for the whole run (bypass runs --dangerously-skip-permissions, guard accepts edits, deny keeps the default mode where everything that asks is refused).".into(),
                }
                .into(),
            );
        }
        if !spec.mcp_servers.is_empty() {
            let _ = spec.events.send(
                SessionEventKind::Warning {
                    message: "Antigravity CLI has no per-session MCP configuration: it reads ~/.gemini/config/mcp_config.json for every session, and --settings rejects mcpServers. The collaboration tools were not mounted for this session (`agy mcp add` adds them everywhere instead).".into(),
                }
                .into(),
            );
        }
        let started = resume.is_some();
        if let Some(id) = &resume {
            let _ = spec.events.send(
                SessionEventKind::Started {
                    session_id: id.clone(),
                }
                .into(),
            );
        }
        let initial_effort = spec
            .agent
            .config
            .get("effort")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(str::to_owned);
        Self {
            id,
            conversation: Mutex::new(resume),
            started: AtomicBool::new(started),
            command,
            cwd: spec.agent.cwd,
            env: spec.env,
            model: Mutex::new(spec.agent.model.filter(|m| !m.is_empty())),
            resolved_model: Mutex::new(None),
            effort: Mutex::new(initial_effort),
            listed,
            permission: spec.permission,
            instructions: Mutex::new(spec.instructions),
            tokens: Mutex::new(TokenUsage::default()),
            context_used: std::sync::atomic::AtomicU64::new(0),
            tools: Mutex::new(HashSet::new()),
            events: spec.events,
            turn: tokio::sync::Mutex::new(()),
            child: tokio::sync::Mutex::new(None),
            running: Mutex::new(None),
            cancelled: AtomicBool::new(false),
            interrupt_image: Notify::new(),
            closed: AtomicBool::new(false),
        }
    }

    fn emit(&self, kind: SessionEventKind) {
        let _ = self.events.send(kind.into());
    }

    /// The headless message includes the opening instructions once and references to text files.
    fn prompt_text(&self, prompt: &Prompt) -> String {
        let mut text = match self.instructions.lock().expect("instructions").take() {
            Some(instructions) => format!("{instructions}\n\n{}", prompt.text),
            None => prompt.text.clone(),
        };
        for attachment in &prompt.attachments {
            text.push_str("\n\n");
            text.push_str(&attachment.reference());
        }
        text
    }

    /// Remembers the CLI's conversation id, reporting it as the session's once.
    fn remember(&self, id: &str) {
        if id.is_empty() {
            return;
        }
        let mut conversation = self.conversation.lock().expect("conversation");
        if conversation.as_deref() == Some(id) {
            return;
        }
        let resumed = conversation.is_some();
        *conversation = Some(id.to_owned());
        drop(conversation);
        if resumed {
            self.emit(SessionEventKind::Warning {
                message: format!(
                    "Antigravity CLI reported conversation {id}, not {}; resuming this session may fail",
                    self.id
                ),
            });
            return;
        }
        if !self.started.swap(true, Ordering::SeqCst) {
            self.emit(SessionEventKind::Started {
                session_id: id.to_owned(),
            });
        }
    }

    /// Model and effort flags after resolving conflicts with effort-suffixed slugs and
    /// per-model support.
    fn effective_model_and_effort(&self) -> (Option<String>, Option<String>) {
        let model = self.model.lock().expect("model").clone();
        let effort = self.effort.lock().expect("effort").clone();
        let listed = self.listed.lock().expect("model list");
        resolve_model_effort(model.as_deref(), effort.as_deref(), &listed)
    }

    /// The arguments a process is started with; the conversation id makes a restarted
    /// process carry on where the last one stopped.
    fn args(&self) -> Vec<String> {
        let mut args = vec![
            "--input-format".to_owned(),
            "stream-json".to_owned(),
            "--output-format".to_owned(),
            "stream-json".to_owned(),
        ];
        if let Some(id) = self
            .conversation
            .lock()
            .expect("conversation")
            .clone()
            .filter(|id| !id.is_empty())
        {
            args.push("--conversation".into());
            args.push(id);
        }
        let (model, effort) = self.effective_model_and_effort();
        if let Some(model) = model {
            args.push("--model".into());
            args.push(model);
        }
        if let Some(effort) = effort {
            args.push("--effort".into());
            args.push(effort);
        }
        match self.permission {
            PermissionPolicy::Bypass => args.push("--dangerously-skip-permissions".into()),
            PermissionPolicy::Guard => {
                args.push("--mode".into());
                args.push("accept-edits".into());
            }
            // The default mode asks, and headless refuses what it cannot ask: the turn goes
            // on without the tool.
            PermissionPolicy::Deny => {}
        }
        args
    }

    /// Starts the process when there is none.
    async fn ensure(&self) -> Result<(), CoreError> {
        let mut child = self.child.lock().await;
        if child.is_some() {
            return Ok(());
        }
        let args = self.args();
        let (program, all_args) = crate::argv::launch_command(&self.command, args)
            .ok_or_else(|| CoreError::Agent("empty Antigravity launch command".into()))?;
        let mut env = system_proxy_env();
        env.extend(self.env.iter().cloned());
        #[cfg(windows)]
        env.push(("AGY_CLI_DISABLE_AUTO_UPDATE".into(), "true".into()));
        let launched = crate::launch::launch(LaunchSpec {
            program,
            args: all_args,
            cwd: self.cwd.clone(),
            env,
            label: "Antigravity CLI".into(),
            ..LaunchSpec::default()
        })
        .await?;
        let Launched {
            stdin,
            stdout,
            stderr,
            process,
        } = launched;
        let (lines_tx, lines) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut lines_in = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines_in.next_line().await {
                if lines_tx.send(line).is_err() {
                    break;
                }
            }
        });
        let process = Arc::new(process);
        *self.running.lock().expect("running") = Some(process.clone());
        *child = Some(Live {
            stdin,
            lines,
            process,
            stderr,
        });
        Ok(())
    }

    /// One answer fragment, or `None` for steps that say nothing.
    fn on_step(&self, step: &Step) {
        if let Some(id) = &step.conversation_id {
            self.remember(id);
        }
        if let Some(usage) = step.usage {
            let step_ctx = if usage.input > 0 {
                usage.input.saturating_add(usage.output)
            } else {
                usage.total
            };
            if step_ctx > 0 {
                self.context_used.store(step_ctx, Ordering::Relaxed);
            }
        }
        if step.is_answer() {
            if let Some(delta) = step.text_delta.as_deref().filter(|d| !d.is_empty()) {
                self.emit(SessionEventKind::Output {
                    stream: OutputStream::Text,
                    text: delta.to_owned(),
                });
            }
            return;
        }
        if !step.is_tool() {
            return;
        }
        let id = format!("step-{}", step.step_index.unwrap_or(0));
        let info = step.tool_info.as_ref();
        let name = step
            .tool_name
            .clone()
            .or_else(|| info.and_then(|i| i.name.clone()))
            .unwrap_or_else(|| "tool".to_owned());
        let kind = tool_kind(&name);
        let title = tool_title(&name, info);
        let detail = tool_detail(&name, step);
        if step.running() {
            let first_sight = self.tools.lock().expect("tools").insert(id.clone());
            if first_sight {
                self.emit(SessionEventKind::ToolCall {
                    tool_call_id: id.clone(),
                    title,
                    kind: Some(kind.to_owned()),
                    status: ToolStatus::InProgress,
                });
            } else {
                self.emit(SessionEventKind::ToolCallUpdated {
                    tool_call_id: id.clone(),
                    title: (title != name).then_some(title),
                    status: None,
                });
            }
            if detail != ToolDetail::default() {
                self.emit(SessionEventKind::ToolDetail {
                    tool_call_id: id,
                    detail,
                });
            }
            return;
        }
        let failed = info.and_then(|i| i.error.as_ref()).is_some()
            || detail.exit_code.is_some_and(|code| code != 0);
        let status = if failed {
            ToolStatus::Failed
        } else {
            ToolStatus::Completed
        };
        let first_sight = self.tools.lock().expect("tools").insert(id.clone());
        if first_sight {
            self.emit(SessionEventKind::ToolCall {
                tool_call_id: id.clone(),
                title,
                kind: Some(kind.to_owned()),
                status,
            });
        } else {
            self.emit(SessionEventKind::ToolCallUpdated {
                tool_call_id: id.clone(),
                title: (title != name).then_some(title),
                status: Some(status),
            });
        }
        if detail != ToolDetail::default() {
            self.emit(SessionEventKind::ToolDetail {
                tool_call_id: id,
                detail,
            });
        }
    }

    fn context_window(&self) -> u64 {
        let resolved = self.resolved_model.lock().expect("resolved model").clone();
        let configured = self.model.lock().expect("model").clone();
        context_window_for(resolved.as_deref())
            .or_else(|| context_window_for(configured.as_deref()))
            .unwrap_or(0)
    }

    /// The `result` event: the conversation, the answer when it never streamed, and the
    /// session's cumulative token counts.
    fn on_result(&self, result: &wire::Result) {
        if let Some(id) = &result.conversation_id {
            self.remember(id);
        }
        if let Some(usage) = result.usage {
            let tokens = {
                let mut totals = self.tokens.lock().expect("tokens");
                totals.input = usage.input;
                totals.cached_input = usage.cache_read.min(usage.input);
                totals.output = usage.output;
                totals.reasoning_output = usage.thinking;
                totals.total = usage.total;
                totals.clone()
            };
            let step_ctx = self.context_used.load(Ordering::Relaxed);
            let used = if step_ctx > 0 {
                step_ctx
            } else if usage.input > 0 {
                usage.input.saturating_add(usage.output)
            } else {
                usage.total
            };
            self.emit(SessionEventKind::Usage {
                used,
                size: self.context_window(),
                cost_usd: None,
                tokens: Some(tokens),
            });
        }
    }

    async fn run(&self, prompt: &Prompt) -> Result<TurnOutcome, CoreError> {
        let _guard = self.turn.lock().await;
        if self.closed.load(Ordering::SeqCst) {
            return Err(CoreError::Agent("the session has ended".into()));
        }
        self.cancelled.store(false, Ordering::SeqCst);
        self.tools.lock().expect("tools").clear();
        if prompt
            .attachments
            .iter()
            .any(|attachment| attachment.is_image())
        {
            return self.run_image_turn(prompt).await;
        }
        let text = self.prompt_text(prompt);
        if let Err(err) = self.ensure().await {
            self.emit(SessionEventKind::Error {
                message: err.to_string(),
            });
            self.emit(SessionEventKind::TurnCompleted {
                turn_id: None,
                stop_reason: StopReason::Error,
            });
            return Err(err);
        }
        self.emit(SessionEventKind::TurnStarted { turn_id: None });
        let message = format!(
            "{}\n",
            json!({ "event": "user", "message": { "content": text } })
        );
        let mut child = self.child.lock().await;
        let Some(live) = child.as_mut() else {
            return Err(CoreError::Agent("Antigravity CLI is not running".into()));
        };
        if let Err(err) = live.stdin.write_all(message.as_bytes()).await {
            self.emit(SessionEventKind::Error {
                message: format!("cannot send the turn to Antigravity CLI: {err}"),
            });
            *child = None;
            self.emit(SessionEventKind::TurnCompleted {
                turn_id: None,
                stop_reason: StopReason::Error,
            });
            return Ok(TurnOutcome {
                stop_reason: StopReason::Error,
                turn_id: None,
            });
        }
        let _ = live.stdin.flush().await;
        let mut saw_result = false;
        let mut ended = false;
        let mut stop: Option<StopReason> = None;
        let mut streamed = false;
        loop {
            let Some(line) = live.lines.recv().await else {
                ended = true;
                break;
            };
            match wire::parse(&line) {
                Some(Event::Init(init)) => {
                    if let Some(id) = &init.conversation_id {
                        self.remember(id);
                    }
                    if let Some(model) = &init.model {
                        *self.resolved_model.lock().expect("resolved model") = Some(model.clone());
                    }
                }
                Some(Event::Step(step)) => {
                    if step.is_answer() && step.text_delta.as_deref().is_some_and(|d| !d.is_empty())
                    {
                        streamed = true;
                    }
                    self.on_step(&step);
                }
                Some(Event::Result(result)) => {
                    saw_result = true;
                    self.on_result(&result);
                    if !streamed && !result.response.is_empty() {
                        self.emit(SessionEventKind::Output {
                            stream: OutputStream::Text,
                            text: result.response.clone(),
                        });
                    }
                    stop = Some(self.stop_reason(&result));
                    break;
                }
                _ => {}
            }
        }
        if !saw_result && !self.cancelled.load(Ordering::SeqCst) {
            let exit = live.process.exit_within(EXIT_TIMEOUT).await;
            let what = exit
                .map(|exit| exit.describe("Antigravity CLI"))
                .unwrap_or_else(|| "Antigravity CLI stopped answering".into());
            let message = live
                .stderr
                .describe(&format!("{what} before the turn ended"))
                .await;
            self.emit(SessionEventKind::Error { message });
            ended = true;
            stop = Some(StopReason::Error);
        }
        let stop_reason = stop.unwrap_or(StopReason::Cancelled);
        // A CLI that ended the run itself (a failed sign-in, say) leaves a dead process
        // behind: the next turn starts a new one, resuming the conversation by id.
        if ended
            || self.cancelled.load(Ordering::SeqCst)
            || live.lines.is_closed()
            || live.process.exit_info().is_some()
        {
            *child = None;
        }
        *self.running.lock().expect("running") = None;
        self.emit(SessionEventKind::TurnCompleted {
            turn_id: None,
            stop_reason,
        });
        Ok(TurnOutcome {
            stop_reason,
            turn_id: None,
        })
    }

    async fn run_image_turn(&self, prompt: &Prompt) -> Result<TurnOutcome, CoreError> {
        let cancelled = || TurnOutcome {
            stop_reason: StopReason::Cancelled,
            turn_id: None,
        };
        let interrupted = self.interrupt_image.notified();
        tokio::pin!(interrupted);
        if self.cancelled.load(Ordering::SeqCst) || self.closed.load(Ordering::SeqCst) {
            return Ok(cancelled());
        }
        // Flush the headless process before another process opens the same conversation.
        if let Some(live) = self.child.lock().await.take() {
            drop(live.stdin);
            if live
                .process
                .exit_within(Duration::from_secs(5))
                .await
                .is_none()
            {
                live.process.start_kill();
                if live
                    .process
                    .exit_within(Duration::from_secs(5))
                    .await
                    .is_none()
                {
                    return Err(CoreError::Agent(
                        "could not stop the Antigravity headless process".into(),
                    ));
                }
            }
        }

        let mut launch = AcpLaunch::from_command(&self.command, self.cwd.clone())?;
        launch.args.push("acp".into());
        if self.permission == PermissionPolicy::Bypass {
            launch.args.push("--dangerously-skip-permissions".into());
        }
        launch.env = system_proxy_env();
        launch.env.extend(self.env.iter().cloned());
        #[cfg(windows)]
        launch
            .env
            .push(("AGY_CLI_DISABLE_AUTO_UPDATE".into(), "true".into()));

        let (effective_model, effective_effort) = self.effective_model_and_effort();
        let opts = AcpOptions {
            launch,
            mcp_servers: Vec::new(),
            // Guard accepts edits through the ACP mode. Other permission requests are declined.
            permission: if self.permission == PermissionPolicy::Bypass {
                PermissionPolicy::Bypass
            } else {
                PermissionPolicy::Deny
            },
            model: effective_model,
            instructions: self.instructions.lock().expect("instructions").clone(),
            events: self.events.clone(),
        };
        let image = ImageTurnOptions {
            resume: self.conversation.lock().expect("conversation").clone(),
            effort: effective_effort,
            mode: match self.permission {
                PermissionPolicy::Bypass => None,
                PermissionPolicy::Guard => Some("accept-edits"),
                PermissionPolicy::Deny => Some("default"),
            },
        };
        let acp = tokio::select! {
            biased;
            () = &mut interrupted => return Ok(cancelled()),
            session = AcpSession::start_for_image(opts, image) => session?,
        };
        self.remember(acp.session_id());
        let send = acp.prompt_with_images(prompt);
        tokio::pin!(send);
        let outcome = tokio::select! {
            biased;
            () = &mut interrupted => {
                let _ = acp.interrupt().await;
                Ok(cancelled())
            }
            result = &mut send => result,
        };
        if outcome
            .as_ref()
            .is_ok_and(|result| result.stop_reason != StopReason::Cancelled)
        {
            self.instructions.lock().expect("instructions").take();
        }
        acp.close_image_turn().await;
        outcome
    }

    fn stop_reason(&self, result: &wire::Result) -> StopReason {
        if self.cancelled.load(Ordering::SeqCst) {
            return StopReason::Cancelled;
        }
        match result.status.as_str() {
            "SUCCESS" => StopReason::EndTurn,
            "CANCELED" | "INTERRUPTED" => StopReason::Cancelled,
            _ => {
                let message = result
                    .error
                    .clone()
                    .filter(|e| !e.is_empty())
                    .unwrap_or_else(|| {
                        format!(
                            "Antigravity CLI ended the turn as {}",
                            if result.status.is_empty() {
                                "unknown"
                            } else {
                                &result.status
                            }
                        )
                    });
                self.emit(SessionEventKind::Error { message });
                StopReason::Error
            }
        }
    }

    fn stop(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.interrupt_image.notify_waiters();
        if let Some(process) = self.running.lock().expect("running").as_ref() {
            process.start_kill();
        }
    }
}

/// The name the GUI uses for a tool's icon.
fn tool_kind(name: &str) -> &'static str {
    match name {
        "run_command" => "execute",
        "write_file" | "edit_file" | "write_to_file" | "replace_file_content"
        | "multi_replace_file_content" => "edit",
        "read_file" | "list_directory" | "find_by_name" | "view_file" => "read",
        "search" | "grep_search" | "codebase_search" | "google_web_search" => "search",
        "web_fetch" | "browser_navigate" => "fetch",
        _ => "tool",
    }
}

fn param_str<'a>(info: Option<&'a wire::ToolInfo>, keys: &[&str]) -> Option<&'a str> {
    let params = info?.parameters.as_ref()?.as_object()?;
    keys.iter()
        .find_map(|key| params.get(*key).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn tool_title(name: &str, info: Option<&wire::ToolInfo>) -> String {
    if name == "run_command" {
        if let Some(cmd) = param_str(info, &["CommandLine", "command", "command_line"]) {
            return cmd.lines().next().unwrap_or(cmd).trim().to_owned();
        }
        if let Some(summary) = param_str(info, &["toolSummary"]) {
            return summary.to_owned();
        }
    }
    if let Some(path) = param_str(info, &["Path", "path", "TargetFile", "file_path"]) {
        match tool_kind(name) {
            "read" => return format!("Read {path}"),
            "edit" => return format!("Edit {path}"),
            _ => {}
        }
    }
    name.to_owned()
}

fn parse_exit_code(output: &str) -> Option<i32> {
    for line in output.lines().take(8) {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Command exited with code ") {
            let digits = rest.trim_end_matches('.');
            if let Ok(code) = digits.parse::<i32>() {
                return Some(code);
            }
        }
    }
    None
}

fn tool_detail(name: &str, step: &wire::Step) -> ToolDetail {
    let info = step.tool_info.as_ref();
    let duration_ms = step
        .duration_seconds
        .filter(|s| s.is_finite() && *s >= 0.0)
        .map(|s| (s * 1000.0).round() as u64);
    if name == "run_command" {
        let command = param_str(info, &["CommandLine", "command", "command_line"]).map(str::to_owned);
        let output = info.and_then(|i| i.output.clone());
        let exit_code = output.as_deref().and_then(parse_exit_code);
        return ToolDetail {
            command,
            exit_code,
            duration_ms,
            output,
            error: info.and_then(|i| i.error.clone()),
            ..ToolDetail::default()
        };
    }
    ToolDetail {
        duration_ms,
        arguments: info
            .and_then(|i| i.parameters.as_ref())
            .map(|parameters| parameters.to_string()),
        result: info.and_then(|i| i.output.clone()),
        error: info.and_then(|i| i.error.clone()),
        ..ToolDetail::default()
    }
}

#[async_trait]
impl DriverSession for AntigravitySession {
    fn session_id(&self) -> &str {
        &self.id
    }

    async fn prompt(&self, text: &str) -> Result<TurnOutcome, CoreError> {
        self.run(&Prompt::text(text)).await
    }

    async fn prompt_with(&self, prompt: &Prompt) -> Result<TurnOutcome, CoreError> {
        self.run(prompt).await
    }

    fn attachment_support(&self) -> Option<AttachmentSupport> {
        Some(ATTACHMENTS)
    }

    async fn interrupt(&self) -> Result<(), CoreError> {
        self.stop();
        Ok(())
    }

    async fn suspend(&self) -> Result<(), CoreError> {
        if self.turn.try_lock().is_err() {
            return Err(CoreError::Agent("a turn is running".into()));
        }
        Ok(())
    }

    async fn set_model(&self, model: Option<&str>, effort: Option<&str>) -> Result<(), CoreError> {
        if let Some(model) = model {
            *self.model.lock().expect("model") =
                Some(model.to_owned()).filter(|m: &String| !m.is_empty());
        }
        if let Some(effort) = effort {
            *self.effort.lock().expect("effort") =
                Some(effort.to_owned()).filter(|e: &String| !e.is_empty());
        }
        // The flags are read when the process starts, so the next turn starts a new one.
        // Between turns nothing runs; the conversation is resumed by id.
        let mut child = self.child.lock().await;
        if let Some(live) = child.take() {
            live.process.start_kill();
        }
        Ok(())
    }

    async fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.stop();
        let _guard = self.turn.lock().await;
        *self.child.lock().await = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossagents_core::{Agent, AgentId, AgentStatus, Role};
    use std::path::Path;

    fn spec(cwd: &Path) -> (SessionSpec, mpsc::UnboundedReceiver<SessionEvent>) {
        let (events, rx) = mpsc::unbounded_channel();
        (
            SessionSpec {
                agent: Agent {
                    node: None,
                    id: AgentId::new(),
                    name: "Antigravity".into(),
                    name_source: Default::default(),
                    plugin: "antigravity".into(),
                    driver: None,
                    model: Some("gemini-3.1-pro".into()),
                    role: Role::Worker,
                    home: cwd.into(),
                    cwd: cwd.into(),
                    config: json!({}),
                    status: AgentStatus::Stopped,
                    created_at: chrono::Utc::now(),
                    profile: Default::default(),
                    topic: None,
                    archived_at: None,
                },
                mcp_servers: vec![],
                permission: PermissionPolicy::Bypass,
                permissions: None,
                command: None,
                instructions: None,
                env: vec![],
                events,
            },
            rx,
        )
    }

    /// A mock CLI: logs its argv and every message it is given, then answers each with one
    /// stream. It stays up until its stdin closes, as the real one does.
    fn mock_cli(root: &Path, windows: &str, unix: &str) -> Vec<String> {
        let path = root.join(if cfg!(windows) { "mock.ps1" } else { "mock.sh" });
        std::fs::write(&path, if cfg!(windows) { windows } else { unix }).unwrap();
        if cfg!(windows) {
            vec![
                "powershell".into(),
                "-NoProfile".into(),
                "-ExecutionPolicy".into(),
                "Bypass".into(),
                "-File".into(),
                path.display().to_string(),
            ]
        } else {
            vec!["sh".into(), path.display().to_string()]
        }
    }

    const STREAM: &str = r#"
{"event":"init","conversation_id":"conv-1","init":{"cwd":".","permission_mode":"request-review"}}
{"event":"step_update","step_update":{"conversation_id":"conv-1","step_index":1,"state":"ACTIVE","step_type":"agent_response","text_delta":"hello "}}
{"event":"step_update","step_update":{"conversation_id":"conv-1","step_index":1,"state":"DONE","step_type":"agent_response","text_delta":"world","usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}}
{"event":"step_update","step_update":{"conversation_id":"conv-1","step_index":2,"state":"ACTIVE","step_type":"tool","tool_name":"run_command","tool_info":{"name":"run_command","parameters":{"CommandLine":"echo hi"}}}}
{"event":"step_update","step_update":{"conversation_id":"conv-1","step_index":2,"state":"DONE","step_type":"tool","tool_name":"run_command","tool_info":{"name":"run_command","parameters":{"CommandLine":"echo hi"},"output":"hi\r\n"}}}
{"event":"result","result":{"conversation_id":"conv-1","status":"SUCCESS","response":"hello world","num_turns":1,"usage":{"input_tokens":10,"output_tokens":5,"thinking_tokens":1,"cache_read_tokens":2,"total_tokens":15}}}
"#;

    /// The mock's script: logs its argv and every message, then answers each with [`STREAM`]
    /// and waits for the next one. It stays up until its stdin closes, as the real CLI does.
    fn stream_for(windows: bool) -> String {
        let lines: Vec<&str> = STREAM.lines().filter(|l| !l.trim().is_empty()).collect();
        if windows {
            let mut script = String::from(
                "$ErrorActionPreference = 'Stop'\n\
                 $log = $env:CROSSAGENTS_AGY_TEST_LOG\n\
                 foreach ($a in $args) { Add-Content -Path $log -Value (\"ARG`t\" + $a) }\n\
                 while ($true) {\n\
                 \x20 $line = [Console]::In.ReadLine()\n\
                 \x20 if ($null -eq $line) { break }\n\
                 \x20 Add-Content -Path $log -Value (\"IN`t\" + $line)\n",
            );
            for line in lines {
                script.push_str(&format!("  [Console]::Out.WriteLine('{line}')\n"));
            }
            script.push_str("  [Console]::Out.Flush()\n}\n");
            script
        } else {
            let mut script = String::from(
                "log=\"$CROSSAGENTS_AGY_TEST_LOG\"\n\
                 for a in \"$@\"; do printf 'ARG\\t%s\\n' \"$a\" >> \"$log\"; done\n\
                 while IFS= read -r line; do\n\
                 \x20 printf 'IN\\t%s\\n' \"$line\" >> \"$log\"\n",
            );
            for line in lines {
                script.push_str(&format!(
                    "  printf '%s\\n' '{}'\n",
                    line.replace('\'', "'\\''")
                ));
            }
            script.push_str("done\n");
            script
        }
    }

    fn read_log(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    /// The argv the mock was started with, one entry per line it logged.
    fn logged_args(log: &str) -> Vec<String> {
        log.lines()
            .filter_map(|line| line.strip_prefix("ARG	").map(str::to_owned))
            .collect()
    }

    #[tokio::test]
    async fn an_image_turn_resumes_via_acp_and_text_returns_to_headless() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log.txt");
        let (mut spec, _) = spec(dir.path());
        spec.agent.model = None;
        spec.env = vec![("CROSSAGENTS_AGY_TEST_LOG".into(), log.display().to_string())];
        let init = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"promptCapabilities":{"image":true},"sessionCapabilities":{"resume":{},"close":{}}}}}"#;
        let resume = r#"{"jsonrpc":"2.0","id":2,"result":{}}"#;
        let prompt_result = r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#;
        let close = r#"{"jsonrpc":"2.0","id":4,"result":{}}"#;
        let windows = format!(
            r#"
if ($args[0] -eq 'acp') {{
    foreach ($response in @('{init}', '{resume}', '{prompt_result}', '{close}')) {{
        $line = [Console]::In.ReadLine()
        Add-Content -Path $env:CROSSAGENTS_AGY_TEST_LOG -Value $line
        [Console]::Out.WriteLine($response)
        [Console]::Out.Flush()
    }}
    exit
}}
{}"#,
            stream_for(true),
        );
        let unix = format!(
            r#"
if [ "$1" = acp ]; then
    for response in '{init}' '{resume}' '{prompt_result}' '{close}'; do
        IFS= read -r line || exit 1
        printf '%s\n' "$line" >> "$CROSSAGENTS_AGY_TEST_LOG"
        printf '%s\n' "$response"
    done
    exit
fi
{}"#,
            stream_for(false),
        );
        let command = mock_cli(dir.path(), &windows, &unix);
        let driver = AntigravityDriver::new("antigravity", Cli::at(command), vec![]);
        assert!(driver.capabilities().attachments.unwrap().image);
        let session = driver.open(spec).await.unwrap();
        session.prompt("before").await.unwrap();

        let path = dir.path().join("shot.png");
        let bytes = b"\x89PNG\r\n\x1a\n";
        std::fs::write(&path, bytes).unwrap();
        let image = crossagents_core::Attachment {
            id: "shot".into(),
            name: "shot.png".into(),
            mime: "image/png".into(),
            size: bytes.len() as u64,
            path,
        };
        let outcome = session
            .prompt_with(&Prompt {
                text: "look".into(),
                attachments: vec![image],
            })
            .await
            .unwrap();
        assert_eq!(outcome.stop_reason, StopReason::EndTurn);
        session.prompt("after").await.unwrap();
        session.shutdown().await;

        let log = read_log(&log);
        let requests: Vec<serde_json::Value> = log
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        assert_eq!(requests[1]["method"], "session/resume");
        assert_eq!(requests[1]["params"]["sessionId"], "conv-1");
        assert_eq!(requests[2]["method"], "session/prompt");
        assert_eq!(requests[2]["params"]["sessionId"], "conv-1");
        assert_eq!(
            requests[2]["params"]["prompt"],
            json!([
                { "type": "text", "text": "look" },
                { "type": "image", "mimeType": "image/png", "data": "iVBORw0KGgo=" }
            ])
        );
        assert_eq!(requests[3]["method"], "session/close");
        assert!(
            logged_args(&log)
                .windows(2)
                .any(|pair| pair == ["--conversation", "conv-1"])
        );
    }

    #[tokio::test]
    async fn a_turn_streams_the_answer_the_tool_and_the_totals() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log.txt");
        let (mut spec, mut events) = spec(dir.path());
        spec.instructions = Some("be brief".into());
        spec.env = vec![("CROSSAGENTS_AGY_TEST_LOG".into(), log.display().to_string())];
        let command = mock_cli(dir.path(), &stream_for(true), &stream_for(false));
        let driver = AntigravityDriver::new("antigravity", Cli::at(command), vec![]);
        let session = driver.open(spec).await.unwrap();
        assert_eq!(
            session.prompt("hi").await.unwrap().stop_reason,
            StopReason::EndTurn
        );
        assert_eq!(
            session.prompt("again").await.unwrap().stop_reason,
            StopReason::EndTurn
        );

        let mut text = String::new();
        let mut usage = None;
        let mut started = Vec::new();
        let mut tools = 0;
        while let Ok(event) = events.try_recv() {
            match event.kind {
                SessionEventKind::Started { session_id } => started.push(session_id),
                SessionEventKind::Output { text: t, .. } => text.push_str(&t),
                SessionEventKind::ToolCall { .. } => tools += 1,
                SessionEventKind::Usage { tokens, .. } => usage = tokens,
                _ => {}
            }
        }
        // The CLI's conversation id, reported once, not the provisional one.
        assert_eq!(started, vec!["conv-1".to_owned()]);
        assert_eq!(text, "hello worldhello world");
        assert_eq!(tools, 2);
        let usage = usage.expect("usage");
        assert_eq!(usage.input, 10);
        assert_eq!(usage.cached_input, 2);
        assert_eq!(usage.output, 5);
        assert_eq!(usage.reasoning_output, 1);
        assert_eq!(usage.total, 15);

        let log = read_log(&log);
        // One process for both turns: the flags are logged once.
        assert_eq!(log.matches("ARG\t--input-format").count(), 1, "{log}");
        assert!(log.contains("ARG\tstream-json"), "{log}");
        assert!(log.contains("ARG\t--model"), "{log}");
        assert!(log.contains("ARG\tgemini-3.1-pro"), "{log}");
        assert!(log.contains("ARG\t--dangerously-skip-permissions"), "{log}");
        assert!(!log.contains("ARG	--conversation"), "{log}");
        assert_eq!(log.matches("be brief").count(), 1, "{log}");
        assert_eq!(log.matches("IN\t").count(), 2, "{log}");
        session.shutdown().await;
    }

    #[tokio::test]
    async fn a_denying_session_keeps_the_default_mode() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log.txt");
        let (mut spec, _events) = spec(dir.path());
        spec.permission = PermissionPolicy::Deny;
        spec.env = vec![("CROSSAGENTS_AGY_TEST_LOG".into(), log.display().to_string())];
        let command = mock_cli(dir.path(), &stream_for(true), &stream_for(false));
        let driver = AntigravityDriver::new("antigravity", Cli::at(command), vec![]);
        let session = driver.open(spec).await.unwrap();
        session.prompt("hi").await.unwrap();
        let args = logged_args(&read_log(&log));
        assert!(!args.contains(&"--dangerously-skip-permissions".to_owned()));
        assert!(!args.contains(&"--mode".to_owned()));
        session.shutdown().await;
    }

    #[tokio::test]
    async fn a_guarding_session_accepts_edits_only() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log.txt");
        let (mut spec, _events) = spec(dir.path());
        spec.permission = PermissionPolicy::Guard;
        spec.env = vec![("CROSSAGENTS_AGY_TEST_LOG".into(), log.display().to_string())];
        let command = mock_cli(dir.path(), &stream_for(true), &stream_for(false));
        let driver = AntigravityDriver::new("antigravity", Cli::at(command), vec![]);
        let session = driver.open(spec).await.unwrap();
        session.prompt("hi").await.unwrap();
        let log = read_log(&log);
        assert!(log.contains("ARG\t--mode"), "{log}");
        assert!(log.contains("ARG\taccept-edits"), "{log}");
        assert!(!log.contains("--dangerously-skip-permissions"), "{log}");
        session.shutdown().await;
    }

    #[tokio::test]
    async fn resuming_names_the_conversation_before_the_first_turn() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log.txt");
        let (mut spec, mut events) = spec(dir.path());
        spec.env = vec![("CROSSAGENTS_AGY_TEST_LOG".into(), log.display().to_string())];
        let command = mock_cli(dir.path(), &stream_for(true), &stream_for(false));
        let driver = AntigravityDriver::new("antigravity", Cli::at(command), vec![]);
        let session = driver.resume("conv-9", spec).await.unwrap();
        assert_eq!(session.session_id(), "conv-9");
        let started = match events.try_recv().map(|e| e.kind) {
            Ok(SessionEventKind::Started { session_id }) => session_id,
            other => panic!("expected Started, got {other:?}"),
        };
        assert_eq!(started, "conv-9");
        session.prompt("hi").await.unwrap();
        let log = read_log(&log);
        assert!(log.contains("ARG\t--conversation"), "{log}");
        assert!(log.contains("ARG\tconv-9"), "{log}");
        session.shutdown().await;
    }

    #[tokio::test]
    async fn an_error_result_ends_the_turn_as_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let windows = r#"
[Console]::Out.WriteLine('{"event":"result","result":{"conversation_id":"conv-1","status":"ERROR","response":"","error":"invalid model selection","num_turns":0}}')
[Console]::Out.Flush()
$null = [Console]::In.ReadToEnd()
"#;
        let unix = r#"printf '%s\n' '{"event":"result","result":{"conversation_id":"conv-1","status":"ERROR","response":"","error":"invalid model selection","num_turns":0}}'
cat >/dev/null
"#;
        let (spec, mut events) = spec(dir.path());
        let command = mock_cli(dir.path(), windows, unix);
        let driver = AntigravityDriver::new("antigravity", Cli::at(command), vec![]);
        let session = driver.open(spec).await.unwrap();
        let outcome = session.prompt("hi").await.unwrap();
        assert_eq!(outcome.stop_reason, StopReason::Error);
        let mut error = None;
        while let Ok(event) = events.try_recv() {
            if let SessionEventKind::Error { message } = event.kind {
                error = Some(message);
            }
        }
        assert_eq!(error.as_deref(), Some("invalid model selection"));
        session.shutdown().await;
    }

    #[tokio::test]
    async fn an_interrupt_stops_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        let (spec, _events) = spec(dir.path());
        let command = mock_cli(dir.path(), "Start-Sleep -Seconds 60\n", "sleep 60\n");
        let session = Arc::new(AntigravitySession::new(
            "id-1".into(),
            command,
            spec,
            None,
            Arc::default(),
        ));
        let prompt = tokio::spawn({
            let session = session.clone();
            async move { session.prompt("hi").await }
        });
        for _ in 0..100 {
            if session.running.lock().expect("running").is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        session.interrupt().await.unwrap();
        let outcome = prompt.await.unwrap().unwrap();
        assert_eq!(outcome.stop_reason, StopReason::Cancelled);
    }

    #[test]
    fn model_efforts_suppresses_suffixed_and_unsupported_models_and_derives_base_levels() {
        let driver = AntigravityDriver::new(
            "antigravity",
            Cli::at(vec!["agy".into()]),
            vec!["gemini-3.8-flash".into()],
        );
        // Before `agy models` answers, suffixed slugs and `auto`/orchestrator still take no
        // separate `--effort`, while bare fallback slugs defer to the plugin's defaults (`None`).
        assert_eq!(driver.model_efforts("gemini-4-neon-xhigh"), Some(vec![]));
        assert_eq!(driver.model_efforts("gpt-oss-120b-medium"), Some(vec![]));
        assert_eq!(driver.model_efforts("auto"), Some(vec![]));
        assert_eq!(
            driver.model_efforts("gemini-pro-orchestrator-neon"),
            Some(vec![])
        );
        assert_eq!(driver.model_efforts("gemini-3.8-flash"), None);

        *driver.listed.lock().unwrap() = vec![
            "gemini-4-neon-high".into(),
            "gemini-4-neon-xhigh".into(),
            "gemini-4-neon-medium".into(),
            "gemini-4-neon-low".into(),
            "claude-opus-5-5-high".into(),
            "claude-opus-5-5-low".into(),
            "claude-opus-5-5-medium".into(),
            "gpt-oss-120b-medium".into(),
        ];
        assert_eq!(driver.model_efforts("gemini-4-neon-xhigh"), Some(vec![]));
        assert_eq!(
            driver.model_efforts("gemini-4-neon"),
            Some(vec![
                "low".into(),
                "medium".into(),
                "high".into(),
                "xhigh".into()
            ])
        );
        assert_eq!(
            driver.model_efforts("claude-opus-5-5"),
            Some(vec!["low".into(), "medium".into(), "high".into()])
        );
        assert_eq!(
            driver.model_efforts("gpt-oss-120b"),
            Some(vec!["medium".into()])
        );
    }

    #[test]
    fn resolve_model_effort_never_emits_conflicting_or_unsupported_effort() {
        let listed = vec![
            "gemini-4-neon-low".to_owned(),
            "gemini-4-neon-medium".to_owned(),
            "gemini-4-neon-high".to_owned(),
            "gemini-4-neon-xhigh".to_owned(),
            "claude-opus-5-5-low".to_owned(),
            "claude-opus-5-5-medium".to_owned(),
            "claude-opus-5-5-high".to_owned(),
        ];
        // Effort-suffixed model + conflicting effort: keep the slug and drop `--effort`.
        assert_eq!(
            resolve_model_effort(Some("gemini-4-neon-xhigh"), Some("medium"), &listed),
            (Some("gemini-4-neon-xhigh".into()), None)
        );
        assert_eq!(
            resolve_model_effort(Some("gemini-4-neon-xhigh"), Some("xhigh"), &listed),
            (Some("gemini-4-neon-xhigh".into()), None)
        );
        // Models `agy` refuses `--effort` for.
        assert_eq!(
            resolve_model_effort(Some("auto"), Some("medium"), &listed),
            (Some("auto".into()), None)
        );
        assert_eq!(
            resolve_model_effort(Some("gemini-pro-orchestrator-neon"), Some("medium"), &listed),
            (Some("gemini-pro-orchestrator-neon".into()), None)
        );
        // Bare base model + supported effort passes both; unsupported effort (`max`/`xhigh`) drops `--effort`.
        assert_eq!(
            resolve_model_effort(Some("gemini-4-neon"), Some("medium"), &listed),
            (Some("gemini-4-neon".into()), Some("medium".into()))
        );
        assert_eq!(
            resolve_model_effort(Some("gemini-4-neon"), Some("max"), &listed),
            (Some("gemini-4-neon".into()), None)
        );
        assert_eq!(
            resolve_model_effort(Some("claude-opus-5-5"), Some("xhigh"), &listed),
            (Some("claude-opus-5-5".into()), None)
        );
        // Bare fallback when `listed` is empty still forwards `--effort`.
        assert_eq!(
            resolve_model_effort(Some("gemini-3.8-flash"), Some("high"), &[]),
            (Some("gemini-3.8-flash".into()), Some("high".into()))
        );
        // Effort without an explicit model still passes `--effort`.
        assert_eq!(
            resolve_model_effort(None, Some("medium"), &listed),
            (None, Some("medium".into()))
        );
    }

    #[tokio::test]
    async fn suffixed_model_drops_conflicting_effort_and_clearing_effort_works_on_bare_model() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log.txt");
        let (mut spec, _events) = spec(dir.path());
        spec.agent.model = Some("gemini-4-neon-xhigh".into());
        spec.agent.config = json!({ "effort": "medium" });
        spec.env = vec![("CROSSAGENTS_AGY_TEST_LOG".into(), log.display().to_string())];
        let command = mock_cli(dir.path(), &stream_for(true), &stream_for(false));
        let driver = AntigravityDriver::new("antigravity", Cli::at(command), vec![]);
        let session = driver.open(spec).await.unwrap();

        // Turn 1: `gemini-4-neon-xhigh` with stale `config.effort = "medium"` must not pass `--effort`.
        session.prompt("turn 1").await.unwrap();
        let args1 = logged_args(&read_log(&log));
        assert!(
            args1.windows(2).any(|w| w == ["--model", "gemini-4-neon-xhigh"]),
            "{args1:?}"
        );
        assert!(!args1.contains(&"--effort".to_owned()), "{args1:?}");

        // Turn 2: switching to a bare model with explicit effort passes both flags.
        std::fs::write(&log, "").unwrap();
        session
            .set_model(Some("gemini-3.8-flash"), Some("high"))
            .await
            .unwrap();
        session.prompt("turn 2").await.unwrap();
        let args2 = logged_args(&read_log(&log));
        assert!(
            args2.windows(2).any(|w| w == ["--model", "gemini-3.8-flash"]),
            "{args2:?}"
        );
        assert!(
            args2.windows(2).any(|w| w == ["--effort", "high"]),
            "{args2:?}"
        );

        // Turn 3: clearing effort with `Some("")` drops `--effort` on the restarted process.
        std::fs::write(&log, "").unwrap();
        session
            .set_model(Some("gemini-3.8-flash"), Some(""))
            .await
            .unwrap();
        session.prompt("turn 3").await.unwrap();
        let args3 = logged_args(&read_log(&log));
        assert!(
            args3.windows(2).any(|w| w == ["--model", "gemini-3.8-flash"]),
            "{args3:?}"
        );
        assert!(!args3.contains(&"--effort".to_owned()), "{args3:?}");

        session.shutdown().await;
    }

    #[test]
    fn run_command_extracts_title_command_output_exit_code_and_duration() {
        let step = wire::Step {
            state: "DONE".into(),
            step_type: "tool".into(),
            tool_name: Some("run_command".into()),
            duration_seconds: Some(1.25),
            tool_info: Some(wire::ToolInfo {
                name: Some("run_command".into()),
                parameters: Some(serde_json::json!({
                    "CommandLine": "cargo test -p crossagents-core",
                    "toolSummary": "Run core tests"
                })),
                output: Some("Created At: 2026-11-22\nCommand exited with code 1.\n\nCommand output:\nfailed\n".into()),
                error: None,
            }),
            ..wire::Step::default()
        };
        assert_eq!(
            tool_title("run_command", step.tool_info.as_ref()),
            "cargo test -p crossagents-core"
        );
        let detail = tool_detail("run_command", &step);
        assert_eq!(detail.command.as_deref(), Some("cargo test -p crossagents-core"));
        assert_eq!(detail.exit_code, Some(1));
        assert_eq!(detail.duration_ms, Some(1250));
        assert!(detail.output.as_deref().unwrap().contains("failed"));
        assert!(detail.arguments.is_none());
        assert!(detail.result.is_none());
    }
}
