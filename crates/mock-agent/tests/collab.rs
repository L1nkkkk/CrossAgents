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
    // A plain chat outside any run: no role's group, no run tools (docs/SESSION-FIRST.md §7).
    assert!(
        out.contains(r#""everyone","#) && out.contains(r#""conversation""#),
        "{out}"
    );
    assert!(!out.contains(r#""assignee""#), "{out}");
    assert!(
        out.contains("remind_set") && out.contains("work_start") && out.contains("agent_spawn"),
        "{out}"
    );
    for run_tool in [
        "plan_submit",
        "task_complete",
        "task_create",
        "workspace_run",
    ] {
        assert!(!out.contains(run_tool), "{run_tool}: {out}");
    }
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

/// The text of `event` when it is a message for `to` that the hub forwarded from `from`.
fn forwarded_to(
    event: &crossagents_core::Event,
    to: crossagents_core::AgentId,
    from: crossagents_core::AgentId,
) -> bool {
    event.kind.agent_id() == Some(to)
        && input_of(event).is_some_and(|(text, source)| {
            text.contains("forwarded by the hub")
                && *source == MessageSource::Agent { agent_id: from }
        })
}

#[tokio::test]
async fn an_answer_only_written_reaches_the_teammate_who_asked() {
    let t = TestHub::start().await;
    let alice = t.add_agent("alice", None).await;
    let bob = t.add_agent("bob", None).await;
    let mut w = t.watch().await;

    // Bob (the mock) answers in text only: its echo of the question.
    t.send(
        "alice",
        r#"[call agent_send {"to":"bob","text":"what did the tests say?"}]"#,
    )
    .await;
    let ask = w
        .until("bob gets the question", |e| {
            e.kind.agent_id() == Some(bob.id)
                && input_of(e).is_some_and(|(text, _)| text.contains("what did the tests say?"))
        })
        .await;
    let EventKind::AgentInput {
        message_id: ask_id,
        text,
        ..
    } = ask.kind
    else {
        unreachable!()
    };
    // The header names the message, so it can be answered with reply_to alone.
    assert!(
        text.starts_with(&format!("Message from alice (id {ask_id}):")),
        "{text}"
    );

    let answer = w
        .until("alice gets bob's answer", |e| {
            forwarded_to(e, alice.id, bob.id)
        })
        .await;
    let EventKind::AgentInput {
        message_id: answer_id,
        text,
        reply_to,
        ..
    } = answer.kind
    else {
        unreachable!()
    };
    assert_eq!(reply_to, Some(ask_id));
    assert!(
        text.contains("what did the tests say?"),
        "the answer is bob's turn text: {text}"
    );

    // Alice's own text answer to the forwarded one stays with her: nothing bounces back.
    w.until("alice answered the forward", |e| {
        matches!(&e.kind, EventKind::AgentTurnCompleted { id, message_id, .. } if *id == alice.id && *message_id == answer_id)
    })
    .await;
    // A forward would be on its way by now; a person's message to bob goes in after it.
    let sent = t.send("bob", "a person's message").await;
    let settled: crossagents_core::MessageId =
        serde_json::from_value(sent["message_id"].clone()).unwrap();
    w.until("bob takes the person's message", |e| {
        matches!(&e.kind, EventKind::AgentTurnCompleted { id, message_id, .. } if *id == bob.id && *message_id == settled)
    })
    .await;
    let to_bob = w
        .seen
        .iter()
        .filter(|e| forwarded_to(e, bob.id, alice.id))
        .count();
    assert_eq!(
        to_bob, 0,
        "an answer to a forwarded answer was forwarded again"
    );
    // Nor is a person's message answered back to anyone.
    let to_alice = w
        .seen
        .iter()
        .filter(|e| forwarded_to(e, alice.id, bob.id))
        .count();
    assert_eq!(to_alice, 1, "{to_alice} forwards to alice");
}

#[tokio::test]
async fn an_answer_that_was_sent_is_not_forwarded_too() {
    let t = TestHub::start().await;
    let alice = t.add_agent("alice", None).await;
    let bob = t.add_agent("bob", None).await;
    let mut w = t.watch().await;

    // Bob's question tells it to answer with reply_to alone (no `to`).
    t.send("alice", r#"[call agent_send {"to":"bob","text":"ping"}]"#)
        .await;
    let ask = w
        .until("bob gets ping", |e| {
            e.kind.agent_id() == Some(bob.id)
                && input_of(e).is_some_and(|(text, _)| text.contains("ping"))
        })
        .await;
    let EventKind::AgentInput {
        message_id: ask_id, ..
    } = ask.kind
    else {
        unreachable!()
    };
    // Bob's first turn (the echo) ends and is forwarded; this second one answers on purpose.
    w.until(
        "bob's first turn",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == bob.id),
    )
    .await;
    let reply = format!(r#"[call agent_send {{"reply_to":"{ask_id}","text":"pong"}}]"#);
    t.send("bob", &reply).await;
    let pong = w
        .until("alice gets pong", |e| {
            e.kind.agent_id() == Some(alice.id)
                && input_of(e)
                    .is_some_and(|(text, _)| text.contains("pong") && !text.contains("forwarded"))
        })
        .await;
    let EventKind::AgentInput { reply_to, .. } = pong.kind else {
        unreachable!()
    };
    assert_eq!(reply_to, Some(ask_id), "reply_to alone went to who asked");
}

#[tokio::test]
async fn a_namesake_of_the_teammate_talking_to_you_is_refused() {
    let t = TestHub::start().await;
    let pia = t.add_agent("pia", None).await;
    let pia4 = t.add_agent("pia 4", None).await;
    let rune = t.add_agent("rune", None).await;
    let mut w = t.watch().await;

    t.send(
        &pia4.id.to_string(),
        r#"[call agent_send {"to":"rune","text":"status?"}]"#,
    )
    .await;
    w.until("rune gets it", |e| {
        e.kind.agent_id() == Some(rune.id)
            && input_of(e).is_some_and(|(text, _)| text.contains("status?"))
    })
    .await;
    w.until(
        "rune's turn ends",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == rune.id),
    )
    .await;
    let sent = t
        .send(
            "rune",
            r#"[call agent_send {"to":"pia","text":"all done"}]"#,
        )
        .await;
    let message: crossagents_core::MessageId =
        serde_json::from_value(sent["message_id"].clone()).unwrap();
    w.until("rune tried", |e| {
        matches!(&e.kind, EventKind::AgentTurnCompleted { id, message_id, .. } if *id == rune.id && *message_id == message)
    })
    .await;
    let out = w.output();
    assert!(
        out.contains("is not pia 4") && out.contains(r#"send to "pia 4""#),
        "{out}"
    );
    let reached_pia = w.seen.iter().any(|e| {
        input_of(e).is_some_and(|(text, _)| text.contains("all done"))
            && e.kind.agent_id() == Some(pia.id)
    });
    assert!(!reached_pia, "the namesake got the message");
}

// ---------------------------------------------------------------------------------------
// Wake rules (docs/research/multica.md §2.1): the owner's own turn never wakes it, a chain
// with nobody in between pauses it, a person's word starts the chain over, a wait lasts at
// most seven days, and a reminder asking for longer is cut to that.

/// A notice in `to`'s digest that mentions `needle`.
fn notice_to(event: &crossagents_core::Event, to: crossagents_core::AgentId, needle: &str) -> bool {
    event.kind.agent_id() == Some(to)
        && input_of(event)
            .is_some_and(|(text, from)| *from == MessageSource::Notice && text.contains(needle))
}

/// A watch `owner` set on `agent`, with how far its chain already went.
fn seeded_watch(
    owner: crossagents_core::AgentId,
    agent: crossagents_core::AgentId,
    chain: u32,
    created_at: chrono::DateTime<chrono::Utc>,
) -> crossagents_core::Watch {
    crossagents_core::Watch {
        id: format!("w-{chain}-{}", created_at.timestamp()),
        owner,
        target: crossagents_core::WatchTarget::Agent { agent_id: agent },
        on: vec![crossagents_core::WatchOn::Done],
        created_at,
        expires_at: None,
        chain,
        health: Default::default(),
    }
}

#[tokio::test]
async fn a_watch_never_wakes_its_owner_about_itself() {
    let t = TestHub::start().await;
    let alice = t.add_agent("alice", None).await;
    let bob = t.add_agent("bob", None).await;
    let mut w = t.watch().await;

    // Alice watching Bob hears about his turns...
    t.send("alice", r#"[call watch {"agent":"bob","on":["done"]}]"#)
        .await;
    w.until(
        "alice's turn ended",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == alice.id),
    )
    .await;
    t.send("bob", "one").await;
    w.until("alice hears about bob", |e| {
        notice_to(e, alice.id, "bob finished a turn")
    })
    .await;

    // ...but an agent watching itself does not: its own turn is not news it asked for.
    t.send("bob", r#"[call watch {"agent":"bob","on":["done"]}]"#)
        .await;
    w.until(
        "bob's turn ended",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == bob.id),
    )
    .await;
    t.send("bob", "two").await;
    w.until(
        "bob's turn ended again",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == bob.id),
    )
    .await;
    let heard = w
        .seen
        .iter()
        .filter(|e| notice_to(e, bob.id, "bob finished a turn"))
        .count();
    assert_eq!(heard, 0, "an agent was woken by its own turn");
}

#[tokio::test]
async fn a_chain_with_nobody_in_between_pauses_the_rule_and_tells_the_person() {
    let t = TestHub::start().await;
    let alice = t.add_agent("alice", None).await;
    let bob = t.add_agent("bob", None).await;
    let mut w = t.watch().await;
    let store = t.hub.bus.store().clone();
    // Two triggers already went by with nobody taking part; the next is the third.
    store
        .insert_watch(&seeded_watch(alice.id, bob.id, 2, chrono::Utc::now()))
        .await
        .unwrap();

    t.send("bob", "one").await;
    let paused = w
        .until("the person is told the rule paused", |e| {
            matches!(&e.kind, EventKind::HumanAsked { item }
                if item.kind == crossagents_core::HumanItemKind::Notice
                    && item.text.contains("已自动暂停"))
        })
        .await;
    if let EventKind::HumanAsked { item } = &paused.kind {
        assert!(
            item.text.contains(&alice.name),
            "the notice names the rule's owner: {}",
            item.text
        );
    }
    let watch = store.list_watches(None).await.unwrap().remove(0);
    assert!(watch.health.is_paused(), "{watch:?}");
    assert!(
        !w.seen
            .iter()
            .any(|e| notice_to(e, alice.id, "bob finished a turn")),
        "the third trigger woke the owner before pausing"
    );

    // Paused means paused: Bob's next turn wakes nobody.
    t.send("bob", "two").await;
    w.until(
        "bob's next turn",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == bob.id),
    )
    .await;
    assert!(
        !w.seen
            .iter()
            .any(|e| notice_to(e, alice.id, "bob finished a turn")),
        "a paused rule woke its owner again"
    );
}

#[tokio::test]
async fn a_person_speaking_to_the_owner_starts_the_chain_over() {
    let t = TestHub::start().await;
    let alice = t.add_agent("alice", None).await;
    let bob = t.add_agent("bob", None).await;
    let mut w = t.watch().await;
    let store = t.hub.bus.store().clone();
    store
        .insert_watch(&seeded_watch(alice.id, bob.id, 2, chrono::Utc::now()))
        .await
        .unwrap();

    // A person speaks to Alice: the chain she was on had human involvement, so it restarts.
    t.send("alice", "still there?").await;
    w.until(
        "alice answered the person",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == alice.id),
    )
    .await;
    let reset = store.list_watches(None).await.unwrap().remove(0);
    assert_eq!(reset.chain, 0, "{reset:?}");

    // The next trigger is a first one again, not a third.
    t.send("bob", "one").await;
    w.until("alice hears about bob", |e| {
        notice_to(e, alice.id, "bob finished a turn")
    })
    .await;
    let watch = store.list_watches(None).await.unwrap().remove(0);
    assert!(!watch.health.is_paused(), "{watch:?}");
    assert_eq!(watch.chain, 1, "{watch:?}");
}

#[tokio::test]
async fn a_waiting_watch_expires_after_seven_days() {
    let t = TestHub::start().await;
    let alice = t.add_agent("alice", None).await;
    let bob = t.add_agent("bob", None).await;
    let mut w = t.watch().await;
    let store = t.hub.bus.store().clone();
    store
        .insert_watch(&seeded_watch(
            alice.id,
            bob.id,
            0,
            chrono::Utc::now() - chrono::Duration::days(8),
        ))
        .await
        .unwrap();

    let expired = w
        .until("the wait ran out", |e| notice_to(e, alice.id, "expired"))
        .await;
    if let EventKind::AgentInput { text, .. } = &expired.kind {
        assert!(
            text.contains("did not happen") && text.contains("7 days"),
            "the wake says what did not happen: {text}"
        );
    }
    assert!(
        store.list_watches(None).await.unwrap().is_empty(),
        "an expired watch is closed, not left waiting"
    );
}

#[tokio::test]
async fn a_reminder_asking_for_more_than_a_week_is_cut_to_it() {
    let t = TestHub::start().await;
    let alice = t.add_agent("alice", None).await;
    let mut w = t.watch().await;
    let store = t.hub.bus.store().clone();

    // Twenty thousand minutes is almost fourteen days: the wait is cut to seven.
    t.send(
        "alice",
        r#"[call remind_set {"after_minutes":20000,"note":"far away"}]"#,
    )
    .await;
    w.until(
        "alice's turn ended",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == alice.id),
    )
    .await;
    let reminder = store.list_reminders(None).await.unwrap().remove(0);
    assert!(reminder.wait_expired, "{reminder:?}");
    let cut = reminder.created_at + chrono::Duration::days(7);
    assert!(
        (reminder.due_at - cut).num_seconds().abs() < 60,
        "the due time was not cut to the cap: {reminder:?}"
    );

    // When it comes due it says the thing waited for did not happen.
    let soon = crossagents_core::Reminder {
        due_at: chrono::Utc::now() - chrono::Duration::seconds(1),
        ..reminder
    };
    store.delete_reminder(&soon.id).await.unwrap();
    store.insert_reminder(&soon).await.unwrap();
    let fired = w
        .until("the cut reminder fired", |e| {
            notice_to(e, alice.id, "far away")
        })
        .await;
    if let EventKind::AgentInput { text, .. } = &fired.kind {
        assert!(
            text.contains("did not happen"),
            "a cut wait says so when it fires: {text}"
        );
    }
}
