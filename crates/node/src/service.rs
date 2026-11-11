//! The node's side: run `Driver` calls, send `SessionEvent`s, keep what the hub has not
//! acknowledged (docs/tasks/B1-NODE.md §3.1, §4).
//!
//! [`NodeService`] knows nothing about the transport beyond [`TransportServer`] and nothing
//! about the hub beyond the frames. It is the one execution path for both a remote node and
//! the hub's built-in one: B1.2 points this at the same drivers the hub used to call
//! directly, over an in-memory link.

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crossagents_core::{
    AgentCommand, CoreError, Driver, DriverSession, MessageId, PermissionDecision,
    PermissionHandler, PermissionPolicy, PermissionRequest, SessionEvent, SessionEventKind,
    SessionSpec,
};
use tokio::sync::{mpsc, oneshot};

use crate::protocol::{
    Op, RegisterInfo, SessionInfo, ToHub, ToNode, WireCliStatus, WireError, WireReply, WireResult,
    WireSpec, decode, encode,
};
use crate::transport::{Transport, TransportServer};

/// How many unacknowledged frames the node keeps before dropping the oldest.
pub const UNACKED_LIMIT: usize = 100_000;

/// How long the node waits for the hub's `Answer` before falling back to the spec's policy.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(60);

/// Where the node gets a driver for `(plugin, driver)`. B1.2 implements this with the hub's
/// plugin registry; the node itself does not know how drivers are built.
pub trait DriverSource: Send + Sync {
    fn driver(&self, plugin: &str, driver: Option<&str>) -> Result<Arc<dyn Driver>, CoreError>;
}

/// A source with exactly one driver, for tests and for a hub with a single plugin.
pub struct SingleDriverSource {
    pub plugin: String,
    pub driver: Arc<dyn Driver>,
}

impl DriverSource for SingleDriverSource {
    fn driver(&self, plugin: &str, _driver: Option<&str>) -> Result<Arc<dyn Driver>, CoreError> {
        if plugin == self.plugin {
            Ok(self.driver.clone())
        } else {
            Err(CoreError::NotFound {
                kind: "plugin",
                id: plugin.to_owned(),
            })
        }
    }
}

/// A node: drivers by name, the sessions it runs, and the frames the hub has not acked.
pub struct NodeService {
    source: Arc<dyn DriverSource>,
    register: RegisterInfo,
}

impl NodeService {
    pub fn new(source: Arc<dyn DriverSource>) -> Self {
        Self {
            source,
            register: RegisterInfo::local(),
        }
    }

    /// What `Register` reports (B1.4 fills `plugins` in; B1.1 sends an empty list).
    pub fn with_register(mut self, register: RegisterInfo) -> Self {
        self.register = register;
        self
    }

    /// Serves connections until the link's accepting end goes away. Agent processes keep
    /// running while no hub is connected: their frames go into the unacked cache.
    pub async fn run(self, server: Arc<dyn TransportServer>) {
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            sessions: Mutex::new(HashMap::new()),
            answers: Mutex::new(HashMap::new()),
            out: out_tx,
            next_handle: AtomicU64::new(0),
            next_ask: AtomicU64::new(0),
        });
        let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Inbound>();
        let mut frames: BTreeMap<u64, ToHub> = BTreeMap::new();
        let mut next_seq: u64 = 1;
        let mut writer: Option<mpsc::UnboundedSender<String>> = None;
        let mut current: Option<u64> = None;
        let mut generation: u64 = 0;

        loop {
            tokio::select! {
                accepted = server.accept() => {
                    let Some(conn) = accepted else { break };
                    generation += 1;
                    current = Some(generation);
                    let (tx, rx) = mpsc::unbounded_channel();
                    writer = Some(tx);
                    tokio::spawn(connection(conn, rx, in_tx.clone(), generation));

                    // Register first, then everything the hub may have missed.
                    let register = ToHub::Register {
                        node_version: self.register.node_version.clone(),
                        os: self.register.os.clone(),
                        hostname: self.register.hostname.clone(),
                        plugins: self.register.plugins.clone(),
                    };
                    write(&writer, encode(&register));
                    for frame in frames.values() {
                        write(&writer, encode(frame));
                    }
                }
                Some(inbound) = in_rx.recv() => {
                    match inbound {
                        Inbound::Line { generation: g, line } if current == Some(g) => {
                            on_line(&self.source, &shared, &mut frames, &writer, &line);
                        }
                        Inbound::Closed { generation: g } if current == Some(g) => {
                            writer = None;
                            current = None;
                        }
                        _ => {}
                    }
                }
                Some(out) = out_rx.recv() => {
                    next_seq = emit(&mut frames, next_seq, out, &writer);
                }
            }
        }
    }
}

