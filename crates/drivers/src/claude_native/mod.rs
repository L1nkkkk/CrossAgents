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
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use crossagents_core::{
    AgentCommand, AttachmentSupport, Capabilities, CliStatus, CoreError, Driver, DriverKind,
    DriverSession, MessageId, PermissionHandler, PermissionPolicy, Prompt, SessionEvent,
    SessionEventKind, SessionSpec, StopReason, TurnOutcome,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

use self::wire::{Flavor, Launch, Start, Translator};
use crate::launch::{self, BoxRead, BoxWrite, ExitWatch, LaunchSpec, Launched, ProcessHandle};
use crate::locate::Cli;
use crate::process::{StderrTail, check_proxy, hide_console, system_proxy_env};

/// `entrypoint` of the sessions CrossAgents starts, in Claude Code's transcripts.
pub const ENTRYPOINT: &str = "crossagents";

const INIT_TIMEOUT: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// For interrupting and reading the quota after a turn.
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);
const WRITER_CLOSE: Duration = Duration::from_secs(1);
/// For a resumed CodeBuddy session to replay its history (read from disk, so quick).
const REPLAY_TIMEOUT: Duration = Duration::from_secs(15);
/// `get_usage` asks claude.ai; once a minute is plenty.
const LIMITS_EVERY: Duration = Duration::from_secs(60);

/// The `claude-native` driver for one Profile.
pub struct ClaudeNativeDriver {
    name: String,
    models: Vec<String>,
    flavor: Flavor,
    /// The CLI the description names, found again whenever a session starts
    /// (docs/DESIGN.md §9.3).
    cli: Cli,
}

impl ClaudeNativeDriver {
    /// `cli` is the description's CLI: `native_command` when it gives one, else whatever the
    /// description finds. The stream-json arguments are added to it.
    pub fn new(name: impl Into<String>, cli: Cli, models: Vec<String>) -> Self {
        Self::with_flavor(name, cli, models, Flavor::Claude)
    }

    /// The same protocol, launched with the flags `flavor` accepts.
    pub fn with_flavor(
        name: impl Into<String>,
        cli: Cli,
        models: Vec<String>,
        flavor: Flavor,
    ) -> Self {
        Self {
            name: name.into(),
            models,
            flavor,
            cli,
        }
    }

    /// The CLI behind this driver, as this machine has it.
    pub async fn status(&self) -> CliStatus {
        self.cli.status().await
    }

    /// The command a session runs: the agent's own when it names one, else the CLI the
    /// description finds now -- installed, moved or updated since the hub started.
    fn command_for(&self, spec: &SessionSpec) -> Vec<String> {
        match &spec.command {
            Some(command) => self.cli.launch(command),
            None => self.cli.command().unwrap_or_else(|| {
                self.cli
                    .default_name()
                    .map(|name| vec![name])
                    .unwrap_or_default()
            }),
        }
    }

    async fn start(&self, spec: SessionSpec, start: Start) -> Result<ClaudeSession, CoreError> {
        let command = self.command_for(&spec);
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
            // Claude Code reads a message written while a turn runs at its next tool
            // boundary: measured with claude 2.1.317, a steer written during a 12 s tool
            // was read right after the tool result and changed the turn's answer. CodeBuddy
            // only reads between turns, so its flavor stays off.
            steer: self.flavor == Flavor::Claude,
            mcp: true,
            multi_session: false,
            resume: true,
            move_cwd: true,
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
            // stream-json content blocks: images and PDFs as such, text inlined
            // (docs/tasks/DOGFOOD.md §3).
            attachments: Some(AttachmentSupport::ALL),
        }
    }

    fn models(&self) -> Vec<String> {
        self.models.clone()
    }

    fn installed(&self) -> bool {
        self.cli.installed()
    }

    async fn cli_status(&self) -> Option<CliStatus> {
        Some(self.status().await)
    }

    /// Asked at hub startup, in the background: `/v1/plugins` never waits for it.
    async fn probe_cli(&self) {
        let _ = self.cli.version().await;
    }

    /// After an install or an upgrade: the file the cache named may be gone or replaced.
    fn refresh_cli(&self) {
        self.cli.forget();
    }

    fn cli_places(&self) -> Vec<String> {
        self.cli.searched()
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
        let command = self.command_for(&spec);
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
    writer: mpsc::Sender<WriteCommand>,
    writer_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    writer_gate: Mutex<()>,
    pending: Mutex<HashMap<String, oneshot::Sender<Result<Value, String>>>>,
    turn: Mutex<Turn>,
    translator: Mutex<Translator>,
    /// The CLI's own slash commands and skills, from its `system`/`init` message.
    commands: Mutex<Option<Vec<AgentCommand>>>,
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
    /// Steers whose own turn may still report a `result` after the hub turn they belonged
    /// to ended. Such a result ends that steer's turn, never the next one (`user_message_uuid`
    /// names it).
    stale_steers: Mutex<Vec<MessageId>>,
}

#[derive(Debug)]
enum WriteCommand {
    Frame(Vec<u8>, oneshot::Sender<Result<(), String>>),
    Close,
}

struct PendingRequest<'a> {
    shared: &'a Shared,
    id: &'a str,
}

impl Drop for PendingRequest<'_> {
    fn drop(&mut self) {
        self.shared
            .pending
            .lock()
            .expect("pending lock")
            .remove(self.id);
    }
}

#[derive(Default)]
struct Turn {
    /// The running turn's user message uuid.
    id: Option<String>,
    waiter: Option<oneshot::Sender<StopReason>>,
    interrupted: bool,
    last_assistant: Option<String>,
    /// Messages put into this turn with `steer` that the model has not read yet.
    steers: Vec<PendingSteer>,
    /// Every message steered into this turn, read or not: a `result` names the message
    /// whose turn it ends with `user_message_uuid`.
    steered: Vec<MessageId>,
    /// The prompt's own result arrived.
    prompt_done: bool,
    /// The CLI opened a turn of its own for steered messages (a `system/init` while they
    /// waited): its result ends this turn, not the next one.
    separate_steer_turn: bool,
    /// Steers whose own turn reported its `result`.
    steer_results: Vec<MessageId>,
    /// The prompt's result, kept until that separate turn has reported.
    held_stop: Option<StopReason>,
    /// The interrupt left the unread steers queued (`still_queued`): they will still be
    /// read, so the turn waits for them like a late message.
    keep_queued: bool,
    /// Interrupt requests whose answers can preserve this turn's unread steers.
    interrupt_requests: Vec<String>,
}

