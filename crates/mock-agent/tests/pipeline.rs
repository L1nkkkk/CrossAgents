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
use crossagents_core::NoticeClass;
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

/// The planner's step after a task is reported: accept it. Most tasks here have no checks, so
/// the accept is a forced one (docs/TODO.md §九 ①): it says why it takes the work unverified.
fn accept(key: &str) -> String {
    call(
        "task_accept",
        json!({ "task": key, "force": true, "reason": "no checks configured" }),
    )
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

    let mut a = task("a", goal("[write a.txt=alpha]", "wrote a"), &[]);
    a["spec"]["acceptance"] = json!([has("a.txt", "alpha")]);
    let b = task("b", goal("[write b.txt=beta]", "wrote b"), &["a"]);
    let id = start_run(&t, request(json!([a, b])), json!({})).await;
    w.until(
        "awaiting approval",
        run_status_is(&id, RunStatus::AwaitingApproval),
    )
    .await;
    let resp = t.post(&format!("/v1/runs/{id}/approve"), json!({})).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    // The lead is not handed the reports: it accepts each once it waits for it.
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "a", &accept("a")).await;
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "b", &accept_last("b")).await;
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

    // The planner is not handed the worker's report: it was told that a waited for it, with
    // its checks' result (the notice is what it reads).
    let said = w
        .seen
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::NoticeRecorded { line, .. }
                if line.contains("#a waits for your acceptance") =>
            {
                Some(line.clone())
            }
            _ => None,
        })
        .expect("a's acceptance notice");
    assert!(said.contains("checks passed"), "{said}");

    // The tasks' execution sessions are archived, not deleted (docs/tasks/S1.md §3.4): only
    // the two teammates are left in the lists, and each task's worker can still be read.
    let agents: Value = t.get("/v1/agents").await.json().await.unwrap();
    let (archived, listed): (Vec<&Value>, Vec<&Value>) = agents["agents"]
        .as_array()
        .unwrap()
        .iter()
        .partition(|a| !a["archived_at"].is_null());
    let names = |v: &[&Value]| -> Vec<String> {
        v.iter()
            .map(|a| a["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(names(&listed), ["boss", "coder"]);
    let archived = names(&archived);
    assert_eq!(archived.len(), 2, "{archived:?}");
    assert!(archived.iter().all(|n| n.starts_with("w-")), "{archived:?}");
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
            json!({ "task_id": "{task_id}", "summary": "v1" })
        )
        .replacen("[call ", "[call@1 ", 1),
        call(
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": "v2" })
        )
        .replacen("[call ", "[call@2 ", 1),
    );
    let id = start_run(
        &t,
        request(json!([task("b", worker, &[])])),
        json!({ "policy": { "plan_approval": "auto" } }),
    )
    .await;
    // The lead is not handed the reports: it sends the rework once the first waits, then accepts.
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "b", &rework).await;
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "b", &accept_last("b")).await;
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
async fn the_issuer_can_accept_a_stalled_task() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // The worker does the work but never reports; the issuer checks it and takes it anyway.
    let plan = json!([task("d", "[write d.txt=x]".into(), &[])]);
    let id = start_run(&t, request(plan), json!({ "auto_approve": true })).await;
    w.until("d stalled", task_is("d", TaskStatus::Stalled))
        .await;
    t.send("boss", &accept_last("d")).await;
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;

    let detail = detail(&t, &id).await;
    assert_eq!(detail["tasks"][0]["status"], "merged");
    let branch = detail["run"]["branch"].as_str().unwrap();
    assert_eq!(git_show(repo.path(), &format!("{branch}:d.txt")), "x");
}