struct SessionState {
    session: Arc<dyn DriverSession>,
    /// The commands last reported to the hub, so only changes ride along.
    commands: Mutex<Option<Vec<AgentCommand>>>,
}

impl SessionState {
    /// The session's commands, if they changed since the last frame that carried them.
    fn changed_commands(&self) -> Option<Vec<AgentCommand>> {
        let now = self.session.commands();
        let mut last = self.commands.lock().expect("commands lock poisoned");
        if *last == now {
            return None;
        }
        *last = now.clone();
        now
    }
}

struct Shared {
    sessions: Mutex<HashMap<String, Arc<SessionState>>>,
    answers: Mutex<HashMap<String, oneshot::Sender<PermissionDecision>>>,
    out: mpsc::UnboundedSender<Outgoing>,
    next_handle: AtomicU64,
    next_ask: AtomicU64,
}

impl Shared {
    fn new_handle(&self) -> String {
        format!("s{}", self.next_handle.fetch_add(1, Ordering::SeqCst) + 1)
    }

    fn session(&self, handle: &str) -> Result<Arc<SessionState>, CoreError> {
        self.sessions
            .lock()
            .expect("sessions lock poisoned")
            .get(handle)
            .cloned()
            .ok_or_else(|| CoreError::NotFound {
                kind: "session",
                id: handle.to_owned(),
            })
    }
}

/// Something the node has to number, cache and send.
enum Outgoing {
    Event {
        session: String,
        event: SessionEvent,
        commands: Option<Vec<AgentCommand>>,
    },
    Return {
        /// The session the call named, so an overflow warning has somewhere to go.
        session: Option<String>,
        id: String,
        result: WireResult,
        commands: Option<Vec<AgentCommand>>,
    },
    Ask {
        id: String,
        session: String,
        request: PermissionRequest,
    },
}

impl Outgoing {
    fn into_frame(self, seq: u64) -> ToHub {
        match self {
            Self::Event {
                session,
                event,
                commands,
            } => ToHub::Event {
                seq,
                session,
                event,
                commands,
            },
            Self::Return {
                id,
                result,
                commands,
                ..
            } => ToHub::Return {
                seq,
                id,
                result,
                commands,
            },
            Self::Ask {
                id,
                session,
                request,
            } => ToHub::Ask {
                seq,
                id,
                session,
                request,
            },
        }
    }

    fn session(&self) -> Option<&str> {
        match self {
            Self::Event { session, .. } | Self::Ask { session, .. } => Some(session),
            Self::Return { session, .. } => session.as_deref(),
        }
    }
}

/// Number a frame, keep it until acked, and send it if a hub is connected. Over the limit,
/// the oldest frames go and one `Warning` tells the session they did.
fn emit(
    frames: &mut BTreeMap<u64, ToHub>,
    mut next_seq: u64,
    out: Outgoing,
    writer: &Option<mpsc::UnboundedSender<String>>,
) -> u64 {
    let session = out.session().map(str::to_owned);
    let frame = out.into_frame(next_seq);
    next_seq += 1;
    frames.insert(next_seq - 1, frame.clone());
    write(writer, encode(&frame));

    if frames.len() > UNACKED_LIMIT {
        let mut warn_session = session;
        let mut dropped = 0usize;
        while frames.len() > UNACKED_LIMIT {
            let Some((_, old)) = frames.pop_first() else {
                break;
            };
            dropped += 1;
            if warn_session.is_none() {
                warn_session = old.session().map(str::to_owned);
            }
        }
        // A `Return` names no session on the wire; fall back to any frame still cached.
        let warn_session = warn_session.or_else(|| {
            frames
                .values()
                .find_map(|frame| frame.session().map(str::to_owned))
        });
        if let Some(session) = warn_session {
            let message = format!(
                "{dropped} unacknowledged event(s) were dropped: the node keeps at most {UNACKED_LIMIT}"
            );
            let warning = ToHub::Event {
                seq: next_seq,
                session,
                event: SessionEvent::from(SessionEventKind::Warning { message }),
                commands: None,
            };
            next_seq += 1;
            frames.insert(next_seq - 1, warning.clone());
            write(writer, encode(&warning));
            // The warning itself must not push the cache back over the limit.
            while frames.len() > UNACKED_LIMIT {
                frames.pop_first();
            }
        }
    }
    next_seq
}

