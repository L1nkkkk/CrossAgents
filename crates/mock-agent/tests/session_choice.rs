//! Session choices through a real hub and SQLite.
mod common;
use common::{TestHub, git_repo, native_plugin, pinned_plugin};
use crossagents_api_types::EventKind;
use serde_json::{Value, json};

async fn run(t: &TestHub, body: Value) -> Value {
    let r = t.post("/v1/runs", body).await;
    assert_eq!(r.status(), 201, "{}", r.text().await.unwrap());
    r.json().await.unwrap()
}

#[tokio::test]
async fn default_and_explicit_fresh_and_busy_chat() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    let a = t.add_native("Pia", "planner", repo.path()).await;
    let mut watch = t.watch().await;
    t.send("Pia", "[session]").await;
    watch
        .until(
            "first turn",
            |e| matches!(e.kind, EventKind::AgentTurnCompleted { id, .. } if id == a.id),
        )
        .await;
    let first = watch.output();
    let session = first
        .split("session=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    let r = run(&t, json!({"planner":"Pia", "request":"[session]"})).await;
    assert_eq!(r["planner"], json!(a.id));
    watch
        .until(
            "run turn",
            |e| matches!(e.kind, EventKind::AgentTurnCompleted { id, .. } if id == a.id),
        )
        .await;
    assert!(
        watch
            .output()
            .contains(&format!("session={session} (resumed)"))
    );
    let busy = t
        .post(
            "/v1/runs",
            json!({"planner":"Pia", "request":"other", "fresh":false}),
        )
        .await;
    assert_eq!(busy.status(), 409);
    let message = busy.text().await.unwrap();
    assert!(
        message.contains("busy") && message.contains(r["id"].as_str().unwrap()),
        "{message}"
    );
    let chat = t.post("/v1/agents/Pia/chat", json!({"text":"hello"})).await;
    assert_eq!(chat.status(), 409);
    let copy = run(&t, json!({"planner":"Pia", "request":"new", "fresh":true})).await;
    assert_ne!(copy["planner"], json!(a.id));
}

#[tokio::test]
async fn explicit_fresh_on_idle_and_chat_continues() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    let a = t.add_native("Pia", "planner", repo.path()).await;
    let chat = t
        .post("/v1/agents/Pia/chat", json!({"text":"new topic"}))
        .await;
    assert_eq!(chat.status(), 202, "{}", chat.text().await.unwrap());
    assert_eq!(t.agent("Pia").await.topic.as_deref(), Some("new topic"));
    let copy = run(&t, json!({"planner":"Pia", "request":"new", "fresh":true})).await;
    assert_ne!(copy["planner"], json!(a.id));
}

#[tokio::test]
async fn archived_requires_restore() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    let a = t.add_native("Pia", "planner", repo.path()).await;
    assert_eq!(
        t.post("/v1/agents/Pia/archive", json!({"archived":true}))
            .await
            .status(),
        200
    );
    let mut url = reqwest::Url::parse(&format!("{}/v1/session-candidates", t.base)).unwrap();
    url.query_pairs_mut()
        .append_pair("repo", &repo.path().to_string_lossy());
    let candidates: Value = t
        .client
        .get(url)
        .bearer_auth(&t.hub.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pia = &candidates["sessions"][0];
    assert_eq!(pia["can_use"], true);
    assert_eq!(pia["reason"], "archived");
    let chat = t.post("/v1/agents/Pia/chat", json!({"text":"hello"})).await;
    assert_eq!(chat.status(), 409);
    let error = chat.text().await.unwrap();
    assert!(
        error.contains("Pia") && error.contains("restore: true"),
        "{error}"
    );
    let rejected = t
        .post(
            "/v1/runs",
            json!({"planner":"Pia", "request":"test", "fresh":false}),
        )
        .await;
    assert_eq!(rejected.status(), 409);
    assert!(rejected.text().await.unwrap().contains("restore"));
    let r = run(
        &t,
        json!({"planner":"Pia", "request":"test", "fresh":false, "restore":true}),
    )
    .await;
    assert_eq!(r["planner"], json!(a.id));
}

#[tokio::test]
async fn pinned_driver_is_rejected() {
    let repo = git_repo();
    let sessions = tempfile::tempdir().unwrap();
    let t = TestHub::start_with(pinned_plugin(sessions.path())).await;
    t.add_on("pinned", "Pia", "planner", repo.path()).await;
    let r = t
        .post(
            "/v1/runs",
            json!({"planner":"Pia", "request":"test", "fresh":false}),
        )
        .await;
    assert_eq!(r.status(), 409);
    assert!(r.text().await.unwrap().contains("another directory"));
}

