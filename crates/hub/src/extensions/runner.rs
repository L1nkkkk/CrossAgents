//! Runs extension processes (docs/EXTENSIONS.md §3).
//!
//! Tool calls — an agent calling `<extension>__<tool>` or `extension_call`, a person's GUI —
//! all go through [`Runner::start`], which checks in the order the document gives and only
//! then spawns anything:
//!
//! 1. the extension exists, is enabled, and which version this call uses is fixed (the
//!    snapshot is pinned until the call ends, so a version switch under it changes nothing);
//! 2. the caller may call the tool (an agent's groups now; a person needs `human`);
//! 3. the input fits the tool's `input_schema`;
//! 4. the concurrency limits hold (otherwise "busy" at once, never queued).
//!
//! Event hooks and resident services ([`Runner::start_command`]) skip the tool checks but
//! reuse everything else: the pinned snapshot, the process tree, the token, the log and —
//! for hooks — the same concurrency limits. The tool protocol adds only the stdin/stdout
//! JSON and the result bookkeeping on top of this.
//!
//! Every process runs in the snapshot and in a process tree
//! ([`crossagents_core::process::Tree`], a Job Object on Windows), so a timeout, a cancel, a
//! disable or the hub stopping ends the whole tree.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crossagents_api_types::{
    ExtensionAccess, ExtensionCallAccepted, ExtensionCallResponse, ExtensionCallStatus,
    ExtensionToolInfo,
};
use crossagents_core::process::{Tree, spawn_tree_strict};
use crossagents_core::{CoreError, EventBus, EventKind};
use crossagents_orchestrator::host::{ExtensionCaller, ExtensionHost, ExtensionToolSpec};
use futures::FutureExt;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

use super::registry::{EnabledVersion, Registry, SnapshotLease};

/// stdout past this ends the call: a cut-off JSON means nothing.
pub const STDOUT_LIMIT: usize = 1024 * 1024;
/// How much of the end of stderr a result carries; the log has all of it.
pub const STDERR_KEEP: usize = 64 * 1024;
/// One log file rolls over at this size.
pub const LOG_ROLL: u64 = 10 * 1024 * 1024;
/// Prompts bigger than this are cut: they go into every new session.
const PROMPT_LIMIT: u64 = 64 * 1024;

/// The limits of §3; a test can shrink them.
#[derive(Debug, Clone)]
pub struct Limits {
    pub per_extension: usize,
    pub global: usize,
    /// How long a finished call's result stays readable.
    pub retention: Duration,
    /// How long a call may go on after its extension is disabled.
    pub disable_grace: Duration,
    /// A log file rolls over at this size.
    pub log_roll: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            per_extension: 4,
            global: 16,
            retention: Duration::from_secs(10 * 60),
            disable_grace: Duration::from_secs(5),
            log_roll: LOG_ROLL,
        }
    }
}

/// Who a call is for. The hub decides this from the credential; a request body never says.
#[derive(Debug, Clone)]
pub enum Caller {
    Agent(Box<ExtensionCaller>),
    /// A person, through the GUI route. The tool must declare the `human` group.
    Human,
}

impl Caller {
    fn json(&self) -> Value {
        match self {
            Caller::Human => json!({ "kind": "human" }),
            Caller::Agent(c) => json!({
                "kind": "agent",
                "id": c.agent.id.to_string(),
                "name": c.agent.name,
                "groups": c.groups,
                "cwd": c.agent.cwd.display().to_string(),
                "run": c.run.map(|r| r.to_string()),
                "task": c.task,
            }),
        }
    }

    fn label(&self) -> String {
        match self {
            Caller::Human => "human".into(),
            Caller::Agent(c) => format!("agent {}", c.agent.name),
        }
    }
}

/// Why a call did not start. Nothing ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// No such extension or tool.
    Unknown(String),
    /// The extension is there but not enabled (never approved, or disabled).
    NotEnabled(String),
    /// The caller may not call this tool.
    Forbidden(String),
    /// The input does not fit the tool's schema.
    InvalidInput(String),
    /// Over a concurrency limit.
    Busy(String),
    Internal(String),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Self::Unknown(m)
        | Self::NotEnabled(m)
        | Self::Forbidden(m)
        | Self::InvalidInput(m)
        | Self::Busy(m)
        | Self::Internal(m)) = self;
        f.write_str(m)
    }
}

impl std::error::Error for CallError {}

impl From<CallError> for CoreError {
    fn from(e: CallError) -> Self {
        match e {
            CallError::Unknown(m) => CoreError::NotFound {
                kind: "tool",
                id: m,
            },
            CallError::NotEnabled(m) | CallError::Forbidden(m) | CallError::InvalidInput(m) => {
                CoreError::Conflict(m)
            }
            CallError::Busy(m) => CoreError::Busy(m),
            CallError::Internal(m) => CoreError::Storage(m),
        }
    }
}

/// One call, running or finished.
struct Call {
    id: String,
    extension: String,
    tool: String,
    fingerprint: String,
    started_at: DateTime<Utc>,
    timeout_secs: u32,
    /// `DELETE`, or the hub stopping: end it now.
    cancel: CancellationToken,
    reason: Arc<Mutex<Option<String>>>,
    /// The extension was disabled: end it after the grace period.
    disabled: CancellationToken,
    done: watch::Sender<Option<Arc<ExtensionCallResponse>>>,
    finished: Mutex<Option<Instant>>,
}

impl Call {
    fn response(&self) -> ExtensionCallResponse {
        match self.done.borrow().as_ref() {
            Some(done) => (**done).clone(),
            None => ExtensionCallResponse {
                call_id: self.id.clone(),
                extension: self.extension.clone(),
                tool: self.tool.clone(),
                fingerprint: self.fingerprint.clone(),
                status: ExtensionCallStatus::Running,
                started_at: self.started_at,
                finished_at: None,
                duration_ms: None,
                output: None,
                error: None,
                stderr: None,
            },
        }
    }

    fn is_running(&self) -> bool {
        self.done.borrow().is_none()
    }
}

