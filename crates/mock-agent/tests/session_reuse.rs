//! Sessions belong to the workspace, runs only record who took part (docs/tasks/S1.md): a run
//! is led by the agent picked, itself, and lets go of it when it ends; deleting a run touches
//! nobody another run holds.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{TestHub, Watcher, git_repo, native_plugin, pinned_plugin};
use crossagents_api_types::{AgentId, EventKind, RunStatus};
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

fn same_dir(a: &Path, b: &Path) -> bool {
    std::fs::canonicalize(a).ok() == std::fs::canonicalize(b).ok()
}

/// `Orchestrator::short`: the id without dashes, last 8 characters.
fn short(id: &str) -> String {
    let id = id.replace('-', "");
    id[id.len() - 8..].to_owned()
}

async fn start_run(t: &TestHub, planner: &str, request: &str) -> Value {
    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": request, "planner": planner }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    resp.json().await.unwrap()
}

async fn run_ends(w: &mut Watcher, run: &Value) {
    let id = run["id"].as_str().unwrap().to_owned();
    w.until("run completed", |e| {
        matches!(&e.kind, EventKind::RunStatusChanged { id: rid, to: RunStatus::Completed, .. }
            if rid.to_string() == id)
    })
    .await;
}

/// Waits for `agent` to be back in `home`.
async fn back_home(w: &mut Watcher, agent: AgentId, home: &Path) {
    w.find("back home", |e| {
        matches!(&e.kind, EventKind::AgentUpdated { agent: a } if a.id == agent && same_dir(&a.cwd, home))
    })
    .await;
}

