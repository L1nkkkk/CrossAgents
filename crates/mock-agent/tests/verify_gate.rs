//! The verification gate (docs/TODO.md §九 ①): a task is `Passed` only once the hub committed
//! it, ran its checks and recorded the verified commit; a plain accept merges that commit and
//! nothing else; being verified blocks every other decision. `Passed` no longer means "checked
//! and approved" - it means "the report is ready, decide".
//!
//! The "being verified" window is made by a check that waits for a gate file the test writes:
//! a controllable slow verifier, not a sleep.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{TestHub, git_repo, types};
use crossagents_api_types::{Event, EventKind, RunStatus, TaskStatus};
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

/// A worker goal: `directives`, then report done with `summary` (which its issuer acts on).
fn goal(directives: &str, summary: &str) -> String {
    format!(
        "{directives} {}",
        call(
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": summary })
        )
    )
}

fn planned(key: &str, goal: String, acceptance: Option<Vec<String>>) -> Value {
    let mut spec = json!({ "goal": goal });
    if let Some(commands) = acceptance {
        spec["acceptance"] = json!(commands);
    }
    json!({ "key": key, "title": format!("task {key}"), "spec": spec })
}

fn request(tasks: Value) -> String {
    format!(
        "Build it. {}",
        call("plan_submit", json!({ "tasks": tasks }))
    )
}

async fn setup(repo: &Path) -> TestHub {
    let t = TestHub::start().await;
    t.add_agent_in("boss", Some("opus"), "planner", repo).await;
    t.add_agent_in("coder", Some("haiku"), "worker", repo).await;
    t
}

async fn start_run(t: &TestHub, req: String, extra: Value) -> String {
    let mut body = json!({ "request": req, "planner": "boss", "worker": "coder" });
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let resp = t.post("/v1/runs", body).await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let run: Value = resp.json().await.unwrap();
    run["id"].as_str().unwrap().to_owned()
}

async fn approve(t: &TestHub, id: &str) {
    let resp = t.post(&format!("/v1/runs/{id}/approve"), json!({})).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
}

async fn detail(t: &TestHub, id: &str) -> Value {
    t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap()
}

fn task_in<'a>(detail: &'a Value, key: &str) -> &'a Value {
    detail["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == key)
        .unwrap_or_else(|| panic!("no task {key} in {detail}"))
}

fn run_status_is(id: &str, want: RunStatus) -> impl Fn(&Event) -> bool {
    let id = id.to_owned();
    move |e| {
        matches!(&e.kind, EventKind::RunStatusChanged { id: rid, to, .. }
            if rid.to_string() == id && *to == want)
    }
}

fn task_is(key: &'static str, want: TaskStatus) -> impl Fn(&Event) -> bool {
    move |e| matches!(&e.kind, EventKind::TaskStatusChanged { key: k, to, .. } if k == key && *to == want)
}

fn said_in(what: &'static str) -> impl Fn(&Event) -> bool {
    move |e| matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains(what))
}

