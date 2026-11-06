//! Delivering a person's message: why it waits, retries that do not repeat, and "send
//! now" that promotes a waiting message in place (docs/tasks/DELIVERY-AUDIT.md §2–§4).

mod common;

use common::{TestHub, Watcher, native_plugin_with};
use crossagents_api_types::{EventKind, MessageId, QueuedReason};
use serde_json::{Value, json};

/// Waits for an `agent.output` whose text contains `needle`.
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

fn message_id(body: &Value) -> MessageId {
    body["message_id"].as_str().unwrap().parse().unwrap()
}

/// An ordinary queued send says why it waits too: a turn is running, or a person's message
/// is already ahead of it.
#[tokio::test]
async fn a_queued_send_says_what_it_waits_for() {
    let t = TestHub::start().await;
    let agent = t.add_agent("Vera", None).await;
    let mut w = t.watch().await;

    t.send(&agent.id.to_string(), "first [slow]").await;
    output_with(&mut w, "working").await;

    let second = t.send(&agent.id.to_string(), "SECOND PEAR").await;
    assert_eq!(second["queued"], true, "{second}");
    assert_eq!(second["queued_reason"], "turn_running", "{second}");

    // A person's message is already waiting: the next one is behind it.
    let third = t.send(&agent.id.to_string(), "THIRD PLUM").await;
    assert_eq!(third["queued"], true, "{third}");
    assert_eq!(third["queued_reason"], "owner_queue_ahead", "{third}");
}

/// Reloading the transcript keeps what the live events said: why a waiting message waits,
/// and one taken back is still there, marked, instead of vanishing
/// (docs/TODO.md §二十五「2 的尾巴」).
#[tokio::test]
async fn a_reloaded_transcript_keeps_the_queue_reason_and_the_withdrawal() {
    let t = TestHub::start().await;
    let agent = t.add_agent("Vera", None).await;
    let mut w = t.watch().await;

    t.send(&agent.id.to_string(), "first [slow]").await;
    output_with(&mut w, "working").await;
    let waiting = t.send(&agent.id.to_string(), "SECOND PEAR").await;
    let waiting_id = message_id(&waiting);
    let taken_back = t.send(&agent.id.to_string(), "THIRD PLUM").await;
    let taken_back_id = message_id(&taken_back);

    let resp = t
        .delete(&format!("/v1/agents/{}/messages/{taken_back_id}", agent.id))
        .await;
    assert_eq!(resp.status(), 204, "{}", resp.text().await.unwrap());

    let transcript: Value = t
        .get(&format!("/v1/agents/{}/transcript", agent.id))
        .await
        .json()
        .await
        .unwrap();
    let turns = transcript["turns"].as_array().unwrap();
    let waiting_turn = turns
        .iter()
        .find(|turn| turn["message_id"] == json!(waiting_id))
        .unwrap_or_else(|| panic!("no waiting turn in {transcript:#?}"));
    assert_eq!(
        waiting_turn["queued_reason"], "turn_running",
        "the reason the live events showed is missing after a reload: {transcript:#?}"
    );
    assert!(waiting_turn["withdrawn"].is_null(), "{waiting_turn}");

    let withdrawn = turns
        .iter()
        .find(|turn| turn["message_id"] == json!(taken_back_id))
        .unwrap_or_else(|| panic!("the withdrawn row vanished on a reload: {transcript:#?}"));
    assert_eq!(withdrawn["withdrawn"], true, "{withdrawn}");
    assert!(withdrawn["started_at"].is_null(), "{withdrawn}");
}

/// A driver that cannot steer (ACP) queues a `steer: true` message behind the running
/// turn, and says why: `runtime_no_steer`, never a silent queue.
#[tokio::test]
async fn a_driver_without_steer_queues_with_its_reason() {
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
    assert_eq!(body["queued_reason"], "runtime_no_steer", "{body}");
    let id = message_id(&body);

    let input = w
        .until(
            "queued input",
            |e| matches!(&e.kind, EventKind::AgentInput { message_id: m, .. } if *m == id),
        )
        .await;
    let EventKind::AgentInput {
        queued,
        queued_reason,
        ..
    } = &input.kind
    else {
        unreachable!()
    };
    assert!(*queued, "{input:?}");
    assert_eq!(
        *queued_reason,
        Some(QueuedReason::RuntimeNoSteer),
        "{input:?}"
    );

    // It was not lost: it runs once the turn it could not join ends.
    let resp = t
        .post(&format!("/v1/agents/{}/cancel", agent.id), json!({}))
        .await;
    assert_eq!(resp.status(), 204, "{}", resp.text().await.unwrap());
    output_with(&mut w, "QUEUED PEAR").await;
    turn_completed(&mut w).await;
    assert!(
        !w.seen
            .iter()
            .any(|e| matches!(&e.kind, EventKind::AgentSteered { .. })),
        "nothing was steered"
    );
}

