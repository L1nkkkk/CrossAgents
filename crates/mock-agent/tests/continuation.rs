//! Real hub, SQLite, task transitions and agent inboxes; no LLM needed.
mod common;

use std::time::Duration;

use chrono::Utc;
use common::{TestHub, git_repo};
use crossagents_core::{
    AgentId, AgentKey, AgentStatus, EventKind, Run, RunId, RunStatus, TaskStatus,
};
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

fn plan(goal: &str, dependent: bool) -> String {
    let mut tasks = vec![json!({"key":"a", "title":"a", "spec":{"goal":goal}})];
    if dependent {
        tasks.push(json!({"key":"b", "title":"b", "spec":{"goal":"later"}, "depends_on":["a"]}));
    }
    call("plan_submit", json!({"tasks":tasks}))
}

async fn start(t: &TestHub, request: &str, policy: Value) -> Run {
    let mut w = t.watch().await;
    let response = t
        .post(
            "/v1/runs",
            json!({
                "planner":"boss", "worker":"coder", "request":request, "policy":policy,
            }),
        )
        .await;
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
    let run: Run = response.json().await.unwrap();
    w.until(
        "kickoff started",
        |e| matches!(&e.kind, EventKind::AgentTurnStarted { id, .. } if *id == run.planner),
    )
    .await;
    run
}

fn enabled() -> Value {
    // Explicitly advance the persisted due time instead of waiting on wall-clock timers.
    json!({"plan_approval":"auto", "continuation":{"max_attempts":2,"interval_seconds":3600}})
}

async fn setup(repo: &std::path::Path) -> TestHub {
    let t = TestHub::start().await;
    t.add_agent_in("boss", None, "planner", repo).await;
    t.add_agent_in("coder", None, "worker", repo).await;
    t
}

async fn get(t: &TestHub, id: RunId) -> Run {
    t.hub.orchestrator.store().get_run(id).await.unwrap()
}

async fn idle(t: &TestHub, id: AgentId) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let a = t
                .hub
                .orchestrator
                .store()
                .get_agent(&AgentKey::Id(id))
                .await
                .unwrap();
            if !matches!(a.status, AgentStatus::Running | AgentStatus::Starting)
                && !t.hub.runtime.is_busy(id).await
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("agent did not become idle");
}

async fn phase(t: &TestHub, run: &Run, expected: &str) {
    t.hub.orchestrator.continue_runs().await;
    let state = get(t, run.id).await.continuation.unwrap();
    assert_eq!(state.phase, expected, "{state:?}");
}

async fn due(t: &TestHub, id: RunId) {
    let mut run = get(t, id).await;
    run.continuation.as_mut().unwrap().next_at = Some(Utc::now() - chrono::Duration::seconds(1));
    t.hub.orchestrator.store().update_run(&run).await.unwrap();
}

async fn wake_count(t: &TestHub, run: &Run) -> usize {
    t.hub
        .bus
        .store()
        .events_since(0, 10000)
        .await
        .unwrap()
        .iter()
        .filter(|e| {
            matches!(&e.kind, EventKind::AgentInput { id, text, .. }
            if *id == run.planner && text.contains("Run continuation "))
        })
        .count()
}