/// `git` in `dir`; `false` when it failed (nothing to show, say).
fn git_ok(dir: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A check that passes only once `gate` exists: the verification window, opened by the test.
fn wait_for(gate: &Path) -> String {
    let gate = gate.display().to_string().replace('\\', "/");
    if cfg!(windows) {
        format!(
            "powershell -NoProfile -Command \"while (-not (Test-Path '{gate}')) {{ Start-Sleep -Milliseconds 50 }}\""
        )
    } else {
        format!("while [ ! -f '{gate}' ]; do sleep 0.05; done")
    }
}

#[tokio::test]
async fn nothing_is_merged_while_the_checks_run_and_only_the_verified_commit_merges() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let gate_dir = tempfile::tempdir().unwrap();
    let go = gate_dir.path().join("go");

    let a = planned(
        "a",
        goal(
            "[write a.txt=alpha]",
            &format!("wrote a {}", call("task_accept", json!({ "task": "a" }))),
        ),
        Some(vec![wait_for(&go)]),
    );
    let id = start_run(&t, request(json!([a])), json!({})).await;
    w.until(
        "awaiting approval",
        run_status_is(&id, RunStatus::AwaitingApproval),
    )
    .await;
    approve(&t, &id).await;
    w.until("verifying", task_is("a", TaskStatus::Verifying))
        .await;

    let d = detail(&t, &id).await;
    let worktree = PathBuf::from(task_in(&d, "a")["worktree"].as_str().unwrap());
    let branch = d["run"]["branch"].as_str().unwrap().to_owned();
    assert!(worktree.exists(), "the checks run in it");
    assert!(
        !git_ok(repo.path(), &["show", &format!("{branch}:a.txt")]),
        "nothing is merged before the checks end"
    );

    // Four ways to take it while the checks run: all refused, force or not.
    t.send(
        "boss",
        &format!(
            "{} {} {} {}",
            call("task_accept", json!({ "task": "a" })),
            call(
                "task_accept",
                json!({ "task": "a", "force": true, "reason": "in a hurry" })
            ),
            call("task_drop", json!({ "task": "a" })),
            call("task_cancel", json!({ "task": "a" })),
        ),
    )
    .await;
    w.until("the refusals", said_in("then cancel it")).await;

    let d = detail(&t, &id).await;
    assert_eq!(task_in(&d, "a")["status"], "verifying");
    assert!(
        !git_ok(repo.path(), &["show", &format!("{branch}:a.txt")]),
        "nothing merged: {:?}",
        types(&w.seen)
    );
    assert!(
        worktree.exists(),
        "the checkout an accept would have deleted"
    );
    let said = w.output();
    assert!(said.contains("still being verified"), "{said}");

    // Deleting the run is refused as well.
    let resp = t.delete(&format!("/v1/runs/{id}")).await;
    assert_eq!(resp.status(), 409, "{}", resp.text().await.unwrap());

    // Let the check through. The task becomes passed with its checks, and only then does the
    // issuer hear the worker's summary - whose accept merges the commit the checks saw.
    std::fs::write(&go, "go").unwrap();
    w.until("merged", task_is("a", TaskStatus::Merged)).await;
    let d = detail(&t, &id).await;
    let task = task_in(&d, "a");
    assert_eq!(task["report"]["checks"][0]["passed"], true, "{task}");
    assert_eq!(task["report"]["unchecked"], false);
    let sha = task["report"]["verified_sha"]
        .as_str()
        .expect("a verified commit");
    assert!(
        git_ok(repo.path(), &["merge-base", "--is-ancestor", sha, &branch]),
        "the run branch has the verified commit {sha}, not something newer"
    );
    assert_eq!(
        git(repo.path(), &["show", &format!("{branch}:a.txt")]),
        "alpha"
    );
}

#[tokio::test]
async fn a_failed_check_needs_force_and_a_reason() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let a = planned(
        "a",
        // Writes "alpha" while the check looks for "gamma": the check fails.
        goal(
            "[write a.txt=alpha]",
            &format!("wrote a {}", call("task_accept", json!({ "task": "a" }))),
        ),
        Some(vec!["git grep -q gamma -- a.txt".into()]),
    );
    let id = start_run(&t, request(json!([a])), json!({})).await;
    w.until(
        "awaiting approval",
        run_status_is(&id, RunStatus::AwaitingApproval),
    )
    .await;
    approve(&t, &id).await;
    w.until(
        "passed with a failed check",
        task_is("a", TaskStatus::Passed),
    )
    .await;
    w.until(
        "the issuer's plain accept",
        said_in("its checks did not pass"),
    )
    .await;
    assert_eq!(detail(&t, &id).await["tasks"][0]["status"], "passed");

    // Forced, with a reason, it goes through and the reason stays on the record.
    t.send(
        "boss",
        &call(
            "task_accept",
            json!({ "task": "a", "force": true, "reason": "the flaky check is unrelated" }),
        ),
    )
    .await;
    w.until("merged", task_is("a", TaskStatus::Merged)).await;
    let d = detail(&t, &id).await;
    assert_eq!(
        task_in(&d, "a")["report"]["forced"],
        "the flaky check is unrelated"
    );
}

