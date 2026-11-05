//! Structured task results through the worker tools and persisted task report.
mod common;

use common::{TestHub, git_repo};
use crossagents_api_types::{EventKind, TaskStatus};
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

#[tokio::test]
async fn structured_result_is_validated_stored_and_addressable() {
    let repo = git_repo();
    let t = TestHub::start().await;
    t.add_agent_in("boss", Some("opus"), "planner", repo.path())
        .await;
    t.add_agent_in("coder", Some("haiku"), "worker", repo.path())
        .await;
    let mut watch = t.watch().await;
    let goal = [
        call(
            "task_complete",
            json!({"task_id":"{task_id}","summary":"missing"}),
        ),
        call(
            "task_complete",
            json!({"task_id":"{task_id}","summary":"bad", "result":{"files":[42]}}),
        ),
        call("task_get", json!({"task_id":"{task_id}"})),
        call(
            "task_complete",
            json!({"task_id":"{task_id}","summary":"done", "result":{"files":["a.txt"]}}),
        ),
        call("task_get", json!({"task_id":"{task_id}","path":"/files/0"})),
        call("task_get", json!({"task_id":"{task_id}","path":"/missing"})),
    ]
    .join(" ");
    let tasks = json!([{"key":"a","title":"structured","spec":{"goal":goal,"result_schema":{"type":"object","required":["files"],"properties":{"files":{"type":"array","items":{"type":"string"}}}}}}]);
    let request = call("plan_submit", json!({"tasks":tasks}));
    let resp = t.post("/v1/runs", json!({"title":"Structured result test","request":request,"planner":"boss","worker":"coder","policy":{"plan_approval":"auto"}})).await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let run: Value = resp.json().await.unwrap();
    let id = run["id"].as_str().unwrap();
    let seen = tokio::time::timeout(std::time::Duration::from_secs(30), watch.until("worker calls", |e| matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains("called task_get") && text.contains("/missing")))).await;
    if seen.is_err() {
        eprintln!(
            "OUTPUT: {}\nEVENTS: {:#?}",
            watch.output(),
            watch.seen.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
    }
    assert!(seen.is_ok());
    let output = watch.output();
    assert!(
        output.contains("called plan_submit: {") && output.contains("result is required"),
        "{output}"
    );
    assert!(
        output.contains("called task_complete: conflict: result at /files/0"),
        "{output}"
    );
    let report_text = output
        .split_once("called task_get: ")
        .expect("task_get response")
        .1;
    let before: Value = serde_json::Deserializer::from_str(report_text)
        .into_iter()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(before["status"], "running");
    assert!(before["report"]["summary"].is_null());
    assert_eq!(
        before["spec"]["result_schema"]["properties"]["files"]["type"],
        "array"
    );
    assert!(output.contains("called task_get: \"a.txt\""), "{output}");
    assert!(
        output.contains("called task_get: conflict: result path \"/missing\" does not exist"),
        "{output}"
    );
    watch.find("task passed", |e| matches!(&e.kind, EventKind::TaskStatusChanged { key, to: TaskStatus::Passed, .. } if key == "a")).await;
    let detail: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    assert_eq!(
        detail["tasks"][0]["report"]["result"],
        json!({"files":["a.txt"]})
    );
}

#[tokio::test]
async fn task_without_schema_still_completes() {
    let repo = git_repo();
    let t = TestHub::start().await;
    t.add_agent_in("boss", Some("opus"), "planner", repo.path())
        .await;
    t.add_agent_in("coder", Some("haiku"), "worker", repo.path())
        .await;
    let mut watch = t.watch().await;
    let goal = call(
        "task_complete",
        json!({"task_id":"{task_id}","summary":"legacy"}),
    );
    let request = call(
        "plan_submit",
        json!({"tasks":[{"key":"legacy","title":"legacy","spec":{"goal":goal}}]}),
    );
    let resp = t.post("/v1/runs", json!({"title":"Structured result test","request":request,"planner":"boss","worker":"coder","policy":{"plan_approval":"auto"}})).await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let run: Value = resp.json().await.unwrap();
    watch.until("legacy passed", |e| matches!(&e.kind, EventKind::TaskStatusChanged { key, to: TaskStatus::Passed, .. } if key == "legacy")).await;
    let detail: Value = t
        .get(&format!("/v1/runs/{}", run["id"].as_str().unwrap()))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(detail["tasks"][0]["report"]["summary"], "legacy");
}
