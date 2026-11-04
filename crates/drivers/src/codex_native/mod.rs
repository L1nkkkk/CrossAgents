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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use crossagents_core::{
    AttachmentSupport, Capabilities, CliStatus, CoreError, Driver, DriverKind, DriverSession,
    MessageId, PermissionDecision, PermissionHandler, PermissionPolicy, Prompt, SessionEvent,
    SessionEventKind, SessionSpec, TurnOutcome,
};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc, oneshot};

use self::wire::{ThreadSettings, TurnEnd, method};
use crate::jsonrpc::{RpcError, RpcHandler, RpcPeer};
use crate::launch::{self, LaunchSpec, Launched, ProcessHandle};
use crate::locate::Cli;

const INIT_TIMEOUT: Duration = Duration::from_secs(60);
/// Control requests answer quickly; turns are awaited separately.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// For interrupting and unsubscribing. Codex never answers an interrupt of a turn that has
/// already ended, which a stop racing the turn's end can send.
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

/// The `codex-native` driver for one Profile.
pub struct CodexNativeDriver {
    name: String,
    /// The launch command as configured (`codex app-server` by default): `app-server` waits
    /// for a program, which the description finds when a session starts.
    command: Vec<String>,
    models: Vec<String>,
    pool: tokio::sync::Mutex<HashMap<ProcessKey, Weak<AppServer>>>,
    /// The CLI the description names, found whenever a process starts (docs/DESIGN.md §9.3).
    cli: Cli,
}

/// Sessions with the same launch command and environment share a process.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProcessKey {
    command: Vec<String>,
    env: Vec<(String, String)>,
}

impl CodexNativeDriver {
    /// `cli` is the description's CLI: `native_command` when it gives one, else the `codex`
    /// it describes. `app-server` is added to the program.
    pub fn new(name: impl Into<String>, cli: Cli, models: Vec<String>) -> Self {
        let command = if cli.configured().is_empty() {
            // Unresolved on purpose: the program is found when a process starts, and the
            // key a process is taken over by stays the same across an update.
            vec![cli.default_name().unwrap_or_default(), "app-server".into()]
        } else {
            cli.configured().to_vec()
        };
        Self {
            name: name.into(),
            command,
            models,
            pool: tokio::sync::Mutex::default(),
            cli,
        }
    }

    /// The CLI behind this driver, as this machine has it.
    pub async fn status(&self) -> CliStatus {
        self.cli.status().await
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
        let key = self.key_for(spec);
        let mut pool = self.pool.lock().await;
        pool.retain(|_, server| server.strong_count() > 0);
        if let Some(server) = pool.get(&key).and_then(Weak::upgrade)
            && !server.router.exited.load(Ordering::SeqCst)
        {
            return Ok(server);
        }
        let server = AppServer::spawn(&key, self.command_for(&key), self.owner_of(&key)).await?;
        pool.insert(key, Arc::downgrade(&server));
        Ok(server)
    }

    /// The argv a process is started with: the key's program, found now.
    fn command_for(&self, key: &ProcessKey) -> Vec<String> {
        if key.command.first().is_some_and(String::is_empty) {
            // The description names no program to keep unresolved (no `commands`): whatever
            // it finds when the process starts, with the arguments the key carries.
            let mut found = self.cli.command().unwrap_or_default();
            found.extend(key.command.iter().skip(1).cloned());
            return found;
        }
        self.cli.launch(&key.command)
    }

    fn key_for(&self, spec: &SessionSpec) -> ProcessKey {
        ProcessKey {
            command: spec.command.clone().unwrap_or_else(|| self.command.clone()),
            env: wire::sorted_env(&spec.env),
        }
    }

    /// Who may take the process over: this Profile, for the same command and environment.
    fn owner_of(&self, key: &ProcessKey) -> String {
        let text = serde_json::to_string(&(&key.command, &key.env)).expect("key serializes");
        format!("codex-native:{}:{:016x}", self.name, fnv1a(text.as_bytes()))
    }

    /// The process a previous hub left for `spec`'s command and environment, taken over
    /// (or already taken over for another session).
    async fn adopted_server(
        &self,
        spec: &SessionSpec,
    ) -> Result<Option<Arc<AppServer>>, CoreError> {
        let key = self.key_for(spec);
        let mut pool = self.pool.lock().await;
        if let Some(server) = pool.get(&key).and_then(Weak::upgrade)
            && !server.router.exited.load(Ordering::SeqCst)
        {
            return Ok(server.router.adopted.then_some(server));
        }
        let Some(launched) = launch::adopt(&self.owner_of(&key)).await else {
            return Ok(None);
        };
        let server = AppServer::start(launched?, true);
        pool.insert(key, Arc::downgrade(&server));
        Ok(Some(server))
    }
}

