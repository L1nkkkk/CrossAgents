//! The hub's side: a [`Driver`] that runs on a node (docs/tasks/B1-NODE.md §3.1).
//!
//! A node keeps up one control stream to the hub, so the hub holds one [`RemoteNode`] per
//! node and takes a [`RemoteDriver`] from it for each `(plugin, driver)` on that node: the
//! connection, the frames not yet acked and the replay bookkeeping are shared, and a driver
//! adds only the calls. [`RemoteDriver`] and [`RemoteSession`] have the same shape as a local
//! driver; each call becomes an [`Op`] on the link and each `SessionEvent` comes back as a
//! frame. Everything that is not a session call — name, kind, capabilities, models, whether
//! the CLI is installed — comes from a [`DriverInfo`]: the local driver's answer for the
//! built-in node (B1.2), the `Register` snapshot for a remote one (B1.3). This step
//! implements the local form and leaves the interface for the other.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crossagents_core::{
    AgentCommand, AttachmentSupport, Capabilities, CliStatus, CoreError, Driver, DriverKind,
    DriverSession, PermissionDecision, PermissionHandler, Prompt, SessionEvent, SessionSpec,
    TurnOutcome,
};
use tokio::sync::{mpsc, oneshot};

use crate::protocol::{
    Op, PROTOCOL_V1, ProtocolMismatch, RegisterInfo, SessionInfo, ToHub, ToNode, WireError,
    WirePrompt, WireReply, WireResult, WireSpec, decode, encode, negotiate_protocol,
};
use crate::transport::{Transport, TransportError};

/// How long a call waits for the very first connection before answering "offline".
pub const INITIAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the hub retries a dropped connection.
const RECONNECT_DELAY: Duration = Duration::from_millis(20);
/// Buffered events per session for the window between `open`'s first event and its `Return`.
const UNROUTED_LIMIT: usize = 10_000;

const OFFLINE: &str = "node offline: the hub has no connection to it";

fn offline() -> CoreError {
    CoreError::Agent(OFFLINE.to_owned())
}

/// A driver's non-session answers, for [`RemoteDriver`]. The built-in node wraps the local
/// driver; a remote node will answer from its `Register` snapshot (B1.3).
#[async_trait]
pub trait DriverInfo: Send + Sync {
    fn name(&self) -> &str;
    fn kind(&self) -> DriverKind;
    fn capabilities(&self) -> Capabilities;
    fn models(&self) -> Vec<String>;
    fn model_efforts(&self, _model: &str) -> Option<Vec<String>> {
        None
    }
    fn installed(&self) -> bool {
        true
    }
    fn cli_places(&self) -> Vec<String> {
        Vec::new()
    }
    async fn cli_status(&self) -> Option<CliStatus> {
        None
    }
}

/// [`DriverInfo`] from a local driver (the built-in node's form).
pub struct LocalDriverInfo {
    driver: Arc<dyn Driver>,
}

impl LocalDriverInfo {
    pub fn new(driver: Arc<dyn Driver>) -> Self {
        Self { driver }
    }
}

#[async_trait]
impl DriverInfo for LocalDriverInfo {
    fn name(&self) -> &str {
        self.driver.name()
    }

    fn kind(&self) -> DriverKind {
        self.driver.kind()
    }

    fn capabilities(&self) -> Capabilities {
        self.driver.capabilities()
    }

    fn models(&self) -> Vec<String> {
        self.driver.models()
    }

    fn model_efforts(&self, model: &str) -> Option<Vec<String>> {
        self.driver.model_efforts(model)
    }

    fn installed(&self) -> bool {
        self.driver.installed()
    }

    fn cli_places(&self) -> Vec<String> {
        self.driver.cli_places()
    }

    async fn cli_status(&self) -> Option<CliStatus> {
        self.driver.cli_status().await
    }
}

