//! Steering failures and late messages: nothing may hang, and nothing may claim a message
//! was read when it was not.

mod common;

use common::{TestHub, Watcher, native_plugin_with};
use crossagents_api_types::{EventKind, MessageId, StopReason};
use serde_json::{Value, json};

/// Waits for an `agent.output` whose text contains `needle`.
async fn output_with(w: &mut Watcher, needle: &str) {
    w.until(
        needle,
        |e| matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains(needle)),
    )
    .await;
}

/// A follow-up turn that fails before reading the message must end the turn: the agent
/// must not stay running forever, and nothing may claim the message was read.
#[tokio::test]
async fn a_failed_follow_up_ends_the_turn_and_warns() {
    let t = TestHub::start_with(native_plugin_with(&[])).await;
    let agent = t.add_native("Frida", "worker", t.home.path()).await;
    let mut w = t.watch().await;

    t.send(&agent.id.to_string(), "first [sleep 2500]").await;
    output_with(&mut w, "sleeping").await;

    let resp = t
        .post(
            &format!("/v1/agents/{}/messages", agent.id),
            json!({ "text": "[fail-before-read] LATE ERROR", "steer": true }),
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
    w.until("warning", |e| {
        matches!(&e.kind, EventKind::AgentWarning { message, .. }
            if message.contains("没有被读取") && message.contains("LATE ERROR"))
    })
    .await;
    w.until(
        "turn completed",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == agent.id),
    )
    .await;

    // Not read: the bubble must not say it was.
    let transcript: Value = t
        .get(&format!("/v1/agents/{}/transcript", agent.id))
        .await
        .json()
        .await
        .unwrap();
    let items = transcript["turns"][0]["items"].as_array().unwrap();
    let steer = items.iter().find(|item| item["type"] == "steer").unwrap();
    assert_eq!(steer["read"], false, "{steer}");

    // And the session goes on.
    t.send(&agent.id.to_string(), "after").await;
    output_with(&mut w, "after").await;
}

/// Two late messages both wait for their own read before the turn ends.
#[tokio::test]
async fn two_late_messages_are_both_read() {
    let t = TestHub::start_with(native_plugin_with(&[])).await;
    let agent = t.add_native("Tove", "worker", t.home.path()).await;
    let mut w = t.watch().await;

    t.send(&agent.id.to_string(), "first [sleep 2500]").await;
    output_with(&mut w, "sleeping").await;

    let mut ids = Vec::new();
    for text in ["LATE ONE", "LATE TWO"] {
        let resp = t
            .post(
                &format!("/v1/agents/{}/messages", agent.id),
                json!({ "text": text, "steer": true }),
            )
            .await;
        assert_eq!(resp.status(), 202, "{}", resp.text().await.unwrap());
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["steered"], true, "{body}");
        ids.push(
            body["message_id"]
                .as_str()
                .unwrap()
                .parse::<MessageId>()
                .unwrap(),
        );
    }
    for id in &ids {
        w.until(
            "steer read",
            |e| matches!(&e.kind, EventKind::AgentSteerRead { message_id, .. } if message_id == id),
        )
        .await;
    }
    w.until(
        "turn completed",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == agent.id),
    )
    .await;

    let transcript: Value = t
        .get(&format!("/v1/agents/{}/transcript", agent.id))
        .await
        .json()
        .await
        .unwrap();
    let items = transcript["turns"][0]["items"].as_array().unwrap();
    for (index, item) in items
        .iter()
        .filter(|item| item["type"] == "steer")
        .enumerate()
    {
        assert_eq!(item["read"], true, "{item}");
        assert_eq!(item["message_id"], json!(ids[index]));
    }
}

/// The bubble shows what the person wrote; the notes go only to the model.
#[tokio::test]
async fn a_steer_bubble_shows_the_message_not_the_notice() {
    let t = TestHub::start_with(native_plugin_with(&["--steer-merge"])).await;
    let agent = t.add_native("Nina", "worker", t.home.path()).await;
    let mut w = t.watch().await;

    t.send(&agent.id.to_string(), "first [sleep 3000]").await;
    output_with(&mut w, "sleeping").await;
    t.hub
        .runtime
        .tell_later(agent.id, "model", "model changed to mock-haiku".into())
        .await;

    let resp = t
        .post(
            &format!("/v1/agents/{}/messages", agent.id),
            json!({ "text": "STEER PLUM", "steer": true }),
        )
        .await;
    assert_eq!(resp.status(), 202, "{}", resp.text().await.unwrap());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["steered"], true, "{body}");

    let steered = w
        .until("steered", |e| {
            matches!(&e.kind, EventKind::AgentSteered { .. })
        })
        .await;
    let EventKind::AgentSteered { text, .. } = &steered.kind else {
        unreachable!()
    };
    assert_eq!(text, "STEER PLUM", "the bubble shows the notice too");

    w.until(
        "turn completed",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == agent.id),
    )
    .await;
    // The model was told, with the steered text.
    let output = w.output();
    assert!(output.contains("model changed to mock-haiku"), "{output}");
    assert!(output.contains("STEER PLUM"), "{output}");

    // The note was used once.
    t.send(&agent.id.to_string(), "clean next").await;
    output_with(&mut w, "clean next").await;
    assert_eq!(
        w.output().matches("model changed to mock-haiku").count(),
        1,
        "{}",
        w.output()
    );
}

