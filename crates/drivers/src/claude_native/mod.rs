//! `claude-native`: drives Claude Code through its stream-json control protocol, the one the
//! Agent SDK speaks (`claude -p --input-format stream-json --output-format stream-json`).
//!
//! One process per session. Sessions persist in Claude Code's own transcripts (under the
//! account's `CLAUDE_CONFIG_DIR`), so a suspended session resumes by id in a new process,
//! from any directory. We choose the session id (`--session-id`) and every user message's
//! uuid, so turns can be named for forks without asking Claude Code. Protocol notes:
//! docs/research/claude-stream-json.md.

pub mod wire;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use crossagents_core::{
    Capabilities, CoreError, Driver, DriverKind, DriverSession, PermissionHandler,
    PermissionPolicy, Prompt, SessionEvent, SessionEventKind, SessionSpec, StopReason, TurnOutcome,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

use self::wire::{Flavor, Launch, Start, Translator};
use crate::launch::{self, BoxRead, BoxWrite, ExitWatch, LaunchSpec, Launched, ProcessHandle};
use crate::process::{StderrTail, check_proxy, find_in_path, hide_console, system_proxy_env};

/// `entrypoint` of the sessions CrossAgents starts, in Claude Code's transcripts.
pub const ENTRYPOINT: &str = "crossagents";

const INIT_TIMEOUT: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// For interrupting and reading the quota after a turn.
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);
/// For a resumed CodeBuddy session to replay its history (read from disk, so quick).
const REPLAY_TIMEOUT: Duration = Duration::from_secs(15);
/// `get_usage` asks claude.ai; once a minute is plenty.
const LIMITS_EVERY: Duration = Duration::from_secs(60);

/// The `claude-native` driver for one Profile.
pub struct ClaudeNativeDriver {
    name: String,
    command: Vec<String>,
    models: Vec<String>,
    flavor: Flavor,
}

impl ClaudeNativeDriver {
    /// `command` launches Claude Code; empty means `claude` (on `PATH`, else the one the
    /// Claude desktop app bundles). The stream-json arguments are added to it.
    pub fn new(name: impl Into<String>, command: Vec<String>, models: Vec<String>) -> Self {
        Self::with_flavor(name, command, models, Flavor::Claude)
    }

    /// The same protocol, launched with the flags `flavor` accepts.
    pub fn with_flavor(
        name: impl Into<String>,
        command: Vec<String>,
        models: Vec<String>,
        flavor: Flavor,
    ) -> Self {
        Self {
            name: name.into(),
            command: if command.is_empty() {
                vec!["claude".into()]
            } else {
                command
            },
            models,
            flavor,
        }
    }

    async fn start(&self, spec: SessionSpec, start: Start) -> Result<ClaudeSession, CoreError> {
        let command = spec.command.clone().unwrap_or_else(|| self.command.clone());
        ClaudeSession::spawn(command, launch_for(&spec), start, spec, self.flavor).await
    }
}

fn launch_for(spec: &SessionSpec) -> Launch {
    let config = &spec.agent.config;
    let text = |key: &str| config.get(key).and_then(Value::as_str).map(str::to_owned);
    let permission_mode = text("permission_mode");
    Launch {
        model: spec.agent.model.clone(),
        effort: text("effort"),
        // An explicit permission mode replaces the bypass.
        bypass: spec.permission == PermissionPolicy::Bypass && permission_mode.is_none(),
        permission_mode,
        mcp_servers: spec.mcp_servers.clone(),
        instructions: spec.instructions.clone(),
        setting_sources: text("setting_sources")
            .unwrap_or_else(|| wire::DEFAULT_SETTING_SOURCES.into()),
    }
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[async_trait]
impl Driver for ClaudeNativeDriver {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> DriverKind {
        DriverKind::ClaudeNative
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            interrupt: true,
            mcp: true,
            multi_session: false,
            resume: true,
            fork: true,
            // `rewind_files` undoes files but the history rewind needs a new session id;
            // not wired yet.
            rollback: false,
            revert_files: false,
            set_model: true,
            structured_diff: false,
            usage: true,
            rate_limits: true,
            permission_callback: true,
            compaction: true,
        }
    }

    fn models(&self) -> Vec<String> {
        self.models.clone()
    }

    fn installed(&self) -> bool {
        let program = &self.command[0];
        let found = if self.flavor == Flavor::Claude {
            locate_claude(program)
        } else {
            find_in_path(program)
        };
        found.is_file()
    }

    async fn open(&self, spec: SessionSpec) -> Result<Box<dyn DriverSession>, CoreError> {
        Ok(Box::new(self.start(spec, Start::New(new_id())).await?))
    }

    async fn resume(
        &self,
        session_id: &str,
        spec: SessionSpec,
    ) -> Result<Box<dyn DriverSession>, CoreError> {
        Ok(Box::new(
            self.start(spec, Start::Resume(session_id.to_owned()))
                .await?,
        ))
    }

    async fn adopt(
        &self,
        session_id: &str,
        turn_running: bool,
        spec: SessionSpec,
    ) -> Result<Option<Box<dyn DriverSession>>, CoreError> {
        let Some(launched) = launch::adopt(&owner_of(self.flavor, session_id)).await else {
            return Ok(None);
        };
        let command = spec.command.clone().unwrap_or_else(|| self.command.clone());
        Ok(Some(Box::new(ClaudeSession::adopt(
            launched?,
            session_id.to_owned(),
            command,
            spec,
            self.flavor,
            turn_running,
        ))))
    }
}