/// Where the hub gets a connection to its node: an in-memory link now, the tunnel later.
#[async_trait]
pub trait HubLink: Send + Sync {
    async fn connect(&self) -> Result<Arc<dyn Transport>, TransportError>;
}

/// One frame on its way to the node. `call` names the call it carries, so a connection that
/// dies before the frame leaves can fail that call instead of leaving it waiting.
struct OutFrame {
    line: String,
    call: Option<String>,
}

struct Pending {
    answer: oneshot::Sender<WireResult>,
    /// The session the call belongs to, so `Return`'s command update finds it.
    session: Option<String>,
}

struct Route {
    events: mpsc::UnboundedSender<SessionEvent>,
    permissions: Option<Arc<dyn PermissionHandler>>,
    commands: Arc<Mutex<Option<Vec<AgentCommand>>>>,
}

struct HubShared {
    link: Arc<dyn HubLink>,
    writer: Mutex<Option<mpsc::UnboundedSender<OutFrame>>>,
    ever_connected: AtomicBool,
    pending: Mutex<HashMap<String, Pending>>,
    sessions: Mutex<HashMap<String, Route>>,
    /// Events for a handle whose `open`/`fork` has not returned yet.
    unrouted: Mutex<HashMap<String, Vec<SessionEvent>>>,
    last_seq: AtomicU64,
    next_id: AtomicU64,
    register: Mutex<Option<Result<RegisterInfo, ProtocolMismatch>>>,
}

impl HubShared {
    fn writer(&self) -> Option<mpsc::UnboundedSender<OutFrame>> {
        self.writer.lock().expect("writer lock poisoned").clone()
    }

    fn send(&self, frame: ToNode) {
        if let Some(writer) = self.writer() {
            let _ = writer.send(OutFrame {
                line: encode(&frame),
                call: None,
            });
        }
    }

    fn fail(&self, call: &str) {
        if let Some(pending) = self
            .pending
            .lock()
            .expect("pending lock poisoned")
            .remove(call)
        {
            let _ = pending.answer.send(WireResult::Err {
                error: WireError::Agent {
                    message: OFFLINE.to_owned(),
                },
            });
        }
    }

    /// A writer for a call, or "offline" at once once the link has been up before.
    async fn writer_for_call(&self) -> Result<mpsc::UnboundedSender<OutFrame>, CoreError> {
        if let Some(writer) = self.writer() {
            return Ok(writer);
        }
        if self.ever_connected.load(Ordering::SeqCst) {
            return Err(offline());
        }
        let deadline = tokio::time::Instant::now() + INITIAL_CONNECT_TIMEOUT;
        loop {
            tokio::time::sleep(RECONNECT_DELAY).await;
            if let Some(writer) = self.writer() {
                return Ok(writer);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(offline());
            }
        }
    }

    async fn call(&self, op: Op, session: Option<String>) -> Result<WireReply, CoreError> {
        let writer = self.writer_for_call().await?;
        let id = format!("c{}", self.next_id.fetch_add(1, Ordering::SeqCst) + 1);
        let (answer, response) = oneshot::channel();
        self.pending.lock().expect("pending lock poisoned").insert(
            id.clone(),
            Pending {
                answer,
                session: session.clone(),
            },
        );
        let frame = OutFrame {
            line: encode(&ToNode::Call { id: id.clone(), op }),
            call: Some(id.clone()),
        };
        if writer.send(frame).is_err() {
            self.pending
                .lock()
                .expect("pending lock poisoned")
                .remove(&id);
            return Err(offline());
        }
        match response.await {
            Ok(WireResult::Ok { reply }) => Ok(reply),
            Ok(WireResult::Err { error }) => Err(error.into()),
            Err(_) => Err(offline()),
        }
    }

    /// A call whose answer nobody waits for (`refresh_cli` is sync in the trait).
    fn send_detached(&self, op: Op) {
        if let Some(writer) = self.writer() {
            let _ = writer.send(OutFrame {
                line: encode(&ToNode::Call {
                    id: format!("c{}", self.next_id.fetch_add(1, Ordering::SeqCst) + 1),
                    op,
                }),
                call: None,
            });
        }
    }