#[tokio::test]
async fn explicit_self_continues_conversation_without_copy() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    let a = t.add_native("Pia", "planner", repo.path()).await;
    let mut watch = t.watch().await;
    t.send("Pia", "[session]").await;
    watch
        .until(
            "chat turn",
            |e| matches!(e.kind, EventKind::AgentTurnCompleted { id, .. } if id == a.id),
        )
        .await;
    let before = watch.output();
    let session = before
        .split("session=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    let count: Value = t.get("/v1/agents").await.json().await.unwrap();
    let r = run(
        &t,
        json!({"planner":"Pia", "request":"[session]", "fresh":false}),
    )
    .await;
    assert_eq!(r["planner"], json!(a.id));
    watch
        .until(
            "run turn",
            |e| matches!(e.kind, EventKind::AgentTurnCompleted { id, .. } if id == a.id),
        )
        .await;
    assert!(
        watch
            .output()
            .contains(&format!("session={session} (resumed)"))
    );
    let after: Value = t.get("/v1/agents").await.json().await.unwrap();
    assert_eq!(
        count["agents"].as_array().unwrap().len(),
        after["agents"].as_array().unwrap().len()
    );
}

#[tokio::test]
async fn candidates_report_why_and_filter_workspace() {
    let repo = git_repo();
    let elsewhere = git_repo();
    let sessions = tempfile::tempdir().unwrap();
    let t = TestHub::start_with(pinned_plugin(sessions.path())).await;
    t.add_native("Pia", "planner", repo.path()).await;
    t.add_on("pinned", "Pinned", "planner", repo.path()).await;
    t.add_native("Other", "planner", elsewhere.path()).await;
    let r = run(&t, json!({"planner":"Pia", "request":"waiting"})).await;
    let mut url = reqwest::Url::parse(&format!("{}/v1/session-candidates", t.base)).unwrap();
    url.query_pairs_mut()
        .append_pair("repo", &repo.path().to_string_lossy());
    let response = t
        .client
        .get(url)
        .bearer_auth(&t.hub.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let value: Value = response.json().await.unwrap();
    let list = value["sessions"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    let pia = list.iter().find(|s| s["agent"]["name"] == "Pia").unwrap();
    assert_eq!(pia["reason"], "busy");
    assert!(
        pia["busy_with"]
            .as_str()
            .unwrap()
            .contains(r["id"].as_str().unwrap())
    );
    let pinned = list
        .iter()
        .find(|s| s["agent"]["name"] == "Pinned")
        .unwrap();
    assert_eq!(pinned["reason"], "cannot_move");
    assert_eq!(pinned["can_use"], false);
}

#[tokio::test]
async fn in_place_pinned_session_can_lead_itself() {
    let repo = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let t = TestHub::start_with(pinned_plugin(sessions.path())).await;
    let pia = t.add_on("pinned", "Pia", "planner", repo.path()).await;
    let mut watch = t.watch().await;
    t.send("Pia", "[session]").await;
    watch
        .until(
            "chat",
            |e| matches!(e.kind, EventKind::AgentTurnCompleted { id, .. } if id == pia.id),
        )
        .await;
    let session = watch
        .output()
        .split("session=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    let mut url = reqwest::Url::parse(&format!("{}/v1/session-candidates", t.base)).unwrap();
    url.query_pairs_mut()
        .append_pair("repo", &repo.path().to_string_lossy());
    let candidates: Value = t
        .client
        .get(url)
        .bearer_auth(&t.hub.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(candidates["sessions"][0]["can_use"], true);
    assert!(candidates["sessions"][0]["reason"].is_null());
    let before: Value = t.get("/v1/agents").await.json().await.unwrap();
    let r = run(
        &t,
        json!({"planner":"Pia", "request":"[session]", "fresh":false}),
    )
    .await;
    assert_eq!(r["planner"], json!(pia.id));
    assert_eq!(r["in_place"], true);
    watch
        .until(
            "run",
            |e| matches!(e.kind, EventKind::AgentTurnCompleted { id, .. } if id == pia.id),
        )
        .await;
    let output = watch.output();
    assert_eq!(
        output.matches(&format!("session={session} ")).count(),
        2,
        "{output}"
    );
    let after: Value = t.get("/v1/agents").await.json().await.unwrap();
    assert_eq!(
        before["agents"].as_array().unwrap().len(),
        after["agents"].as_array().unwrap().len()
    );
}