#[derive(Default)]
struct Counts {
    global: usize,
    per: HashMap<String, usize>,
}

/// A place in the concurrency limits, returned on drop.
struct Permit {
    runner: Arc<Runner>,
    extension: String,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut counts = self.runner.counts.lock().unwrap();
        counts.global = counts.global.saturating_sub(1);
        if let Some(n) = counts.per.get_mut(&self.extension) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                counts.per.remove(&self.extension);
            }
        }
    }
}

/// The token issued for one process, valid until the process tree is gone.
pub struct TokenGuard {
    runner: Weak<Runner>,
    token: String,
}

impl TokenGuard {
    /// The token to hand the process as `CROSSAGENTS_TOKEN`.
    pub fn token(&self) -> &str {
        &self.token
    }
}

impl Drop for TokenGuard {
    fn drop(&mut self) {
        if let Some(runner) = self.runner.upgrade() {
            runner.tokens.write().unwrap().remove(&self.token);
        }
    }
}

/// Where a command's stdout goes. A tool's output is its result; a hook's or a service's is
/// just text for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputCapture {
    /// Collected for the caller, up to [`STDOUT_LIMIT`]; past that the process is killed.
    Collect,
    /// Written to the extension log as it arrives; nothing is kept in memory.
    LogOnly,
}

/// One command outside the tool checks: an event hook or a resident service
/// (docs/EXTENSIONS.md §3). Everything a tool gets — snapshot cwd, hub env, token, process
/// tree, log — is the same; only the group and schema checks and the result protocol are
/// left out.
pub struct CommandSpec {
    /// Whose limits, log and token this command runs under.
    pub extension: String,
    /// Pins the snapshot until the process tree is gone.
    pub lease: SnapshotLease,
    pub command: Vec<String>,
    /// Written to stdin and closed; `None` closes stdin at once.
    pub stdin: Option<Vec<u8>>,
    /// `Some` kills the tree at the deadline; `None` runs until it ends (a service).
    pub timeout: Option<Duration>,
    pub stdout: OutputCapture,
    /// Take a place in the same limits as tool calls. Hooks do, so a burst of events cannot
    /// start unbounded processes; a service is one per extension already.
    pub limits: bool,
    /// How the log names it (`hook run.landed`, `service`).
    pub label: String,
    /// Cancelling this ends the whole tree.
    pub cancel: CancellationToken,
}

/// How a command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandStatus {
    /// It ended by itself; the code is `None` when a signal ended it.
    Exited(Option<i32>),
    /// The deadline passed and the tree was killed.
    TimedOut,
    /// Cancelled: a version switch, a disable or the hub stopping.
    Cancelled,
    /// It wrote more than [`STDOUT_LIMIT`] to stdout, which cut-off JSON would make worthless.
    TooLarge,
    /// It never started.
    NotStarted,
}

/// What [`CommandRun::wait`] answers. Hooks log it; the service supervisor reads it to decide
/// whether the exit was a crash.
#[derive(Debug, Clone)]
pub struct CommandResult {
    pub status: CommandStatus,
    pub duration: Duration,
    /// Collected only when the spec asked for it.
    pub stdout: Vec<u8>,
    pub stderr: String,
    /// Why it was cancelled, or why it never started.
    pub error: Option<String>,
}

impl CommandResult {
    /// Whether it exited 0. A hook's and a service's exit code decides nothing by itself,
    /// but it is what the log and `ExtensionInfo.service.last_exit` say.
    pub fn ok(&self) -> bool {
        self.status == CommandStatus::Exited(Some(0))
    }

    /// One line for the log and for `last_exit`.
    pub fn describe(&self) -> String {
        let head = match self.status {
            CommandStatus::Exited(Some(code)) => format!("exited {code}"),
            CommandStatus::Exited(None) => "was ended by a signal".into(),
            CommandStatus::TimedOut => "timed out".into(),
            CommandStatus::Cancelled => "was cancelled".into(),
            CommandStatus::TooLarge => "wrote too much to stdout".into(),
            CommandStatus::NotStarted => "did not start".into(),
        };
        match &self.error {
            // "cancelled" is the plain reason; a more specific one (a disable) says more.
            Some(error) if !error.is_empty() && error != "cancelled" => {
                format!("{head}: {error}")
            }
            _ => head,
        }
    }
}

/// A command that has been started. It ends by itself or when its cancel token is cancelled;
/// dropping the handle leaves the process running (its supervisor owns it), so keep it.
#[derive(Debug)]
pub struct CommandRun {
    id: String,
    done: watch::Receiver<Option<Arc<CommandResult>>>,
}

impl CommandRun {
    /// The id the log uses.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Waits for the process tree to be gone. Waiting twice is fine.
    pub async fn wait(&self) -> CommandResult {
        let mut done = self.done.clone();
        match done.wait_for(|value| value.is_some()).await {
            Ok(value) => value
                .as_ref()
                .map(|result| (**result).clone())
                .unwrap_or_else(CommandResult::lost),
            Err(_) => CommandResult::lost(),
        }
    }
}

impl CommandResult {
    /// The hub went away before the command recorded an outcome.
    fn lost() -> Self {
        Self {
            status: CommandStatus::Cancelled,
            duration: Duration::ZERO,
            stdout: Vec::new(),
            stderr: String::new(),
            error: Some("the runner stopped before this command ended".into()),
        }
    }
}

/// Runs extension tools and keeps their results.
pub struct Runner {
    me: Weak<Runner>,
    registry: Arc<Registry>,
    bus: Option<Arc<EventBus>>,
    home: PathBuf,
    url: OnceLock<String>,
    limits: Limits,
    /// Proxy variables of the hub, handed to every tool (`claude.com` and the like are not
    /// reachable without them).
    proxy: Vec<(String, String)>,
    counts: Mutex<Counts>,
    calls: Mutex<HashMap<String, Arc<Call>>>,
    /// Token -> (extension, level). Memory only: a restart voids all of them.
    tokens: RwLock<HashMap<String, (String, ExtensionAccess)>>,
    log_lock: Mutex<()>,
    stopping: AtomicBool,
}

