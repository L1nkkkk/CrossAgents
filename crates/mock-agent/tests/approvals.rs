//! Guarded agents (docs/tasks/DOGFOOD.md §7): safe commands go ahead, dangerous ones wait
//! for the person's answer in 需要你. Codex here is the mock app-server; `[tool CMD]` makes it
//! ask about CMD.

mod common;

use common::{MOCK_AGENT, TestHub};
use crossagents_api_types::EventKind;
use crossagents_core::DriverKind;
use crossagents_plugins::PluginConfig;
use serde_json::{Value, json};

fn turn_completed(e: &crossagents_api_types::Event) -> bool {
    matches!(e.kind, EventKind::AgentTurnCompleted { .. })
}

#[tokio::test]
async fn dangerous_commands_wait_for_the_person() {
    let t = TestHub::start_with(|config| {
        config.plugins.insert(
            "codex".into(),
            PluginConfig {
                driver: Some(DriverKind::CodexNative),
                native_command: vec![MOCK_AGENT.into(), "app-server".into()],
                ..PluginConfig::default()
            },
        );
    })
    .await;
    let resp = t
        .post(
            "/v1/agents",
            json!({ "name": "cx", "plugin": "codex", "model": "mock-codex", "role": "worker",
                    "cwd": t.home.path().display().to_string(), "config": { "permission": "guard" } }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let mut w = t.watch().await;

    // Safe: allowed at once, nobody asked.
    t.send("cx", "[tool cargo test --workspace]").await;
    w.until("turn completed", turn_completed).await;
    assert!(w.output().contains("permission=accept"), "{}", w.output());
    assert!(
        !w.seen
            .iter()
            .any(|e| matches!(e.kind, EventKind::HumanAsked { .. }))
    );

    // Dangerous: the person is asked, and their answer decides.
    t.send("cx", "[tool git push origin main]").await;
    let asked = w
        .until("approval asked", |e| {
            matches!(e.kind, EventKind::HumanAsked { .. })
        })
        .await;
    let EventKind::HumanAsked { item } = asked.kind else {
        unreachable!()
    };
    assert!(item.text.contains("git push origin main"), "{}", item.text);
    assert_eq!(item.options, ["允许", "拒绝"]);
    let resp = t
        .post(
            &format!("/v1/questions/{}/answer", item.id),
            json!({ "answer": "拒绝" }),
        )
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    while w.output().matches("permission=").count() < 2 {
        w.until("turn completed", turn_completed).await;
    }
    assert!(
        last_permission(&w.output()).starts_with("decline"),
        "{}",
        w.output()
    );

    // Allowing goes ahead.
    t.send("cx", "[tool npm publish]").await;
    let asked = w
        .until("approval asked", |e| {
            matches!(&e.kind, EventKind::HumanAsked { item } if item.text.contains("npm publish"))
        })
        .await;
    let EventKind::HumanAsked { item } = asked.kind else {
        unreachable!()
    };
    let answered: Value = t
        .post(
            &format!("/v1/questions/{}/answer", item.id),
            json!({ "answer": "允许" }),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(answered["status"], "answered");
    while w.output().matches("permission=").count() < 3 {
        w.until("turn completed", turn_completed).await;
    }
    assert!(
        last_permission(&w.output()).starts_with("accept"),
        "{}",
        w.output()
    );
}

/// What the latest `permission=` said.
fn last_permission(output: &str) -> &str {
    output.rsplit("permission=").next().unwrap_or_default()
}
