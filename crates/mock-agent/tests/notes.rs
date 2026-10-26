//! H1-6 (docs/tasks/H1.md): shared notes, usage, and agents that run outside the hub.

mod common;

use common::TestHub;
use crossagents_api_types::EventKind;
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

async fn turn(t: &TestHub, w: &mut common::Watcher, agent: &str, text: &str) {
    t.send(agent, text).await;
    w.until("turn completed", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;
}

#[tokio::test]
async fn team_notes_are_shared_and_only_authors_delete_them() {
    let t = TestHub::start().await;
    t.add_agent("alice", None).await;
    t.add_agent("bob", None).await;
    let mut w = t.watch().await;

    let write = call(
        "note_write",
        json!({ "key": "style/rust", "text": "Use thiserror for errors.", "scope": "team" }),
    );
    turn(&t, &mut w, "alice", &write).await;
    turn(
        &t,
        &mut w,
        "bob",
        &call("note_read", json!({ "key": "style/rust" })),
    )
    .await;
    assert!(
        w.output().contains("Use thiserror for errors."),
        "{}",
        w.output()
    );
    turn(
        &t,
        &mut w,
        "bob",
        &call("note_list", json!({ "prefix": "style/" })),
    )
    .await;
    assert!(
        w.output().contains("\"key\": \"style/rust\""),
        "{}",
        w.output()
    );
    // Bob did not write it and leads nothing.
    turn(
        &t,
        &mut w,
        "bob",
        &call(
            "note_write",
            json!({ "key": "style/rust", "text": null, "scope": "team" }),
        ),
    )
    .await;
    assert!(
        w.output().contains("only the note's author"),
        "{}",
        w.output()
    );
    turn(
        &t,
        &mut w,
        "alice",
        &call(
            "note_write",
            json!({ "key": "style/rust", "text": null, "scope": "team" }),
        ),
    )
    .await;
    let notes: Value = t.get("/v1/notes?scope=team").await.json().await.unwrap();
    assert!(notes["notes"].as_array().unwrap().is_empty(), "{notes}");
    // Every write and the delete are on record.
    let writes = w
        .seen
        .iter()
        .filter(|e| matches!(e.kind, EventKind::NoteWritten { .. }))
        .count();
    assert_eq!(writes, 2);
}

#[tokio::test]
async fn a_message_to_an_unknown_name_says_who_is_there() {
    let t = TestHub::start().await;
    t.add_agent("alice", None).await;
    t.add_agent("bob", None).await;
    let mut w = t.watch().await;
    turn(
        &t,
        &mut w,
        "alice",
        &call("agent_send", json!({ "to": "robert", "text": "hi" })),
    )
    .await;
    let out = w.output();
    assert!(
        out.contains("teammates now: bob") && out.contains("agent_list"),
        "{out}"
    );
}

#[tokio::test]
async fn usage_reports_spend_and_accounts() {
    let t = TestHub::start().await;
    t.add_agent("alice", None).await;
    let mut w = t.watch().await;
    turn(&t, &mut w, "alice", "warm up").await;
    turn(&t, &mut w, "alice", &call("usage_get", json!({}))).await;
    let out = w.output();
    assert!(
        out.contains("\"by_agent\"") && out.contains("\"accounts\""),
        "{out}"
    );
}

#[tokio::test]
async fn an_external_agent_connects_and_reads_its_inbox() {
    let t = TestHub::start().await;
    t.add_agent("alice", None).await;
    let resp = t
        .post(
            "/v1/agents",
            json!({ "name": "outside", "plugin": "external", "role": "worker",
                    "cwd": t.home.path().display().to_string(),
                    "profile": { "display_name": "Ext" } }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let info: Value = t
        .post("/v1/agents/outside/connect", json!({}))
        .await
        .json()
        .await
        .unwrap();
    let token = info["token"].as_str().unwrap().to_owned();
    assert!(info["json_config"].as_str().unwrap().contains("mcpServers"));
    assert!(
        info["toml_config"]
            .as_str()
            .unwrap()
            .contains("[mcp_servers.crossagents]")
    );
    assert!(info["toml_config"].as_str().unwrap().contains(&token));

    // A teammate messages it; the hub keeps the message until it asks.
    let mut w = t.watch().await;
    turn(
        &t,
        &mut w,
        "alice",
        &call(
            "agent_send",
            json!({ "to": "outside", "text": "hello from inside" }),
        ),
    )
    .await;
    let tool = |name: &'static str| {
        let client = t.client.clone();
        let url = format!("{}/v1/me/tools/{name}", t.base);
        let token = token.clone();
        async move {
            client
                .post(url)
                .bearer_auth(token)
                .json(&json!({}))
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };
    // Delivery is in the background; give it a moment.
    let mut inbox = Value::Null;
    for _ in 0..50 {
        inbox = tool("inbox_read").await;
        if !inbox["messages"].as_array().is_some_and(|m| m.is_empty()) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let messages = inbox["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1, "{inbox}");
    assert!(
        messages[0]["text"]
            .as_str()
            .unwrap()
            .contains("hello from inside")
    );
    assert_eq!(messages[0]["from"]["kind"], "agent");
    // Read once.
    assert!(
        tool("inbox_read").await["messages"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // It can use the same tools as everyone.
    let me = tool("whoami").await;
    assert_eq!(me["name"], "outside");
    // The hub never tries to start it.
    let start = t.post("/v1/agents/outside/start", json!({})).await;
    assert_eq!(start.status(), 409);
}
