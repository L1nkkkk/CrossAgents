//! One set of project rules for every vendor (docs/tasks/DOGFOOD.md §6), and the switch of
//! which Claude settings an agent loads.

mod common;

use common::{MOCK_AGENT, TestHub, git_repo};
use crossagents_api_types::{AgentStatus, EventKind};
use crossagents_core::DriverKind;
use crossagents_plugins::PluginConfig;
use serde_json::{Value, json};

#[tokio::test]
async fn project_rules_reach_a_codex_agent() {
    let repo = git_repo();
    std::fs::write(
        repo.path().join("CROSSAGENTS.md"),
        "Commit messages are in Chinese.",
    )
    .unwrap();
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
                    "cwd": repo.path().display().to_string() }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let mut w = t.watch().await;
    t.send("cx", "[instructions]").await;
    w.until("turn completed", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;
    let output = w.output();
    assert!(
        output.contains("Commit messages are in Chinese."),
        "{output}"
    );
    assert!(output.contains("CROSSAGENTS.md"), "{output}");
}

#[tokio::test]
async fn setting_sources_are_switched_per_agent() {
    let t = TestHub::start().await;
    t.add_agent("ada", Some("haiku")).await;
    let set = |sources: Value| {
        t.post(
            "/v1/agents/ada/settings",
            json!({ "setting_sources": sources }),
        )
    };
    let agent: Value = set(json!("user, project,local"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(agent["config"]["setting_sources"], "user,project,local");
    let agent: Value = set(json!("")).await.json().await.unwrap();
    assert_eq!(agent["config"]["setting_sources"], "");
    let agent: Value = set(Value::Null).await.json().await.unwrap();
    assert!(agent["config"].get("setting_sources").is_none(), "{agent}");
    assert_eq!(set(json!("everything")).await.status(), 400);
}

/// The CLI reads its settings at launch: changing them stops an idle session, and the next
/// message resumes it with the new ones.
#[tokio::test]
async fn switching_setting_sources_restarts_an_idle_session() {
    let t = TestHub::start().await;
    t.add_agent("ada", Some("haiku")).await;
    let mut w = t.watch().await;
    t.send("ada", "hello").await;
    w.until("idle", |e| {
        matches!(
            e.kind,
            EventKind::AgentStatusChanged {
                to: AgentStatus::Idle,
                ..
            }
        )
    })
    .await;
    let agent: Value = t.get("/v1/agents/ada").await.json().await.unwrap();
    assert_eq!(agent["status"], "idle", "{agent}");

    let agent: Value = t
        .post("/v1/agents/ada/settings", json!({ "setting_sources": "" }))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(agent["status"], "stopped", "{agent}");
    // The same setting again changes nothing and stops nothing.
    t.send("ada", "again").await;
    w.until("running again", |e| {
        matches!(
            e.kind,
            EventKind::AgentStatusChanged {
                to: AgentStatus::Idle,
                ..
            }
        )
    })
    .await;
    let agent: Value = t
        .post("/v1/agents/ada/settings", json!({ "setting_sources": "" }))
        .await
        .json()
        .await
        .unwrap();
    assert_ne!(agent["status"], "stopped", "{agent}");
}

#[tokio::test]
async fn permission_settings_have_three_states_and_restart() {
    let t = TestHub::start().await;
    t.add_agent("ada", Some("haiku")).await;
    let mut w = t.watch().await;
    t.send("ada", "hello").await;
    w.until("idle", |e| {
        matches!(
            e.kind,
            EventKind::AgentStatusChanged {
                to: AgentStatus::Idle,
                ..
            }
        )
    })
    .await;
    let agent: Value = t
        .post("/v1/agents/ada/settings", json!({"permission":"bypass"}))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(agent["config"]["permission"], "bypass");
    assert_eq!(agent["status"], "stopped");
    let agent: Value = t
        .post("/v1/agents/ada/settings", json!({}))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(agent["config"]["permission"], "bypass");
    let agent: Value = t
        .post("/v1/agents/ada/settings", json!({"permission":null}))
        .await
        .json()
        .await
        .unwrap();
    assert!(agent["config"].get("permission").is_none());
}
