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

/// Waits until `name` works in `dir` again (a lead the run let go of goes home once its turn
/// is over).
async fn until_home(t: &TestHub, name: &str, dir: &Path) {
    for _ in 0..100 {
        let cwd = agent(t, name).await["cwd"].as_str().unwrap().to_owned();
        if std::fs::canonicalize(&cwd).ok() == std::fs::canonicalize(dir).ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("{name} never went back to {}", dir.display());
}

/// Gives boss a conversation first. Its driver (ACP) cannot resume a session elsewhere, so a
/// run gets a copy of it, which keeps living in the run's checkout after the run ends.
async fn chat_first(t: &TestHub, w: &mut common::Watcher) {
    let boss = agent(t, "boss").await["id"].as_str().unwrap().to_owned();
    t.send("boss", "hello").await;
    w.until(
        "boss answered",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if id.to_string() == boss),
    )
    .await;
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

    // boss led it itself, in the run's checkout rather than the person's (the change is on
    // the run branch only); the run let go of it, so it is back in the repository.
    assert_eq!(run["planner"], agent(&t, "boss").await["id"], "{run}");
    until_home(&t, "boss", repo.path()).await;
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
async fn the_projects_checks_hold_back_run_finish() {
    let repo = git_repo();
    std::fs::write(
        repo.path().join("crossagents.toml"),
        "[run]\nchecks = [\"git ls-files --error-unmatch ok.txt\"]\n",
    )
    .unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    };
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "checks"]);
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // The first close is refused (no ok.txt yet); after the fix the second goes through.
    let finish = call(
        "run_finish",
        json!({ "summary": "added notes", "message": "docs: add notes" }),
    );
    let request = format!("Add notes. [write notes.txt=x] {finish} [write ok.txt=yes] {finish}");
    let id = start(&t, request).await["id"].as_str().unwrap().to_owned();
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;
    w.until("turn completed", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;
    let output = w.output();
    // The lead was told up front, and the refusal carried the failing command.
    assert!(output.contains("checks (crossagents.toml)"), "{output}");
    assert!(
        output.contains("checks (crossagents.toml) failed"),
        "{output}"
    );
    assert!(output.contains("ok.txt"), "{output}");
    let run: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    assert_eq!(run["run"]["checks"][0]["passed"], true, "{run}");
    let human = t.get("/v1/questions").await.text().await.unwrap();
    assert!(
        human.contains("验收（crossagents.toml）：1/1 通过"),
        "{human}"
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
    chat_first(&t, &mut w).await;

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

    // The lead is a copy: archived when the run ended (S1 §3.4), keeping the checkout's path
    // as its directory, while the checkout itself went.
    let lead = agent(&t, landed["planner"].as_str().unwrap()).await;
    assert!(!lead["archived_at"].is_null(), "{lead}");
    let dir = std::path::PathBuf::from(lead["cwd"].as_str().unwrap());
    let branch = landed["branch"].as_str().unwrap().to_owned();
    assert!(!dir.exists());

    let archive_path = format!("/v1/agents/{}/archive", lead["id"].as_str().unwrap());
    let archive = |on: bool| t.post(&archive_path, json!({ "archived": on }));
    // Restored, it gets its checkout of the run branch back.
    assert_eq!(archive(false).await.status(), 200);
    assert!(dir.join("one.txt").exists());
    // What a cargo build there left in the checkout's own build directory
    // (`<run>/targets/integration`, docs/TODO.md §九 ②).
    let target = dir.parent().unwrap().join("targets").join("integration");
    std::fs::create_dir_all(target.join("debug")).unwrap();

    assert_eq!(archive(true).await.status(), 200);
    // Landed: nothing waits on the branch, so it goes with the checkout.
    assert!(!dir.exists());
    assert!(!target.exists(), "the run's build directory goes too");
    assert!(
        !branches(repo.path()).contains(&branch),
        "{}",
        branches(repo.path())
    );

    let from = w.seen.len();
    assert_eq!(archive(false).await.status(), 200);
    assert!(dir.join("one.txt").exists(), "restored from the base");
    // Landed or not, the person is told the run's code was not checked out again: a landing
    // does not prove the base still has it.
    let lead_id = lead["id"].as_str().unwrap().to_owned();
    w.until("told the code was rebuilt from the base", |e| {
        matches!(&e.kind, EventKind::AgentWarning { id, message }
            if id.to_string() == lead_id && message.contains("原执行代码没有恢复") && message.contains("没有核对"))
    })
    .await;
    let rebuilt_once = w.seen[from..]
        .iter()
        .filter(|e| matches!(&e.kind, EventKind::AgentWarning { message, .. } if message.contains("原执行代码没有恢复")))
        .count();
    assert_eq!(rebuilt_once, 1);
}

#[tokio::test]
async fn an_unlanded_branch_outlives_the_checkout() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    chat_first(&t, &mut w).await;

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

/// S1 §7: restoring a copied lead whose unlanded run branch was deleted gets it a checkout
/// from the base: the conversation is back, the run's code is not, and the person is told.
#[tokio::test]
async fn restoring_a_lead_whose_branch_is_gone_says_the_code_did_not_come_back() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    chat_first(&t, &mut w).await;
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
    assert!(!lead["archived_at"].is_null(), "{lead}");
    let dir = std::path::PathBuf::from(lead["cwd"].as_str().unwrap());
    let branch = run["branch"].as_str().unwrap().to_owned();
    let out = std::process::Command::new("git")
        .args(["branch", "-D", &branch])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let lead_id = lead["id"].as_str().unwrap().to_owned();
    let resp = t
        .post(
            &format!("/v1/agents/{lead_id}/archive"),
            json!({ "archived": false }),
        )
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    w.until("told the code did not come back", |e| {
        matches!(&e.kind, EventKind::AgentWarning { id, message }
            if id.to_string() == lead_id && message.contains("原执行代码没有恢复") && message.contains(&branch))
    })
    .await;
    assert!(dir.is_dir(), "{dir:?}");
    assert!(!dir.join("two.txt").exists(), "rebuilt from the base");
}
