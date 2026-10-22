//! `codex-native`: drives `codex app-server` (JSON-RPC over stdio).
//!
//! One app-server process per Profile and environment (the environment carries the
//! account's `CODEX_HOME`, so accounts never share a process); every session is a thread in
//! that process. The process ends when its last session is closed or suspended. Threads
//! persist in Codex's own rollout files, so a suspended session resumes by thread id in a
//! fresh process. Protocol notes: docs/research/codex-app-server.md.

pub mod wire;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use crossagents_core::{
    Capabilities, CoreError, Driver, DriverKind, DriverSession, PermissionDecision,
    PermissionHandler, PermissionPolicy, SessionEvent, SessionEventKind, SessionSpec, TurnOutcome,
};
use serde_json::{Value, json};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};

use self::wire::{ThreadSettings, TurnEnd, method};
use crate::jsonrpc::{RpcError, RpcHandler, RpcPeer};
use crate::process::{StderrTail, hide_console};

const INIT_TIMEOUT: Duration = Duration::from_secs(60);
/// Control requests answer quickly; turns are awaited separately.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// For interrupting and unsubscribing. Codex never answers an interrupt of a turn that has
/// already ended, which a stop racing the turn's end can send.
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

/// The `codex-native` driver for one Profile.
pub struct CodexNativeDriver {
    name: String,
    command: Vec<String>,
    models: Vec<String>,
    pool: tokio::sync::Mutex<HashMap<ProcessKey, Weak<AppServer>>>,
}

/// Sessions with the same launch command and environment share a process.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProcessKey {
    command: Vec<String>,
    env: Vec<(String, String)>,
}

impl CodexNativeDriver {
    pub const DEFAULT_COMMAND: [&str; 2] = ["codex", "app-server"];

    /// `command` launches the app-server; empty means `codex app-server`.
    pub fn new(name: impl Into<String>, command: Vec<String>, models: Vec<String>) -> Self {
        let command = if command.is_empty() {
            Self::DEFAULT_COMMAND.map(String::from).to_vec()
        } else {
            command
        };
        Self {
            name: name.into(),
            command,
            models,
            pool: tokio::sync::Mutex::default(),
        }
    }

    /// App-server processes currently running for this Profile.
    pub async fn process_count(&self) -> usize {
        self.pool
            .lock()
            .await
            .values()
            .filter_map(Weak::upgrade)
            .filter(|s| !s.router.exited.load(Ordering::SeqCst))
            .count()
    }

    async fn server_for(&self, spec: &SessionSpec) -> Result<Arc<AppServer>, CoreError> {
        let key = ProcessKey {
            command: spec.command.clone().unwrap_or_else(|| self.command.clone()),
            env: wire::sorted_env(&spec.env),
        };
        let mut pool = self.pool.lock().await;
        pool.retain(|_, server| server.strong_count() > 0);
        if let Some(server) = pool.get(&key).and_then(Weak::upgrade)
            && !server.router.exited.load(Ordering::SeqCst)
        {
            return Ok(server);
        }
        let server = AppServer::spawn(&key).await?;
        pool.insert(key, Arc::downgrade(&server));
        Ok(server)
    }
}

fn settings_for(spec: &SessionSpec) -> ThreadSettings {
    let (sandbox, approval_policy) = wire::policies(&spec.agent.config, spec.permission);
    ThreadSettings {
        model: spec.agent.model.clone(),
        cwd: spec.agent.cwd.display().to_string(),
        approval_policy,
        sandbox,
        mcp_servers: spec.mcp_servers.clone(),
    }
}