#[tokio::test]
async fn a_task_with_no_checks_needs_force_and_a_reason() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let b = planned(
        "b",
        goal(
            "[write b.txt=beta]",
            &format!("wrote b {}", call("task_accept", json!({ "task": "b" }))),
        ),
        None,
    );
    let id = start_run(&t, request(json!([b])), json!({})).await;
    w.until(
        "awaiting approval",
        run_status_is(&id, RunStatus::AwaitingApproval),
    )
    .await;
    approve(&t, &id).await;
    w.until("passed", task_is("b", TaskStatus::Passed)).await;
    w.until("the issuer's plain accept", said_in("it has no checks"))
        .await;
    let d = detail(&t, &id).await;
    assert_eq!(task_in(&d, "b")["status"], "passed");
    assert_eq!(task_in(&d, "b")["report"]["unchecked"], true);

    t.send(
        "boss",
        &call(
            "task_accept",
            json!({ "task": "b", "force": true, "reason": "docs only" }),
        ),
    )
    .await;
    w.until("merged", task_is("b", TaskStatus::Merged)).await;
    let d = detail(&t, &id).await;
    assert_eq!(task_in(&d, "b")["report"]["forced"], "docs only");
}

#[tokio::test]
async fn work_done_after_the_checks_is_not_merged_plainly() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let a = planned(
        "a",
        goal("[write a.txt=alpha]", "wrote a"),
        Some(vec!["git grep -q alpha -- a.txt".into()]),
    );
    let id = start_run(&t, request(json!([a])), json!({})).await;
    w.until(
        "awaiting approval",
        run_status_is(&id, RunStatus::AwaitingApproval),
    )
    .await;
    approve(&t, &id).await;
    w.until("passed", task_is("a", TaskStatus::Passed)).await;
    let d = detail(&t, &id).await;
    let worktree = PathBuf::from(task_in(&d, "a")["worktree"].as_str().unwrap());
    let sha = task_in(&d, "a")["report"]["verified_sha"]
        .as_str()
        .expect("verified")
        .to_owned();
    // The worker (or anyone in its checkout) goes on after the checks.
    std::fs::write(worktree.join("a.txt"), "beta\n").unwrap();
    git(&worktree, &["add", "-A"]);
    git(
        &worktree,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "after the checks",
        ],
    );

    // A plain accept now would merge work nobody checked.
    t.send("boss", &call("task_accept", json!({ "task": "a" })))
        .await;
    w.until("the second refusal", said_in("it changed after its checks"))
        .await;
    assert_eq!(detail(&t, &id).await["tasks"][0]["status"], "passed");

    // Forced, it merges what is there now; what the checks saw stays on record.
    t.send(
        "boss",
        &call(
            "task_accept",
            json!({ "task": "a", "force": true, "reason": "the edit is a typo fix" }),
        ),
    )
    .await;
    w.until("merged", task_is("a", TaskStatus::Merged)).await;
    let d = detail(&t, &id).await;
    let branch = d["run"]["branch"].as_str().unwrap().to_owned();
    assert_eq!(
        git(repo.path(), &["show", &format!("{branch}:a.txt")]),
        "beta"
    );
    assert_eq!(task_in(&d, "a")["report"]["verified_sha"], sha.as_str());
    assert_eq!(
        task_in(&d, "a")["report"]["forced"],
        "the edit is a typo fix"
    );
}