/// The hub's own proxy variables, whichever case they are set in.
pub fn proxy_env(vars: impl IntoIterator<Item = (String, String)>) -> Vec<(String, String)> {
    vars.into_iter()
        .filter(|(k, v)| {
            !v.is_empty()
                && matches!(
                    k.to_ascii_uppercase().as_str(),
                    "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY" | "NO_PROXY"
                )
        })
        .collect()
}

impl Runner {
    pub fn new(registry: Arc<Registry>, bus: Option<Arc<EventBus>>, home: PathBuf) -> Arc<Self> {
        Self::with_limits(
            registry,
            bus,
            home,
            Limits::default(),
            proxy_env(std::env::vars()),
        )
    }

    pub fn with_limits(
        registry: Arc<Registry>,
        bus: Option<Arc<EventBus>>,
        home: PathBuf,
        limits: Limits,
        proxy: Vec<(String, String)>,
    ) -> Arc<Self> {
        let runner = Arc::new_cyclic(|me| Self {
            me: me.clone(),
            registry: registry.clone(),
            bus,
            home,
            url: OnceLock::new(),
            limits,
            proxy,
            counts: Mutex::default(),
            calls: Mutex::default(),
            tokens: RwLock::default(),
            log_lock: Mutex::default(),
            stopping: AtomicBool::new(false),
        });
        let weak = Arc::downgrade(&runner);
        registry.on_disabled(move |name| {
            if let Some(runner) = weak.upgrade() {
                runner.extension_disabled(name);
            }
        });
        runner
    }

    /// Where tools reach the hub (`CROSSAGENTS_URL`); known once the listener is bound.
    pub fn set_url(&self, url: String) {
        let _ = self.url.set(url);
    }

    /// The extension and level a token was issued for, if its call is still running.
    pub fn token(&self, token: &str) -> Option<(String, ExtensionAccess)> {
        self.tokens.read().unwrap().get(token).cloned()
    }

    /// How many tokens are valid right now.
    pub fn live_tokens(&self) -> usize {
        self.tokens.read().unwrap().len()
    }