#[async_trait]
impl Driver for CodexNativeDriver {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> DriverKind {
        DriverKind::CodexNative
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            interrupt: true,
            mcp: true,
            multi_session: true,
            resume: true,
            fork: true,
            rollback: true,
            // thread/revert only rewrites history (docs/research/codex-app-server.md §4).
            revert_files: false,
            set_model: true,
            structured_diff: true,
            usage: true,
            rate_limits: true,
            permission_callback: true,
            compaction: true,
        }
    }

    /// The configured models, else the ones Codex lists for the default login.
    fn models(&self) -> Vec<String> {
        if self.models.is_empty() {
            return default_codex_home()
                .map(|home| cached_models(&home))
                .unwrap_or_default();
        }
        self.models.clone()
    }

    async fn open(&self, spec: SessionSpec) -> Result<Box<dyn DriverSession>, CoreError> {
        let server = self.server_for(&spec).await?;
        let settings = settings_for(&spec);
        let response = server
            .request(
                method::THREAD_START,
                wire::thread_start(&settings, spec.instructions.as_deref()),
            )
            .await?;
        let thread_id = wire::thread_id_of(&response)
            .ok_or_else(|| CoreError::Agent("thread/start returned no thread id".into()))?
            .to_owned();
        Ok(Box::new(CodexSession::attach(
            server, thread_id, &spec, settings, &response,
        )))
    }

    async fn resume(
        &self,
        session_id: &str,
        spec: SessionSpec,
    ) -> Result<Box<dyn DriverSession>, CoreError> {
        let server = self.server_for(&spec).await?;
        let settings = settings_for(&spec);
        let response = server
            .request(
                method::THREAD_RESUME,
                wire::thread_resume(session_id, &settings),
            )
            .await?;
        let thread_id = wire::thread_id_of(&response)
            .unwrap_or(session_id)
            .to_owned();
        Ok(Box::new(CodexSession::attach(
            server, thread_id, &spec, settings, &response,
        )))
    }
}

/// One running `codex app-server`.
struct AppServer {
    peer: RpcPeer,
    child: Mutex<Option<Child>>,
    router: Arc<Router>,
}

impl AppServer {
    async fn spawn(key: &ProcessKey) -> Result<Arc<Self>, CoreError> {
        let (program, args) = key
            .command
            .split_first()
            .ok_or_else(|| CoreError::Agent("empty codex launch command".into()))?;
        let program = locate_codex(program);
        let mut command = Command::new(&program);
        command
            .args(args)
            .envs(key.env.iter().cloned())
            // Not a session directory: on Windows a process's working directory cannot be
            // deleted, and worker directories come and go. Threads carry their own cwd.
            .current_dir(std::env::temp_dir())
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

        let router = Arc::new(Router::default());
        let (peer, reader) = RpcPeer::spawn(
            stdout,
            stdin,
            Arc::new(ServerHandler {
                router: router.clone(),
            }),
        );
        tokio::spawn({
            let router = router.clone();
            let tail = tail.clone();
            async move {
                let _ = reader.await;
                router.exited.store(true, Ordering::SeqCst);
                if router.closing.load(Ordering::SeqCst) {
                    return;
                }
                let message = tail.describe("codex app-server exited").await;
                for sink in router.drain() {
                    sink.end_turn();
                    sink.emit(
                        SessionEventKind::Exited {
                            message: message.clone(),
                        },
                        None,
                    );
                }
            }
        });

        let server = Arc::new(Self {
            peer,
            child: Mutex::new(Some(child)),
            router,
        });
        let init = tokio::time::timeout(
            INIT_TIMEOUT,
            server
                .peer
                .request::<_, Value>(method::INITIALIZE, wire::initialize_params()),
        )
        .await;
        match init {
            Ok(Ok(info)) => {
                tracing::debug!(
                    user_agent = info["userAgent"].as_str().unwrap_or_default(),
                    codex_home = info["codexHome"].as_str().unwrap_or_default(),
                    "codex app-server ready"
                );
            }
            Ok(Err(err)) => {
                return Err(CoreError::Agent(
                    tail.describe(&format!("codex app-server handshake failed: {err}"))
                        .await,
                ));
            }
            Err(_) => {
                return Err(CoreError::Agent(
                    "codex app-server handshake timed out".into(),
                ));
            }
        }
        server
            .peer
            .notify_without_params(method::INITIALIZED)
            .map_err(|e| CoreError::Agent(e.to_string()))?;
        Ok(server)
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, CoreError> {
        match tokio::time::timeout(
            REQUEST_TIMEOUT,
            self.peer.request::<_, Value>(method, params),
        )
        .await
        {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(err)) => Err(CoreError::Agent(format!("{method}: {err}"))),
            Err(_) => Err(CoreError::Agent(format!("{method}: no answer"))),
        }
    }
}

impl Drop for AppServer {
    fn drop(&mut self) {
        self.router.closing.store(true, Ordering::SeqCst);
        if let Some(mut child) = self.child.lock().expect("child lock").take() {
            let _ = child.start_kill();
        }
    }
}

