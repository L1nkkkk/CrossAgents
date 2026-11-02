//! `POST /v1/agents/{key}/compact` answers when the summary is written, which takes minutes
//! for a long history. A caller that stops waiting (the GUI gave up after 30 s) must not
//! cancel the compaction halfway: the session then stayed "compacting" and every later
//! reply was hidden as if it were the summary.

mod common;

use std::time::Duration;

use common::{MOCK_AGENT, TestHub};
use crossagents_api_types::EventKind;
use crossagents_core::DriverKind;
use crossagents_plugins::PluginConfig;

#[tokio::test]
async fn a_caller_giving_up_does_not_leave_the_session_compacting() {
    let t = TestHub::start_with(|config| {
        config.plugins.insert(
            "native".to_owned(),
            PluginConfig {
                driver: Some(DriverKind::ClaudeNative),
                native_command: vec![
                    MOCK_AGENT.to_owned(),
                    "stream-json".to_owned(),
                    "--compact-ms".to_owned(),
                    "1500".to_owned(),
                ],
                models: vec!["mock-native".into()],
                ..PluginConfig::default()
            },
        );
    })
    .await;
    let cwd = tempfile::tempdir().unwrap();
    let agent = t.add_native("Nova", "worker", cwd.path()).await;

    // A first turn starts the session.
    let mut w = t.watch().await;
    t.send(&agent.name, "hello").await;
    w.until("first turn", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;

    // The caller stops waiting long before the summary is written.
    let gave_up = t
        .client
        .post(format!("{}/v1/agents/{}/compact", t.base, agent.name))
        .bearer_auth(&t.hub.token)
        .timeout(Duration::from_millis(200))
        .send()
        .await;
    assert!(gave_up.is_err(), "the compaction answered within 200 ms");
    // Long enough for the compaction to finish on its own.
    tokio::time::sleep(Duration::from_millis(2500)).await;

    let mut w = t.watch().await;
    t.send(&agent.name, "after compacting").await;
    w.until("the next turn", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;
    let output = w.output();
    assert!(
        output.contains("echo(model=mock-native): after compacting"),
        "{output}"
    );
    t.hub.shutdown();
}
