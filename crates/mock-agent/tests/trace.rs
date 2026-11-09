//! A run's trace (docs/tasks/DOGFOOD.md §8): which task failed where, from the events, by
//! the API and from the event log on disk.

mod common;

use common::{TestHub, Watcher, git_repo};
use crossagents_api_types::{AgentId, EventKind, StopReason, TaskStatus};
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

fn finish() -> String {
    call("run_finish", json!({ "summary": "done" }))
}

/// `[call plan_submit {tasks}]` whose strings hide their `[` (as `\u{1}`, written out as
/// `\u005b`): the lead's mock acts on markers anywhere in its kickoff, and the plan is in
/// there. The hub reads them back as `[` when it parses the call, so the worker gets the
/// markers as written.
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

/// A worker that got stuck: a new execution session wrote in its first turn, then a second
/// turn failed the way a model its account cannot run does.
struct Stuck {
    run: String,
    worker: AgentId,
}

/// Starts a one-task run and leaves its worker stuck; `[fail]` comes from a person's message,
/// so the plan the lead reads has no marker it would act on itself.
async fn a_worker_gets_stuck(t: &TestHub, w: &mut Watcher) -> Stuck {
    let task = json!([{ "key": "a", "title": "task a",
        "spec": { "goal": "[write a.txt=alpha]" } }]);
    let resp = t
        .post(
            "/v1/runs",
            json!({
                "request": format!("Build it. {}", plan(task)),
                "planner": "boss", "worker": "coder",
                "policy": { "plan_approval": "auto" },
            }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let run: Value = resp.json().await.unwrap();
    let run = run["id"].as_str().unwrap().to_owned();
    // The task goes to a new execution session: the clone whose removal used to lose the
    // story of where it got stuck (SELF §十一 A3).
    let started = w
        .until("the task started", |e| {
            matches!(&e.kind,
                EventKind::TaskStatusChanged { key, to: TaskStatus::Running, assignee: Some(_), .. }
                    if key == "a")
        })
        .await;
    let EventKind::TaskStatusChanged {
        assignee: Some(worker),
        ..
    } = started.kind
    else {
        unreachable!("the predicate matched a task with an assignee")
    };
    w.until(
        "its first turn",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == worker),
    )
    .await;
    // A turn that ends without a report gets one reminder from the hub (docs/TODO.md §28.9);
    // the mock ends that turn empty-handed too, and only then does the task stall.
    w.until("the reminder's turn stalls the task", |e| {
        matches!(&e.kind,
            EventKind::TaskStatusChanged { key, to: TaskStatus::Stalled, .. } if key == "a")
    })
    .await;
    let transcript: Value = t
        .get(&format!("/v1/agents/{worker}/transcript"))
        .await
        .json()
        .await
        .unwrap();
    assert!(
        transcript.to_string().contains("ended without a report"),
        "the worker was not reminded before it stalled: {transcript}"
    );
    t.send(&worker.to_string(), "[fail] where I got stuck")
        .await;
    w.until("the failed turn", |e| {
        matches!(&e.kind,
            EventKind::AgentTurnCompleted { id, stop_reason: StopReason::Error, .. } if *id == worker)
    })
    .await;
    Stuck { run, worker }
}

/// Waits for the run to end, whichever way: one that landed nothing ends failed.
async fn run_ends(w: &mut Watcher, run: &str) {
    w.until("run ended", |e| {
        matches!(&e.kind, EventKind::RunStatusChanged { id, to, .. }
            if id.to_string().as_str() == run && to.is_terminal())
    })
    .await;
}

/// The task's entry in `GET /v1/runs/{key}/trace`.
async fn task_trace(t: &TestHub, run: &str) -> Value {
    let trace: Value = t
        .get(&format!("/v1/runs/{run}/trace"))
        .await
        .json()
        .await
        .unwrap();
    trace["tasks"]
        .as_array()
        .unwrap_or_else(|| panic!("no tasks in {trace}"))
        .iter()
        .find(|task| task["key"] == "a")
        .unwrap_or_else(|| panic!("no task a in {trace}"))
        .clone()
}

/// The worker was archived rather than deleted: it can still be read, its conversation with
/// it, and it is still listed among the archived sessions.
async fn archived_not_deleted(t: &TestHub, worker: AgentId) {
    let resp = t.get(&format!("/v1/agents/{worker}")).await;
    assert_eq!(
        resp.status(),
        200,
        "the worker was deleted, so nothing is left to look back at"
    );
    let agent: Value = resp.json().await.unwrap();
    assert!(agent["archived_at"].is_string(), "{agent}");
    let id = worker.to_string();
    let all: Value = t
        .get("/v1/agents?archived=true")
        .await
        .json()
        .await
        .unwrap();
    assert!(
        all["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"].as_str() == Some(&id)),
        "not among the archived sessions: {all}"
    );
    let transcript: Value = t
        .get(&format!("/v1/agents/{worker}/transcript"))
        .await
        .json()
        .await
        .unwrap();
    assert!(
        !transcript["turns"].as_array().unwrap().is_empty(),
        "{transcript}"
    );
}

