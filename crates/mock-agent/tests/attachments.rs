//! Messages with files (docs/tasks/DOGFOOD.md §3): stored by hash, described in events and
//! the transcript, readable back, passed on to the agent (here an ACP mock, which takes
//! text only: the files arrive written into the prompt), and removed with the agent.

mod common;

use common::TestHub;
use crossagents_api_types::EventKind;
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

    // The agent got the text file inline and the image by path.
    let output = w.output();
    assert!(output.contains("look at these"), "{output}");
    assert!(
        output.contains("<file name=\"notes.md\">\nnotes\n</file>"),
        "{output}"
    );
    assert!(
        output.contains("[attached file: ui.png (image/png, 4 bytes) at "),
        "{output}"
    );

    let transcript: Value = t
        .get("/v1/agents/alice/transcript")
        .await
        .json()
        .await
        .unwrap();
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
