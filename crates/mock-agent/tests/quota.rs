//! Quota-aware workers (docs/tasks/DOGFOOD.md §5): a worker whose account hits its limit
//! mid-task hands the task to a worker on another account of the same vendor, and the run
//! says so. Codex here is the mock app-server; `[limit 100]` makes it report the limit.

mod common;

use common::{MOCK_AGENT, TestHub, git_repo};
use crossagents_api_types::EventKind;
use crossagents_core::DriverKind;
use crossagents_plugins::PluginConfig;
use serde_json::{Value, json};

#[tokio::test]
async fn a_rate_limited_worker_hands_its_task_to_another_account() {
    let repo = git_repo();
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
    let homes = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    for (id, home) in ["codex-a", "codex-b"].iter().zip(&homes) {
        let resp = t
            .post(
                "/v1/accounts",
                json!({ "id": id, "provider": "codex", "home": home.path().display().to_string() }),
            )
            .await;
        assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    }
    t.add_agent_in("boss", Some("opus"), "planner", repo.path())
        .await;
    let resp = t
        .post(
            "/v1/agents",
            json!({ "name": "coder", "plugin": "codex", "model": "mock-codex", "role": "worker",
                    "cwd": repo.path().display().to_string(), "config": { "account": "codex-a" } }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let mut w = t.watch().await;

    let plan =
        json!([{ "key": "a", "title": "task a", "spec": { "goal": "[limit 100] work on it" } }]);
    let req = format!("Build it. [call plan_submit {}]", json!({ "tasks": plan }));
    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": req, "planner": "boss", "worker": "coder", "policy": { "plan_approval": "auto" } }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());

    let moved = w
        .until("task rerouted", |e| {
            matches!(e.kind, EventKind::TaskRerouted { .. })
        })
        .await;
    let EventKind::TaskRerouted {
        key, to, reason, ..
    } = moved.kind
    else {
        unreachable!()
    };
    assert_eq!(key, "a");
    assert!(
        reason.contains("codex-a") && reason.contains("codex-b"),
        "{reason}"
    );
    let worker: Value = t
        .get(&format!("/v1/agents/{to}"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(worker["config"]["account"], "codex-b", "{worker}");
    assert_eq!(worker["plugin"], "codex");
    // Not an escalation: the new session keeps the plain worker name.
    assert!(
        worker["name"].as_str().unwrap().starts_with("w-"),
        "{worker}"
    );

    // The run says why: on the task's card.
    let runs: Value = t.get("/v1/runs").await.json().await.unwrap();
    let id = runs["runs"][0]["id"].as_str().unwrap();
    let detail: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    let progress = detail["tasks"][0]["progress"].as_str().unwrap_or_default();
    assert!(progress.contains("codex-b"), "{detail}");
}