/// FNV-1a: a stable hash for naming processes across hub restarts.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, b| {
        (hash ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
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
            // `turn/steer` with the running turn's id (verified against codex-cli
            // 0.166.0-alpha.5.1, see docs/research/codex-app-server.md §3.1).
            steer: true,
            mcp: true,
            multi_session: true,
            resume: true,
            move_cwd: true,
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
            // Images go as `localImage`; text is inlined or named by path. Codex has no
            // document input, so a PDF would only be a path it cannot read as one.
            attachments: Some(AttachmentSupport {
                image: true,
                pdf: false,
                text: true,
            }),
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

    async fn adopt(
        &self,
        session_id: &str,
        turn_running: bool,
        spec: SessionSpec,
    ) -> Result<Option<Box<dyn DriverSession>>, CoreError> {
        let Some(server) = self.adopted_server(&spec).await? else {
            return Ok(None);
        };
        // The thread may have been released (or never loaded) in that process; then the
        // hub resumes it there like any other.
        if let Ok(loaded) = server.request(method::THREAD_LOADED_LIST, json!({})).await
            && let Some(ids) = loaded["data"].as_array()
            && !ids.iter().any(|id| id.as_str() == Some(session_id))
        {
            return Ok(None);
        }
        let settings = settings_for(&spec);
        Ok(Some(Box::new(CodexSession::adopt(
            server,
            session_id.to_owned(),
            &spec,
            settings,
            turn_running,
        ))))
    }
}

/// One running `codex app-server`.
struct AppServer {
    peer: RpcPeer,
    process: ProcessHandle,
    router: Arc<Router>,
}

impl AppServer {
    async fn spawn(
        key: &ProcessKey,
        command: Vec<String>,
        owner: String,
    ) -> Result<Arc<Self>, CoreError> {
        let (program, args) = command
            .split_first()
            .ok_or_else(|| CoreError::Agent("empty codex launch command".into()))?;
        let launched = launch::launch(LaunchSpec {
            program: PathBuf::from(program),
            args: args.to_vec(),
            env: key.env.clone(),
            // Not a session directory: on Windows a process's working directory cannot be
            // deleted, and worker directories come and go. Threads carry their own cwd.
            cwd: std::env::temp_dir(),
            owner: Some(owner),
            label: "codex app-server".into(),
            ..LaunchSpec::default()
        })
        .await?;
        let tail = launched.stderr.clone();
        let server = Self::start(launched, false);
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

    /// Talks to a started process. An `adopted` one is past its handshake, and what it
    /// sent the previous hub (answers to that hub's requests among it) arrives first.
    fn start(launched: Launched, adopted: bool) -> Arc<Self> {
        let Launched {
            stdin,
            stdout,
            stderr: tail,
            process,
        } = launched;
        let router = Arc::new(Router {
            adopted,
            ..Router::default()
        });
        let handler = Arc::new(ServerHandler {
            router: router.clone(),
        });
        // Request ids no previous hub used.
        let first_id = if adopted {
            chrono::Utc::now().timestamp_millis() * 100
        } else {
            1
        };
        let (peer, reader) = RpcPeer::spawn_from(stdout, stdin, handler, first_id);
        tokio::spawn({
            let router = router.clone();
            let exit = process.watch();
            async move {
                let _ = reader.await;
                router.exited.store(true, Ordering::SeqCst);
                if router.closing.load(Ordering::SeqCst) {
                    return;
                }
                let info = exit.within(Duration::from_secs(2)).await;
                let what = match info {
                    Some(info) => info.describe("codex app-server"),
                    None => "codex app-server exited".to_owned(),
                };
                let message = tail.describe(&what).await;
                for sink in router.drain() {
                    sink.end_turn();
                    sink.emit(
                        SessionEventKind::Exited {
                            message: message.clone(),
                            cause: info.map(|i| i.cause),
                            code: info.and_then(|i| i.code),
                        },
                        None,
                    );
                }
            }
        });
        Arc::new(Self {
            peer,
            process,
            router,
        })
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
        // Its exit is ours, not a failure; dropping `process` stops it (unless the hub is
        // leaving it running for the next one).
        self.router.closing.store(true, Ordering::SeqCst);
    }
}

/// Messages for a thread no session has claimed yet, kept for it (adopted processes only).
const ORPHANS_KEPT: usize = 5_000;
/// How long a permission request for an unclaimed thread waits for its session.
const CLAIM_WAIT: Duration = Duration::from_secs(30);

/// Routes the process's messages to the sessions, by thread id.
#[derive(Default)]
struct Router {
    threads: Mutex<HashMap<String, Arc<ThreadSink>>>,
    /// We are stopping the process: its exit is not a failure.
    closing: AtomicBool,
    exited: AtomicBool,
    /// Taken over from a previous hub: messages for threads whose sessions have not been
    /// taken over yet wait in `orphans`.
    adopted: bool,
    orphans: Mutex<HashMap<String, Vec<(String, Value)>>>,
    claimed: Notify,
}

impl Router {
    fn get(&self, thread_id: &str) -> Option<Arc<ThreadSink>> {
        self.lock().get(thread_id).cloned()
    }

