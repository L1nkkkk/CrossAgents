//! The fixed pipeline end to end, with mock agents driven by directives (no LLM):
//! plan → approve → dispatch → verify → retry / escalate → merge → finish.

mod common;

use std::path::Path;

use common::{TestHub, git_repo, types};
use crossagents_api_types::{EventKind, RunStatus};
use serde_json::{Value, json};

/// A worker goal: run `directives`, then report completion.
fn goal(directives: &str) -> String {
    format!(r#"{directives} [call task_complete {{"task_id":"{{task_id}}","summary":"done"}}]"#)
}

/// The planner's request: submit `tasks` as the plan.
fn request(tasks: Value) -> String {
    format!("Build it. [call plan_submit {}]", json!({ "tasks": tasks }))
}

fn task(key: &str, goal: String, check: &str, deps: &[&str]) -> Value {
    json!({
        "key": key,
        "title": format!("task {key}"),
        "spec": { "goal": goal, "acceptance": [check] },
        "depends_on": deps,
    })
}

/// Passes when `file` is tracked and contains `word` (works in cmd and sh alike).
fn has(file: &str, word: &str) -> String {
    format!("git grep -q {word} -- {file}")
}

async fn start_run(t: &TestHub, req: String, extra: Value) -> String {
    let mut body = json!({ "request": req, "planner": "boss", "worker": "coder" });
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let resp = t.post("/v1/runs", body).await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let run: Value = resp.json().await.unwrap();
    run["id"].as_str().unwrap().to_owned()
}

async fn setup(repo: &Path) -> TestHub {
    let t = TestHub::start().await;
    t.add_agent_in("boss", Some("opus"), "planner", repo).await;
    t.add_agent_in("coder", Some("haiku"), "worker", repo).await;
    t
}

fn run_status_is(id: &str, want: RunStatus) -> impl Fn(&crossagents_api_types::Event) -> bool {
    let id = id.to_owned();
    move |e| {
        matches!(&e.kind, EventKind::RunStatusChanged { id: rid, to, .. }
            if rid.to_string() == id && *to == want)
    }
}

fn git_show(repo: &Path, spec: &str) -> String {
    let out = std::process::Command::new("git")
        .args(["show", spec])
        .current_dir(repo)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[tokio::test]
async fn plan_approve_retry_and_merge() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    let plan = json!([
        task(
            "a",
            goal("[write a.txt=alpha]"),
            &has("a.txt", "alpha"),
            &[]
        ),
        task(
            "b",
            goal("[write@1 b.txt=wrong] [write@2 b.txt=beta]"),
            &has("b.txt", "beta"),
            &["a"],
        ),
    ]);
    let id = start_run(&t, request(plan), json!({})).await;
    w.until(
        "plan awaiting approval",
        run_status_is(&id, RunStatus::AwaitingApproval),
    )
    .await;

    let detail: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    assert_eq!(detail["run"]["plan"].as_array().unwrap().len(), 2);
    assert!(detail["tasks"].as_array().unwrap().is_empty());

    let resp = t.post(&format!("/v1/runs/{id}/approve"), json!({})).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());

    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;

    // b failed its first check, then passed.
    let checks: Vec<(String, bool)> = w
        .seen
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::TaskChecked { key, passed, .. } => Some((key.clone(), *passed)),
            _ => None,
        })
        .collect();
    assert_eq!(
        checks,
        [("a".into(), true), ("b".into(), false), ("b".into(), true)],
        "{:?}",
        types(&w.seen)
    );

    let detail: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    let branch = detail["run"]["branch"].as_str().unwrap();
    for task in detail["tasks"].as_array().unwrap() {
        assert_eq!(task["status"], "merged", "{task}");
    }
    assert_eq!(git_show(repo.path(), &format!("{branch}:a.txt")), "alpha");
    assert_eq!(git_show(repo.path(), &format!("{branch}:b.txt")), "beta");
    // The human's checkout is untouched.
    assert!(!repo.path().join("a.txt").exists());

    // Temporary workers are gone; only the two templates remain.
    let agents: Value = t.get("/v1/agents").await.json().await.unwrap();
    let names: Vec<&str> = agents["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["boss", "coder"]);
}

#[tokio::test]
async fn failing_task_escalates_to_stronger_worker() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    t.add_agent_in("senior", Some("opus"), "worker", repo.path())
        .await;
    let mut w = t.watch().await;

    // Only the opus model writes the right content.
    let plan = json!([task(
        "c",
        goal("[write#mock-haiku c.txt=bad] [write#mock-opus c.txt=good]"),
        &has("c.txt", "good"),
        &[]
    )]);
    let id = start_run(
        &t,
        request(plan),
        json!({ "escalate": "senior", "auto_approve": true, "max_attempts": 1 }),
    )
    .await;
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;

    let detail: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    let task = &detail["tasks"][0];
    assert_eq!(task["status"], "merged");
    assert_eq!(task["escalated"], true);
    let branch = detail["run"]["branch"].as_str().unwrap();
    assert_eq!(git_show(repo.path(), &format!("{branch}:c.txt")), "good");
}

