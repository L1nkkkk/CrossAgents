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

/// The agent compacting its own context (Claude Code's `compact_boundary`) lets a rule go
/// again: what the rule said is no longer in the context. The turn that compacts keeps what
/// it already sent, so the rule is not handed in twice in that turn.
#[tokio::test]
async fn the_agents_own_compaction_lets_the_rule_go_again() {
    let t = TestHub::start_with(native_plugin_with(&["--steer-merge"])).await;
    let cwd = tempfile::tempdir().unwrap();
    write_rule(
        cwd.path(),
        "pause.md",
        "---\nmatch: sleeping\n---\nRULE PINEAPPLE: finish the file first.\n",
    );
    let agent = t.add_native("Nova", "worker", cwd.path()).await;
    let mut w = t.watch().await;

    // Turn 1 sends the rule, then the agent compacts by itself in the same turn: the output
    // after the notice matches again, but the rule is not handed in twice in one turn.
    t.send(&agent.id.to_string(), "first [sleep 200] [compact]")
        .await;
    w.until("the first turn completed", turn_completed).await;
    assert_eq!(
        steers_by_turn(&w, "PINEAPPLE"),
        [1],
        "one send a turn, compaction or not"
    );

    // Turn 2 matches again: the compaction lets it go, well inside the twenty turns.
    t.send(&agent.id.to_string(), "second [sleep 200]").await;
    w.until("the second turn completed", turn_completed).await;
    assert_eq!(
        steers_by_turn(&w, "PINEAPPLE"),
        [1, 2],
        "sent again after the agent's own compaction"
    );
    t.hub.shutdown();
}

/// An enabled extension's rule reaches the session as a project rule would, under the
/// extension's name; a disabled extension contributes nothing.
#[tokio::test]
async fn an_enabled_extensions_rule_reaches_the_session() {
    let home = tempfile::tempdir().unwrap();
    let fingerprint = "a".repeat(64);
    let mut records = serde_json::Map::new();
    for (name, enabled, marker) in [
        ("standup", true, "RULE LEMON: poll instead of sleeping."),
        ("quiet", false, "RULE QUIET: never sent."),
    ] {
        let dir = home
            .path()
            .join("extension-versions")
            .join(name)
            .join(&fingerprint);
        std::fs::create_dir_all(dir.join("rules")).unwrap();
        std::fs::write(
            dir.join("extension.toml"),
            format!(
                "name = \"{name}\"\napi = 1\naccess = \"read\"\n\n[[rules]]\nfile = \"rules/one.md\"\n"
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("rules/one.md"),
            format!("---\nid: no-sleep\nmatch: sleeping\n---\n{marker}\n"),
        )
        .unwrap();
        records.insert(
            name.to_owned(),
            serde_json::json!({ "enabled": enabled, "current": fingerprint }),
        );
    }
    std::fs::write(
        home.path().join("extensions.json"),
        serde_json::Value::Object(records).to_string(),
    )
    .unwrap();
    let t = TestHub::start_in(home, native_plugin_with(&["--steer-merge"])).await;
    let cwd = tempfile::tempdir().unwrap();
    let agent = t.add_native("Nova", "worker", cwd.path()).await;
    let mut w = t.watch().await;

    t.send(&agent.id.to_string(), "first [sleep 200]").await;
    let steered = w
        .until("the rule steered", |e| steered_with(e, "LEMON"))
        .await;
    let EventKind::AgentSteered { text, .. } = steered.kind else {
        unreachable!()
    };
    assert!(
        text.contains("CrossAgents rule `standup/no-sleep`"),
        "{text}"
    );
    w.until("the turn completed", turn_completed).await;
    assert!(
        !w.output().contains("QUIET"),
        "the disabled extension's rule is absent: {}",
        w.output()
    );
    t.hub.shutdown();
}

/// Handing a rule to the driver runs beside the event pump: while a slow steer is in
/// flight, what the agent writes is still forwarded, and only then is the rule reported.
/// With the pump waiting for the steer, the rule would be published first.
#[tokio::test]
async fn a_slow_steer_does_not_hold_back_the_output() {
    let t = TestHub::start_with(|config| {
        config.plugins.insert(
            "codex".into(),
            crossagents_plugins::PluginConfig {
                driver: Some(crossagents_core::DriverKind::CodexNative),
                native_command: vec![
                    common::MOCK_AGENT.into(),
                    "app-server".into(),
                    "--slow-steer".into(),
                    "1200".into(),
                ],
                ..crossagents_plugins::PluginConfig::default()
            },
        );
    })
    .await;
    let cwd = tempfile::tempdir().unwrap();
    write_rule(
        cwd.path(),
        "wait.md",
        "---\nmatch: working\n---\nRULE PLUM: stop and check the file.\n",
    );
    let resp = t
        .post(
            "/v1/agents",
            serde_json::json!({
                "name": "Ada", "plugin": "codex", "model": "mock-codex",
                "role": "worker", "cwd": cwd.path().display().to_string(),
            }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let agent: crossagents_api_types::Agent = resp.json().await.unwrap();
    let mut w = t.watch().await;

    t.send(&agent.id.to_string(), "first [slow]").await;
    w.until(
        "the rule was taken in",
        |e| matches!(&e.kind, EventKind::AgentSteered { text, .. } if text.contains("PLUM")),
    )
    .await;

    let output_at = w
        .seen
        .iter()
        .position(|e| matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains("still working")))
        .expect("the output written while the steer was pending");
    let steered_at = w
        .seen
        .iter()
        .position(
            |e| matches!(&e.kind, EventKind::AgentSteered { text, .. } if text.contains("PLUM")),
        )
        .unwrap();
    assert!(
        output_at < steered_at,
        "output was forwarded before the rule was reported"
    );
    t.hub.shutdown();
}