/// Who may take a session's process over: the session itself.
fn owner_of(flavor: Flavor, session_id: &str) -> String {
    format!("claude-native:{}:{session_id}", flavor.program())
}

/// What the reader task and the session share.
struct Shared {
    session_id: String,
    stdin: tokio::sync::Mutex<Option<BoxWrite>>,
    pending: Mutex<HashMap<String, oneshot::Sender<Result<Value, String>>>>,
    turn: Mutex<Turn>,
    translator: Mutex<Translator>,
    /// The last assistant message of each finished turn, by the turn's user message uuid:
    /// where a fork "up to and including this turn" cuts.
    turn_ends: Mutex<HashMap<String, String>>,
    events: mpsc::UnboundedSender<SessionEvent>,
    permission: PermissionPolicy,
    permissions: Option<Arc<dyn PermissionHandler>>,
    exited: AtomicBool,
    /// We are stopping the process: its exit is not a failure.
    closing: AtomicBool,
    /// The CLI's name, for messages.
    program: &'static str,
    flavor: Flavor,
    /// Set while a resumed CodeBuddy session replays its history, which it does after
    /// `initialize`: old messages and a `result` closing them, which would otherwise end
    /// the first real turn. Taken (and sent) at that `result`.
    replay: Mutex<Option<oneshot::Sender<()>>>,
}

#[derive(Default)]
struct Turn {
    /// The running turn's user message uuid.
    id: Option<String>,
    waiter: Option<oneshot::Sender<StopReason>>,
    interrupted: bool,
    last_assistant: Option<String>,
}

impl Shared {
    fn new(session_id: String, stdin: BoxWrite, spec: &SessionSpec, flavor: Flavor) -> Self {
        Self {
            session_id,
            stdin: tokio::sync::Mutex::new(Some(stdin)),
            pending: Mutex::default(),
            turn: Mutex::default(),
            translator: Mutex::default(),
            turn_ends: Mutex::default(),
            events: spec.events.clone(),
            permission: spec.permission,
            permissions: spec.permissions.clone(),
            exited: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            program: flavor.program(),
            flavor,
            replay: Mutex::default(),
        }
    }

    fn emit(&self, kind: SessionEventKind, raw: Option<&Value>) {
        let _ = self.events.send(SessionEvent {
            kind,
            raw: raw.cloned(),
        });
    }

    async fn write(&self, message: &Value) -> Result<(), CoreError> {
        let mut stdin = self.stdin.lock().await;
        let stdin = stdin
            .as_mut()
            .ok_or_else(|| CoreError::Agent(format!("{} is not running", self.program)))?;
        let mut line = message.to_string();
        line.push('\n');
        let result = async {
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await
        }
        .await;
        result.map_err(|e| CoreError::Agent(format!("cannot write to claude: {e}")))
    }

