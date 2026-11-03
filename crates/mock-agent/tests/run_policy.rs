//! A person changes an existing run's automatic continuation (docs/tasks/H1.md §4): turn it
//! on or off, change its limits, and hand the allowance out again when it ran out. Real hub,
//! real SQLite, no LLM.
mod common;

use std::time::Duration;

use chrono::Utc;
use common::{TestHub, git_repo};
use crossagents_api_types::{
    AgentStatus, ContinuationUpdate, EventKind, Run, RunDetail, RunId, UpdateRunPolicyRequest,
};
use crossagents_core::{AgentId, AgentKey, HumanItem, HumanItemKind, PlanApproval};
use serde_json::{Value, json};

/// A run whose lead has nothing to do: it is woken by the clock, not by a task.
async fn start(t: &TestHub, policy: Value) -> Run {
    let mut w = t.watch().await;
    let response = t
        .post(
            "/v1/runs",
            json!({
                "planner":"boss", "worker":"coder", "request":"inspect first", "policy":policy,
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
    tokio::time::timeout(common::wait_limit(), async {
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

/// One clock tick, then what the run's checkpoint says.
async fn phase(t: &TestHub, run: &Run, expected: &str) {
    t.hub.orchestrator.continue_runs().await;
    let state = get(t, run.id).await.continuation.unwrap();
    assert_eq!(state.phase, expected, "{state:?}");
}

/// Advances the persisted due time, instead of waiting out the interval.
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

/// The person's call, answered with the run as it is now.
async fn change_policy(t: &TestHub, run: &Run, req: &UpdateRunPolicyRequest) -> RunDetail {
    let resp = t
        .post(&format!("/v1/runs/{}/policy", run.id), json!(req))
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    resp.json().await.unwrap()
}

fn update(enabled: bool, max_attempts: Option<u32>, reset: bool) -> UpdateRunPolicyRequest {
    UpdateRunPolicyRequest {
        continuation: Some(ContinuationUpdate {
            enabled,
            max_attempts,
            interval_seconds: Some(3600),
            stuck_minutes: None,
        }),
        reset_attempts: reset,
    }
}

/// The stuck report is a nullable limit: saying nothing about it keeps the run's own
/// setting, a number sets it, and `null` turns it off again. A request body, not the typed
/// struct, so the three cases are told apart the way a client's JSON tells them apart.
#[tokio::test]
async fn the_stuck_report_is_set_kept_and_turned_off() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let run = start(
        &t,
        json!({"plan_approval":"auto", "continuation":{"max_attempts":2}}),
    )
    .await;
    idle(&t, run.planner).await;
    let stuck = |run: &Run| {
        run.config
            .policy
            .continuation
            .as_ref()
            .and_then(|p| p.stuck_minutes)
    };
    assert_eq!(stuck(&run), None, "off unless asked for");

    let set = t
        .post(
            &format!("/v1/runs/{}/policy", run.id),
            json!({"continuation":{"enabled":true,"stuck_minutes":15}}),
        )
        .await;
    assert_eq!(set.status(), 200, "{}", set.text().await.unwrap());
    let detail: RunDetail = set.json().await.unwrap();
    assert_eq!(stuck(&detail.run), Some(15));

    // Another change that does not mention it leaves it alone.
    let other = change_policy(&t, &run, &update(true, Some(4), false)).await;
    assert_eq!(stuck(&other.run), Some(15), "unstated means unchanged");
    assert_eq!(
        other.run.config.policy.continuation.unwrap().max_attempts,
        4
    );

    let cleared = t
        .post(
            &format!("/v1/runs/{}/policy", run.id),
            json!({"continuation":{"enabled":true,"stuck_minutes":null}}),
        )
        .await;
    assert_eq!(cleared.status(), 200, "{}", cleared.text().await.unwrap());
    let detail: RunDetail = cleared.json().await.unwrap();
    assert_eq!(stuck(&detail.run), None, "null turns the report off");
    assert_eq!(
        detail.run.config.policy.continuation.unwrap().max_attempts,
        4,
        "the other limits stay"
    );
    assert_eq!(stuck(&get(&t, run.id).await), None, "and it is recorded");
}

/// The allowance ran out; the person hands out a new one, and the clock wakes the lead
/// again. The notice that was sent for the first exhaustion does not stop the second one:
/// resetting the attempts clears the flag that deduplicated it.
#[tokio::test]
async fn a_person_rearms_an_exhausted_run_and_the_clock_wakes_the_lead_again() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let run = start(
        &t,
        json!({"continuation":{"max_attempts":1,"interval_seconds":3600}}),
    )
    .await;
    idle(&t, run.planner).await;
    phase(&t, &run, "continue").await;
    due(&t, run.id).await;
    t.hub.orchestrator.continue_runs().await;
    idle(&t, run.planner).await;
    phase(&t, &run, "exhausted").await;
    assert_eq!(get(&t, run.id).await.continuation.unwrap().attempts, 1);
    assert_eq!(notices(&t, &run).await.len(), 1);
    assert_eq!(wake_count(&t, &run).await, 1);
    // The clock keeps landing on the cap: still one notice, no further wakeup.
    phase(&t, &run, "exhausted").await;
    assert_eq!(wake_count(&t, &run).await, 1);

    let detail = change_policy(&t, &run, &update(true, Some(1), true)).await;
    let state = detail.run.continuation.clone().unwrap();
    assert_eq!(state.attempts, 0, "the allowance is back: {state:?}");
    assert!(!state.exhausted_notified, "{state:?}");
    assert_eq!(
        detail.run.config.policy.continuation.unwrap().max_attempts,
        1
    );

    // The clock reads the new record: one more wakeup, then the cap again, and the person is
    // told once more because the old notice's flag went with the old allowance.
    phase(&t, &run, "continue").await;
    due(&t, run.id).await;
    t.hub.orchestrator.continue_runs().await;
    idle(&t, run.planner).await;
    assert_eq!(get(&t, run.id).await.continuation.unwrap().attempts, 1);
    assert_eq!(wake_count(&t, &run).await, 2);
    phase(&t, &run, "exhausted").await;
    assert_eq!(
        notices(&t, &run).await.len(),
        2,
        "a re-armed run that runs out again is news"
    );
}

/// Turning it off is a clean stop: the run itself carries on, but nothing is written back
/// into it and no wakeup is delivered.
#[tokio::test]
async fn turning_continuation_off_stops_the_wakeups_and_leaves_no_state() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let run = start(
        &t,
        json!({"continuation":{"max_attempts":3,"interval_seconds":3600}}),
    )
    .await;
    idle(&t, run.planner).await;
    phase(&t, &run, "continue").await;
    assert_eq!(get(&t, run.id).await.continuation.unwrap().attempts, 0);
    let before = get(&t, run.id).await;

    let detail = change_policy(&t, &run, &update(false, None, false)).await;
    assert!(detail.run.config.policy.continuation.is_none());
    assert!(
        detail.run.continuation.is_none(),
        "nothing left that reads as still on: {:?}",
        detail.run.continuation
    );

    for _ in 0..3 {
        t.hub.orchestrator.continue_runs().await;
    }
    let current = get(&t, run.id).await;
    assert!(current.config.policy.continuation.is_none());
    assert!(current.continuation.is_none(), "the clock left it alone");
    assert_eq!(wake_count(&t, &run).await, 0);
    assert_eq!(current.status, before.status, "the run itself carries on");
    assert_eq!(notices(&t, &run).await.len(), 0);

    // Turning it back on starts from scratch.
    let detail = change_policy(&t, &run, &update(true, Some(2), false)).await;
    assert_eq!(
        detail.run.config.policy.continuation.unwrap().max_attempts,
        2
    );
    phase(&t, &run, "continue").await;
    due(&t, run.id).await;
    t.hub.orchestrator.continue_runs().await;
    idle(&t, run.planner).await;
    assert_eq!(wake_count(&t, &run).await, 1);
}

/// A run that started without the policy can be given one while it goes, and starts being
/// woken from the next interval on.
#[tokio::test]
async fn a_running_run_without_the_policy_can_be_turned_on() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let run = start(&t, json!({})).await;
    idle(&t, run.planner).await;
    for _ in 0..2 {
        t.hub.orchestrator.continue_runs().await;
    }
    assert!(get(&t, run.id).await.continuation.is_none());
    assert_eq!(wake_count(&t, &run).await, 0);

    let detail = change_policy(&t, &run, &update(true, Some(1), false)).await;
    let policy = detail.run.config.policy.continuation.clone().unwrap();
    assert_eq!((policy.max_attempts, policy.interval_seconds), (1, 3600));
    assert!(
        detail.run.continuation.is_none(),
        "the clock arms the first wakeup, not the person's call"
    );

    phase(&t, &run, "continue").await;
    assert_eq!(wake_count(&t, &run).await, 0, "only armed so far");
    due(&t, run.id).await;
    t.hub.orchestrator.continue_runs().await;
    idle(&t, run.planner).await;
    assert_eq!(get(&t, run.id).await.continuation.unwrap().attempts, 1);
    assert_eq!(wake_count(&t, &run).await, 1);
}

/// Changing the limits of a run that is already on keeps the rest of its policy and its
/// checkpoint: only the cap and the interval move.
#[tokio::test]
async fn changing_the_limits_keeps_the_rest_of_the_policy() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let run = start(
        &t,
        json!({
            "plan_approval":"auto",
            "continuation":{"max_attempts":2,"interval_seconds":3600},
        }),
    )
    .await;
    idle(&t, run.planner).await;
    phase(&t, &run, "continue").await;

    let detail = change_policy(&t, &run, &update(true, Some(5), false)).await;
    let policy = &detail.run.config.policy;
    assert_eq!(policy.continuation.as_ref().unwrap().max_attempts, 5);
    assert_eq!(
        policy.plan_approval,
        PlanApproval::Auto,
        "only the continuation policy moves"
    );
    assert_eq!(detail.run.continuation.unwrap().attempts, 0);
}

/// Re-arming a run that is not exhausted (attempts below the cap) is allowed and simply
/// puts the allowance back: the person may want a fresh count without changing the limits.
#[tokio::test]
async fn resetting_the_attempts_works_below_the_cap_too() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let run = start(
        &t,
        json!({"continuation":{"max_attempts":2,"interval_seconds":3600}}),
    )
    .await;
    idle(&t, run.planner).await;
    phase(&t, &run, "continue").await;
    due(&t, run.id).await;
    t.hub.orchestrator.continue_runs().await;
    idle(&t, run.planner).await;
    assert_eq!(get(&t, run.id).await.continuation.unwrap().attempts, 1);

    let detail = change_policy(&t, &run, &update(true, None, true)).await;
    assert_eq!(detail.run.continuation.unwrap().attempts, 0);
    assert_eq!(
        wake_count(&t, &run).await,
        1,
        "resetting wakes nothing itself"
    );
}

/// It is the person's call: a read-only credential and an agent's session token are both
/// refused, and nothing about the run changes.
#[tokio::test]
async fn only_the_person_may_change_the_policy() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let run = start(&t, json!({})).await;
    let body = json!(update(true, Some(5), true));

    let read = t
        .client
        .post(format!("{}/v1/runs/{}/policy", t.base, run.id))
        .bearer_auth(&t.hub.read_token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(read.status(), 403, "{}", read.text().await.unwrap());

    let anonymous = t
        .client
        .post(format!("{}/v1/runs/{}/policy", t.base, run.id))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);

    let current = get(&t, run.id).await;
    assert!(current.config.policy.continuation.is_none());
    assert!(current.continuation.is_none());
    assert_eq!(wake_count(&t, &run).await, 0);
}

/// A body that asks for nothing is refused instead of being a silent no-op.
#[tokio::test]
async fn an_empty_change_is_refused() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let run = start(&t, json!({})).await;
    let resp = t
        .post(&format!("/v1/runs/{}/policy", run.id), json!({}))
        .await;
    assert_eq!(resp.status(), 400, "{}", resp.text().await.unwrap());
}
