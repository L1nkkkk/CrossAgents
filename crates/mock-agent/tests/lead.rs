//! A Work's lead does the change itself (docs/tasks/W1.md §4): it works in the run's own
//! checkout, closes with `run_finish`, and the run branch lands without any task.

mod common;

use std::path::Path;

use common::{TestHub, git_repo};
use crossagents_api_types::{EventKind, RunStatus};
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

fn run_status_is(id: &str, want: RunStatus) -> impl Fn(&crossagents_api_types::Event) -> bool {
    let id = id.to_owned();
    move |e| {
        matches!(&e.kind, EventKind::RunStatusChanged { id: rid, to, .. }
            if rid.to_string() == id && *to == want)
    }
}

/// Starts a run led by `boss` with no worker given; returns the run.
async fn start(t: &TestHub, request: String) -> Value {
    let resp = t
        .post("/v1/runs", json!({ "request": request, "planner": "boss" }))
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    resp.json().await.unwrap()
}

async fn agent(t: &TestHub, id: &str) -> Value {
    t.get(&format!("/v1/agents/{id}"))
        .await
        .json()
        .await
        .unwrap()
}

async fn setup(repo: &Path) -> TestHub {
    let t = TestHub::start().await;
    t.add_agent_in("boss", Some("opus"), "planner", repo).await;
    t
}

#[tokio::test]
async fn the_lead_changes_the_run_branch_and_it_lands() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    let request = format!(
        "Add notes. [write notes.txt=from the lead] {}",
        call(
            "run_finish",
            json!({ "summary": "added notes", "message": "docs: add notes" })
        )
    );
    let run = start(&t, request).await;
    let id = run["id"].as_str().unwrap().to_owned();
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;

    // The lead worked in the run's checkout, not in the person's.
    let lead = agent(&t, run["planner"].as_str().unwrap()).await;
    let cwd = lead["cwd"].as_str().unwrap();
    assert!(cwd.contains("integration"), "{lead}");
    // With no worker given, tasks would go to copies of the lead: no teammates listed.
    assert_eq!(run["config"]["team"], json!([]), "{run}");
    assert!(!repo.path().join("notes.txt").exists());

    let open = || async { t.get("/v1/questions").await.text().await.unwrap() };
    assert!(open().await.contains("等你落地"));
    let resp = t
        .post(&format!("/v1/runs/{id}/land"), json!({ "mode": "merge" }))
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    // Landed: the note asking for it is cleared.
    assert!(!open().await.contains("等你落地"));
    assert_eq!(
        std::fs::read_to_string(repo.path().join("notes.txt")).unwrap(),
        "from the lead"
    );
    let log = std::process::Command::new("git")
        .args(["log", "--format=%s", "-3"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&log.stdout).contains("docs: add notes"),
        "{}",
        String::from_utf8_lossy(&log.stdout)
    );
}

#[tokio::test]
async fn a_run_that_only_answered_has_nothing_to_land() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    let request = format!(
        "Look around. {}",
        call("run_finish", json!({ "summary": "it is a small repo" }))
    );
    let id = start(&t, request).await["id"].as_str().unwrap().to_owned();
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;

    let resp = t
        .post(&format!("/v1/runs/{id}/land"), json!({ "mode": "merge" }))
        .await;
    assert_eq!(resp.status(), 409, "{}", resp.text().await.unwrap());
    let human: Value = t.get("/v1/questions").await.json().await.unwrap();
    assert!(human.to_string().contains("没有改动"), "{human}");
}

#[tokio::test]
async fn a_lead_with_a_session_leads_through_a_copy() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // The first run moves the fresh boss into its checkout and starts its session.
    let first = start(
        &t,
        format!("One. {}", call("run_finish", json!({ "summary": "ok" }))),
    )
    .await;
    let first_id = first["id"].as_str().unwrap().to_owned();
    assert_eq!(first["planner"], agent(&t, "boss").await["id"]);
    w.until(
        "first completed",
        run_status_is(&first_id, RunStatus::Completed),
    )
    .await;

    // boss has a session now: the second run gets a copy in its own checkout.
    let second = start(
        &t,
        format!("Two. {}", call("run_finish", json!({ "summary": "ok" }))),
    )
    .await;
    let boss = agent(&t, "boss").await;
    assert_ne!(second["planner"], boss["id"]);
    let lead = agent(&t, second["planner"].as_str().unwrap()).await;
    assert_eq!(lead["role"], "planner");
    assert_ne!(lead["cwd"], boss["cwd"], "{lead}");
    assert_eq!(lead["model"], "opus");
}

fn branches(repo: &Path) -> String {
    let out = std::process::Command::new("git")
        .args(["branch", "--list"])
        .current_dir(repo)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[tokio::test]
async fn archiving_the_lead_clears_its_checkout_and_restoring_brings_it_back() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // Run 1 lands; run 2 changes something but is never landed.
    let landed = start(
        &t,
        format!(
            "One. [write one.txt=1] {}",
            call("run_finish", json!({ "summary": "ok" }))
        ),
    )
    .await;
    let id = landed["id"].as_str().unwrap().to_owned();
    w.until(
        "landed run completed",
        run_status_is(&id, RunStatus::Completed),
    )
    .await;
    let resp = t
        .post(&format!("/v1/runs/{id}/land"), json!({ "mode": "merge" }))
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());

    let lead = agent(&t, landed["planner"].as_str().unwrap()).await;
    let dir = std::path::PathBuf::from(lead["cwd"].as_str().unwrap());
    let branch = landed["branch"].as_str().unwrap().to_owned();
    assert!(dir.exists());
    // What a cargo build there left in the run's shared build directory.
    let target = dir.parent().unwrap().join("target");
    std::fs::create_dir_all(target.join("debug")).unwrap();

    let archive_path = format!("/v1/agents/{}/archive", lead["id"].as_str().unwrap());
    let archive = |on: bool| t.post(&archive_path, json!({ "archived": on }));
    assert_eq!(archive(true).await.status(), 200);
    // Landed: nothing waits on the branch, so it goes with the checkout.
    assert!(!dir.exists());
    assert!(!target.exists(), "the run's build directory goes too");
    assert!(
        !branches(repo.path()).contains(&branch),
        "{}",
        branches(repo.path())
    );

    assert_eq!(archive(false).await.status(), 200);
    assert!(dir.join("one.txt").exists(), "restored from the base");
}

#[tokio::test]
async fn an_unlanded_branch_outlives_the_checkout() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    let run = start(
        &t,
        format!(
            "Two. [write two.txt=2] {}",
            call("run_finish", json!({ "summary": "ok" }))
        ),
    )
    .await;
    let id = run["id"].as_str().unwrap().to_owned();
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;
    let lead = agent(&t, run["planner"].as_str().unwrap()).await;
    let dir = std::path::PathBuf::from(lead["cwd"].as_str().unwrap());
    let branch = run["branch"].as_str().unwrap().to_owned();

    let resp = t
        .delete(&format!("/v1/agents/{}", lead["id"].as_str().unwrap()))
        .await;
    assert_eq!(resp.status(), 204);
    assert!(!dir.exists());
    // Still waiting to be landed.
    assert!(branches(repo.path()).contains(&branch));
}
