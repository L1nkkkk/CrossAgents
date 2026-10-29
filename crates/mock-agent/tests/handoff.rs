//! Handoff cards (docs/research/magpie-lancer.md §2.1): every time a task changes hands the
//! hub stores a card on the task and publishes `task.handed_off` before the new worker hears
//! of it, and the new worker's first message carries the card.
//!
//! Workers here only act on directives: a goal of `[write a.txt=alpha]` writes and ends the
//! turn without a report, so the task stalls and the lead (told by the test) reassigns it.

mod common;

use std::path::Path;

use common::{MOCK_AGENT, TestHub, Watcher, git_repo};
use crossagents_api_types::{Event, EventKind, Handoff, HandoffKind, TaskStatus};
use crossagents_core::DriverKind;
use crossagents_plugins::PluginConfig;
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

async fn setup(repo: &Path) -> TestHub {
    let t = TestHub::start().await;
    t.add_agent_in("boss", Some("opus"), "planner", repo).await;
    t.add_agent_in("rex", Some("haiku"), "worker", repo).await;
    t.add_agent_in("ada", Some("opus"), "worker", repo).await;
    t
}

/// Starts a run of one task `a` with `spec`, worked by rex; returns the run.
async fn start(t: &TestHub, spec: Value) -> Value {
    let plan = json!([{ "key": "a", "title": "task a", "spec": spec }]);
    let request = format!(
        "Build it. {}",
        call("plan_submit", json!({ "tasks": plan }))
    );
    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": request, "planner": "boss", "worker": "rex",
                    "policy": { "plan_approval": "auto" } }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    resp.json().await.unwrap()
}

fn task_is(want: TaskStatus) -> impl Fn(&Event) -> bool {
    move |e| matches!(&e.kind, EventKind::TaskStatusChanged { key, to, .. } if key == "a" && *to == want)
}

async fn card(w: &mut Watcher, kind: HandoffKind) -> Handoff {
    let event = w
        .until(
            "task handed off",
            |e| matches!(&e.kind, EventKind::TaskHandedOff { handoff, .. } if handoff.kind == kind),
        )
        .await;
    let EventKind::TaskHandedOff { handoff, .. } = event.kind else {
        unreachable!()
    };
    handoff
}

/// The first message `agent` was sent, and where it is in what `w` saw.
async fn first_input(w: &mut Watcher, agent: crossagents_api_types::AgentId) -> (usize, String) {
    let event = w
        .find(
            "the new worker's first message",
            |e| matches!(&e.kind, EventKind::AgentInput { id, .. } if *id == agent),
        )
        .await;
    let at = w.seen.iter().position(|e| e.seq == event.seq).unwrap();
    let EventKind::AgentInput { text, .. } = event.kind else {
        unreachable!()
    };
    (at, text)
}

/// Tells the lead to give task `a` to `to`.
async fn reassign(t: &TestHub, run: &Value, to: &str) {
    let lead = run["planner"].as_str().unwrap();
    t.send(lead, &call("task_assign", json!({ "task": "a", "to": to })))
        .await;
}

async fn task_a(t: &TestHub, run: &Value) -> Value {
    let id = run["id"].as_str().unwrap();
    let detail: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    detail["tasks"][0].clone()
}

#[tokio::test]
async fn the_first_worker_gets_an_assign_card_from_the_lead() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let run = start(
        &t,
        json!({ "goal": "[write a.txt=alpha]", "scope": { "allow": ["a.txt", "src/"] },
                "interfaces": ["fn a() -> u8"] }),
    )
    .await;

    let card = card(&mut w, HandoffKind::Assign).await;
    assert_eq!(
        card.from.map(|a| a.to_string()),
        run["planner"].as_str().map(str::to_owned)
    );
    assert_eq!(card.from_name.as_deref(), Some("boss"));
    // The card names the execution session that works the task, and the template it came from.
    assert!(card.to_name.starts_with("w-"), "{card:?}");
    assert_ne!(card.to_name, "rex");
    assert_eq!(card.template.as_deref(), Some("rex"));
    let session: Value = t
        .get(&format!("/v1/agents/{}", card.to))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(session["name"], card.to_name, "{session}");
    assert_eq!(card.task_key, "a");
    assert_eq!(card.files, ["a.txt", "src/"]);
    assert_eq!(card.decisions, ["fn a() -> u8"]);
    assert_eq!(card.goal, "[write a.txt=alpha]");
    assert!(card.reason.is_none() && card.summary.is_none());
    // Announced before the worker heard of it.
    let handed = w.seen.len() - 1;
    let (first, _) = first_input(&mut w, card.to).await;
    assert!(
        handed < first,
        "the card came after the worker's first message"
    );
    // Stored on the task.
    let task = task_a(&t, &run).await;
    assert_eq!(task["handoffs"].as_array().unwrap().len(), 1, "{task}");
    assert_eq!(task["handoffs"][0]["kind"], "assign");
}