/// Routes the process's messages to the sessions, by thread id.
#[derive(Default)]
struct Router {
    threads: Mutex<HashMap<String, Arc<ThreadSink>>>,
    /// We are stopping the process: its exit is not a failure.
    closing: AtomicBool,
    exited: AtomicBool,
}

impl Router {
    fn get(&self, thread_id: &str) -> Option<Arc<ThreadSink>> {
        self.lock().get(thread_id).cloned()
    }

    fn insert(&self, thread_id: &str, sink: Arc<ThreadSink>) {
        self.lock().insert(thread_id.to_owned(), sink);
    }

    /// Removes the thread's sink if it is still `sink` (a resumed copy may have replaced it).
    fn remove(&self, thread_id: &str, sink: &Arc<ThreadSink>) {
        let mut threads = self.lock();
        if threads.get(thread_id).is_some_and(|s| Arc::ptr_eq(s, sink)) {
            threads.remove(thread_id);
        }
    }

    fn all(&self) -> Vec<Arc<ThreadSink>> {
        self.lock().values().cloned().collect()
    }

    fn drain(&self) -> Vec<Arc<ThreadSink>> {
        self.lock().drain().map(|(_, s)| s).collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<ThreadSink>>> {
        self.threads.lock().expect("router lock")
    }
}

/// One session's end of the router.
struct ThreadSink {
    events: mpsc::UnboundedSender<SessionEvent>,
    permission: PermissionPolicy,
    permissions: Option<Arc<dyn PermissionHandler>>,
    turn: Mutex<TurnState>,
}

#[derive(Default)]
struct TurnState {
    /// The running turn, once its id is known.
    active: Option<String>,
    /// Wakes `prompt` when the turn completes.
    waiter: Option<oneshot::Sender<TurnEnd>>,
    /// `interrupt` came before the turn id did.
    interrupt_requested: bool,
}

impl ThreadSink {
    fn emit(&self, kind: SessionEventKind, raw: Option<&Value>) {
        let event = SessionEvent {
            kind,
            raw: raw.cloned(),
        };
        let _ = self.events.send(event);
    }

    fn turn(&self) -> std::sync::MutexGuard<'_, TurnState> {
        self.turn.lock().expect("turn lock")
    }

    /// Forgets the running turn; a waiting `prompt` fails.
    fn end_turn(&self) {
        let mut turn = self.turn();
        turn.active = None;
        turn.waiter = None;
    }
}

struct ServerHandler {
    router: Arc<Router>,
}

#[async_trait]
impl RpcHandler for ServerHandler {
    async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let Some(request) = wire::permission_request(method, &params) else {
            return Err(RpcError::method_not_found(method));
        };
        let sink = wire::thread_of(&params).and_then(|id| self.router.get(id));
        let decision = match &sink {
            Some(sink) => match &sink.permissions {
                Some(handler) => handler.decide(&request).await,
                None => sink.permission.decide(),
            },
            // Not one of ours: never approve on someone else's behalf.
            None => PermissionDecision::Deny,
        };
        let (answer, given) = wire::permission_response(method, &params, decision);
        if let Some(sink) = sink {
            sink.emit(
                SessionEventKind::PermissionRequested {
                    request,
                    decision: given,
                },
                Some(&params),
            );
        }
        Ok(answer)
    }

    async fn notification(&self, method: &str, params: Value) {
        let Some(thread_id) = wire::thread_of(&params) else {
            // Account-wide: every session of this account sees it.
            if method == method::RATE_LIMITS {
                for kind in wire::translate(method, &params) {
                    for sink in self.router.all() {
                        sink.emit(kind.clone(), Some(&params));
                    }
                }
            }
            return;
        };
        let Some(sink) = self.router.get(thread_id) else {
            return;
        };
        if method == method::TURN_STARTED
            && let Some(id) = wire::turn_started_id(&params)
        {
            sink.turn().active = Some(id.to_owned());
        }
        for kind in wire::translate(method, &params) {
            sink.emit(kind, Some(&params));
        }
        if method == method::TURN_COMPLETED
            && let Some(end) = wire::turn_end(&params)
        {
            let mut turn = sink.turn();
            turn.active = None;
            if let Some(waiter) = turn.waiter.take() {
                let _ = waiter.send(end);
            }
        }
    }
}

/// A Codex thread.
pub struct CodexSession {
    server: Arc<AppServer>,
    thread_id: String,
    sink: Arc<ThreadSink>,
    settings: ThreadSettings,
    /// Model and effort for the next turns, from `set_model` (and `config.effort`).
    next: Mutex<(Option<String>, Option<String>)>,
    closed: AtomicBool,
}

