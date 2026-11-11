//! A real driver over the node (docs/tasks/B1-NODE.md §8, B1.1): the ACP driver that launches
//! `crossagents-mock-agent`, a [`NodeService`] behind an in-memory link, and the hub's
//! [`RemoteDriver`]. What the scripted driver in `node_flow.rs` cannot show is the wire shape
//! of a real driver's events (`SessionEvent.raw` carries the vendor JSON), so this test runs
//! the same turn directly and through the node and compares the two event streams.
//!
//! The mock agent binary is built beside the test binaries, as `crates/cli/tests/smoke.rs`
//! looks for it: `cargo build -p crossagents-mock-agent` first. Without it the tests skip with
//! that message rather than failing, so `cargo test -p crossagents-node` works on its own.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crossagents_core::{
    Agent, AgentId, AgentStatus, Driver, NameSource, OutputStream, PermissionPolicy, Role,
    SessionEvent, SessionEventKind, SessionSpec, StopReason,
};
use crossagents_drivers::acp::AcpDriver;
use crossagents_node::{
    DriverSource, HubLink, LocalDriverInfo, MemoryHub, NodeService, RemoteDriver, RemoteNode,
    SingleDriverSource, TransportServer, memory_pair,
};
use tokio::sync::mpsc;

/// The mock agent built beside this test's binary (`target/debug/`).
fn mock_agent() -> Option<PathBuf> {
    let exe = std::env::current_exe().expect("the test binary has a path");
    let path = exe.parent().and_then(Path::parent).map(|dir| {
        dir.join(format!(
            "crossagents-mock-agent{}",
            std::env::consts::EXE_SUFFIX
        ))
    })?;
    path.exists().then_some(path)
}

fn spec(
    cwd: &Path,
    command: Vec<String>,
    events: mpsc::UnboundedSender<SessionEvent>,
) -> SessionSpec {
    SessionSpec {
        agent: Agent {
            id: AgentId::new(),
            name: "mocky".into(),
            name_source: NameSource::Chosen,
            topic: None,
            archived_at: None,
            home: cwd.to_path_buf(),
            plugin: "mock".into(),
            driver: Some("acp-generic".into()),
            model: None,
            role: Role::Worker,
            cwd: cwd.to_path_buf(),
            config: serde_json::Value::Null,
            status: AgentStatus::Idle,
            created_at: chrono::Utc::now(),
            profile: Default::default(),
        },
        mcp_servers: vec![],
        permission: PermissionPolicy::Bypass,
        permissions: None,
        command: Some(command),
        instructions: None,
        env: vec![],
        events,
    }
}

/// A node running the ACP driver, and the hub's driver for it over one in-memory link.
struct Harness {
    hub: MemoryHub,
    remote: RemoteDriver,
    service: tokio::task::JoinHandle<()>,
}