    /// Issues a token for `extension` at `level`. It stops working the moment the guard is
    /// dropped, so one guard lives exactly as long as its process tree
    /// (docs/EXTENSIONS.md §3). Never outlives the hub: the table is memory only.
    pub fn issue_token(&self, extension: &str, level: ExtensionAccess) -> TokenGuard {
        let token = format!(
            "ca-ext-{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        self.tokens
            .write()
            .unwrap()
            .insert(token.clone(), (extension.to_owned(), level));
        TokenGuard {
            runner: self.me.clone(),
            token,
        }
    }

    /// Appends one line to the extension's log; hooks and services report their lifecycle
    /// there (docs/EXTENSIONS.md §3).
    pub fn note(&self, extension: &str, line: &str) {
        self.write_log(
            extension,
            format!("--- {} {line}\n", Utc::now().to_rfc3339()).as_bytes(),
        );
    }

    fn extension_disabled(&self, name: &str) {
        for call in self.calls.lock().unwrap().values() {
            if call.extension == name && call.is_running() {
                call.disabled.cancel();
            }
        }
    }

    fn prune(&self, calls: &mut HashMap<String, Arc<Call>>) {
        let keep = self.limits.retention;
        calls.retain(|_, c| match *c.finished.lock().unwrap() {
            Some(at) => at.elapsed() < keep,
            None => true,
        });
    }

    fn reserve(self: &Arc<Self>, extension: &str) -> Result<Permit, CallError> {
        let mut counts = self.counts.lock().unwrap();
        if counts.global >= self.limits.global {
            return Err(CallError::Busy(format!(
                "busy: {} extension tool calls are running (the most there can be); try again shortly",
                self.limits.global
            )));
        }
        let have = counts.per.get(extension).copied().unwrap_or(0);
        if have >= self.limits.per_extension {
            return Err(CallError::Busy(format!(
                "busy: extension {extension} already has {have} calls running (the most there can be); try again shortly"
            )));
        }
        counts.global += 1;
        *counts.per.entry(extension.to_owned()).or_default() += 1;
        Ok(Permit {
            runner: self.clone(),
            extension: extension.to_owned(),
        })
    }

    /// Checks the call in the order of §3 and starts it. Returns at once; the result is
    /// awaited with [`Runner::wait`] or read with [`Runner::get`]. Must run inside a tokio
    /// runtime.
    pub fn start(
        &self,
        caller: Caller,
        extension: &str,
        tool: &str,
        input: Value,
    ) -> Result<ExtensionCallAccepted, CallError> {
        let this = self
            .me
            .upgrade()
            .ok_or_else(|| CallError::Internal("the runner is gone".into()))?;
        if self.stopping.load(Ordering::SeqCst) {
            return Err(CallError::NotEnabled("the hub is stopping".into()));
        }
        // 1. Enabled, and this call's version fixed: the lease pins the snapshot until the
        // call ends.
        let lease = this.registry.acquire(extension).map_err(|e| match e {
            CoreError::NotFound { .. } => {
                CallError::Unknown(format!("no extension named {extension}"))
            }
            CoreError::Conflict(why) => {
                CallError::NotEnabled(format!("extension {extension} cannot be called: {why}"))
            }
            other => CallError::Internal(other.to_string()),
        })?;
        let qualified = format!("{extension}__{tool}");
        let info: ExtensionToolInfo = lease
            .enabled
            .version
            .tools
            .iter()
            .find(|t| t.name == tool)
            .cloned()
            .ok_or_else(|| {
                CallError::Unknown(format!("extension {extension} has no tool named {tool}"))
            })?;
        let command = lease
            .enabled
            .manifest
            .tools
            .iter()
            .find(|t| t.name == tool)
            .map(|t| t.command.clone())
            .ok_or_else(|| CallError::Internal(format!("{qualified} has no command")))?;
        // 2. The caller may call it.
        match &caller {
            Caller::Agent(c) => {
                if !info.groups.iter().any(|g| c.groups.contains(g)) {
                    return Err(CallError::Forbidden(format!(
                        "{qualified} is not available to you (your groups: {})",
                        c.groups.join(", ")
                    )));
                }
            }
            Caller::Human => {
                if !info.groups.iter().any(|g| g == "human") {
                    return Err(CallError::Forbidden(format!(
                        "{qualified} is not open to people: its groups ({}) do not include human",
                        info.groups.join(", ")
                    )));
                }
            }
        }
        // 3. The input fits the schema. A missing input is `{}`.
        let input = if input.is_null() { json!({}) } else { input };
        let validator = jsonschema::validator_for(&info.input_schema)
            .map_err(|e| CallError::Internal(format!("{qualified}: invalid input_schema: {e}")))?;
        let problems: Vec<String> = validator
            .iter_errors(&input)
            .take(5)
            .map(|e| {
                let at = e.instance_path.to_string();
                if at.is_empty() {
                    e.to_string()
                } else {
                    format!("{at}: {e}")
                }
            })
            .collect();
        if !problems.is_empty() {
            return Err(CallError::InvalidInput(format!(
                "the input does not fit the schema of {qualified}: {}",
                problems.join("; ")
            )));
        }
        // 4. Room for another process.
        let permit = this.reserve(extension)?;

        let id = format!("call-{}", uuid::Uuid::new_v4().simple());
        let started_at = Utc::now();
        let timeout_secs = info.timeout_secs.clamp(1, 600);
        let (done, _) = watch::channel(None);
        let call = Arc::new(Call {
            id: id.clone(),
            extension: extension.to_owned(),
            tool: tool.to_owned(),
            fingerprint: lease.enabled.version.fingerprint.clone(),
            started_at,
            timeout_secs,
            cancel: CancellationToken::new(),
            reason: Arc::new(Mutex::new(None)),
            disabled: CancellationToken::new(),
            done,
            finished: Mutex::new(None),
        });
        {
            let mut calls = self.calls.lock().unwrap();
            self.prune(&mut calls);
            calls.insert(id.clone(), call.clone());
        }
        let level = lease.enabled.version.access;
        let token_guard = this.issue_token(extension, level);
        let token = token_guard.token().to_owned();
        let accepted = ExtensionCallAccepted {
            call_id: id,
            extension: extension.to_owned(),
            tool: tool.to_owned(),
            fingerprint: call.fingerprint.clone(),
            started_at,
            timeout_secs,
        };
        let job = Job {
            call,
            lease,
            command,
            stdin: serde_json::to_vec(&json!({
                "tool": tool,
                "input": input,
                "caller": caller.json(),
            }))
            .unwrap_or_default(),
            caller: caller.label(),
            token,
            token_guard,
            permit,
        };
        tokio::spawn(this.supervise(job));
        Ok(accepted)
    }

    /// Starts one command outside the tool checks: a hook or a service
    /// (docs/EXTENSIONS.md §3). It returns at once; await [`CommandRun::wait`]. Must run
    /// inside a tokio runtime. A hook takes a tool-call slot, so an over-limit event is
    /// dropped here with [`CallError::Busy`] and nothing runs; a service never waits for one.
    pub fn start_command(&self, spec: CommandSpec) -> Result<CommandRun, CallError> {
        let this = self
            .me
            .upgrade()
            .ok_or_else(|| CallError::Internal("the runner is gone".into()))?;
        if self.stopping.load(Ordering::SeqCst) {
            return Err(CallError::NotEnabled("the hub is stopping".into()));
        }
        let CommandSpec {
            extension,
            lease,
            command,
            stdin,
            timeout,
            stdout,
            limits,
            label,
            cancel,
        } = spec;
        if command.first().is_none_or(|program| program.is_empty()) {
            return Err(CallError::Internal(format!(
                "{label} of {extension} has no command"
            )));
        }
        if cancel.is_cancelled() {
            return Err(CallError::NotEnabled(format!(
                "{label} of {extension} was cancelled before it started"
            )));
        }
        let permit = if limits {
            Some(this.reserve(&extension)?)
        } else {
            None
        };
        let token_guard = this.issue_token(&extension, lease.enabled.version.access);
        let token = token_guard.token().to_owned();
        let id = format!("cmd-{}", uuid::Uuid::new_v4().simple());
        let (done, rx) = watch::channel(None);
        let job = CommandJob {
            id: id.clone(),
            extension,
            label,
            lease,
            command,
            stdin,
            timeout,
            stdout,
            cancel,
            token,
            token_guard,
            permit,
            done: done.clone(),
        };
        tokio::spawn(this.supervise_command(job));
        Ok(CommandRun { id, done: rx })
    }

    /// The call's current state, if the hub still has it.
    pub fn get(&self, id: &str) -> Option<ExtensionCallResponse> {
        let mut calls = self.calls.lock().unwrap();
        self.prune(&mut calls);
        calls.get(id).map(|c| c.response())
    }

    /// Waits until the call ends. `None` when the hub does not know the call.
    pub async fn wait(&self, id: &str) -> Option<ExtensionCallResponse> {
        let call = self.calls.lock().unwrap().get(id).cloned()?;
        let mut rx = call.done.subscribe();
        let done = rx.wait_for(|v| v.is_some()).await.ok()?;
        done.as_ref().map(|r| (**r).clone())
    }

    /// Ends the call (`DELETE`) and answers with how it ended; a call that already ended is
    /// answered as it is. `None` when the hub does not know the call.
    pub async fn cancel(&self, id: &str) -> Option<ExtensionCallResponse> {
        let call = self.calls.lock().unwrap().get(id).cloned()?;
        if call.is_running() {
            call.reason
                .lock()
                .unwrap()
                .get_or_insert_with(|| "cancelled".into());
            call.cancel.cancel();
            let mut rx = call.done.subscribe();
            let _ =
                tokio::time::timeout(Duration::from_secs(10), rx.wait_for(|v| v.is_some())).await;
        }
        Some(call.response())
    }

    /// Ends every running call and refuses new ones: the hub is stopping. Results are lost
    /// and nothing is retried (§3).
    pub async fn shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        let running: Vec<Arc<Call>> = self
            .calls
            .lock()
            .unwrap()
            .values()
            .filter(|c| c.is_running())
            .cloned()
            .collect();
        for call in &running {
            call.reason
                .lock()
                .unwrap()
                .get_or_insert_with(|| "the hub is stopping".into());
            call.cancel.cancel();
        }
        for call in running {
            let mut rx = call.done.subscribe();
            let _ =
                tokio::time::timeout(Duration::from_secs(5), rx.wait_for(|v| v.is_some())).await;
        }
    }

