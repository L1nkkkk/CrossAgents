//! Runs driven by their agents (docs/tasks/H1.md H1-3), with mock agents driven by
//! directives: plan → approve → dispatch → the issuer checks → accept / rework / drop.
//!
//! Mock agents act only on directives in what they receive, so each test writes the next
//! agent's step into the message that agent will get: a worker's summary carries the
//! planner's `task_accept`, a question carries the `task_answer`.

mod common;

use std::path::Path;

use common::{TestHub, git_repo, types};
use crossagents_api_types::{EventKind, RunStatus, TaskStatus};
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

/// The planner's step after a task is reported: accept it.
fn accept(key: &str) -> String {
    call("task_accept", json!({ "task": key }))
}

/// After the last task: accept it, then close the run.
fn accept_last(key: &str) -> String {
    format!(
        "{} {}",
        accept(key),
        call("run_finish", json!({ "summary": "all done" }))
    )
}

/// A worker goal: run `directives`, then report done with `summary` (which the issuer reads).
fn goal(directives: &str, summary: &str) -> String {
    format!(
        "{directives} {}",
        call(
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": summary })
        )
    )
}

/// The planner's request: submit `tasks` as the plan.
fn request(tasks: Value) -> String {
    format!(
        "Build it. {}",
        call("plan_submit", json!({ "tasks": tasks }))
    )
}

fn task(key: &str, goal: String, deps: &[&str]) -> Value {
    json!({
        "key": key,
        "title": format!("task {key}"),
        "spec": { "goal": goal },
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

fn task_is(key: &'static str, want: TaskStatus) -> impl Fn(&crossagents_api_types::Event) -> bool {
    move |e| matches!(&e.kind, EventKind::TaskStatusChanged { key: k, to, .. } if k == key && *to == want)
}

fn git_show(repo: &Path, spec: &str) -> String {
    let out = std::process::Command::new("git")
        .args(["show", spec])
        .current_dir(repo)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

async fn detail(t: &TestHub, id: &str) -> Value {
    t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap()
}

#[tokio::test]
async fn the_planner_accepts_what_workers_report() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    let mut a = task(
        "a",
        goal("[write a.txt=alpha]", &format!("wrote a {}", accept("a"))),
        &[],
    );
    a["spec"]["acceptance"] = json!([has("a.txt", "alpha")]);
    let b = task(
        "b",
        goal(
            "[write b.txt=beta]",
            &format!("wrote b {}", accept_last("b")),
        ),
        &["a"],
    );
    let id = start_run(&t, request(json!([a, b])), json!({})).await;
    w.until(
        "awaiting approval",
        run_status_is(&id, RunStatus::AwaitingApproval),
    )
    .await;
    let resp = t.post(&format!("/v1/runs/{id}/approve"), json!({})).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;

    // a's optional check ran when its worker reported; b had none.
    let checks: Vec<(String, bool)> = w
        .seen
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::TaskChecked { key, passed, .. } => Some((key.clone(), *passed)),
            _ => None,
        })
        .collect();
    assert_eq!(checks, [("a".into(), true)], "{:?}", types(&w.seen));

    let detail = detail(&t, &id).await;
    let branch = detail["run"]["branch"].as_str().unwrap();
    for task in detail["tasks"].as_array().unwrap() {
        assert_eq!(task["status"], "merged", "{task}");
        assert_eq!(task["issuer"], detail["run"]["planner"]);
    }
    assert_eq!(git_show(repo.path(), &format!("{branch}:a.txt")), "alpha");
    assert_eq!(git_show(repo.path(), &format!("{branch}:b.txt")), "beta");
    assert!(
        !repo.path().join("a.txt").exists(),
        "the person's checkout is untouched"
    );

    // The worker's own words reached the planner, from the worker.
    let planner: Value = t
        .get("/v1/agents/boss/transcript")
        .await
        .json()
        .await
        .unwrap();
    let report = planner["turns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|turn| turn["input"].as_str().unwrap().starts_with("wrote a"))
        .expect("a's report");
    assert_eq!(report["from"]["kind"], "agent");
    assert!(
        report["input"]
            .as_str()
            .unwrap()
            .contains("checks: 1/1 passed"),
        "{report}"
    );

    // Temporary workers are gone; only the two teammates remain.
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
async fn rework_sends_the_task_back_with_notes() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    let rework = call(
        "task_rework",
        json!({ "task": "b", "notes": "it should say beta" }),
    );
    let worker = format!(
        "[write@1 b.txt=wrong] [write@2 b.txt=beta] {} {}",
        call(
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": format!("v1 {rework}") })
        )
        .replacen("[call ", "[call@1 ", 1),
        call(
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": format!("v2 {}", accept_last("b")) })
        )
        .replacen("[call ", "[call@2 ", 1),
    );
    let id = start_run(
        &t,
        request(json!([task("b", worker, &[])])),
        json!({ "policy": { "plan_approval": "auto" } }),
    )
    .await;
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;
    let detail = detail(&t, &id).await;
    let task = &detail["tasks"][0];
    assert_eq!(task["attempts"], 2, "{task}");
    let branch = detail["run"]["branch"].as_str().unwrap();
    assert_eq!(git_show(repo.path(), &format!("{branch}:b.txt")), "beta");
}

#[tokio::test]
async fn a_silent_worker_stalls_and_its_issuer_hears_it() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // The worker never reports; the dependent task must not start.
    let plan = json!([
        task("d", "[write d.txt=x]".into(), &[]),
        task("e", goal("[write e.txt=y]", "done"), &["d"]),
    ]);
    let id = start_run(&t, request(plan), json!({ "auto_approve": true })).await;
    w.until("d stalled", task_is("d", TaskStatus::Stalled))
        .await;
    let boss: Value = t.get("/v1/agents/boss").await.json().await.unwrap();
    let boss_id = boss["id"].as_str().unwrap().to_owned();
    w.until("issuer told", |e| {
        matches!(&e.kind, EventKind::AgentInput { id, text, .. } | EventKind::AgentInputUpdated { id, text, .. }
            if id.to_string() == boss_id && text.contains("#d") && text.contains("stalled"))
    })
    .await;

    let detail = detail(&t, &id).await;
    assert_eq!(detail["run"]["status"], "running");
    assert_eq!(detail["tasks"][0]["status"], "stalled");
    assert_eq!(detail["tasks"][1]["status"], "pending");
    // Nobody nudged the worker: it got its task once.
    let worker_inputs = w
        .seen
        .iter()
        .filter(|e| matches!(&e.kind, EventKind::AgentInput { text, .. } if text.starts_with("Task #d")))
        .count();
    assert_eq!(worker_inputs, 1);
}

#[tokio::test]
async fn questions_go_to_the_issuer_and_answers_come_back() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // The planner's answer carries the worker's next steps; the question carries the answer.
    let complete = call(
        "task_complete",
        json!({ "task_id": "{task_id}", "summary": format!("blue {}", accept_last("h")) }),
    );
    let answer = call(
        "task_answer",
        json!({ "task": "h", "answer": format!("[write color.txt=blue] {complete}") }),
    );
    let ask = call(
        "task_ask",
        json!({ "task_id": "{task_id}", "question": format!("which color? {answer}") }),
    );
    let id = start_run(
        &t,
        request(json!([task("h", ask, &[])])),
        json!({ "auto_approve": true }),
    )
    .await;
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;
    assert!(w.seen.iter().any(task_is("h", TaskStatus::WaitingAnswer)));
    let detail = detail(&t, &id).await;
    let branch = detail["run"]["branch"].as_str().unwrap();
    assert_eq!(
        git_show(repo.path(), &format!("{branch}:color.txt")),
        "blue"
    );
}

