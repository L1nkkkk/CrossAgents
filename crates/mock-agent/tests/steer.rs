//! Steering a message into a running turn: the hub hands it to the driver when it can,
//! and queues it as before when it cannot.

mod common;

use common::{TestHub, Watcher, native_plugin_with};
use crossagents_api_types::{EventKind, MessageId};
use serde_json::{Value, json};

/// Waits for an `agent.output` whose text contains `needle`.
async fn output_with(w: &mut Watcher, needle: &str) {
    w.until(
        needle,
        |e| matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains(needle)),
    )
    .await;
}

/// A steered message goes into the running turn: `agent.steered` right away, the model
/// reads it at the next boundary (`agent.steer_read`), and the transcript shows it in that
/// turn.
#[tokio::test]
async fn a_message_steers_into_the_running_turn() {
    let t = TestHub::start_with(native_plugin_with(&["--steer-merge"])).await;
    let agent = t.add_native("Nova", "worker", t.home.path()).await;
    let mut w = t.watch().await;

    t.send(&agent.id.to_string(), "first [sleep 3000]").await;
    output_with(&mut w, "sleeping").await;

    let resp = t
        .post(
            &format!("/v1/agents/{}/messages", agent.id),
            json!({ "text": "STEER KIWI", "steer": true }),
        )
        .await;
    assert_eq!(resp.status(), 202, "{}", resp.text().await.unwrap());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["steered"], true, "{body}");
    assert_eq!(body["queued"], false, "{body}");
    let message_id: MessageId = body["message_id"].as_str().unwrap().parse().unwrap();

    w.until("steered", |e| {
        matches!(&e.kind, EventKind::AgentSteered { message_id: id, text, .. }
            if *id == message_id && text.contains("KIWI"))
    })
    .await;
    w.until("steer read", |e| {
        matches!(&e.kind, EventKind::AgentSteerRead { message_id: id, .. } if *id == message_id)
    })
    .await;
    w.until(
        "turn completed",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == agent.id),
    )
    .await;

    // The model answered the turn with both messages.
    let output = w.output();
    assert!(output.contains("first"), "{output}");
    assert!(output.contains("STEER KIWI"), "{output}");

    let transcript: Value = t
        .get(&format!("/v1/agents/{}/transcript", agent.id))
        .await
        .json()
        .await
        .unwrap();
    let items = transcript["turns"][0]["items"].as_array().unwrap();
    let steer = items
        .iter()
        .find(|item| item["type"] == "steer")
        .unwrap_or_else(|| panic!("no steer item in {items:#?}"));
    assert_eq!(steer["message_id"], json!(message_id));
    assert_eq!(steer["text"], "STEER KIWI");
    assert_eq!(steer["read"], true, "{steer}");

    // Idle: a capable driver still queues nothing; the message is an ordinary turn.
    let resp = t
        .post(
            &format!("/v1/agents/{}/messages", agent.id),
            json!({ "text": "IDLE GAME", "steer": true }),
        )
        .await;
    assert_eq!(resp.status(), 202, "{}", resp.text().await.unwrap());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["steered"], false, "{body}");
    assert_eq!(body["queued"], false, "{body}");
    output_with(&mut w, "IDLE GAME").await;
}

/// A driver that cannot steer queues the message as before; nothing is lost.
#[tokio::test]
async fn an_unsupported_driver_queues_it_instead() {
    let t = TestHub::start().await;
    let agent = t.add_agent("Otto", None).await;
    let mut w = t.watch().await;

    t.send(&agent.id.to_string(), "first [slow]").await;
    output_with(&mut w, "working").await;

    let resp = t
        .post(
            &format!("/v1/agents/{}/messages", agent.id),
            json!({ "text": "QUEUED PEAR", "steer": true }),
        )
        .await;
    assert_eq!(resp.status(), 202, "{}", resp.text().await.unwrap());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["steered"], false, "{body}");
    assert_eq!(body["queued"], true, "{body}");
    let message_id: MessageId = body["message_id"].as_str().unwrap().parse().unwrap();

    // The message waits for its turn, as it would without `steer`.
    w.until("queued input", |e| {
        matches!(&e.kind, EventKind::AgentInput { message_id: id, queued: true, .. }
            if *id == message_id)
    })
    .await;
    // End the long turn so the queued message gets its own.
    let resp = t
        .post(&format!("/v1/agents/{}/cancel", agent.id), json!({}))
        .await;
    assert_eq!(resp.status(), 204, "{}", resp.text().await.unwrap());
    output_with(&mut w, "QUEUED PEAR").await;
    w.until(
        "second turn completed",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == agent.id),
    )
    .await;
    assert!(
        !w.seen
            .iter()
            .any(|e| matches!(&e.kind, EventKind::AgentSteered { .. })),
        "nothing was steered"
    );

    // A second turn carries it in the transcript.
    let transcript: Value = t
        .get(&format!("/v1/agents/{}/transcript", agent.id))
        .await
        .json()
        .await
        .unwrap();
    let turns = transcript["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2, "{turns:#?}");
    assert_eq!(turns[1]["input"], "QUEUED PEAR");
}

/// The CLI may open another turn for a message written after its last model call. Its
/// result must not end the next turn: the message is read, and the turn ends once.
#[tokio::test]
async fn a_late_message_is_read_without_an_orphan_result() {
    // No `--steer-merge`: the mock queues it as another turn, like the real CLI does when
    // the message comes too late for the model call in flight.
    let t = TestHub::start_with(native_plugin_with(&[])).await;
    let agent = t.add_native("Lena", "worker", t.home.path()).await;
    let mut w = t.watch().await;

    t.send(&agent.id.to_string(), "first [sleep 1500]").await;
    output_with(&mut w, "sleeping").await;

    let resp = t
        .post(
            &format!("/v1/agents/{}/messages", agent.id),
            json!({ "text": "STEER LATE", "steer": true }),
        )
        .await;
    assert_eq!(resp.status(), 202, "{}", resp.text().await.unwrap());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["steered"], true, "{body}");
    let message_id: MessageId = body["message_id"].as_str().unwrap().parse().unwrap();

    w.until(
        "steered",
        |e| matches!(&e.kind, EventKind::AgentSteered { message_id: id, .. } if *id == message_id),
    )
    .await;
    w.until("steer read", |e| {
        matches!(&e.kind, EventKind::AgentSteerRead { message_id: id, .. } if *id == message_id)
    })
    .await;
    w.until(
        "first turn completed",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == agent.id),
    )
    .await;
    let output = w.output();
    assert!(output.contains("STEER LATE"), "{output}");

    // The next message is a turn of its own, not ended by a stray result.
    t.send(&agent.id.to_string(), "second").await;
    w.until(
        "second turn completed",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == agent.id),
    )
    .await;
    assert!(w.output().contains("second"), "{}", w.output());
    let transcript: Value = t
        .get(&format!("/v1/agents/{}/transcript", agent.id))
        .await
        .json()
        .await
        .unwrap();
    let turns = transcript["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2, "{turns:#?}");
    assert_eq!(turns[1]["input"], "second");
    assert_eq!(turns[1]["stop_reason"], "end_turn");
}