    /// Shared log view for the agent tool and the GUI read route.
    pub fn logs(
        &self,
        name: &str,
    ) -> Result<crossagents_api_types::ExtensionLogsResponse, CoreError> {
        use std::io::{Read, Seek, SeekFrom};
        crossagents_api_types::check_extension_name(name).map_err(CoreError::InvalidName)?;
        self.registry.get(name)?;
        let _guard = self.log_lock.lock().unwrap();
        let mut file = match std::fs::File::open(self.log_path(name)) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(crossagents_api_types::ExtensionLogsResponse {
                    name: name.into(),
                    content: String::new(),
                    truncated: false,
                });
            }
            Err(e) => return Err(CoreError::Storage(e.to_string())),
        };
        let result = (|| -> std::io::Result<_> {
            let length = file.metadata()?.len();
            let start = length.saturating_sub(64 * 1024);
            file.seek(SeekFrom::Start(start))?;
            let mut bytes = Vec::new();
            file.take(64 * 1024).read_to_end(&mut bytes)?;
            Ok(crossagents_api_types::ExtensionLogsResponse {
                name: name.into(),
                content: String::from_utf8_lossy(&bytes).into_owned(),
                truncated: start > 0,
            })
        })();
        result.map_err(|e| CoreError::Storage(e.to_string()))
    }

    fn log_path(&self, extension: &str) -> PathBuf {
        self.home
            .join("extension-logs")
            .join(format!("{extension}.log"))
    }

    /// Appends to the extension's log, rolling it over at [`LOG_ROLL`]: the old file becomes
    /// `<name>.log.1`, replacing the one before.
    fn write_log(&self, extension: &str, bytes: &[u8]) {
        use std::io::Write;
        let _guard = self.log_lock.lock().unwrap();
        let path = self.log_path(extension);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(meta) = std::fs::metadata(&path)
            && meta.len() + bytes.len() as u64 > self.limits.log_roll
        {
            let old = path.with_extension("log.1");
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(&path, &old);
        }
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(mut file) => {
                let _ = file.write_all(bytes);
            }
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "cannot write the extension log")
            }
        }
    }

    async fn supervise(self: Arc<Self>, job: Job) {
        let Job {
            call,
            lease,
            command,
            stdin,
            caller,
            token,
            token_guard,
            permit,
        } = job;
        let _finisher = Finisher(call.clone());
        let started = Instant::now();
        let short = &call.fingerprint[..call.fingerprint.len().min(8)];
        self.write_log(
            &call.extension,
            format!(
                "\n--- {} call {} {}__{} @{short} by {caller}\n",
                Utc::now().to_rfc3339(),
                call.id,
                call.extension,
                call.tool
            )
            .as_bytes(),
        );
        // A panic while running must still end in a result.
        let control = control_of(&call, self.limits.disable_grace);
        let outcome = match std::panic::AssertUnwindSafe(self.execute(
            &control,
            &lease.enabled,
            &command,
            Some(stdin),
            &token,
            true,
        ))
        .catch_unwind()
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => Outcome::NotStarted {
                why: "internal error while running the tool".into(),
            },
        };
        let duration = started.elapsed();
        // The tree is gone by now: the token must stop working with it, and only then does
        // the result become visible.
        drop(token_guard);
        let (status, output, error, stderr) = match outcome {
            Outcome::Exited {
                code: Some(0),
                stdout,
                stderr,
            } => match serde_json::from_slice::<Value>(&stdout) {
                Ok(value) => (ExtensionCallStatus::Ok, Some(value), None, stderr),
                Err(e) => {
                    let head: String = String::from_utf8_lossy(&stdout).chars().take(200).collect();
                    (
                        ExtensionCallStatus::Failed,
                        None,
                        Some(format!(
                            "the tool exited 0 but its output is not JSON ({e}): {head}"
                        )),
                        stderr,
                    )
                }
            },
            Outcome::Exited { code, stderr, .. } => {
                let text = stderr.trim();
                let error = if text.is_empty() {
                    match code {
                        Some(code) => format!("the tool exited with code {code}"),
                        None => "the tool was ended by a signal".into(),
                    }
                } else {
                    text.to_owned()
                };
                (ExtensionCallStatus::Failed, None, Some(error), stderr)
            }
            Outcome::TimedOut { stderr } => (
                ExtensionCallStatus::TimedOut,
                None,
                Some(format!(
                    "timed out after {}s and was killed",
                    call.timeout_secs
                )),
                stderr,
            ),
            Outcome::TooLarge { stderr } => (
                ExtensionCallStatus::Failed,
                None,
                Some(format!(
                    "the tool wrote more than {STDOUT_LIMIT} bytes to stdout and was killed; output is never cut off"
                )),
                stderr,
            ),
            Outcome::Cancelled { why, stderr } => {
                (ExtensionCallStatus::Cancelled, None, Some(why), stderr)
            }
            Outcome::NotStarted { why } => {
                (ExtensionCallStatus::Failed, None, Some(why), String::new())
            }
        };
        let finished_at = Utc::now();
        let response = ExtensionCallResponse {
            call_id: call.id.clone(),
            extension: call.extension.clone(),
            tool: call.tool.clone(),
            fingerprint: call.fingerprint.clone(),
            status,
            started_at: call.started_at,
            finished_at: Some(finished_at),
            duration_ms: Some(duration.as_millis() as u64),
            output,
            error: error.clone(),
            stderr: (!stderr.is_empty()).then_some(stderr),
        };
        let word = serde_json::to_value(status)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        self.write_log(
            &call.extension,
            format!(
                "--- call {} {word} in {} ms{}\n",
                call.id,
                duration.as_millis(),
                error
                    .as_deref()
                    .map(|e| format!(": {e}"))
                    .unwrap_or_default()
            )
            .as_bytes(),
        );
        tracing::info!(
            extension = %call.extension, tool = %call.tool, call = %call.id,
            status = %word, ms = duration.as_millis() as u64, "extension call finished"
        );
        *call.finished.lock().unwrap() = Some(Instant::now());
        call.done.send_replace(Some(Arc::new(response)));
        // Unpin the snapshot and give the slot back before anyone is told it ended.
        drop(lease);
        drop(permit);
        if let Some(bus) = &self.bus
            && let Err(error) = bus
                .publish(EventKind::ExtensionCallFinished {
                    call_id: call.id.clone(),
                    extension: call.extension.clone(),
                    tool: call.tool.clone(),
                    fingerprint: call.fingerprint.clone(),
                    status: word,
                    duration_ms: duration.as_millis() as u64,
                    error,
                })
                .await
        {
            tracing::warn!(%error, "cannot publish extension.call.finished");
        }
    }

    /// Runs one command in a pinned snapshot. What ends it and where its output goes is in
    /// `control`; a tool wraps this with the JSON protocol, a hook or a service uses it as it
    /// is.
    async fn execute(
        &self,
        control: &Control,
        enabled: &EnabledVersion,
        command: &[String],
        stdin: Option<Vec<u8>>,
        token: &str,
        capture_stdout: bool,
    ) -> Outcome {
        let Some((program, args)) = command.split_first() else {
            return Outcome::NotStarted {
                why: format!("{} has no command", control.label),
            };
        };
        // No shell: the program and its arguments go to the OS as they are.
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .current_dir(&enabled.path)
            .stdin(if stdin.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            // An agent's own token is not for extensions.
            .env_remove(crate::collab::TOKEN_ENV)
            .env(
                "CROSSAGENTS_URL",
                self.url.get().map(String::as_str).unwrap_or(""),
            )
            .env("CROSSAGENTS_TOKEN", token)
            .env("CROSSAGENTS_EXTENSION_DATA", &enabled.data);
        for (k, v) in &self.proxy {
            cmd.env(k, v);
        }
        let (mut child, mut tree) = match spawn_tree_strict(&mut cmd) {
            Ok(started) => started,
            Err(e) => {
                return Outcome::NotStarted {
                    why: format!("cannot start {program}: {e}"),
                };
            }
        };
        let (Some(stdout_pipe), Some(stderr_pipe)) = (child.stdout.take(), child.stderr.take())
        else {
            tree.kill();
            return Outcome::NotStarted {
                why: "cannot open the process's pipes".into(),
            };
        };
        // The child may never read its input, so writing must not block the supervisor.
        if let (Some(mut stdin_pipe), Some(input)) = (child.stdin.take(), stdin) {
            tokio::spawn(async move {
                let _ = stdin_pipe.write_all(&input).await;
                let _ = stdin_pipe.shutdown().await;
            });
        }
        let too_large = Arc::new(Notify::new());
        let overflow = Arc::new(AtomicBool::new(false));
        // The readers fill these as they go, so what was read survives a reader that is
        // stopped because a child still holds its pipe open.
        let stdout_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let stderr_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let stdout_task = tokio::spawn({
            let (too_large, overflow, buf) =
                (too_large.clone(), overflow.clone(), stdout_buf.clone());
            let me = self.me.clone();
            let extension = control.extension.clone();
            async move {
                let mut out = stdout_pipe;
                let mut chunk = vec![0u8; 16 * 1024];
                loop {
                    match out.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if capture_stdout {
                                let mut data = buf.lock().unwrap();
                                data.extend_from_slice(&chunk[..n]);
                                if data.len() > STDOUT_LIMIT {
                                    overflow.store(true, Ordering::SeqCst);
                                    too_large.notify_one();
                                    break;
                                }
                            } else if let Some(me) = me.upgrade() {
                                // A service's stdout is a log, not a result: no cap, no copy.
                                me.write_log(&extension, &chunk[..n]);
                            }
                        }
                    }
                }
            }
        });
        let stderr_task = tokio::spawn({
            let me = self.me.clone();
            let name = control.extension.clone();
            let buf = stderr_buf.clone();
            async move {
                let mut err = stderr_pipe;
                let mut chunk = vec![0u8; 8 * 1024];
                let mut last = b'\n';
                loop {
                    match err.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if let Some(me) = me.upgrade() {
                                me.write_log(&name, &chunk[..n]);
                            }
                            last = chunk[n - 1];
                            let mut tail = buf.lock().unwrap();
                            tail.extend_from_slice(&chunk[..n]);
                            if tail.len() > STDERR_KEEP * 2 {
                                let cut = tail.len() - STDERR_KEEP;
                                tail.drain(..cut);
                            }
                        }
                    }
                }
                if last != b'\n'
                    && let Some(me) = me.upgrade()
                {
                    me.write_log(&name, b"\n");
                }
            }
        });

        enum End {
            Exited(Option<i32>),
            Timeout,
            TooLarge,
            Cancelled(String),
        }
        let disabled = match &control.disabled {
            Some(token) => {
                let (token, grace) = (token.clone(), control.grace);
                futures::future::Either::Left(async move {
                    token.cancelled().await;
                    tokio::time::sleep(grace).await;
                })
            }
            None => futures::future::Either::Right(futures::future::pending::<()>()),
        };
        let deadline = match control.timeout {
            Some(timeout) => futures::future::Either::Left(tokio::time::sleep(timeout)),
            None => futures::future::Either::Right(futures::future::pending::<()>()),
        };
        let end = tokio::select! {
            biased;
            _ = control.cancel.cancelled() => End::Cancelled(
                control.reason.lock().unwrap().clone().unwrap_or_else(|| "cancelled".into()),
            ),
            _ = disabled => End::Cancelled(format!(
                "extension {} was disabled; the process was ended after {}s",
                control.extension,
                control.grace.as_secs()
            )),
            _ = too_large.notified() => End::TooLarge,
            _ = deadline => End::Timeout,
            exited = child.wait() => End::Exited(exited.ok().and_then(|s| s.code())),
        };
        match end {
            End::Exited(code) => {
                // The process ended on its own; take what it wrote. A leftover child that
                // keeps the pipes open is ended with the tree after a short wait.
                // Each reader is awaited once, against one shared deadline: what one of
                // them already read is kept when the other is still held open.
                let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
                join_reader(stdout_task, deadline).await;
                join_reader(stderr_task, deadline).await;
                kill_tree(&mut tree, &mut child).await;
                let stdout = std::mem::take(&mut *stdout_buf.lock().unwrap());
                let stderr = tail_text(&stderr_buf);
                if overflow.load(Ordering::SeqCst) {
                    return Outcome::TooLarge { stderr };
                }
                Outcome::Exited {
                    code,
                    stdout,
                    stderr,
                }
            }
            other => {
                kill_tree(&mut tree, &mut child).await;
                let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
                join_reader(stdout_task, deadline).await;
                join_reader(stderr_task, deadline).await;
                let stderr = tail_text(&stderr_buf);
                match other {
                    End::Timeout => Outcome::TimedOut { stderr },
                    End::TooLarge => Outcome::TooLarge { stderr },
                    End::Cancelled(why) => Outcome::Cancelled { why, stderr },
                    End::Exited(_) => unreachable!(),
                }
            }
        }
    }
    /// Supervises one hook or service command: runs it, logs how it ended, and gives the
    /// token and the snapshot back. The result (also on a panic) always reaches waiters.
    async fn supervise_command(self: Arc<Self>, job: CommandJob) {
        let CommandJob {
            id,
            extension,
            label,
            lease,
            command,
            stdin,
            timeout,
            stdout,
            cancel,
            token,
            token_guard,
            permit,
            done,
        } = job;
        let _finisher = CommandFinisher(done.clone(), label.clone());
        let started = Instant::now();
        let fingerprint = &lease.enabled.version.fingerprint;
        let short = &fingerprint[..fingerprint.len().min(8)];
        self.note(&extension, &format!("{label} {id} @{short} started"));
        let control = Control {
            extension: extension.clone(),
            label: label.clone(),
            timeout,
            cancel,
            reason: Arc::new(Mutex::new(None)),
            disabled: None,
            grace: Duration::ZERO,
        };
        let outcome = match std::panic::AssertUnwindSafe(self.execute(
            &control,
            &lease.enabled,
            &command,
            stdin,
            &token,
            stdout == OutputCapture::Collect,
        ))
        .catch_unwind()
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => Outcome::NotStarted {
                why: "internal error while running the command".into(),
            },
        };
        let duration = started.elapsed();
        // The tree is gone by now: the token must stop working with it.
        drop(token_guard);
        let result = CommandResult::from_outcome(outcome, duration, timeout);
        let how = result.describe();
        self.note(
            &extension,
            &format!("{label} {id} {how} in {} ms", duration.as_millis()),
        );
        tracing::info!(
            extension = %extension, command = %label, status = %how,
            ms = duration.as_millis() as u64, "extension command finished"
        );
        drop(lease);
        drop(permit);
        done.send_replace(Some(Arc::new(result)));
    }
}

