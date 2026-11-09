//! The review loop (docs/TODO.md §九 ①, §二十五). A report that passed its checks is reviewed by
//! the hub itself when its task has a reviewer. Changes the reviewer asks for go straight back to
//! the worker that made the work, as numbered findings its answers name, and the lead hears one
//! quiet line. What needs a decision (a limit reached, a dispute) comes to the lead as a notice.
//! The person's own comments still send a task back, as before.

mod common;

use common::{TestHub, git_repo};
use crossagents_api_types::{Event, EventKind, MessageSource, TaskStatus};
use crossagents_core::NoticeClass;
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

/// The same call, qualified to run only on `model` (and attempt `attempt`, when given).
fn on(model: &str, attempt: Option<u32>, tool: &str, args: Value) -> String {
    let attempt = attempt.map(|a| a.to_string()).unwrap_or_default();
    format!("[call@{attempt}#{model} {tool} {args}]")
}

fn task_is(key: &'static str, want: TaskStatus) -> impl Fn(&Event) -> bool {
    move |e| matches!(&e.kind, EventKind::TaskStatusChanged { key: k, to, .. } if k == key && *to == want)
}

/// A notice given to `agent` whose line says `what`.
fn notice_to(agent: String, what: &'static str) -> impl Fn(&Event) -> bool {
    move |e| {
        matches!(&e.kind, EventKind::NoticeRecorded { id, line, .. }
            if id.to_string() == agent && line.contains(what))
    }
}