    /// False for a frame already seen: the node replays everything it has not been acked,
    /// so the hub sees a prefix again after every reconnect.
    fn first_time(&self, seq: u64) -> bool {
        if seq <= self.last_seq.load(Ordering::SeqCst) {
            return false;
        }
        self.last_seq.store(seq, Ordering::SeqCst);
        true
    }

    fn ack(&self, seq: u64) {
        self.send(ToNode::Ack { seq });
    }

    fn commands_of(&self, session: &str) -> Option<Arc<Mutex<Option<Vec<AgentCommand>>>>> {
        self.sessions
            .lock()
            .expect("sessions lock poisoned")
            .get(session)
            .map(|route| route.commands.clone())
    }

    fn set_commands(&self, session: &str, commands: Vec<AgentCommand>) {
        if let Some(cell) = self.commands_of(session) {
            *cell.lock().expect("commands lock poisoned") = Some(commands);
        }
    }

    fn deliver(&self, session: &str, event: SessionEvent) {
        let sessions = self.sessions.lock().expect("sessions lock poisoned");
        if let Some(route) = sessions.get(session) {
            let _ = route.events.send(event);
            return;
        }
        // `open` emits `Started` before its `Return`; keep those until the session exists.
        let mut unrouted = self.unrouted.lock().expect("unrouted lock poisoned");
        let buffered = unrouted.entry(session.to_owned()).or_default();
        if buffered.len() < UNROUTED_LIMIT {
            buffered.push(event);
        }
    }

    fn register(
        &self,
        handle: &str,
        events: mpsc::UnboundedSender<SessionEvent>,
        permissions: Option<Arc<dyn PermissionHandler>>,
        commands: Arc<Mutex<Option<Vec<AgentCommand>>>>,
    ) {
        let mut sessions = self.sessions.lock().expect("sessions lock poisoned");
        sessions.insert(
            handle.to_owned(),
            Route {
                events: events.clone(),
                permissions,
                commands,
            },
        );
        let mut unrouted = self.unrouted.lock().expect("unrouted lock poisoned");
        if let Some(buffered) = unrouted.remove(handle) {
            for event in buffered {
                let _ = events.send(event);
            }
        }
    }
}

async fn run(shared: Arc<HubShared>) {
    loop {
        match shared.link.connect().await {
            Ok(conn) => {
                let (writer, frames) = mpsc::unbounded_channel();
                *shared.writer.lock().expect("writer lock poisoned") = Some(writer);
                shared.ever_connected.store(true, Ordering::SeqCst);
                tokio::spawn(write_loop(shared.clone(), conn.clone(), frames));
                while let Some(line) = conn.recv_line().await {
                    match decode::<ToHub>(&line) {
                        Ok(frame) => dispatch(&shared, frame),
                        Err(err) => tracing::warn!("hub: unreadable frame: {err}"),
                    }
                }
                // The connection is gone; frames that never made it fail their calls, and
                // calls already written wait for the node's replay after the next connect.
                *shared.writer.lock().expect("writer lock poisoned") = None;
            }
            Err(_) => tokio::time::sleep(RECONNECT_DELAY).await,
        }
    }
}

async fn write_loop(
    shared: Arc<HubShared>,
    conn: Arc<dyn Transport>,
    mut frames: mpsc::UnboundedReceiver<OutFrame>,
) {
    while let Some(frame) = frames.recv().await {
        if conn.send_line(frame.line).await.is_err() {
            if let Some(call) = &frame.call {
                shared.fail(call);
            }
            // Everything still queued for this dead connection never reached the node.
            while let Ok(frame) = frames.try_recv() {
                if let Some(call) = &frame.call {
                    shared.fail(call);
                }
            }
            return;
        }
    }
}