/// A reader's result, or nothing (the reader is stopped) when it is not done by `deadline`:
/// a child that kept the pipe open must not hold the call up.
async fn join_reader(mut task: tokio::task::JoinHandle<()>, deadline: tokio::time::Instant) {
    if tokio::time::timeout_at(deadline, &mut task).await.is_err() {
        task.abort();
    }
}

/// The last [`STDERR_KEEP`] bytes of what a reader collected, as text.
fn tail_text(buf: &Mutex<Vec<u8>>) -> String {
    let tail = buf.lock().unwrap();
    let from = tail.len().saturating_sub(STDERR_KEEP);
    String::from_utf8_lossy(&tail[from..]).into_owned()
}

/// Makes sure a call always ends: if the supervising task is dropped before it recorded an
/// outcome (a panic, the runtime going away), the call becomes `failed` and waiters wake.
struct Finisher(Arc<Call>);

impl Drop for Finisher {
    fn drop(&mut self) {
        let call = &self.0;
        if call.is_running() {
            *call.finished.lock().unwrap() = Some(Instant::now());
            call.done.send_replace(Some(Arc::new(ExtensionCallResponse {
                call_id: call.id.clone(),
                extension: call.extension.clone(),
                tool: call.tool.clone(),
                fingerprint: call.fingerprint.clone(),
                status: ExtensionCallStatus::Failed,
                started_at: call.started_at,
                finished_at: Some(Utc::now()),
                duration_ms: None,
                output: None,
                error: Some("internal error: the call's supervisor ended unexpectedly".into()),
                stderr: None,
            })));
        }
    }
}

