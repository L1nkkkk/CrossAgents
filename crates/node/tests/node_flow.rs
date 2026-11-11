//! The node and the remote driver over the in-memory link (docs/tasks/B1-NODE.md §8, B1.1).
//!
//! Most tests run a scripted [`Driver`] that plays the mock agent's event sequence (turn
//! start, a tool call, output, usage, turn end) and takes [`PermissionHandler`] requests. It
//! lets a test pause a turn at an exact point (to cut the link, answer a permission request or
//! time out an `Ask`), which a spawned agent cannot. The real ACP driver over the actual
//! `crates/mock-agent` binary is covered by `tests/mock_agent.rs`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crossagents_core::{
    Agent, AgentId, AgentStatus, Capabilities, CoreError, Driver, DriverKind, DriverSession,
    NameSource, OutputStream, PermissionDecision, PermissionHandler, PermissionPolicy,
    PermissionRequest, Role, SessionEvent, SessionEventKind, SessionSpec, StopReason, ToolStatus,
    TurnOutcome,
};
use crossagents_node::{
    DriverInfo, DriverSource, LocalDriverInfo, MemoryHub, NodeService, RemoteDriver, RemoteNode,
    SingleDriverSource, TransportServer, memory_pair,
};
use tokio::sync::mpsc;

fn agent(plugin: &str) -> Agent {
    Agent {
        id: AgentId::new(),
        name: "mocky".into(),
        name_source: NameSource::Chosen,
        plugin: plugin.into(),
        driver: None,
        model: None,
        role: Role::Worker,
        home: std::env::temp_dir(),
        cwd: std::env::temp_dir(),
        config: serde_json::Value::Null,
        status: AgentStatus::Idle,
        created_at: chrono::Utc::now(),
        profile: Default::default(),
        topic: None,
        archived_at: None,
    }
}

fn spec(
    events: mpsc::UnboundedSender<SessionEvent>,
    permissions: Option<Arc<dyn PermissionHandler>>,
) -> SessionSpec {
    SessionSpec {
        agent: agent("scripted"),
        mcp_servers: vec![],
        permission: PermissionPolicy::Bypass,
        permissions,
        command: None,
        instructions: None,
        env: vec![],
        events,
    }
}

/// The scripted agent, with the mock agent's event shapes.
struct ScriptedDriver {
    label: &'static str,
    /// How long a turn takes. Non-zero leaves a window in which a test can cut the link.
    turn: Duration,
    shutdowns: Arc<AtomicUsize>,
}