fn dispatch(shared: &Arc<HubShared>, frame: ToHub) {
    match frame {
        ToHub::Register {
            epoch,
            protocol,
            capabilities,
            node_version,
            os,
            hostname,
            plugins,
        } => {
            let mut register = shared.register.lock().expect("register lock poisoned");
            if let Err(mismatch) = negotiate_protocol(PROTOCOL_V1, protocol) {
                *register = Some(Err(mismatch));
                return;
            }
            // A restarted node numbers its frames from 1 again; without this the hub would
            // take every one of them for a replay and drop it.
            let restarted = register
                .as_ref()
                .and_then(|previous| previous.as_ref().ok())
                .is_some_and(|previous| previous.epoch != epoch);
            if restarted {
                shared.last_seq.store(0, Ordering::SeqCst);
            }
            *register = Some(Ok(RegisterInfo {
                epoch,
                protocol,
                capabilities,
                node_version,
                os,
                hostname,
                plugins,
            }));
        }
        ToHub::Event {
            seq,
            session,
            event,
            commands,
        } => {
            if !shared.first_time(seq) {
                shared.ack(seq);
                return;
            }
            if let Some(commands) = commands {
                shared.set_commands(&session, commands.into_iter().map(Into::into).collect());
            }
            if let Some(event) = event.into() {
                shared.deliver(&session, event);
            } else {
                tracing::debug!(seq, %session, "discarding unknown session event");
            }
            shared.ack(seq);
        }
        ToHub::Return {
            seq,
            id,
            result,
            commands,
        } => {
            if !shared.first_time(seq) {
                shared.ack(seq);
                return;
            }
            let pending = shared
                .pending
                .lock()
                .expect("pending lock poisoned")
                .remove(&id);
            if let Some(pending) = pending {
                if let (Some(commands), Some(session)) = (commands, &pending.session) {
                    shared.set_commands(session, commands.into_iter().map(Into::into).collect());
                }
                let _ = pending.answer.send(result);
            }
            shared.ack(seq);
        }
        ToHub::Ask {
            seq,
            id,
            session,
            request,
        } => {
            shared.ack(seq);
            if !shared.first_time(seq) {
                return;
            }
            let handler = shared
                .sessions
                .lock()
                .expect("sessions lock poisoned")
                .get(&session)
                .and_then(|route| route.permissions.clone());
            let shared = shared.clone();
            tokio::spawn(async move {
                let request = request.into();
                let decision = match handler {
                    Some(handler) => handler.decide(&request).await,
                    // The hub did not say it has a handler; refusing is the safe answer.
                    None => PermissionDecision::Deny,
                };
                shared.send(ToNode::Answer {
                    id,
                    decision: (&decision).into(),
                });
            });
        }
        ToHub::Pong => {}
    }
}

/// The hub's end of one node: the single control stream, what the node has not been acked
/// for, and the calls waiting for a `Return`. A node accepts one connection
/// (docs/tasks/B1-NODE.md §3), so every [`RemoteDriver`] on it shares one of these — with a
/// connection each they would evict one another and ack for each other's `seq`s.
pub struct RemoteNode {
    shared: Arc<HubShared>,
    actor: tokio::task::JoinHandle<()>,
}

impl RemoteNode {
    pub fn new(link: Arc<dyn HubLink>) -> Self {
        let shared = Arc::new(HubShared {
            link,
            writer: Mutex::new(None),
            ever_connected: AtomicBool::new(false),
            pending: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            unrouted: Mutex::new(HashMap::new()),
            last_seq: AtomicU64::new(0),
            next_id: AtomicU64::new(0),
            register: Mutex::new(None),
        });
        let actor = tokio::spawn(run(shared.clone()));
        Self { shared, actor }
    }

    /// A driver for one of this node's `(plugin, driver)` pairs.
    pub fn driver(
        self: &Arc<Self>,
        plugin: impl Into<String>,
        driver: Option<DriverKind>,
        info: Arc<dyn DriverInfo>,
    ) -> RemoteDriver {
        RemoteDriver {
            plugin: plugin.into(),
            driver: driver.map(|kind| kind.as_str().to_owned()),
            info,
            node: self.clone(),
        }
    }

