//! Successive rounds share the lead's checkout, but retain separate branches and records.
mod common;

use common::{TestHub, git_repo};
use crossagents_api_types::{EventKind, RunStatus};
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
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
    assert!(marker.exists());
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
        }
        let second = round(&t, &work, change("second.txt"), (!landed).then_some("base")).await;
        completed(&mut w, &second).await;
        assert_eq!(second["base_branch"], "main");
        assert_eq!(t.agent("Pia").await.cwd, checkout);
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