async fn idle_lead(t: &TestHub, id: AgentId) {
    tokio::time::timeout(common::wait_limit(), async {
        loop {
            let a = t.agent(&id.to_string()).await;
            if a.status == crossagents_api_types::AgentStatus::Idle
                && !t.hub.runtime.is_busy(id).await
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("lead did not finish its queued turns");
}

async fn retained_lead(t: &TestHub, w: &mut Watcher, id: AgentId, repo: &Path) {
    w.find("lead finished its turn", |e| {
        matches!(&e.kind,
        EventKind::AgentTurnCompleted { id: ended, .. } if *ended == id)
    })
    .await;
    idle_lead(t, id).await;
    let retained = t.agent(&id.to_string()).await;
    assert!(retained.archived_at.is_none());
    assert!(retained.cwd.is_dir());
    assert!(!same_dir(&retained.cwd, repo));
}

async fn agent_count(t: &TestHub) -> usize {
    let all: Value = t
        .get("/v1/agents?archived=true")
        .await
        .json()
        .await
        .unwrap();
    all["agents"].as_array().unwrap().len()
}

fn finish() -> String {
    call("run_finish", json!({ "summary": "done" }))
}

#[tokio::test]
async fn run_b_is_led_by_run_as_lead_itself() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    let pia = t.add_native("Pia", "planner", repo.path()).await;
    let mut w = t.watch().await;

    let a = start_run(&t, "Pia", &format!("Run A. [session] {}", finish())).await;
    assert_eq!(a["planner"], json!(pia.id), "the lead is Pia, not a copy");
    run_ends(&mut w, &a).await;
    // Run A ended and released occupancy while keeping her session and checkout.
    retained_lead(&t, &mut w, pia.id, repo.path()).await;
    let first = w.output();
    let session = first
        .split("session=")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .unwrap_or_else(|| panic!("no session in {first}"))
        .to_owned();
    let before = agent_count(&t).await;

    let b = start_run(&t, "Pia", &format!("Run B. [session] [cwd] {}", finish())).await;
    // Pia herself, with run A's conversation: the same session resumed in run B's checkout.
    assert_eq!(b["planner"], json!(pia.id), "{b}");
    assert_eq!(agent_count(&t).await, before, "no agent was added");
    assert!(
        b["lead_note"].is_null(),
        "she was not copied: {}",
        b["lead_note"]
    );
    run_ends(&mut w, &b).await;
    let second = w.output()[first.len()..].to_owned();
    assert!(
        second.contains(&format!("session={session} (resumed)")),
        "{second}"
    );
    let cwd = second
        .split("cwd=")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .unwrap();
    assert!(
        cwd.contains(&short(b["id"].as_str().unwrap())) && cwd.contains("integration"),
        "{cwd}"
    );
    retained_lead(&t, &mut w, pia.id, repo.path()).await;
    let claims = t.hub.bus.store().list_claims(None).await.unwrap();
    assert!(claims.is_empty(), "{claims:?}");
}

#[tokio::test]
async fn deleting_an_old_run_leaves_the_run_that_holds_its_lead_now() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    let pia = t.add_native("Pia", "planner", repo.path()).await;
    let mut w = t.watch().await;

    let a = start_run(&t, "Pia", &format!("Run A. {}", finish())).await;
    run_ends(&mut w, &a).await;
    retained_lead(&t, &mut w, pia.id, repo.path()).await;

    // Run B holds her now, working in its checkout.
    let b = start_run(&t, "Pia", "Run B. Wait for my word.").await;
    let b_id = b["id"].as_str().unwrap().to_owned();
    w.until(
        "run B's first turn",
        |e| matches!(e.kind, EventKind::AgentTurnCompleted { id, .. } if id == pia.id),
    )
    .await;
    let in_b = t.agent("Pia").await.cwd;
    assert!(
        in_b.display().to_string().contains(&short(&b_id)),
        "{in_b:?}"
    );

    // Run A took part in her past, not her present: deleting it leaves her where she is.
    let resp = t
        .delete(&format!("/v1/runs/{}", a["id"].as_str().unwrap()))
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let done: Value = resp.json().await.unwrap();
    assert_eq!(
        done["agents"],
        json!({ "returned": [], "archived": [] }),
        "{done}"
    );
    assert_eq!(t.agent("Pia").await.cwd, in_b);
    let claim = t
        .hub
        .bus
        .store()
        .claim_of(pia.id)
        .await
        .unwrap()
        .expect("run B still holds her");
    assert_eq!(claim.run_id.to_string(), b_id);

    // Run B goes on and finishes as usual.
    t.send("Pia", &format!("Go ahead. {}", finish())).await;
    run_ends(&mut w, &b).await;
    retained_lead(&t, &mut w, pia.id, repo.path()).await;
}

// ---------------------------------------------------------------------------------------
// Assigning work: a new execution session by default, the session itself with `fresh: false`
// (S1 §3.3).

/// `[call plan_submit {tasks}]` whose strings hide their `[` (as `[`): the lead's mock
/// acts on markers anywhere in its kickoff, and the plan is in there. The worker gets the
/// task's goal decoded, markers and all.
fn plan(tasks: Value) -> String {
    fn hide(v: &mut Value) {
        match v {
            Value::String(s) => *s = s.replace('[', "\u{1}"),
            Value::Array(a) => a.iter_mut().for_each(hide),
            Value::Object(o) => o.values_mut().for_each(hide),
            _ => {}
        }
    }
    let mut args = json!({ "tasks": tasks });
    hide(&mut args);
    format!(
        "[call plan_submit {}]",
        args.to_string().replace("\\u0001", "\\u005b")
    )
}

fn accept(key: &str) -> String {
    call(
        "task_accept",
        json!({ "task": key, "force": true, "reason": "no checks configured" }),
    )
}

/// A worker's goal: `markers`, then report done with a summary carrying the lead's next step.
fn goal(markers: &str, then: &str) -> String {
    format!(
        "{markers} {}",
        call(
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": format!("done {then}") })
        )
    )
}

/// The lead's steps once `key`, the last task, is reported: accept it, close the run.
fn accept_and_finish(key: &str) -> String {
    format!("{} {}", accept(key), finish())
}

/// A task for `assignee`; `fresh: None` leaves the field out (the default).
fn task(key: &str, goal: String, assignee: Option<&str>, fresh: Option<bool>) -> Value {
    let mut t = json!({ "key": key, "title": format!("task {key}"), "spec": { "goal": goal } });
    if let Some(a) = assignee {
        t["assignee"] = json!(a);
    }
    if let Some(f) = fresh {
        t["fresh"] = json!(f);
    }
    t
}

async fn run_with(t: &TestHub, planner: &str, worker: &str, request: String, auto: bool) -> Value {
    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": request, "planner": planner, "worker": worker, "auto_approve": auto }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    resp.json().await.unwrap()
}

/// What `agent` said so far.
fn said(w: &Watcher, agent: AgentId) -> String {
    w.seen
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::AgentOutput { id, text, .. } if *id == agent => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// The word after the last `marker` in `text`.
fn after<'a>(text: &'a str, marker: &str) -> &'a str {
    text.rsplit(marker)
        .next()
        .filter(|_| text.contains(marker))
        .and_then(|s| s.split_whitespace().next())
        .unwrap_or_else(|| panic!("no {marker} in {text}"))
}