    /// What the node last said in `Register`, when it has said anything.
    pub fn register(&self) -> Option<RegisterInfo> {
        self.shared
            .register
            .lock()
            .expect("register lock poisoned")
            .clone()
            .and_then(Result::ok)
    }

    /// The most recent protocol mismatch, if registration was rejected.
    pub fn protocol_mismatch(&self) -> Option<ProtocolMismatch> {
        self.shared
            .register
            .lock()
            .expect("register lock poisoned")
            .as_ref()
            .and_then(|result| result.as_ref().err().copied())
    }
}

impl Drop for RemoteNode {
    fn drop(&mut self) {
        // Let the connection's writer task end instead of waiting for another frame.
        *self.shared.writer.lock().expect("writer lock poisoned") = None;
        self.actor.abort();
    }
}

/// The hub's [`Driver`] for one node and one plugin driver.
pub struct RemoteDriver {
    plugin: String,
    driver: Option<String>,
    info: Arc<dyn DriverInfo>,
    node: Arc<RemoteNode>,
}

impl RemoteDriver {
    /// A driver on a node the caller already holds. Build the [`RemoteNode`] once per node
    /// and take every driver from it.
    pub fn new(
        node: Arc<RemoteNode>,
        plugin: impl Into<String>,
        driver: Option<DriverKind>,
        info: Arc<dyn DriverInfo>,
    ) -> Self {
        Self {
            plugin: plugin.into(),
            driver: driver.map(|kind| kind.as_str().to_owned()),
            info,
            node,
        }
    }

    /// What the node last said in `Register`, when it has said anything.
    pub fn register(&self) -> Option<RegisterInfo> {
        self.node.register()
    }

    fn shared(&self) -> &Arc<HubShared> {
        &self.node.shared
    }

    async fn call(&self, op: Op, session: Option<String>) -> Result<WireReply, CoreError> {
        self.shared().call(op, session).await
    }

    fn attach(
        &self,
        info: SessionInfo,
        events: mpsc::UnboundedSender<SessionEvent>,
        permissions: Option<Arc<dyn PermissionHandler>>,
    ) -> RemoteSession {
        attach_session(&self.node, info, events, permissions)
    }

    fn session_info(reply: WireReply) -> Result<SessionInfo, CoreError> {
        match reply {
            WireReply::Session { session } => Ok(session),
            other => Err(unexpected("a session", &other)),
        }
    }

    async fn open_with(
        &self,
        op: Op,
        spec: &SessionSpec,
    ) -> Result<Box<dyn DriverSession>, CoreError> {
        let reply = self.call(op, None).await?;
        let info = Self::session_info(reply)?;
        Ok(Box::new(self.attach(
            info,
            spec.events.clone(),
            spec.permissions.clone(),
        )))
    }
}

fn unexpected(want: &str, reply: &WireReply) -> CoreError {
    CoreError::Agent(format!("node answered {reply:?} where {want} was expected"))
}

/// Make a session from the node's `Return`: its events go to the caller's channel, and the
/// commands the node reported sit in a cell the driver and the frames both update.
fn attach_session(
    node: &Arc<RemoteNode>,
    info: SessionInfo,
    events: mpsc::UnboundedSender<SessionEvent>,
    permissions: Option<Arc<dyn PermissionHandler>>,
) -> RemoteSession {
    let commands = Arc::new(Mutex::new(
        info.commands
            .clone()
            .map(|xs| xs.into_iter().map(Into::into).collect()),
    ));
    node.shared
        .register(&info.handle, events, permissions, commands.clone());
    RemoteSession {
        node: node.clone(),
        handle: info.handle,
        session_id: info.session_id,
        attachment_support: info.attachment_support.map(Into::into),
        outlives_hub: info.outlives_hub,
        commands,
        closed: AtomicBool::new(false),
    }
}

