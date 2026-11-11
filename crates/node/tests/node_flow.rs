//! The node and the remote driver over the in-memory link (docs/tasks/B1-NODE.md §8, B1.1).
//!
//! The agent behind them is a scripted [`Driver`] that plays the event sequence of the
//! workspace's mock agent (turn start, a tool call, output, usage, turn end) and takes
//! [`PermissionHandler`] requests when the spec gives it one. It is scripted here rather than
//! spawned through `crates/mock-agent` because the ACP driver that launches that binary
//! depends on `crossagents-drivers`, and `cargo clippy -p crossagents-node` lints every
//! workspace path dependency: that would pull drivers' existing warnings into the node's
//! `-D warnings` check. The mock agent's own tests keep covering the ACP driver; these
//! tests cover what the node owns — routing calls, numbering and replaying events, asking
//! the hub, and failing calls while the link is down.

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
    DriverInfo, LocalDriverInfo, MemoryHub, NodeService, RemoteDriver, SingleDriverSource,
    memory_pair,
};
use tokio::sync::mpsc;

fn agent() -> Agent {
    Agent {
        id: AgentId::new(),
        name: "mocky".into(),
        name_source: NameSource::Chosen,
        plugin: "scripted".into(),
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
        agent: agent(),
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
struct ScriptedDriver;

#[async_trait]
impl Driver for ScriptedDriver {
    fn name(&self) -> &str {
        "scripted"
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
                session_id: "scripted-session".into(),
            }));
        Ok(Box::new(ScriptedSession {
            events: spec.events,
            permissions: spec.permissions,
        }))
    }
}

struct ScriptedSession {
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
        "scripted-session"
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
        tokio::time::sleep(Duration::from_millis(40)).await;
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
            text: format!("echo: {text}"),
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

    async fn shutdown(&self) {}
}

/// A node serving the scripted driver, and the hub's remote driver over it.
struct Harness {
    hub: MemoryHub,
    remote: RemoteDriver,
    service: tokio::task::JoinHandle<()>,
}

fn start() -> Harness {
    let (hub, node) = memory_pair();
    let driver: Arc<dyn Driver> = Arc::new(ScriptedDriver);
    let source = SingleDriverSource {
        plugin: "scripted".into(),
        driver: driver.clone(),
    };
    let service = tokio::spawn(async move {
        NodeService::new(Arc::new(source)).run(Arc::new(node)).await;
    });
    let info: Arc<dyn DriverInfo> = Arc::new(LocalDriverInfo::new(driver));
    let remote = RemoteDriver::new("scripted", None, info, Arc::new(hub.clone()));
    Harness {
        hub,
        remote,
        service,
    }
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
    assert_eq!(session.session_id(), "scripted-session");

    let outcome = session.prompt("hello").await.unwrap();
    assert_eq!(outcome.stop_reason, StopReason::EndTurn);

    let events = turn_events(&mut rx).await;
    assert_eq!(text_of(&events), "echo: hello");
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
    let direct = ScriptedDriver;
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

    session.shutdown().await;
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
    assert_eq!(text_of(&events), "permission=AllowForSession echo: do it");

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
    let driver: Arc<dyn Driver> = Arc::new(ScriptedDriver);
    let local: Arc<dyn DriverInfo> = Arc::new(LocalDriverInfo::new(driver.clone()));
    assert_eq!(local.name(), "scripted");
    assert_eq!(local.kind(), DriverKind::AcpGeneric);
    assert_eq!(local.capabilities(), driver.capabilities());
    assert_eq!(local.models(), vec!["scripted-model".to_owned()]);
    assert!(local.installed());
    assert!(local.cli_status().await.is_none());
    assert!(local.cli_places().is_empty());
}
