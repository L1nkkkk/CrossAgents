//! H1-5 (docs/tasks/H1.md): the lead recruits and manages teammates; workers split their
//! tasks when the run lets them. Mock agents act on directives in what they receive.

mod common;

use std::path::Path;

use common::{TestHub, git_repo};
use crossagents_api_types::{EventKind, RunStatus, TaskStatus};
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

fn accept(key: &str) -> String {
    call("task_accept", json!({ "task": key }))
}

fn finish() -> String {
    call("run_finish", json!({ "summary": "done" }))
}

/// A worker goal that writes, then reports with `summary` for its issuer to act on.
fn goal(key: &str, write: &str, summary: &str) -> String {
    format!(
        "{write} {}",
        call("task_complete", json!({ "task": key, "summary": summary }))
    )
}

fn task(key: &str, goal: String) -> Value {
    json!({ "key": key, "title": format!("task {key}"), "spec": { "goal": goal } })
}

async fn setup(repo: &Path) -> TestHub {
    let t = TestHub::start().await;
    t.add_agent_in("boss", Some("opus"), "planner", repo).await;
    t.add_agent_in("coder", Some("haiku"), "worker", repo).await;
    t
}

async fn start_run(t: &TestHub, tasks: Value, policy: Value) -> String {
    let request = format!(
        "Build it. {}",
        call("plan_submit", json!({ "tasks": tasks }))
    );
    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": request, "planner": "boss", "worker": "coder", "policy": policy }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    resp.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn completed(id: &str) -> impl Fn(&crossagents_api_types::Event) -> bool {
    let id = id.to_owned();
    move |e| {
        matches!(&e.kind, EventKind::RunStatusChanged { id: rid, to: RunStatus::Completed, .. }
            if rid.to_string() == id)
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
async fn the_lead_recruits_within_the_cap_and_assigns_by_name() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // When a is reported: recruit an opus helper, give it a new task, try a second recruit
    // (over the cap of 1), accept a.
    let b = json!({
        "key": "b", "title": "task b", "assignee": "helper",
        "spec": { "goal": goal("b", "[write b.txt=bee]", &format!("{} {}", accept("b"), finish())) },
        "depends_on": ["a"],
    });
    let lead_steps = format!(
        "{} {} {} {}",
        call(
            "agent_spawn",
            json!({ "name": "helper", "model": "opus", "title": "Helper" })
        ),
        call("task_create", json!({ "tasks": [b] })),
        call("agent_spawn", json!({ "name": "helper2" })),
        accept("a")
    );
    let a = task(
        "a",
        goal("a", "[write a.txt=ay]", &format!("a done {lead_steps}")),
    );
    let id = start_run(
        &t,
        json!([a]),
        json!({ "plan_approval": "auto", "max_agents": 1 }),
    )
    .await;
    w.until("run completed", completed(&id)).await;

    assert!(w.output().contains("at most 1"), "{}", w.output());
    let helper: Value = t.get("/v1/agents/helper").await.json().await.unwrap();
    assert_eq!(helper["config"]["spawned_by_run"], json!(id));
    assert_eq!(helper["model"], "opus");
    // b's worker was cloned from the recruit.
    let clone = w.seen.iter().find_map(|e| match &e.kind {
        EventKind::AgentCreated { agent } if agent.name.ends_with("-b") => Some(agent.clone()),
        _ => None,
    });
    assert_eq!(clone.unwrap().model.as_deref(), Some("opus"));
    let detail: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    let branch = detail["run"]["branch"].as_str().unwrap();
    assert_eq!(git_show(repo.path(), &format!("{branch}:b.txt")), "bee");
}

#[tokio::test]
async fn a_worker_splits_its_task_and_accepts_the_part() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // p's worker hands part.txt to a subtask; when the part is reported, p's worker accepts
    // it (into p's branch) and reports p, whose summary has the lead accept and finish.
    let p_report = call(
        "task_complete",
        json!({ "task": "p", "summary": format!("p with part {} {}", accept("p"), finish()) }),
    );
    let part = task(
        "part",
        goal(
            "part",
            "[write part.txt=piece]",
            &format!("{} {p_report}", accept("part")),
        ),
    );
    let p = task(
        "p",
        format!(
            "[write p.txt=main] {}",
            call("task_create", json!({ "tasks": [part] }))
        ),
    );
    let id = start_run(
        &t,
        json!([p]),
        json!({ "plan_approval": "auto", "delegate": true }),
    )
    .await;
    w.until("run completed", completed(&id)).await;

    let detail: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    let tasks = detail["tasks"].as_array().unwrap();
    let part = tasks.iter().find(|t| t["key"] == "part").unwrap();
    let p = tasks.iter().find(|t| t["key"] == "p").unwrap();
    assert_eq!(part["parent"], p["id"]);
    // The part was issued by p's worker (gone now that p merged), not by the lead.
    assert!(part["issuer"].is_string());
    assert_ne!(part["issuer"], p["issuer"]);
    let branch = detail["run"]["branch"].as_str().unwrap();
    assert_eq!(
        git_show(repo.path(), &format!("{branch}:part.txt")),
        "piece"
    );
    assert_eq!(git_show(repo.path(), &format!("{branch}:p.txt")), "main");
    assert!(w.seen.iter().any(|e| matches!(&e.kind,
        EventKind::TaskStatusChanged { key, to: TaskStatus::Merged, .. } if key == "part")));
}