#[tokio::test]
async fn task_assign_hands_over_the_files_the_worker_changed() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let run = start(&t, json!({ "goal": "[write notes.txt=alpha]" })).await;
    let first = card(&mut w, HandoffKind::Assign).await;
    w.until("stalled", task_is(TaskStatus::Stalled)).await;

    reassign(&t, &run, "ada").await;
    let card = card(&mut w, HandoffKind::Reassign).await;
    assert_eq!(card.from, Some(first.to));
    assert_eq!(card.from_name.as_deref(), Some(first.to_name.as_str()));
    assert_eq!(card.template.as_deref(), Some("ada"));
    assert!(card.to_name.starts_with("w-"), "{card:?}");
    assert_ne!(card.to_name, first.to_name);
    assert!(
        card.files.contains(&"notes.txt".to_owned()),
        "{:?}",
        card.files
    );
    let handed = w.seen.len() - 1;
    let (at, text) = first_input(&mut w, card.to).await;
    assert!(
        handed < at,
        "the card came after the worker's first message"
    );
    assert!(text.contains("notes.txt"), "{text}");
    assert!(text.contains(&first.to_name), "{text}");
    assert!(
        !text.contains("Someone already worked on this; their changes"),
        "{text}"
    );

    let task = task_a(&t, &run).await;
    let kinds: Vec<&str> = task["handoffs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["assign", "reassign"], "{task}");
}

#[tokio::test]
async fn a_quota_reroute_is_a_reroute_card_with_the_same_reason() {
    let repo = git_repo();
    let t = TestHub::start_with(|config| {
        config.plugins.insert(
            "codex".into(),
            PluginConfig {
                driver: Some(DriverKind::CodexNative),
                native_command: vec![MOCK_AGENT.into(), "app-server".into()],
                ..PluginConfig::default()
            },
        );
    })
    .await;
    let homes = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    for (id, home) in ["codex-a", "codex-b"].iter().zip(&homes) {
        let resp = t
            .post(
                "/v1/accounts",
                json!({ "id": id, "provider": "codex", "home": home.path().display().to_string() }),
            )
            .await;
        assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    }
    t.add_agent_in("boss", Some("opus"), "planner", repo.path())
        .await;
    let resp = t
        .post(
            "/v1/agents",
            json!({ "name": "coder", "plugin": "codex", "model": "mock-codex", "role": "worker",
                    "cwd": repo.path().display().to_string(), "config": { "account": "codex-a" } }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let mut w = t.watch().await;
    let plan =
        json!([{ "key": "a", "title": "task a", "spec": { "goal": "[limit 100] work on it" } }]);
    let req = format!("Build it. [call plan_submit {}]", json!({ "tasks": plan }));
    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": req, "planner": "boss", "worker": "coder", "policy": { "plan_approval": "auto" } }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());

    let first = card(&mut w, HandoffKind::Assign).await;
    let card = card(&mut w, HandoffKind::Reroute).await;
    assert_eq!(card.from, Some(first.to));
    assert_eq!(card.template.as_deref(), Some("coder"));
    assert!(card.to_name.starts_with("w-"), "{card:?}");
    let reason = card.reason.clone().expect("a reroute says why");
    assert!(reason.contains("codex-b"), "{reason}");
    let rerouted: Vec<String> = w
        .seen
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::TaskRerouted { reason, to, .. } if *to == card.to => Some(reason.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(rerouted, [reason]);
}

#[tokio::test]
async fn rework_notes_travel_as_decisions() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let done = call(
        "task_complete",
        json!({ "task_id": "{task_id}", "summary": "wrote a" }),
    );
    let run = start(&t, json!({ "goal": format!("[write a.txt=alpha] {done}") })).await;
    w.until("reported", task_is(TaskStatus::Passed)).await;

    let lead = run["planner"].as_str().unwrap();
    t.send(
        lead,
        &call(
            "task_rework",
            json!({ "task": "a", "notes": "keep the old API" }),
        ),
    )
    .await;
    w.until("back to running", task_is(TaskStatus::Running))
        .await;
    // The mock reports again on its rework turn; the issuer then gives it to someone else.
    w.until("reported again", task_is(TaskStatus::Passed)).await;

    reassign(&t, &run, "ada").await;
    let card = card(&mut w, HandoffKind::Reassign).await;
    assert!(
        card.decisions.contains(&"keep the old API".to_owned()),
        "{:?}",
        card.decisions
    );
    assert_eq!(card.summary.as_deref(), Some("wrote a"));
    let (_, text) = first_input(&mut w, card.to).await;
    assert!(text.contains("keep the old API"), "{text}");
}

#[tokio::test]
async fn an_unanswered_question_stays_open_on_the_card() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let ask = call(
        "task_ask",
        json!({ "task_id": "{task_id}", "question": "which way?" }),
    );
    let run = start(&t, json!({ "goal": ask })).await;
    w.until("asking", task_is(TaskStatus::WaitingAnswer)).await;

    reassign(&t, &run, "ada").await;
    let card = card(&mut w, HandoffKind::Reassign).await;
    assert_eq!(card.open_questions, ["which way?"]);
    let (_, text) = first_input(&mut w, card.to).await;
    assert!(text.contains("which way?"), "{text}");
}

#[tokio::test]
async fn a_worktree_that_is_gone_hands_over_no_files() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let run = start(&t, json!({ "goal": "[write a.txt=alpha]" })).await;
    let first = card(&mut w, HandoffKind::Assign).await;
    w.until("stalled", task_is(TaskStatus::Stalled)).await;

    // The worker's process lives in the worktree: stop it before removing it.
    let resp = t
        .post(&format!("/v1/agents/{}/stop", first.to), json!({}))
        .await;
    assert!(resp.status().is_success(), "{}", resp.text().await.unwrap());
    let task = task_a(&t, &run).await;
    let worktree = task["worktree"].as_str().unwrap().to_owned();
    std::fs::remove_dir_all(&worktree).unwrap();

    reassign(&t, &run, "ada").await;
    let card = card(&mut w, HandoffKind::Reassign).await;
    assert!(card.files.is_empty(), "{:?}", card.files);
    assert_eq!(card.from, Some(first.to));
    let task = task_a(&t, &run).await;
    assert_eq!(task["handoffs"].as_array().unwrap().len(), 2, "{task}");
}