impl CodexSession {
    fn attach(
        server: Arc<AppServer>,
        thread_id: String,
        spec: &SessionSpec,
        settings: ThreadSettings,
        response: &Value,
    ) -> Self {
        let sink = Arc::new(ThreadSink {
            events: spec.events.clone(),
            permission: spec.permission,
            permissions: spec.permissions.clone(),
            turn: Mutex::default(),
        });
        server.router.insert(&thread_id, sink.clone());
        sink.emit(
            SessionEventKind::Started {
                session_id: thread_id.clone(),
            },
            response.get("thread"),
        );
        let effort = spec
            .agent
            .config
            .get("effort")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Self {
            server,
            thread_id,
            sink,
            settings,
            next: Mutex::new((None, effort)),
            closed: AtomicBool::new(false),
        }
    }

    fn check_open(&self) -> Result<(), CoreError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(CoreError::Conflict(format!(
                "session {} is closed",
                self.thread_id
            )));
        }
        if self.server.router.exited.load(Ordering::SeqCst) {
            return Err(CoreError::Agent("codex app-server exited".into()));
        }
        Ok(())
    }

    fn turn_running(&self) -> bool {
        let turn = self.sink.turn();
        turn.active.is_some() || turn.waiter.is_some()
    }

    async fn send_interrupt(&self, turn_id: &str) -> Result<(), CoreError> {
        let request = self.server.request(
            method::TURN_INTERRUPT,
            json!({ "threadId": self.thread_id, "turnId": turn_id }),
        );
        match tokio::time::timeout(CLEANUP_TIMEOUT, request).await {
            Ok(result) => result.map(|_| ()),
            Err(_) => Ok(()), // the turn ended meanwhile
        }
    }

    /// Stops listening to the thread; it stays in Codex's store.
    async fn release(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if !self.server.router.exited.load(Ordering::SeqCst) {
            let request = self.server.request(
                method::THREAD_UNSUBSCRIBE,
                json!({ "threadId": self.thread_id }),
            );
            let _ = tokio::time::timeout(CLEANUP_TIMEOUT, request).await;
        }
        self.sink.end_turn();
        self.server.router.remove(&self.thread_id, &self.sink);
    }
}

impl Drop for CodexSession {
    fn drop(&mut self) {
        self.server.router.remove(&self.thread_id, &self.sink);
    }
}

#[async_trait]
impl DriverSession for CodexSession {
    fn session_id(&self) -> &str {
        &self.thread_id
    }

    async fn prompt(&self, text: &str) -> Result<TurnOutcome, CoreError> {
        self.check_open()?;
        let (tx, rx) = oneshot::channel();
        {
            let mut turn = self.sink.turn();
            if turn.active.is_some() || turn.waiter.is_some() {
                // Codex would merge this input into the running turn; the hub queues.
                return Err(CoreError::Conflict("a turn is already running".into()));
            }
            *turn = TurnState {
                waiter: Some(tx),
                ..TurnState::default()
            };
        }
        let (model, effort) = self.next.lock().expect("next lock").clone();
        let params = wire::turn_start(&self.thread_id, text, model.as_deref(), effort.as_deref());
        let response = match self.server.request(method::TURN_START, params).await {
            Ok(response) => response,
            Err(err) => {
                self.sink.end_turn();
                return Err(err);
            }
        };
        if let Some(id) = wire::turn_started_id(&response) {
            let interrupt = {
                let mut turn = self.sink.turn();
                // Unless the turn already finished.
                if turn.waiter.is_some() && turn.active.is_none() {
                    turn.active = Some(id.to_owned());
                }
                turn.waiter.is_some() && std::mem::take(&mut turn.interrupt_requested)
            };
            if interrupt {
                self.send_interrupt(id).await?;
            }
        }
        let end = rx.await.map_err(|_| {
            CoreError::Agent(
                "the turn ended without a result (session closed or app-server exited)".into(),
            )
        })?;
        self.refresh_limits().await;
        Ok(TurnOutcome {
            stop_reason: wire::stop_reason(&end.status),
            turn_id: Some(end.turn_id),
        })
    }