#[tokio::test]
async fn without_delegate_workers_cannot_create_tasks() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let p = task(
        "p",
        call("task_create", json!({ "tasks": [task("q", "x".into())] })),
    );
    start_run(&t, json!([p]), json!({ "plan_approval": "auto" })).await;
    w.until("p stalled", |e| {
        matches!(&e.kind, EventKind::TaskStatusChanged { key, to: TaskStatus::Stalled, .. } if key == "p")
    })
    .await;
    assert!(
        w.output().contains("does not let workers delegate"),
        "{}",
        w.output()
    );
}

#[tokio::test]
async fn the_lead_moves_a_teammate_to_a_stronger_model() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let lead = format!(
        "{} {} {}",
        call(
            "agent_set_model",
            json!({ "name": "coder", "model": "opus" })
        ),
        accept("a"),
        finish()
    );
    let a = task("a", goal("a", "[write a.txt=ay]", &lead));
    let id = start_run(&t, json!([a]), json!({ "plan_approval": "auto" })).await;
    w.until("run completed", completed(&id)).await;
    let coder: Value = t.get("/v1/agents/coder").await.json().await.unwrap();
    assert_eq!(coder["model"], "opus");
}

/// A conversation outside any run recruits for itself, within its policy; every refusal says
/// why, and archiving the conversation archives its recruit.
#[tokio::test]
async fn a_conversation_recruits_within_its_policy() {
    let t = TestHub::start().await;
    let resp = t
        .post(
            "/v1/agents",
            json!({
                "name": "Chatty", "plugin": "mock", "role": "worker",
                "cwd": t.home.path().display().to_string(),
                "config": { "policy": { "max_agents": 1 } },
            }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let chatty: Value = resp.json().await.unwrap();
    let mut w = t.watch().await;

    let text = [
        call(
            "agent_spawn",
            json!({ "name": "Haiku", "model": "mock-opus" }),
        ),
        call("agent_spawn", json!({ "model": "gpt-6-luna" })),
        call("agent_spawn", json!({ "model": "mock-opus" })),
        call(
            "agent_spawn",
            json!({ "model": "mock-opus", "name": "Second" }),
        ),
    ]
    .join(" ");
    t.send("Chatty", &text).await;
    w.until("turn completed", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;
    let out = w.output();
    assert!(out.contains("vendor or model word"), "{out}");
    assert!(
        out.contains("no logged-in account runs model gpt-6-luna"),
        "{out}"
    );
    assert!(out.contains("at most 1 teammates"), "{out}");

    let agents: Value = t.get("/v1/agents").await.json().await.unwrap();
    let recruits: Vec<&Value> = agents["agents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["config"]["spawned_by_agent"] == chatty["id"])
        .collect();
    assert_eq!(recruits.len(), 1, "{agents}");
    let recruit = recruits[0];
    assert_eq!(recruit["model"], "mock-opus");
    assert_eq!(recruit["name_source"], "generated");
    assert!(
        !crossagents_core::names::is_vendor_word(recruit["name"].as_str().unwrap(), &[]),
        "{recruit}"
    );

    // Archiving the conversation archives its recruit; restoring brings both back.
    let resp = t
        .post("/v1/agents/Chatty/archive", json!({ "archived": true }))
        .await;
    assert_eq!(resp.status(), 200);
    let id = recruit["id"].as_str().unwrap();
    let archived: Value = t
        .get(&format!("/v1/agents/{id}"))
        .await
        .json()
        .await
        .unwrap();
    assert!(archived["archived_at"].is_string(), "{archived}");
    let resp = t
        .post("/v1/agents/Chatty/messages", json!({ "text": "hi" }))
        .await;
    assert_eq!(
        resp.status(),
        409,
        "an archived conversation takes no messages"
    );
    t.post("/v1/agents/Chatty/archive", json!({ "archived": false }))
        .await;
    let restored: Value = t
        .get(&format!("/v1/agents/{id}"))
        .await
        .json()
        .await
        .unwrap();
    assert!(restored["archived_at"].is_null(), "{restored}");
}
