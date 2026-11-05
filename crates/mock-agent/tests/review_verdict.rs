//! A reviewer's structured verdict (docs/TODO.md §二十五 第 6 条): findings graded P0-P3 with
//! a confidence, a merge verdict that must agree with `approve`, and the order the issuer
//! reads them in. The old approve-only call keeps working.

mod common;

use common::{TestHub, git_repo};
use crossagents_api_types::EventKind;
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

/// A worker goal that writes a file, reports, and asks the planner to have `critic` review it.
/// The reviewer's directives ride in the goal (qualified `#mock-opus`), which the review brief
/// shows the reviewer and the worker's own model skips.
fn reviewed_task(reviewer_directives: &str) -> String {
    let request_review = call(
        "review_request",
        json!({ "task": "g", "reviewer": "critic" }),
    );
    format!(
        "[write#mock-haiku g.txt=v1] {} {reviewer_directives}",
        call(
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": format!("wrote g {request_review}") })
        )
    )
}

async fn setup(repo: &std::path::Path) -> TestHub {
    let t = TestHub::start().await;
    t.add_agent_in("boss", Some("opus"), "planner", repo).await;
    t.add_agent_in("coder", Some("haiku"), "worker", repo).await;
    t.add_agent_in("critic", Some("opus"), "reviewer", repo)
        .await;
    t
}

async fn start_run(t: &TestHub, goal: String) -> String {
    let plan = json!([{ "key": "g", "title": "task g", "spec": { "goal": goal } }]);
    let request = format!(
        "Build it. {}",
        call("plan_submit", json!({ "tasks": plan }))
    );
    let resp = t
        .post(
            "/v1/runs",
            json!({
                "request": request,
                "planner": "boss",
                "worker": "coder",
                "policy": { "plan_approval": "auto", "auto_approve": true }
            }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let run: Value = resp.json().await.unwrap();
    run["id"].as_str().unwrap().to_owned()
}

async fn detail(t: &TestHub, id: &str) -> Value {
    t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap()
}

#[tokio::test]
async fn findings_are_stored_and_reach_the_issuer_most_severe_first() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let boss: Value = t.get("/v1/agents/boss").await.json().await.unwrap();
    let boss_id = boss["id"].as_str().unwrap().to_owned();

    // A contradictory call comes first: a P0 finding but verdict merge. It must be refused,
    // so the real review below still finds the task in Reviewing and lands its notes.
    let refused = call(
        "review_submit",
        json!({
            "task_id": "{task_id}",
            "approve": true,
            "notes": "must not be recorded",
            "verdict": "merge",
            "findings": [{ "severity": "P0", "confidence": 0.9, "title": "wrong" }]
        }),
    )
    .replacen("[call ", "[call#mock-opus ", 1);
    // Findings are given out of order on purpose: P2, then P0, then P1.
    let review = call(
        "review_submit",
        json!({
            "task_id": "{task_id}",
            "approve": false,
            "notes": "not yet",
            "verdict": "do_not_merge",
            "findings": [
                { "severity": "P2", "confidence": 0.4, "file": "g.txt", "title": "minor naming" },
                { "severity": "P0", "confidence": 0.95, "file": "g.txt", "line": 1, "title": "wrong content", "detail": "it must say v1" },
                { "severity": "P1", "confidence": 0.7, "title": "no test" }
            ]
        }),
    )
    .replacen("[call ", "[call#mock-opus ", 1);
    let goal = reviewed_task(&format!("{refused} {review}"));

    let id = start_run(&t, goal).await;

    // The review message to the issuer is what carries the sorted findings.
    let event = w
        .until("the review reaches the planner", |e| {
            matches!(&e.kind, EventKind::AgentInput { id, text, .. }
                if id.to_string() == boss_id && text.starts_with("Review of #g"))
        })
        .await;
    let EventKind::AgentInput { text, .. } = &event.kind else {
        unreachable!()
    };
    assert!(text.starts_with("Review of #g "), "{text}");
    assert!(text.contains(": changes needed"), "{text}");
    assert!(!text.contains("must not be recorded"), "{text}");
    assert!(text.contains("Verdict: do_not_merge"), "{text}");
    let p0 = text
        .find("wrong content")
        .expect("the P0 is in the message");
    let p1 = text.find("no test").expect("the P1 is in the message");
    let p2 = text.find("minor naming").expect("the P2 is in the message");
    assert!(p0 < p1 && p1 < p2, "{text}");
    assert!(text.contains("confidence 0.95"), "{text}");
    assert!(text.contains("g.txt:1"), "{text}");

    let detail = detail(&t, &id).await;
    let task = &detail["tasks"][0];
    assert_eq!(task["status"], "passed", "{task}");
    assert_eq!(task["report"]["verdict"], "do_not_merge", "{task}");
    let findings = task["report"]["findings"]
        .as_array()
        .expect("the findings are stored");
    let severities: Vec<&str> = findings
        .iter()
        .map(|f| f["severity"].as_str().unwrap())
        .collect();
    assert_eq!(severities, ["P0", "P1", "P2"], "{findings:?}");
    assert_eq!(findings[0]["confidence"], 0.95);
    assert_eq!(findings[0]["file"], "g.txt");
    assert_eq!(findings[0]["line"], 1);
    // The stored notes carry the findings too, so the GUI and the trace see one story.
    assert!(
        task["report"]["review"]
            .as_str()
            .unwrap()
            .contains("Verdict: do_not_merge")
    );
}

#[tokio::test]
async fn an_approve_only_submission_still_works() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let boss: Value = t.get("/v1/agents/boss").await.json().await.unwrap();
    let boss_id = boss["id"].as_str().unwrap().to_owned();

    // The pre-findings call, exactly as a reviewer wrote it before this existed.
    let review = call(
        "review_submit",
        json!({ "task_id": "{task_id}", "approve": true, "notes": "lgtm" }),
    )
    .replacen("[call ", "[call#mock-opus ", 1);
    let goal = reviewed_task(&review);

    let id = start_run(&t, goal).await;
    // A `passed` task is reported, not reviewed: wait for the review to reach the planner.
    w.until("the review reaches the planner", |e| {
        matches!(&e.kind, EventKind::AgentInput { id, text, .. }
            if id.to_string() == boss_id && text.starts_with("Review of #g"))
    })
    .await;

    let detail = detail(&t, &id).await;
    let task = &detail["tasks"][0];
    assert_eq!(task["status"], "passed", "{task}");
    assert_eq!(task["report"]["review"], "lgtm", "{task}");
    // No structured findings were given, so none are stored; approve alone decides the
    // verdict (docs/TODO.md §二十五 第 6 条).
    assert!(task["report"]["findings"].is_null(), "{task}");
    assert_eq!(task["report"]["verdict"], "merge", "{task}");
}