#[tokio::test]
async fn questions_go_to_the_issuer_and_answers_come_back() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // The planner's answer carries the worker's next steps; the question carries the answer.
    let complete = call(
        "task_complete",
        json!({ "task_id": "{task_id}", "summary": "blue" }),
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
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "h", &accept_last("h")).await;
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
        json!({ "task_id": "{task_id}", "reviewed_sha": "{reviewed_sha}", "approve": true, "notes": "lgtm" }),
    )
    .replacen("[call ", "[call#mock-opus ", 1);
    let worker = format!(
        "[write#mock-haiku g.txt=v1] {} {verdict}",
        call(
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": "wrote g" })
        )
        .replacen("[call ", "[call#mock-haiku ", 1),
    );
    let id = start_run(
        &t,
        request(json!([task("g", worker, &[])])),
        json!({ "auto_approve": true }),
    )
    .await;
    // The lead is not handed the report: it asks for the review once the task waits, then
    // accepts the reviewed task.
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "g", &request_review).await;
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "g", &accept_last("g")).await;
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;
    assert!(w.seen.iter().any(task_is("g", TaskStatus::Reviewing)));
    // Opening the review settled the notice that the task waited to be accepted.
    assert!(
        w.seen.iter().any(|e| matches!(&e.kind,
            EventKind::NoticeRecorded { topic, line, .. }
                if topic.ends_with(":accept") && line.contains("is in review"))),
        "the acceptance notice was settled when the review opened"
    );
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
    let mut x = task("x", goal("[write README=one]", "x"), &[]);
    x["spec"]["scope"] = json!({ "allow": ["x/**"] });
    let mut y = task("y", goal("[write README=two]", "y"), &[]);
    y["spec"]["scope"] = json!({ "allow": ["y/**"] });
    let id = start_run(&t, request(json!([x, y])), json!({ "auto_approve": true })).await;
    // The lead is not handed the reports: it tries both accepts once each waits.
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "x", &accept("x")).await;
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "y", &accept("y")).await;
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

/// docs/tasks/LEAD.md §10 item 4: a directory outside version control has nothing to
/// snapshot, so a run whose lead works there gives no task out. The refusal is readable and
/// leaves the directory as it was.
#[tokio::test]
async fn a_run_outside_git_refuses_to_hand_work_to_a_worker() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cv.md"), "me\n").unwrap();
    let t = setup(dir.path()).await;
    let mut w = t.watch().await;
    let a = task(
        "a",
        goal("[write a.txt=alpha]", &format!("wrote a {}", accept("a"))),
        &[],
    );
    let b = task(
        "b",
        goal("[write b.txt=beta]", &format!("wrote b {}", accept("b"))),
        &[],
    );
    let id = start_run(&t, request(json!([a, b])), json!({ "max_parallel": 3 })).await;
    let run = detail(&t, &id).await["run"].clone();
    assert_eq!(run["in_place"], true, "{run}");
    assert_eq!(
        run["config"]["max_parallel"], 1,
        "one directory still means one task at a time: {run}"
    );
    w.until(
        "awaiting approval",
        run_status_is(&id, RunStatus::AwaitingApproval),
    )
    .await;
    let resp = t.post(&format!("/v1/runs/{id}/approve"), json!({})).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());

    // Neither task starts: each says why, in words a person can read.
    w.find("a failed", task_is("a", TaskStatus::Failed)).await;
    w.find("b failed", task_is("b", TaskStatus::Failed)).await;
    let stored = detail(&t, &id).await;
    for task in stored["tasks"].as_array().unwrap() {
        let failure = task["report"]["failure"].as_str().unwrap_or_default();
        assert!(
            failure.contains("version control") && failure.contains("no task"),
            "{}: {failure:?}",
            task["key"]
        );
        assert!(task["worktree"].is_null(), "{}: {task}", task["key"]);
    }
    // Nothing was written in the person's directory, and it is still not a repository.
    assert!(!dir.path().join("a.txt").exists());
    assert!(!dir.path().join("b.txt").exists());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("cv.md")).unwrap(),
        "me\n"
    );
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

    let mut a = task("a", goal("[write a.txt=alpha]", "a done"), &[]);
    a["assignee"] = json!("sec");
    let b = task("b", goal("[write b.txt=beta]", "b done"), &[]);
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
    // The lead is not handed the reports: it accepts each once it waits for it.
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "a", &accept("a")).await;
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "b", &accept_last("b")).await;
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