/// A steer that falls back keeps its notes for the queued copy, and its input shows the
/// message as written.
#[tokio::test]
async fn a_failed_steer_keeps_the_notice_for_the_queue() {
    let t = TestHub::start().await;
    let agent = t.add_agent("Olga", None).await;
    let mut w = t.watch().await;

    t.send(&agent.id.to_string(), "first [slow]").await;
    output_with(&mut w, "working").await;
    t.hub
        .runtime
        .tell_later(agent.id, "model", "model changed to mock-opus".into())
        .await;

    let resp = t
        .post(
            &format!("/v1/agents/{}/messages", agent.id),
            json!({ "text": "STEER PEAR", "steer": true }),
        )
        .await;
    assert_eq!(resp.status(), 202, "{}", resp.text().await.unwrap());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["steered"], false, "{body}");
    let message_id: MessageId = body["message_id"].as_str().unwrap().parse().unwrap();

    let input = w
        .until("queued input", |e| {
            matches!(&e.kind, EventKind::AgentInput { message_id: id, .. } if *id == message_id)
        })
        .await;
    let EventKind::AgentInput { text, .. } = &input.kind else {
        unreachable!()
    };
    assert_eq!(text, "STEER PEAR", "the queued input carries the notice");

    let resp = t
        .post(&format!("/v1/agents/{}/cancel", agent.id), json!({}))
        .await;
    assert_eq!(resp.status(), 204, "{}", resp.text().await.unwrap());
    output_with(&mut w, "STEER PEAR").await;
    assert_eq!(
        w.output().matches("model changed to mock-opus").count(),
        1,
        "{}",
        w.output()
    );

    t.send(&agent.id.to_string(), "clean again").await;
    output_with(&mut w, "clean again").await;
    assert_eq!(
        w.output().matches("model changed to mock-opus").count(),
        1,
        "{}",
        w.output()
    );
}

/// Interrupt keeps the unread steer queued (as Claude Code does): the turn waits for it,
/// reads it, and ends once.
#[tokio::test]
async fn an_interrupt_keeps_the_unread_steer() {
    let t = TestHub::start_with(native_plugin_with(&[])).await;
    let agent = t.add_native("Iris", "worker", t.home.path()).await;
    let mut w = t.watch().await;

    t.send(&agent.id.to_string(), "first [slow]").await;
    output_with(&mut w, "working").await;

    let resp = t
        .post(
            &format!("/v1/agents/{}/messages", agent.id),
            json!({ "text": "KEEP ME", "steer": true }),
        )
        .await;
    assert_eq!(resp.status(), 202, "{}", resp.text().await.unwrap());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["steered"], true, "{body}");
    let message_id: MessageId = body["message_id"].as_str().unwrap().parse().unwrap();

    let resp = t
        .post(&format!("/v1/agents/{}/cancel", agent.id), json!({}))
        .await;
    assert_eq!(resp.status(), 204, "{}", resp.text().await.unwrap());

    w.until("steer read", |e| {
        matches!(&e.kind, EventKind::AgentSteerRead { message_id: id, .. } if *id == message_id)
    })
    .await;
    let ended = w
        .until(
            "turn completed",
            |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == agent.id),
        )
        .await;
    let EventKind::AgentTurnCompleted { stop_reason, .. } = &ended.kind else {
        unreachable!()
    };
    assert_eq!(*stop_reason, StopReason::Cancelled, "{ended:?}");
    assert!(
        !w.seen.iter().any(|e| matches!(
            &e.kind,
            EventKind::AgentWarning { message, .. } if message.contains("没有被读取")
        )),
        "the steer was read, not lost"
    );

    // The session goes on.
    t.send(&agent.id.to_string(), "after interrupt").await;
    output_with(&mut w, "after interrupt").await;
}