/// The texts handed to `agent` as its turns.
fn inputs_to(seen: &[Event], agent: &str) -> Vec<String> {
    seen.iter()
        .filter_map(|e| match &e.kind {
            EventKind::AgentInput { id, text, .. } if id.to_string() == agent => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn class_of(seen: &[Event], agent: &str, what: &str) -> Option<NoticeClass> {
    seen.iter().find_map(|e| match &e.kind {
        EventKind::NoticeRecorded {
            id, line, class, ..
        } if id.to_string() == agent && line.contains(what) => Some(*class),
        _ => None,
    })
}

async fn setup(repo: &std::path::Path) -> TestHub {
    let t = TestHub::start().await;
    t.add_agent_in("boss", Some("opus"), "planner", repo).await;
    t.add_agent_in("coder", Some("haiku"), "worker", repo).await;
    t.add_agent_in("critic", Some("opus"), "reviewer", repo)
        .await;
    t
}

/// Starts a run whose only task is `g` with `goal`, reviewed by `critic` when `reviewed`.
async fn start_run(t: &TestHub, goal: String, reviewed: bool, spec_extra: Value) -> String {
    let mut spec = json!({ "goal": goal });
    spec.as_object_mut()
        .unwrap()
        .extend(spec_extra.as_object().unwrap().clone());
    let plan = json!([{ "key": "g", "title": "task g", "spec": spec }]);
    let request = format!(
        "Build it. {}",
        call("plan_submit", json!({ "tasks": plan }))
    );
    let mut body = json!({
        "request": request,
        "planner": "boss",
        "worker": "coder",
        "policy": { "plan_approval": "auto", "auto_approve": true }
    });
    if reviewed {
        body["reviewer"] = json!("critic");
    }
    let resp = t.post("/v1/runs", body).await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let run: Value = resp.json().await.unwrap();
    run["id"].as_str().unwrap().to_owned()
}

async fn detail(t: &TestHub, id: &str) -> Value {
    t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap()
}

#[tokio::test]
async fn a_verified_report_is_reviewed_by_the_hub_and_the_lead_hears_one_quiet_line() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let boss = t.agent("boss").await.id.to_string();

    // The worker writes g and reports; the reviewer (qualified to mock-opus) approves.
    let goal = format!(
        "[write#mock-haiku g.txt=v1] {} {}",
        on(
            "mock-haiku",
            None,
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": "wrote g" })
        ),
        on(
            "mock-opus",
            None,
            "review_submit",
            json!({ "task_id": "{task_id}", "approve": true, "notes": "lgtm" })
        ),
    );
    let id = start_run(&t, goal, true, json!({})).await;

    // The review is the hub's: the lead's acceptance notice says the review passed, and is a
    // batch (nothing downstream waits on it).
    w.until(
        "the acceptance notice",
        notice_to(boss.clone(), "waits for your acceptance"),
    )
    .await;
    let d = detail(&t, &id).await;
    let task = &d["tasks"][0];
    assert_eq!(task["status"], "passed", "{task}");
    assert_eq!(task["report"]["review"], "lgtm", "{task}");
    assert_eq!(task["report"]["verdict"], "merge", "{task}");
    assert_eq!(task["report"]["review_attempt"], 1, "{task}");
    assert_eq!(
        task["report"]["reviewed_sha"], task["report"]["verified_sha"],
        "the review is bound to the verified commit: {task}"
    );
    assert_eq!(
        class_of(&w.seen, &boss, "waits for your acceptance"),
        Some(NoticeClass::Batch),
        "nothing downstream waits: a batch"
    );
    // The review opened quietly, and the lead was not handed the worker's report.
    assert_eq!(
        class_of(&w.seen, &boss, "review round 1 is with"),
        Some(NoticeClass::Silent)
    );
    assert!(
        inputs_to(&w.seen, &boss)
            .iter()
            .all(|text| !text.contains("---\ntask #g")),
        "the report is not relayed: {:?}",
        inputs_to(&w.seen, &boss)
    );
}

#[tokio::test]
async fn changes_asked_for_go_back_to_the_same_worker_and_the_lead_hears_one_line() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let boss = t.agent("boss").await.id.to_string();

    // Round 1 writes v1 and the review asks for a fix (P0, numbered F1). Round 2 writes v2,
    // answers F1 as fixed, and the second review approves it.
    let goal = format!(
        "[write@1#mock-haiku g.txt=v1] [write@2#mock-haiku g.txt=v2] {} {} {} {}",
        on(
            "mock-haiku",
            Some(1),
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": "wrote g" })
        ),
        on(
            "mock-haiku",
            Some(2),
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": "fixed g", "answers": [{ "id": "F1", "status": "fixed", "note": "g holds v2 now" }] })
        ),
        on(
            "mock-opus",
            Some(1),
            "review_submit",
            json!({
                "task_id": "{task_id}",
                "approve": false,
                "notes": "g must say v2",
                "findings": [{ "severity": "P0", "confidence": 0.9, "title": "g says v1", "file": "g.txt", "line": 1, "verify": "g.txt holds v2" }]
            })
        ),
        on(
            "mock-opus",
            Some(2),
            "review_submit",
            json!({ "task_id": "{task_id}", "approve": true, "notes": "lgtm now" })
        ),
    );
    let id = start_run(&t, goal, true, json!({})).await;

    w.until(
        "the acceptance notice",
        notice_to(boss.clone(), "waits for your acceptance"),
    )
    .await;

    // The lead heard one quiet line about the change request and nothing that needed it.
    assert_eq!(
        class_of(&w.seen, &boss, "changes needed (F1 P0)"),
        Some(NoticeClass::Silent),
        "one quiet line: {:?}",
        w.seen
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::NoticeRecorded { line, .. } => Some(line.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    );
    assert!(
        class_of(&w.seen, &boss, "it went back to w-").is_some(),
        "the line names the worker it went back to"
    );
    assert!(
        inputs_to(&w.seen, &boss)
            .iter()
            .all(|text| !text.contains("---\ntask #g")),
        "the worker's report is not relayed to the lead"
    );

    // The same worker (the hub's session of coder, named in the line) got the numbered finding
    // as its next turn, with its attempt.
    let went_back = w
        .seen
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::NoticeRecorded { line, .. } => line
                .split("it went back to ")
                .nth(1)
                .map(|rest| rest.trim_end_matches('.').to_owned()),
            _ => None,
        })
        .expect("the line names the worker");
    let coder = t.agent(&went_back).await.id.to_string();
    let for_coder = inputs_to(&w.seen, &coder);
    let rework = for_coder
        .iter()
        .find(|text| text.contains("F1 P0 (blocking) g says v1"))
        .unwrap_or_else(|| panic!("the worker was sent back the finding: {for_coder:?}"));
    assert!(rework.contains("g.txt:1"), "{rework}");
    assert!(rework.contains("verify: g.txt holds v2"), "{rework}");
    assert!(rework.contains("attempt: 2"), "{rework}");

    // Round 2's reviewer saw the worker's answer to F1, under the same id.
    assert!(
        w.seen.iter().any(|e| matches!(&e.kind,
            EventKind::AgentInput { text, .. } if text.contains("worker: fixed"))),
        "the next reviewer is shown the answer"
    );

    let d = detail(&t, &id).await;
    let task = &d["tasks"][0];
    assert_eq!(task["status"], "passed", "{task}");
    assert_eq!(
        task["attempts"], 2,
        "the same worker's second attempt: {task}"
    );
    assert_eq!(task["report"]["review_returns"], 1, "{task}");
    assert_eq!(task["report"]["review_attempt"], 2, "{task}");
    assert_eq!(task["report"]["review"], "lgtm now", "{task}");
    assert_eq!(task["report"]["answers"][0]["id"], "F1", "{task}");
}