async fn tasks_of(t: &TestHub, run: &Value) -> Vec<Value> {
    let detail: Value = t
        .get(&format!("/v1/runs/{}", run["id"].as_str().unwrap()))
        .await
        .json()
        .await
        .unwrap();
    detail["tasks"].as_array().unwrap().clone()
}

async fn awaiting_approval(w: &mut Watcher, run: &Value) {
    let id = run["id"].as_str().unwrap().to_owned();
    w.find("awaiting approval", |e| {
        matches!(&e.kind, EventKind::RunStatusChanged { id: rid, to: RunStatus::AwaitingApproval, .. }
            if rid.to_string() == id)
    })
    .await;
}

async fn approve(t: &TestHub, run: &Value) -> reqwest::Response {
    t.post(
        &format!("/v1/runs/{}/approve", run["id"].as_str().unwrap()),
        json!({}),
    )
    .await
}

async fn turn_of(w: &mut Watcher, agent: AgentId) {
    w.until(
        "turn completed",
        |e| matches!(e.kind, EventKind::AgentTurnCompleted { id, .. } if id == agent),
    )
    .await;
}

#[tokio::test]
async fn fresh_false_gives_a_task_to_the_session_run_a_used() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    let pia = t.add_native("Pia", "planner", repo.path()).await;
    let rex = t.add_native("Rex", "worker", repo.path()).await;
    let mut w = t.watch().await;

    let a_task = task(
        "a",
        goal("[session] [write a.txt=1]", &accept_and_finish("a")),
        Some("Rex"),
        Some(false),
    );
    let a = run_with(
        &t,
        "Pia",
        "Rex",
        format!("Run A. {}", plan(json!([a_task]))),
        true,
    )
    .await;
    run_ends(&mut w, &a).await;
    back_home(&mut w, rex.id, repo.path()).await;
    // Pia leads run B too: once run A has let go of her.
    retained_lead(&t, &mut w, pia.id, repo.path()).await;
    let session = after(&said(&w, rex.id), "session=").to_owned();
    let before = agent_count(&t).await;

    let b_task = task(
        "b",
        goal("[session] [cwd] [write b.txt=2]", &accept_and_finish("b")),
        Some("Rex"),
        Some(false),
    );
    let b = run_with(
        &t,
        "Pia",
        "Rex",
        format!("Run B. [call agent_list {{}}] {}", plan(json!([b_task]))),
        true,
    )
    .await;
    run_ends(&mut w, &b).await;
    // The lead was told who works in the workspace, and what each did last (S1 §3.6).
    let kickoff = w
        .seen
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::AgentInput { id, text, .. }
                if *id == pia.id && text.starts_with("Run B.") =>
            {
                Some(text.clone())
            }
            _ => None,
        })
        .unwrap();
    assert!(kickoff.contains("sessions in this workspace"), "{kickoff}");
    assert!(kickoff.contains("fresh: false"), "{kickoff}");
    let rex_line = kickoff
        .lines()
        .find(|l| l.starts_with("- Rex ·"))
        .unwrap_or_else(|| panic!("{kickoff}"));
    assert!(
        rex_line.contains("idle") && rex_line.contains("last: run \"Run A."),
        "{rex_line}"
    );
    let listed = said(&w, pia.id);
    assert!(
        listed.contains("\"state\"") && listed.contains("\"last_execution\""),
        "{listed}"
    );
    // Rex himself got it: the same agent, his conversation resumed in run B's worktree.
    let tasks = tasks_of(&t, &b).await;
    assert_eq!(tasks[0]["handoffs"][0]["to"], json!(rex.id), "{tasks:?}");
    // The execution record says it was Rex himself, and that he delivered (S1 §3.5).
    assert_eq!(tasks[0]["handoffs"][0]["fresh"], false, "{tasks:?}");
    assert_eq!(tasks[0]["handoffs"][0]["outcome"], "delivered", "{tasks:?}");
    let text = said(&w, rex.id);
    assert!(
        text.contains(&format!("session={session} (resumed)")),
        "{text}"
    );
    let cwd = after(&text, "cwd=");
    assert!(
        cwd.contains(&short(b["id"].as_str().unwrap())) && cwd.ends_with('b'),
        "{cwd}"
    );
    assert_eq!(
        agent_count(&t).await,
        before,
        "no execution session was started"
    );
    // Merged: back in the workspace, and still there.
    back_home(&mut w, rex.id, repo.path()).await;
    assert!(t.agent("Rex").await.archived_at.is_none());
}

