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
    start_plan(t, plan).await
}

/// Starts a run of `tasks`, worked by rex; returns the run.
async fn start_plan(t: &TestHub, plan: Value) -> Value {
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
    reached("a", want)
}

/// `task` reached `want`.
fn reached(task: &'static str, want: TaskStatus) -> impl Fn(&Event) -> bool {
    move |e| matches!(&e.kind, EventKind::TaskStatusChanged { key, to, .. } if key == task && *to == want)
}

/// A worker goal: do `directives`, then report done with `summary`.
fn goal(directives: &str, summary: &str) -> String {
    format!(
        "{directives} {}",
        call(
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": summary })
        )
    )
}

/// Passes when `file` is tracked and holds `word`.
fn has(file: &str, word: &str) -> String {
    format!("git grep -q {word} -- {file}")
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
    // A new execution session of the escalation teammate: an `e-` name, not `w-`.
    assert!(card.to_name.starts_with("e-"), "{card:?}");
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
    assert_eq!(
        template["config"]["spawned_by_run"], "an-old-run",
        "{template}"
    );
    assert_eq!(
        template["config"]["spawned_by_agent"],
        json!(boss.id),
        "{template}"
    );
}

// -------------------------------------------------------------------------------------
// Workflow handoffs (docs/research/multica.md §2.12): `spec.next` and `spec.on_reject`.

/// The task with `key` in the run's detail.
async fn task_by_key(t: &TestHub, run: &Value, key: &str) -> Value {
    let id = run["id"].as_str().unwrap();
    let detail: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    detail["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == key)
        .unwrap_or_else(|| panic!("no task {key} in {detail}"))
        .clone()
}

/// A plan of two tasks: `a` reports done and accepts itself, `next` says where it goes; `b`
/// waits for it — its `depends_on` is left out on purpose, since `next` is the plan.
fn two_steps(next: &str) -> Value {
    json!([
        {
            "key": "a",
            "title": "task a",
            "spec": {
                "goal": goal(
                    "[write a.txt=alpha]",
                    &format!("wrote a {}", call("task_accept", json!({ "task": "a" })))
                ),
                "acceptance": [has("a.txt", "alpha")],
                "scope": { "allow": ["a.txt"] },
                "next": next,
            },
        },
        {
            "key": "b",
            "title": "task b",
            "spec": { "goal": goal("[write b.txt=beta]", "wrote b"), "scope": { "allow": ["b.txt"] } },
        },
    ])
}

#[tokio::test]
async fn next_releases_the_step_it_names_once_the_task_is_accepted() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let run = start_plan(&t, two_steps("b")).await;

    // `b` waits for `a` although the plan gave it no depends_on: `next` is the edge.
    w.until(
        "a handed out",
        |e| matches!(&e.kind, EventKind::TaskHandedOff { handoff, .. } if handoff.task_key == "a"),
    )
    .await;
    w.until("a reported", reached("a", TaskStatus::Passed))
        .await;
    let b_before = task_by_key(&t, &run, "b").await;
    assert_eq!(b_before["status"], "pending", "{b_before}");
    assert_eq!(b_before["depends_on"], json!(["a"]), "{b_before}");

    w.until("a merged", reached("a", TaskStatus::Merged)).await;
    let merged = w.seen.len() - 1;
    let to_b = w
        .until("b handed out", |e| {
            matches!(&e.kind, EventKind::TaskHandedOff { handoff, .. } if handoff.task_key == "b")
        })
        .await;
    let at = w.seen.iter().position(|e| e.seq == to_b.seq).unwrap();
    assert!(merged < at, "b started before a was merged");

    let b = task_by_key(&t, &run, "b").await;
    assert_eq!(b["status"], "running", "{b}");
    // The lead is told what accepting released.
    w.until("the accept's answer", |e| {
        matches!(&e.kind, EventKind::AgentOutput { text, .. }
            if text.contains("\"next_step\"") && text.contains("#b is released"))
    })
    .await;
}