/// A chat session is a `planner` by role, yet the lead may give it a task or ask it for a
/// review like anyone: a role is how a session was made, not what it may do.
#[tokio::test]
async fn chat_sessions_can_take_tasks_and_review() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    t.add_agent_in("pal", Some("haiku"), "planner", repo.path())
        .await;
    t.add_agent_in("leo", Some("opus"), "planner", repo.path())
        .await;
    let mut w = t.watch().await;

    let request_review = call("review_request", json!({ "task": "g", "reviewer": "leo" }));
    let verdict = call(
        "review_submit",
        json!({ "task_id": "{task_id}", "reviewed_sha": "{reviewed_sha}", "approve": true, "notes": "lgtm" }),
    )
    .replacen("[call ", "[call#mock-opus ", 1);
    let worker = format!(
        "[write#mock-haiku g.txt=v1] {} {verdict}",
        call(
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": "wrote g" })
        )
        .replacen("[call ", "[call#mock-haiku ", 1),
    );
    let mut g = task("g", worker, &[]);
    g["assignee"] = json!("pal");
    let id = start_run(
        &t,
        request(json!([g])),
        json!({ "auto_approve": true, "team": ["coder", "pal", "leo"] }),
    )
    .await;
    // The lead is not handed the report: it asks for the review once the task waits.
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "g", &request_review).await;
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "g", &accept_last("g")).await;
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;
    assert!(w.seen.iter().any(task_is("g", TaskStatus::Reviewing)));
    let detail = detail(&t, &id).await;
    assert!(
        detail["tasks"][0]["report"]["review"]
            .as_str()
            .is_some_and(|r| r.starts_with("lgtm")),
        "{detail}"
    );
    // Named in the team, they are on the lead's roster like anyone else.
    let lead: Value = t
        .get("/v1/agents/boss/transcript")
        .await
        .json()
        .await
        .unwrap();
    let kickoff = lead["turns"][0]["input"].as_str().unwrap();
    assert!(
        kickoff.contains(
            "
team (name one as a task's assignee or reviewer):"
        ),
        "{kickoff}"
    );
    assert!(
        kickoff.contains("- pal — no title · haiku") && kickoff.contains("- leo — no title · opus"),
        "{kickoff}"
    );
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
                json!({ "task": "extra", "summary": "extra done" })
            )
        ),
        &["a"],
    );
    let follow_up = format!(
        "{} {}",
        call("task_create", json!({ "tasks": [extra] })),
        accept("a")
    );
    let a = task("a", goal("[write a.txt=alpha]", "a done"), &[]);
    let id = start_run(&t, request(json!([a])), json!({ "auto_approve": true })).await;
    // The lead is not handed the reports: when a waits, it adds the follow-up and accepts a;
    // when the follow-up waits, it accepts that.
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "a", &follow_up).await;
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "extra", &accept_last("extra")).await;
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
        // Commit messages written to the project's rules.
        call(
            "task_accept",
            json!({ "task": "z", "message": "ledger: add z", "force": true, "reason": "no checks configured" })
        ),
        call(
            "run_land",
            json!({ "mode": "merge", "message": "ledger: land z" })
        ),
        call("run_finish", json!({ "summary": "Added z.txt." }))
    );
    let z = task("z", goal("[write z.txt=zed]", "z done"), &[]);
    let id = start_run(
        &t,
        request(json!([z])),
        json!({ "policy": { "plan_approval": "auto", "land": "auto", "human_ask": "timeout" } }),
    )
    .await;
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "z", &close).await;
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
    let log = std::process::Command::new("git")
        .args(["log", "--format=%s", "main"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&log.stdout);
    assert!(
        log.contains("ledger: add z")
            && log.contains("ledger: land z")
            && !log.contains("crossagents:"),
        "{log}"
    );
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
async fn a_lead_that_takes_over_a_dropped_task_completes_the_run() {
    let repo = git_repo();
    // The project's checks look for the lead's part.
    std::fs::write(
        repo.path().join("crossagents.toml"),
        format!("[run]\nchecks = [\"{}\"]\n", has("rest.txt", "rest")),
    )
    .unwrap();
    for args in [
        &["add", "-A"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "checks",
        ],
    ] {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // The worker never reports; the lead drops its task and does the rest itself.
    let plan = json!([task("d", "[write d.txt=x]".into(), &[])]);
    let id = start_run(&t, request(plan), json!({ "auto_approve": true })).await;
    w.until("d stalled", task_is("d", TaskStatus::Stalled))
        .await;
    let take_over = format!(
        "{} [write rest.txt=rest] {}",
        call("task_drop", json!({ "task": "d", "reason": "silent" })),
        call("run_finish", json!({ "summary": "did it myself" }))
    );
    t.send("boss", &take_over).await;
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;

    let detail = detail(&t, &id).await;
    assert_eq!(detail["tasks"][0]["status"], "cancelled");
    assert!(
        detail["run"]["note"]
            .as_str()
            .unwrap()
            .contains("passed the checks"),
        "{detail}"
    );
    // And the person can land it.
    let resp = t
        .post(&format!("/v1/runs/{id}/land"), json!({ "mode": "squash" }))
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    assert!(repo.path().join("rest.txt").exists());
}

/// Commits everything in the person's checkout of `repo`.
fn commit_all(repo: &Path, message: &str) {
    for args in [
        &["add", "-A"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            message,
        ],
    ] {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }
}

/// A run whose project check fails once `bad.txt` exists; task `z` is merged into it, then
/// `before_land` runs on the person's checkout and the lead calls `run_land`. Returns the
/// run's detail once the lead heard back.
async fn land_after(before_land: impl FnOnce(&Path)) -> (tempfile::TempDir, Value) {
    let check = if cfg!(windows) {
        "if exist bad.txt exit 1"
    } else {
        "test ! -e bad.txt"
    };
    land_with(&format!("[run]\nchecks = [\"{check}\"]\n"), before_land).await
}

/// `land_after` with `config` as the project's crossagents.toml.
async fn land_with(config: &str, before_land: impl FnOnce(&Path)) -> (tempfile::TempDir, Value) {
    let repo = git_repo();
    std::fs::write(repo.path().join("crossagents.toml"), config).unwrap();
    commit_all(repo.path(), "checks");
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    let z = task("z", goal("[write z.txt=zed]", "z done"), &[]);
    let id = start_run(
        &t,
        request(json!([z])),
        json!({ "policy": { "plan_approval": "auto", "land": "auto", "human_ask": "timeout" } }),
    )
    .await;
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "z", &accept("z")).await;
    w.until("z merged", task_is("z", TaskStatus::Merged)).await;
    before_land(repo.path());
    let boss: Value = t.get("/v1/agents/boss").await.json().await.unwrap();
    let boss_id = boss["id"].as_str().unwrap().to_owned();
    t.send("boss", &call("run_land", json!({ "mode": "merge" })))
        .await;
    while !w.output().contains("\"landed\"") && !w.output().contains("project's checks") {
        w.until("planner turn", |e| {
            matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if id.to_string() == boss_id)
        })
        .await;
    }
    let output = w.output();
    let mut detail = detail(&t, &id).await;
    detail["lead_output"] = output.into();
    (repo, detail)
}