fn write(writer: &Option<mpsc::UnboundedSender<String>>, line: String) {
    if let Some(writer) = writer {
        let _ = writer.send(line);
    }
}

fn on_line(
    source: &Arc<dyn DriverSource>,
    shared: &Arc<Shared>,
    frames: &mut BTreeMap<u64, ToHub>,
    writer: &Option<mpsc::UnboundedSender<String>>,
    line: &str,
) {
    let frame: ToNode = match decode(line) {
        Ok(frame) => frame,
        Err(err) => {
            tracing::warn!("node: unreadable frame: {err}");
            return;
        }
    };
    match frame {
        ToNode::Ack { seq } => frames.retain(|numbered, _| *numbered > seq),
        ToNode::Answer { id, decision } => {
            if let Some(waiter) = shared
                .answers
                .lock()
                .expect("answers lock poisoned")
                .remove(&id)
            {
                let _ = waiter.send(decision);
            }
        }
        ToNode::Ping => write(writer, encode(&ToHub::Pong)),
        ToNode::Call { id, op } => {
            let source = source.clone();
            let shared = shared.clone();
            tokio::spawn(async move {
                let (result, commands, session) = execute(&source, &shared, op).await;
                let _ = shared.out.send(Outgoing::Return {
                    session,
                    id,
                    result,
                    commands,
                });
            });
        }
    }
}

async fn execute(
    source: &Arc<dyn DriverSource>,
    shared: &Arc<Shared>,
    op: Op,
) -> (WireResult, Option<Vec<AgentCommand>>, Option<String>) {
    match run_op(source, shared, op).await {
        Ok((reply, commands, session)) => (WireResult::Ok { reply }, commands, session),
        Err(err) => (
            WireResult::Err {
                error: WireError::from(&err),
            },
            None,
            None,
        ),
    }
}

type OpResult = Result<(WireReply, Option<Vec<AgentCommand>>, Option<String>), CoreError>;

