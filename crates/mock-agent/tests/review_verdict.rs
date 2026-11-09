//! A reviewer's structured verdict (docs/TODO.md §二十五 第 6 条): findings graded P0-P3 with a
//! confidence, numbered by the hub, a merge verdict that must agree with `approve`, and the order
//! the worker reads them in. A verdict bound to another commit, or a second one for the same round,
//! is refused and changes nothing. The old approve-only call keeps working.

mod common;

use common::{TestHub, git_repo};
use crossagents_api_types::EventKind;
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

/// Qualified to run only for `model` in round `attempt` (the review round, from its brief).
fn on(model: &str, attempt: u32, tool: &str, args: Value) -> String {
    format!("[call@{attempt}#{model} {tool} {args}]")
}

/// A worker goal that writes g.txt, reports done, and then the reviewer's directives (which ride
/// along in the brief the reviewer gets, and only the reviewer's model runs them).
fn reviewed_task(reviewer_directives: &str) -> String {
    format!(
        "[write@1#mock-haiku g.txt=v1] {} {reviewer_directives}",
        on(
            "mock-haiku",
            1,
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": "wrote g" })
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

/// Starts a run of one task `g` with `goal`, reviewed by `critic`.
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
                "reviewer": "critic",
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
async fn findings_are_numbered_stored_and_reach_the_worker_most_severe_first() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // A contradictory call comes first: a P0 finding but verdict merge. It must be refused, so the
    // real review below still finds the task in Reviewing and lands its notes.
    let refused = on(
        "mock-opus",
        1,
        "review_submit",
        json!({
            "task_id": "{task_id}", "reviewed_sha": "{reviewed_sha}",
            "approve": true,
            "notes": "must not be recorded",
            "verdict": "merge",
            "findings": [{ "severity": "P0", "confidence": 0.9, "title": "wrong" }]
        }),
    );
    // Findings are given out of order on purpose: P2, then P0, then P1.
    let review = on(
        "mock-opus",
        1,
        "review_submit",
        json!({
            "task_id": "{task_id}", "reviewed_sha": "{reviewed_sha}",
            "approve": false,
            "notes": "not yet",
            "verdict": "do_not_merge",
            "findings": [
                { "severity": "P2", "confidence": 0.4, "file": "g.txt", "title": "minor naming" },
                { "severity": "P0", "confidence": 0.95, "file": "g.txt", "line": 1, "title": "wrong content", "detail": "it must say v1", "basis": "the spec", "verify": "g.txt reads v1" },
                { "severity": "P1", "confidence": 0.7, "title": "no test" }
            ]
        }),
    );
    // The fix is what the second round asks for and approves.
    let round2 = on(
        "mock-opus",
        2,
        "review_submit",
        json!({ "task_id": "{task_id}", "reviewed_sha": "{reviewed_sha}", "approve": true, "notes": "lgtm" }),
    );
    let worker_round2 = on(
        "mock-haiku",
        2,
        "task_complete",
        json!({ "task_id": "{task_id}", "summary": "fixed g" }),
    );
    let goal = format!(
        "[write@2#mock-haiku g.txt=v2] {} {worker_round2}",
        reviewed_task(&format!("{refused} {review} {round2}")),
    );
    let id = start_run(&t, goal).await;

    // The worker is sent back with the findings, numbered in the order the reviewer gave them and
    // read most severe first.
    let rework = w
        .until("the worker is sent back", |e| {
            matches!(&e.kind, EventKind::AgentInput { text, .. }
                if text.contains("F2 P0 (blocking) wrong content"))
        })
        .await;
    let EventKind::AgentInput { text, .. } = &rework.kind else {
        unreachable!()
    };
    assert!(!text.contains("must not be recorded"), "{text}");
    let p0 = text
        .find("F2 P0 (blocking) wrong content")
        .expect("the P0 is in the message");
    let p1 = text.find("F3 P1").expect("the P1 is in the message");
    let p2 = text.find("F1 P2").expect("the P2 is in the message");
    assert!(p0 < p1 && p1 < p2, "{text}");
    assert!(text.contains("g.txt:1"), "{text}");
    assert!(text.contains("basis: the spec"), "{text}");
    assert!(text.contains("verify: g.txt reads v1"), "{text}");
    assert!(text.contains("attempt: 2"), "{text}");

    // Only the second round's approval gives an acceptance notice.
    w.until("the second round passes", |e| {
        matches!(&e.kind, EventKind::NoticeRecorded { line, .. } if line.contains("waits for your acceptance"))
    })
    .await;
    let d = detail(&t, &id).await;
    let task = &d["tasks"][0];
    assert_eq!(task["status"], "passed", "{task}");
    assert_eq!(task["report"]["review_attempt"], 2, "{task}");
}

#[tokio::test]
async fn an_approve_only_submission_still_works() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // The pre-findings call, exactly as a reviewer wrote it before this existed.
    let review = on(
        "mock-opus",
        1,
        "review_submit",
        json!({ "task_id": "{task_id}", "reviewed_sha": "{reviewed_sha}", "approve": true, "notes": "lgtm" }),
    );
    let id = start_run(&t, reviewed_task(&review)).await;
    w.until("the acceptance notice", |e| {
        matches!(&e.kind, EventKind::NoticeRecorded { line, .. } if line.contains("waits for your acceptance"))
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

#[tokio::test]
async fn a_stale_review_is_refused_and_the_current_one_still_lands() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // The worker reports; the reviewer's only turn is a verdict on a commit the round does not
    // read: refused, and the review stays open.
    let stale = on(
        "mock-opus",
        1,
        "review_submit",
        json!({ "task_id": "{task_id}", "approve": true, "reviewed_sha": "deadbeef", "notes": "stale" }),
    );
    let id = start_run(&t, reviewed_task(&stale)).await;
    let opened = w
        .until("the review is opened", |e| {
            matches!(&e.kind, EventKind::NoticeRecorded { line, .. } if line.contains("review round 1 is with"))
        })
        .await;
    let EventKind::NoticeRecorded { line, .. } = &opened.kind else {
        unreachable!()
    };
    // The reviewer's name is in the notice: "... review round 1 is with NAME (the hub opened it)."
    let name = line
        .split("review round 1 is with ")
        .nth(1)
        .and_then(|rest| rest.split(" (the hub").next())
        .expect("the reviewer is named")
        .to_owned();
    w.until("the stale verdict is refused", |e| {
        matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains("your review is of commit deadbeef"))
    })
    .await;
    let mid = detail(&t, &id).await;
    assert_eq!(
        mid["tasks"][0]["status"], "reviewing",
        "{}",
        mid["tasks"][0]
    );

    // The verdict on the commit the round reads is the one that lands.
    let task_id = mid["tasks"][0]["id"].as_str().unwrap().to_owned();
    t.send(
        &name,
        &call(
            "review_submit",
            json!({ "task_id": task_id, "reviewed_sha": mid["tasks"][0]["report"]["reviewed_sha"], "approve": true, "notes": "lgtm" }),
        ),
    )
    .await;
    w.until("the acceptance notice", |e| {
        matches!(&e.kind, EventKind::NoticeRecorded { line, .. } if line.contains("waits for your acceptance"))
    })
    .await;

    let detail = detail(&t, &id).await;
    let task = &detail["tasks"][0];
    assert_eq!(task["status"], "passed", "{task}");
    assert_eq!(
        task["report"]["review"], "lgtm",
        "the current verdict stands: {task}"
    );
    assert_eq!(task["report"]["verdict"], "merge", "{task}");
    assert_eq!(task["attempts"], 1, "nothing was sent back: {task}");
    assert!(
        task["report"]["review_returns"].as_u64().unwrap_or(0) == 0,
        "{task}"
    );
}

#[tokio::test]
async fn a_verdict_that_names_no_commit_is_refused_and_the_next_one_lands() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // The reviewer's verdicts come as messages of their own: the one that names no commit is
    // refused with its reason, and the review stays open for the one that names it.
    let id = start_run(&t, reviewed_task("")).await;
    let opened = w
        .until("the review is opened", |e| {
            matches!(&e.kind, EventKind::NoticeRecorded { line, .. } if line.contains("review round 1 is with"))
        })
        .await;
    let EventKind::NoticeRecorded { line, .. } = &opened.kind else {
        unreachable!()
    };
    let name = line
        .split("review round 1 is with ")
        .nth(1)
        .and_then(|rest| rest.split(" (the hub").next())
        .expect("the reviewer is named")
        .to_owned();
    let mid = detail(&t, &id).await;
    let task_id = mid["tasks"][0]["id"].as_str().unwrap().to_owned();

    t.send(
        &name,
        &call(
            "review_submit",
            json!({ "task_id": task_id, "approve": true, "notes": "no commit" }),
        ),
    )
    .await;
    w.until("the refusal reached the reviewer", |e| {
        matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains("say which commit you reviewed"))
    })
    .await;
    let still = detail(&t, &id).await;
    assert_eq!(still["tasks"][0]["status"], "reviewing", "{}", still["tasks"][0]);

    let reviewed = mid["tasks"][0]["report"]["reviewed_sha"].clone();
    t.send(
        &name,
        &call(
            "review_submit",
            json!({ "task_id": task_id, "reviewed_sha": reviewed, "approve": true, "notes": "lgtm" }),
        ),
    )
    .await;
    w.until("the acceptance notice", |e| {
        matches!(&e.kind, EventKind::NoticeRecorded { line, .. } if line.contains("waits for your acceptance"))
    })
    .await;

    let detail = detail(&t, &id).await;
    let task = &detail["tasks"][0];
    assert_eq!(task["report"]["review"], "lgtm", "{task}");
    assert_eq!(task["report"]["verdict"], "merge", "{task}");
    assert_eq!(task["attempts"], 1, "nothing was sent back: {task}");
}