#[tokio::test]
async fn the_lead_lands_only_what_passed_the_checks_after_the_base_came_in() {
    // main gains a file that breaks the check while the run is open.
    let (repo, detail) = land_after(|repo| {
        std::fs::write(repo.join("bad.txt"), "breaks the check").unwrap();
        commit_all(repo, "break the check");
    })
    .await;
    let output = detail["lead_output"].as_str().unwrap();
    assert!(
        output.contains("the project's checks (crossagents.toml) failed"),
        "{output}"
    );
    assert!(detail["run"]["landed"].is_null(), "{detail}");
    assert!(!repo.path().join("z.txt").exists());
    assert!(!git_show(repo.path(), "main:z.txt").contains("zed"));
    let checks = detail["run"]["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1, "{detail}");
    assert_eq!(checks[0]["passed"], false);
    assert_eq!(checks[0]["exit_code"], 1);
}

#[tokio::test]
async fn the_lead_lands_when_the_base_keeps_the_checks_passing() {
    let (repo, detail) = land_after(|repo| {
        std::fs::write(repo.join("fine.txt"), "harmless").unwrap();
        commit_all(repo, "harmless change");
    })
    .await;
    assert_eq!(detail["run"]["landed"]["mode"], "merge", "{detail}");
    assert_eq!(git_show(repo.path(), "main:z.txt"), "zed");
    assert_eq!(git_show(repo.path(), "main:fine.txt"), "harmless");
    assert_eq!(detail["run"]["checks"][0]["passed"], true, "{detail}");
}

#[tokio::test]
async fn a_check_for_other_paths_does_not_hold_up_landing() {
    // The check would fail, but it is only for `crates`, and the run writes `z.txt`.
    let (repo, detail) = land_with(
        "[run]\nchecks = [{ command = \"exit 1\", paths = [\"crates\"] }]\n",
        |_| {},
    )
    .await;
    assert_eq!(detail["run"]["landed"]["mode"], "merge", "{detail}");
    assert_eq!(git_show(repo.path(), "main:z.txt"), "zed");
    assert!(
        detail["run"]["checks"].as_array().is_none_or(Vec::is_empty),
        "{detail}"
    );
}

#[tokio::test]
async fn a_manual_run_leaves_landing_to_the_person() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    let close = format!("{} {}", accept("m"), call("run_land", json!({})));
    let m = task("m", goal("[write m.txt=em]", "m done"), &[]);
    let id = start_run(&t, request(json!([m])), json!({ "auto_approve": true })).await;
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "m", &close).await;
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