#[tokio::test]
async fn a_new_execution_session_is_archived_and_can_be_used_again_once_restored() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    let pia = t.add_native("Pia", "planner", repo.path()).await;
    let rex = t.add_native("Rex", "worker", repo.path()).await;
    let mut w = t.watch().await;

    // No fresh: a new execution session of Rex does it.
    let a_task = task(
        "a",
        goal("[session] [write a.txt=1]", &accept_and_finish("a")),
        None,
        None,
    );
    let a = run_with(
        &t,
        "Pia",
        "Rex",
        format!("Run A. {}", plan(json!([a_task]))),
        true,
    )
    .await;
    run_ends(&mut w, &a).await;
    retained_lead(&t, &mut w, pia.id, repo.path()).await;
    let tasks = tasks_of(&t, &a).await;
    let exec_id: AgentId = tasks[0]["handoffs"][0]["to"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_ne!(exec_id, rex.id);
    assert_eq!(tasks[0]["handoffs"][0]["fresh"], true, "{tasks:?}");
    assert_eq!(tasks[0]["handoffs"][0]["outcome"], "delivered", "{tasks:?}");
    // Archived, not deleted: its conversation can still be read.
    let mut exec = Value::Null;
    for _ in 0..100 {
        exec = t
            .get(&format!("/v1/agents/{exec_id}"))
            .await
            .json()
            .await
            .unwrap();
        if !exec["archived_at"].is_null() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(!exec["archived_at"].is_null(), "{exec}");
    let transcript: Value = t
        .get(&format!("/v1/agents/{exec_id}/transcript"))
        .await
        .json()
        .await
        .unwrap();
    assert!(
        !transcript["turns"].as_array().unwrap().is_empty(),
        "{transcript}"
    );
    let exec_session = after(&said(&w, exec_id), "session=").to_owned();

    // Run B uses it itself, restoring it first.
    let name = exec["name"].as_str().unwrap();
    let mut b_task = task(
        "b",
        goal("[session] [write b.txt=2]", &accept_and_finish("b")),
        Some(name),
        Some(false),
    );
    b_task["restore"] = json!(true);
    let b = run_with(
        &t,
        "Pia",
        "Rex",
        format!("Run B. {}", plan(json!([b_task]))),
        true,
    )
    .await;
    run_ends(&mut w, &b).await;
    let tasks = tasks_of(&t, &b).await;
    assert_eq!(tasks[0]["handoffs"][0]["to"], json!(exec_id), "{tasks:?}");
    let text = said(&w, exec_id);
    assert!(
        text.contains(&format!("session={exec_session} (resumed)")),
        "{text}"
    );
    // Used itself this time: back home, not archived.
    back_home(&mut w, exec_id, repo.path()).await;
    let exec: Value = t
        .get(&format!("/v1/agents/{exec_id}"))
        .await
        .json()
        .await
        .unwrap();
    assert!(exec["archived_at"].is_null(), "{exec}");
}

/// S1 §7: an execution session whose driver moves keeps its conversation when its run is
/// deleted: restored, the next message resumes the same session, with no warning.
#[tokio::test]
async fn an_execution_session_restored_after_its_run_is_deleted_carries_on() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    let pia = t.add_native("Pia", "planner", repo.path()).await;
    t.add_native("Rex", "worker", repo.path()).await;
    let mut w = t.watch().await;
    let a_task = task(
        "a",
        goal("[session] [write a.txt=1]", &accept_and_finish("a")),
        None,
        None,
    );
    let a = run_with(
        &t,
        "Pia",
        "Rex",
        format!("Run A. {}", plan(json!([a_task]))),
        true,
    )
    .await;
    run_ends(&mut w, &a).await;
    retained_lead(&t, &mut w, pia.id, repo.path()).await;
    let tasks = tasks_of(&t, &a).await;
    let exec_id: AgentId = tasks[0]["handoffs"][0]["to"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    w.find("archived", |e| {
        matches!(&e.kind, EventKind::AgentUpdated { agent } if agent.id == exec_id && agent.archived_at.is_some())
    })
    .await;
    let exec_session = after(&said(&w, exec_id), "session=").to_owned();

    let resp = t
        .delete(&format!("/v1/runs/{}", a["id"].as_str().unwrap()))
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let resp = t
        .post(
            &format!("/v1/agents/{exec_id}/archive"),
            json!({ "archived": false }),
        )
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let before = w.seen.len();
    t.send(&exec_id.to_string(), "[session] still you?").await;
    w.until(
        "answered",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == exec_id),
    )
    .await;
    let text = said(&w, exec_id);
    assert!(
        text.contains(&format!("session={exec_session} (resumed)")),
        "{text}"
    );
    let warned: Vec<&String> = w.seen[before..]
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::AgentWarning { id, message } if *id == exec_id => Some(message),
            _ => None,
        })
        .collect();
    assert!(warned.is_empty(), "{warned:?}");
}