/// What ends a process tree besides it exiting, and where its log lines come from.
struct Control {
    /// The extension whose log, limits and token this is.
    extension: String,
    /// How the log names it.
    label: String,
    /// `None` runs until it ends (a service).
    timeout: Option<Duration>,
    cancel: CancellationToken,
    /// Why it was cancelled; read when the cancel branch wins.
    reason: Arc<Mutex<Option<String>>>,
    /// Tools only: the extension was disabled, so end after the grace period.
    disabled: Option<CancellationToken>,
    grace: Duration,
}

fn control_of(call: &Call, grace: Duration) -> Control {
    Control {
        extension: call.extension.clone(),
        label: format!("call {} {}__{}", call.id, call.extension, call.tool),
        timeout: Some(Duration::from_secs(call.timeout_secs.into())),
        cancel: call.cancel.clone(),
        reason: call.reason.clone(),
        disabled: Some(call.disabled.clone()),
        grace,
    }
}

/// Everything a started hook or service carries into its supervising task.
struct CommandJob {
    id: String,
    extension: String,
    label: String,
    lease: SnapshotLease,
    command: Vec<String>,
    stdin: Option<Vec<u8>>,
    timeout: Option<Duration>,
    stdout: OutputCapture,
    cancel: CancellationToken,
    token: String,
    token_guard: TokenGuard,
    permit: Option<Permit>,
    done: watch::Sender<Option<Arc<CommandResult>>>,
}