#[tokio::test]
async fn the_approval_settles_policy_caps_and_who_does_each_task() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    // Not in the repository, so not on the run's team until the person picks it.
    let helper = t.add_agent("helper", Some("sonnet")).await;
    let mut w = t.watch().await;

    let a = task("a", goal("[write a.txt=alpha]", "wrote a"), &[]);
    let b = task("b", goal("[write b.txt=beta]", "wrote b"), &[]);
    let id = start_run(&t, request(json!([a, b])), json!({})).await;
    w.until(
        "awaiting approval",
        run_status_is(&id, RunStatus::AwaitingApproval),
    )
    .await;

    let resp = t
        .post(
            &format!("/v1/runs/{id}/approve"),
            json!({ "assignees": { "nope": "helper" } }),
        )
        .await;
    assert_eq!(resp.status(), 409, "{}", resp.text().await.unwrap());

    let mut policy = detail(&t, &id).await["run"]["config"]["policy"].clone();
    policy["land"] = json!("auto");
    policy["budget"]["max_usd"] = json!(5.0);
    let resp = t
        .post(
            &format!("/v1/runs/{id}/approve"),
            json!({ "policy": policy, "max_parallel": 1, "assignees": { "a": "helper", "b": null } }),
        )
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());

    // The lead hears who does what, and what the person changed.
    let told = w
        .until("the lead hears the approval", |e| {
            matches!(&e.kind, EventKind::AgentInput { text, .. } if text.contains("was approved"))
        })
        .await;
    let EventKind::AgentInput { text, .. } = &told.kind else {
        unreachable!()
    };
    assert!(
        text.contains("#a 由模板 helper (sonnet) 派生的新执行会话执行"),
        "{text}"
    );
    assert!(
        text.contains("#a goes to helper (your plan said the default worker)"),
        "{text}"
    );
    assert!(text.contains("you may land the result yourself"), "{text}");
    assert!(text.contains("budget: $5"), "{text}");
    assert!(text.contains("at most 1 task(s)"), "{text}");

    let d = detail(&t, &id).await;
    let config = &d["run"]["config"];
    assert_eq!(config["max_parallel"], 1);
    assert_eq!(config["policy"]["land"], "auto");
    assert_eq!(config["policy"]["budget"]["max_usd"], 5.0);
    assert!(
        config["team"]
            .as_array()
            .unwrap()
            .contains(&helper.id.to_string().into()),
        "{config}"
    );
    let tasks = d["tasks"].as_array().unwrap();
    let template =
        |key: &str| tasks.iter().find(|t| t["key"] == key).unwrap()["worker_template"].clone();
    assert_eq!(template("a"), json!(helper.id));
    assert_eq!(template("b"), Value::Null);
    // One at a time: b waits while a runs.
    let running = tasks.iter().filter(|t| t["status"] == "running").count();
    assert_eq!(running, 1, "{tasks:?}");
}