impl ScriptedDriver {
    fn new(label: &'static str, turn: Duration) -> Arc<Self> {
        Arc::new(Self {
            label,
            turn,
            shutdowns: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn session_id(&self) -> String {
        format!("scripted-{}", self.label)
    }
}

#[async_trait]
impl Driver for ScriptedDriver {
    fn name(&self) -> &str {
        self.label
    }

    fn kind(&self) -> DriverKind {
        DriverKind::AcpGeneric
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            interrupt: true,
            permission_callback: true,
            ..Capabilities::default()
        }
    }

    fn models(&self) -> Vec<String> {
        vec!["scripted-model".into()]
    }

    async fn open(&self, spec: SessionSpec) -> Result<Box<dyn DriverSession>, CoreError> {
        let _ = spec
            .events
            .send(SessionEvent::from(SessionEventKind::Started {
                session_id: self.session_id(),
            }));
        Ok(Box::new(ScriptedSession {
            label: self.label,
            session_id: self.session_id(),
            turn: self.turn,
            shutdowns: self.shutdowns.clone(),
            events: spec.events,
            permissions: spec.permissions,
        }))
    }
}

struct ScriptedSession {
    label: &'static str,
    session_id: String,
    turn: Duration,
    shutdowns: Arc<AtomicUsize>,
    events: mpsc::UnboundedSender<SessionEvent>,
    permissions: Option<Arc<dyn PermissionHandler>>,
}

impl ScriptedSession {
    fn emit(&self, kind: SessionEventKind) {
        let _ = self.events.send(SessionEvent::from(kind));
    }
}

#[async_trait]
impl DriverSession for ScriptedSession {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    async fn prompt(&self, text: &str) -> Result<TurnOutcome, CoreError> {
        self.emit(SessionEventKind::TurnStarted { turn_id: None });
        self.emit(SessionEventKind::ToolCall {
            tool_call_id: "t1".into(),
            title: "Run mock tool".into(),
            kind: Some("execute".into()),
            status: ToolStatus::Pending,
        });
        // Long enough that a test can cut the link after `TurnStarted` and still have
        // events (and the turn's `Return`) crossing the node while it is disconnected.
        if !self.turn.is_zero() {
            tokio::time::sleep(self.turn).await;
        }
        if let Some(handler) = &self.permissions {
            let decision = handler
                .decide(&PermissionRequest {
                    id: "r1".into(),
                    turn_id: None,
                    kind: "command".into(),
                    title: "cargo test".into(),
                })
                .await;
            self.emit(SessionEventKind::Output {
                stream: OutputStream::Text,
                text: format!("permission={decision:?} "),
            });
        }
        self.emit(SessionEventKind::ToolCallUpdated {
            tool_call_id: "t1".into(),
            title: None,
            status: Some(ToolStatus::Completed),
        });
        self.emit(SessionEventKind::Output {
            stream: OutputStream::Text,
            text: format!("echo[{}]: {text}", self.label),
        });
        self.emit(SessionEventKind::Usage {
            used: 42,
            size: 1000,
            cost_usd: None,
            tokens: None,
        });
        self.emit(SessionEventKind::TurnCompleted {
            turn_id: None,
            stop_reason: StopReason::EndTurn,
        });
        Ok(TurnOutcome {
            stop_reason: StopReason::EndTurn,
            turn_id: None,
        })
    }

    async fn interrupt(&self) -> Result<(), CoreError> {
        Ok(())
    }

    async fn shutdown(&self) {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
    }
}

/// A node serving the scripted driver, and the hub's side of it.
struct Harness {
    hub: MemoryHub,
    remote: RemoteDriver,
    service: tokio::task::JoinHandle<()>,
}

/// A node whose `DriverSource` is `source`, and the hub's end of its single connection.
fn serve(
    source: Arc<dyn DriverSource>,
) -> (MemoryHub, Arc<RemoteNode>, tokio::task::JoinHandle<()>) {
    let (hub, node) = memory_pair();
    let service = tokio::spawn(async move {
        NodeService::new(source).run(Arc::new(node)).await;
    });
    let node = Arc::new(RemoteNode::new(
        Arc::new(hub.clone()) as Arc<dyn crossagents_node::HubLink>
    ));
    (hub, node, service)
}

fn harness_for(driver: Arc<ScriptedDriver>) -> Harness {
    let driver: Arc<dyn Driver> = driver;
    let source: Arc<dyn DriverSource> = Arc::new(SingleDriverSource {
        plugin: "scripted".into(),
        driver: driver.clone(),
    });
    let (hub, node, service) = serve(source);
    let info: Arc<dyn DriverInfo> = Arc::new(LocalDriverInfo::new(driver));
    let remote = node.driver("scripted", None, info);
    Harness {
        hub,
        remote,
        service,
    }
}

fn start() -> Harness {
    harness_for(ScriptedDriver::new("scripted", Duration::from_millis(40)))
}

impl Harness {
    fn stop(self) {
        self.service.abort();
    }
}

/// Collects events up to and including the turn's `TurnCompleted`.
async fn turn_events(rx: &mut mpsc::UnboundedReceiver<SessionEvent>) -> Vec<SessionEvent> {
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(60), rx.recv())
            .await
            .expect("the turn's events arrive")
            .expect("the event channel stays open");
        let done = matches!(
            event.kind,
            SessionEventKind::TurnCompleted { .. } | SessionEventKind::Exited { .. }
        );
        events.push(event);
        if done {
            return events;
        }
    }
}

fn text_of(events: &[SessionEvent]) -> String {
    let mut text = String::new();
    for event in events {
        if let SessionEventKind::Output {
            stream: OutputStream::Text,
            text: chunk,
        } = &event.kind
        {
            text.push_str(chunk);
        }
    }
    text
}

#[tokio::test]
async fn open_prompt_events_and_shutdown() {
    let harness = start();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = harness.remote.open(spec(tx, None)).await.unwrap();
    assert_eq!(session.session_id(), "scripted-scripted");

    let outcome = session.prompt("hello").await.unwrap();
    assert_eq!(outcome.stop_reason, StopReason::EndTurn);

    let events = turn_events(&mut rx).await;
    assert_eq!(text_of(&events), "echo[scripted]: hello");
    assert!(events.iter().any(|event| matches!(
        event.kind,
        SessionEventKind::ToolCall {
            status: ToolStatus::Pending,
            ..
        }
    )));
    assert!(
        events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::Usage { used: 42, .. }))
    );

    session.shutdown().await;
    harness.stop();
}