#[tokio::test]
async fn two_runs_taking_one_session_at_once_only_one_gets_it() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    let rex = t.add_native("Rex", "worker", repo.path()).await;
    t.add_agent_in("Ada", None, "planner", repo.path()).await;
    t.add_agent_in("Bea", None, "planner", repo.path()).await;
    let mut w = t.watch().await;
    let a_task = task("a", "[cwd] wait".into(), Some("Rex"), Some(false));
    let b_task = task("b", "[cwd] wait".into(), Some("Rex"), Some(false));
    let runs = [
        run_with(
            &t,
            "Ada",
            "Rex",
            format!("Run A. {}", plan(json!([a_task]))),
            false,
        )
        .await,
        run_with(
            &t,
            "Bea",
            "Rex",
            format!("Run B. {}", plan(json!([b_task]))),
            false,
        )
        .await,
    ];
    for run in &runs {
        awaiting_approval(&mut w, run).await;
    }
    // Two runs, two run locks: only the claim in the store decides.
    let (ra, rb) = tokio::join!(approve(&t, &runs[0]), approve(&t, &runs[1]));
    assert_eq!((ra.status().as_u16(), rb.status().as_u16()), (200, 200));

    let mut tasks = Vec::new();
    for run in &runs {
        tasks.push(tasks_of(&t, run).await.remove(0));
    }
    let running: Vec<&Value> = tasks.iter().filter(|t| t["status"] == "running").collect();
    let waiting: Vec<&Value> = tasks.iter().filter(|t| t["status"] == "pending").collect();
    assert_eq!((running.len(), waiting.len()), (1, 1), "{tasks:?}");
    assert_eq!(running[0]["assignee"], json!(rex.id));
    // The other run is told who has him.
    let winner = running[0]["run_id"].as_str().unwrap();
    let why = waiting[0]["progress"].as_str().unwrap_or_default();
    assert!(why.contains("Rex is busy") && why.contains(winner), "{why}");
    let claim = t.hub.bus.store().claim_of(rex.id).await.unwrap().unwrap();
    assert_eq!(claim.run_id.to_string(), winner);
}

#[tokio::test]
async fn a_persons_waiting_message_comes_before_a_task() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    let rex = t.add_native("Rex", "worker", repo.path()).await;
    t.add_agent_in("Ada", None, "planner", repo.path()).await;
    let mut w = t.watch().await;
    let a_task = task("a", "[cwd] go".into(), Some("Rex"), Some(false));
    let run = run_with(
        &t,
        "Ada",
        "Rex",
        format!("Run. {}", plan(json!([a_task]))),
        false,
    )
    .await;
    awaiting_approval(&mut w, &run).await;

    // Busy with a notice, a person's message waiting behind it.
    t.hub
        .runtime
        .send(
            &crossagents_core::AgentKey::Id(rex.id),
            "[sleep 1500] notice".into(),
            crossagents_core::MessageSource::Notice,
        )
        .await
        .unwrap();
    t.send("Rex", "a question from the person").await;
    let resp = approve(&t, &run).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let task = tasks_of(&t, &run).await.remove(0);
    assert_eq!(task["status"], "pending", "{task}");
    assert!(
        task["progress"]
            .as_str()
            .unwrap_or_default()
            .contains("person"),
        "{task}"
    );

    // The person's message is answered, and no task was put in front of it.
    w.until("the person answered", |e| {
        matches!(&e.kind, EventKind::AgentOutput { id, text, .. } if *id == rex.id && text.contains("a question from the person"))
    })
    .await;
    assert!(
        !w.seen.iter().any(|e| matches!(&e.kind,
            EventKind::AgentInput { id, text, .. } if *id == rex.id && text.contains("task_id:"))),
        "a task reached Rex"
    );
    assert!(t.hub.bus.store().claim_of(rex.id).await.unwrap().is_none());
}