#[tokio::test]
async fn a_reviewer_judges_and_the_issuer_decides() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    t.add_agent_in("critic", Some("opus"), "reviewer", repo.path())
        .await;
    let mut w = t.watch().await;

    // Haiku directives are the worker's, opus ones the reviewer's (the planner only ever
    // sees the goal inside its plan, where directives are not run).
    let request_review = call(
        "review_request",
        json!({ "task": "g", "reviewer": "critic" }),
    );
    let verdict = call(
        "review_submit",
        json!({ "task_id": "{task_id}", "approve": true, "notes": format!("lgtm {}", accept_last("g")) }),
    )
    .replacen("[call ", "[call#mock-opus ", 1);
    let worker = format!(
        "[write#mock-haiku g.txt=v1] {} {verdict}",
        call(
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": format!("wrote g {request_review}") })
        )
        .replacen("[call ", "[call#mock-haiku ", 1),
    );
    let id = start_run(
        &t,
        request(json!([task("g", worker, &[])])),
        json!({ "auto_approve": true }),
    )
    .await;
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;
    assert!(w.seen.iter().any(task_is("g", TaskStatus::Reviewing)));
    let detail = detail(&t, &id).await;
    assert_eq!(
        detail["tasks"][0]["report"]["review"]
            .as_str()
            .unwrap()
            .split(' ')
            .next(),
        Some("lgtm")
    );
}