    /// Adds the thread's session and hands it what arrived for it before.
    fn insert(&self, thread_id: &str, sink: Arc<ThreadSink>) {
        self.lock().insert(thread_id.to_owned(), sink.clone());
        let waiting = self
            .orphans
            .lock()
            .expect("orphans lock")
            .remove(thread_id)
            .unwrap_or_default();
        for (method, params) in waiting {
            route(self, &sink, &method, &params);
        }
        self.claimed.notify_waiters();
    }

    /// Keeps a message for a thread without a session; `false` if it is not kept.
    fn keep(&self, thread_id: &str, method: &str, params: &Value) -> bool {
        if !self.adopted {
            return false;
        }
        let mut orphans = self.orphans.lock().expect("orphans lock");
        let kept = orphans.entry(thread_id.to_owned()).or_default();
        if kept.len() < ORPHANS_KEPT {
            kept.push((method.to_owned(), params.clone()));
        }
        true
    }

    /// The thread's session, waiting for it to be taken over in an adopted process.
    async fn claimed(&self, thread_id: &str) -> Option<Arc<ThreadSink>> {
        if !self.adopted {
            return self.get(thread_id);
        }
        let deadline = tokio::time::Instant::now() + CLAIM_WAIT;
        loop {
            let notified = self.claimed.notified();
            if let Some(sink) = self.get(thread_id) {
                return Some(sink);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return None;
            }
        }
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
    /// Messages steered into the turn that the model has not read yet.
    steers: Vec<MessageId>,
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
        turn.steers.clear();
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
        let sink = match wire::thread_of(&params) {
            Some(id) => self.router.claimed(id).await,
            None => None,
        };
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
            self.router.keep(thread_id, method, &params);
            return;
        };
        route(&self.router, &sink, method, &params);
    }
}

