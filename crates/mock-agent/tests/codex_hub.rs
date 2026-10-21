//! A hub whose `codex` plugin runs on the `codex-native` driver (the mock app-server).

mod common;

use common::{MOCK_AGENT, TestHub};
use crossagents_api_types::EventKind;
use crossagents_core::DriverKind;
use crossagents_plugins::PluginConfig;
use serde_json::{Value, json};

#[tokio::test]
async fn codex_native_through_the_hub() {
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

    let plugins: Value = t.get("/v1/plugins").await.json().await.unwrap();
    let codex = plugins["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "codex")
        .unwrap();
    assert_eq!(codex["driver"], "codex-native");
    assert_eq!(codex["capabilities"]["resume"], true);
    assert_eq!(codex["capabilities"]["revert_files"], false);
    let mock = plugins["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "mock")
        .unwrap();
    assert_eq!(mock["driver"], "acp-generic");

    // The account's home becomes the app-server's CODEX_HOME (M5 isolation).
    let account_home = tempfile::tempdir().unwrap();
    let resp = t
        .post(
            "/v1/accounts",
            json!({ "id": "cx-acct", "provider": "codex", "home": account_home.path().display().to_string() }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let resp = t
        .post(
            "/v1/agents",
            json!({ "name": "cx", "plugin": "codex", "model": "mock-codex", "role": "worker",
                    "cwd": t.home.path().display().to_string(), "config": { "account": "cx-acct" } }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());

    let mut w = t.watch().await;
    t.send("cx", "hi [mcp] [env CODEX_HOME]").await;
    w.until("turn completed", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;
    let output = w.output();
    // `crossagents mcp` is mounted with its URL and token, tools approved.
    assert!(output.contains("mcp=crossagents:approve:2"), "{output}");
    assert!(
        output.contains(&format!("env CODEX_HOME={}", account_home.path().display())),
        "{output}"
    );
    assert!(
        output.contains("echo(model=mock-codex,effort=medium): hi"),
        "{output}"
    );
    // The thread id is recorded for usage attribution, and usage is reported.
    assert!(w.seen.iter().any(|e| matches!(&e.kind,
        EventKind::AgentSessionStarted { session_id, .. } if session_id.starts_with("thread-"))));
    assert!(
        w.seen
            .iter()
            .any(|e| matches!(&e.kind, EventKind::AgentUsage { used: 1100, .. }))
    );

    // An unknown driver is refused.
    let resp = t
        .post(
            "/v1/agents",
            json!({ "name": "bad", "plugin": "codex", "driver": "codex-turbo", "role": "worker",
                    "cwd": t.home.path().display().to_string() }),
        )
        .await;
    assert_eq!(resp.status(), 400);
    t.hub.shutdown();
}
