//! `GET /v1/agents/{key}/commands`: the slash commands and skills the agent's own CLI
//! brings, which Claude Code's `system/init` names (docs/DESIGN.md §11).

mod common;

use common::{TestHub, native_plugin};
use crossagents_api_types::EventKind;
use serde_json::{Value, json};

#[tokio::test]
async fn an_agent_lists_the_commands_and_skills_its_cli_brings() {
    let t = TestHub::start_with(native_plugin).await;
    let cwd = tempfile::tempdir().unwrap();
    let agent = t.add_native("Nova", "worker", cwd.path()).await;
    let path = format!("/v1/agents/{}/commands", agent.name);

    // Nothing has started: the hub says it does not know, and why.
    let resp = t.get(&path).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["known"], false);
    assert_eq!(body["commands"], json!([]));
    assert!(
        body["note"].as_str().unwrap().contains("还没启动过会话"),
        "{body}"
    );

    // The first turn starts the CLI, whose init names its own commands and skills.
    let mut w = t.watch().await;
    t.send(&agent.name, "hello").await;
    w.until("the turn's reply", |e| {
        matches!(&e.kind, EventKind::AgentOutput { .. })
    })
    .await;

    let body: Value = t.get(&path).await.json().await.unwrap();
    assert_eq!(body["known"], true, "{body}");
    assert!(body.get("note").is_none(), "{body}");
    let listed: Vec<(&str, &str)> = body["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["name"].as_str().unwrap(), c["kind"].as_str().unwrap()))
        .collect();
    assert_eq!(
        listed,
        [
            ("compact", "command"),
            ("clear", "command"),
            ("review", "command"),
            ("mock-skill", "skill"),
        ]
    );
}