#[async_trait]
impl Driver for RemoteDriver {
    fn name(&self) -> &str {
        self.info.name()
    }

    fn kind(&self) -> DriverKind {
        self.info.kind()
    }

    fn capabilities(&self) -> Capabilities {
        self.info.capabilities()
    }

    fn models(&self) -> Vec<String> {
        self.info.models()
    }

    fn model_efforts(&self, model: &str) -> Option<Vec<String>> {
        self.info.model_efforts(model)
    }

    fn installed(&self) -> bool {
        self.info.installed()
    }

    async fn cli_status(&self) -> Option<CliStatus> {
        self.info.cli_status().await
    }

    async fn probe_cli(&self) {
        let _ = self
            .call(
                Op::ProbeCli {
                    plugin: self.plugin.clone(),
                    driver: self.driver.clone(),
                },
                None,
            )
            .await;
    }

    fn refresh_cli(&self) {
        self.shared().send_detached(Op::RefreshCli {
            plugin: self.plugin.clone(),
            driver: self.driver.clone(),
        });
    }

    fn cli_places(&self) -> Vec<String> {
        self.info.cli_places()
    }

    async fn open(&self, spec: SessionSpec) -> Result<Box<dyn DriverSession>, CoreError> {
        let wire = WireSpec::from_spec(&spec);
        self.open_with(
            Op::Open {
                plugin: self.plugin.clone(),
                driver: self.driver.clone(),
                spec: Box::new(wire),
            },
            &spec,
        )
        .await
    }

    async fn resume(
        &self,
        session_id: &str,
        spec: SessionSpec,
    ) -> Result<Box<dyn DriverSession>, CoreError> {
        let wire = WireSpec::from_spec(&spec);
        self.open_with(
            Op::Resume {
                plugin: self.plugin.clone(),
                driver: self.driver.clone(),
                session_id: session_id.to_owned(),
                spec: Box::new(wire),
            },
            &spec,
        )
        .await
    }

    async fn adopt(
        &self,
        session_id: &str,
        turn_running: bool,
        spec: SessionSpec,
    ) -> Result<Option<Box<dyn DriverSession>>, CoreError> {
        let wire = WireSpec::from_spec(&spec);
        let reply = self
            .call(
                Op::Adopt {
                    plugin: self.plugin.clone(),
                    driver: self.driver.clone(),
                    session_id: session_id.to_owned(),
                    turn_running,
                    spec: Box::new(wire),
                },
                None,
            )
            .await?;
        match reply {
            WireReply::NoSession => Ok(None),
            WireReply::Session { session } => Ok(Some(Box::new(self.attach(
                session,
                spec.events.clone(),
                spec.permissions.clone(),
            )))),
            other => Err(unexpected("a session or none", &other)),
        }
    }
}

/// The hub's [`DriverSession`]: every call names the node's handle for it.
pub struct RemoteSession {
    node: Arc<RemoteNode>,
    handle: String,
    session_id: String,
    attachment_support: Option<AttachmentSupport>,
    outlives_hub: bool,
    commands: Arc<Mutex<Option<Vec<AgentCommand>>>>,
    /// `shutdown` already told the node; `Drop` must not send it again.
    closed: AtomicBool,
}

impl RemoteSession {
    async fn call(&self, op: Op) -> Result<WireReply, CoreError> {
        self.node.shared.call(op, Some(self.handle.clone())).await
    }

    fn session(&self) -> String {
        self.handle.clone()
    }
}

impl Drop for RemoteSession {
    fn drop(&mut self) {
        self.node
            .shared
            .sessions
            .lock()
            .expect("sessions lock poisoned")
            .remove(&self.handle);
        // A local driver ends the agent process with the session, so a dropped session has
        // to end on the node too — otherwise the process runs on with nobody to talk to.
        // A session that is meant to outlive the hub stays.
        if !self.closed.load(Ordering::SeqCst) && !self.outlives_hub {
            self.node.shared.send_detached(Op::Shutdown {
                session: self.handle.clone(),
            });
        }
    }
}

