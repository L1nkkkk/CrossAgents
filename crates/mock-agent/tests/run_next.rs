//! Successive rounds share the lead's checkout, but retain separate branches and records.
mod common;

use common::{TestHub, git_repo};
use crossagents_api_types::{AgentId, EventKind, RunStatus};
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

/// Who a run's kickoff says it came from, in the lead's transcript.
async fn kickoff_from(t: &TestHub, lead: &str, run: &Value) -> Value {
    let transcript: Value = t
        .get(&format!("/v1/agents/{lead}/transcript"))
        .await
        .json()
        .await
        .unwrap();
    let marker = format!("(run_id: {})", run["id"].as_str().unwrap());
    transcript["turns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|turn| turn["input"].as_str().unwrap().contains(&marker))
        .unwrap_or_else(|| panic!("no kickoff of {run} in {transcript}"))["from"]
        .clone()
}

/// The text of a run's kickoff in the lead's transcript.
async fn kickoff_text(t: &TestHub, lead: &str, run: &Value) -> String {
    let transcript: Value = t
        .get(&format!("/v1/agents/{lead}/transcript"))
        .await
        .json()
        .await
        .unwrap();
    let marker = format!("(run_id: {})", run["id"].as_str().unwrap());
    transcript["turns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|turn| turn["input"].as_str().unwrap().contains(&marker))
        .unwrap_or_else(|| panic!("no kickoff of {run} in {transcript}"))["input"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Every session the agent has started, oldest first, from the event log.
async fn sessions_started(t: &TestHub, agent: AgentId) -> Vec<String> {
    t.hub
        .bus
        .store()
        .agent_events(agent, 0, 10_000)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|event| match event.kind {
            EventKind::AgentSessionStarted { session_id, .. } => Some(session_id),
            _ => None,
        })
        .collect()
}

/// What the mock printed from `session` on: it says itself with `session=<id> (new)`, and
/// everything after that line belongs to it (or to what came later).
fn output_from_session(w: &common::Watcher, session: &str) -> String {
    let output = w.output();
    let marker = format!("session={session} (new) ");
    output
        .split(&marker)
        .nth(1)
        .unwrap_or_else(|| panic!("no new session {session} in {output}"))
        .to_owned()
}

/// The new session recalls nothing: what it knows of the old conversation is the handoff in
/// its kickoff, not the session's memory.
fn assert_session_recalls_nothing(w: &common::Watcher, session: &str) {
    let tail = output_from_session(w, session);
    assert!(tail.starts_with("recall= "), "{tail}");
}

/// The warnings the agent's event log holds, as text.
async fn warnings_of(t: &TestHub, agent: AgentId) -> Vec<String> {
    t.hub
        .bus
        .store()
        .agent_events(agent, 0, 10_000)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|event| match event.kind {
            EventKind::AgentWarning { message, .. } => Some(message),
            _ => None,
        })
        .collect()
}

/// Writes the team note `constraints`, which every later round is under.
async fn put_constraints(t: &TestHub, author: AgentId, text: &str) {
    t.hub
        .bus
        .store()
        .put_note(&crossagents_core::Note {
            scope: crossagents_core::NoteScope::Team,
            run_id: None,
            key: "constraints".into(),
            text: text.into(),
            author,
            updated_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
}

async fn setup(t: &TestHub, repo: &std::path::Path) -> String {
    t.add_agent_in("Pia", None, "planner", repo).await;
    let response = t
        .post("/v1/works", json!({"title":"rounds", "lead":"Pia"}))
        .await;
    assert_eq!(response.status(), 201);
    response.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn round(t: &TestHub, work: &str, request: String, from: Option<&str>) -> Value {
    let response = t
        .post(
            &format!("/v1/works/{work}/runs"),
            json!({"title":"A round", "request":request,"from":from}),
        )
        .await;
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
    response.json().await.unwrap()
}

async fn completed(w: &mut common::Watcher, run: &Value) {
    let id = run["id"].as_str().unwrap();
    w.until("round completed", |e| matches!(&e.kind, EventKind::RunStatusChanged { id: rid, to: RunStatus::Completed, .. } if rid.to_string() == id)).await;
    // The next person message must wait until the mock finishes its closing turn.
    w.until("turn completed", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;
}

fn change(file: &str) -> String {
    format!(
        "[write {file}=from {file}] {}",
        call(
            "run_finish",
            json!({"summary":format!("Added {file}\nMore details")})
        )
    )
}

async fn land(t: &TestHub, run: &Value) {
    let response = t
        .post(
            &format!("/v1/runs/{}/land", run["id"].as_str().unwrap()),
            json!({"mode":"merge"}),
        )
        .await;
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
}

async fn detail(t: &TestHub, run: &Value) -> Value {
    t.get(&format!("/v1/runs/{}", run["id"].as_str().unwrap()))
        .await
        .json::<Value>()
        .await
        .unwrap()["run"]
        .clone()
}

#[tokio::test]
async fn unlanded_round_stacks_and_lands_ancestors_without_moving_lead() {
    let repo = git_repo();
    let t = TestHub::start().await;
    let work = setup(&t, repo.path()).await;
    let mut w = t.watch().await;
    let first = round(&t, &work, change("first.txt"), None).await;
    completed(&mut w, &first).await;
    let before = t.agent("Pia").await;
    let checkout = before.cwd.clone();
    let marker = checkout.parent().unwrap().join("targets/integration/keep");
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, "cache").unwrap();
    t.send(
        "Pia",
        &call(
            "run_next",
            json!({"title":"second", "request":change("second.txt")}),
        ),
    )
    .await;
    let event = w
        .until(
            "next round",
            |e| matches!(&e.kind, EventKind::RunCreated { run } if run.title == "second"),
        )
        .await;
    let EventKind::RunCreated { run } = event.kind else {
        unreachable!()
    };
    let second = serde_json::to_value(run).unwrap();
    completed(&mut w, &second).await;
    assert_eq!(second["base_branch"], first["branch"]);
    assert_eq!(t.agent("Pia").await.cwd, checkout);
    assert_eq!(t.agent("Pia").await.id, before.id);
    assert!(checkout.join("first.txt").exists());
    assert!(checkout.join("second.txt").exists());
    // Each round's finish reclaims the build trees; the checkout stays for the next round.
    assert!(!marker.exists(), "finish reclaims the group's targets");
    assert!(!repo.path().join("first.txt").exists());
    assert!(w.output().contains("Previous rounds in this work group"));
    assert!(w.output().contains("Added first.txt"));
    land(&t, &second).await;
    assert!(repo.path().join("first.txt").exists());
    assert!(repo.path().join("second.txt").exists());
    assert_eq!(detail(&t, &first).await["landed"]["by"], "随第 2 轮落地");
    let log = std::process::Command::new("git")
        .args(["log", "-1", "--format=%B"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&log.stdout).contains("第 1 轮"));
    let questions: Value = t.get("/v1/questions?all=true").await.json().await.unwrap();
    assert!(
        !questions["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["action"] == "work")
    );
}

#[tokio::test]
async fn landed_round_and_explicit_base_start_from_current_baseline() {
    for landed in [true, false] {
        let repo = git_repo();
        let t = TestHub::start().await;
        let work = setup(&t, repo.path()).await;
        let mut w = t.watch().await;
        let first = round(&t, &work, change("first.txt"), None).await;
        completed(&mut w, &first).await;
        let checkout = t.agent("Pia").await.cwd;
        if landed {
            land(&t, &first).await;
            // Landed into base, last round, clean, lead idle: the checkout goes and Pia is
            // back in the repository; the next round builds a new checkout.
            assert!(!checkout.exists(), "landing removed the checkout");
            assert_eq!(t.agent("Pia").await.cwd, repo.path());
        }
        let second = round(&t, &work, change("second.txt"), (!landed).then_some("base")).await;
        completed(&mut w, &second).await;
        assert_eq!(second["base_branch"], "main");
        let lead = t.agent(second["planner"].as_str().unwrap()).await;
        assert_eq!(lead.cwd, checkout);
        assert_eq!(checkout.join("first.txt").exists(), landed);
        land(&t, &second).await;
        assert_eq!(repo.path().join("first.txt").exists(), landed);
        assert_eq!(detail(&t, &first).await["landed"].is_null(), !landed);
    }
}

#[tokio::test]
async fn run_next_refuses_in_progress_and_invalid_from() {
    let repo = git_repo();
    let t = TestHub::start().await;
    let work = setup(&t, repo.path()).await;
    let mut w = t.watch().await;
    let first = round(
        &t,
        &work,
        call("run_next", json!({"title":"refused", "request":"new goal"})),
        None,
    )
    .await;
    w.until("turn completed", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;
    assert!(w.output().contains("terminal run"), "{}", w.output());
    let response = t
        .post(
            &format!("/v1/works/{work}/runs"),
            json!({"request":"other"}),
        )
        .await;
    assert_eq!(response.status(), 409);
    let invalid = t
        .post(
            &format!("/v1/works/{work}/runs"),
            json!({"request":"other","from":"other"}),
        )
        .await;
    assert_eq!(invalid.status(), 409);
    assert_eq!(detail(&t, &first).await["status"], "planning");
}

#[tokio::test]
async fn stacked_rounds_survive_restart_and_land_transitively() {
    let repo = git_repo();
    let t = TestHub::start().await;
    let work = setup(&t, repo.path()).await;
    let mut w = t.watch().await;
    let first = round(&t, &work, change("one.txt"), None).await;
    completed(&mut w, &first).await;
    let second = round(&t, &work, change("two.txt"), None).await;
    completed(&mut w, &second).await;
    let checkout = t.agent("Pia").await.cwd;
    let home = t.restart_home().await;
    let t = TestHub::start_in(home, |_| {}).await;
    let mut w = t.watch().await;
    let third = round(&t, &work, change("three.txt"), None).await;
    completed(&mut w, &third).await;
    assert_eq!(third["base_branch"], second["branch"]);
    assert_eq!(t.agent("Pia").await.cwd, checkout);
    let cleanup = t
        .post(
            &format!("/v1/runs/{}/cleanup", first["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(cleanup.status(), 409);
    land(&t, &third).await;
    for file in ["one.txt", "two.txt", "three.txt"] {
        assert!(repo.path().join(file).exists());
    }
    assert_eq!(detail(&t, &first).await["landed"]["by"], "随第 3 轮落地");
    assert_eq!(detail(&t, &second).await["landed"]["by"], "随第 3 轮落地");
}

#[tokio::test]
async fn run_without_a_work_group_gets_one_and_carries_on() {
    let repo = git_repo();
    let t = TestHub::start().await;
    t.add_agent_in("Pia", None, "planner", repo.path()).await;
    let mut w = t.watch().await;
    let response = t
        .post(
            "/v1/runs",
            json!({"planner":"Pia", "title":"first goal", "request":change("first.txt")}),
        )
        .await;
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
    let first: Value = response.json().await.unwrap();
    let run_id = first["id"].as_str().unwrap().to_owned();
    completed(&mut w, &first).await;

    // A run from before groups existed: its row has no work group.
    let home = t.restart_home().await;
    let paths = crossagents_hub::Paths::new(home.path());
    let store = crossagents_store_sqlite::SqliteStore::open(&paths.db())
        .await
        .unwrap();
    crossagents_store_sqlite::sqlx::query(
        "UPDATE runs SET data = json_remove(data, '$.work_id') WHERE id = ?",
    )
    .bind(&run_id)
    .execute(store.pool())
    .await
    .unwrap();
    store.close().await;

    let t = TestHub::start_in(home, |_| {}).await;
    let mut w = t.watch().await;
    assert!(detail(&t, &first).await["work_id"].is_null());
    t.send(
        "Pia",
        &call(
            "run_next",
            json!({"title":"second", "request":change("second.txt")}),
        ),
    )
    .await;
    let event = w
        .until(
            "next round",
            |e| matches!(&e.kind, EventKind::RunCreated { run } if run.title == "second"),
        )
        .await;
    let EventKind::RunCreated { run } = event.kind else {
        unreachable!()
    };
    let second = serde_json::to_value(run).unwrap();
    let work = second["work_id"]
        .as_str()
        .expect("the round joins a work group")
        .to_owned();
    let old = detail(&t, &first).await;
    assert_eq!(old["work_id"], json!(work));
    let group: Value = t
        .get(&format!("/v1/works/{work}"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(group["work"]["title"], "first goal");
    assert_eq!(group["work"]["lead"], old["planner"]);
    assert!(
        group["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == run_id.as_str())
    );
    completed(&mut w, &second).await;
    // The tool's answer names the group it made for the run that had none.
    assert!(w.output().contains("called run_next:"), "{}", w.output());
    assert!(w.output().contains(&work), "{}", w.output());
    // The person's words are already in the conversation: the lead's own round is not them again.
    let from = kickoff_from(&t, "Pia", &second).await;
    assert_eq!(from["kind"], "orchestrator", "{from}");
    assert_eq!(second["base_branch"], first["branch"]);
    assert!(t.agent("Pia").await.cwd.join("first.txt").exists());
    assert!(t.agent("Pia").await.cwd.join("second.txt").exists());
}

#[tokio::test]
async fn run_next_after_cleanup_builds_a_new_checkout_from_the_unlanded_branch() {
    let repo = git_repo();
    let t = TestHub::start().await;
    let work = setup(&t, repo.path()).await;
    let mut w = t.watch().await;
    let first = round(&t, &work, change("first.txt"), None).await;
    completed(&mut w, &first).await;
    let checkout = t.agent("Pia").await.cwd;
    let cleanup = t
        .post(
            &format!("/v1/runs/{}/cleanup", first["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(cleanup.status(), 200, "{}", cleanup.text().await.unwrap());
    assert!(!checkout.exists(), "cleanup took the checkout away");
    t.send(
        "Pia",
        &call(
            "run_next",
            json!({"title":"second", "request":change("second.txt")}),
        ),
    )
    .await;
    let event = w
        .until(
            "next round",
            |e| matches!(&e.kind, EventKind::RunCreated { run } if run.title == "second"),
        )
        .await;
    let EventKind::RunCreated { run } = event.kind else {
        unreachable!()
    };
    let second = serde_json::to_value(run).unwrap();
    assert_eq!(second["work_id"], first["work_id"]);
    assert_eq!(
        second["base_branch"], first["branch"],
        "the unlanded round's branch is what the new checkout starts from"
    );
    completed(&mut w, &second).await;
    // The checkout went with the cleanup, and so did the directory the lead's session lived
    // in: the round works in a new checkout, led by a copy.
    assert_eq!(t.agent("Pia").await.cwd, repo.path());
    let lead = t.agent(second["planner"].as_str().unwrap()).await;
    assert_ne!(lead.id, t.agent("Pia").await.id);
    assert_eq!(lead.cwd, checkout);
    assert!(checkout.exists(), "the round made itself a checkout");
    assert!(checkout.join("first.txt").exists());
    assert!(checkout.join("second.txt").exists());
}

#[tokio::test]
async fn after_landing_the_checkout_is_deleted_then_run_next_works() {
    let repo = git_repo();
    let t = TestHub::start().await;
    let work = setup(&t, repo.path()).await;
    let mut w = t.watch().await;
    let first = round(&t, &work, change("first.txt"), None).await;
    completed(&mut w, &first).await;
    let checkout = t.agent("Pia").await.cwd;
    land(&t, &first).await;
    assert!(repo.path().join("first.txt").exists());
    assert!(!checkout.exists(), "landing took the checkout away");
    assert_eq!(t.agent("Pia").await.cwd, repo.path());
    t.send(
        "Pia",
        &call(
            "run_next",
            json!({"title":"second", "request":change("second.txt")}),
        ),
    )
    .await;
    let event = w
        .until(
            "next round",
            |e| matches!(&e.kind, EventKind::RunCreated { run } if run.title == "second"),
        )
        .await;
    let EventKind::RunCreated { run } = event.kind else {
        unreachable!()
    };
    let second = serde_json::to_value(run).unwrap();
    assert_eq!(second["work_id"], first["work_id"]);
    assert_eq!(
        second["base_branch"], "main",
        "a landed round starts from base"
    );
    completed(&mut w, &second).await;
    let lead = t.agent(second["planner"].as_str().unwrap()).await;
    assert_ne!(
        lead.id,
        t.agent("Pia").await.id,
        "the non-moving mock keeps its copy path"
    );
    assert_eq!(lead.cwd, checkout);
    assert!(checkout.exists(), "the round made itself a new checkout");
    assert!(checkout.join("first.txt").exists());
    assert!(checkout.join("second.txt").exists());
    land(&t, &second).await;
    assert!(repo.path().join("second.txt").exists());
}

#[tokio::test]
async fn in_place_rounds_keep_the_current_directory_contents() {
    let t = TestHub::start().await;
    let repo = tempfile::tempdir().unwrap();
    let work = setup(&t, repo.path()).await;
    let mut w = t.watch().await;
    let first = round(&t, &work, change("one.txt"), None).await;
    completed(&mut w, &first).await;
    let checkout = t.agent("Pia").await.cwd;
    let second = round(&t, &work, change("two.txt"), Some("base")).await;
    completed(&mut w, &second).await;
    assert_eq!(t.agent("Pia").await.cwd, checkout);
    assert_eq!(second["branch"], "");
    assert!(second["in_place"].as_bool().unwrap());
    assert!(repo.path().join("one.txt").exists());
    assert!(repo.path().join("two.txt").exists());
    assert!(detail(&t, &first).await["landed"].is_null());
}

#[tokio::test]
async fn previous_round_can_land_separately_while_lead_stays_on_next_branch() {
    let repo = git_repo();
    let t = TestHub::start().await;
    let work = setup(&t, repo.path()).await;
    let mut w = t.watch().await;
    let first = round(&t, &work, change("one.txt"), None).await;
    completed(&mut w, &first).await;
    let second = round(&t, &work, change("two.txt"), None).await;
    completed(&mut w, &second).await;
    let checkout = t.agent("Pia").await.cwd;
    land(&t, &first).await;
    assert_eq!(t.agent("Pia").await.cwd, checkout);
    assert!(repo.path().join("one.txt").exists());
    assert!(!repo.path().join("two.txt").exists());
    let branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&branch.stdout).trim(),
        second["branch"].as_str().unwrap()
    );
    land(&t, &second).await;
    assert!(repo.path().join("two.txt").exists());
    assert_eq!(detail(&t, &first).await["landed"]["by"], "person");
}

#[tokio::test]
async fn a_copied_lead_starts_next_round_without_another_copy_or_move() {
    let repo = git_repo();
    let t = TestHub::start().await;
    let work = setup(&t, repo.path()).await;
    let mut w = t.watch().await;
    t.send("Pia", "hello").await;
    w.until("initial conversation ended", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;
    let first = round(&t, &work, change("one.txt"), None).await;
    completed(&mut w, &first).await;
    let lead_id = first["planner"].as_str().unwrap();
    let lead = t.agent(lead_id).await;
    assert_ne!(lead.id, t.agent("Pia").await.id);
    t.send(
        lead_id,
        &call(
            "run_next",
            json!({"title":"copied lead's second", "request":change("two.txt")}),
        ),
    )
    .await;
    let event = w.until("copied lead continued", |e| matches!(&e.kind, EventKind::RunCreated { run } if run.title == "copied lead's second")).await;
    let EventKind::RunCreated { run } = event.kind else {
        unreachable!()
    };
    let second = serde_json::to_value(run).unwrap();
    completed(&mut w, &second).await;
    assert_eq!(second["planner"], first["planner"]);
    assert_eq!(t.agent(lead_id).await.cwd, lead.cwd);
    assert!(lead.cwd.join("one.txt").exists());
    assert!(lead.cwd.join("two.txt").exists());
}

#[tokio::test]
async fn cancelled_round_keeps_its_checkout_and_next_starts_from_baseline() {
    let repo = git_repo();
    let t = TestHub::start().await;
    let work = setup(&t, repo.path()).await;
    let mut w = t.watch().await;
    let first = round(
        &t,
        &work,
        "[write cancelled.txt=keep on cancelled branch]".into(),
        None,
    )
    .await;
    w.until("lead turn completed", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;
    let checkout = t.agent("Pia").await.cwd;
    let response = t
        .post(
            &format!("/v1/runs/{}/cancel", first["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    assert_eq!(t.agent("Pia").await.cwd, checkout);
    let second = round(&t, &work, change("next.txt"), None).await;
    completed(&mut w, &second).await;
    assert_eq!(second["base_branch"], "main");
    assert_eq!(t.agent("Pia").await.cwd, checkout);
    assert!(!checkout.join("cancelled.txt").exists());
    let old = std::process::Command::new("git")
        .args([
            "show",
            &format!("{}:cancelled.txt", first["branch"].as_str().unwrap()),
        ])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(old.status.success());
    land(&t, &second).await;
    assert!(!repo.path().join("cancelled.txt").exists());
    assert!(detail(&t, &first).await["landed"].is_null());
}

#[tokio::test]
async fn native_lead_run_next_after_landing() {
    let repo = git_repo();
    let sessions = tempfile::tempdir().unwrap();
    let session_dir = sessions.path().to_str().unwrap().to_owned();
    let t = TestHub::start_with(common::native_plugin_with(&[
        "--mock-mobile-sessions",
        &session_dir,
    ]))
    .await;
    t.add_native("Pia", "planner", repo.path()).await;
    let response = t
        .post("/v1/works", json!({"title":"rounds", "lead":"Pia"}))
        .await;
    assert_eq!(response.status(), 201);
    let work = response.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut w = t.watch().await;
    let first = round(
        &t,
        &work,
        format!("[remember distinctive-amber-fox] {}", change("first.txt")),
        None,
    )
    .await;
    completed(&mut w, &first).await;
    let checkout = t.agent("Pia").await.cwd;
    land(&t, &first).await;
    assert!(repo.path().join("first.txt").exists());
    assert!(!checkout.exists(), "landing took the checkout away");
    assert_eq!(t.agent("Pia").await.cwd, repo.path());
    let before = t.agent("Pia").await;
    let session = t.hub.bus.store().last_session_id(before.id).await.unwrap();
    put_constraints(&t, before.id, "- Keep the public API.\n  Tests first.").await;
    t.send(
        "Pia",
        &{
            let next = call(
                "run_next",
                json!({"title":"second", "request":format!("[session] [cwd] [recall] {}", change("second.txt"))}),
            );
            format!("{next} {next}")
        },
    )
    .await;
    let event = w
        .until(
            "next round",
            |e| matches!(&e.kind, EventKind::RunCreated { run } if run.title == "second"),
        )
        .await;
    let EventKind::RunCreated { run } = event.kind else {
        unreachable!()
    };
    let second = serde_json::to_value(run).unwrap();
    assert_eq!(second["planner"], first["planner"]);
    assert!(w.output().contains("scheduled"), "{}", w.output());
    assert_eq!(kickoff_from(&t, "Pia", &first).await["kind"], "human");
    assert_eq!(second["work_id"], first["work_id"]);
    assert_eq!(
        second["base_branch"], "main",
        "a landed round starts from base"
    );
    completed(&mut w, &second).await;
    assert_eq!(
        w.output().matches("\"status\": \"scheduled\"").count(),
        2,
        "{}",
        w.output()
    );
    // The round is the same agent, in a new session; the old one stays in the event log.
    let renewed = t
        .hub
        .bus
        .store()
        .last_session_id(before.id)
        .await
        .unwrap()
        .expect("the round's session");
    assert_ne!(session.as_deref(), Some(renewed.as_str()));
    assert!(
        sessions_started(&t, before.id)
            .await
            .contains(session.as_ref().expect("the first session")),
        "the old session stays in the event log"
    );
    // The new session's own output: nothing it recalls. (The old session's turn that issued
    // run_next still recalls, so only what follows the new session's marker counts.)
    assert_session_recalls_nothing(&w, &renewed);
    // The scheduled round's kickoff went to the new session, and starting it is not a warning.
    assert!(
        output_from_session(&w, &renewed).contains("run: second (run_id"),
        "{}",
        w.output()
    );
    assert!(
        warnings_of(&t, before.id)
            .await
            .iter()
            .all(|m| !m.contains("没有带上之前的上下文")),
        "a round's new session is not a lost context"
    );
    let kickoff = kickoff_text(&t, "Pia", &second).await;
    for part in [
        "1. Goal (this round's request, as written):\n[session] [cwd] [recall]",
        "2. Constraints in force (team note `constraints`, verbatim):\n- Keep the public API.\n  Tests first.",
        "3. Previous round result: \"A round\" (run ",
        "4. Open items:",
    ] {
        assert!(kickoff.contains(part), "{kickoff}");
    }
    assert!(kickoff.contains("Added first.txt"), "{kickoff}");
    assert!(
        w.output().contains(&format!("cwd={}", checkout.display())),
        "{}",
        w.output()
    );
    let lead = t.agent(second["planner"].as_str().unwrap()).await;
    assert_eq!(lead.id, before.id);
    assert_eq!(lead.cwd, checkout);
    assert!(checkout.exists(), "the round made itself a new checkout");
    assert!(checkout.join("first.txt").exists());
    assert!(checkout.join("second.txt").exists());
    land(&t, &second).await;
    assert!(repo.path().join("second.txt").exists());
}

/// A round in the lead's own checkout (nothing moved): the lead is not moved, so the round is
/// the direct path, and its kickoff still opens a new session with the handoff.
#[tokio::test]
async fn native_round_in_the_same_checkout_opens_a_new_session() {
    let repo = git_repo();
    let sessions = tempfile::tempdir().unwrap();
    let session_dir = sessions.path().to_str().unwrap().to_owned();
    let t = TestHub::start_with(common::native_plugin_with(&[
        "--mock-mobile-sessions",
        &session_dir,
    ]))
    .await;
    t.add_native("Pia", "planner", repo.path()).await;
    let response = t
        .post("/v1/works", json!({"title":"rounds", "lead":"Pia"}))
        .await;
    assert_eq!(response.status(), 201);
    let work = response.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut w = t.watch().await;
    let first = round(
        &t,
        &work,
        format!("[remember distinctive-amber-fox] {}", change("first.txt")),
        None,
    )
    .await;
    completed(&mut w, &first).await;
    let lead = t.agent("Pia").await;
    let checkout = lead.cwd.clone();
    let old = t
        .hub
        .bus
        .store()
        .last_session_id(lead.id)
        .await
        .unwrap()
        .expect("the first round's session");
    // Not landed: the checkout stays with the lead.
    put_constraints(&t, lead.id, "- Keep the public API.").await;
    t.send(
        "Pia",
        &call(
            "run_next",
            json!({"title":"second", "request":format!("[session] [recall] {}", change("second.txt"))}),
        ),
    )
    .await;
    let event = w
        .until(
            "next round",
            |e| matches!(&e.kind, EventKind::RunCreated { run } if run.title == "second"),
        )
        .await;
    let EventKind::RunCreated { run } = event.kind else {
        unreachable!()
    };
    let second = serde_json::to_value(run).unwrap();
    completed(&mut w, &second).await;
    let after = t.agent("Pia").await;
    assert_eq!(after.id, lead.id, "the same agent");
    assert_eq!(after.cwd, checkout);
    assert_eq!(second["base_branch"], first["branch"]);
    let new = t
        .hub
        .bus
        .store()
        .last_session_id(lead.id)
        .await
        .unwrap()
        .expect("the second round's session");
    assert_ne!(new, old);
    assert!(
        sessions_started(&t, lead.id).await.contains(&old),
        "the old session stays in the event log"
    );
    assert_session_recalls_nothing(&w, &new);
    assert!(
        warnings_of(&t, lead.id)
            .await
            .iter()
            .all(|m| !m.contains("没有带上之前的上下文")),
        "a round's new session is not a lost context"
    );
    let kickoff = kickoff_text(&t, "Pia", &second).await;
    for part in [
        "1. Goal (this round's request, as written):\n[session] [recall]",
        "2. Constraints in force (team note `constraints`, verbatim):\n- Keep the public API.",
        "3. Previous round result: \"A round\" (run ",
        "Added first.txt",
        "changes not landed",
        "4. Open items:",
        "note_write",
    ] {
        assert!(kickoff.contains(part), "{kickoff}");
    }
    assert!(checkout.join("first.txt").exists());
    assert!(checkout.join("second.txt").exists());
}

#[tokio::test]
async fn native_scheduled_round_waits_for_the_person() {
    let repo = git_repo();
    let t = TestHub::start_with(common::native_plugin).await;
    t.add_native("Pia", "planner", repo.path()).await;
    let response = t
        .post("/v1/works", json!({"title":"rounds", "lead":"Pia"}))
        .await;
    let work = response.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut w = t.watch().await;
    let first = round(&t, &work, change("first.txt"), None).await;
    completed(&mut w, &first).await;
    land(&t, &first).await;
    t.send(
        "Pia",
        &format!(
            "[sleep 1500] {}",
            call(
                "run_next",
                json!({"title":"queued next", "request":change("next.txt")})
            )
        ),
    )
    .await;
    w.until(
        "sleeping",
        |e| matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains("sleeping")),
    )
    .await;
    let queued = t.send("Pia", "person was waiting").await;
    assert_eq!(queued["queued"], true);
    w.until(
        "next round",
        |e| matches!(&e.kind, EventKind::RunCreated { run } if run.title == "queued next"),
    )
    .await;
    assert!(w.output().contains("person was waiting"), "{}", w.output());
    assert!(w.output().contains("scheduled"), "{}", w.output());
}

#[tokio::test]
async fn native_scheduled_round_reports_creation_failure() {
    let repo = git_repo();
    let t = TestHub::start_with(common::native_plugin).await;
    t.add_native("Pia", "planner", repo.path()).await;
    let response = t
        .post("/v1/works", json!({"title":"rounds", "lead":"Pia"}))
        .await;
    let work = response.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut w = t.watch().await;
    let first = round(&t, &work, change("first.txt"), None).await;
    completed(&mut w, &first).await;
    let checkout = t.agent("Pia").await.cwd;
    land(&t, &first).await;
    t.send(
        "Pia",
        &format!(
            "[sleep 1500] {}",
            call(
                "run_next",
                json!({"title":"conflict", "request":"new work"})
            )
        ),
    )
    .await;
    w.until(
        "sleeping",
        |e| matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains("sleeping")),
    )
    .await;
    std::fs::create_dir_all(&checkout).unwrap();
    w.until("failure notice", |e| matches!(&e.kind, EventKind::AgentInput { from, text, .. } if *from == Some(crossagents_api_types::MessageSource::Notice) && text.contains("Scheduled next round:"))).await;
    assert!(w.output().contains("scheduled"), "{}", w.output());
}

#[tokio::test]
async fn native_reused_checkout_schedules_while_gui_reports_busy() {
    let repo = git_repo();
    let t = TestHub::start_with(common::native_plugin).await;
    let pia = t.add_native("Pia", "planner", repo.path()).await;
    let response = t
        .post("/v1/works", json!({"title":"rounds", "lead":"Pia"}))
        .await;
    let work = response.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut w = t.watch().await;
    let first = round(&t, &work, change("first.txt"), None).await;
    completed(&mut w, &first).await;
    let checkout = t.agent("Pia").await.cwd;
    t.hub
        .runtime
        .place(&crossagents_core::AgentKey::Id(pia.id), repo.path(), true)
        .await
        .unwrap();
    t.send(
        "Pia",
        &format!(
            "[sleep 1500] {}",
            call(
                "run_next",
                json!({"title":"reused", "request":change("second.txt")})
            )
        ),
    )
    .await;
    w.until(
        "lead sleeping",
        |e| matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains("sleeping")),
    )
    .await;
    let response = t
        .post(
            &format!("/v1/works/{work}/runs"),
            json!({"title":"gui", "request":"other"}),
        )
        .await;
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, 409, "{body}");
    assert!(body.to_lowercase().contains("busy"), "{body}");
    let event = w
        .until(
            "reused round",
            |e| matches!(&e.kind, EventKind::RunCreated { run } if run.title == "reused"),
        )
        .await;
    let EventKind::RunCreated { run } = event.kind else {
        unreachable!()
    };
    assert_eq!(run.planner, pia.id);
    assert_eq!(t.agent("Pia").await.cwd, checkout);
    assert!(w.output().contains("scheduled"), "{}", w.output());
}

/// A person's message that arrives while the lead's turn, the one calling run_next, still runs
/// waits in the lead's queue. It is handled once, never withdrawn, and comes before the round's
/// kickoff, whichever session takes it: the old one (it was queued before the switch) or the new.
#[tokio::test]
async fn a_persons_message_during_run_next_is_handled_once_before_the_kickoff() {
    let repo = git_repo();
    let sessions = tempfile::tempdir().unwrap();
    let session_dir = sessions.path().to_str().unwrap().to_owned();
    let t = TestHub::start_with(common::native_plugin_with(&[
        "--mock-mobile-sessions",
        &session_dir,
    ]))
    .await;
    t.add_native("Pia", "planner", repo.path()).await;
    let response = t
        .post("/v1/works", json!({"title":"rounds", "lead":"Pia"}))
        .await;
    assert_eq!(response.status(), 201);
    let work = response.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut w = t.watch().await;
    let first = round(&t, &work, change("first.txt"), None).await;
    completed(&mut w, &first).await;
    let lead = t.agent("Pia").await;
    let checkout = lead.cwd.clone();
    let old = t
        .hub
        .bus
        .store()
        .last_session_id(lead.id)
        .await
        .unwrap()
        .expect("the first round's session");

    // The turn that calls run_next keeps running for a while after the call.
    t.send(
        "Pia",
        &format!(
            "{} [sleep 1500]",
            call(
                "run_next",
                json!({"title":"second", "request":change("second.txt")}),
            )
        ),
    )
    .await;
    let event = w
        .until(
            "next round",
            |e| matches!(&e.kind, EventKind::RunCreated { run } if run.title == "second"),
        )
        .await;
    let EventKind::RunCreated { run } = event.kind else {
        unreachable!()
    };
    let second = serde_json::to_value(run).unwrap();
    // Sent while that turn runs: it waits behind the turn.
    t.send("Pia", "PERSON-WAITING-7f [sleep 10]").await;
    completed(&mut w, &second).await;

    let out = w.output();
    assert_eq!(out.matches("PERSON-WAITING-7f").count(), 1, "handled once: {out}");
    let person = out.find("PERSON-WAITING-7f").unwrap();
    let kickoff = out
        .find("run: second (run_id")
        .unwrap_or_else(|| panic!("no kickoff in {out}"));
    assert!(person < kickoff, "the person's message comes before the kickoff: {out}");
    let events = t
        .hub
        .bus
        .store()
        .agent_events(lead.id, 0, 10_000)
        .await
        .unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::AgentInputWithdrawn { .. })),
        "nothing waiting is withdrawn"
    );
    let new = t
        .hub
        .bus
        .store()
        .last_session_id(lead.id)
        .await
        .unwrap()
        .expect("the second round's session");
    assert_ne!(new, old);
    assert_eq!(t.agent("Pia").await.cwd, checkout);
}