/// What the trace says about the stuck worker: both its turns (the one it worked in, the one
/// it failed in), the error, and where it got stuck. This is what a retrospective reads.
fn assert_the_worker_is_hosted(a: &Value, worker: AgentId) {
    let lane = &a["lane"];
    let id = worker.to_string();
    let agents: Vec<&str> = lane["agents"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(agents.contains(&id.as_str()), "{a}");
    let turns = lane["turns"].as_array().unwrap();
    assert!(turns.len() >= 2, "the worker's turns are gone: {a}");
    assert_eq!(turns[0]["agent"], json!(worker), "{a}");
    assert_eq!(turns[0]["stop_reason"], "end_turn", "{a}");
    // Its last turn, and what it failed with.
    let last = turns.last().unwrap();
    assert_eq!(last["agent"], json!(worker), "{a}");
    assert_eq!(last["stop_reason"], "error", "{a}");
    assert!(last["completed_at"].is_string(), "{a}");
    let error = last["error"].as_str().unwrap_or_default();
    assert!(error.contains("mock-gone"), "{a}");
    // Where it got stuck, in words, for the run's retrospective (C1).
    let first = a["first_failure"].as_str().unwrap_or_default();
    assert!(first.contains("a turn failed"), "{a}");
}

#[tokio::test]
async fn a_failed_check_is_traced_to_its_task() {
    let repo = git_repo();
    let t = TestHub::start_with(|config| config.event_log = true).await;
    t.add_agent_in("boss", Some("opus"), "planner", repo.path())
        .await;
    t.add_agent_in("coder", Some("haiku"), "worker", repo.path())
        .await;
    let mut w = t.watch().await;

    let goal = format!(
        "[write a.txt=alpha] [call task_complete {}]",
        json!({ "task_id": "{task_id}", "summary": "wrote a" })
    );
    let plan = json!([{ "key": "a", "title": "task a",
        "spec": { "goal": goal, "acceptance": ["git grep -q omega -- a.txt"] } }]);
    let req = format!("Build it. [call plan_submit {}]", json!({ "tasks": plan }));
    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": req, "planner": "boss", "worker": "coder", "policy": { "plan_approval": "auto" } }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let id = resp.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    w.until(
        "a checked",
        |e| matches!(&e.kind, EventKind::TaskChecked { key, .. } if key == "a"),
    )
    .await;

    let trace: Value = t
        .get(&format!("/v1/runs/{id}/trace"))
        .await
        .json()
        .await
        .unwrap();
    let a = &trace["tasks"][0];
    assert_eq!(a["key"], "a");
    assert_eq!(a["title"], "task a");
    assert_eq!(a["checks"][0]["passed"], false);
    let first = a["first_failure"].as_str().unwrap();
    assert!(first.contains("git grep -q omega"), "{first}");
    assert!(!a["lane"]["turns"].as_array().unwrap().is_empty(), "{a}");
    assert!(
        !trace["lead"]["turns"].as_array().unwrap().is_empty(),
        "{trace}"
    );

    // The same story from the log on disk, with no hub to ask.
    let events = crossagents_hub::trace::read_log(&t.paths.logs()).unwrap();
    let mut tracer = crossagents_hub::trace::Tracer::new(id.parse().unwrap());
    for event in &events {
        tracer.push(event);
    }
    let from_log = tracer.finish();
    assert_eq!(from_log.tasks[0].key, "a");
    assert_eq!(
        from_log.tasks[0].first_failure.as_deref(),
        Some(first),
        "{from_log:?}"
    );
}

/// SELF §十一 A3 (DOGFOOD §10 第 3 条): a worker's clone is archived, not deleted, when its
/// task is merged, and the run's trace still has its last turns and what it failed with.
#[tokio::test]
async fn a_merged_task_keeps_its_workers_last_turns_and_error_in_the_trace() {
    let repo = git_repo();
    let t = TestHub::start().await;
    t.add_agent_in("boss", Some("opus"), "planner", repo.path())
        .await;
    t.add_agent_in("coder", Some("haiku"), "worker", repo.path())
        .await;
    let mut w = t.watch().await;
    let stuck = a_worker_gets_stuck(&t, &mut w).await;

    // The issuer merges it anyway: a stalled task is accepted only with force and why.
    t.send(
        "boss",
        &format!(
            "{} {}",
            call(
                "task_accept",
                json!({ "task": "a", "force": true, "reason": "the write it left is enough" })
            ),
            finish()
        ),
    )
    .await;
    run_ends(&mut w, &stuck.run).await;

    let a = task_trace(&t, &stuck.run).await;
    assert_eq!(a["status"], "merged", "{a}");
    assert_the_worker_is_hosted(&a, stuck.worker);
    archived_not_deleted(&t, stuck.worker).await;
}

/// The same once the task is cancelled: the worker is archived rather than deleted, and the
/// trace keeps its last turn and error.
#[tokio::test]
async fn a_cancelled_task_keeps_its_workers_last_turns_and_error_in_the_trace() {
    let repo = git_repo();
    let t = TestHub::start().await;
    t.add_agent_in("boss", Some("opus"), "planner", repo.path())
        .await;
    t.add_agent_in("coder", Some("haiku"), "worker", repo.path())
        .await;
    let mut w = t.watch().await;
    let stuck = a_worker_gets_stuck(&t, &mut w).await;

    t.send(
        "boss",
        &format!(
            "{} {}",
            call("task_cancel", json!({ "task": "a" })),
            finish()
        ),
    )
    .await;
    run_ends(&mut w, &stuck.run).await;

    let a = task_trace(&t, &stuck.run).await;
    assert_eq!(a["status"], "cancelled", "{a}");
    assert_the_worker_is_hosted(&a, stuck.worker);
    archived_not_deleted(&t, stuck.worker).await;
}