    async fn interrupt(&self) -> Result<(), CoreError> {
        let active = {
            let mut turn = self.sink.turn();
            if turn.active.is_none() && turn.waiter.is_some() {
                turn.interrupt_requested = true;
            }
            turn.active.clone()
        };
        match active {
            Some(turn_id) => self.send_interrupt(&turn_id).await,
            None => Ok(()),
        }
    }

    async fn suspend(&self) -> Result<(), CoreError> {
        if self.turn_running() {
            return Err(CoreError::Conflict(
                "cannot suspend while a turn runs".into(),
            ));
        }
        self.release().await;
        Ok(())
    }

    async fn fork(
        &self,
        at_turn: Option<&str>,
        spec: SessionSpec,
    ) -> Result<Box<dyn DriverSession>, CoreError> {
        self.check_open()?;
        // The fork lives in this process: its history is in this account's CODEX_HOME.
        let settings = settings_for(&spec);
        let response = self
            .server
            .request(
                method::THREAD_FORK,
                wire::thread_fork(
                    &self.thread_id,
                    at_turn,
                    &settings,
                    spec.instructions.as_deref(),
                ),
            )
            .await?;
        let thread_id = wire::thread_id_of(&response)
            .ok_or_else(|| CoreError::Agent("thread/fork returned no thread id".into()))?
            .to_owned();
        Ok(Box::new(CodexSession::attach(
            self.server.clone(),
            thread_id,
            &spec,
            settings,
            &response,
        )))
    }

    async fn rollback(&self, before_turn: &str, revert_files: bool) -> Result<(), CoreError> {
        if revert_files {
            return Err(CoreError::Unsupported(
                "codex-native cannot undo file changes when rolling back".into(),
            ));
        }
        self.check_open()?;
        if self.turn_running() {
            return Err(CoreError::Conflict(
                "cannot roll back while a turn runs".into(),
            ));
        }
        self.server
            .request(
                method::THREAD_REVERT,
                json!({ "threadId": self.thread_id, "beforeTurnId": before_turn }),
            )
            .await
            .map(|_| ())
    }

    async fn set_model(&self, model: Option<&str>, effort: Option<&str>) -> Result<(), CoreError> {
        let mut next = self.next.lock().expect("next lock");
        if let Some(model) = model {
            next.0 = Some(model.to_owned());
        }
        if let Some(effort) = effort {
            next.1 = Some(effort.to_owned());
        }
        Ok(())
    }

    async fn compact(&self) -> Result<(), CoreError> {
        self.check_open()?;
        self.server
            .request(
                method::THREAD_COMPACT,
                json!({ "threadId": self.thread_id }),
            )
            .await
            .map(|_| ())
    }

    async fn shutdown(&self) {
        let _ = self.interrupt().await;
        self.release().await;
    }
}

impl CodexSession {
    /// Reads every limit and the reset credits (per-call updates carry only the limit in
    /// use) and reports them. Best effort: an older Codex without the call is fine.
    async fn refresh_limits(&self) {
        let request = self.server.request(method::RATE_LIMITS_READ, json!({}));
        if let Ok(Ok(result)) = tokio::time::timeout(CLEANUP_TIMEOUT, request).await
            && let Some(limits) = wire::rate_limits_read(&result)
        {
            self.sink
                .emit(SessionEventKind::RateLimits { limits }, Some(&result));
        }
    }

    /// The settings the thread was opened with (cwd, sandbox, approvals, MCP servers).
    pub fn settings(&self) -> &ThreadSettings {
        &self.settings
    }
}

/// The native `codex` executable behind `program`. npm installs `codex` as a Node script
/// (`codex.cmd` / `codex.ps1` on Windows) that starts the real binary as a child; killing
/// the script would leave that child running, so we start the binary ourselves.
/// Without a `codex` on `PATH`, the binary bundled with the Codex desktop app is used.
pub fn locate_codex(program: &str) -> PathBuf {
    let found = find_in_path(program);
    if found.components().count() == 1
        && program == "codex"
        && let Some(bundled) = bundled_codex()
    {
        return bundled;
    }
    native_behind_shim(&found).unwrap_or(found)
}

/// `$CODEX_HOME`, else `~/.codex`.
fn default_codex_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        return Some(PathBuf::from(home));
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
    Some(PathBuf::from(home).join(".codex"))
}