    async fn control(&self, request: Value, timeout: Duration) -> Result<Value, CoreError> {
        let subtype = request["subtype"].as_str().unwrap_or("request").to_owned();
        let id = new_id();
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("pending lock")
            .insert(id.clone(), tx);
        if let Err(err) = self.write(&wire::control_request(&id, request)).await {
            self.pending.lock().expect("pending lock").remove(&id);
            return Err(err);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(error))) => Err(CoreError::Agent(format!("{subtype}: {error}"))),
            Ok(Err(_)) => Err(CoreError::Agent(format!(
                "{subtype}: {} exited",
                self.program
            ))),
            Err(_) => {
                self.pending.lock().expect("pending lock").remove(&id);
                Err(CoreError::Agent(format!("{subtype}: no answer")))
            }
        }
    }

    fn handle(self: &Arc<Self>, message: Value) {
        let kind = message.get("type").and_then(Value::as_str);
        if !matches!(kind, Some("control_response" | "control_request")) {
            let mut replay = self.replay.lock().expect("replay lock");
            if replay.is_some() {
                // History the hub already has.
                if kind == Some("result")
                    && let Some(done) = replay.take()
                {
                    let _ = done.send(());
                }
                return;
            }
        }
        match kind {
            Some("control_response") => {
                if let Some((id, answer)) = wire::control_answer(&message)
                    && let Some(tx) = self.pending.lock().expect("pending lock").remove(&id)
                {
                    let _ = tx.send(answer);
                }
            }
            Some("control_request") => {
                let this = self.clone();
                tokio::spawn(async move { this.answer(message).await });
            }
            Some("result") => {
                let mut turn = self.turn.lock().expect("turn lock");
                let (stop, events) = self
                    .translator
                    .lock()
                    .expect("translator lock")
                    .turn_end(&message, turn.interrupted);
                for kind in events {
                    self.emit(kind, Some(&message));
                }
                if let (Some(id), Some(last)) = (turn.id.take(), turn.last_assistant.take()) {
                    self.turn_ends
                        .lock()
                        .expect("turn_ends lock")
                        .insert(id, last);
                }
                if let Some(waiter) = turn.waiter.take() {
                    let _ = waiter.send(stop);
                }
                *turn = Turn::default();
            }
            // CodeBuddy reports a turn it cannot run at all (a session it cannot find, say)
            // as a bare `error` message and then waits: no `result` follows, so this ends
            // the turn.
            Some("error") => {
                let error = match message.get("error") {
                    Some(Value::String(text)) => text.clone(),
                    Some(other) => other.to_string(),
                    None => "error".into(),
                };
                self.emit(SessionEventKind::Error { message: error }, Some(&message));
                let mut turn = self.turn.lock().expect("turn lock");
                if let Some(waiter) = turn.waiter.take() {
                    self.emit(
                        SessionEventKind::TurnCompleted {
                            turn_id: turn.id.clone(),
                            stop_reason: StopReason::Error,
                        },
                        Some(&message),
                    );
                    let _ = waiter.send(StopReason::Error);
                }
                *turn = Turn::default();
            }
            Some(kind) => {
                if kind == "assistant"
                    && message.get("parent_tool_use_id").is_none_or(Value::is_null)
                    && let Some(uuid) = message.get("uuid").and_then(Value::as_str)
                {
                    self.turn.lock().expect("turn lock").last_assistant = Some(uuid.to_owned());
                }
                let events = self
                    .translator
                    .lock()
                    .expect("translator lock")
                    .translate(&message);
                for kind in events {
                    self.emit(kind, Some(&message));
                }
            }
            None => {}
        }
    }

    /// Answers a request from Claude Code: permission asks go to the hub; nothing else is
    /// offered (no SDK hooks, in-process MCP servers or forms).
    async fn answer(&self, message: Value) {
        let id = message
            .get("request_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let request = message.get("request").cloned().unwrap_or(Value::Null);
        if request.get("subtype").and_then(Value::as_str) != Some("can_use_tool") {
            let _ = self
                .write(&wire::control_error(&id, "not supported by CrossAgents"))
                .await;
            return;
        }
        let asked = wire::permission_request(&id, &request);
        let decision = match &self.permissions {
            Some(handler) => handler.decide(&asked).await,
            None => self.permission.decide(),
        };
        let answer = wire::permission_response(decision, &request, self.flavor);
        let _ = self.write(&wire::control_success(&id, answer)).await;
        self.emit(
            SessionEventKind::PermissionRequested {
                request: asked,
                decision,
            },
            Some(&message),
        );
    }

    /// The process ended: wake everyone waiting on it.
    fn ended(&self) -> bool {
        self.exited.store(true, Ordering::SeqCst);
        self.pending.lock().expect("pending lock").clear();
        *self.turn.lock().expect("turn lock") = Turn::default();
        !self.closing.load(Ordering::SeqCst)
    }
}