/// A steer that arrives just after the running turn ended is queued for the next one
/// with `no_active_turn` — also not silently.
#[tokio::test]
async fn a_steer_after_the_turn_ended_says_no_active_turn() {
    let t = TestHub::start_with(native_plugin_with(&[])).await;
    let agent = t.add_native("Nell", "worker", t.home.path()).await;
    let mut w = t.watch().await;

    t.send(&agent.id.to_string(), "first").await;
    turn_completed(&mut w).await;

    let resp = t
        .post(
            &format!("/v1/agents/{}/messages", agent.id),
            json!({ "text": "NEXT PLUM", "steer": true }),
        )
        .await;
    assert_eq!(resp.status(), 202, "{}", resp.text().await.unwrap());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["steered"], false, "{body}");
    assert_eq!(body["queued"], false, "{body}");
    assert_eq!(body["queued_reason"], "no_active_turn", "{body}");
    let id = message_id(&body);

    let input = w
        .until(
            "input",
            |e| matches!(&e.kind, EventKind::AgentInput { message_id: m, .. } if *m == id),
        )
        .await;
    let EventKind::AgentInput { queued_reason, .. } = &input.kind else {
        unreachable!()
    };
    assert_eq!(
        *queued_reason,
        Some(QueuedReason::NoActiveTurn),
        "{input:?}"
    );

    // It is the next turn, read by the model.
    output_with(&mut w, "NEXT PLUM").await;
    turn_completed(&mut w).await;
}

/// A steer the CLI cannot take (its stdin is gone) is a visible failure: the response says
/// `steer_write_failed`, the event says it too, and nothing is queued — the person decides
/// whether to send it again.
#[tokio::test]
async fn a_failed_steer_write_is_shown_and_never_queued() {
    let t = TestHub::start_with(native_plugin_with(&["--stdin-stuck"])).await;
    let agent = t.add_native("Wade", "worker", t.home.path()).await;
    let mut w = t.watch().await;

    t.send(&agent.id.to_string(), "first [slow]").await;
    // The mock stopped reading its input when it took this message: the steer below
    // cannot be written into it.
    output_with(&mut w, "working").await;

    let resp = t
        .post(
            &format!("/v1/agents/{}/messages", agent.id),
            json!({ "text": "STEER WREN", "steer": true }),
        )
        .await;
    assert_eq!(resp.status(), 502, "{}", resp.text().await.unwrap());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "agent_error", "{body}");
    assert_eq!(body["error"]["reason"], "steer_write_failed", "{body}");

    let refused = w
        .until("refused", |e| {
            matches!(&e.kind, EventKind::AgentInputRefused { text, .. } if text.contains("WREN"))
        })
        .await;
    let EventKind::AgentInputRefused {
        message_id: refused_id,
        reason,
        ..
    } = &refused.kind
    else {
        unreachable!()
    };
    assert_eq!(*reason, QueuedReason::SteerWriteFailed, "{refused:?}");
    // Not queued: no `agent.input` ever carried this message, so no turn can pick it up.
    assert!(
        !w.seen.iter().any(|e| matches!(
            &e.kind,
            EventKind::AgentInput { message_id: m, .. } if m == refused_id
        )),
        "the refused message was queued anyway"
    );

    // The CLI died with the write, so the hub says it exited and no `agent.input` for the
    // refused message ever existed. A new session carries on, and the refused text was
    // never read by a model.
    w.until("exited", |e| {
        matches!(&e.kind, EventKind::AgentExited { .. })
    })
    .await;
    t.send(&agent.id.to_string(), "after failure").await;
    output_with(&mut w, "after failure").await;
    assert!(!w.output().contains("WREN"), "{}", w.output());
}

/// The same `client_message_id` twice is one message: the second answer names the first
/// turn, and only one turn runs.
#[tokio::test]
async fn the_same_client_message_id_twice_is_one_message() {
    let t = TestHub::start().await;
    let agent = t.add_agent("Dana", None).await;
    let mut w = t.watch().await;

    let client = MessageId::new();
    let path = format!("/v1/agents/{}/messages", agent.id);
    let body = json!({ "text": "ONCE FIG", "client_message_id": client });
    let (first, repeat) = tokio::join!(t.post(&path, body.clone()), t.post(&path, body.clone()));
    assert_eq!(first.status(), 202, "{}", first.text().await.unwrap());
    assert_eq!(repeat.status(), 202, "{}", repeat.text().await.unwrap());
    let first: Value = first.json().await.unwrap();
    let repeat: Value = repeat.json().await.unwrap();
    assert_eq!(
        message_id(&repeat),
        message_id(&first),
        "a repeat is answered with the first message: {first} {repeat}"
    );
    assert_eq!(repeat["queued"], first["queued"], "{first} {repeat}");

    turn_completed(&mut w).await;
    // One turn in the conversation, and the model read the words once.
    let transcript: Value = t
        .get(&format!("/v1/agents/{}/transcript", agent.id))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        transcript["turns"].as_array().unwrap().len(),
        1,
        "{transcript:#?}"
    );
    assert_eq!(w.output().matches("ONCE FIG").count(), 1, "{}", w.output());
}

