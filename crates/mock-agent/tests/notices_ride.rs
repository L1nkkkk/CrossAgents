//! docs/TODO.md §二十五 第 3 条: a notice digest still waiting when a person writes rides
//! along with the person's message instead of taking a turn of its own.

mod common;

use common::TestHub;
use crossagents_core::{AgentId, EventKind, MessageId, MessageSource};
use serde_json::json;

/// The notice the hub queued for `who`: its message id and text.
fn notice_for(event: &crossagents_core::Event, who: AgentId) -> Option<(MessageId, String)> {
    match &event.kind {
        EventKind::AgentInput {
            id,
            message_id,
            text,
            from: Some(MessageSource::Notice),
            ..
        } if *id == who => Some((*message_id, text.clone())),
        _ => None,
    }
}

#[tokio::test]
async fn a_waiting_digest_rides_with_a_person_s_message() {
    let t = TestHub::start().await;
    let alice = t.add_agent("alice", None).await;
    t.add_agent("bob", None).await;
    let mut w = t.watch().await;

    t.send("alice", r#"[call watch {"agent":"bob","on":["done"]}]"#)
        .await;
    w.until(
        "alice set the watch",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == alice.id),
    )
    .await;

    // Alice is busy: bob's finished turn becomes one waiting digest.
    t.send("alice", "[slow]").await;
    w.until(
        "alice busy",
        |e| matches!(&e.kind, EventKind::AgentTurnStarted { id, .. } if *id == alice.id),
    )
    .await;
    t.send("bob", "one").await;
    let notice = w
        .until("the digest waits", |e| notice_for(e, alice.id).is_some())
        .await;
    let (digest_id, digest_text) = notice_for(&notice, alice.id).expect("the digest");
    assert!(digest_text.contains("bob finished a turn"), "{digest_text}");

    // A person's message takes the waiting digest with it: the digest leaves the queue and
    // the message says what it carried, while `text` stays the person's words.
    let sent = t.send("alice", "please stop").await;
    let human_id: MessageId = serde_json::from_value(sent["message_id"].clone()).unwrap();
    let human = w
        .until("the person's message", |e| {
            matches!(
                &e.kind,
                EventKind::AgentInput { message_id, from: Some(MessageSource::Human), .. }
                    if *message_id == human_id
            )
        })
        .await;
    let EventKind::AgentInput { text, notices, .. } = &human.kind else {
        unreachable!()
    };
    assert_eq!(text, "please stop");
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("bob finished a turn"), "{notices:?}");
    w.find("the digest left the queue", |e| {
        matches!(
            &e.kind,
            EventKind::AgentInputWithdrawn { message_id, .. } if *message_id == digest_id
        )
    })
    .await;

    // It ends up in one turn, not two: the person's words first, then the notices.
    t.post("/v1/agents/alice/cancel", json!({})).await;
    w.until("alice finishes the message", |e| {
        matches!(
            &e.kind,
            EventKind::AgentTurnCompleted { id, message_id, .. }
                if *id == alice.id && *message_id == human_id
        )
    })
    .await;
    let from = w
        .seen
        .iter()
        .position(|e| {
            matches!(
                &e.kind,
                EventKind::AgentInput { message_id, .. } if *message_id == human_id
            )
        })
        .expect("the input was seen");
    let started = w.seen[from..]
        .iter()
        .filter(|e| matches!(&e.kind, EventKind::AgentTurnStarted { id, .. } if *id == alice.id))
        .count();
    assert_eq!(started, 1, "the carried notices took a turn of their own");

    let out = w.output();
    let person = out
        .find("please stop")
        .expect("the person's words reached it");
    let carried = out
        .find("CrossAgents notices")
        .expect("the notice reached it");
    assert!(
        person < carried,
        "the notice came before the person's words: {out}"
    );
    assert!(out.contains("bob finished a turn"), "{out}");

    // News that arrives after the message gets a digest of its own, as before.
    t.send("bob", "two").await;
    let next = w
        .until("a new digest", |e| {
            notice_for(e, alice.id).is_some_and(|(id, _)| id != digest_id)
        })
        .await;
    let (next_id, next_text) = notice_for(&next, alice.id).expect("the new digest");
    assert_ne!(next_id, digest_id);
    assert!(next_text.contains("bob finished a turn"), "{next_text}");
}

#[tokio::test]
async fn a_digest_that_already_started_is_not_merged() {
    let t = TestHub::start().await;
    let alice = t.add_agent("alice", None).await;
    t.add_agent("bob", None).await;
    let mut w = t.watch().await;

    t.send("alice", r#"[call watch {"agent":"bob","on":["done"]}]"#)
        .await;
    w.until(
        "alice set the watch",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == alice.id),
    )
    .await;
    t.send("alice", "[slow]").await;
    w.until(
        "alice busy",
        |e| matches!(&e.kind, EventKind::AgentTurnStarted { id, .. } if *id == alice.id),
    )
    .await;
    t.send("bob", "one").await;
    let notice = w
        .until("the digest waits", |e| notice_for(e, alice.id).is_some())
        .await;
    let (digest_id, _) = notice_for(&notice, alice.id).expect("the digest");

    // Stop the slow turn: the digest starts running as its own turn.
    t.post("/v1/agents/alice/cancel", json!({})).await;
    w.until("the digest ran", |e| {
        matches!(
            &e.kind,
            EventKind::AgentTurnCompleted { id, message_id, .. }
                if *id == alice.id && *message_id == digest_id
        )
    })
    .await;

    // A person's message after that does not take it: it already started.
    let sent = t.send("alice", "next").await;
    let human_id: MessageId = serde_json::from_value(sent["message_id"].clone()).unwrap();
    let human = w
        .until("the person's message", |e| {
            matches!(
                &e.kind,
                EventKind::AgentInput { message_id, .. } if *message_id == human_id
            )
        })
        .await;
    let EventKind::AgentInput {
        text,
        notices,
        from,
        ..
    } = &human.kind
    else {
        unreachable!()
    };
    assert_eq!(*from, Some(MessageSource::Human));
    assert_eq!(text, "next");
    assert!(notices.is_empty(), "{notices:?}");
    let withdrawn = w.seen.iter().any(|e| {
        matches!(
            &e.kind,
            EventKind::AgentInputWithdrawn { message_id, .. } if *message_id == digest_id
        )
    });
    assert!(!withdrawn, "a digest that started was withdrawn");

    w.until("alice answers the person", |e| {
        matches!(
            &e.kind,
            EventKind::AgentTurnCompleted { id, message_id, .. }
                if *id == alice.id && *message_id == human_id
        )
    })
    .await;
    assert!(w.output().contains("next"), "{}", w.output());
}