#[tokio::test]
async fn a_review_that_keeps_asking_stops_at_the_limit_and_the_lead_decides() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let boss = t.agent("boss").await.id.to_string();

    // Both rounds ask for a change; the spec allows one return, so the second request is the
    // lead's to decide.
    let goal = format!(
        "[write@1#mock-haiku g.txt=v1] [write@2#mock-haiku g.txt=v2] {} {} {} {}",
        on(
            "mock-haiku",
            Some(1),
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": "wrote g" })
        ),
        on(
            "mock-haiku",
            Some(2),
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": "wrote g again" })
        ),
        on(
            "mock-opus",
            Some(1),
            "review_submit",
            json!({ "task_id": "{task_id}", "approve": false, "notes": "not yet", "findings": [{ "severity": "P1", "confidence": 0.8, "title": "still wrong" }] })
        ),
        on(
            "mock-opus",
            Some(2),
            "review_submit",
            json!({ "task_id": "{task_id}", "approve": false, "notes": "still not", "findings": [{ "severity": "P1", "confidence": 0.8, "title": "still wrong again" }] })
        ),
    );
    let id = start_run(&t, goal, true, json!({ "max_returns": 1 })).await;

    w.until(
        "the lead decides",
        notice_to(
            boss.clone(),
            "asks for changes, and it has gone back to its worker 1 times already",
        ),
    )
    .await;
    assert_eq!(
        class_of(&w.seen, &boss, "gone back to its worker 1 times already"),
        Some(NoticeClass::Decide),
        "the limit is a decision for the lead"
    );
    let d = detail(&t, &id).await;
    let task = &d["tasks"][0];
    assert_eq!(task["status"], "passed", "it waits with the lead: {task}");
    assert_eq!(task["attempts"], 2, "one return happened: {task}");
    assert_eq!(task["report"]["review_returns"], 1, "{task}");
}

#[tokio::test]
async fn a_worker_that_disputes_a_finding_hands_it_to_the_lead() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let boss = t.agent("boss").await.id.to_string();

    let goal = format!(
        "[write@1#mock-haiku g.txt=v1] {} {} {}",
        on(
            "mock-haiku",
            Some(1),
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": "wrote g" })
        ),
        on(
            "mock-haiku",
            Some(2),
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": "g is as the spec says", "answers": [{ "id": "F1", "status": "disputed", "note": "the spec asks for v1" }] })
        ),
        on(
            "mock-opus",
            Some(1),
            "review_submit",
            json!({ "task_id": "{task_id}", "approve": false, "notes": "use v2", "findings": [{ "severity": "P0", "confidence": 0.9, "title": "g must say v2" }] })
        ),
    );
    let id = start_run(&t, goal, true, json!({})).await;

    w.until(
        "the lead decides the dispute",
        notice_to(boss.clone(), "the worker disputes F1"),
    )
    .await;
    assert_eq!(
        class_of(&w.seen, &boss, "the worker disputes F1"),
        Some(NoticeClass::Decide)
    );
    let d = detail(&t, &id).await;
    let task = &d["tasks"][0];
    assert_eq!(task["status"], "passed", "{task}");
    assert_eq!(
        task["report"]["review_attempt"], 1,
        "a dispute does not open another review: {task}"
    );
}

#[tokio::test]
async fn the_person_comments_on_a_task_and_sends_it_back() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let boss = t.agent("boss").await.id.to_string();
    let mut w = t.watch().await;

    // The worker writes a.txt and reports; the lead is asked to accept it, and nobody does yet.
    let goal = format!(
        "[write@1#mock-haiku a.txt=alpha] [write@2#mock-haiku a.txt=beta] {} {}",
        on(
            "mock-haiku",
            Some(1),
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": "wrote a" })
        ),
        on(
            "mock-haiku",
            Some(2),
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": "now beta" })
        ),
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
    w.until(
        "a waits for acceptance",
        notice_to(boss.clone(), "#a waits for your acceptance"),
    )
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

    // A line comment, and the person's own words: the task goes back to its worker.
    let resp = t
        .post(
            &format!("/v1/runs/{id}/tasks/%23a/review"),
            json!({
                "comments": [{ "path": "a.txt", "line": 1, "text": "say beta instead" }],
                "note": "close, one change",
                "rework": true,
            }),
        )
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let task: Value = resp.json().await.unwrap();
    assert_eq!(task["status"], "running");
    assert_eq!(task["attempts"], 2);

    // The worker's second report, and the lead accepts it by hand.
    w.until("a reported again", task_is("a", TaskStatus::Passed))
        .await;
    w.until(
        "the second acceptance notice",
        notice_to(boss.clone(), "#a waits for your acceptance"),
    )
    .await;
    let accept = call(
        "task_accept",
        json!({ "task": "a", "force": true, "reason": "no checks configured" }),
    );
    t.send("boss", &accept).await;
    w.until("merged", task_is("a", TaskStatus::Merged)).await;

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
