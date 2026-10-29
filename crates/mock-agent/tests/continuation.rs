//! Real hub, SQLite, task transitions and agent inboxes; no LLM needed.
mod common;

use std::time::Duration;

use chrono::Utc;
use common::{TestHub, git_repo};
use crossagents_core::{
    AgentId, AgentKey, AgentStatus, EventKind, HumanItem, HumanItemKind, Run, RunId, RunStatus,
    TaskStatus,
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

/// Like [`enabled`], plus the opt-in "no progress for this long" report.
fn stuck(minutes: u32) -> Value {
    json!({"plan_approval":"auto", "continuation":
        {"max_attempts":2,"interval_seconds":3600,"stuck_minutes":minutes}})
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

/// Waits until the run's checkpoint reads `expected`, checking it again as the hub's own
/// clock keeps running: the lead can still be answering a wakeup this instant, which reads
/// as `working` for a moment and is not what a test asking for a settled phase means.
async fn phase(t: &TestHub, run: &Run, expected: &str) {
    let mut last = None;
    let settled = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            t.hub.orchestrator.continue_runs().await;
            let state = get(t, run.id).await.continuation.unwrap();
            if state.phase == expected {
                return;
            }
            last = Some(state);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(
        settled.is_ok(),
        "the run never reached {expected}; last: {last:?}"
    );
}

async fn due(t: &TestHub, id: RunId) {
    let mut run = get(t, id).await;
    run.continuation.as_mut().unwrap().next_at = Some(Utc::now() - chrono::Duration::seconds(1));
    t.hub.orchestrator.store().update_run(&run).await.unwrap();
}

async fn wake_count(t: &TestHub, run: &Run) -> usize {
    wake_texts(t, run).await.len()
}

/// Everything the continuation delivered to the lead, in order.
async fn wake_texts(t: &TestHub, run: &Run) -> Vec<String> {
    t.hub
        .bus
        .store()
        .events_since(0, 10000)
        .await
        .unwrap()
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::AgentInput { id, text, .. }
                if *id == run.planner && text.contains("Run continuation ") =>
            {
                Some(text.clone())
            }
            _ => None,
        })
        .collect()
}

/// Stands in for minutes of real waiting: moves the run's records and the arm of its stuck
/// watch back, so the next tick sees a quiet stretch instead of sleeping.
async fn quiet(t: &TestHub, id: RunId, minutes: i64) {
    let ago = chrono::Duration::minutes(minutes);
    let mut run = get(t, id).await;
    run.updated_at -= ago;
    if let Some(watch) = run
        .continuation
        .as_mut()
        .and_then(|continuation| continuation.stuck_watch.as_mut())
    {
        watch.progress_at -= ago;
    }
    t.hub.orchestrator.store().update_run(&run).await.unwrap();
    for mut task in t.hub.orchestrator.store().list_tasks(id).await.unwrap() {
        task.updated_at -= ago;
        t.hub.orchestrator.store().update_task(&task).await.unwrap();
    }
}

/// Ages the run past the threshold and ticks until the lead is woken. The hub clock may land
/// between the writes of [`quiet`] and refresh the watch; a second aging then does the job,
/// so only a run that is never reported can fail this.
async fn quiet_until_woken(t: &TestHub, run: &Run) {
    for _ in 0..3 {
        if wake_count(t, run).await > 0 {
            return;
        }
        quiet(t, run.id, 5).await;
        t.hub.orchestrator.continue_runs().await;
    }
    panic!(
        "the lead was never told the busy run is stuck; state: {:?}",
        get(t, run.id).await.continuation
    );
}

/// A run whose only task waits on a worker with an open `[slow]` turn, so the phase is
/// `waiting_tasks` and one agent and one task can be named in a report.
async fn waiting_task_run(t: &TestHub, policy: Value) -> (Run, crossagents_core::Task) {
    let mut w = t.watch().await;
    let run = start(t, &plan("inspect", true), policy).await;
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
    idle(t, run.planner).await;
    let task = t
        .hub
        .orchestrator
        .store()
        .list_tasks(run.id)
        .await
        .unwrap()
        .remove(0);
    (run, task)
}

/// The notices the person has for this run, in any state.
async fn notices(t: &TestHub, run: &Run) -> Vec<HumanItem> {
    t.hub
        .orchestrator
        .store()
        .list_human_items(false)
        .await
        .unwrap()
        .into_iter()
        .filter(|item| item.run_id == Some(run.id) && item.kind == HumanItemKind::Notice)
        .collect()
}