#[tokio::test]
async fn asking_again_replaces_the_open_question() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // The worker asks, learns more in the same turn and asks again in other words. The second
    // wording replaces the first (it used to be refused: "WaitingAnswer -> WaitingAnswer"),
    // and the answer to it gets the task done.
    let complete = call(
        "task_complete",
        json!({ "task_id": "{task_id}", "summary": "green" }),
    );
    let answer = call(
        "task_answer",
        json!({ "task": "q", "answer": format!("[write color.txt=green] {complete}") }),
    );
    let asks = format!(
        "{} {}",
        call(
            "task_ask",
            json!({ "task_id": "{task_id}", "question": "which color?" })
        ),
        call(
            "task_ask",
            json!({ "task_id": "{task_id}", "question": format!("which color, now that I synced? {answer}") })
        ),
    );
    let id = start_run(
        &t,
        request(json!([task("q", asks, &[])])),
        json!({ "auto_approve": true }),
    )
    .await;
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "q", &accept_last("q")).await;
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;
    let detail = detail(&t, &id).await;
    let branch = detail["run"]["branch"].as_str().unwrap();
    assert_eq!(
        git_show(repo.path(), &format!("{branch}:color.txt")),
        "green"
    );
    // The issuer was asked the second wording: the re-ask went through.
    let boss = t
        .get("/v1/agents/boss/transcript")
        .await
        .text()
        .await
        .unwrap();
    assert!(boss.contains("now that I synced"), "{boss}");
    let coder = t
        .get("/v1/agents/coder/transcript")
        .await
        .text()
        .await
        .unwrap();
    assert!(!coder.contains("invalid task transition"), "{coder}");
}

#[tokio::test]
async fn a_task_starts_beside_a_branch_a_dropped_attempt_left() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // `a` never reports, so `r` waits; meanwhile a branch is left where `r`'s would go, as a
    // dropped attempt whose checkout was kept leaves it. `r` must start beside it, not fail.
    let r = task("r", goal("[write r.txt=ok]", "r done"), &["a"]);
    let plan = json!([task("a", "[write a.txt=alpha]".into(), &[]), r]);
    let id = start_run(&t, request(plan), json!({ "auto_approve": true })).await;
    w.until("a stalled", task_is("a", TaskStatus::Stalled))
        .await;
    let branch = detail(&t, &id).await["run"]["branch"]
        .as_str()
        .unwrap()
        .to_owned();
    let leftover = format!("{branch}-r");
    let out = std::process::Command::new("git")
        .args(["branch", &leftover, &branch])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    t.send("boss", &accept("a")).await;
    // r reports once a is in; the lead accepts it when it waits.
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "r", &accept_last("r")).await;
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;
    let detail = detail(&t, &id).await;
    let r = detail["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == "r")
        .unwrap();
    assert_eq!(r["status"], "merged", "{detail}");
    assert_eq!(git_show(repo.path(), &format!("{branch}:r.txt")), "ok");
    // The branch it found is left as it was.
    assert!(
        std::process::Command::new("git")
            .args([
                "rev-parse",
                "--verify",
                "-q",
                &format!("refs/heads/{leftover}")
            ])
            .current_dir(repo.path())
            .status()
            .unwrap()
            .success()
    );
}