#[tokio::test]
async fn stalled_worker_recovers_only_the_lead_with_a_durable_cap() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let run = start(
        &t,
        &plan("[write a.txt=work already done]", true),
        enabled(),
    )
    .await;
    w.until("stalled", |e| {
        matches!(
            &e.kind,
            EventKind::TaskStatusChanged {
                to: TaskStatus::Stalled,
                ..
            }
        )
    })
    .await;
    idle(&t, run.planner).await;
    phase(&t, &run, "recover").await;
    assert_eq!(
        wake_count(&t, &run).await,
        0,
        "first observation only arms the clock"
    );
    for attempt in 1..=2 {
        phase(&t, &run, "recover").await;
        due(&t, run.id).await;
        // Concurrent clocks must not reserve the same due attempt twice.
        tokio::join!(
            t.hub.orchestrator.continue_runs(),
            t.hub.orchestrator.continue_runs()
        );
        idle(&t, run.planner).await;
        assert_eq!(
            get(&t, run.id).await.continuation.unwrap().attempts,
            attempt
        );
        assert_eq!(wake_count(&t, &run).await, attempt as usize);
    }
    phase(&t, &run, "exhausted").await;
    let tasks = t.hub.orchestrator.store().list_tasks(run.id).await.unwrap();
    assert_eq!(tasks[0].status, TaskStatus::Stalled);
    assert_eq!(tasks[1].status, TaskStatus::Pending);
    assert_eq!(wake_count(&t, &run).await, 2);
    // Persisted cap survives a real hub shutdown/start, without a restart notice bypass.
    t.hub.shutdown();
    t.hub.wait().await.unwrap();
    let t = TestHub::start_in(t.home, |_| {}).await;
    phase(&t, &run, "exhausted").await;
    assert_eq!(wake_count(&t, &run).await, 2);
}

#[tokio::test]
async fn active_worker_and_waiting_dependencies_do_not_consume_recovery() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let run = start(&t, &plan("inspect", true), enabled()).await;
    w.until("stalled", |e| {
        matches!(
            &e.kind,
            EventKind::TaskStatusChanged {
                to: TaskStatus::Stalled,
                ..
            }
        )
    })
    .await;
    let worker = t.hub.orchestrator.store().list_tasks(run.id).await.unwrap()[0]
        .assignee
        .unwrap();
    t.send(&worker.to_string(), "[slow]").await;
    w.until(
        "worker executing",
        |e| matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains("working")),
    )
    .await;
    idle(&t, run.planner).await;
    phase(&t, &run, "waiting_tasks").await;
    // Even a misleading stalled label cannot cause an adopted/live worker to be replayed.
    let mut task = t
        .hub
        .orchestrator
        .store()
        .list_tasks(run.id)
        .await
        .unwrap()
        .remove(0);
    task.status = TaskStatus::Stalled;
    t.hub.orchestrator.store().update_task(&task).await.unwrap();
    phase(&t, &run, "waiting_tasks").await;
    assert_eq!(get(&t, run.id).await.continuation.unwrap().attempts, 0);
    assert_eq!(wake_count(&t, &run).await, 0);
}

#[tokio::test]
async fn busy_lead_and_queued_messages_are_not_nudged() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let run = start(&t, "[slow]", enabled()).await;
    w.until(
        "lead executing",
        |e| matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains("working")),
    )
    .await;
    t.send(&run.planner.to_string(), "already queued").await;
    phase(&t, &run, "working").await;
    assert_eq!(get(&t, run.id).await.continuation.unwrap().attempts, 0);
    assert_eq!(wake_count(&t, &run).await, 0);
}

#[tokio::test]
async fn human_wait_approval_pause_and_cancel_suppress_due_wakeups() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let run = start(
        &t,
        &call("human_ask", json!({"text":"Need a product decision"})),
        enabled(),
    )
    .await;
    w.until("asked", |e| matches!(e.kind, EventKind::HumanAsked { .. }))
        .await;
    idle(&t, run.planner).await;
    phase(&t, &run, "waiting_human").await;
    for (status, expected) in [
        (RunStatus::AwaitingApproval, "waiting_approval"),
        (RunStatus::Paused, "paused"),
        (RunStatus::Cancelled, "ended"),
    ] {
        let mut current = get(&t, run.id).await;
        current.status = status;
        current.continuation.as_mut().unwrap().next_at =
            Some(Utc::now() - chrono::Duration::seconds(1));
        t.hub
            .orchestrator
            .store()
            .update_run(&current)
            .await
            .unwrap();
        phase(&t, &run, expected).await;
        assert_eq!(get(&t, run.id).await.continuation.unwrap().attempts, 0);
    }
    assert_eq!(wake_count(&t, &run).await, 0);
}

