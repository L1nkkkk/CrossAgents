//! Stream rules (docs/tasks/STREAM-RULES.md) end to end: a rule file in the workspace, an
//! agent whose output matches it, and the rule reaching the turn the agent is in.

mod common;

use std::path::Path;

use common::{TestHub, Watcher, native_plugin_with};
use crossagents_api_types::{Event, EventKind};
use crossagents_core::StopReason;
use serde_json::Value;

/// A rule file under the workspace's `.crossagents/rules`.
fn write_rule(cwd: &Path, file: &str, text: &str) {
    let dir = cwd.join(".crossagents").join("rules");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(file), text).unwrap();
}

fn steered_with(e: &Event, needle: &str) -> bool {
    matches!(&e.kind, EventKind::AgentSteered { text, .. } if text.contains(needle))
}

fn turn_completed(e: &Event) -> bool {
    matches!(e.kind, EventKind::AgentTurnCompleted { .. })
}

/// Turns started before each steered rule, in the order the watcher saw them.
fn steers_by_turn(w: &Watcher, needle: &str) -> Vec<usize> {
    let mut turns = 0;
    let mut out = Vec::new();
    for e in &w.seen {
        match &e.kind {
            EventKind::AgentTurnStarted { .. } => turns += 1,
            EventKind::AgentSteered { text, .. } if text.contains(needle) => out.push(turns),
            _ => {}
        }
    }
    out
}

/// A rule matching the mock's "sleeping" output reaches the turn it matched in: steered
/// into it, marked as a rule, and read by the model at the turn's boundary. A rule file
/// that does not parse is skipped without stopping the session.
#[tokio::test]
async fn a_matching_output_steers_the_rule_into_the_turn() {
    let t = TestHub::start_with(native_plugin_with(&["--steer-merge"])).await;
    let cwd = tempfile::tempdir().unwrap();
    write_rule(
        cwd.path(),
        "pause.md",
        "---\nmatch: sleeping\n---\nRULE PINEAPPLE: finish the file first.\n",
    );
    write_rule(cwd.path(), "broken.md", "---\nmatch: (\n---\nNever sent.\n");
    let agent = t.add_native("Nova", "worker", cwd.path()).await;
    let mut w = t.watch().await;

    t.send(&agent.id.to_string(), "first [sleep 1500]").await;
    let steered = w
        .until("the rule steered", |e| steered_with(e, "PINEAPPLE"))
        .await;
    let EventKind::AgentSteered { text, .. } = steered.kind else {
        unreachable!()
    };
    assert!(text.contains("CrossAgents rule `pause`"), "{text}");
    w.until("the turn completed", turn_completed).await;

    // The model read the rule in the turn, and it was not queued as a turn of its own.
    let output = w.output();
    assert!(
        output.contains("[steered]") && output.contains("PINEAPPLE"),
        "{output}"
    );
    assert!(
        !w.seen.iter().any(|e| matches!(&e.kind,
            EventKind::AgentInput { text, .. } if text.contains("PINEAPPLE"))),
        "the rule was queued, not steered"
    );
    assert!(!output.contains("Never sent"), "{output}");

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
    t.hub.shutdown();
}

/// Within 20 turns a rule goes once: it is steered in turn 1 and again only in turn 21.
#[tokio::test]
async fn a_rule_goes_once_in_twenty_turns() {
    let t = TestHub::start_with(native_plugin_with(&["--steer-merge"])).await;
    let cwd = tempfile::tempdir().unwrap();
    write_rule(
        cwd.path(),
        "pause.md",
        "---\nmatch: sleeping\n---\nRULE PINEAPPLE: finish the file first.\n",
    );
    let agent = t.add_native("Nova", "worker", cwd.path()).await;
    let mut w = t.watch().await;

    for turn in 1..=21 {
        t.send(&agent.id.to_string(), &format!("turn {turn} [sleep 200]"))
            .await;
        w.until(&format!("turn {turn} completed"), turn_completed)
            .await;
    }

    assert_eq!(
        steers_by_turn(&w, "PINEAPPLE"),
        [1, 21],
        "steered in turns 1 and 21"
    );
    t.hub.shutdown();
}

/// Compacting the context lets a rule go again, even inside the twenty turns.
#[tokio::test]
async fn compaction_lets_the_rule_go_again() {
    let t = TestHub::start_with(native_plugin_with(&["--steer-merge"])).await;
    let cwd = tempfile::tempdir().unwrap();
    write_rule(
        cwd.path(),
        "pause.md",
        "---\nmatch: sleeping\n---\nRULE PINEAPPLE: finish the file first.\n",
    );
    let agent = t.add_native("Nova", "worker", cwd.path()).await;
    let mut w = t.watch().await;

    t.send(&agent.id.to_string(), "first [sleep 200]").await;
    w.until("the first turn completed", turn_completed).await;
    let resp = t
        .post(
            &format!("/v1/agents/{}/compact", agent.id),
            serde_json::json!({}),
        )
        .await;
    assert!(resp.status().is_success(), "{}", resp.text().await.unwrap());

    t.send(&agent.id.to_string(), "second [sleep 200]").await;
    w.until("the second turn completed", turn_completed).await;
    assert_eq!(
        steers_by_turn(&w, "PINEAPPLE"),
        [1, 2],
        "sent again after compaction"
    );
    t.hub.shutdown();
}

/// A driver that cannot steer is interrupted, and the rule runs next as a message of its
/// own, ahead of what waits.
#[tokio::test]
async fn a_driver_that_cannot_steer_is_interrupted_and_the_rule_runs_next() {
    let t = TestHub::start().await;
    let cwd = tempfile::tempdir().unwrap();
    write_rule(
        cwd.path(),
        "pear.md",
        "---\nid: pear-rule\nmatch: working\n---\nRULE PEAR: keep going on the next step.\n",
    );
    let agent = t.add_agent_in("Otto", None, "worker", cwd.path()).await;
    let mut w = t.watch().await;

    t.send(&agent.id.to_string(), "first [slow]").await;
    w.until(
        "the agent is working",
        |e| matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains("working")),
    )
    .await;

    // `find`: the rule's input may already have been seen while waiting for the output.
    let input = w
        .find("the rule queued ahead", |e| {
            matches!(&e.kind, EventKind::AgentInput { text, queued: true, from: Some(_), .. }
                if text.contains("PEAR"))
        })
        .await;
    let EventKind::AgentInput { from, .. } = input.kind else {
        unreachable!()
    };
    assert_eq!(
        from,
        Some(crossagents_core::MessageSource::Notice),
        "a rule is a notice"
    );

    // The first turn is cut short, and the rule is what the agent reads next.
    w.find("the first turn interrupted", |e| {
        matches!(
            e.kind,
            EventKind::AgentTurnCompleted {
                stop_reason: StopReason::Cancelled,
                ..
            }
        )
    })
    .await;
    w.find(
        "the rule answered",
        |e| matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains("RULE PEAR")),
    )
    .await;
    w.find("the rule's turn completed", |e| {
        matches!(
            e.kind,
            EventKind::AgentTurnCompleted {
                stop_reason: StopReason::EndTurn,
                ..
            }
        )
    })
    .await;

    assert!(
        !w.seen
            .iter()
            .any(|e| matches!(e.kind, EventKind::AgentSteered { .. })),
        "nothing was steered"
    );
    t.hub.shutdown();
}