async fn run_op(source: &Arc<dyn DriverSource>, shared: &Arc<Shared>, op: Op) -> OpResult {
    match op {
        Op::Open {
            plugin,
            driver,
            spec,
        } => {
            let driver = source.driver(&plugin, driver.as_deref())?;
            let handle = shared.new_handle();
            let (spec, events) = make_spec(shared, &handle, *spec);
            let session: Arc<dyn DriverSession> = Arc::from(driver.open(spec).await?);
            let info = finish_open(shared, handle.clone(), session, events);
            Ok((WireReply::Session { session: info }, None, Some(handle)))
        }
        Op::Resume {
            plugin,
            driver,
            session_id,
            spec,
        } => {
            let driver = source.driver(&plugin, driver.as_deref())?;
            let handle = shared.new_handle();
            let (spec, events) = make_spec(shared, &handle, *spec);
            let session: Arc<dyn DriverSession> =
                Arc::from(driver.resume(&session_id, spec).await?);
            let info = finish_open(shared, handle.clone(), session, events);
            Ok((WireReply::Session { session: info }, None, Some(handle)))
        }
        Op::Adopt {
            plugin,
            driver,
            session_id,
            turn_running,
            spec,
        } => {
            let driver = source.driver(&plugin, driver.as_deref())?;
            let handle = shared.new_handle();
            let (spec, events) = make_spec(shared, &handle, *spec);
            match driver.adopt(&session_id, turn_running, spec).await? {
                Some(session) => {
                    let session: Arc<dyn DriverSession> = Arc::from(session);
                    let info = finish_open(shared, handle.clone(), session, events);
                    Ok((WireReply::Session { session: info }, None, Some(handle)))
                }
                None => Ok((WireReply::NoSession, None, None)),
            }
        }
        Op::AdoptedTurn { session } => {
            let state = shared.session(&session)?;
            let outcome = match state.session.adopted_turn().await {
                Some(Ok(outcome)) => Some(outcome),
                Some(Err(err)) => return Err(err),
                None => None,
            };
            Ok((WireReply::MaybeTurn { outcome }, None, Some(session)))
        }
        Op::Prompt { session, prompt } => {
            let state = shared.session(&session)?;
            let outcome = state.session.prompt_with(&prompt.into()).await?;
            let commands = state.changed_commands();
            Ok((WireReply::Turn { outcome }, commands, Some(session)))
        }
        Op::Steer {
            session,
            message_id,
            prompt,
        } => {
            let state = shared.session(&session)?;
            let message_id = MessageId::from_str(&message_id)
                .map_err(|err| CoreError::Agent(format!("bad message id {message_id:?}: {err}")))?;
            state.session.steer(message_id, &prompt.into()).await?;
            Ok((WireReply::Unit, None, Some(session)))
        }
        Op::Interrupt { session } => {
            shared.session(&session)?.session.interrupt().await?;
            Ok((WireReply::Unit, None, Some(session)))
        }
        Op::Suspend { session } => {
            shared.session(&session)?.session.suspend().await?;
            Ok((WireReply::Unit, None, Some(session)))
        }
        Op::Fork {
            session,
            at_turn,
            plugin: _,
            driver: _,
            spec,
        } => {
            let state = shared.session(&session)?;
            let handle = shared.new_handle();
            let (spec, events) = make_spec(shared, &handle, *spec);
            let forked: Arc<dyn DriverSession> =
                Arc::from(state.session.fork(at_turn.as_deref(), spec).await?);
            let info = finish_open(shared, handle.clone(), forked, events);
            Ok((WireReply::Session { session: info }, None, Some(handle)))
        }
        Op::Rollback {
            session,
            before_turn,
            revert_files,
        } => {
            shared
                .session(&session)?
                .session
                .rollback(&before_turn, revert_files)
                .await?;
            Ok((WireReply::Unit, None, Some(session)))
        }
        Op::SetModel {
            session,
            model,
            effort,
        } => {
            shared
                .session(&session)?
                .session
                .set_model(model.as_deref(), effort.as_deref())
                .await?;
            Ok((WireReply::Unit, None, Some(session)))
        }
        Op::Compact { session } => {
            shared.session(&session)?.session.compact().await?;
            Ok((WireReply::Unit, None, Some(session)))
        }
        Op::Shutdown { session } => {
            let state = shared.session(&session)?;
            state.session.shutdown().await;
            shared
                .sessions
                .lock()
                .expect("sessions lock poisoned")
                .remove(&session);
            Ok((WireReply::Unit, None, Some(session)))
        }
        Op::ProbeCli { plugin, driver } => {
            source.driver(&plugin, driver.as_deref())?.probe_cli().await;
            Ok((WireReply::Unit, None, None))
        }
        Op::RefreshCli { plugin, driver } => {
            source.driver(&plugin, driver.as_deref())?.refresh_cli();
            Ok((WireReply::Unit, None, None))
        }
        Op::CliStatus { plugin, driver } => {
            let status = source
                .driver(&plugin, driver.as_deref())?
                .cli_status()
                .await
                .map(WireCliStatus::from);
            Ok((WireReply::CliStatus { status }, None, None))
        }
    }
}

/// The spec the driver opens with: the node's event channel, and the `Ask` handler when the
/// hub said it has one.
fn make_spec(
    shared: &Arc<Shared>,
    handle: &str,
    wire: WireSpec,
) -> (SessionSpec, mpsc::UnboundedReceiver<SessionEvent>) {
    let (events, receiver) = mpsc::unbounded_channel();
    let handler = wire.permissions.then(|| {
        Arc::new(AskHandler {
            shared: shared.clone(),
            session: handle.to_owned(),
            policy: wire.permission,
            events: events.clone(),
        }) as Arc<dyn PermissionHandler>
    });
    (wire.into_spec(events, handler), receiver)
}

fn finish_open(
    shared: &Arc<Shared>,
    handle: String,
    session: Arc<dyn DriverSession>,
    events: mpsc::UnboundedReceiver<SessionEvent>,
) -> SessionInfo {
    let info = SessionInfo {
        handle: handle.clone(),
        session_id: session.session_id().to_owned(),
        attachment_support: session.attachment_support(),
        outlives_hub: session.outlives_hub(),
        commands: session.commands(),
    };
    let state = Arc::new(SessionState {
        session,
        commands: Mutex::new(info.commands.clone()),
    });
    shared
        .sessions
        .lock()
        .expect("sessions lock poisoned")
        .insert(handle.clone(), state.clone());
    tokio::spawn(pump(shared.out.clone(), handle, state, events));
    info
}