/// Tasks in `spec.stage` are a batch: no task of the next stage starts before the batch has
/// ended, and the lead is woken with the batch's results when it does
/// (docs/research/multica.md §2.2).
#[tokio::test]
async fn a_stage_wakes_the_lead_and_holds_the_next_one_back() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // a is stage 1; b is stage 2 though it depends on nothing. Their scopes do not overlap,
    // so only the stage holds b back.
    let mut a = task("a", goal("[write a.txt=alpha]", "a done"), &[]);
    let mut b = task("b", goal("[write b.txt=beta]", "b done".into()), &[]);
    for (task, key, stage) in [(&mut a, "a", 1), (&mut b, "b", 2)] {
        task["spec"]["stage"] = json!(stage);
        task["spec"]["scope"] = json!({ "allow": [format!("{key}.txt")] });
    }
    let plan = json!([a, b]);
    let id = start_run(&t, request(plan), json!({ "auto_approve": true })).await;
    // The lead is not handed the report: it accepts a once it waits. The end of the stage then
    // releases the next one; nothing else makes b start.
    common::lead_accepts_when_waiting(&t, &mut w, "boss", "a", &accept("a")).await;
    w.until("b started", task_is("b", TaskStatus::Running))
        .await;

    // b started only after stage 1 had ended.
    let (mut ended_a, mut started_b) = (None, None);
    for (i, e) in w.seen.iter().enumerate() {
        if let EventKind::TaskStatusChanged { key, to, .. } = &e.kind {
            match (key.as_str(), to) {
                ("a", TaskStatus::Merged) => ended_a = Some(i),
                ("b", TaskStatus::Running) => started_b = Some(i),
                _ => {}
            }
        }
    }
    let ended_a = ended_a.expect("a merged");
    let started_b = started_b.expect("b started");
    assert!(started_b > ended_a, "b started before stage 1 ended");

    // The lead heard the batch and that the next stage may go.
    let boss: Value = t.get("/v1/agents/boss").await.json().await.unwrap();
    let boss_id = boss["id"].as_str().unwrap().to_owned();
    let texts: Vec<&str> = w
        .seen
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::AgentInput { id, text, .. }
            | EventKind::AgentInputUpdated { id, text, .. }
                if id.to_string() == boss_id =>
            {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect();
    let stage = texts
        .iter()
        .find(|text| text.contains("Stage 1 of run"))
        .unwrap_or_else(|| panic!("no stage 1 notice: {texts:?}"));
    assert!(stage.contains("- #a merged"), "{stage}");
    assert!(stage.contains("Stage 2 may start now"), "{stage}");

    let detail = detail(&t, &id).await;
    let status = |key: &str| {
        detail["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|task| task["key"] == key)
            .unwrap()["status"]
            .clone()
    };
    assert_eq!(status("a"), "merged");
    assert_eq!(status("b"), "running");
}

/// The notice that task `key` waits for its acceptance.
fn waiting_for(key: &'static str) -> impl Fn(&crossagents_api_types::Event) -> bool {
    let said = format!("#{key} waits for your acceptance");
    move |e| matches!(&e.kind, EventKind::NoticeRecorded { line, .. } if line.contains(&said))
}

#[tokio::test]
async fn several_waiting_tasks_are_accepted_in_one_call_each_with_its_own_result() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // Their scopes do not overlap, so they run side by side.
    let mut a = task("a", goal("[write a.txt=alpha]", "a done"), &[]);
    a["spec"]["scope"] = json!({ "allow": ["a.txt"] });
    let mut c = task("c", goal("[write c.txt=gamma]", "c done"), &[]);
    c["spec"]["scope"] = json!({ "allow": ["c.txt"] });
    let id = start_run(
        &t,
        request(json!([a, c])),
        json!({ "auto_approve": true, "max_parallel": 2 }),
    )
    .await;
    w.find("a waits", waiting_for("a")).await;
    w.find("c waits", waiting_for("c")).await;

    // One call for both, and one for a key that names nothing: each comes back on its own.
    t.send(
        "boss",
        &call(
            "task_accept",
            json!({ "tasks": ["a", "c", "nope"], "force": true, "reason": "no checks configured" }),
        ),
    )
    .await;
    // The merges come before the answer is printed, so each is looked for among what was read.
    w.find(
        "the results come back",
        |e| matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains("\"results\"")),
    )
    .await;
    w.find("a merged", task_is("a", TaskStatus::Merged)).await;
    w.find("c merged", task_is("c", TaskStatus::Merged)).await;
    let answer = w.output();
    assert!(answer.contains("\"refused\""), "{answer}");
    assert!(answer.contains("nope"), "{answer}");

    let detail = detail(&t, &id).await;
    for key in ["a", "c"] {
        let task = detail["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["key"] == key)
            .unwrap();
        assert_eq!(task["status"], "merged", "{task}");
    }
}