/// The same flow, once directly through the driver and once through the node with the link
/// cut mid-turn: the events must be the same list, in the same order, with no repeats, and
/// the turn's `Return` must still arrive.
#[tokio::test]
async fn a_disconnect_mid_turn_keeps_every_event_and_the_return() {
    // Baseline: the driver called directly.
    let direct = ScriptedDriver::new("scripted", Duration::from_millis(40));
    let (tx, mut baseline_rx) = mpsc::unbounded_channel();
    let direct_session = direct.open(spec(tx, None)).await.unwrap();
    let direct_outcome = direct_session.prompt("hello").await.unwrap();
    let baseline = turn_events(&mut baseline_rx).await;
    direct_session.shutdown().await;

    // The same flow over the node, cutting the link after the turn started.
    let harness = start();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = Arc::new(harness.remote.open(spec(tx, None)).await.unwrap());
    let prompt = tokio::spawn({
        let session = session.clone();
        async move { session.prompt("hello").await }
    });
    // `open` reports `Started` first; wait for the turn itself, then cut the link.
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(60), rx.recv())
            .await
            .expect("the turn starts")
            .expect("the channel stays open");
        let started = matches!(event.kind, SessionEventKind::TurnStarted { .. });
        events.push(event);
        if started {
            break;
        }
    }
    harness.hub.disconnect().await;

    // A call made while the node is off the link fails at once.
    let offline = tokio::time::timeout(Duration::from_secs(5), session.prompt("later"))
        .await
        .expect("offline calls answer at once")
        .unwrap_err();
    assert!(offline.to_string().contains("node offline"), "{offline:?}");

    // The node keeps running while disconnected; reconnect and replay.
    tokio::time::sleep(Duration::from_millis(150)).await;
    harness.hub.set_online(true);
    events.extend(turn_events(&mut rx).await);

    let outcome = tokio::time::timeout(Duration::from_secs(60), prompt)
        .await
        .expect("the turn finishes after the reconnect")
        .unwrap()
        .unwrap();
    assert_eq!(outcome, direct_outcome);
    assert_eq!(events.len(), baseline.len(), "{events:?}\n{baseline:?}");
    for (event, expected) in events.iter().zip(&baseline) {
        assert_eq!(
            serde_json::to_string(event).unwrap(),
            serde_json::to_string(expected).unwrap()
        );
    }

    Arc::try_unwrap(session).ok().unwrap().shutdown().await;
    harness.stop();
}

struct RecordingHandler {
    seen: Mutex<Vec<PermissionRequest>>,
    decision: PermissionDecision,
}

#[async_trait]
impl PermissionHandler for RecordingHandler {
    async fn decide(&self, request: &PermissionRequest) -> PermissionDecision {
        self.seen.lock().unwrap().push(request.clone());
        self.decision
    }
}

/// `Ask` goes to the hub's handler and its `Answer` comes back to the driver.
#[tokio::test]
async fn permission_requests_go_to_the_hub_and_answers_come_back() {
    let handler = Arc::new(RecordingHandler {
        seen: Mutex::new(Vec::new()),
        decision: PermissionDecision::AllowForSession,
    });
    let harness = start();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = harness
        .remote
        .open(spec(tx, Some(handler.clone())))
        .await
        .unwrap();

    let outcome = session.prompt("do it").await.unwrap();
    assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    let events = turn_events(&mut rx).await;
    assert_eq!(
        text_of(&events),
        "permission=AllowForSession echo[scripted]: do it"
    );

    {
        let seen = handler.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].title, "cargo test");
    }

    session.shutdown().await;
    harness.stop();
}

/// Session calls travel as `Call`s, and a driver's refusal comes back as the same
/// `CoreError` variant rather than as a generic agent error.
#[tokio::test]
async fn session_calls_round_trip_and_errors_keep_their_variant() {
    let harness = start();
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = harness.remote.open(spec(tx, None)).await.unwrap();

    session.interrupt().await.unwrap();
    let unsupported = session.rollback("t1", false).await.unwrap_err();
    assert!(
        matches!(unsupported, CoreError::Unsupported(_)),
        "{unsupported:?}"
    );
    let model = session.set_model(Some("m"), None).await.unwrap_err();
    assert!(matches!(model, CoreError::Unsupported(_)), "{model:?}");
    let fork = session
        .fork(None, spec(mpsc::unbounded_channel().0, None))
        .await;
    let fork_err = fork.err().expect("the scripted driver has no fork");
    assert!(
        matches!(fork_err, CoreError::Unsupported(_)),
        "{fork_err:?}"
    );

    session.shutdown().await;
    harness.stop();
}

