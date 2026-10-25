//! Messages with files (docs/tasks/DOGFOOD.md §3, docs/TODO.md §7): stored by hash,
//! described in events and the transcript, readable back, passed on to the agent (here an
//! ACP mock: text inline, images as image blocks when it declares them), refused with the
//! reason when the agent does not take the kind, and removed with the agent.

mod common;

use common::{MOCK_AGENT, TestHub};
use crossagents_api_types::EventKind;
use crossagents_plugins::PluginConfig;
use serde_json::{Value, json};

fn turn_completed(e: &crossagents_api_types::Event) -> bool {
    matches!(e.kind, EventKind::AgentTurnCompleted { .. })
}

#[tokio::test]
async fn files_travel_with_a_message() {
    let t = TestHub::start().await;
    t.add_agent("alice", Some("haiku")).await;
    let mut w = t.watch().await;

    // "notes" and "\x89PNG", base64.
    let resp = t
        .post(
            "/v1/agents/alice/messages",
            json!({
                "text": "look at these",
                "attachments": [
                    { "name": "notes.md", "mime": "text/markdown", "data": "bm90ZXM=" },
                    { "name": "C:\\shots\\ui.png", "mime": "image/png", "data": "iVBORw==" },
                ],
            }),
        )
        .await;
    assert_eq!(resp.status(), 202, "{}", resp.text().await.unwrap());
    w.until("turn completed", turn_completed).await;

    // The agent got the text file inline and the image as an image block.
    let output = w.output();
    assert!(output.contains("look at these"), "{output}");
    assert!(
        output.contains("<file name=\"notes.md\">\nnotes\n</file>"),
        "{output}"
    );
    assert!(output.contains("[image image/png 4 bytes]"), "{output}");

    let transcript: Value = t
        .get("/v1/agents/alice/transcript")
        .await
        .json()
        .await
        .unwrap();
    // The ACP mock declares images, and says nothing about documents.
    assert_eq!(transcript["driver"], "acp-generic");
    assert_eq!(
        transcript["capabilities"]["attachments"],
        json!({ "image": true, "pdf": false, "text": true })
    );
    let files = transcript["turns"][0]["attachments"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[1]["name"], "ui.png");
    let id = files[1]["id"].as_str().unwrap();
    let path = std::path::PathBuf::from(files[1]["path"].as_str().unwrap());
    assert!(
        path.starts_with(t.home.path().join("attachments")),
        "{path:?}"
    );

    let content: Value = t
        .get(&format!("/v1/agents/alice/attachments/{id}"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(content["data"], "iVBORw==");
    assert_eq!(content["mime"], "image/png");
    assert_eq!(
        t.get("/v1/agents/alice/attachments/..%2Ftoken")
            .await
            .status(),
        404
    );

    // Refused types and sizes say why.
    let resp = t
        .post(
            "/v1/agents/alice/messages",
            json!({ "text": "x", "attachments": [{ "name": "a.exe", "mime": "application/x-msdownload", "data": "TVo=" }] }),
        )
        .await;
    assert_eq!(resp.status(), 400);
    assert!(resp.text().await.unwrap().contains("cannot be sent"));

    // Removing the agent takes its files with it.
    assert!(path.exists());
    assert_eq!(t.delete("/v1/agents/alice").await.status(), 204);
    assert!(!path.parent().unwrap().exists(), "{path:?}");
}

/// An ACP agent that declares no image support: images are refused with the reason before
/// anything is stored, while text still goes (docs/TODO.md §7).
#[tokio::test]
async fn what_the_agent_does_not_take_is_refused_not_dropped() {
    let t = TestHub::start_with(|config| {
        config.plugins.insert(
            "mock-text".into(),
            PluginConfig {
                command: vec![MOCK_AGENT.to_owned(), "--no-images".into()],
                models: vec!["mock-opus".into()],
                model_env: None,
                ..PluginConfig::default()
            },
        );
    })
    .await;
    let resp = t
        .post(
            "/v1/agents",
            json!({
                "name": "bob", "plugin": "mock-text", "model": "mock-opus",
                "role": "worker", "cwd": t.home.path().display().to_string(),
            }),
        )
        .await;
    assert_eq!(resp.status(), 201);
    let agent: Value = resp.json().await.unwrap();

    // Before it has said anything the hub does not know what it takes.
    let transcript: Value = t
        .get("/v1/agents/bob/transcript")
        .await
        .json()
        .await
        .unwrap();
    assert!(transcript["capabilities"]["attachments"].is_null());

    // A text message starts it; it declares text only.
    let mut w = t.watch().await;
    assert_eq!(
        t.post("/v1/agents/bob/messages", json!({ "text": "hello" }))
            .await
            .status(),
        202
    );
    w.until("turn completed", turn_completed).await;
    let transcript: Value = t
        .get("/v1/agents/bob/transcript")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        transcript["capabilities"]["attachments"],
        json!({ "image": false, "pdf": false, "text": true })
    );

    // An image is refused with a reason naming the file, the kind and the driver.
    let resp = t
        .post(
            "/v1/agents/bob/messages",
            json!({
                "text": "and this",
                "attachments": [{ "name": "ui.png", "mime": "image/png", "data": "iVBORw==" }],
            }),
        )
        .await;
    assert_eq!(resp.status(), 400);
    let refused = resp.text().await.unwrap();
    assert!(refused.contains("ui.png"), "{refused}");
    assert!(refused.contains("images"), "{refused}");
    assert!(refused.contains("acp-generic"), "{refused}");
    assert!(refused.contains("text"), "{refused}");
    // Nothing was stored for the refused message.
    let dir = t
        .home
        .path()
        .join("attachments")
        .join(agent["id"].as_str().unwrap());
    assert!(!dir.exists(), "{dir:?}");
    // A PDF is not declared either.
    let resp = t
        .post(
            "/v1/agents/bob/messages",
            json!({
                "text": "also this",
                "attachments": [{ "name": "spec.pdf", "mime": "application/pdf", "data": "JVBERg==" }],
            }),
        )
        .await;
    assert_eq!(resp.status(), 400);
    assert!(resp.text().await.unwrap().contains("PDFs"));
}