#[tokio::test]
async fn the_person_interrupts_an_execution() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    let pia = t.add_native("Pia", "planner", repo.path()).await;
    let rex = t.add_native("Rex", "worker", repo.path()).await;
    let mut w = t.watch().await;
    let a_task = task(
        "a",
        "[sleep 30000] working".into(),
        Some("Rex"),
        Some(false),
    );
    let run = run_with(
        &t,
        "Pia",
        "Rex",
        format!("Run. {}", plan(json!([a_task]))),
        true,
    )
    .await;
    w.until("Rex at work", |e| {
        matches!(&e.kind, EventKind::AgentOutput { id, text, .. } if *id == rex.id && text.contains("sleeping"))
    })
    .await;
    let now: Value = t
        .get("/v1/agents/Rex/execution")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(now["execution"]["run_id"], run["id"], "{now}");
    assert_eq!(now["execution"]["task_key"], "a", "{now}");

    let resp = t
        .post("/v1/agents/Rex/execution/interrupt", json!({}))
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let task: Value = resp.json().await.unwrap();
    assert_eq!(task["status"], "stalled", "{task}");
    assert!(task["assignee"].is_null(), "{task}");
    assert_eq!(task["handoffs"][0]["outcome"], "interrupted", "{task}");
    // Let go, back home, and the lead hears it.
    let now: Value = t
        .get("/v1/agents/Rex/execution")
        .await
        .json()
        .await
        .unwrap();
    assert!(now["execution"].is_null(), "{now}");
    back_home(&mut w, rex.id, repo.path()).await;
    w.find("the lead hears it", |e| match &e.kind {
        EventKind::AgentInput { id, text, .. } | EventKind::AgentInputUpdated { id, text, .. } => {
            *id == pia.id && text.contains("The person interrupted Rex")
        }
        _ => false,
    })
    .await;
}

#[tokio::test]
async fn taking_a_session_that_cannot_resume_there_fails_and_starts_no_new_one() {
    let repo = git_repo();
    // Every worktree of this repository refuses `--resume`.
    std::fs::write(repo.path().join("mock-no-resume"), "").unwrap();
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
            "marker",
        ],
    ] {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(repo.path())
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }
    let t = TestHub::start_with(native_plugin).await;
    let pia = t.add_native("Pia", "planner", repo.path()).await;
    let rex = t.add_native("Rex", "worker", repo.path()).await;
    let mut w = t.watch().await;
    t.send("Rex", "hello").await;
    turn_of(&mut w, rex.id).await;
    let session = t.hub.bus.store().last_session_id(rex.id).await.unwrap();
    assert!(session.is_some());
    let before = agent_count(&t).await;

    let a_task = task("a", "[cwd] go".into(), Some("Rex"), Some(false));
    let run = run_with(
        &t,
        "Pia",
        "Rex",
        format!("Run. {}", plan(json!([a_task]))),
        true,
    )
    .await;
    // A notice in the lead's digest: a new message, or one still waiting, updated.
    w.find("the lead hears it", |e| match &e.kind {
        EventKind::AgentInput { id, text, .. } | EventKind::AgentInputUpdated { id, text, .. } => {
            *id == pia.id && text.contains("上下文没能延续")
        }
        _ => false,
    })
    .await;
    let task = tasks_of(&t, &run).await.remove(0);
    assert_eq!(task["status"], "pending", "{task}");
    let why = task["progress"].as_str().unwrap_or_default();
    assert!(why.contains("its context was not carried over"), "{task}");
    // The execution is on record as Rex himself, failed, and why.
    let card = &task["handoffs"][0];
    assert_eq!(
        (card["fresh"].clone(), card["outcome"].clone()),
        (json!(false), json!("failed")),
        "{task}"
    );
    assert!(
        card["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("上下文没能延续"),
        "{task}"
    );
    // No session took its place, nobody new, nothing held, and Rex is home.
    assert_eq!(
        t.hub.bus.store().last_session_id(rex.id).await.unwrap(),
        session
    );
    assert_eq!(agent_count(&t).await, before);
    assert!(t.hub.bus.store().claim_of(rex.id).await.unwrap().is_none());
    assert!(same_dir(&t.agent("Rex").await.cwd, repo.path()));
}