/// A repeat that arrives after the message started still gets the first answer: it does
/// not open another turn.
#[tokio::test]
async fn a_repeat_after_it_started_is_the_first_message() {
    let t = TestHub::start_with(native_plugin_with(&[])).await;
    let agent = t.add_native("Rune", "worker", t.home.path()).await;
    let mut w = t.watch().await;

    let client = MessageId::new();
    let path = format!("/v1/agents/{}/messages", agent.id);
    let first: Value = t
        .post(
            &path,
            json!({ "text": "RUNNING FIG", "client_message_id": client }),
        )
        .await
        .json()
        .await
        .unwrap();
    let id = message_id(&first);
    output_with(&mut w, "RUNNING FIG").await;

    let repeat: Value = t
        .post(
            &path,
            json!({ "text": "RUNNING FIG", "client_message_id": client }),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(message_id(&repeat), id, "{first} {repeat}");
    assert_eq!(repeat["queued"], first["queued"], "{first} {repeat}");

    turn_completed(&mut w).await;
    let transcript: Value = t
        .get(&format!("/v1/agents/{}/transcript", agent.id))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        transcript["turns"].as_array().unwrap().len(),
        1,
        "{transcript:#?}"
    );
    assert_eq!(
        w.output().matches("RUNNING FIG").count(),
        1,
        "{}",
        w.output()
    );
}

/// `send-now` promotes the waiting message in place: the same id runs first, nothing is
/// withdrawn and sent again.
#[tokio::test]
async fn send_now_promotes_the_waiting_message_in_place() {
    let t = TestHub::start().await;
    let agent = t.add_agent("Pia", None).await;
    let mut w = t.watch().await;

    t.send(&agent.id.to_string(), "first [slow]").await;
    output_with(&mut w, "working").await;
    let second = t.send(&agent.id.to_string(), "SECOND PEAR").await;
    let third = t.send(&agent.id.to_string(), "THIRD PLUM").await;
    let third_id = message_id(&third);

    let resp = t
        .post(
            &format!("/v1/agents/{}/messages/{third_id}/send-now", agent.id),
            json!({}),
        )
        .await;
    assert_eq!(resp.status(), 202, "{}", resp.text().await.unwrap());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        message_id(&body),
        third_id,
        "the promoted message keeps its id: {body}"
    );

    // The running turn is interrupted, and the promoted message is the next turn: it runs
    // before the one that was ahead of it.
    output_with(&mut w, "THIRD PLUM").await;
    output_with(&mut w, "SECOND PEAR").await;
    let output = w.output();
    assert!(
        output.find("THIRD PLUM") < output.find("SECOND PEAR"),
        "{output}"
    );
    assert_ne!(message_id(&second), third_id);

    // One turn, under the id it had while waiting.
    let transcript: Value = t
        .get(&format!("/v1/agents/{}/transcript", agent.id))
        .await
        .json()
        .await
        .unwrap();
    let promoted = transcript["turns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|turn| turn["input"] == "THIRD PLUM")
        .unwrap_or_else(|| panic!("no promoted turn in {transcript:#?}"));
    assert_eq!(promoted["message_id"], json!(third_id), "{promoted}");
    assert!(promoted["started_at"].is_string(), "{promoted}");
}

/// `send-now` on a message that already started is a conflict, and leaves the queue as it
/// was: the message waiting behind it still runs next.
#[tokio::test]
async fn send_now_on_a_started_message_is_a_conflict() {
    let t = TestHub::start().await;
    let agent = t.add_agent("Ulf", None).await;
    let mut w = t.watch().await;

    let first = t.send(&agent.id.to_string(), "first [slow]").await;
    let first_id = message_id(&first);
    output_with(&mut w, "working").await;
    t.send(&agent.id.to_string(), "SECOND PEAR").await;

    let resp = t
        .post(
            &format!("/v1/agents/{}/messages/{first_id}/send-now", agent.id),
            json!({}),
        )
        .await;
    assert_eq!(resp.status(), 409, "{}", resp.text().await.unwrap());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "conflict", "{body}");
    assert_eq!(body["error"]["reason"], "already_started", "{body}");

    // The queue is untouched: the message sent before the attempt runs after the turn.
    let resp = t
        .post(&format!("/v1/agents/{}/cancel", agent.id), json!({}))
        .await;
    assert_eq!(resp.status(), 204, "{}", resp.text().await.unwrap());
    output_with(&mut w, "SECOND PEAR").await;
}