fn harness(mock: &Path) -> Harness {
    let driver: Arc<dyn Driver> = Arc::new(AcpDriver::new(
        "mock",
        vec![mock.display().to_string()],
        vec![],
        None,
    ));
    let source: Arc<dyn DriverSource> = Arc::new(SingleDriverSource {
        plugin: "mock".into(),
        driver: driver.clone(),
    });
    let (hub, node) = memory_pair();
    let server: Arc<dyn TransportServer> = Arc::new(node);
    let service = tokio::spawn(async move {
        NodeService::new(source).run(server).await;
    });
    let node = Arc::new(RemoteNode::new(Arc::new(hub.clone()) as Arc<dyn HubLink>));
    let info: Arc<dyn crossagents_node::DriverInfo> = Arc::new(LocalDriverInfo::new(driver));
    let remote = node.driver("mock", None, info);
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

/// The mock agent's command line, as a session spec carries it.
fn command(mock: &Path) -> Vec<String> {
    vec![mock.display().to_string()]
}

/// Collects events up to and including the turn's `TurnCompleted`.
async fn turn_events(rx: &mut mpsc::UnboundedReceiver<SessionEvent>) -> Vec<SessionEvent> {
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(120), rx.recv())
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

/// The events as JSON, with the session's own id replaced: the direct run and the node run
/// are separate agent processes and so have different ids.
fn normalized(events: &[SessionEvent], session_id: &str) -> Vec<String> {
    events
        .iter()
        .map(|event| {
            serde_json::to_string(event)
                .expect("a session event is JSON")
                .replace(session_id, "<session>")
        })
        .collect()
}

/// open → prompt → the driver's events → shutdown, once directly and once through the node:
/// the hub sees exactly the events the driver emitted, vendor `raw` payloads included.
#[tokio::test]
async fn open_prompt_events_and_shutdown_through_the_mock_agent() {
    let Some(mock) = mock_agent() else {
        eprintln!("skipped: build the mock agent first: cargo build -p crossagents-mock-agent");
        return;
    };
    let cwd = std::env::temp_dir();

    // Baseline: the driver called directly.
    let (direct_tx, mut direct_rx) = mpsc::unbounded_channel();
    let direct = AcpDriver::new("mock", command(&mock), vec![], None);
    let direct_session = direct
        .open(spec(&cwd, command(&mock), direct_tx))
        .await
        .expect("the mock agent starts");
    let direct_id = direct_session.session_id().to_owned();
    let direct_outcome = direct_session.prompt("hello").await.expect("the turn runs");
    let direct_events = turn_events(&mut direct_rx).await;
    direct_session.shutdown().await;

    // Through the node and the remote driver.
    let harness = harness(&mock);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = harness
        .remote
        .open(spec(&cwd, command(&mock), tx))
        .await
        .expect("the node opens the session");
    let node_id = session.session_id().to_owned();
    let outcome = session.prompt("hello").await.expect("the turn runs");
    let events = turn_events(&mut rx).await;
    session.shutdown().await;
    harness.stop();

    assert_eq!(outcome, direct_outcome);
    assert!(!direct_events.is_empty(), "the mock agent emitted nothing");
    assert_eq!(
        text_of(&events),
        text_of(&direct_events),
        "the hub saw different text"
    );
    assert_eq!(
        normalized(&events, &node_id),
        normalized(&direct_events, &direct_id),
        "the node changed the driver's events"
    );
}

/// Cutting the link after a turn starts must not lose or repeat its events, and the turn's
/// `Return` still reaches the hub once the link is back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disconnect_mid_turn_keeps_the_mock_agents_events() {
    let Some(mock) = mock_agent() else {
        eprintln!("skipped: build the mock agent first: cargo build -p crossagents-mock-agent");
        return;
    };
    let harness = harness(&mock);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = Arc::new(
        harness
            .remote
            .open(spec(&std::env::temp_dir(), command(&mock), tx))
            .await
            .expect("the node opens the session"),
    );
    let prompt = tokio::spawn({
        let session = session.clone();
        async move { session.prompt("[slow] wait for me").await }
    });

    // The turn has started on the node; cut the link while the agent is paused in it.
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(120), rx.recv())
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
    tokio::time::sleep(Duration::from_millis(150)).await;
    harness.hub.set_online(true);

    // The session is still live on the node: cancelling it ends the paused turn, and its
    // `Return` crosses the reconnected link.
    let mut waited = Duration::ZERO;
    loop {
        match session.interrupt().await {
            Ok(()) => break,
            Err(error) if waited < Duration::from_secs(30) => {
                assert!(error.to_string().contains("node offline"), "{error:?}");
                tokio::time::sleep(Duration::from_millis(20)).await;
                waited += Duration::from_millis(20);
            }
            Err(error) => panic!("the link did not come back: {error:?}"),
        }
    }
    let outcome = tokio::time::timeout(Duration::from_secs(120), prompt)
        .await
        .expect("the turn finishes after the reconnect")
        .expect("the prompt task does not panic")
        .expect("the turn's return arrives after the reconnect");
    assert_eq!(outcome.stop_reason, StopReason::Cancelled);

    events.extend(turn_events(&mut rx).await);
    assert_eq!(text_of(&events), "working", "{events:?}");
    let count = |kind: fn(&SessionEventKind) -> bool| {
        events.iter().filter(|event| kind(&event.kind)).count()
    };
    assert_eq!(
        count(|kind| matches!(kind, SessionEventKind::Started { .. })),
        1,
        "the session started twice: {events:?}"
    );
    assert_eq!(
        count(|kind| matches!(kind, SessionEventKind::TurnStarted { .. })),
        1,
        "the turn started twice: {events:?}"
    );
    assert_eq!(
        count(|kind| matches!(kind, SessionEventKind::TurnCompleted { .. })),
        1,
        "the turn completed twice or not at all: {events:?}"
    );

    Arc::try_unwrap(session).ok().unwrap().shutdown().await;
    harness.stop();
}