/// Makes sure a command always ends: if the supervising task is dropped before it recorded
/// an outcome (the runtime going away), waiters still get an answer.
struct CommandFinisher(watch::Sender<Option<Arc<CommandResult>>>, String);

impl Drop for CommandFinisher {
    fn drop(&mut self) {
        if self.0.borrow().is_none() {
            self.0.send_replace(Some(Arc::new(CommandResult {
                status: CommandStatus::Cancelled,
                duration: Duration::ZERO,
                stdout: Vec::new(),
                stderr: String::new(),
                error: Some(format!("{}: the supervisor ended unexpectedly", self.1)),
            })));
        }
    }
}

impl CommandResult {
    fn from_outcome(outcome: Outcome, duration: Duration, timeout: Option<Duration>) -> Self {
        let (status, stdout, stderr, error) = match outcome {
            Outcome::Exited {
                code,
                stdout,
                stderr,
            } => (CommandStatus::Exited(code), stdout, stderr, None),
            Outcome::TimedOut { stderr } => (
                CommandStatus::TimedOut,
                Vec::new(),
                stderr,
                Some(match timeout {
                    Some(timeout) => {
                        format!("timed out after {}s and was killed", timeout.as_secs())
                    }
                    None => "timed out and was killed".into(),
                }),
            ),
            Outcome::TooLarge { stderr } => (
                CommandStatus::TooLarge,
                Vec::new(),
                stderr,
                Some(format!(
                    "wrote more than {STDOUT_LIMIT} bytes to stdout and was killed"
                )),
            ),
            Outcome::Cancelled { why, stderr } => {
                (CommandStatus::Cancelled, Vec::new(), stderr, Some(why))
            }
            Outcome::NotStarted { why } => (
                CommandStatus::NotStarted,
                Vec::new(),
                String::new(),
                Some(why),
            ),
        };
        Self {
            status,
            duration,
            stdout,
            stderr,
            error,
        }
    }
}

/// Ends the tree and reaps the root process.
async fn kill_tree(tree: &mut Tree, child: &mut tokio::process::Child) {
    tree.kill();
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
}

enum Outcome {
    Exited {
        code: Option<i32>,
        stdout: Vec<u8>,
        stderr: String,
    },
    TimedOut {
        stderr: String,
    },
    TooLarge {
        stderr: String,
    },
    Cancelled {
        why: String,
        stderr: String,
    },
    NotStarted {
        why: String,
    },
}

/// Everything a started call carries into its supervising task.
struct Job {
    call: Arc<Call>,
    lease: SnapshotLease,
    command: Vec<String>,
    stdin: Vec<u8>,
    caller: String,
    token: String,
    token_guard: TokenGuard,
    permit: Permit,
}

fn tool_specs(registry: &Registry) -> Vec<ExtensionToolSpec> {
    registry
        .list()
        .into_iter()
        .filter(|e| e.enabled)
        .filter_map(|e| e.current)
        .flat_map(|v| v.tools)
        .map(|t| ExtensionToolSpec {
            name: t.qualified_name,
            description: t.description,
            input_schema: t.input_schema,
            groups: t.groups,
        })
        .collect()
}

#[async_trait]
impl ExtensionHost for Runner {
    fn tools(&self) -> Vec<ExtensionToolSpec> {
        tool_specs(&self.registry)
    }

    async fn call(
        &self,
        caller: ExtensionCaller,
        tool: &str,
        input: Value,
    ) -> Result<Value, CoreError> {
        if let Some(result) =
            super::creator::call(self, &self.registry, &caller, tool, input.clone())
        {
            return result;
        }
        let Some((extension, bare)) = tool.split_once("__") else {
            return Err(CoreError::NotFound {
                kind: "tool",
                id: tool.to_owned(),
            });
        };
        let accepted = self.start(Caller::Agent(Box::new(caller)), extension, bare, input)?;
        let done = self.wait(&accepted.call_id).await.ok_or_else(|| {
            CoreError::Conflict(format!("{tool}: the call was lost before it ended"))
        })?;
        let why = done.error.clone().unwrap_or_default();
        match done.status {
            ExtensionCallStatus::Ok => Ok(done.output.unwrap_or(Value::Null)),
            ExtensionCallStatus::TimedOut => {
                Err(CoreError::Conflict(format!("{tool} failed: {why}")))
            }
            ExtensionCallStatus::Cancelled => {
                Err(CoreError::Conflict(format!("{tool} was cancelled: {why}")))
            }
            _ => Err(CoreError::Conflict(format!("{tool} failed: {why}"))),
        }
    }

    fn prompts(&self, groups: &[&str]) -> Vec<String> {
        let mut out = Vec::new();
        for info in self.registry.list() {
            if !info.enabled {
                continue;
            }
            let Ok(enabled) = self.registry.enabled(&info.name) else {
                continue;
            };
            for prompt in &enabled.version.prompts {
                if !prompt.groups.iter().any(|g| groups.contains(&g.as_str())) {
                    continue;
                }
                let path = enabled.path.join(&prompt.file);
                let text = std::fs::File::open(&path).and_then(|f| {
                    use std::io::Read;
                    let mut text = String::new();
                    f.take(PROMPT_LIMIT).read_to_string(&mut text).map(|_| text)
                });
                match text {
                    Ok(text) if !text.trim().is_empty() => out.push(text),
                    Ok(_) => {}
                    Err(error) => tracing::warn!(
                        extension = %info.name, file = %prompt.file, %error,
                        "cannot read an extension's prompt"
                    ),
                }
            }
        }
        out
    }
}