/// A run whose single task stalls, so continuation has to wake the lead.
async fn stalled_run(t: &TestHub) -> Run {
    let mut w = t.watch().await;
    let run = start(t, &plan("[write a.txt=work already done]", true), enabled()).await;
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
    run
}

/// Uses both wakeups of [`enabled`] and leaves the run on its cap.
async fn exhaust(t: &TestHub, run: &Run) {
    idle(t, run.planner).await;
    phase(t, run, "recover").await;
    for attempt in 1..=2 {
        phase(t, run, "recover").await;
        due(t, run.id).await;
        t.hub.orchestrator.continue_runs().await;
        idle(t, run.planner).await;
        assert_eq!(get(t, run.id).await.continuation.unwrap().attempts, attempt);
    }
    phase(t, run, "exhausted").await;
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
    let summary = call(
        "task_accept",
        json!({"task":"a", "force": true, "reason": "no checks configured"}),
    );
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
    // The checkpoint's `next_step` follows the live situation (`working` while the lead
    // answers this wakeup), so assert on the advice that was actually delivered.
    w.until("finalize advice", |e| {
        matches!(&e.kind, EventKind::AgentInput { id, text, .. }
        if *id == run.planner
            && text.contains("Run continuation 1/2")
            && text.contains("original request"))
    })
    .await;
    idle(&t, run.planner).await;
    let current = get(&t, run.id).await;
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
    // The turn a wakeup bought has ended: the checkpoint reads the settled phase with the
    // next wakeup reserved, not `working` with nothing due (`settle_continuation`). Wait for
    // it rather than for the clock that would otherwise get there a tick later.
    phase(&t, &run, "continue").await;
    let before = get(&t, run.id).await.continuation.unwrap();
    assert_eq!(before.attempts, 1, "{before:?}");
    assert_eq!(
        before
            .next_at
            .zip(before.last_attempt_at)
            .map(|(at, last)| (at - last).num_minutes()),
        Some(60),
        "the interval is reserved past the attempt: {before:?}"
    );
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

/// Reaching the cap is exactly when a person has to hear about it, and only then: one
/// notice per exhaustion, however often the clock revisits the run or the hub restarts.
#[tokio::test]
async fn exhausted_continuation_tells_the_person_once_across_ticks_and_a_restart() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let run = stalled_run(&t).await;
    idle(&t, run.planner).await;
    phase(&t, &run, "recover").await;
    assert!(
        notices(&t, &run).await.is_empty(),
        "nothing to report before the cap"
    );
    exhaust(&t, &run).await;

    let items = notices(&t, &run).await;
    assert_eq!(
        items.len(),
        1,
        "one notice when the cap is reached: {items:?}"
    );
    let text = &items[0].text;
    assert!(text.contains(&run.title), "names the run: {text}");
    assert!(
        text.contains(&run.id.to_string()),
        "names the run id: {text}"
    );
    assert!(
        text.contains("2/2"),
        "says how often it woke the lead: {text}"
    );
    assert!(
        text.contains("recover"),
        "says what it was waiting on: {text}"
    );
    assert!(
        text.contains("Automatic continuation limit"),
        "carries the suggested next step: {text}"
    );

    // The clock keeps landing on the exhausted branch; still one notice.
    for _ in 0..3 {
        due(&t, run.id).await;
        t.hub.orchestrator.continue_runs().await;
        phase(&t, &run, "exhausted").await;
    }
    assert_eq!(notices(&t, &run).await.len(), 1);
    assert_eq!(wake_count(&t, &run).await, 2);

    // The flag lives in the run record, so a real restart reads it back and stays quiet.
    t.hub.shutdown();
    t.hub.wait().await.unwrap();
    let t = TestHub::start_in(t.home, |_| {}).await;
    idle(&t, run.planner).await;
    phase(&t, &run, "exhausted").await;
    assert_eq!(
        notices(&t, &run).await.len(),
        1,
        "a restarted hub must not send a second notice"
    );
}

