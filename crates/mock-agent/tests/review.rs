//! The person reviews a run (docs/tasks/DOGFOOD.md §2): a task's diff, comments that reach
//! the worker as its next turn and send the task back, and the run's total diff.

mod common;

use common::{TestHub, git_repo};
use crossagents_api_types::{EventKind, MessageSource, TaskStatus};
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

fn task_is(key: &'static str, want: TaskStatus) -> impl Fn(&crossagents_api_types::Event) -> bool {
    move |e| matches!(&e.kind, EventKind::TaskStatusChanged { key: k, to, .. } if k == key && *to == want)
}

#[tokio::test]
async fn the_person_comments_on_a_task_and_sends_it_back() {
    let repo = git_repo();
    let t = TestHub::start().await;
    t.add_agent_in("boss", Some("opus"), "planner", repo.path())
        .await;
    t.add_agent_in("coder", Some("haiku"), "worker", repo.path())
        .await;
    let mut w = t.watch().await;

    // The worker writes a.txt and reports; nobody accepts yet.
    let goal = format!(
        "[write a.txt=alpha] {}",
        call(
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": "wrote a" })
        )
    );
    let plan = json!([{ "key": "a", "title": "task a", "spec": { "goal": goal } }]);
    let req = format!(
        "Build it. {}",
        call("plan_submit", json!({ "tasks": plan }))
    );
    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": req, "planner": "boss", "worker": "coder", "policy": { "plan_approval": "auto" } }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let run: Value = resp.json().await.unwrap();
    let id = run["id"].as_str().unwrap().to_owned();
    w.until("a reported", task_is("a", TaskStatus::Passed))
        .await;

    let diff: Value = t
        .get(&format!("/v1/runs/{id}/tasks/a/diff"))
        .await
        .json()
        .await
        .unwrap();
    assert!(diff["diff"].as_str().unwrap().contains("+alpha"), "{diff}");
    assert_eq!(diff["base"], run["branch"]);
    assert_eq!(diff["truncated"], false);

    // A line comment that carries the worker's next steps (mock agents act on directives),
    // and the person's own words.
    let next = format!(
        "say beta instead [write a.txt=beta] {}",
        call(
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": format!("now beta {}", call("task_accept", json!({ "task": "a", "force": true, "reason": "no checks configured" }))) })
        )
    );
    let resp = t
        .post(
            &format!("/v1/runs/{id}/tasks/%23a/review"),
            json!({
                "comments": [{ "path": "a.txt", "line": 1, "text": next }],
                "note": "close, one change",
                "rework": true,
            }),
        )
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let task: Value = resp.json().await.unwrap();
    assert_eq!(task["status"], "running");
    assert_eq!(task["attempts"], 2);

    w.until("a merged", task_is("a", TaskStatus::Merged)).await;
    let delivered = w
        .seen
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::AgentInput {
                text,
                from: Some(MessageSource::Human),
                ..
            } if text.contains("Review comments on #a") => Some(text.clone()),
            _ => None,
        })
        .expect("the comments reached the worker");
    assert!(delivered.starts_with("close, one change"), "{delivered}");
    assert!(
        delivered.contains("- a.txt:1 — say beta instead"),
        "{delivered}"
    );
    assert!(delivered.contains("rework of task #a"), "{delivered}");

    // Merged: the task's worktree is gone, the run's diff has it all.
    let resp = t.get(&format!("/v1/runs/{id}/tasks/a/diff")).await;
    assert_eq!(resp.status(), 409);
    let total: Value = t
        .get(&format!("/v1/runs/{id}/diff"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(total["base"], "main");
    let text = total["diff"].as_str().unwrap();
    assert!(text.contains("+beta") && !text.contains("+alpha"), "{text}");

    // An ended task takes no more comments.
    let resp = t
        .post(
            &format!("/v1/runs/{id}/tasks/a/review"),
            json!({ "comments": [{ "path": "a.txt", "text": "late" }] }),
        )
        .await;
    assert_eq!(resp.status(), 409);
}
