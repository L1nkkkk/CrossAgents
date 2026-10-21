//! H1-1 (docs/tasks/H1.md): identity, threads, retries, reminders, watches and the
//! notification digest, driven through the mock agent's `[call …]` directives.

mod common;

use common::TestHub;
use crossagents_core::{EventKind, MessageSource};

fn input_of(event: &crossagents_core::Event) -> Option<(&str, &MessageSource)> {
    match &event.kind {
        EventKind::AgentInput {
            text,
            from: Some(from),
            ..
        } => Some((text.as_str(), from)),
        _ => None,
    }
}

#[tokio::test]
async fn whoami_lists_groups_and_tools() {
    let t = TestHub::start().await;
    t.add_agent("alice", None).await;
    let mut w = t.watch().await;
    t.send("alice", "[call whoami {}]").await;
    w.find("turn completed", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;
    let out = w.output();
    assert!(
        out.contains(r#""everyone","#) && out.contains(r#""assignee""#),
        "{out}"
    );
    assert!(
        out.contains("remind_set") && out.contains("task_complete"),
        "{out}"
    );
    assert!(!out.contains("plan_submit"), "{out}");
}

#[tokio::test]
async fn replies_join_the_thread_and_retries_send_once() {
    let t = TestHub::start().await;
    let alice = t.add_agent("alice", None).await;
    let bob = t.add_agent("bob", None).await;
    let mut w = t.watch().await;

    t.send(
        "alice",
        r#"[call agent_send {"to":"bob","text":"ping, are you there?"}]"#,
    )
    .await;
    let ping = w
        .until("bob gets ping", |e| {
            e.kind.agent_id() == Some(bob.id)
                && input_of(e).is_some_and(|(text, _)| text.contains("ping"))
        })
        .await;
    let EventKind::AgentInput {
        message_id: ping_id,
        from,
        thread,
        ..
    } = ping.kind
    else {
        unreachable!()
    };
    assert_eq!(from, Some(MessageSource::Agent { agent_id: alice.id }));
    assert_eq!(thread, None);

    // A retried send within the minute is the same message.
    w.find("alice done", |e| {
        e.kind.agent_id() == Some(alice.id)
            && matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;
    t.send(
        "alice",
        r#"[call agent_send {"to":"bob","text":"ping, are you there?"}]"#,
    )
    .await;
    w.until("alice done again", |e| {
        e.kind.agent_id() == Some(alice.id)
            && matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;
    assert!(w.output().contains(r#""repeated": true"#), "{}", w.output());

    // Bob answers; Alice's input joins the thread and quotes what it answers.
    let reply =
        format!(r#"[call agent_send {{"to":"alice","text":"pong","reply_to":"{ping_id}"}}]"#);
    t.send("bob", &reply).await;
    let pong = w
        .until("alice gets pong", |e| {
            e.kind.agent_id() == Some(alice.id)
                && input_of(e).is_some_and(|(text, _)| text.contains("pong"))
        })
        .await;
    let EventKind::AgentInput {
        reply_to,
        thread,
        reply_excerpt,
        ..
    } = pong.kind
    else {
        unreachable!()
    };
    assert_eq!(reply_to, Some(ping_id));
    assert_eq!(thread, Some(ping_id));
    assert_eq!(reply_excerpt.as_deref(), Some("ping, are you there?"));
    let pings = w
        .seen
        .iter()
        .filter(|e| {
            e.kind.agent_id() == Some(bob.id)
                && input_of(e).is_some_and(|(t, _)| t.contains("ping"))
        })
        .count();
    assert_eq!(pings, 1, "the retried ping was delivered again");
}

#[tokio::test]
async fn reminders_and_watches_arrive_as_one_digest() {
    let t = TestHub::start().await;
    let alice = t.add_agent("alice", None).await;
    t.add_agent("bob", None).await;
    let mut w = t.watch().await;

    // A reminder comes due and arrives as a notice.
    t.send(
        "alice",
        r#"[call remind_set {"after_minutes":0.02,"note":"check the build"}] [call watch {"agent":"bob","on":["done"]}]"#,
    )
    .await;
    w.find("reminder fired", |e| {
        matches!(e.kind, EventKind::ReminderFired { .. })
    })
    .await;
    w.find("notice to alice", |e| {
        e.kind.agent_id() == Some(alice.id)
            && input_of(e).is_some_and(|(text, from)| {
                *from == MessageSource::Notice && text.contains("check the build")
            })
    })
    .await;

    // While Alice is busy, news about Bob gathers in one waiting digest.
    w.until(
        "alice read the notice",
        |e| matches!(e.kind, EventKind::AgentTurnCompleted { id, .. } if id == alice.id),
    )
    .await;
    t.send("alice", "[slow]").await;
    w.until(
        "alice busy",
        |e| matches!(e.kind, EventKind::AgentTurnStarted { id, .. } if id == alice.id),
    )
    .await;
    t.send("bob", "one").await;
    w.until("first notice queued", |e| {
        e.kind.agent_id() == Some(alice.id)
            && input_of(e).is_some_and(|(text, from)| {
                *from == MessageSource::Notice && text.contains("bob finished a turn")
            })
    })
    .await;
    t.send("bob", "two").await;
    w.until(
        "digest updated",
        |e| matches!(e.kind, EventKind::AgentInputUpdated { id, .. } if id == alice.id),
    )
    .await;
    let queued_notices = w
        .seen
        .iter()
        .filter(|e| {
            e.kind.agent_id() == Some(alice.id)
                && input_of(e).is_some_and(|(text, from)| {
                    *from == MessageSource::Notice && text.contains("bob")
                })
        })
        .count();
    assert_eq!(queued_notices, 1, "notices piled up instead of merging");
    t.post("/v1/agents/alice/cancel", serde_json::json!({}))
        .await;
}