#[tokio::test]
async fn a_session_whose_driver_cannot_move_is_refused() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    let pia = t.add_native("Pia", "planner", repo.path()).await;
    // ACP: no `move_cwd`.
    t.add_agent_in("coder", None, "worker", repo.path()).await;
    let mut w = t.watch().await;
    let a_task = task("a", "go".into(), Some("coder"), Some(false));
    let run = run_with(
        &t,
        "Pia",
        "coder",
        format!("Run. {}", plan(json!([a_task]))),
        true,
    )
    .await;
    turn_of(&mut w, pia.id).await;
    let text = said(&w, pia.id);
    assert!(
        text.contains("cannot resume its session in another directory"),
        "{text}"
    );
    assert!(text.contains("leave fresh out"), "{text}");
    let detail: Value = t
        .get(&format!("/v1/runs/{}", run["id"].as_str().unwrap()))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(detail["run"]["status"], "planning", "{detail}");
}

/// Ivy's CLI resumes a session only where it ran (`move_cwd = false`) and she has a session,
/// so the run gets a copy of her as lead. Finish retains it; explicit archival removes
/// its checkout. Restoring rebuilds at the same path, and the copy
/// carries on its own session there, with what it was told.
async fn pinned_run(
    t: &TestHub,
    w: &mut Watcher,
    repo: &Path,
) -> (AgentId, String, std::path::PathBuf) {
    let ivy = t.add_on("pinned", "Ivy", "planner", repo).await;
    t.send("Ivy", "[session] hello").await;
    w.until(
        "Ivy answered",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == ivy.id),
    )
    .await;
    let run = start_run(
        t,
        "Ivy",
        &format!("Run. [remember PELICAN-42] [session] {}", finish()),
    )
    .await;
    assert_ne!(run["planner"], json!(ivy.id), "a copy leads: {run}");
    assert!(!run["lead_note"].is_null(), "{run}");
    let lead: AgentId = run["planner"].as_str().unwrap().parse().unwrap();
    run_ends(w, &run).await;
    // The turn that closed the run is still finishing: wait for it to end (it may still be
    // answering), then for the copy to be archived with its checkout gone (S1 §3.4). Both
    // land after the run's status changes, and leaving the turn's events unread would let
    // the restore below answer to the old turn.
    w.until(
        "the lead finished the turn that closed the run",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == lead),
    )
    .await;
    let retained = t.agent(&lead.to_string()).await;
    assert!(retained.archived_at.is_none());
    assert!(retained.cwd.is_dir(), "finish retains the checkout");
    let resp = t
        .post(
            &format!("/v1/agents/{lead}/archive"),
            json!({ "archived": true }),
        )
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let mut copy = t.agent(&lead.to_string()).await;
    for _ in 0..300 {
        if copy.archived_at.is_some() && !copy.cwd.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        copy = t.agent(&lead.to_string()).await;
    }
    assert!(copy.archived_at.is_some(), "{copy:?}");
    assert!(!copy.cwd.exists(), "{copy:?}");
    let first = after(&said(w, lead), "session=").to_owned();
    (lead, first, copy.cwd)
}

fn warnings_since(w: &Watcher, from: usize, agent: AgentId) -> Vec<String> {
    w.seen[from..]
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::AgentWarning { id, message } if *id == agent => Some(message.clone()),
            _ => None,
        })
        .collect()
}