/// Re-arming (attempts back to zero, as a policy update will do) makes the next
/// exhaustion a new fact: it tells the person once more.
#[tokio::test]
async fn a_rearmed_run_tells_the_person_again_when_it_exhausts_once_more() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let run = stalled_run(&t).await;
    exhaust(&t, &run).await;
    assert_eq!(notices(&t, &run).await.len(), 1);

    let mut current = get(&t, run.id).await;
    current.continuation.as_mut().unwrap().attempts = 0;
    t.hub
        .orchestrator
        .store()
        .update_run(&current)
        .await
        .unwrap();

    exhaust(&t, &run).await;
    assert_eq!(
        notices(&t, &run).await.len(),
        2,
        "re-arming allows exactly one more notice"
    );
    assert_eq!(wake_count(&t, &run).await, 4);
}

/// A cap of zero means no wakeup at all, so the run is exhausted the first time it is
/// observed; that is still worth one notice, not one per tick.
#[tokio::test]
async fn a_zero_cap_is_exhausted_at_once_and_told_once() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let run = start(
        &t,
        "inspect first",
        json!({"continuation":{"max_attempts":0,"interval_seconds":60}}),
    )
    .await;
    idle(&t, run.planner).await;
    phase(&t, &run, "exhausted").await;
    assert_eq!(notices(&t, &run).await.len(), 1);
    phase(&t, &run, "exhausted").await;
    assert_eq!(notices(&t, &run).await.len(), 1);
    assert_eq!(wake_count(&t, &run).await, 0);
}

/// Runs without the opt-in policy are untouched: no checkpoint, no notice, whatever
/// state they are in.
#[tokio::test]
async fn runs_without_continuation_never_get_the_exhausted_notice() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let run = start(&t, "inspect first", json!({})).await;
    idle(&t, run.planner).await;
    for _ in 0..3 {
        t.hub.orchestrator.continue_runs().await;
    }
    assert!(get(&t, run.id).await.continuation.is_none());
    assert!(notices(&t, &run).await.is_empty());

    let mut current = get(&t, run.id).await;
    current.status = RunStatus::Cancelled;
    t.hub
        .orchestrator
        .store()
        .update_run(&current)
        .await
        .unwrap();
    t.hub.orchestrator.continue_runs().await;
    assert!(get(&t, run.id).await.continuation.is_none());
    assert!(notices(&t, &run).await.is_empty());
}

/// Without `stuck_minutes` a busy run is left alone forever, as it always was: no arm, no
/// wakeup, however long its records have been quiet.
#[tokio::test]
async fn without_stuck_minutes_a_busy_run_is_never_reported() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let (run, _) = waiting_task_run(&t, enabled()).await;
    phase(&t, &run, "waiting_tasks").await;
    assert!(
        get(&t, run.id)
            .await
            .continuation
            .unwrap()
            .stuck_watch
            .is_none(),
        "the default policy arms nothing"
    );
    quiet(&t, run.id, 120).await;
    for _ in 0..3 {
        t.hub.orchestrator.continue_runs().await;
    }
    let state = get(&t, run.id).await.continuation.unwrap();
    assert_eq!(state.attempts, 0);
    assert_eq!(state.phase, "waiting_tasks");
    assert!(state.stuck_watch.is_none());
    assert_eq!(wake_count(&t, &run).await, 0);
    assert!(notices(&t, &run).await.is_empty());
}

/// A worker that keeps its turn open and writes nothing for longer than `stuck_minutes` is a
/// report for the lead, naming the agent and the task, through the same wakeup path and
/// quota as recovery. Nothing is interrupted: the worker's turn is still running after it.
#[tokio::test]
async fn a_quiet_busy_worker_is_reported_to_the_lead_with_agent_and_task() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let (run, task) = waiting_task_run(&t, stuck(2)).await;
    let worker = task.assignee.unwrap();
    phase(&t, &run, "waiting_tasks").await;
    assert_eq!(
        wake_count(&t, &run).await,
        0,
        "a busy agent is not stuck the moment it is first seen"
    );
    quiet_until_woken(&t, &run).await;

    let texts = wake_texts(&t, &run).await;
    assert_eq!(texts.len(), 1, "{texts:?}");
    let text = &texts[0];
    let who = t
        .hub
        .orchestrator
        .store()
        .get_agent(&AgentKey::Id(worker))
        .await
        .unwrap()
        .name;
    assert!(text.contains(&who), "names the busy agent: {text}");
    assert!(
        text.contains(&format!("#{}", task.key)),
        "names the task: {text}"
    );
    assert!(text.contains("No progress for"), "{text}");
    assert!(text.contains("Nothing was interrupted"), "{text}");

    let state = get(&t, run.id).await.continuation.unwrap();
    assert_eq!(state.attempts, 1, "the report spent one shared attempt");
    // The worker still holds its turn: only the lead was told, nothing was replayed.
    let current = t
        .hub
        .orchestrator
        .store()
        .list_tasks(run.id)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(current.assignee, Some(worker));
    assert!(!current.status.is_terminal());
    // The next one has to wait for both the interval and a fresh quiet stretch.
    for _ in 0..3 {
        t.hub.orchestrator.continue_runs().await;
    }
    assert_eq!(wake_count(&t, &run).await, 1);
}

