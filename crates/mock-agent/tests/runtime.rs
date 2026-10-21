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
async fn queued_messages_can_be_withdrawn_or_sent_now() {
    let t = TestHub::start().await;
    t.add_agent("erin", None).await;
    let mut w = t.watch().await;

    t.send("erin", "[slow] first").await;
    w.until("first turn started", |e| {
        matches!(e.kind, EventKind::AgentTurnStarted { .. })
    })
    .await;
    let second = t.send("erin", "second").await;
    t.send("erin", "third").await;

    // A waiting message can be taken back, once.
    let path = format!(
        "/v1/agents/erin/messages/{}",
        second["message_id"].as_str().unwrap()
    );
    assert_eq!(t.delete(&path).await.status(), 204);
    assert_eq!(t.delete(&path).await.status(), 409);

    // `now` interrupts the running turn and goes ahead of "third".
    let resp = t
        .post(
            "/v1/agents/erin/messages",
            json!({ "text": "urgent", "now": true }),
        )
        .await;
    assert_eq!(resp.status(), 202);
    let first = w.until("first completed", turn_completed).await;
    assert!(matches!(
        first.kind,
        EventKind::AgentTurnCompleted {
            stop_reason: StopReason::Cancelled,
            ..
        }
    ));
    w.until("urgent completed", turn_completed).await;
    w.until("third completed", turn_completed).await;
    let out = w.output();
    let (urgent, third) = (out.find("urgent").unwrap(), out.find("third").unwrap());
    assert!(urgent < third, "{out}");
    assert!(!out.contains("second"), "{out}");

    let transcript: Value = t
        .get("/v1/agents/erin/transcript")
        .await
        .json()
        .await
        .unwrap();
    let inputs: Vec<&str> = transcript["turns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|turn| turn["input"].as_str().unwrap())
        .collect();
    assert_eq!(inputs, ["[slow] first", "third", "urgent"]);
    assert_eq!(transcript["turns"][2]["from"]["kind"], "human");
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
async fn model_can_be_changed_while_running() {
    let t = TestHub::start().await;
    t.add_agent("gail", Some("mock-opus")).await;
    t.post("/v1/agents/gail/start", json!({})).await;
    let resp = t
        .post("/v1/agents/gail/model", json!({ "model": "mock-haiku" }))
        .await;
    assert_eq!(resp.status(), 200);
    let agent: Value = t.get("/v1/agents/gail").await.json().await.unwrap();
    assert_eq!(agent["model"], "mock-haiku");
    assert_eq!(agent["status"], "idle");
    // A blank model goes back to the agent's own default.
    t.post("/v1/agents/gail/model", json!({ "model": " " }))
        .await;
    let agent: Value = t.get("/v1/agents/gail").await.json().await.unwrap();
    assert_eq!(agent["model"], Value::Null);
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

#[tokio::test]
async fn transcript_groups_the_conversation_by_turn() {
    let t = TestHub::start().await;
    t.add_agent("dave", Some("haiku")).await;
    t.add_agent("erin", None).await;
    let mut w = t.watch().await;
    let sent = t.send("dave", "first").await;
    w.until("turn completed", turn_completed).await;
    t.send("erin", "not dave's").await;
    w.until("erin's turn completed", turn_completed).await;

    let transcript: Value = t
        .get("/v1/agents/dave/transcript")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(transcript["agent"]["name"], "dave");
    assert_eq!(transcript["driver"], "acp-generic");
    assert_eq!(transcript["capabilities"]["streaming"], true);
    let turns = transcript["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 1, "{transcript}");
    assert_eq!(turns[0]["message_id"], sent["message_id"]);
    assert_eq!(turns[0]["input"], "first");
    assert_eq!(turns[0]["stop_reason"], "end_turn");
    let text: String = turns[0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["type"] == "output" && i["stream"] == "text")
        .map(|i| i["text"].as_str().unwrap())
        .collect();
    assert_eq!(text, "echo(model=mock-haiku,mode=bypassPermissions): first");
    assert!(transcript["usage"]["size"].as_u64().is_some());
    let head = w.seen.iter().map(|e| e.seq).max().unwrap();
    assert!(transcript["last_seq"].as_u64().unwrap() >= head);

    let resp = t.get("/v1/agents/nobody/transcript").await;
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn events_can_be_filtered_by_agent() {
    let t = TestHub::start().await;
    let frank = t.add_agent("frank", None).await;
    t.add_agent("gina", None).await;
    let mut w = t.watch().await;
    t.send("frank", "a").await;
    w.until("turn completed", turn_completed).await;
    t.send("gina", "b").await;
    w.until("gina's turn completed", turn_completed).await;

    let body = t
        .get("/v1/events?agent=frank&follow=false")
        .await
        .text()
        .await
        .unwrap();
    let events: Vec<crossagents_api_types::Event> = body
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(|d| serde_json::from_str(d.trim()).unwrap())
        .collect();
    assert!(events.iter().any(turn_completed));
    assert!(events.iter().all(|e| e.kind.agent_id() == Some(frank.id)));

    let resp = t.get("/v1/events?agent=nobody&follow=false").await;
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn profile_is_stored_updated_and_given_to_the_agent() {
    let t = TestHub::start().await;
    let cwd = t.home.path().display().to_string();
    let resp = t
        .post(
            "/v1/agents",
            json!({
                "name": "ada", "plugin": "mock", "role": "worker", "cwd": cwd,
                "profile": { "display_name": " Ada ", "title": "设计师", "avatar": "🎨", "color": "rose",
                             "persona": "You care about whitespace." },
            }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let ada: Value = resp.json().await.unwrap();
    assert_eq!(ada["profile"]["display_name"], "Ada", "trimmed");
    assert_eq!(ada["profile"]["title"], "设计师");

    // The persona reaches the agent as opening instructions; a bare agent gets none.
    t.add_agent("plain", None).await;
    let mut w = t.watch().await;
    t.send("ada", "hi").await;
    w.until("ada's turn", turn_completed).await;
    t.send("plain", "hi").await;
    w.until("plain's turn", turn_completed).await;
    let ada_id = ada["id"].as_str().unwrap();
    let ada_text: String = w
        .seen
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::AgentOutput { id, text, .. } if id.to_string() == ada_id => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect();
    assert!(
        ada_text.starts_with("[instructions received]"),
        "{ada_text}"
    );
    let plain: Value = t
        .get("/v1/agents/plain/transcript")
        .await
        .json()
        .await
        .unwrap();
    assert!(
        !plain.to_string().contains("instructions received"),
        "{plain}"
    );

    // Updating replaces the card and announces it.
    let resp = t
        .post(
            "/v1/agents/ada/profile",
            json!({ "title": "Security reviewer", "color": "teal" }),
        )
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let updated = w
        .until("agent.updated", |e| {
            matches!(e.kind, EventKind::AgentUpdated { .. })
        })
        .await;
    let EventKind::AgentUpdated { agent } = updated.kind else {
        unreachable!()
    };
    assert_eq!(agent.profile.title.as_deref(), Some("Security reviewer"));
    assert_eq!(agent.profile.display_name, None, "replaced, not merged");
    let stored: Value = t.get("/v1/agents/ada").await.json().await.unwrap();
    assert_eq!(stored["profile"]["color"], "teal");

    let resp = t
        .post("/v1/agents/ada/profile", json!({ "color": "neon" }))
        .await;
    assert_eq!(resp.status(), 400);
    let resp = t.post("/v1/agents/nobody/profile", json!({})).await;
    assert_eq!(resp.status(), 404);
}