#[tokio::test]
async fn giving_it_to_the_escalation_teammate_is_an_escalation() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let plan =
        json!([{ "key": "a", "title": "task a", "spec": { "goal": "[write a.txt=alpha]" } }]);
    let request = format!(
        "Build it. {}",
        call("plan_submit", json!({ "tasks": plan }))
    );
    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": request, "planner": "boss", "worker": "rex", "escalate": "ada",
                    "policy": { "plan_approval": "auto" } }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let run: Value = resp.json().await.unwrap();
    w.until("stalled", task_is(TaskStatus::Stalled)).await;

    reassign(&t, &run, "ada").await;
    let card = card(&mut w, HandoffKind::Escalate).await;
    assert_eq!(card.template.as_deref(), Some("ada"));
    assert_ne!(card.to_name, "ada");
    let session: Value = t
        .get(&format!("/v1/agents/{}", card.to))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(session["name"], card.to_name, "{session}");
    let reason = card.reason.expect("an escalation says why");
    assert!(reason.contains("escalation"), "{reason}");
    assert!(card.files.contains(&"a.txt".to_owned()), "{:?}", card.files);
}

/// Two handoffs in a row: each card carries what its own `from` left. rex reports a
/// summary and gives way to ada; ada only says how far she got and gives way to kai. kai's
/// card is ada's word, not rex's summary under ada's name.
#[tokio::test]
async fn each_card_carries_what_its_own_worker_left() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    t.add_agent_in("kai", Some("opus"), "worker", repo.path())
        .await;
    let mut w = t.watch().await;
    let run = start(&t, json!({ "goal": "Write a." })).await;
    let first = card(&mut w, HandoffKind::Assign).await;

    t.send(
        &first.to.to_string(),
        &call(
            "task_complete",
            json!({ "task": "a", "summary": "rex wrote a" }),
        ),
    )
    .await;
    w.until("reported", task_is(TaskStatus::Passed)).await;

    reassign(&t, &run, "ada").await;
    let to_ada = card(&mut w, HandoffKind::Reassign).await;
    assert_eq!(to_ada.summary.as_deref(), Some("rex wrote a"));
    // The summary went with the card: the task no longer says it is ada's.
    let task = task_a(&t, &run).await;
    assert!(task["report"]["summary"].is_null(), "{task}");

    t.send(
        &to_ada.to.to_string(),
        &call(
            "task_progress",
            json!({ "task": "a", "text": "ada is halfway" }),
        ),
    )
    .await;
    w.until(
        "ada's progress",
        |e| matches!(&e.kind, EventKind::TaskProgress { text, .. } if text == "ada is halfway"),
    )
    .await;

    reassign(&t, &run, "kai").await;
    let to_kai = w
        .until(
            "the card to kai",
            |e| matches!(&e.kind, EventKind::TaskHandedOff { handoff, .. } if handoff.template.as_deref() == Some("kai")),
        )
        .await;
    let EventKind::TaskHandedOff {
        handoff: to_kai, ..
    } = to_kai.kind
    else {
        unreachable!()
    };
    assert_eq!(to_kai.from, Some(to_ada.to));
    assert_eq!(to_kai.from_name.as_deref(), Some(to_ada.to_name.as_str()));
    assert_eq!(
        to_kai.summary.as_deref(),
        Some("ada is halfway"),
        "{to_kai:?}"
    );
    let (_, text) = first_input(&mut w, to_kai.to).await;
    assert!(!text.contains("rex wrote a"), "{text}");
}