/// Non-session answers come straight from the local driver for a built-in node.
#[tokio::test]
async fn driver_info_matches_the_local_driver() {
    let driver: Arc<dyn Driver> = ScriptedDriver::new("scripted", Duration::ZERO);
    let local: Arc<dyn DriverInfo> = Arc::new(LocalDriverInfo::new(driver.clone()));
    assert_eq!(local.name(), "scripted");
    assert_eq!(local.kind(), DriverKind::AcpGeneric);
    assert_eq!(local.capabilities(), driver.capabilities());
    assert_eq!(local.models(), vec!["scripted-model".to_owned()]);
    assert!(local.installed());
    assert!(local.cli_status().await.is_none());
    assert!(local.cli_places().is_empty());
}

/// A node serves one `(plugin, driver)` pair per name; two of them on one node share the one
/// control stream, so their sessions must not evict each other or ack for each other.
struct TwoDrivers {
    a: Arc<dyn Driver>,
    b: Arc<dyn Driver>,
}

impl DriverSource for TwoDrivers {
    fn driver(&self, plugin: &str, _driver: Option<&str>) -> Result<Arc<dyn Driver>, CoreError> {
        match plugin {
            "a" => Ok(self.a.clone()),
            "b" => Ok(self.b.clone()),
            other => Err(CoreError::NotFound {
                kind: "plugin",
                id: other.to_owned(),
            }),
        }
    }
}

#[tokio::test]
async fn two_drivers_on_one_node_share_one_connection() {
    let a = ScriptedDriver::new("a", Duration::from_millis(40));
    let b = ScriptedDriver::new("b", Duration::from_millis(40));
    let source: Arc<dyn DriverSource> = Arc::new(TwoDrivers {
        a: a.clone(),
        b: b.clone(),
    });
    let (hub, node, service) = serve(source);
    let remote_a = node.driver("a", None, Arc::new(LocalDriverInfo::new(a.clone())));
    let remote_b = node.driver("b", None, Arc::new(LocalDriverInfo::new(b.clone())));

    let (tx_a, mut rx_a) = mpsc::unbounded_channel();
    let (tx_b, mut rx_b) = mpsc::unbounded_channel();
    let mut spec_a = spec(tx_a, None);
    spec_a.agent = agent("a");
    let mut spec_b = spec(tx_b, None);
    spec_b.agent = agent("b");
    let session_a = remote_a.open(spec_a).await.unwrap();
    let session_b = remote_b.open(spec_b).await.unwrap();
    // One node, one connection: a driver must not open its own.
    assert_eq!(hub.connections(), 1);

    let prompt_a = tokio::spawn(async move {
        let outcome = session_a.prompt("first").await;
        (session_a, outcome)
    });
    let prompt_b = tokio::spawn(async move {
        let outcome = session_b.prompt("second").await;
        (session_b, outcome)
    });

    // Both turns are running; cut the link under them and let it come back.
    let mut started = Vec::new();
    for rx in [&mut rx_a, &mut rx_b] {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(60), rx.recv())
                .await
                .expect("the turn starts")
                .expect("the channel stays open");
            if matches!(event.kind, SessionEventKind::TurnStarted { .. }) {
                break;
            }
        }
        started.push(());
    }
    hub.disconnect().await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    hub.set_online(true);

    let (session_a, outcome_a) = tokio::time::timeout(Duration::from_secs(60), prompt_a)
        .await
        .expect("a's turn finishes after the reconnect")
        .unwrap();
    let (session_b, outcome_b) = tokio::time::timeout(Duration::from_secs(60), prompt_b)
        .await
        .expect("b's turn finishes after the reconnect")
        .unwrap();
    assert_eq!(outcome_a.unwrap().stop_reason, StopReason::EndTurn);
    assert_eq!(outcome_b.unwrap().stop_reason, StopReason::EndTurn);

    let events_a = turn_events(&mut rx_a).await;
    let events_b = turn_events(&mut rx_b).await;
    // Each session saw its own events once, and none of the other's.
    assert_eq!(text_of(&events_a), "echo[a]: first", "{events_a:?}");
    assert_eq!(text_of(&events_b), "echo[b]: second", "{events_b:?}");
    let completed = |events: &[SessionEvent]| {
        events
            .iter()
            .filter(|event| matches!(event.kind, SessionEventKind::TurnCompleted { .. }))
            .count()
    };
    assert_eq!(completed(&events_a), 1);
    assert_eq!(completed(&events_b), 1);

    // One connection for the first life, one for the reconnect — and no churn after it.
    assert_eq!(
        hub.connections(),
        2,
        "the node was connected to more than once"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(hub.connections(), 2, "the connection kept being replaced");

    session_a.shutdown().await;
    session_b.shutdown().await;
    service.abort();
}