/// A task that moved within the threshold is progress: the worker is reporting on the task,
/// so the quiet stretch has not run out even though the run itself was last written long ago.
#[tokio::test]
async fn a_task_that_moves_within_the_threshold_is_not_reported() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let (run, _) = waiting_task_run(&t, stuck(2)).await;
    phase(&t, &run, "waiting_tasks").await;
    quiet(&t, run.id, 5).await;
    let mut task = t
        .hub
        .orchestrator
        .store()
        .list_tasks(run.id)
        .await
        .unwrap()
        .remove(0);
    task.progress = Some("compiled half the crate".into());
    task.updated_at = Utc::now();
    t.hub.orchestrator.store().update_task(&task).await.unwrap();

    t.hub.orchestrator.continue_runs().await;
    let state = get(&t, run.id).await.continuation.unwrap();
    assert_eq!(wake_count(&t, &run).await, 0, "{state:?}");
    assert_eq!(state.attempts, 0);
    assert!(notices(&t, &run).await.is_empty());
}

/// The same for an agent that is visibly working without touching the task record: its new
/// event is progress, even after the records themselves have been quiet past the threshold.
#[tokio::test]
async fn a_new_agent_event_within_the_threshold_is_not_reported() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let (run, task) = waiting_task_run(&t, stuck(2)).await;
    phase(&t, &run, "waiting_tasks").await;
    let mut w = t.watch().await;
    // A message for the busy worker lands as an agent event right away, then the records
    // are aged past the threshold: only that event keeps the run out of the report.
    t.send(&task.assignee.unwrap().to_string(), "still there?")
        .await;
    w.until(
        "worker event",
        |e| matches!(&e.kind, EventKind::AgentInput { id, .. } if *id == task.assignee.unwrap()),
    )
    .await;
    quiet(&t, run.id, 5).await;

    t.hub.orchestrator.continue_runs().await;
    let state = get(&t, run.id).await.continuation.unwrap();
    assert_eq!(wake_count(&t, &run).await, 0, "{state:?}");
    assert_eq!(state.attempts, 0);
    assert!(notices(&t, &run).await.is_empty());
}

/// Stuck reports share `max_attempts` with the other wakeups, and reaching it tells the
/// person once, on the same persisted flag.
#[tokio::test]
async fn stuck_reports_share_the_cap_and_its_single_exhausted_notice() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let (run, _) = waiting_task_run(
        &t,
        json!({"plan_approval":"auto", "continuation":
            {"max_attempts":1,"interval_seconds":3600,"stuck_minutes":2}}),
    )
    .await;
    phase(&t, &run, "waiting_tasks").await;
    quiet_until_woken(&t, &run).await;
    assert_eq!(wake_count(&t, &run).await, 1);
    // The lead answers the wakeup; wait for that turn before the next quiet stretch.
    idle(&t, run.planner).await;
    phase(&t, &run, "waiting_tasks").await;

    // The one attempt is gone: a second quiet stretch is reported to the person, not woken.
    for _ in 0..3 {
        if !notices(&t, &run).await.is_empty() {
            break;
        }
        quiet(&t, run.id, 5).await;
        t.hub.orchestrator.continue_runs().await;
    }
    let state = get(&t, run.id).await.continuation.unwrap();
    assert_eq!(state.attempts, 1, "{state:?}");
    let items = notices(&t, &run).await;
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(items[0].text.contains("1/1"), "{}", items[0].text);
    assert!(
        items[0].text.contains("waiting_tasks"),
        "says what it was waiting on: {}",
        items[0].text
    );
    assert!(
        items[0].text.contains("No progress for"),
        "carries the stuck advice: {}",
        items[0].text
    );

    for _ in 0..3 {
        quiet(&t, run.id, 5).await;
        t.hub.orchestrator.continue_runs().await;
    }
    assert_eq!(wake_count(&t, &run).await, 1);
    assert_eq!(
        notices(&t, &run).await.len(),
        1,
        "the person is told about the cap exactly once"
    );
}