#[tokio::test]
async fn silent_worker_is_nudged_then_fails_the_run() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // The worker never reports; a dependent task must not start.
    let plan = json!([
        task("d", "[write d.txt=x]".into(), &has("d.txt", "x"), &[]),
        task("e", goal("[write e.txt=y]"), &has("e.txt", "y"), &["d"]),
    ]);
    let id = start_run(
        &t,
        request(plan),
        json!({ "auto_approve": true, "max_attempts": 1 }),
    )
    .await;
    w.until("run failed", run_status_is(&id, RunStatus::Failed))
        .await;

    let detail: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    assert_eq!(detail["tasks"][0]["status"], "failed");
    assert_eq!(detail["tasks"][0]["nudges"], 2);
    assert_eq!(detail["tasks"][1]["status"], "cancelled");
    assert!(
        detail["tasks"][0]["report"]["failure"]
            .as_str()
            .unwrap()
            .contains("stopped reporting")
    );
}

#[tokio::test]
async fn reject_then_cancel() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    let plan = json!([task("f", goal("[write f.txt=1]"), &has("f.txt", "1"), &[])]);
    let id = start_run(&t, request(plan), json!({})).await;
    w.until(
        "awaiting approval",
        run_status_is(&id, RunStatus::AwaitingApproval),
    )
    .await;

    let resp = t
        .post(
            &format!("/v1/runs/{id}/reject"),
            json!({ "reason": "smaller tasks please" }),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let detail: Value = resp.json().await.unwrap();
    assert_eq!(detail["run"]["status"], "planning");
    assert_eq!(detail["run"]["note"], "smaller tasks please");

    // Short ids work too.
    let short = &id.replace('-', "")[24..];
    let resp = t.post(&format!("/v1/runs/{short}/cancel"), json!({})).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let detail: Value = resp.json().await.unwrap();
    assert_eq!(detail["run"]["status"], "cancelled");
    assert_eq!(
        t.post(&format!("/v1/runs/{id}/approve"), json!({}))
            .await
            .status(),
        409
    );
}

#[tokio::test]
async fn tools_are_scoped_by_token_and_role() {
    let t = TestHub::start().await;
    // No agent token: rejected.
    let resp = t.get("/v1/me/tools").await;
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn run_needs_a_git_repository() {
    let t = TestHub::start().await;
    let not_repo = tempfile::tempdir().unwrap();
    t.add_agent_in("boss", None, "planner", not_repo.path())
        .await;
    t.add_agent_in("coder", None, "worker", not_repo.path())
        .await;
    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": "x", "planner": "boss", "worker": "coder" }),
        )
        .await;
    assert_eq!(resp.status(), 409, "{}", resp.text().await.unwrap());
}

