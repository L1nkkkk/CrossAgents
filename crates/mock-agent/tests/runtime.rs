//! Agent sessions through the hub's API, against the mock agent.

mod common;

use std::time::Duration;

use common::{TestHub, types};
use crossagents_api_types::{AgentStatus, EventKind, StopReason};
use serde_json::{Value, json};

fn turn_completed(e: &crossagents_api_types::Event) -> bool {
    matches!(e.kind, EventKind::AgentTurnCompleted { .. })
}

#[tokio::test]
async fn send_starts_agent_and_streams_answer() {
    let t = TestHub::start().await;
    t.add_agent("alice", Some("haiku")).await;
    let mut w = t.watch().await;

    let sent = t.send("alice", "hello there").await;
    assert_eq!(sent["queued"], false);

    let done = w.until("turn completed", turn_completed).await;
    let EventKind::AgentTurnCompleted {
        stop_reason,
        message_id,
        ..
    } = done.kind
    else {
        unreachable!()
    };
    assert_eq!(stop_reason, StopReason::EndTurn);
    assert_eq!(message_id.to_string(), sent["message_id"].as_str().unwrap());
    assert_eq!(
        w.output(),
        "echo(model=mock-haiku,mode=bypassPermissions): hello there"
    );

    // Order: status starting -> idle, input, running, turn started, output..., completed.
    let t_ = types(&w.seen);
    let pos = |name: &str| t_.iter().position(|t| *t == name).unwrap();
    assert!(pos("agent.input") < pos("agent.turn_started"));
    assert!(pos("agent.turn_started") < pos("agent.output"));
    assert!(t_.contains(&"agent.usage"));

    w.until("idle", |e| {
        matches!(
            e.kind,
            EventKind::AgentStatusChanged {
                to: AgentStatus::Idle,
                ..
            }
        )
    })
    .await;
    let agent: Value = t.get("/v1/agents/alice").await.json().await.unwrap();
    assert_eq!(agent["status"], "idle");
}

#[tokio::test]
async fn messages_queue_behind_a_running_turn() {
    let t = TestHub::start().await;
    t.add_agent("bob", None).await;
    let mut w = t.watch().await;

    let first = t.send("bob", "[slow] first").await;
    w.until("first turn started", |e| {
        matches!(e.kind, EventKind::AgentTurnStarted { .. })
    })
    .await;
    let second = t.send("bob", "second").await;
    assert_eq!(first["queued"], false);
    assert_eq!(second["queued"], true);

    let resp = t.post("/v1/agents/bob/cancel", json!({})).await;
    assert_eq!(resp.status(), 204);
    let done = w.until("first completed", turn_completed).await;
    assert!(matches!(
        done.kind,
        EventKind::AgentTurnCompleted {
            stop_reason: StopReason::Cancelled,
            ..
        }
    ));
    w.until("second completed", turn_completed).await;
    assert!(w.output().ends_with("second"), "{}", w.output());
}

#[tokio::test]
async fn crash_marks_agent_error_and_restart_works() {
    let t = TestHub::start().await;
    t.add_agent("carol", None).await;
    let mut w = t.watch().await;
    t.send("carol", "[crash]").await;
    w.until("error status", |e| {
        matches!(
            e.kind,
            EventKind::AgentStatusChanged {
                to: AgentStatus::Error,
                ..
            }
        )
    })
    .await;

    // Sending again starts a fresh session.
    t.send("carol", "back").await;
    w.until("turn completed", |e| {
        matches!(
            e.kind,
            EventKind::AgentTurnCompleted {
                stop_reason: StopReason::EndTurn,
                ..
            }
        )
    })
    .await;
    assert!(w.output().ends_with("back"));
}

#[tokio::test]
async fn stop_and_delete() {
    let t = TestHub::start().await;
    t.add_agent("dave", None).await;
    let resp = t.post("/v1/agents/dave/start", json!({})).await;
    assert_eq!(resp.status(), 200);
    let agent: Value = resp.json().await.unwrap();
    assert_eq!(agent["status"], "idle");

    assert_eq!(
        t.post("/v1/agents/dave/stop", json!({})).await.status(),
        204
    );
    let agent: Value = t.get("/v1/agents/dave").await.json().await.unwrap();
    assert_eq!(agent["status"], "stopped");
    // Cancel needs a running session.
    assert_eq!(
        t.post("/v1/agents/dave/cancel", json!({})).await.status(),
        409
    );

    t.post("/v1/agents/dave/start", json!({})).await;
    let resp = t
        .client
        .delete(format!("{}/v1/agents/dave", t.base))
        .bearer_auth(&t.hub.token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
}

#[tokio::test]
async fn unknown_plugin_is_rejected_and_plugins_are_listed() {
    let t = TestHub::start().await;
    let resp = t
        .post(
            "/v1/agents",
            json!({ "name": "x", "plugin": "nope", "role": "worker", "cwd": t.home.path().display().to_string() }),
        )
        .await;
    assert_eq!(resp.status(), 400);
    let plugins: Value = t.get("/v1/plugins").await.json().await.unwrap();
    let names: Vec<&str> = plugins["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["claude", "codex", "mock"]);
}

#[tokio::test]
async fn failing_launch_reports_error() {
    let t = TestHub::start().await;
    let resp = t
        .post(
            "/v1/agents",
            json!({
                "name": "eve", "plugin": "mock", "role": "worker",
                "cwd": t.home.path().display().to_string(),
                "config": { "command": ["definitely-not-a-real-program-xyz"] }
            }),
        )
        .await;
    assert_eq!(resp.status(), 201);
    let resp = t.post("/v1/agents/eve/start", json!({})).await;
    assert_eq!(resp.status(), 502);
    let agent: Value = t.get("/v1/agents/eve").await.json().await.unwrap();
    assert_eq!(agent["status"], "error");
}

#[tokio::test]
async fn hub_shutdown_stops_sessions() {
    let t = TestHub::start().await;
    t.add_agent("frank", None).await;
    t.post("/v1/agents/frank/start", json!({})).await;
    t.hub.shutdown();
    tokio::time::timeout(Duration::from_secs(10), t.hub.wait())
        .await
        .expect("hub did not stop")
        .unwrap();
}