/// A driver that emits its events and returns from `prompt` at once must not let the turn's
/// `Return` overtake them: the hub sees every event before `prompt` answers, as it does with
/// a direct driver.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turns_return_waits_for_its_events() {
    let harness = harness_for(ScriptedDriver::new("scripted", Duration::ZERO));
    for round in 0..200 {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let session = harness.remote.open(spec(tx, None)).await.unwrap();
        session.prompt("fast").await.unwrap();
        let mut seen = Vec::new();
        while let Ok(event) = rx.try_recv() {
            seen.push(event);
        }
        assert_eq!(
            seen.len(),
            7,
            "round {round}: `Started` and the turn's six events must be there when \
             `prompt` answers: {seen:?}"
        );
        assert!(
            matches!(
                seen.last().map(|event| &event.kind),
                Some(SessionEventKind::TurnCompleted { .. })
            ),
            "round {round}: {seen:?}"
        );
        session.shutdown().await;
    }
    harness.stop();
}

/// Dropping a session ends it on the node, as dropping a local driver's session ends the
/// agent process.
#[tokio::test]
async fn dropping_a_session_shuts_it_down_on_the_node() {
    let driver = ScriptedDriver::new("scripted", Duration::ZERO);
    let shutdowns = driver.shutdowns.clone();
    let harness = harness_for(driver);
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = harness.remote.open(spec(tx, None)).await.unwrap();
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);

    drop(session);

    let mut waited = Duration::ZERO;
    while shutdowns.load(Ordering::SeqCst) == 0 && waited < Duration::from_secs(30) {
        tokio::time::sleep(Duration::from_millis(10)).await;
        waited += Duration::from_millis(10);
    }
    assert_eq!(
        shutdowns.load(Ordering::SeqCst),
        1,
        "the node was told to shut the session down"
    );
    harness.stop();
}

/// A restarted node numbers its frames from 1 again; the hub must not take them for replays
/// of the run before.
#[tokio::test]
async fn a_restarted_node_starts_its_sequence_over() {
    let driver: Arc<dyn Driver> = ScriptedDriver::new("scripted", Duration::ZERO);
    let source: Arc<dyn DriverSource> = Arc::new(SingleDriverSource {
        plugin: "scripted".into(),
        driver: driver.clone(),
    });
    let (hub, node) = memory_pair();
    let server: Arc<dyn TransportServer> = Arc::new(node);
    let first_service = tokio::spawn({
        let server = server.clone();
        let source = source.clone();
        async move { NodeService::new(source).run(server).await }
    });
    let node_link = Arc::new(RemoteNode::new(
        Arc::new(hub.clone()) as Arc<dyn crossagents_node::HubLink>
    ));
    let remote = node_link.driver("scripted", None, Arc::new(LocalDriverInfo::new(driver)));

    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = remote.open(spec(tx, None)).await.unwrap();
    session.prompt("before").await.unwrap();
    let before = turn_events(&mut rx).await;
    assert_eq!(text_of(&before), "echo[scripted]: before");
    session.shutdown().await;

    // The node restarts on the same link: a new `NodeService`, so a new epoch and seq 1.
    first_service.abort();
    let _ = first_service.await;
    let second_service = tokio::spawn(async move {
        NodeService::new(source).run(server).await;
    });

    // The hub reconnects by itself; opening waits for the connection.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = remote.open(spec(tx, None)).await.unwrap();
    session.prompt("after").await.unwrap();
    let after = turn_events(&mut rx).await;
    assert_eq!(
        text_of(&after),
        "echo[scripted]: after",
        "the restarted node's frames were dropped as replays"
    );

    session.shutdown().await;
    second_service.abort();
}
