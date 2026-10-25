//! A conversation proposes a Work and the person starts or declines it (docs/tasks/W1.md §6).

mod common;

use common::{TestHub, git_repo};
use crossagents_api_types::EventKind;
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

/// The open Work card `lyra` put up.
async fn card(t: &TestHub) -> Value {
    let all: Value = t.get("/v1/questions").await.json().await.unwrap();
    all["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["action"] == "work" && i["status"] == "open")
        .cloned()
        .unwrap_or_else(|| panic!("no work card in {all}"))
}

async fn propose(t: &TestHub) -> Value {
    let mut w = t.watch().await;
    let proposal = call(
        "work_start",
        json!({ "request": "Add notes.txt with the agreed wording.", "title": "Add notes" }),
    );
    t.send("lyra", &format!("Let's do it on a branch. {proposal}"))
        .await;
    w.until(
        "card",
        |e| matches!(&e.kind, EventKind::HumanAsked { item } if item.text == "Add notes"),
    )
    .await;
    card(t).await
}

#[tokio::test]
async fn starting_a_proposed_work_runs_it_on_its_own_branch() {
    let repo = git_repo();
    let t = TestHub::start().await;
    let lyra = t
        .add_agent_in("lyra", Some("opus"), "worker", repo.path())
        .await;

    let item = propose(&t).await;
    assert_eq!(item["detail"], "Add notes.txt with the agreed wording.");
    assert!(item["run_id"].is_null());

    let resp = t
        .post(
            &format!("/v1/questions/{}/answer", item["id"].as_str().unwrap()),
            json!({ "answer": "开始" }),
        )
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());

    let runs: Value = t.get("/v1/runs").await.json().await.unwrap();
    let run = &runs["runs"][0];
    assert_eq!(run["title"], "Add notes");
    assert_eq!(run["request"], "Add notes.txt with the agreed wording.");
    // A copy leads it in the run's checkout; the conversation stays where it was.
    assert_ne!(run["planner"], json!(lyra.id));
    let lead: Value = t
        .get(&format!("/v1/agents/{}", run["planner"].as_str().unwrap()))
        .await
        .json()
        .await
        .unwrap();
    assert!(
        lead["cwd"].as_str().unwrap().contains("integration"),
        "{lead}"
    );
    let me: Value = t.get("/v1/agents/lyra").await.json().await.unwrap();
    assert_eq!(me["cwd"], json!(lyra.cwd));

    let all: Value = t.get("/v1/questions?all=true").await.json().await.unwrap();
    let done = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == item["id"])
        .unwrap();
    assert_eq!(done["run_id"], run["id"]);
}

#[tokio::test]
async fn declining_starts_nothing() {
    let repo = git_repo();
    let t = TestHub::start().await;
    t.add_agent_in("lyra", Some("opus"), "worker", repo.path())
        .await;

    let item = propose(&t).await;
    let resp = t
        .post(
            &format!("/v1/questions/{}/answer", item["id"].as_str().unwrap()),
            json!({ "answer": "不用" }),
        )
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let runs: Value = t.get("/v1/runs").await.json().await.unwrap();
    assert_eq!(runs["runs"].as_array().unwrap().len(), 0, "{runs}");
}