/// A message written into the running turn, waiting for the model to read it.
struct PendingSteer {
    id: MessageId,
    /// Its opening words, for a warning when it never gets read.
    snippet: String,
}

fn interrupt_kept_queued(answer: &Result<Value, String>) -> bool {
    answer.as_ref().is_ok_and(|answer| {
        answer
            .get("still_queued")
            .and_then(Value::as_array)
            .is_some_and(|queued| !queued.is_empty())
    })
}

impl Shared {
    fn new(
        session_id: String,
        stdin: BoxWrite,
        spec: &SessionSpec,
        flavor: Flavor,
        process: &Arc<ProcessHandle>,
    ) -> Arc<Self> {
        let (writer, queue) = mpsc::channel(32);
        let shared = Arc::new(Self {
            session_id,
            writer,
            writer_task: Mutex::new(None),
            writer_gate: Mutex::new(()),
            pending: Mutex::default(),
            turn: Mutex::default(),
            translator: Mutex::default(),
            commands: Mutex::default(),
            turn_ends: Mutex::default(),
            events: spec.events.clone(),
            permission: spec.permission,
            permissions: spec.permissions.clone(),
            exited: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            program: flavor.program(),
            flavor,
            replay: Mutex::default(),
            stale_steers: Mutex::default(),
        });
        shared.start_writer(
            stdin,
            queue,
            Some(Arc::downgrade(process)),
            crate::host::STDIN_STALL,
        );
        shared
    }

    fn start_writer(
        self: &Arc<Self>,
        mut stdin: BoxWrite,
        mut queue: mpsc::Receiver<WriteCommand>,
        process: Option<std::sync::Weak<ProcessHandle>>,
        stall: Duration,
    ) {
        let shared = Arc::downgrade(self);
        let task = tokio::spawn(async move {
            while let Some(command) = queue.recv().await {
                let WriteCommand::Frame(line, ack) = command else {
                    break;
                };
                let result = crate::host::write_progress(&mut *stdin, &line, stall).await;
                match result {
                    Ok(()) => {
                        let _ = ack.send(Ok(()));
                    }
                    Err(failure) => {
                        let error = failure.to_string();
                        // Drop the pipe before notifying anyone: no later frame may use it.
                        drop(stdin);
                        if let Some(shared) = shared.upgrade() {
                            shared.ended();
                            shared.emit(
                                SessionEventKind::Warning {
                                    message: format!("{} stdin failed: {error}", shared.program),
                                },
                                None,
                            );
                        }
                        if let Some(process) = process.and_then(|p| p.upgrade()) {
                            process.start_kill();
                        }
                        let _ = ack.send(Err(error));
                        return;
                    }
                }
            }
        });
        *self.writer_task.lock().expect("writer task lock") = Some(task);
    }

    fn emit(&self, kind: SessionEventKind, raw: Option<&Value>) {
        let _ = self.events.send(SessionEvent {
            kind,
            raw: raw.cloned(),
        });
    }

    async fn write(&self, message: &Value) -> Result<(), CoreError> {
        let permit = self.reserve_write().await?;
        let done = {
            let _gate = self.writer_gate.lock().expect("writer gate");
            self.check_writable()?;
            self.enqueue(permit, message)
        };
        self.wait_written(done).await
    }

    fn check_writable(&self) -> Result<(), CoreError> {
        if self.exited.load(Ordering::SeqCst) || self.closing.load(Ordering::SeqCst) {
            Err(CoreError::Agent(format!("{} is not running", self.program)))
        } else {
            Ok(())
        }
    }