async fn read(shared: Arc<Shared>, stdout: BoxRead, tail: StderrTail, exit: ExitWatch) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        match serde_json::from_str::<Value>(&line) {
            Ok(message) => shared.handle(message),
            Err(_) => tracing::debug!(target: "crossagents::agent_stdout", "{line}"),
        }
    }
    if shared.ended() {
        let info = exit.within(Duration::from_secs(2)).await;
        let what = match info {
            Some(info) => info.describe(shared.program),
            None => format!("{} exited", shared.program),
        };
        let message = tail.describe(&what).await;
        shared.emit(
            SessionEventKind::Exited {
                message,
                cause: info.map(|i| i.cause),
                code: info.and_then(|i| i.code),
            },
            None,
        );
    }
}

/// A Claude Code session in its own `claude -p` process.
pub struct ClaudeSession {
    shared: Arc<Shared>,
    process: ProcessHandle,
    command: Vec<String>,
    flavor: Flavor,
    last_limits: Mutex<Option<Instant>>,
    /// The turn a previous hub started, taken over with the process.
    adopted: Mutex<Option<oneshot::Receiver<StopReason>>>,
}

impl ClaudeSession {
    /// What fills the context window, by kind (`get_context_usage`, as `/context` shows
    /// it). Claude Code only: CodeBuddy does not know the request. Best effort.
    async fn refresh_context(&self) {
        if self.flavor != Flavor::Claude {
            return;
        }
        let request = json!({ "subtype": "get_context_usage" });
        if let Ok(answer) = self.shared.control(request, CLEANUP_TIMEOUT).await
            && let Some(parts) = wire::context_parts(&answer)
        {
            self.shared
                .emit(SessionEventKind::ContextBreakdown { parts }, Some(&answer));
        }
    }

    fn set_compacting(&self, on: bool) {
        self.shared
            .translator
            .lock()
            .expect("translator lock")
            .compacting = on;
    }

    /// The session of a process taken over from a previous hub: past its handshake, with
    /// `turn_running` the turn it is in.
    fn adopt(
        launched: Launched,
        session_id: String,
        command: Vec<String>,
        spec: SessionSpec,
        flavor: Flavor,
        turn_running: bool,
    ) -> Self {
        let Launched {
            stdin,
            stdout,
            stderr: tail,
            process,
        } = launched;
        let shared = Arc::new(Shared::new(session_id, stdin, &spec, flavor));
        let adopted = turn_running.then(|| {
            let (tx, rx) = oneshot::channel();
            shared.turn.lock().expect("turn lock").waiter = Some(tx);
            rx
        });
        tokio::spawn(read(shared.clone(), stdout, tail, process.watch()));
        Self {
            shared,
            process,
            command,
            flavor,
            last_limits: Mutex::new(None),
            adopted: Mutex::new(adopted),
        }
    }
    async fn spawn(
        command: Vec<String>,
        launch: Launch,
        start: Start,
        spec: SessionSpec,
        flavor: Flavor,
    ) -> Result<Self, CoreError> {
        let (program, extra) = command.split_first().ok_or_else(|| {
            CoreError::Agent(format!("empty {} launch command", flavor.program()))
        })?;
        let program = locate_claude(program);
        if flavor == Flavor::Codebuddy
            && let Start::Resume(from) | Start::Fork { from, .. } = &start
        {
            crate::codebuddy_native::adopt_transcript(from, &spec.agent.cwd, &spec.env);
        }
        let session_id = match &start {
            Start::New(id) | Start::Resume(id) | Start::Fork { id, .. } => id.clone(),
        };
        let mut args = extra.to_vec();
        args.extend(wire::args(&launch, &start, flavor));
        let mut env = vec![
            (
                "CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING".to_owned(),
                "1".to_owned(),
            ),
            // Names the sessions' origin in Claude Code's transcripts (usage by origin).
            ("CLAUDE_CODE_ENTRYPOINT".to_owned(), ENTRYPOINT.to_owned()),
        ];
        env.extend(system_proxy_env());
        env.extend(spec.env.iter().cloned());
        // A CLI pointed at a dead proxy starts fine and then cannot reach its API; the
        // turn would stall instead of saying why. Fail here, where the proxy can be named.
        let api_host = flavor.api_host(&spec.env);
        // A host the proxy cannot reach but a direct connection can is exempted instead.
        env.extend(check_proxy(&spec.env, Some(&api_host)).await?);
        let launched = launch::launch(LaunchSpec {
            program,
            args,
            // Claude Code works in its own working directory.
            cwd: spec.agent.cwd.clone(),
            env,
            env_remove: std::env::vars_os()
                .map(|(key, _)| key.to_string_lossy().into_owned())
                .filter(|key| flavor.withholds(key))
                .collect(),
            owner: Some(owner_of(flavor, &session_id)),
            label: format!("{} session {session_id}", flavor.program()),
        })
        .await?;
        let Launched {
            stdin,
            stdout,
            stderr: tail,
            process,
        } = launched;
        let shared = Arc::new(Shared::new(session_id.clone(), stdin, &spec, flavor));
        let replayed =
            (flavor == Flavor::Codebuddy && matches!(start, Start::Resume(_))).then(|| {
                let (tx, rx) = oneshot::channel();
                *shared.replay.lock().expect("replay lock") = Some(tx);
                rx
            });
        tokio::spawn(read(shared.clone(), stdout, tail.clone(), process.watch()));
        if let Err(err) = shared
            .control(json!({ "subtype": "initialize" }), INIT_TIMEOUT)
            .await
        {
            shared.closing.store(true, Ordering::SeqCst);
            process.start_kill();
            return Err(CoreError::Agent(
                tail.describe(&format!("{} did not start ({err})", flavor.program()))
                    .await,
            ));
        }
        if let Some(replayed) = replayed
            && tokio::time::timeout(REPLAY_TIMEOUT, replayed)
                .await
                .is_err()
        {
            // Nothing to replay, or no `result` after it: go on without waiting.
            shared.replay.lock().expect("replay lock").take();
            tracing::debug!("{session_id}: no history replay seen after initialize");
        }
        shared.emit(SessionEventKind::Started { session_id }, None);
        Ok(Self {
            shared,
            process,
            command,
            flavor,
            last_limits: Mutex::new(None),
            adopted: Mutex::new(None),
        })
    }