#[async_trait]
impl DriverSession for RemoteSession {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    async fn prompt(&self, text: &str) -> Result<TurnOutcome, CoreError> {
        self.prompt_with(&Prompt::text(text)).await
    }

    async fn prompt_with(&self, prompt: &Prompt) -> Result<TurnOutcome, CoreError> {
        let reply = self
            .call(Op::Prompt {
                session: self.session(),
                prompt: WirePrompt::from(prompt),
            })
            .await?;
        match reply {
            WireReply::Turn { outcome } => Ok(outcome.into()),
            other => Err(unexpected("a turn", &other)),
        }
    }

    fn attachment_support(&self) -> Option<AttachmentSupport> {
        self.attachment_support
    }

    fn commands(&self) -> Option<Vec<AgentCommand>> {
        self.commands
            .lock()
            .expect("commands lock poisoned")
            .clone()
    }

    async fn steer(
        &self,
        message_id: crossagents_core::MessageId,
        prompt: &Prompt,
    ) -> Result<(), CoreError> {
        self.call(Op::Steer {
            session: self.session(),
            message_id: message_id.to_string(),
            prompt: WirePrompt::from(prompt),
        })
        .await
        .map(|_| ())
    }

    async fn interrupt(&self) -> Result<(), CoreError> {
        self.call(Op::Interrupt {
            session: self.session(),
        })
        .await
        .map(|_| ())
    }

    async fn suspend(&self) -> Result<(), CoreError> {
        self.call(Op::Suspend {
            session: self.session(),
        })
        .await
        .map(|_| ())
    }

    async fn fork(
        &self,
        at_turn: Option<&str>,
        spec: SessionSpec,
    ) -> Result<Box<dyn DriverSession>, CoreError> {
        let wire = WireSpec::from_spec(&spec);
        let reply = self
            .call(Op::Fork {
                session: self.session(),
                at_turn: at_turn.map(str::to_owned),
                plugin: spec.agent.plugin.clone(),
                driver: spec.agent.driver.clone(),
                spec: Box::new(wire),
            })
            .await?;
        match reply {
            WireReply::Session { session } => Ok(Box::new(attach_session(
                &self.node,
                session,
                spec.events.clone(),
                spec.permissions.clone(),
            ))),
            other => Err(unexpected("a session", &other)),
        }
    }

    async fn rollback(&self, before_turn: &str, revert_files: bool) -> Result<(), CoreError> {
        self.call(Op::Rollback {
            session: self.session(),
            before_turn: before_turn.to_owned(),
            revert_files,
        })
        .await
        .map(|_| ())
    }

    async fn set_model(&self, model: Option<&str>, effort: Option<&str>) -> Result<(), CoreError> {
        self.call(Op::SetModel {
            session: self.session(),
            model: model.map(str::to_owned),
            effort: effort.map(str::to_owned),
        })
        .await
        .map(|_| ())
    }

    async fn compact(&self) -> Result<(), CoreError> {
        self.call(Op::Compact {
            session: self.session(),
        })
        .await
        .map(|_| ())
    }

    async fn shutdown(&self) {
        if self
            .call(Op::Shutdown {
                session: self.session(),
            })
            .await
            .is_ok()
        {
            self.closed.store(true, Ordering::SeqCst);
        }
    }

    fn outlives_hub(&self) -> bool {
        self.outlives_hub
    }

    async fn adopted_turn(&self) -> Option<Result<TurnOutcome, CoreError>> {
        match self
            .call(Op::AdoptedTurn {
                session: self.session(),
            })
            .await
        {
            Ok(WireReply::MaybeTurn { outcome }) => outcome.map(|value| Ok(value.into())),
            Ok(other) => Some(Err(unexpected("an adopted turn", &other))),
            Err(err) => Some(Err(err)),
        }
    }
}