async fn restore_and_ask(t: &TestHub, w: &mut Watcher, lead: AgentId) -> String {
    let resp = t
        .post(
            &format!("/v1/agents/{lead}/archive"),
            json!({ "archived": false }),
        )
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    // The checkout is made as part of restoring (S1 §7): wait for it rather than race the
    // event that builds it.
    let mut rebuilt = false;
    for _ in 0..300 {
        if t.agent(&lead.to_string()).await.cwd.is_dir() {
            rebuilt = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(rebuilt, "restoring {lead} did not bring its checkout back");
    let before = said(w, lead).len();
    t.send(&lead.to_string(), "[recall] [session] still you?")
        .await;
    w.until(
        "answered",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == lead),
    )
    .await;
    said(w, lead)[before..].to_owned()
}

#[tokio::test]
async fn a_copy_that_resumes_only_where_it_ran_carries_on_in_its_rebuilt_checkout() {
    let repo = git_repo();
    let sessions = tempfile::tempdir().unwrap();
    let t = TestHub::start_with(pinned_plugin(sessions.path())).await;
    let mut w = t.watch().await;
    let (lead, first, checkout) = pinned_run(&t, &mut w, repo.path()).await;

    let from = w.seen.len();
    let text = restore_and_ask(&t, &mut w, lead).await;
    assert!(checkout.is_dir(), "{checkout:?}");
    assert!(
        text.contains(&format!("session={first} (resumed)")),
        "{text}"
    );
    assert!(text.contains("recall=PELICAN-42"), "{text}");
    // The conversation carried on: nothing said it did not. The run's branch went with the
    // checkout (the run never landed), so the code being rebuilt from the base is said, and
    // that is not about the conversation.
    let warned = warnings_since(&w, from, lead);
    assert!(
        warned
            .iter()
            .all(|m| !m.contains("不带原来的上下文") && !m.contains("这是新会话")),
        "{warned:?}"
    );
}

/// The same copy once its run is deleted: no checkout to rebuild, so it does not try to
/// resume elsewhere (the CLI would refuse anyway): a new session, said on restore and again
/// when it opens.
#[tokio::test]
async fn a_copy_that_resumes_only_where_it_ran_starts_over_after_its_run_is_deleted_and_says_so() {
    let repo = git_repo();
    let sessions = tempfile::tempdir().unwrap();
    let t = TestHub::start_with(pinned_plugin(sessions.path())).await;
    let mut w = t.watch().await;
    let (lead, first, _) = pinned_run(&t, &mut w, repo.path()).await;
    let runs: Value = t.get("/v1/runs").await.json().await.unwrap();
    let run = runs["runs"][0]["id"].as_str().unwrap().to_owned();
    let resp = t.delete(&format!("/v1/runs/{run}")).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());

    let from = w.seen.len();
    let text = restore_and_ask(&t, &mut w, lead).await;
    assert!(text.contains("(new)") && !text.contains(&first), "{text}");
    assert!(text.contains("recall= "), "nothing carried over: {text}");
    let warned = warnings_since(&w, from, lead);
    assert!(
        warned.iter().any(|m| m.contains("不带原来的上下文"))
            && warned.iter().any(|m| m.contains("这是新会话")),
        "{warned:?}"
    );
}

#[tokio::test]
async fn a_finished_work_can_start_another_round_and_keeps_the_previous_workspace() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    let pia = t.add_native("Pia", "planner", repo.path()).await;
    let resp = t
        .post("/v1/works", json!({ "title": "Two rounds", "lead": "Pia" }))
        .await;
    assert_eq!(resp.status(), 201);
    let work: Value = resp.json().await.unwrap();
    let route = format!("/v1/works/{}/runs", work["id"].as_str().unwrap());
    let mut w = t.watch().await;
    let resp = t
        .post(
            &route,
            json!({ "request": format!("First. [session] {}", finish()) }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let first: Value = resp.json().await.unwrap();
    run_ends(&mut w, &first).await;
    turn_of(&mut w, pia.id).await;
    idle_lead(&t, pia.id).await;
    let retained = t.agent("Pia").await;
    let checkout = retained.cwd;
    assert!(checkout.is_dir());
    assert!(retained.archived_at.is_none());
    let targets = checkout.parent().unwrap().join("targets/integration");
    std::fs::create_dir_all(&targets).unwrap();
    std::fs::write(targets.join("cache"), "compiled").unwrap();
    // This exercises the SQL partial unique index too: completed releases the group.
    let resp = t
        .post(&route, json!({ "request": "Second. [session]" }))
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let second: Value = resp.json().await.unwrap();
    assert_ne!(first["id"], second["id"]);
    assert_eq!(first["work_id"], second["work_id"]);
    assert_eq!(second["planner"], json!(pia.id));
    assert!(
        checkout.is_dir(),
        "a move does not discard the previous workspace"
    );
    assert!(targets.join("cache").exists());
}