    fn check_open(&self) -> Result<(), CoreError> {
        if self.shared.closing.load(Ordering::SeqCst) {
            return Err(CoreError::Conflict(format!(
                "session {} is closed",
                self.shared.session_id
            )));
        }
        if self.shared.exited.load(Ordering::SeqCst) {
            return Err(CoreError::Agent(format!("{} exited", self.shared.program)));
        }
        Ok(())
    }

    fn turn_running(&self) -> bool {
        self.shared.turn.lock().expect("turn lock").waiter.is_some()
    }

    async fn close(&self) {
        self.shared.closing.store(true, Ordering::SeqCst);
        // Closing stdin lets Claude Code finish writing its transcript.
        self.shared.stdin.lock().await.take();
        if self.process.exit_within(CLEANUP_TIMEOUT).await.is_none() {
            self.process.start_kill();
            let _ = self.process.exit_within(CLEANUP_TIMEOUT).await;
        }
    }

    /// Reads every limit and the plan, at most once a minute. Best effort.
    async fn refresh_limits(&self) {
        {
            let mut last = self.last_limits.lock().expect("limits lock");
            if last.is_some_and(|at| at.elapsed() < LIMITS_EVERY) {
                return;
            }
            *last = Some(Instant::now());
        }
        let request = json!({ "subtype": "get_usage", "skip_behaviors": true });
        if let Ok(usage) = self.shared.control(request, CLEANUP_TIMEOUT).await
            && let Some(limits) = wire::usage_limits(&usage)
        {
            self.shared
                .emit(SessionEventKind::RateLimits { limits }, Some(&usage));
        }
    }
}

#[async_trait]
impl DriverSession for ClaudeSession {
    fn session_id(&self) -> &str {
        &self.shared.session_id
    }

    async fn prompt(&self, text: &str) -> Result<TurnOutcome, CoreError> {
        self.prompt_with(&Prompt::text(text)).await
    }