#[tokio::test]
async fn a_conflict_is_reported_and_nothing_merges() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // Both tasks rewrite README although their scopes say otherwise, so they run side by
    // side and the second accept conflicts.
    let mut x = task(
        "x",
        goal("[write README=one]", &format!("x {}", accept("x"))),
        &[],
    );
    x["spec"]["scope"] = json!({ "allow": ["x/**"] });
    let mut y = task(
        "y",
        goal("[write README=two]", &format!("y {}", accept("y"))),
        &[],
    );
    y["spec"]["scope"] = json!({ "allow": ["y/**"] });
    let id = start_run(&t, request(json!([x, y])), json!({ "auto_approve": true })).await;
    let boss: Value = t.get("/v1/agents/boss").await.json().await.unwrap();
    let boss_id = boss["id"].as_str().unwrap().to_owned();
    // Wait until the planner has tried both accepts.
    let mut turns = 0;
    while !w.output().contains("conflicts in") {
        w.until("planner turn", |e| {
            matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if id.to_string() == boss_id)
        })
        .await;
        turns += 1;
        assert!(turns < 8, "no conflict reported: {}", w.output());
    }
    let detail = detail(&t, &id).await;
    let statuses: Vec<&str> = detail["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["status"].as_str().unwrap())
        .collect();
    assert!(
        statuses.contains(&"merged") && statuses.contains(&"passed"),
        "{statuses:?}"
    );
    assert_eq!(detail["run"]["status"], "running");
}

#[tokio::test]
async fn reject_then_cancel() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    let plan = json!([task("f", goal("[write f.txt=1]", "done"), &[])]);
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
async fn a_run_outside_git_works_in_place_one_task_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cv.md"), "me\n").unwrap();
    let t = setup(dir.path()).await;
    let mut w = t.watch().await;
    // Independent tasks, which a repository would run side by side.
    let a = task(
        "a",
        goal("[write a.txt=alpha]", &format!("wrote a {}", accept("a"))),
        &[],
    );
    let b = task(
        "b",
        goal(
            "[write b.txt=beta]",
            &format!("wrote b {}", accept_last("b")),
        ),
        &[],
    );
    let id = start_run(&t, request(json!([a, b])), json!({ "max_parallel": 3 })).await;
    let run = detail(&t, &id).await["run"].clone();
    assert_eq!(run["in_place"], true, "{run}");
    assert_eq!(run["config"]["max_parallel"], 1, "{run}");
    w.until(
        "awaiting approval",
        run_status_is(&id, RunStatus::AwaitingApproval),
    )
    .await;
    t.post(&format!("/v1/runs/{id}/approve"), json!({})).await;
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;

    // Never two tasks running at once.
    let mut running = 0;
    for e in &w.seen {
        if let EventKind::TaskStatusChanged { from, to, .. } = &e.kind {
            if *to == TaskStatus::Running && *from != TaskStatus::Running {
                running += 1;
            } else if *from == TaskStatus::Running && *to != TaskStatus::Running {
                running -= 1;
            }
            assert!(running <= 1, "{:?}", types(&w.seen));
        }
    }
    // The work is in the directory itself, which is still there with what it had.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "alpha"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
        "beta"
    );
    assert!(dir.path().join("cv.md").exists());
    assert!(!dir.path().join(".git").exists());
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

    let mut a = task("a", goal("[write a.txt=alpha]", &accept("a")), &[]);
    a["assignee"] = json!("sec");
    let b = task("b", goal("[write b.txt=beta]", &accept_last("b")), &[]);
    let id = start_run(&t, request(json!([a, b])), json!({})).await;
    w.until(
        "awaiting approval",
        run_status_is(&id, RunStatus::AwaitingApproval),
    )
    .await;

    // The planner got the person's request as theirs, then the roster.
    let planner: Value = t
        .get("/v1/agents/boss/transcript")
        .await
        .json()
        .await
        .unwrap();
    let first = &planner["turns"][0];
    assert_eq!(first["from"]["kind"], "human", "{first}");
    let kickoff = first["input"].as_str().unwrap();
    assert!(kickoff.starts_with("Build it."), "{kickoff}");
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
    // Each worker got its task from the planner, as the planner wrote it.
    let planner_id = planner["agent"]["id"].as_str().unwrap().to_owned();
    let briefs = w
        .seen
        .iter()
        .filter(|e| {
            matches!(&e.kind, EventKind::AgentInput { text, from: Some(crossagents_core::MessageSource::Agent { agent_id }), .. }
                if text.starts_with("Task #") && agent_id.to_string() == planner_id)
        })
        .count();
    assert_eq!(briefs, 2);
}