async fn pump(
    out: mpsc::UnboundedSender<Outgoing>,
    handle: String,
    state: Arc<SessionState>,
    mut events: mpsc::UnboundedReceiver<SessionEvent>,
) {
    while let Some(event) = events.recv().await {
        let commands = state.changed_commands();
        if out
            .send(Outgoing::Event {
                session: handle.clone(),
                event,
                commands,
            })
            .is_err()
        {
            break;
        }
    }
}

/// Answers a driver's permission request with an `Ask` frame, waiting for the hub's
/// `Answer`; after [`ANSWER_TIMEOUT`] the spec's policy decides, and a `Warning` says why.
struct AskHandler {
    shared: Arc<Shared>,
    session: String,
    policy: PermissionPolicy,
    events: mpsc::UnboundedSender<SessionEvent>,
}

#[async_trait]
impl PermissionHandler for AskHandler {
    async fn decide(&self, request: &PermissionRequest) -> PermissionDecision {
        let id = format!(
            "ask-{}",
            self.shared.next_ask.fetch_add(1, Ordering::SeqCst) + 1
        );
        let (waiter, answer) = oneshot::channel();
        self.shared
            .answers
            .lock()
            .expect("answers lock poisoned")
            .insert(id.clone(), waiter);
        if self
            .shared
            .out
            .send(Outgoing::Ask {
                id: id.clone(),
                session: self.session.clone(),
                request: request.clone(),
            })
            .is_err()
        {
            self.shared
                .answers
                .lock()
                .expect("answers lock poisoned")
                .remove(&id);
            return self.policy.decide();
        }
        match tokio::time::timeout(ANSWER_TIMEOUT, answer).await {
            Ok(Ok(decision)) => decision,
            _ => {
                self.shared
                    .answers
                    .lock()
                    .expect("answers lock poisoned")
                    .remove(&id);
                let decision = self.policy.decide();
                let message = format!(
                    "the hub did not answer permission request {:?} within {}s; the policy {:?} decided {decision:?}",
                    request.title,
                    ANSWER_TIMEOUT.as_secs(),
                    self.policy,
                );
                let _ = self
                    .events
                    .send(SessionEvent::from(SessionEventKind::Warning { message }));
                decision
            }
        }
    }
}

enum Inbound {
    Line { generation: u64, line: String },
    Closed { generation: u64 },
}

/// One connection: read lines in, write queued lines out, and say when it closed.
async fn connection(
    conn: Arc<dyn Transport>,
    mut out: mpsc::UnboundedReceiver<String>,
    inbound: mpsc::UnboundedSender<Inbound>,
    generation: u64,
) {
    loop {
        tokio::select! {
            line = conn.recv_line() => match line {
                Some(line) => {
                    if inbound.send(Inbound::Line { generation, line }).is_err() {
                        break;
                    }
                }
                None => break,
            },
            outgoing = out.recv() => match outgoing {
                Some(line) => {
                    if conn.send_line(line).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
        }
    }
    let _ = inbound.send(Inbound::Closed { generation });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossagents_core::{PermissionDecision, SessionEventKind};

    #[tokio::test(start_paused = true)]
    async fn an_unanswered_ask_falls_back_to_the_policy_and_warns() {
        let (out, mut frames) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            sessions: Mutex::new(HashMap::new()),
            answers: Mutex::new(HashMap::new()),
            out,
            next_handle: AtomicU64::new(0),
            next_ask: AtomicU64::new(0),
        });
        let (events, mut emitted) = mpsc::unbounded_channel();
        let handler = AskHandler {
            shared: shared.clone(),
            session: "s1".into(),
            policy: PermissionPolicy::Deny,
            events,
        };
        let request = PermissionRequest {
            id: "r1".into(),
            turn_id: None,
            kind: "command".into(),
            title: "rm -rf /tmp/x".into(),
        };
        let deciding = tokio::spawn({
            let handler = handler;
            let request = request.clone();
            async move { handler.decide(&request).await }
        });

        // The Ask reaches the wire; nobody answers it.
        tokio::task::yield_now().await;
        assert!(matches!(frames.try_recv(), Ok(Outgoing::Ask { .. })));
        tokio::time::advance(ANSWER_TIMEOUT).await;

        assert_eq!(deciding.await.unwrap(), PermissionDecision::Deny);
        let warning = emitted.try_recv().expect("a warning was sent");
        assert!(matches!(warning.kind, SessionEventKind::Warning { .. }));
        assert!(shared.answers.lock().unwrap().is_empty());
    }
}