    async fn reserve_write(&self) -> Result<mpsc::Permit<'_, WriteCommand>, CoreError> {
        self.check_writable()?;
        self.writer
            .reserve()
            .await
            .map_err(|_| CoreError::Agent(format!("{} writer stopped", self.program)))
    }

    fn enqueue(
        &self,
        permit: mpsc::Permit<'_, WriteCommand>,
        message: &Value,
    ) -> oneshot::Receiver<Result<(), String>> {
        let mut line = message.to_string();
        line.push('\n');
        let (ack, done) = oneshot::channel();
        permit.send(WriteCommand::Frame(line.into_bytes(), ack));
        done
    }

    async fn wait_written(
        &self,
        done: oneshot::Receiver<Result<(), String>>,
    ) -> Result<(), CoreError> {
        match done.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(CoreError::Agent(format!(
                "cannot write to {}: {error}",
                self.program
            ))),
            Err(_) => Err(CoreError::Agent(format!(
                "cannot write to {}: writer stopped",
                self.program
            ))),
        }
    }

    async fn control(&self, request: Value, timeout: Duration) -> Result<Value, CoreError> {
        self.control_with_id(request, timeout, new_id()).await
    }

    async fn control_with_id(
        &self,
        request: Value,
        timeout: Duration,
        id: String,
    ) -> Result<Value, CoreError> {
        let subtype = request["subtype"].as_str().unwrap_or("request").to_owned();
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("pending lock")
            .insert(id.clone(), tx);
        let _pending = PendingRequest {
            shared: self,
            id: &id,
        };
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

    /// Says these steers were not read, and remembers them: the CLI may still run them,
    /// and their results must not end a later turn.
    fn warn_unread(&self, unread: Vec<PendingSteer>) {
        if unread.is_empty() {
            return;
        }
        let mut stale = self.stale_steers.lock().expect("stale lock");
        for steer in &unread {
            self.emit(
                SessionEventKind::Warning {
                    message: format!("这条插话没有被读取：「{}」", steer.snippet),
                },
                None,
            );
            if !stale.contains(&steer.id) {
                stale.push(steer.id);
            }
        }
        if stale.len() > 16 {
            let extra = stale.len() - 16;
            stale.drain(..extra);
        }
    }

    /// Ends the running turn. A message still unread was not read: say so, and do not
    /// claim otherwise. Its own turn may still report later (an interrupted turn keeps its
    /// queued messages), so its id is remembered as stale.
    fn finish_turn(&self, turn: &mut Turn, stop: StopReason) {
        let stop = turn.held_stop.take().unwrap_or(stop);
        let unread: Vec<PendingSteer> = std::mem::take(&mut turn.steers);
        self.warn_unread(unread);
        let mut stale = self.stale_steers.lock().expect("stale lock");
        if turn.separate_steer_turn {
            for id in &turn.steered {
                if !turn.steer_results.contains(id) && !stale.contains(id) {
                    stale.push(*id);
                }
            }
        }
        if stale.len() > 16 {
            let extra = stale.len() - 16;
            stale.drain(..extra);
        }
        drop(stale);
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

    fn handle(self: &Arc<Self>, message: Value) {
        // Before the replay filter: a resumed CodeBuddy session's init is still the CLI's.
        if let Some(commands) = wire::commands(&message) {
            *self.commands.lock().expect("commands lock") = Some(commands);
        }
        let kind = message.get("type").and_then(Value::as_str);
        // The CLI opens a turn of its own for a message written too late (`system/init`
        // while it waits): that turn's result belongs to this one, not to the next. A
        // steer already read in this turn is not one of those: a later init (compaction,
        // say) must not make the turn wait for a result that will never come.
        if kind == Some("system") && message.get("subtype").and_then(Value::as_str) == Some("init")
        {
            let mut turn = self.turn.lock().expect("turn lock");
            if !turn.steers.is_empty() {
                turn.separate_steer_turn = true;
            }
        }
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
                if let Some((id, answer)) = wire::control_answer(&message) {
                    let tx = self.pending.lock().expect("pending lock").remove(&id);
                    let mut turn = self.turn.lock().expect("turn lock");
                    if let Some(index) = turn
                        .interrupt_requests
                        .iter()
                        .position(|request| request == &id)
                    {
                        turn.interrupt_requests.remove(index);
                        turn.keep_queued |= interrupt_kept_queued(&answer);
                    }
                    drop(turn);
                    if let Some(tx) = tx {
                        let _ = tx.send(answer);
                    }
                }
            }
            Some("control_request") => {
                let this = self.clone();
                tokio::spawn(async move { this.answer(message).await });
            }
            Some("result") => {
                // Claude names the message in `user_message_uuid` (2.1.317).
                // CodeBuddy 2.179.2 does not supply that field.
                let ended: Option<MessageId> = message
                    .get("user_message_uuid")
                    .and_then(Value::as_str)
                    .and_then(|uuid| uuid.parse().ok());
                // A failed prompt only waits on positive evidence of a separate,
                // already-read steer (or an interrupt that kept its queue).
                let failed = message.get("is_error").and_then(Value::as_bool) == Some(true)
                    || message.get("subtype").and_then(Value::as_str) != Some("success");
                // A steer's own turn may report after the hub turn it belonged to ended.
                // Absorb its result: it must never end the next prompt's turn.
                if let Some(id) = ended {
                    let mut stale = self.stale_steers.lock().expect("stale lock");
                    if stale.contains(&id) {
                        stale.retain(|stale| *stale != id);
                        drop(stale);
                        self.emit(
                            SessionEventKind::SteerRead { message_id: id },
                            Some(&message),
                        );
                        return;
                    }
                }
                let mut turn = self.turn.lock().expect("turn lock");
                let (stop, events) = self
                    .translator
                    .lock()
                    .expect("translator lock")
                    .turn_end(&message, turn.interrupted);
                for kind in events {
                    self.emit(kind, Some(&message));
                }
                if ended.is_some_and(|id| turn.steered.contains(&id)) {
                    // The follow-up turn for a late message: it read it, and ends here.
                    let id = ended.expect("checked");
                    if turn.steers.iter().any(|steer| steer.id == id) {
                        turn.steers.retain(|steer| steer.id != id);
                        self.emit(
                            SessionEventKind::SteerRead { message_id: id },
                            Some(&message),
                        );
                    }
                    turn.steer_results.push(id);
                    if !turn.prompt_done {
                        // It came before the prompt's own result: wait for that.
                        return;
                    }
                    // More late messages wait for their turn to read them, unless this one
                    // failed: then nothing more will run for them.
                    if !failed && !turn.steers.is_empty() {
                        return;
                    }
                } else {
                    // The prompt's result. A steer still unread, or a separate turn that
                    // has not reported, keeps this turn open so no stray result ends the
                    // next one. An interrupted turn is kept open because its queued
                    // messages are still read (`still_queued`, verified with claude
                    // 2.1.317); a failure without an interrupt ends now.
                    turn.prompt_done = true;
                    turn.held_stop.get_or_insert(stop);
                    let unread = !turn.steers.is_empty();
                    let separate = turn.separate_steer_turn && turn.steer_results.is_empty();
                    let survives = turn.interrupted && turn.keep_queued;
                    let read_separate = turn.separate_steer_turn
                        && turn.steered.iter().any(|id| {
                            !turn.steers.iter().any(|steer| steer.id == *id)
                                && !turn.steer_results.contains(id)
                        });
                    if ((survives || !failed) && (unread || separate)) || read_separate {
                        return;
                    }
                }
                self.finish_turn(&mut turn, stop);
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
                let unread: Vec<PendingSteer> = std::mem::take(&mut turn.steers);
                *turn = Turn::default();
                drop(turn);
                self.warn_unread(unread);
            }
            Some(kind) => {
                // The CLI echoes a user message with `isReplay` and the uuid we gave it
                // when it reads it into the running turn (`--replay-user-messages`). That,
                // and only that, says the model has seen a steer: the next assistant output
                // may still belong to the turn before it (verified with claude 2.1.317).
                if message.get("isReplay").and_then(Value::as_bool) == Some(true)
                    && let Some(id) = message
                        .get("uuid")
                        .and_then(Value::as_str)
                        .and_then(|uuid| uuid.parse::<MessageId>().ok())
                {
                    let read = {
                        let mut turn = self.turn.lock().expect("turn lock");
                        match turn.steers.iter().position(|steer| steer.id == id) {
                            Some(index) => {
                                turn.steers.remove(index);
                                true
                            }
                            // Warned about when an earlier turn ended, still queued: its
                            // turn has started now.
                            None => {
                                drop(turn);
                                let mut stale = self.stale_steers.lock().expect("stale lock");
                                let found = stale.contains(&id);
                                stale.retain(|stale| *stale != id);
                                found
                            }
                        }
                    };
                    if read {
                        self.emit(
                            SessionEventKind::SteerRead { message_id: id },
                            Some(&message),
                        );
                    }
                }
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
        let unread = {
            let mut turn = self.turn.lock().expect("turn lock");
            let unread: Vec<PendingSteer> = std::mem::take(&mut turn.steers);
            *turn = Turn::default();
            unread
        };
        // The process is gone: these were never read. Say so rather than mark them read.
        for steer in unread {
            self.emit(
                SessionEventKind::Warning {
                    message: format!("这条插话没有被读取：「{}」", steer.snippet),
                },
                None,
            );
        }
        self.stale_steers.lock().expect("stale lock").clear();
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
    process: Arc<ProcessHandle>,
    command: Vec<String>,
    flavor: Flavor,
    last_limits: Mutex<Option<Instant>>,
    /// The turn a previous hub started, taken over with the process.
    adopted: Mutex<Option<oneshot::Receiver<StopReason>>>,
}

/// Turns the compaction flag off when a `/compact` ends, however it ends.
struct CompactingReset<'a>(&'a ClaudeSession);

impl Drop for CompactingReset<'_> {
    fn drop(&mut self) {
        self.0.set_compacting(false);
    }
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        let process = Arc::new(process);
        let shared = Shared::new(session_id, stdin, &spec, flavor, &process);
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
        let program = PathBuf::from(program);
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
        let env_remove = if flavor == Flavor::Codebuddy {
            crate::codebuddy_native::prepare_env(&mut env)
        } else {
            Vec::new()
        };
        let launched = launch::launch(LaunchSpec {
            program,
            args,
            // Claude Code works in its own working directory.
            cwd: spec.agent.cwd.clone(),
            env,
            env_remove,
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
        let process = Arc::new(process);
        let shared = Shared::new(session_id.clone(), stdin, &spec, flavor, &process);
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
        {
            let _gate = self.shared.writer_gate.lock().expect("writer gate");
            self.shared.closing.store(true, Ordering::SeqCst);
        }
        // Let queued frames drain briefly, then drop the pipe even if the CLI is stuck.
        let task = self
            .shared
            .writer_task
            .lock()
            .expect("writer task lock")
            .take();
        if let Some(mut task) = task {
            let drained = tokio::time::timeout(WRITER_CLOSE, async {
                let _ = self.shared.writer.send(WriteCommand::Close).await;
                let _ = (&mut task).await;
            })
            .await
            .is_ok();
            if !drained {
                task.abort();
                let _ = task.await;
                self.shared.ended();
                self.process.start_kill();
                self.shared.emit(
                    SessionEventKind::Warning {
                        message: format!(
                            "{} stdin did not drain during close",
                            self.shared.program
                        ),
                    },
                    None,
                );
            }
        }
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

    fn commands(&self) -> Option<Vec<AgentCommand>> {
        self.shared.commands.lock().expect("commands lock").clone()
    }

    async fn prompt(&self, text: &str) -> Result<TurnOutcome, CoreError> {
        self.prompt_with(&Prompt::text(text)).await
    }

    /// Files go as content blocks (docs/tasks/DOGFOOD.md §3).
    async fn prompt_with(&self, prompt: &Prompt) -> Result<TurnOutcome, CoreError> {
        self.check_open()?;
        let uuid = new_id();
        let (tx, rx) = oneshot::channel();
        let message =
            wire::user_message(&self.shared.session_id, &uuid, wire::user_content(prompt));
        let permit = self.shared.reserve_write().await?;
        let done = {
            let _gate = self.shared.writer_gate.lock().expect("writer gate");
            self.shared.check_writable()?;
            let mut turn = self.shared.turn.lock().expect("turn lock");
            if turn.waiter.is_some() {
                return Err(CoreError::Conflict("a turn is already running".into()));
            }
            *turn = Turn {
                id: Some(uuid.clone()),
                waiter: Some(tx),
                ..Turn::default()
            };
            self.shared.emit(
                SessionEventKind::TurnStarted {
                    turn_id: Some(uuid.clone()),
                },
                None,
            );
            self.shared.enqueue(permit, &message)
        };
        self.shared.wait_written(done).await?;
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

    /// Writes the message into the running turn's stdin, naming it with the hub's message
    /// id. No new turn, and no waiter: the running `prompt_with` still awaits its result.
    async fn steer(&self, message_id: MessageId, prompt: &Prompt) -> Result<(), CoreError> {
        self.check_open()?;
        let snippet: String = prompt
            .text
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(40)
            .collect();
        let message = wire::user_message(
            &self.shared.session_id,
            &message_id.to_string(),
            wire::user_content(prompt),
        );
        let permit = self.shared.reserve_write().await?;
        let done = {
            let _gate = self.shared.writer_gate.lock().expect("writer gate");
            self.shared.check_writable()?;
            let mut turn = self.shared.turn.lock().expect("turn lock");
            if turn.waiter.is_none() {
                return Err(CoreError::Conflict("no turn is running".into()));
            }
            turn.steers.push(PendingSteer {
                id: message_id,
                snippet,
            });
            turn.steered.push(message_id);
            self.shared.enqueue(permit, &message)
        };
        self.shared.wait_written(done).await
    }

    async fn interrupt(&self) -> Result<(), CoreError> {
        let id = new_id();
        {
            let mut turn = self.shared.turn.lock().expect("turn lock");
            if turn.waiter.is_none() {
                return Ok(());
            }
            turn.interrupted = true;
            // A second interruption abandons a separate steer whose result is still
            // outstanding. The first prompt's stop reason remains authoritative.
            if turn.prompt_done {
                self.shared.finish_turn(&mut turn, StopReason::Cancelled);
            }
            if !turn.steers.is_empty() {
                turn.interrupt_requests.push(id.clone());
            }
        }
        let _ = self
            .shared
            .control_with_id(json!({ "subtype": "interrupt" }), CLEANUP_TIMEOUT, id)
            .await;
        // The turn may have ended meanwhile, or the process is gone.
        Ok(())
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
        // Reset even when this future is dropped: a flag left on hides every later reply.
        let _reset = CompactingReset(self);
        let outcome = self.prompt("/compact").await;
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

/// `claude auth status` for a config directory (`env` carries its `CLAUDE_CONFIG_DIR`):
/// logged in or not, how, the email and the plan. `None` when `claude` cannot run.
pub async fn auth_status(program: &Path, env: &[(String, String)]) -> Option<Value> {
    let mut command = Command::new(program);
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
    program: &Path,
    env: &[(String, String)],
) -> Result<(Value, Value), CoreError> {
    let mut command = Command::new(program);
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

#[cfg(test)]
mod interrupt_tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};

    struct TestKiller(tokio::sync::watch::Sender<Option<crate::launch::ExitInfo>>);
    impl crate::launch::Killer for TestKiller {
        fn kill(&self) {
            let _ = self
                .0
                .send(Some(crate::launch::ExitInfo::from_code(None, true)));
        }
    }

    fn test_process() -> Arc<ProcessHandle> {
        let (tx, rx) = tokio::sync::watch::channel(None);
        Arc::new(ProcessHandle::new(None, None, rx, Arc::new(TestKiller(tx))))
    }

    fn test_session(shared: Arc<Shared>, process: Arc<ProcessHandle>) -> ClaudeSession {
        ClaudeSession {
            shared,
            process,
            command: vec![],
            flavor: Flavor::Claude,
            last_limits: Mutex::new(None),
            adopted: Mutex::new(None),
        }
    }

    fn setup(
        steer: Option<MessageId>,
    ) -> (
        Arc<Shared>,
        oneshot::Receiver<StopReason>,
        mpsc::UnboundedReceiver<SessionEvent>,
    ) {
        let (_cli, stdin) = tokio::io::duplex(4096);
        setup_io(steer, Box::new(stdin), None)
    }

    fn setup_io(
        steer: Option<MessageId>,
        stdin: BoxWrite,
        process: Option<std::sync::Weak<ProcessHandle>>,
    ) -> (
        Arc<Shared>,
        oneshot::Receiver<StopReason>,
        mpsc::UnboundedReceiver<SessionEvent>,
    ) {
        let (events, rx) = mpsc::unbounded_channel();
        let (writer, queue) = mpsc::channel(32);
        let shared = Arc::new(Shared {
            session_id: "test".into(),
            writer,
            writer_task: Mutex::new(None),
            writer_gate: Mutex::new(()),
            pending: Mutex::default(),
            turn: Mutex::default(),
            translator: Mutex::default(),
            commands: Mutex::default(),
            turn_ends: Mutex::default(),
            events,
            permission: PermissionPolicy::Bypass,
            permissions: None,
            exited: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            program: "claude",
            flavor: Flavor::Claude,
            replay: Mutex::default(),
            stale_steers: Mutex::default(),
        });
        shared.start_writer(stdin, queue, process, Duration::from_secs(3));
        let (tx, ended) = oneshot::channel();
        let mut turn = shared.turn.lock().unwrap();
        turn.id = Some("turn".into());
        turn.waiter = Some(tx);
        turn.interrupted = true;
        if let Some(id) = steer {
            turn.steers.push(PendingSteer {
                id,
                snippet: "steer".into(),
            });
            turn.steered.push(id);
        }
        drop(turn);
        (shared, ended, rx)
    }

    fn result(id: &str) -> Value {
        json!({"type":"result", "subtype":"error_during_execution", "is_error":true, "user_message_uuid":id})
    }
    fn warnings(events: &mut mpsc::UnboundedReceiver<SessionEvent>) -> usize {
        let mut n = 0;
        while let Ok(event) = events.try_recv() {
            if matches!(event.kind, SessionEventKind::Warning { .. }) {
                n += 1;
            }
        }
        n
    }
    fn register(shared: &Shared) {
        let (tx, _rx) = oneshot::channel();
        shared
            .turn
            .lock()
            .unwrap()
            .interrupt_requests
            .push("req".into());
        shared.pending.lock().unwrap().insert("req".into(), tx);
    }
    async fn batch(shared: &Arc<Shared>, messages: &[Value]) {
        let text = messages
            .iter()
            .map(|v| format!("{v}\n"))
            .collect::<String>();
        let mut lines = tokio::io::BufReader::new(text.as_bytes()).lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            shared.handle(serde_json::from_str(&line).unwrap());
        }
    }

    #[tokio::test]
    async fn adjacent_response_and_result_keep_the_steer() {
        let steer = MessageId::new();
        let (shared, mut ended, mut events) = setup(Some(steer));
        register(&shared);
        batch(
            &shared,
            &[
                wire::control_success("req", json!({"still_queued":[steer.to_string()]})),
                result("turn"),
            ],
        )
        .await;
        assert!(ended.try_recv().is_err());
        assert_eq!(warnings(&mut events), 0);
        shared.handle(json!({"type":"user", "uuid":steer.to_string(), "isReplay":true}));
        shared.handle(result(&steer.to_string()));
        assert_eq!(ended.await.unwrap(), StopReason::Cancelled);
        assert_eq!(warnings(&mut events), 0);
    }

    #[tokio::test]
    async fn cancelled_interrupt_waiter_still_records_queued_answer() {
        let steer = MessageId::new();
        let (shared, mut ended, _) = setup(Some(steer));
        register(&shared);
        shared.pending.lock().unwrap().remove("req");
        shared.handle(wire::control_success(
            "req",
            json!({"still_queued":[steer.to_string()]}),
        ));
        shared.handle(result("turn"));
        assert!(ended.try_recv().is_err());
        shared.handle(result(&steer.to_string()));
        assert_eq!(ended.await.unwrap(), StopReason::Cancelled);
    }

    #[tokio::test]
    async fn empty_or_missing_queue_warns_and_plain_interrupt_ends() {
        for answer in [json!({"still_queued":[]}), json!({})] {
            let (shared, ended, mut events) = setup(Some(MessageId::new()));
            register(&shared);
            batch(
                &shared,
                &[wire::control_success("req", answer), result("turn")],
            )
            .await;
            assert_eq!(ended.await.unwrap(), StopReason::Cancelled);
            assert_eq!(warnings(&mut events), 1);
        }
        let (shared, ended, mut events) = setup(None);
        shared.handle(result("turn"));
        assert_eq!(ended.await.unwrap(), StopReason::Cancelled);
        assert_eq!(warnings(&mut events), 0);
    }

    #[tokio::test]
    async fn control_failure_and_result_do_not_hold_the_turn() {
        let (shared, ended, mut events) = setup(Some(MessageId::new()));
        register(&shared);
        batch(
            &shared,
            &[wire::control_error("req", "failed"), result("turn")],
        )
        .await;
        assert_eq!(ended.await.unwrap(), StopReason::Cancelled);
        assert_eq!(warnings(&mut events), 1);
    }

    #[tokio::test]
    async fn overlapping_interrupts_keep_the_first_queued_answer() {
        let steer = MessageId::new();
        let (shared, mut ended, mut events) = setup(Some(steer));
        register(&shared);
        let (tx, _rx) = oneshot::channel();
        shared
            .turn
            .lock()
            .unwrap()
            .interrupt_requests
            .push("req2".into());
        shared.pending.lock().unwrap().insert("req2".into(), tx);
        batch(
            &shared,
            &[
                wire::control_success("req", json!({"still_queued":[steer.to_string()]})),
                wire::control_success("req2", json!({"still_queued":[]})),
                result("turn"),
            ],
        )
        .await;
        assert!(ended.try_recv().is_err());
        assert_eq!(warnings(&mut events), 0);
        shared.handle(result(&steer.to_string()));
        assert_eq!(ended.await.unwrap(), StopReason::Cancelled);
    }

    #[tokio::test]
    async fn first_interrupt_response_then_result_precedes_second_response() {
        let steer = MessageId::new();
        let (shared, mut ended, mut events) = setup(Some(steer));
        register(&shared);
        let (tx, _rx) = oneshot::channel();
        shared
            .turn
            .lock()
            .unwrap()
            .interrupt_requests
            .push("req2".into());
        shared.pending.lock().unwrap().insert("req2".into(), tx);

        batch(
            &shared,
            &[
                wire::control_success("req", json!({"still_queued":[steer.to_string()]})),
                result("turn"),
            ],
        )
        .await;
        assert!(ended.try_recv().is_err());
        assert_eq!(warnings(&mut events), 0);
        shared.handle(wire::control_success("req2", json!({"still_queued":[]})));
        assert!(shared.turn.lock().unwrap().keep_queued);
        shared.handle(result(&steer.to_string()));
        assert_eq!(ended.await.unwrap(), StopReason::Cancelled);
        assert_eq!(warnings(&mut events), 0);
    }

    #[tokio::test]
    async fn previous_turn_interrupt_response_does_not_affect_next_turn() {
        let (shared, ended, mut events) = setup(Some(MessageId::new()));
        register(&shared);
        shared.handle(result("turn"));
        assert_eq!(ended.await.unwrap(), StopReason::Cancelled);
        assert_eq!(warnings(&mut events), 1);

        let next_steer = MessageId::new();
        let (tx, mut next) = oneshot::channel();
        {
            let mut turn = shared.turn.lock().unwrap();
            assert!(turn.interrupt_requests.is_empty());
            turn.id = Some("next".into());
            turn.waiter = Some(tx);
            turn.interrupted = true;
            turn.steers.push(PendingSteer {
                id: next_steer,
                snippet: "next steer".into(),
            });
            turn.steered.push(next_steer);
        }
        shared.handle(wire::control_success(
            "req",
            json!({"still_queued":["old"]}),
        ));
        assert!(!shared.turn.lock().unwrap().keep_queued);
        assert!(next.try_recv().is_err());
        shared.handle(result("next"));
        assert_eq!(next.await.unwrap(), StopReason::Cancelled);
        assert_eq!(warnings(&mut events), 1);
    }

    #[tokio::test]
    async fn result_before_response_ends_and_warns() {
        let (shared, ended, mut events) = setup(Some(MessageId::new()));
        register(&shared);
        batch(
            &shared,
            &[
                result("turn"),
                wire::control_success("req", json!({"still_queued":["steer"]})),
            ],
        )
        .await;
        assert_eq!(ended.await.unwrap(), StopReason::Cancelled);
        assert_eq!(warnings(&mut events), 1);
    }

    #[tokio::test]
    async fn read_separate_steer_waits_for_its_result_after_failed_prompt() {
        let steer = MessageId::new();
        let (shared, mut ended, _) = setup(Some(steer));
        shared.handle(json!({"type":"system", "subtype":"init"}));
        shared.handle(json!({"type":"user", "uuid":steer.to_string(), "isReplay":true}));
        register(&shared);
        // The steer was already read, so the response can have an empty queue.
        shared.handle(wire::control_success("req", json!({"still_queued":[]})));
        shared.handle(result("turn"));
        assert!(ended.try_recv().is_err());
        shared.handle(result(&steer.to_string()));
        assert_eq!(ended.await.unwrap(), StopReason::Cancelled);
    }

    #[tokio::test]
    async fn separate_steer_keeps_prompt_error_as_stop_reason() {
        let steer = MessageId::new();
        let (shared, mut ended, _) = setup(Some(steer));
        shared.turn.lock().unwrap().interrupted = false;
        shared.handle(json!({"type":"system", "subtype":"init"}));
        shared.handle(json!({"type":"user", "uuid":steer.to_string(), "isReplay":true}));
        shared.handle(result("turn"));
        assert!(ended.try_recv().is_err());
        shared.handle(json!({"type":"result", "subtype":"success", "is_error":false, "user_message_uuid":steer.to_string()}));
        assert_eq!(ended.await.unwrap(), StopReason::Error);
    }

    #[tokio::test]
    async fn unrelated_replay_uuid_does_not_prove_steer_was_read() {
        let (shared, ended, mut events) = setup(Some(MessageId::new()));
        shared.handle(json!({"type":"system", "subtype":"init"}));
        shared.handle(json!({"type":"user", "uuid":MessageId::new().to_string(), "isReplay":true}));
        shared.handle(result("turn"));
        assert_eq!(ended.await.unwrap(), StopReason::Cancelled);
        assert_eq!(warnings(&mut events), 1);
    }

    #[tokio::test]
    async fn codebuddy_failure_without_correlatable_echo_ends_immediately() {
        let (shared, ended, mut events) = setup(Some(MessageId::new()));
        shared.handle(json!({"type":"system", "subtype":"init"}));
        shared.handle(json!({"type":"result", "subtype":"error_during_execution", "is_error":true, "uuid":MessageId::new().to_string()}));
        assert_eq!(ended.await.unwrap(), StopReason::Cancelled);
        assert_eq!(warnings(&mut events), 1);
    }

    #[tokio::test]
    async fn read_merged_steer_does_not_hold_failed_prompt() {
        let steer = MessageId::new();
        let (shared, ended, _) = setup(Some(steer));
        shared.handle(json!({"type":"user", "uuid":steer.to_string(), "isReplay":true}));
        shared.handle(result("turn"));
        assert_eq!(ended.await.unwrap(), StopReason::Cancelled);
    }

    #[tokio::test]
    async fn exit_releases_waiting_separate_steer() {
        let steer = MessageId::new();
        let (shared, mut ended, _) = setup(Some(steer));
        shared.handle(json!({"type":"system", "subtype":"init"}));
        shared.handle(json!({"type":"user", "uuid":steer.to_string(), "isReplay":true}));
        shared.handle(result("turn"));
        assert!(ended.try_recv().is_err());
        shared.ended();
        assert!(ended.await.is_err());
    }

    #[tokio::test]
    async fn second_interrupt_releases_waiting_separate_steer() {
        let steer = MessageId::new();
        let (shared, mut ended, _) = setup(Some(steer));
        shared.handle(json!({"type":"system", "subtype":"init"}));
        shared.handle(json!({"type":"user", "uuid":steer.to_string(), "isReplay":true}));
        shared.handle(result("turn"));
        assert!(ended.try_recv().is_err());
        let session = Arc::new(test_session(shared, test_process()));
        let interrupt = tokio::spawn(async move { session.interrupt().await });
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), ended)
                .await
                .unwrap()
                .unwrap(),
            StopReason::Cancelled
        );
        interrupt.abort();
    }

    #[tokio::test]
    async fn blocked_pipe_breaks_session_and_releases_prompt_and_permission() {
        let (_reader, writer) = tokio::io::duplex(8);
        let process = test_process();
        let (shared, _, mut events) =
            setup_io(None, Box::new(writer), Some(Arc::downgrade(&process)));
        *shared.turn.lock().unwrap() = Turn::default();
        let session = Arc::new(test_session(shared.clone(), process.clone()));
        let prompt_session = session.clone();
        let prompt = tokio::spawn(async move { prompt_session.prompt(&"x".repeat(8192)).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let answer_shared = shared.clone();
        let answer = tokio::spawn(async move {
            answer_shared.answer(json!({"type":"control_request", "request_id":"permission", "request":{"subtype":"can_use_tool", "tool_name":"Bash", "input":{}}})).await;
        });
        assert!(
            tokio::time::timeout(Duration::from_secs(5), prompt)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        tokio::time::timeout(Duration::from_secs(1), answer)
            .await
            .unwrap()
            .unwrap();
        assert!(shared.exited.load(Ordering::SeqCst));
        assert!(process.exit_info().is_some());
        assert!(warnings(&mut events) >= 1);
        tokio::time::timeout(Duration::from_secs(2), session.close())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn stalled_write_releases_a_held_turn() {
        let (_reader, writer) = tokio::io::duplex(8);
        let process = test_process();
        let steer = MessageId::new();
        let (shared, mut ended, _) = setup_io(
            Some(steer),
            Box::new(writer),
            Some(Arc::downgrade(&process)),
        );
        shared.handle(json!({"type":"system", "subtype":"init"}));
        shared.handle(json!({"type":"user", "uuid":steer.to_string(), "isReplay":true}));
        shared.handle(result("turn"));
        assert!(ended.try_recv().is_err());
        assert!(
            shared
                .write(&json!({"payload":"x".repeat(8192)}))
                .await
                .is_err()
        );
        assert!(ended.await.is_err());
        assert!(process.exit_info().is_some());
    }

    #[tokio::test]
    async fn close_aborts_a_stalled_writer_without_waiting_for_stall() {
        let (_reader, writer) = tokio::io::duplex(8);
        let process = test_process();
        let (shared, ended, _) = setup_io(None, Box::new(writer), Some(Arc::downgrade(&process)));
        let session = test_session(shared.clone(), process.clone());
        let writing =
            tokio::spawn(async move { shared.write(&json!({"text":"x".repeat(8192)})).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        tokio::time::timeout(CLEANUP_TIMEOUT + Duration::from_secs(2), session.close())
            .await
            .unwrap();
        assert!(writing.await.unwrap().is_err());
        assert!(ended.await.is_err());
        assert!(process.exit_info().is_some());
    }

    #[tokio::test]
    async fn cancelling_control_does_not_leave_a_partial_json_line() {
        let (mut reader, writer) = tokio::io::duplex(8);
        let (shared, _, _) = setup_io(None, Box::new(writer), None);
        let first = shared.clone();
        let control = tokio::spawn(async move {
            first
                .control(
                    json!({"subtype":"test", "payload":"x".repeat(1024)}),
                    REQUEST_TIMEOUT,
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        control.abort();
        let _ = control.await;
        assert!(shared.pending.lock().unwrap().is_empty());
        let second = shared.clone();
        let write = tokio::spawn(async move { second.write(&json!({"type":"second"})).await });
        let mut bytes = Vec::new();
        while bytes.iter().filter(|&&byte| byte == b'\n').count() < 2 {
            let mut chunk = [0; 8];
            let n = tokio::time::timeout(Duration::from_secs(2), reader.read(&mut chunk))
                .await
                .unwrap()
                .unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&chunk[..n]);
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        write.await.unwrap().unwrap();
        let lines: Vec<_> = bytes
            .split(|&b| b == b'\n')
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(lines.len(), 2);
        let first: Value = serde_json::from_slice(lines[0]).unwrap();
        let second: Value = serde_json::from_slice(lines[1]).unwrap();
        assert_eq!(first["request"]["payload"].as_str().unwrap().len(), 1024);
        assert_eq!(second["type"], "second");
    }

    #[tokio::test]
    async fn cancelled_accepted_prompt_and_steer_keep_their_ids() {
        let (reader, writer) = tokio::io::duplex(8);
        let (shared, _, _) = setup_io(None, Box::new(writer), None);
        *shared.turn.lock().unwrap() = Turn::default();
        let session = Arc::new(test_session(shared.clone(), test_process()));
        let running = {
            let session = session.clone();
            tokio::spawn(async move { session.prompt(&"x".repeat(256)).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        running.abort();
        let _ = running.await;
        let prompt_id = shared
            .turn
            .lock()
            .unwrap()
            .id
            .clone()
            .expect("accepted prompt remains registered");
        let steer_id = MessageId::new();
        let steer = {
            let session = session.clone();
            tokio::spawn(async move {
                session
                    .steer(steer_id, &Prompt::text("y".repeat(256)))
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        steer.abort();
        let _ = steer.await;
        assert!(shared.turn.lock().unwrap().steered.contains(&steer_id));
        let mut lines = BufReader::new(reader).lines();
        let first: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let second: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(first["uuid"], prompt_id);
        assert_eq!(second["uuid"], steer_id.to_string());
        shared.handle(result(&prompt_id));
        assert!(shared.turn.lock().unwrap().waiter.is_none());
    }

    #[tokio::test]
    async fn accepted_prompt_is_not_rolled_back_after_five_seconds_of_progress() {
        let (mut reader, writer) = tokio::io::duplex(8);
        let (shared, _, _) = setup_io(None, Box::new(writer), None);
        *shared.turn.lock().unwrap() = Turn::default();
        let session = Arc::new(test_session(shared.clone(), test_process()));
        let prompt = {
            let session = session.clone();
            tokio::spawn(async move { session.prompt(&"x".repeat(2400)).await })
        };
        let drain = tokio::spawn(async move {
            let mut bytes = Vec::new();
            loop {
                let mut chunk = [0; 8];
                let n = reader.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&chunk[..n]);
                if bytes.ends_with(b"\n") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            serde_json::from_slice::<Value>(&bytes).unwrap()
        });
        tokio::time::sleep(Duration::from_millis(5200)).await;
        assert!(!prompt.is_finished());
        let id = shared
            .turn
            .lock()
            .unwrap()
            .id
            .clone()
            .expect("accepted prompt retained");
        assert!(!shared.exited.load(Ordering::SeqCst));
        let frame = drain.await.unwrap();
        assert_eq!(frame["uuid"], id);
        prompt.abort();
    }

    #[tokio::test]
    async fn saturated_queue_waits_for_permission_and_cancel_before_acceptance_is_safe() {
        let (reader, writer) = tokio::io::duplex(8);
        let (shared, _, _) = setup_io(None, Box::new(writer), None);
        *shared.turn.lock().unwrap() = Turn::default();
        let (ack, _) = oneshot::channel();
        shared
            .writer
            .send(WriteCommand::Frame(
                b"{\"first\":\"12345678901234567890\"}\n".to_vec(),
                ack,
            ))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        for _ in 0..32 {
            let (ack, _) = oneshot::channel();
            shared
                .writer
                .try_send(WriteCommand::Frame(b"{}\n".to_vec(), ack))
                .unwrap();
        }
        let session = Arc::new(test_session(shared.clone(), test_process()));
        let prompt = {
            let session = session.clone();
            tokio::spawn(async move { session.prompt("not accepted").await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(shared.turn.lock().unwrap().id.is_none());
        prompt.abort();
        let _ = prompt.await;
        let steer_id = MessageId::new();
        let (turn_tx, _turn_rx) = oneshot::channel();
        shared.turn.lock().unwrap().waiter = Some(turn_tx);
        let steer = {
            let session = session.clone();
            tokio::spawn(
                async move { session.steer(steer_id, &Prompt::text("not accepted")).await },
            )
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        steer.abort();
        let _ = steer.await;
        assert!(!shared.turn.lock().unwrap().steered.contains(&steer_id));
        let answer = {
            let shared = shared.clone();
            tokio::spawn(async move {
                shared.answer(json!({"type":"control_request", "request_id":"permission", "request":{"subtype":"can_use_tool", "tool_name":"Bash", "input":{}}})).await;
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!answer.is_finished());
        let mut lines = BufReader::new(reader).lines();
        for _ in 0..33 {
            lines.next_line().await.unwrap().unwrap();
        }
        let response: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(response["response"]["request_id"], "permission");
        answer.await.unwrap();
        assert!(!shared.exited.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn review_plain_failed_interrupt_must_not_end_next_turn() {
        let (shared, mut ended, _) = setup(None);
        let (tx, _rx) = oneshot::channel();
        shared.pending.lock().unwrap().insert("req".into(), tx);
        shared.handle(wire::control_error("req", "failed"));
        assert!(ended.try_recv().is_err());
        shared.handle(result("turn"));
        assert_eq!(ended.await.unwrap(), StopReason::Cancelled);
        let (tx, mut next) = oneshot::channel();
        {
            let mut turn = shared.turn.lock().unwrap();
            turn.id = Some("next".into());
            turn.waiter = Some(tx);
        }
        assert!(next.try_recv().is_err());
        shared.handle(result("next"));
        assert_eq!(next.await.unwrap(), StopReason::Error);
    }
}