/// The models Codex offers in its picker, from the `models_cache.json` it keeps in `home`,
/// in its priority order.
pub fn cached_models(home: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(home.join("models_cache.json")) else {
        return Vec::new();
    };
    let Ok(cache) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let mut models: Vec<(i64, String)> = cache["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["visibility"].as_str() == Some("list"))
        .filter_map(|m| {
            Some((
                m["priority"].as_i64().unwrap_or(i64::MAX),
                m["slug"].as_str()?.to_owned(),
            ))
        })
        .collect();
    models.sort();
    models.into_iter().map(|(_, slug)| slug).collect()
}

/// The newest `codex` the Codex desktop app keeps for itself.
fn bundled_codex() -> Option<PathBuf> {
    let candidates: Vec<PathBuf> = if cfg!(windows) {
        // %LOCALAPPDATA%\OpenAI\Codex\bin\<build>\codex.exe
        let bin = PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
            .join("OpenAI")
            .join("Codex")
            .join("bin");
        std::fs::read_dir(bin)
            .ok()?
            .flatten()
            .map(|e| e.path().join("codex.exe"))
            .collect()
    } else {
        vec![PathBuf::from(
            "/Applications/Codex.app/Contents/Resources/codex",
        )]
    };
    candidates
        .into_iter()
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .max()
        .map(|(_, p)| p)
}

fn find_in_path(program: &str) -> PathBuf {
    let path = PathBuf::from(program);
    if path.components().count() > 1 {
        return path;
    }
    let Some(dirs) = std::env::var_os("PATH") else {
        return path;
    };
    let exts: &[&str] = if cfg!(windows) {
        &["exe", "cmd", "bat"]
    } else {
        &[""]
    };
    for dir in std::env::split_paths(&dirs) {
        for ext in exts {
            let candidate = if ext.is_empty() {
                dir.join(program)
            } else {
                dir.join(program).with_extension(ext)
            };
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    path
}

fn native_behind_shim(found: &Path) -> Option<PathBuf> {
    let ext = found
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase);
    let package = if matches!(ext.as_deref(), Some("cmd" | "ps1" | "bat")) {
        found
            .parent()?
            .join("node_modules")
            .join("@openai")
            .join("codex")
    } else {
        let real = std::fs::canonicalize(found).ok()?;
        if real.file_name()? != "codex.js" {
            return None;
        }
        real.parent()?.parent()?.to_path_buf()
    };
    let exe = if cfg!(windows) { "codex.exe" } else { "codex" };
    let mut vendors = vec![package.join("vendor")];
    // Platform packages: nested under the package, or hoisted next to it.
    for scope in [
        package.join("node_modules").join("@openai"),
        package.parent()?.to_path_buf(),
    ] {
        let Ok(entries) = std::fs::read_dir(&scope) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("codex-") {
                vendors.push(entry.path().join("vendor"));
            }
        }
    }
    for vendor in vendors {
        let Ok(triples) = std::fs::read_dir(&vendor) else {
            continue;
        };
        for triple in triples.flatten() {
            for dir in ["bin", "codex"] {
                let candidate = triple.path().join(dir).join(exe);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_picker_models_from_the_cache() {
        let home = tempfile::tempdir().unwrap();
        assert!(cached_models(home.path()).is_empty());
        let cache = serde_json::json!({ "models": [
            { "slug": "later", "visibility": "list", "priority": 9 },
            { "slug": "hidden", "visibility": "hide", "priority": 1 },
            { "slug": "first", "visibility": "list", "priority": 5 },
        ]});
        std::fs::write(home.path().join("models_cache.json"), cache.to_string()).unwrap();
        assert_eq!(cached_models(home.path()), ["first", "later"]);
    }

    #[test]
    fn finds_the_binary_behind_an_npm_shim() {
        let dir = tempfile::tempdir().unwrap();
        let exe = if cfg!(windows) { "codex.exe" } else { "codex" };
        let native = dir.path().join(
            "node_modules/@openai/codex/node_modules/@openai/codex-test-x64/vendor/triple/bin",
        );
        std::fs::create_dir_all(&native).unwrap();
        std::fs::write(native.join(exe), "").unwrap();
        let shim = dir.path().join("codex.cmd");
        std::fs::write(&shim, "").unwrap();
        assert_eq!(native_behind_shim(&shim).unwrap(), native.join(exe));
        // A real binary is used as is.
        assert!(native_behind_shim(&native.join(exe)).is_none());
        assert_eq!(
            locate_codex(&native.join(exe).display().to_string()),
            native.join(exe)
        );
    }
}