/// The other wait a hung turn produces: the lead's own turn never returns. The report names
/// the lead, and the wakeup is only queued — the running turn is left alone, not interrupted.
#[tokio::test]
async fn a_hung_lead_turn_is_reported_with_the_lead_named() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let run = start(&t, "[slow]", stuck(2)).await;
    w.until(
        "lead executing",
        |e| matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains("working")),
    )
    .await;
    phase(&t, &run, "working").await;
    quiet_until_woken(&t, &run).await;

    let texts = wake_texts(&t, &run).await;
    assert_eq!(texts.len(), 1, "{texts:?}");
    let lead = t
        .hub
        .orchestrator
        .store()
        .get_agent(&AgentKey::Id(run.planner))
        .await
        .unwrap()
        .name;
    assert!(
        texts[0].contains(&format!("the lead «{lead}»")),
        "names the hung lead: {}",
        texts[0]
    );
    assert!(texts[0].contains("No progress for"), "{}", texts[0]);
    assert_eq!(get(&t, run.id).await.continuation.unwrap().attempts, 1);
    let agent = t
        .hub
        .orchestrator
        .store()
        .get_agent(&AgentKey::Id(run.planner))
        .await
        .unwrap();
    assert!(
        matches!(agent.status, AgentStatus::Running | AgentStatus::Starting),
        "the hung turn is still running, not interrupted: {agent:?}"
    );
}

/// The lead's turn ending is not an observation the stuck watch may act on: at Idle the
/// checkpoint is settled without delivering (`settle_continuation`, from `engine.rs` on
/// `AgentStatusChanged → Idle`). Judging the quiet stretch there would write the stuck
/// advice and re-arm the watch, then send nothing — and the clock's next wakeup would carry
/// the plain advice, with "N minutes without progress, waiting for whom" lost for good.
#[tokio::test]
async fn settling_the_checkpoint_at_idle_does_not_swallow_a_stuck_report() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let (run, task) = waiting_task_run(&t, stuck(2)).await;
    phase(&t, &run, "waiting_tasks").await;
    let armed = get(&t, run.id).await.continuation.unwrap();
    assert!(
        armed.stuck_watch.is_some(),
        "the waiting phase arms the watch: {armed:?}"
    );
    // The worker has been quiet past `stuck_minutes`...
    quiet(&t, run.id, 5).await;
    let quiet_at = get(&t, run.id)
        .await
        .continuation
        .unwrap()
        .stuck_watch
        .as_ref()
        .unwrap()
        .progress_at;
    // ...exactly as the lead's turn ends. Settling is called directly: the real Idle event
    // would run the lead's own turn first, and a clock tick landing inside that turn reads
    // the phase as `working` and arms the watch afresh for a genuine reason, which is not
    // what this test is about.
    t.hub.orchestrator.settle_continuation(run.planner).await;
    let settled = get(&t, run.id).await.continuation.unwrap();
    assert_eq!(
        settled.stuck_watch.as_ref().map(|watch| watch.progress_at),
        Some(quiet_at),
        "settling at Idle must not re-arm the watch: {settled:?}"
    );
    assert_eq!(settled.attempts, 0, "settling wakes nobody: {settled:?}");
    assert_eq!(wake_count(&t, &run).await, 0);

    // The clock's next pass is the one that judges the stretch, and it still reports it.
    t.hub.orchestrator.continue_runs().await;
    let texts = wake_texts(&t, &run).await;
    assert_eq!(texts.len(), 1, "{texts:?}");
    let text = &texts[0];
    assert!(
        text.contains("No progress for 5 minutes"),
        "the quiet stretch is reported, not a plain nudge: {text}"
    );
    let name = t
        .hub
        .orchestrator
        .store()
        .get_agent(&AgentKey::Id(task.assignee.unwrap()))
        .await
        .unwrap()
        .name;
    assert!(
        text.contains(&format!("agent «{name}» on #{} ({})", task.key, task.title)),
        "names who is being waited for: {text}"
    );
    let report = get(&t, run.id).await.continuation.unwrap();
    assert_eq!(report.attempts, 1, "{report:?}");
    assert!(
        report.stuck_watch.as_ref().unwrap().progress_at > quiet_at,
        "the report starts the next stretch: {report:?}"
    );
}