#[tokio::test]
async fn a_report_for_a_round_that_ended_is_not_written_back() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let gate_dir = tempfile::tempdir().unwrap();
    let go = gate_dir.path().join("go");
    let a = planned(
        "a",
        goal("[write a.txt=alpha]", "wrote a"),
        Some(vec![wait_for(&go)]),
    );
    let id = start_run(&t, request(json!([a])), json!({})).await;
    w.until(
        "awaiting approval",
        run_status_is(&id, RunStatus::AwaitingApproval),
    )
    .await;
    approve(&t, &id).await;
    w.until("verifying", task_is("a", TaskStatus::Verifying))
        .await;

    // Nothing ends a round under a running verification - not even the person: both would pull
    // the checkout out from under the checks (docs/TODO.md §九 ①).
    let resp = t.post(&format!("/v1/runs/{id}/cancel"), json!({})).await;
    assert_eq!(resp.status(), 409);
    let said = resp.text().await.unwrap();
    assert!(said.contains("still being verified"), "{said}");

    // A restart ends it instead: the next hub cannot know how the checks went, so it marks the
    // report interrupted and leaves it as passed. The round the old hub started is gone with
    // its verification id, and what it writes when its check finally returns must not land.
    let home = t.restart_home().await;
    let t = TestHub::start_in(home, |_| {}).await;
    let w = t.watch().await;
    let d = detail(&t, &id).await;
    assert_eq!(task_in(&d, "a")["status"], "passed", "{d}");
    assert_eq!(task_in(&d, "a")["report"]["interrupted"], true, "{d}");

    std::fs::write(&go, "go").unwrap();
    // Nothing says the report is in: wait for the check to finish and the write-back to be
    // turned away, then check that it left no trace.
    tokio::time::sleep(Duration::from_secs(3)).await;

    let d = detail(&t, &id).await;
    let task = task_in(&d, "a");
    assert_eq!(task["status"], "passed", "{task}");
    assert_eq!(task["report"]["interrupted"], true, "{task}");
    assert!(
        task["report"]["checks"].as_array().unwrap().is_empty(),
        "a stale verification wrote its checks back: {task}"
    );
    assert!(task["report"]["verified_sha"].is_null(), "{task}");
    assert!(
        !w.seen
            .iter()
            .any(|e| matches!(&e.kind, EventKind::TaskChecked { key, .. } if key == "a")),
        "{:?}",
        types(&w.seen)
    );
}

#[tokio::test]
async fn a_hub_that_restarts_marks_what_it_was_verifying_as_interrupted() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let gate_dir = tempfile::tempdir().unwrap();
    let go = gate_dir.path().join("go");
    let a = planned(
        "a",
        goal("[write a.txt=alpha]", "wrote a"),
        Some(vec![wait_for(&go)]),
    );
    let id = start_run(&t, request(json!([a])), json!({})).await;
    w.until(
        "awaiting approval",
        run_status_is(&id, RunStatus::AwaitingApproval),
    )
    .await;
    approve(&t, &id).await;
    w.until("verifying", task_is("a", TaskStatus::Verifying))
        .await;

    // The hub goes down while the check runs; the next one starts on the same home.
    let home = t.restart_home().await;
    let t = TestHub::start_in(home, |_| {}).await;
    let mut w = t.watch().await;
    let d = detail(&t, &id).await;
    let task = task_in(&d, "a");
    assert_eq!(task["status"], "passed", "not left verifying: {task}");
    assert_eq!(task["report"]["interrupted"], true, "{task}");
    assert!(task["report"]["verified_sha"].is_null(), "{task}");

    // Not stuck: its issuer may accept it with a reason (here), verify it again, or drop it.
    t.send(
        "boss",
        &call(
            "task_accept",
            json!({ "task": "a", "force": true, "reason": "the checks were cut short" }),
        ),
    )
    .await;
    w.until("merged", task_is("a", TaskStatus::Merged)).await;
    let d = detail(&t, &id).await;
    assert_eq!(
        task_in(&d, "a")["report"]["forced"],
        "the checks were cut short"
    );
}