/// Hands one notification to the thread's session.
fn route(router: &Router, sink: &ThreadSink, method: &str, params: &Value) {
    {
        let mut turn = sink.turn();
        if method == method::TURN_STARTED
            && let Some(id) = wire::turn_started_id(params)
        {
            turn.active = Some(id.to_owned());
        } else if router.adopted
            && turn.active.is_none()
            && turn.waiter.is_some()
            && method != method::TURN_COMPLETED
            && let Some(id) = params.get("turnId").and_then(Value::as_str)
        {
            // A turn taken over mid-way: its id comes with its next item.
            turn.active = Some(id.to_owned());
        }
    }
    for kind in wire::translate(method, params) {
        sink.emit(kind, Some(params));
    }
    // A steered message is read by the model's next request; any output of the running
    // turn is the first sign of it.
    if matches!(
        method,
        method::ITEM_STARTED
            | method::ITEM_COMPLETED
            | method::AGENT_MESSAGE_DELTA
            | method::REASONING_SUMMARY_DELTA
            | method::REASONING_TEXT_DELTA
            | method::COMMAND_OUTPUT_DELTA
            | method::TURN_COMPLETED
    ) {
        let read = {
            let mut turn = sink.turn();
            if turn.waiter.is_some() && !turn.steers.is_empty() {
                Some(std::mem::take(&mut turn.steers))
            } else {
                None
            }
        };
        for message_id in read.into_iter().flatten() {
            sink.emit(SessionEventKind::SteerRead { message_id }, Some(params));
        }
    }
    if method == method::TURN_COMPLETED
        && let Some(end) = wire::turn_end(params)
    {
        let mut turn = sink.turn();
        turn.active = None;
        turn.steers.clear();
        if let Some(waiter) = turn.waiter.take() {
            let _ = waiter.send(end);
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
    /// The turn a previous hub started, taken over with the process.
    adopted: Mutex<Option<oneshot::Receiver<TurnEnd>>>,
    /// `adopted_turn` is waiting for that turn.
    adopted_waiting: AtomicBool,
}

impl CodexSession {
    fn attach(
        server: Arc<AppServer>,
        thread_id: String,
        spec: &SessionSpec,
        settings: ThreadSettings,
        response: &Value,
    ) -> Self {
        let session = Self::new(server, thread_id, spec, settings, None);
        session.sink.emit(
            SessionEventKind::Started {
                session_id: session.thread_id.clone(),
            },
            response.get("thread"),
        );
        session
    }

    /// The session of a thread in a process taken over from a previous hub.
    fn adopt(
        server: Arc<AppServer>,
        thread_id: String,
        spec: &SessionSpec,
        settings: ThreadSettings,
        turn_running: bool,
    ) -> Self {
        let (tx, rx) = oneshot::channel();
        let waiter = turn_running.then_some(tx);
        let session = Self::new(server, thread_id, spec, settings, waiter);
        if turn_running {
            *session.adopted.lock().expect("adopted lock") = Some(rx);
        }
        session
    }

    fn new(
        server: Arc<AppServer>,
        thread_id: String,
        spec: &SessionSpec,
        settings: ThreadSettings,
        waiter: Option<oneshot::Sender<TurnEnd>>,
    ) -> Self {
        let sink = Arc::new(ThreadSink {
            events: spec.events.clone(),
            permission: spec.permission,
            permissions: spec.permissions.clone(),
            turn: Mutex::new(TurnState {
                waiter,
                ..TurnState::default()
            }),
        });
        // After the waiter is in place: what arrived for the thread meanwhile may end it.
        server.router.insert(&thread_id, sink.clone());
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
            adopted: Mutex::new(None),
            adopted_waiting: AtomicBool::new(false),
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
        self.prompt_with(&Prompt::text(text)).await
    }

    /// Images go as `localImage` input items (docs/tasks/DOGFOOD.md §3).
    async fn prompt_with(&self, prompt: &Prompt) -> Result<TurnOutcome, CoreError> {
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
        let params = wire::turn_start(&self.thread_id, prompt, model.as_deref(), effort.as_deref());
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

    /// `turn/steer` into the running turn. `expectedTurnId` makes Codex refuse a message
    /// meant for a turn that already ended, which the hub then queues.
    async fn steer(&self, message_id: MessageId, prompt: &Prompt) -> Result<(), CoreError> {
        self.check_open()?;
        let turn_id = {
            let mut turn = self.sink.turn();
            // The turn's id comes from `turn/started`; without it Codex cannot be told
            // which turn is meant, so the message waits for the next one.
            let Some(turn_id) = turn.active.clone().filter(|_| turn.waiter.is_some()) else {
                return Err(CoreError::Conflict("no turn is running".into()));
            };
            turn.steers.push(message_id);
            turn_id
        };
        let params = wire::turn_steer(&self.thread_id, prompt, &turn_id);
        let result = self.server.request(method::TURN_STEER, params).await;
        if result.is_err() {
            self.sink.turn().steers.retain(|steer| *steer != message_id);
        }
        result.map(|_| ())
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
            None => {
                // A turn taken over from a previous hub whose id never showed: Codex cannot
                // be asked to stop it, so stop waiting for it.
                if self.adopted_waiting.load(Ordering::SeqCst) {
                    let waiter = self.sink.turn().waiter.take();
                    if let Some(waiter) = waiter {
                        let _ = waiter.send(TurnEnd {
                            turn_id: String::new(),
                            status: "interrupted".into(),
                        });
                    }
                }
                Ok(())
            }
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

    fn outlives_hub(&self) -> bool {
        self.server.process.outlives_hub()
    }

    async fn adopted_turn(&self) -> Option<Result<TurnOutcome, CoreError>> {
        let rx = self.adopted.lock().expect("adopted lock").take()?;
        self.adopted_waiting.store(true, Ordering::SeqCst);
        let result = match rx.await {
            Ok(end) => {
                self.refresh_limits().await;
                Ok(TurnOutcome {
                    stop_reason: wire::stop_reason(&end.status),
                    turn_id: (!end.turn_id.is_empty()).then_some(end.turn_id),
                })
            }
            Err(_) => Err(CoreError::Agent(
                "the turn ended without a result (codex app-server exited)".into(),
            )),
        };
        self.adopted_waiting.store(false, Ordering::SeqCst);
        Some(result)
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
}