#[tokio::test]
async fn next_naming_a_teammate_opens_the_step_for_them() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let run = start_plan(&t, two_steps("ada")).await;
    w.until("a reported", reached("a", TaskStatus::Passed))
        .await;
    w.until("a merged", reached("a", TaskStatus::Merged)).await;

    // The step was not in the plan: the hub opened it and a new session of ada takes it.
    let event = w
        .until("the opened step handed out", |e| {
            matches!(&e.kind, EventKind::TaskHandedOff { handoff, .. } if handoff.task_key == "a-next")
        })
        .await;
    let EventKind::TaskHandedOff { handoff: card, .. } = event.kind else {
        unreachable!()
    };
    assert_eq!(card.kind, HandoffKind::Assign);
    assert_eq!(card.template.as_deref(), Some("ada"));
    assert!(card.to_name.starts_with("w-"), "{card:?}");

    let (_, text) = first_input(&mut w, card.to).await;
    assert!(text.contains("Task #a-next"), "{text}");
    assert!(text.contains("the step before this one, merged"), "{text}");

    let step = task_by_key(&t, &run, "a-next").await;
    assert_eq!(step["depends_on"], json!(["a"]), "{step}");
    assert_eq!(step["spec"]["next"], Value::Null, "{step}");
    assert_eq!(
        step["spec"]["goal"],
        task_by_key(&t, &run, "a").await["spec"]["goal"],
        "the step takes over this task's contract"
    );
    // The lead hears of a step it did not plan.
    w.find("the lead's notice", |e| {
        matches!(&e.kind, EventKind::AgentInput { text, .. }
            if text.contains("#a was accepted") && text.contains("#a-next is a new step for ada"))
    })
    .await;
}

#[tokio::test]
async fn on_reject_sends_the_work_back_to_the_teammate_it_names() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let run = start(
        &t,
        json!({
            "goal": goal("[write a.txt=alpha]", "wrote a"),
            "acceptance": [has("a.txt", "alpha")],
            "on_reject": "ada",
        }),
    )
    .await;
    let first = card(&mut w, HandoffKind::Assign).await;
    w.until("reported", task_is(TaskStatus::Passed)).await;

    let lead = run["planner"].as_str().unwrap();
    t.send(
        lead,
        &call(
            "task_rework",
            json!({ "task": "a", "notes": "the API is wrong; keep the old one" }),
        ),
    )
    .await;
    let back = card(&mut w, HandoffKind::Reassign).await;
    assert_eq!(back.from, Some(first.to), "the worker before gives it back");
    assert_eq!(back.template.as_deref(), Some("ada"));
    assert_ne!(back.to, first.to);
    let reason = back.reason.expect("a send-back says why");
    assert!(reason.contains("keep the old one"), "{reason}");
    assert!(
        back.decisions
            .contains(&"the API is wrong; keep the old one".to_owned()),
        "{:?}",
        back.decisions
    );
    let (_, text) = first_input(&mut w, back.to).await;
    assert!(text.contains("keep the old one"), "{text}");
    assert!(text.contains(&first.to_name), "{text}");
}

#[tokio::test]
async fn the_lead_previews_where_the_work_goes() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let mut plan = two_steps("b");
    plan[0]["spec"]["on_reject"] = json!("ada");
    let run = start_plan(&t, plan).await;
    let lead = run["planner"].as_str().unwrap().to_owned();

    t.send(
        lead.as_str(),
        &call("handoff_preview", json!({ "task": "a" })),
    )
    .await;
    let seen = w
        .until("the preview", |e| {
            matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains("called handoff_preview"))
        })
        .await;
    let EventKind::AgentOutput { text, .. } = seen.kind else {
        unreachable!()
    };
    let preview = json_in(&text, "called handoff_preview: ");
    assert_eq!(preview["next"]["goes_to"], "#b", "{preview}");
    assert_eq!(preview["next"]["when"], "when this task is accepted");
    let brief = preview["next"]["brief"].as_str().unwrap();
    assert!(brief.contains("Task #b"), "{brief}");
    assert!(brief.contains("b.txt"), "{brief}");
    assert_eq!(preview["on_reject"]["goes_to"], "ada", "{preview}");
    assert!(
        preview["on_reject"]["who"]
            .as_str()
            .unwrap()
            .contains("ada"),
        "{preview}"
    );

    // The same section comes back from task_get.
    t.send(lead.as_str(), &call("task_get", json!({ "task": "a" })))
        .await;
    let seen = w
        .until("task_get", |e| {
            matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains("called task_get"))
        })
        .await;
    let EventKind::AgentOutput { text, .. } = seen.kind else {
        unreachable!()
    };
    let detail = json_in(&text, "called task_get: ");
    assert_eq!(detail["handoff"]["next"]["goes_to"], "#b", "{detail}");
}

/// The first JSON value after `marker` in an agent's output.
fn json_in(text: &str, marker: &str) -> Value {
    let rest = text.split_once(marker).expect("the tool's answer").1;
    serde_json::Deserializer::from_str(rest)
        .into_iter()
        .next()
        .unwrap()
        .unwrap()
}