/// Neither an old failure nor what the hub said on a handoff is the next worker's: rex gives
/// up, the task goes to ada for that reason, ada says nothing and gives way to kai, whose card
/// has no reason and no last word.
#[tokio::test]
async fn a_worker_that_said_nothing_hands_over_nothing_said() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    t.add_agent_in("kai", Some("opus"), "worker", repo.path())
        .await;
    let mut w = t.watch().await;
    let run = start(&t, json!({ "goal": "Write a." })).await;
    let first = card(&mut w, HandoffKind::Assign).await;
    t.send(
        &first.to.to_string(),
        &call("task_fail", json!({ "task": "a", "reason": "rex gave up" })),
    )
    .await;
    w.until("stalled", task_is(TaskStatus::Stalled)).await;

    reassign(&t, &run, "ada").await;
    let to_ada = card(&mut w, HandoffKind::Reassign).await;
    assert_eq!(to_ada.reason.as_deref(), Some("rex gave up"));
    // The failure went with the card; the task's line says why it changed hands until ada
    // says something.
    let task = task_a(&t, &run).await;
    assert!(task["report"]["failure"].is_null(), "{task}");
    assert_eq!(task["progress"], "rex gave up", "{task}");

    reassign(&t, &run, "kai").await;
    let to_kai = w
        .until(
            "the card to kai",
            |e| matches!(&e.kind, EventKind::TaskHandedOff { handoff, .. } if handoff.template.as_deref() == Some("kai")),
        )
        .await;
    let EventKind::TaskHandedOff {
        handoff: to_kai, ..
    } = to_kai.kind
    else {
        unreachable!()
    };
    assert_eq!(to_kai.from_name.as_deref(), Some(to_ada.to_name.as_str()));
    assert!(to_kai.reason.is_none(), "{to_kai:?}");
    assert!(to_kai.summary.is_none(), "{to_kai:?}");
}

/// The lead hears which execution session took the task, by the name it answers to.
#[tokio::test]
async fn the_lead_is_told_the_execution_sessions_name() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let run = start(&t, json!({ "goal": "[write a.txt=alpha]" })).await;
    let card = card(&mut w, HandoffKind::Assign).await;
    let lead = run["planner"].as_str().unwrap().to_owned();
    let want = format!("#a 已派给 {}（来自模板 rex）", card.to_name);
    let event = w
        .find("the lead's dispatch notice", |e| {
            matches!(&e.kind, EventKind::AgentInput { id, text, .. } | EventKind::AgentInputUpdated { id, text, .. }
                if id.to_string() == lead && text.contains(&want))
        })
        .await;
    let (EventKind::AgentInput { text, .. } | EventKind::AgentInputUpdated { text, .. }) =
        event.kind
    else {
        unreachable!()
    };
    assert!(text.contains("给它发消息用这个名字"), "{text}");
}

/// A worker cloned from a template is the run's execution session, not anyone's recruit: the
/// template's `spawned_by_*` keys stay on the template and do not come along.
#[tokio::test]
async fn a_cloned_worker_does_not_carry_the_templates_recruit_marks() {
    let repo = git_repo();
    let t = TestHub::start().await;
    let boss = t
        .add_agent_in("boss", Some("opus"), "planner", repo.path())
        .await;
    let resp = t
        .post(
            "/v1/agents",
            json!({ "name": "rex", "plugin": "mock", "model": "haiku", "role": "worker",
                    "cwd": repo.path().display().to_string(),
                    "config": { "spawned_by_agent": boss.id, "spawned_by_run": "an-old-run" } }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let mut w = t.watch().await;
    start(&t, json!({ "goal": "[write a.txt=alpha]" })).await;
    let card = card(&mut w, HandoffKind::Assign).await;
    assert_eq!(card.template.as_deref(), Some("rex"));

    let clone: Value = t
        .get(&format!("/v1/agents/{}", card.to))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(clone["name"], card.to_name, "{clone}");
    assert!(clone["config"].get("spawned_by_agent").is_none(), "{clone}");
    assert!(clone["config"].get("spawned_by_run").is_none(), "{clone}");
    let template: Value = t.get("/v1/agents/rex").await.json().await.unwrap();
    assert_eq!(template["config"]["spawned_by_run"], "an-old-run", "{template}");
    assert_eq!(
        template["config"]["spawned_by_agent"],
        json!(boss.id),
        "{template}"
    );
}