    /// Files go as content blocks (docs/tasks/DOGFOOD.md §3).
    async fn prompt_with(&self, prompt: &Prompt) -> Result<TurnOutcome, CoreError> {
        self.check_open()?;
        let uuid = new_id();
        let (tx, rx) = oneshot::channel();
        {
            let mut turn = self.shared.turn.lock().expect("turn lock");
            if turn.waiter.is_some() {
                return Err(CoreError::Conflict("a turn is already running".into()));
            }
            *turn = Turn {
                id: Some(uuid.clone()),
                waiter: Some(tx),
                ..Turn::default()
            };
        }
        self.shared.emit(
            SessionEventKind::TurnStarted {
                turn_id: Some(uuid.clone()),
            },
            None,
        );
        let message =
            wire::user_message(&self.shared.session_id, &uuid, wire::user_content(prompt));
        if let Err(err) = self.shared.write(&message).await {
            *self.shared.turn.lock().expect("turn lock") = Turn::default();
            return Err(err);
        }
        let stop_reason = rx.await.map_err(|_| {
            CoreError::Agent(
                "the turn ended without a result (session closed or claude exited)".into(),
            )
        })?;
        self.refresh_limits().await;
        self.refresh_context().await;
        Ok(TurnOutcome {
            stop_reason,
            turn_id: Some(uuid),
        })
    }

    async fn interrupt(&self) -> Result<(), CoreError> {
        {
            let mut turn = self.shared.turn.lock().expect("turn lock");
            if turn.waiter.is_none() {
                return Ok(());
            }
            turn.interrupted = true;
        }
        match self
            .shared
            .control(json!({ "subtype": "interrupt" }), CLEANUP_TIMEOUT)
            .await
        {
            // The turn may have ended meanwhile, or the process is gone.
            Ok(_) | Err(_) => Ok(()),
        }
    }

    async fn suspend(&self) -> Result<(), CoreError> {
        if self.turn_running() {
            return Err(CoreError::Conflict(
                "cannot suspend while a turn runs".into(),
            ));
        }
        self.close().await;
        Ok(())
    }

    async fn fork(
        &self,
        at_turn: Option<&str>,
        spec: SessionSpec,
    ) -> Result<Box<dyn DriverSession>, CoreError> {
        let at = match at_turn {
            None => None,
            Some(turn) => Some(
                self.shared
                    .turn_ends
                    .lock()
                    .expect("turn_ends lock")
                    .get(turn)
                    .cloned()
                    // Turns are known only from this process's own output.
                    .ok_or_else(|| CoreError::NotFound {
                        kind: "turn",
                        id: turn.to_owned(),
                    })?,
            ),
        };
        let id = new_id();
        let start = if self.flavor == Flavor::Codebuddy {
            // CodeBuddy's `--fork-session` ignores `--session-id` and names the fork itself,
            // so the fork is written here, under our id, and resumed.
            crate::codebuddy_native::fork_transcript(
                &self.shared.session_id,
                &id,
                at.as_deref(),
                &spec.agent.cwd,
                &spec.env,
            )?;
            Start::Resume(id)
        } else {
            Start::Fork {
                from: self.shared.session_id.clone(),
                id,
                at,
            }
        };
        let command = spec.command.clone().unwrap_or_else(|| self.command.clone());
        let session =
            ClaudeSession::spawn(command, launch_for(&spec), start, spec, self.flavor).await?;
        Ok(Box::new(session))
    }

    async fn set_model(&self, model: Option<&str>, effort: Option<&str>) -> Result<(), CoreError> {
        self.check_open()?;
        if let Some(model) = model {
            self.shared
                .control(
                    json!({ "subtype": "set_model", "model": model }),
                    REQUEST_TIMEOUT,
                )
                .await?;
        }
        if let Some(effort) = effort {
            self.shared
                .control(wire::set_effort(self.flavor, effort), REQUEST_TIMEOUT)
                .await?;
        }
        Ok(())
    }

    async fn compact(&self) -> Result<(), CoreError> {
        // CodeBuddy streams the summary like a reply; it is not one.
        self.set_compacting(true);
        let outcome = self.prompt("/compact").await;
        self.set_compacting(false);
        match outcome?.stop_reason {
            StopReason::Error => Err(CoreError::Agent("compaction failed".into())),
            _ => Ok(()),
        }
    }

    async fn shutdown(&self) {
        let _ = self.interrupt().await;
        self.close().await;
    }

    fn outlives_hub(&self) -> bool {
        self.process.outlives_hub()
    }