#[tokio::test]
async fn a_task_whose_acceptance_releases_downstream_work_is_an_unblock() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    let a = task("a", goal("[write a.txt=alpha]", "a done"), &[]);
    let b = task("b", goal("[write b.txt=beta]", "b done"), &["a"]);
    let id = start_run(&t, request(json!([a, b])), json!({ "auto_approve": true })).await;
    w.until("a waits", waiting_for("a")).await;

    // Accepting a releases b, so the lead is woken at once for it, not after a quiet minute.
    let line = w
        .seen
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::NoticeRecorded { line, class, .. }
                if line.contains("#a waits for your acceptance") =>
            {
                assert_eq!(*class, NoticeClass::Unblock, "{line}");
                Some(line.clone())
            }
            _ => None,
        })
        .expect("a's notice");
    assert!(line.contains("unlocks #b"), "{line}");

    t.send("boss", &accept("a")).await;
    w.until("b waits", waiting_for("b")).await;
    t.send("boss", &accept_last("b")).await;
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;
}

#[tokio::test]
async fn a_task_that_leaves_another_still_waiting_says_so_in_its_notice() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // b waits for a and for c. Accepting a does not unlock b while c is still open.
    let scoped = |key: &str, goal: String, deps: &[&str]| {
        let mut t = task(key, goal, deps);
        t["spec"]["scope"] = json!({ "allow": [format!("{key}.txt")] });
        t
    };
    let plan = json!([
        scoped("a", goal("[write a.txt=alpha]", "a done"), &[]),
        scoped("c", goal("[write c.txt=gamma]", "c done"), &[]),
        scoped("b", goal("[write b.txt=beta]", "b done"), &["a", "c"]),
    ]);
    let _id = start_run(
        &t,
        request(plan),
        json!({ "auto_approve": true, "max_parallel": 3 }),
    )
    .await;
    let waiting = w.until("a waits", waiting_for("a")).await;
    let EventKind::NoticeRecorded { line, class, .. } = &waiting.kind else {
        unreachable!()
    };
    assert!(line.contains("#b still waits for #c"), "{line}");
    assert!(!line.contains("unlocks"), "{line}");
    assert_eq!(*class, NoticeClass::Unblock, "{line}");
}
