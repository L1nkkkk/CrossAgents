//! Landing after the base branch moved (docs/tasks/DOGFOOD.md §4): what main gained is
//! merged into the run branch first; a conflict becomes a task for the run's worker instead
//! of a failed landing.

mod common;

use std::path::Path;

use common::{TestHub, git_repo};
use crossagents_api_types::{EventKind, TaskStatus};
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

fn task_is(key: &'static str, want: TaskStatus) -> impl Fn(&crossagents_api_types::Event) -> bool {
    move |e| matches!(&e.kind, EventKind::TaskStatusChanged { key: k, to, .. } if k == key && *to == want)
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// The person commits `file` = `text` on main, in their own checkout.
fn commit_on_main(repo: &Path, file: &str, text: &str) {
    std::fs::write(repo.join(file), text).unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "meanwhile on main"]);
}

/// A run whose one task writes a.txt and is accepted by the lead; returns the run id once
/// the task is merged (landing is left to the person).
async fn merged_run(t: &TestHub, w: &mut common::Watcher) -> String {
    let goal = format!(
        "[write a.txt=from the run] {}",
        call(
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": format!("done {}", call("task_accept", json!({ "task": "a" }))) })
        )
    );
    let plan = json!([{ "key": "a", "title": "task a", "spec": { "goal": goal } }]);
    let req = format!(
        "Build it. {}",
        call("plan_submit", json!({ "tasks": plan }))
    );
    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": req, "planner": "boss", "worker": "coder", "policy": { "plan_approval": "auto" } }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let run: Value = resp.json().await.unwrap();
    w.until("a merged", task_is("a", TaskStatus::Merged)).await;
    run["id"].as_str().unwrap().to_owned()
}

async fn setup(repo: &Path) -> TestHub {
    let t = TestHub::start().await;
    t.add_agent_in("boss", Some("opus"), "planner", repo).await;
    t.add_agent_in("coder", Some("haiku"), "worker", repo).await;
    t
}

#[tokio::test]
async fn main_that_moved_cleanly_is_merged_in_before_landing() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let id = merged_run(&t, &mut w).await;

    commit_on_main(repo.path(), "b.txt", "from main");
    let resp = t
        .post(&format!("/v1/runs/{id}/land"), json!({ "mode": "merge" }))
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let detail: Value = resp.json().await.unwrap();
    assert_eq!(detail["run"]["landed"]["by"], "person", "{detail}");
    // Both sides are on main.
    assert_eq!(
        std::fs::read_to_string(repo.path().join("a.txt")).unwrap(),
        "from the run"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("b.txt")).unwrap(),
        "from main"
    );
}

#[tokio::test]
async fn a_conflict_with_main_becomes_a_task_and_lands_after_it() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let id = merged_run(&t, &mut w).await;

    commit_on_main(repo.path(), "a.txt", "from main");
    let resp = t
        .post(&format!("/v1/runs/{id}/land"), json!({ "mode": "merge" }))
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let detail: Value = resp.json().await.unwrap();
    assert!(detail["run"]["landed"].is_null(), "{detail}");
    let note = detail["run"]["note"].as_str().unwrap();
    assert!(
        note.contains("#sync-main") && note.contains("a.txt"),
        "{note}"
    );
    // Nothing reached main yet.
    assert_eq!(
        std::fs::read_to_string(repo.path().join("a.txt")).unwrap(),
        "from main"
    );

    w.until("sync task runs", task_is("sync-main", TaskStatus::Running))
        .await;
    let detail: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    let sync = detail["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == "sync-main")
        .unwrap()
        .clone();
    assert_eq!(sync["issuer"], detail["run"]["planner"]);
    assert!(
        sync["spec"]["goal"]
            .as_str()
            .unwrap()
            .contains("git merge main")
    );
    // Landing waits for it.
    let resp = t.post(&format!("/v1/runs/{id}/land"), json!({})).await;
    assert_eq!(resp.status(), 409);

    // The worker resolves it in the task's worktree (done here by hand), then reports; the
    // lead accepts.
    let worktree = Path::new(sync["worktree"].as_str().unwrap());
    git(worktree, &["merge", "-q", "main"]);
    std::fs::write(worktree.join("a.txt"), "from both").unwrap();
    git(worktree, &["add", "-A"]);
    git(worktree, &["commit", "-q", "-m", "resolve"]);
    let report = call(
        "task_complete",
        json!({ "task_id": sync["id"], "summary": format!("resolved {}", call("task_accept", json!({ "task": "sync-main" }))) }),
    );
    let worker = sync["assignee"].as_str().unwrap();
    let resp = t
        .post(
            &format!("/v1/agents/{worker}/messages"),
            json!({ "text": report }),
        )
        .await;
    assert_eq!(resp.status(), 202);
    w.until("sync merged", task_is("sync-main", TaskStatus::Merged))
        .await;
    // Resolved: the run no longer says it waits for #sync-main.
    let before: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    assert!(before["run"]["note"].is_null(), "{before}");

    let resp = t
        .post(&format!("/v1/runs/{id}/land"), json!({ "mode": "merge" }))
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let detail: Value = resp.json().await.unwrap();
    assert_eq!(detail["run"]["landed"]["by"], "person", "{detail}");
    assert!(detail["run"]["note"].is_null(), "{detail}");
    assert_eq!(
        std::fs::read_to_string(repo.path().join("a.txt")).unwrap(),
        "from both"
    );
}