    async fn adopted_turn(&self) -> Option<Result<TurnOutcome, CoreError>> {
        let rx = self.adopted.lock().expect("adopted lock").take()?;
        let result = rx.await.map_err(|_| {
            CoreError::Agent(format!(
                "the turn ended without a result ({} exited)",
                self.shared.program
            ))
        });
        if result.is_ok() {
            self.refresh_limits().await;
            self.refresh_context().await;
        }
        Some(result.map(|stop_reason| TurnOutcome {
            stop_reason,
            turn_id: None,
        }))
    }
}

impl Drop for ClaudeSession {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::SeqCst);
    }
}

/// The `claude` to run: as given when it is a path; else from `PATH`; else Claude Code's
/// native install (`~/.local/bin`), else the newest one the Claude desktop app keeps.
pub fn locate_claude(program: &str) -> PathBuf {
    let found = find_in_path(program);
    if found.components().count() > 1 || program != "claude" {
        return found;
    }
    let exe = if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    };
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    if let Some(native) = home
        .map(|h| PathBuf::from(h).join(".local").join("bin").join(exe))
        .filter(|p| p.is_file())
    {
        return native;
    }
    bundled_claude().unwrap_or(found)
}

/// `%LOCALAPPDATA%\Claude-Data\<app>\claude-code\<version>\<build>\claude.exe`, the newest.
fn bundled_claude() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let data = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Claude-Data");
    let children = |dir: PathBuf| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|entries| entries.flatten().map(|e| e.path()).collect())
            .unwrap_or_default()
    };
    children(data)
        .into_iter()
        .flat_map(|app| children(app.join("claude-code")))
        .flat_map(children)
        .map(|build| build.join("claude.exe"))
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .max()
        .map(|(_, p)| p)
}

/// `claude auth status` for a config directory (`env` carries its `CLAUDE_CONFIG_DIR`):
/// logged in or not, how, the email and the plan. `None` when `claude` cannot run.
pub async fn auth_status(program: &str, env: &[(String, String)]) -> Option<Value> {
    let program = locate_claude(program);
    let mut command = Command::new(&program);
    command
        .args(["auth", "status"])
        .envs(system_proxy_env())
        .envs(env.iter().cloned())
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .kill_on_drop(true);
    hide_console(&mut command);
    let output = tokio::time::timeout(CLEANUP_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    serde_json::from_slice(&output.stdout).ok()
}

/// Asks Claude Code about an account without starting a session: the `initialize` answer
/// (account, models) and `get_usage` (plan and limits).
pub async fn probe_account(
    program: &str,
    env: &[(String, String)],
) -> Result<(Value, Value), CoreError> {
    let program = locate_claude(program);
    let mut command = Command::new(&program);
    command
        .args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--strict-mcp-config",
            "--setting-sources",
            wire::DEFAULT_SETTING_SOURCES,
        ])
        .envs(system_proxy_env())
        .envs(env.iter().cloned())
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    hide_console(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| CoreError::Agent(format!("cannot start {}: {e}", program.display())))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let ask = async {
        for (id, request) in [
            ("init", json!({ "subtype": "initialize" })),
            (
                "usage",
                json!({ "subtype": "get_usage", "skip_behaviors": true }),
            ),
        ] {
            let mut line = wire::control_request(id, request).to_string();
            line.push('\n');
            stdin.write_all(line.as_bytes()).await?;
        }
        stdin.flush().await?;
        let mut lines = BufReader::new(stdout).lines();
        let (mut init, mut usage) = (None, None);
        while let Some(line) = lines.next_line().await? {
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match wire::control_answer(&message) {
                Some((id, answer)) if id == "init" => init = Some(answer),
                Some((id, answer)) if id == "usage" => usage = Some(answer),
                _ => {}
            }
            if let (Some(init), Some(usage)) = (&init, &usage) {
                return Ok::<_, std::io::Error>((init.clone(), usage.clone()));
            }
        }
        Err(std::io::Error::other("claude exited before answering"))
    };
    let answers = tokio::time::timeout(INIT_TIMEOUT, ask).await;
    let _ = child.start_kill();
    match answers {
        Ok(Ok((Ok(init), Ok(usage)))) => Ok((init, usage)),
        Ok(Ok((Err(e), _) | (_, Err(e)))) => Err(CoreError::Agent(e)),
        Ok(Err(e)) => Err(CoreError::Agent(e.to_string())),
        Err(_) => Err(CoreError::Agent("claude did not answer".into())),
    }
}
