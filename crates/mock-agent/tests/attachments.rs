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

/// A query value with everything but letters and digits percent-encoded.
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Paths an agent names (an image a tool call showed, a path in its output) are read only
/// inside its working directory: `..`, absolute paths elsewhere, other drives and shares are
/// refused at every entry (docs/tasks/SELF.md B6), while attachments are read by id only.
#[tokio::test]
async fn paths_an_agent_names_stay_in_its_working_directory() {
    let t = TestHub::start().await;
    let outer = tempfile::tempdir().unwrap();
    let ws = outer.path().join("ws");
    std::fs::create_dir_all(ws.join("sub")).unwrap();
    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    std::fs::write(ws.join("shot.png"), PNG).unwrap();
    std::fs::write(ws.join("notes.txt"), "notes").unwrap();
    std::fs::write(outer.path().join("secret.png"), PNG).unwrap();
    std::fs::write(outer.path().join("secret.txt"), "secret").unwrap();
    t.add_agent_in("vera", Some("haiku"), "worker", &ws).await;

    let secret_txt = outer.path().join("secret.txt").display().to_string();
    let mut refused = vec![
        "../secret.png".to_owned(),
        "..\\secret.png".to_owned(),
        "sub/../../secret.png".to_owned(),
    ];
    if cfg!(windows) {
        refused.extend(
            [
                "\\\\no-such-host.invalid\\share\\x.png",
                "//no-such-host.invalid/share/x.png",
                "\\\\?\\UNC\\no-such-host.invalid\\share\\x.png",
                "\\\\.\\C:\\Windows\\x.png",
                "\\\\?\\GLOBALROOT\\Device\\x.png",
            ]
            .map(str::to_owned),
        );
    }

    // The image a tool call named: inside is read, everything else is 403.
    let ok = t
        .get(&format!(
            "/v1/agents/vera/files/image?path={}",
            encode("shot.png")
        ))
        .await;
    assert_eq!(ok.status(), 200, "{}", ok.text().await.unwrap());
    // An image may also be in the system temp directory (where tools leave screenshots, and
    // where this test's directories are), so the absolute path out is the executable's.
    let exe = std::env::current_exe().unwrap().display().to_string();
    let mut refused_image = refused.clone();
    refused_image.push(exe.clone());
    refused_image.push(exe.replace('\\', "/"));
    for path in &refused_image {
        let resp = t
            .get(&format!(
                "/v1/agents/vera/files/image?path={}",
                encode(path)
            ))
            .await;
        assert_eq!(resp.status(), 403, "{path}: {}", resp.text().await.unwrap());
    }

    // The file browser's reads: the same.
    let ok = t
        .get(&format!(
            "/v1/agents/vera/files/content?path={}",
            encode("notes.txt")
        ))
        .await;
    assert_eq!(ok.status(), 200, "{}", ok.text().await.unwrap());
    let mut refused_text: Vec<String> = refused.iter().map(|p| p.replace(".png", ".txt")).collect();
    refused_text.push(secret_txt.clone());
    refused_text.push(secret_txt.replace('\\', "/"));
    refused_text.push(exe);
    for path in &refused_text {
        let resp = t
            .get(&format!(
                "/v1/agents/vera/files/content?path={}",
                encode(path)
            ))
            .await;
        assert_eq!(resp.status(), 403, "{path}: {}", resp.text().await.unwrap());
        let resp = t
            .get(&format!("/v1/agents/vera/files/tree?path={}", encode(path)))
            .await;
        assert_eq!(resp.status(), 403, "{path}: {}", resp.text().await.unwrap());
    }

    // Paths in an agent's output are made links only inside; the rest answer nothing.
    let mut inputs = vec!["notes.txt".to_owned()];
    inputs.extend(refused_text.iter().cloned());
    let resolved: Value = t
        .post("/v1/agents/vera/files/resolve", json!({ "paths": inputs }))
        .await
        .json()
        .await
        .unwrap();
    let entries = resolved["entries"].as_array().unwrap();
    assert_eq!(entries.len(), inputs.len());
    assert_eq!(entries[0]["path"], "notes.txt", "{resolved}");
    for (entry, input) in entries.iter().zip(&inputs).skip(1) {
        assert!(entry["path"].is_null(), "{input}: {entry}");
        // The answer names the input as it was sent.
        assert_eq!(entry["input"], input.as_str());
    }

    // Attachments are read by id only: a path in its place is not found.
    for id in [
        "..%2Ftoken",
        "..%5Ctoken",
        "C%3A%5Cx",
        "%5C%5Cs%5Cshare%5Cx",
    ] {
        assert_eq!(
            t.get(&format!("/v1/agents/vera/attachments/{id}"))
                .await
                .status(),
            404,
            "{id}"
        );
    }
}

/// Files given when a run starts go to its lead with the kickoff.
#[tokio::test]
async fn files_given_with_a_run_reach_its_lead() {
    let t = TestHub::start().await;
    t.add_agent("boss", Some("haiku")).await;
    let mut w = t.watch().await;
    let resp = t
        .post(
            "/v1/runs",
            json!({
                "request": "Make it look like the screenshot.",
                "planner": "boss",
                "attachments": [{ "name": "mock.png", "mime": "image/png", "data": "iVBORw==" }],
            }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    w.until("kickoff answered", turn_completed).await;
    let output = w.output();
    assert!(
        output.contains("Make it look like the screenshot."),
        "{output}"
    );
    assert!(output.contains("[image image/png 4 bytes]"), "{output}");

    // The same checks as a message: a file messages cannot carry stops the run.
    let resp = t
        .post(
            "/v1/runs",
            json!({
                "request": "x",
                "planner": "boss",
                "attachments": [{ "name": "a.exe", "mime": "application/x-msdownload", "data": "TVo=" }],
            }),
        )
        .await;
    assert_eq!(resp.status(), 400);
}

/// A standalone chat is carried on from the start page: it is offered when no workspace is
/// picked (each chat lives in its own `<home>/chats/<id>`, so no workspace lists it), and the
/// message that carries it on takes files like any other.
#[tokio::test]
async fn a_standalone_chat_is_offered_and_carried_on_with_files() {
    let t = TestHub::start().await;
    // No cwd: a chat of its own.
    let resp = t
        .post(
            "/v1/agents",
            json!({ "name": "Cleo", "plugin": "mock", "model": "haiku", "role": "worker", "cwd": "" }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let chat: Value = resp.json().await.unwrap();
    // One that works in a workspace is not a standalone chat.
    t.add_agent("Wes", Some("haiku")).await;

    let listed: Value = t.get("/v1/session-candidates").await.json().await.unwrap();
    let names: Vec<&str> = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["agent"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Cleo"], "{listed}");
    assert_eq!(listed["sessions"][0]["reason"], Value::Null, "{listed}");

    let mut w = t.watch().await;
    let resp = t
        .post(
            &format!("/v1/agents/{}/chat", chat["id"].as_str().unwrap()),
            json!({
                "text": "about this file",
                "attachments": [{ "name": "notes.md", "mime": "text/markdown", "data": "bm90ZXM=" }],
            }),
        )
        .await;
    assert_eq!(resp.status(), 202, "{}", resp.text().await.unwrap());
    w.until("turn completed", turn_completed).await;
    let output = w.output();
    assert!(
        output.contains("<file name=\"notes.md\">\nnotes\n</file>"),
        "{output}"
    );
}