#[tokio::test]
async fn reviewer_rejects_once_then_approves() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    t.add_agent_in("critic", Some("opus"), "reviewer", repo.path())
        .await;
    let mut w = t.watch().await;

    // Haiku directives are the worker's, opus directives the reviewer's.
    let goal = r#"[write@1#mock-haiku g.txt=v1] [write@2#mock-haiku g.txt=v2]
        [call#mock-haiku task_complete {"task_id":"{task_id}","summary":"wrote g"}]
        [call@1#mock-opus review_submit {"task_id":"{task_id}","approve":false,"notes":"needs v2"}]
        [call@2#mock-opus review_submit {"task_id":"{task_id}","approve":true,"notes":"lgtm"}]"#;
    let plan = json!([task("g", goal.into(), &has("g.txt", "v"), &[])]);
    let id = start_run(
        &t,
        request(plan),
        json!({ "reviewer": "critic", "auto_approve": true }),
    )
    .await;
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;

    let detail: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    let task = &detail["tasks"][0];
    assert_eq!(task["status"], "merged");
    let errors: Vec<String> = w
        .seen
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::AgentError { message, .. } => Some(message.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(task["attempts"], 2, "{errors:#?}");
    assert_eq!(task["report"]["review"], "lgtm");
    let branch = detail["run"]["branch"].as_str().unwrap();
    assert_eq!(git_show(repo.path(), &format!("{branch}:g.txt")), "v2");
    let reviews = w
        .seen
        .iter()
        .filter(|e| matches!(&e.kind, EventKind::TaskStatusChanged { to, .. } if *to == crossagents_api_types::TaskStatus::Reviewing))
        .count();
    assert_eq!(reviews, 2);
}

#[tokio::test]
async fn worker_asks_and_planner_answers() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    let answer =
        r#"[write color.txt=blue] [call task_complete {"task_id":"{task_id}","summary":"blue"}]"#;
    let worker_goal =
        r#"[call task_ask {"task_id":"{task_id}","question":"which color?"}]"#.to_owned();
    let plan = json!([task("h", worker_goal, &has("color.txt", "blue"), &[])]);
    // The planner's directives are replayed when the question (with a task_id) arrives.
    let req = format!(
        "{} [call task_answer {}]",
        request(plan),
        json!({ "task_id": "{task_id}", "answer": answer })
    );
    let id = start_run(&t, req, json!({ "auto_approve": true })).await;
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;

    let saw_question = w.seen.iter().any(|e| {
        matches!(&e.kind, EventKind::TaskStatusChanged { to, .. }
            if *to == crossagents_api_types::TaskStatus::WaitingAnswer)
    });
    assert!(saw_question);
    let detail: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    let branch = detail["run"]["branch"].as_str().unwrap();
    assert_eq!(
        git_show(repo.path(), &format!("{branch}:color.txt")),
        "blue"
    );
}

/// Models of the worker clones created for each task key.
fn clone_models(seen: &[crossagents_api_types::Event]) -> Vec<(String, Option<String>)> {
    let mut out: Vec<(String, Option<String>)> = seen
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::AgentCreated { agent } if agent.name.starts_with("w-") => {
                let key = agent.name.rsplit('-').next().unwrap().to_owned();
                Some((key, agent.model.clone()))
            }
            _ => None,
        })
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn planner_assigns_tasks_by_specialty() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    // Registered in the same repository, so it joins the run's team.
    let resp = t
        .post(
            "/v1/agents",
            json!({
                "name": "sec", "plugin": "mock", "model": "opus", "role": "worker",
                "cwd": repo.path().display().to_string(),
                "profile": { "display_name": "Rex", "title": "Security engineer",
                             "persona": "You harden code. Nothing else matters." },
            }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let mut w = t.watch().await;

    let mut a = task(
        "a",
        goal("[write a.txt=alpha]"),
        &has("a.txt", "alpha"),
        &[],
    );
    a["assignee"] = json!("sec");
    let b = task("b", goal("[write b.txt=beta]"), &has("b.txt", "beta"), &[]);
    let id = start_run(&t, request(json!([a, b])), json!({})).await;
    w.until(
        "plan awaiting approval",
        run_status_is(&id, RunStatus::AwaitingApproval),
    )
    .await;

    // The planner was told who is on the team and what they are good at.
    let planner: Value = t
        .get("/v1/agents/boss/transcript")
        .await
        .json()
        .await
        .unwrap();
    let kickoff = planner["turns"][0]["input"].as_str().unwrap();
    assert!(kickoff.contains("Your team"), "{kickoff}");
    assert!(
        kickoff.contains("- sec — Rex, Security engineer · opus. You harden code."),
        "{kickoff}"
    );
    assert!(
        kickoff.contains("- coder — no title · haiku (default)"),
        "{kickoff}"
    );
    assert!(
        !kickoff.contains("- boss"),
        "planners are not teammates: {kickoff}"
    );

    let resp = t.post(&format!("/v1/runs/{id}/approve"), json!({})).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;

    // Task a went to the specialist (cloned with its model), b to the default worker.
    assert_eq!(
        clone_models(&w.seen),
        [
            ("a".into(), Some("opus".into())),
            ("b".into(), Some("haiku".into()))
        ]
    );
    let sec: Value = t.get("/v1/agents/sec").await.json().await.unwrap();
    let detail: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    let template = |key: &str| {
        detail["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["key"] == key)
            .unwrap()["worker_template"]
            .clone()
    };
    assert_eq!(template("a"), sec["id"]);
    assert!(template("b").is_null());

    // Orchestration messages say which run and task they are about; the planner's merge
    // notices are tagged with the task that merged.
    let first = &planner["turns"][0]["from"];
    assert_eq!(first["kind"], "orchestrator", "{first}");
    assert_eq!(first["run_id"], json!(id));
    assert!(first.get("task_id").is_none(), "{first}");
    let planner: Value = t
        .get("/v1/agents/boss/transcript")
        .await
        .json()
        .await
        .unwrap();
    let task_ids: Vec<&Value> = detail["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| &t["id"])
        .collect();
    let tagged = planner["turns"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|turn| task_ids.contains(&&turn["from"]["task_id"]))
        .count();
    assert_eq!(tagged, 2, "one merge notice per task: {planner}");
}

#[tokio::test]
async fn unknown_assignee_rejects_the_plan() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let mut a = task(
        "a",
        goal("[write a.txt=alpha]"),
        &has("a.txt", "alpha"),
        &[],
    );
    a["assignee"] = json!("nobody");
    let id = start_run(&t, request(json!([a])), json!({})).await;
    w.until("planner's turn", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;

    let detail: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    assert_eq!(detail["run"]["status"], "planning");
    assert!(detail["run"]["plan"].as_array().unwrap().is_empty());
    assert!(
        !types(&w.seen).contains(&"run.plan_submitted"),
        "{:?}",
        types(&w.seen)
    );
}