#[tokio::test]
async fn budget_pause_precedes_recovery_including_a_lead_working_without_tasks() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let run = start(&t, "inspect first", enabled()).await;
    idle(&t, run.planner).await;
    phase(&t, &run, "continue").await;
    due(&t, run.id).await;
    let mut current = get(&t, run.id).await;
    current.config.policy.budget.max_tokens = Some(0);
    t.hub
        .orchestrator
        .store()
        .update_run(&current)
        .await
        .unwrap();
    let budgets = crossagents_hub::budget::Budgets::load(
        t.hub.bus.store().as_ref(),
        t.hub.orchestrator.clone(),
    )
    .await
    .unwrap();
    budgets.check(t.hub.bus.store().as_ref()).await.unwrap();
    phase(&t, &run, "paused").await;
    assert_eq!(get(&t, run.id).await.status, RunStatus::Paused);
    assert_eq!(wake_count(&t, &run).await, 0);
}

#[tokio::test]
async fn all_tasks_ended_prompts_integration_verification_without_auto_finish_or_land() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let summary = call("task_accept", json!({"task":"a"}));
    let goal = format!(
        "[write a.txt=done] {}",
        call(
            "task_complete",
            json!({"task_id":"{task_id}", "summary":summary})
        )
    );
    let run = start(&t, &plan(&goal, false), enabled()).await;
    w.until("merged", |e| {
        matches!(
            &e.kind,
            EventKind::TaskStatusChanged {
                to: TaskStatus::Merged,
                ..
            }
        )
    })
    .await;
    idle(&t, run.planner).await;
    phase(&t, &run, "finalize").await;
    due(&t, run.id).await;
    t.hub.orchestrator.continue_runs().await;
    idle(&t, run.planner).await;
    let current = get(&t, run.id).await;
    assert!(
        current
            .continuation
            .unwrap()
            .next_step
            .contains("original request")
    );
    assert_eq!(current.status, RunStatus::Running);
    assert!(current.landed.is_none());
    assert_eq!(wake_count(&t, &run).await, 1);
}

#[tokio::test]
async fn reserved_due_time_survives_restart_and_default_runs_stay_unchanged() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let run = start(&t, "inspect first", enabled()).await;
    idle(&t, run.planner).await;
    phase(&t, &run, "continue").await;
    due(&t, run.id).await;
    t.hub.orchestrator.continue_runs().await;
    idle(&t, run.planner).await;
    let before = get(&t, run.id).await.continuation.unwrap();
    t.hub.shutdown();
    t.hub.wait().await.unwrap();
    let t = TestHub::start_in(t.home, |_| {}).await;
    phase(&t, &run, "continue").await;
    assert_eq!(get(&t, run.id).await.continuation.unwrap(), before);
    assert_eq!(wake_count(&t, &run).await, 1);

    let repo2 = git_repo();
    let t2 = setup(repo2.path()).await;
    let old = start(&t2, "inspect first", json!({})).await;
    idle(&t2, old.planner).await;
    t2.hub.orchestrator.continue_runs().await;
    assert!(get(&t2, old.id).await.continuation.is_none());
    assert_eq!(wake_count(&t2, &old).await, 0);
}

#[tokio::test]
async fn hub_clock_delivers_recovery_without_manual_ticks() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let run = start(
        &t,
        "inspect first",
        json!({"continuation":{"max_attempts":1,"interval_seconds":1}}),
    )
    .await;
    w.until("clock recovery", |e| {
        matches!(&e.kind, EventKind::AgentInput { id, text, .. }
        if *id == run.planner && text.contains("Run continuation 1/1"))
    })
    .await;
    assert_eq!(get(&t, run.id).await.continuation.unwrap().attempts, 1);
}