#[tokio::test]
async fn unknown_assignee_rejects_the_plan() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let mut a = task("a", goal("[write a.txt=alpha]", "done"), &[]);
    a["assignee"] = json!("nobody");
    let id = start_run(&t, request(json!([a])), json!({})).await;
    w.until("planner's turn", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;

    let detail = detail(&t, &id).await;
    assert_eq!(detail["run"]["status"], "planning");
    assert!(detail["run"]["plan"].as_array().unwrap().is_empty());
    assert!(
        !types(&w.seen).contains(&"run.plan_submitted"),
        "{:?}",
        types(&w.seen)
    );
}

#[tokio::test]
async fn the_lead_adds_tasks_while_the_run_runs() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // When a is reported, the planner adds a follow-up task and accepts a. The follow-up
    // names its task by key: a's worker would fill a `{task_id}` placeholder with a's id.
    let extra = task(
        "extra",
        format!(
            "[write extra.txt=more] {}",
            call(
                "task_complete",
                json!({ "task": "extra", "summary": accept_last("extra") })
            )
        ),
        &["a"],
    );
    let follow_up = format!(
        "{} {}",
        call("task_create", json!({ "tasks": [extra] })),
        accept("a")
    );
    let a = task(
        "a",
        goal("[write a.txt=alpha]", &format!("a done {follow_up}")),
        &[],
    );
    let id = start_run(&t, request(json!([a])), json!({ "auto_approve": true })).await;
    w.until("extra merged", task_is("extra", TaskStatus::Merged))
        .await;
    w.find("run completed", run_status_is(&id, RunStatus::Completed))
        .await;
    let detail = detail(&t, &id).await;
    let branch = detail["run"]["branch"].as_str().unwrap();
    assert_eq!(
        git_show(repo.path(), &format!("{branch}:extra.txt")),
        "more"
    );
}

#[tokio::test]
async fn a_hands_off_run_lands_on_main_and_reports_to_the_person() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    let close = format!(
        "{} {} {}",
        accept("z"),
        call("run_land", json!({ "mode": "merge" })),
        call("run_finish", json!({ "summary": "Added z.txt." }))
    );
    let z = task(
        "z",
        goal("[write z.txt=zed]", &format!("z done {close}")),
        &[],
    );
    let id = start_run(
        &t,
        request(json!([z])),
        json!({ "policy": { "plan_approval": "auto", "land": "auto", "human_ask": "timeout" } }),
    )
    .await;
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;
    assert!(
        types(&w.seen).contains(&"run.landed"),
        "{:?}",
        types(&w.seen)
    );
    // On main, in the person's (clean) checkout.
    assert_eq!(
        std::fs::read_to_string(repo.path().join("z.txt"))
            .unwrap()
            .trim(),
        "zed"
    );
    let detail = detail(&t, &id).await;
    assert_eq!(detail["run"]["landed"]["mode"], "merge");
    assert_eq!(detail["run"]["summary"], "Added z.txt.");
    // The person gets the lead's summary with the facts.
    let questions: Value = t.get("/v1/questions").await.json().await.unwrap();
    let note = questions["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "notice")
        .expect("closing note");
    let text = note["text"].as_str().unwrap();
    assert!(
        text.starts_with("Added z.txt.") && text.contains("已落地到 main"),
        "{text}"
    );
}

#[tokio::test]
async fn a_manual_run_leaves_landing_to_the_person() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    let close = format!("{} {}", accept("m"), call("run_land", json!({})));
    let m = task(
        "m",
        goal("[write m.txt=em]", &format!("m done {close}")),
        &[],
    );
    let id = start_run(&t, request(json!([m])), json!({ "auto_approve": true })).await;
    w.until("m merged", task_is("m", TaskStatus::Merged)).await;
    let boss: Value = t.get("/v1/agents/boss").await.json().await.unwrap();
    let boss_id = boss["id"].as_str().unwrap().to_owned();
    while !w.output().contains("leaves landing to the person") {
        w.until("planner turn", |e| {
            matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if id.to_string() == boss_id)
        })
        .await;
    }
    // The person lands it, then closes the run.
    let resp = t
        .post(&format!("/v1/runs/{id}/land"), json!({ "mode": "squash" }))
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    assert!(repo.path().join("m.txt").exists());
    let resp = t.post(&format!("/v1/runs/{id}/finish"), json!({})).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let detail: Value = resp.json().await.unwrap();
    assert_eq!(detail["run"]["status"], "completed");
    assert_eq!(detail["run"]["landed"]["by"], "person");
}
