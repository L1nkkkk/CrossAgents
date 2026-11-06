//! The hub's one answer for what a session does with messages: idle, a running turn,
//! messages queued behind it, or messages held for an external agent — in the transcript
//! snapshot and in `agent.delivery_changed`, saying the same thing
//! (docs/tasks/DELIVERY-AUDIT.md §8 P1-4).

mod common;

use common::{TestHub, Watcher};
use crossagents_api_types::{AgentId, DeliveryStatus, EventKind};
use serde_json::{Value, json};

/// The transcript's `delivery`: what a client shows as "busy" and "next".
async fn snapshot(t: &TestHub, key: &str) -> Value {
    let resp = t.get(&format!("/v1/agents/{key}/transcript")).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let body: Value = resp.json().await.unwrap();
    body["delivery"].clone()
}

fn assert_snapshot(delivery: &Value, state: &str, queued: u64) {
    assert_eq!(delivery["state"], state, "{delivery}");
    assert_eq!(delivery["queued"], queued, "{delivery}");
}

/// Waits for the `agent.delivery_changed` event that says exactly what the snapshot says,
/// and returns it: the two must agree, so the GUI need not work anything out itself.
async fn event_agrees(w: &mut Watcher, id: AgentId, snapshot: &Value) -> DeliveryStatus {
    let event = w
        .find("delivery changed to what the snapshot says", |e| {
            matches!(
                &e.kind,
                EventKind::AgentDeliveryChanged { id: agent, delivery }
                    if *agent == id
                        && serde_json::to_value(delivery).ok().as_ref() == Some(snapshot)
            )
        })
        .await;
    let EventKind::AgentDeliveryChanged { delivery, .. } = &event.kind else {
        unreachable!()
    };
    *delivery
}

async fn output_with(w: &mut Watcher, needle: &str) {
    w.until(
        needle,
        |e| matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains(needle)),
    )
    .await;
}

async fn turn_completed(w: &mut Watcher) {
    w.until("turn completed", |e| {
        matches!(&e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;
}

/// Nothing runs, nothing waits: idle, both at rest and again once a turn ends.
#[tokio::test]
async fn an_idle_session_says_idle() {
    let t = TestHub::start().await;
    let agent = t.add_agent("Vera", None).await;
    let mut w = t.watch().await;

    let delivery = snapshot(&t, &agent.id.to_string()).await;
    assert_snapshot(&delivery, "idle", 0);

    t.send(&agent.id.to_string(), "hello").await;
    turn_completed(&mut w).await;

    let delivery = snapshot(&t, &agent.id.to_string()).await;
    assert_snapshot(&delivery, "idle", 0);
    event_agrees(&mut w, agent.id, &delivery).await;
}

/// A turn runs: the state is `turn`, and no message is behind it yet.
#[tokio::test]
async fn a_running_turn_says_turn() {
    let t = TestHub::start().await;
    let agent = t.add_agent("Vera", None).await;
    let mut w = t.watch().await;

    t.send(&agent.id.to_string(), "first [slow]").await;
    output_with(&mut w, "working").await;

    let delivery = snapshot(&t, &agent.id.to_string()).await;
    assert_snapshot(&delivery, "turn", 0);
    event_agrees(&mut w, agent.id, &delivery).await;
}

/// Messages behind a running turn are counted, and the snapshot says so; once they have all
/// run the session is idle again.
#[tokio::test]
async fn messages_behind_a_turn_are_counted() {
    let t = TestHub::start().await;
    let agent = t.add_agent("Vera", None).await;
    let mut w = t.watch().await;

    t.send(&agent.id.to_string(), "first [slow]").await;
    output_with(&mut w, "working").await;

    let second = t.send(&agent.id.to_string(), "SECOND PEAR").await;
    assert_eq!(second["queued"], true, "{second}");

    let delivery = snapshot(&t, &agent.id.to_string()).await;
    assert_snapshot(&delivery, "turn", 1);
    event_agrees(&mut w, agent.id, &delivery).await;

    // The queued message gets its turn once the slow one is cancelled.
    let resp = t
        .post(&format!("/v1/agents/{}/cancel", agent.id), json!({}))
        .await;
    assert_eq!(resp.status(), 204, "{}", resp.text().await.unwrap());
    output_with(&mut w, "SECOND PEAR").await;
    turn_completed(&mut w).await;

    let delivery = snapshot(&t, &agent.id.to_string()).await;
    assert_snapshot(&delivery, "idle", 0);
    event_agrees(&mut w, agent.id, &delivery).await;
}

/// An agent outside the hub runs no turn here: its messages are held until it fetches them,
/// and it says so as `held` with the count.
#[tokio::test]
async fn messages_held_for_an_external_agent_say_held() {
    let t = TestHub::start().await;
    let resp = t
        .post(
            "/v1/agents",
            json!({ "name": "outside", "plugin": "external", "role": "worker",
                    "cwd": t.home.path().display().to_string(),
                    "profile": { "display_name": "Ext" } }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let token = t
        .post("/v1/agents/outside/connect", json!({}))
        .await
        .json::<Value>()
        .await
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();

    let idle = snapshot(&t, "outside").await;
    assert_snapshot(&idle, "idle", 0);
    let outside = t.agent("outside").await;

    let mut w = t.watch().await;
    t.send("outside", "HELLO FROM INSIDE").await;

    let delivery = snapshot(&t, "outside").await;
    assert_snapshot(&delivery, "held", 1);
    event_agrees(&mut w, outside.id, &delivery).await;

    // It asks for them: nothing is held any more.
    let inbox = t
        .client
        .post(format!("{}/v1/me/tools/inbox_read", t.base))
        .bearer_auth(&token)
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(
        serde_json::to_string(&inbox)
            .unwrap()
            .contains("HELLO FROM INSIDE"),
        "{inbox}"
    );

    let delivery = snapshot(&t, "outside").await;
    assert_snapshot(&delivery, "idle", 0);
    event_agrees(&mut w, outside.id, &delivery).await;
}
