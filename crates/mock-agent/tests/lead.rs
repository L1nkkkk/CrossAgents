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

/// Starts a run whose lead works in the person's own directory (docs/tasks/LEAD.md §2).
async fn start_in_place(t: &TestHub, planner: &str, request: String) -> Value {
    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": request, "planner": planner, "lead_place": "in_place" }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    resp.json().await.unwrap()
}

/// What `git <args>` in `dir` wrote, trimmed.
fn git_out(dir: &Path, args: &[&str]) -> String {
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

/// Commits `files` in `dir`, as the person would.
fn commit(dir: &Path, files: &[&str], message: &str) {
    let mut add = vec!["add", "--"];
    add.extend(files);
    git_out(dir, &add);
    git_out(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
}

async fn detail(t: &TestHub, id: &str) -> Value {
    t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap()
}

async fn agent(t: &TestHub, id: &str) -> Value {
    t.get(&format!("/v1/agents/{id}"))
        .await
        .json()
        .await
        .unwrap()
}

/// Finishing retains the lead and tree; explicit archival removes the managed checkout.
async fn archive_and_wait(t: &TestHub, id: &str, dir: &Path) {
    assert!(agent(t, id).await["archived_at"].is_null());
    assert!(dir.exists(), "finish retains the checkout");
    let resp = t
        .post(
            &format!("/v1/agents/{id}/archive"),
            json!({ "archived": true }),
        )
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let deadline = tokio::time::Instant::now() + common::wait_limit();
    loop {
        let a = agent(t, id).await;
        if !a["archived_at"].is_null() && !dir.exists() {
            return;
        }
        if tokio::time::Instant::now() > deadline {
            panic!(
                "{id} never left {} (archived: {}, cwd: {}, there: {})",
                dir.display(),
                a["archived_at"],
                a["cwd"],
                dir.exists()
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Waits for a restored lead's checkout to be made again and to hold `file`: restoring
/// publishes the agent, and the checkout is built from that event.
async fn until_back(repo: &Path, dir: &Path, file: &str) {
    let deadline = tokio::time::Instant::now() + common::wait_limit();
    loop {
        if dir.join(file).exists() {
            return;
        }
        if tokio::time::Instant::now() > deadline {
            panic!(
                "{} never came back under {} (repo {})",
                file,
                dir.display(),
                repo.display()
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Waits for a directory to be removed (archiving or deleting a lead removes what the run
/// left, from the event that announces it).
async fn until_gone(dir: &Path) {
    let deadline = tokio::time::Instant::now() + common::wait_limit();
    loop {
        if !dir.exists() {
            return;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("{} is still there", dir.display());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
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
    // the run branch only); finishing keeps it there.
    assert_eq!(run["planner"], agent(&t, "boss").await["id"], "{run}");
    let retained = agent(&t, "boss").await;
    assert!(retained["archived_at"].is_null());
    let checkout = std::path::PathBuf::from(retained["cwd"].as_str().unwrap());
    assert!(checkout.join("notes.txt").exists());
    assert_ne!(checkout, repo.path());
    // With no worker given, tasks would go to copies of the lead: no teammates listed.
    assert_eq!(run["config"]["team"], json!([]), "{run}");
    assert!(!repo.path().join("notes.txt").exists());

    // The project declares no checks: the lead is told so up front, and the run says it was
    // not verified rather than passing.
    w.until("turn completed", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;
    let output = w.output();
    assert!(
        output.contains("checks: none declared in crossagents.toml"),
        "{output}"
    );
    assert!(
        output.contains("not configured in crossagents.toml"),
        "{output}"
    );
    let stored: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    assert_eq!(
        stored["run"]["checks_state"]["state"], "not_configured",
        "{stored}"
    );
    let open = || async { t.get("/v1/questions").await.text().await.unwrap() };
    assert!(open().await.contains("未执行验证"));
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
    // A Rust project, as far as the presets go.
    std::fs::write(repo.path().join("Cargo.toml"), "").unwrap();
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
    assert!(output.contains("build env: CARGO_TARGET_DIR="), "{output}");
    assert!(output.contains("(preset rust)"), "{output}");
    assert!(
        output.contains("checks (crossagents.toml) failed"),
        "{output}"
    );
    assert!(output.contains("ok.txt"), "{output}");
    let run: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    assert_eq!(run["run"]["checks"][0]["passed"], true, "{run}");
    assert_eq!(run["run"]["checks_state"]["state"], "ran", "{run}");
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

/// docs/tasks/LEAD.md §12 P2: with the lead working in place, the session, the directory, the
/// branch and the staging area are the person's own, and stay as they are. The hub makes no
/// branch, commits nothing and touches no index.
#[tokio::test]
async fn a_lead_in_place_keeps_the_directory_branch_and_index_as_they_are() {
    let repo = git_repo();
    // What the person had staged, and what they edited and left uncommitted.
    std::fs::write(repo.path().join("staged.txt"), "staged\n").unwrap();
    git_out(repo.path(), &["add", "staged.txt"]);
    std::fs::write(repo.path().join("README.md"), "edited by the person\n").unwrap();
    let head = git_out(repo.path(), &["rev-parse", "HEAD"]);
    let t = setup(repo.path()).await;
    let boss = agent(&t, "boss").await["id"].as_str().unwrap().to_owned();
    let mut w = t.watch().await;

    let run = start_in_place(
        &t,
        "boss",
        format!(
            "Add notes. [write notes.txt=from the lead] {}",
            call("run_finish", json!({ "summary": "added notes" }))
        ),
    )
    .await;
    let id = run["id"].as_str().unwrap().to_owned();
    // The four meanings, kept apart: where the lead works, what the workspace is, whose it
    // is, and what happens to it (docs/tasks/LEAD.md §11).
    assert_eq!(run["mode"]["lead"], "in_place", "{run}");
    assert_eq!(run["mode"]["vcs"], "git", "{run}");
    assert_eq!(run["mode"]["owner"], "person", "{run}");
    assert_eq!(run["mode"]["retain"], "keep", "{run}");
    assert_eq!(
        run["in_place"], false,
        "a git workspace is not `in_place`: {run}"
    );
    assert_eq!(run["branch"], "", "no branch of the hub's: {run}");
    assert_eq!(run["base_branch"], "main", "{run}");
    assert_eq!(run["planner"], boss, "the lead is the agent itself: {run}");

    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;

    // The lead's own directory: same session, same place.
    let lead = agent(&t, "boss").await;
    assert_eq!(lead["id"], boss);
    assert!(lead["archived_at"].is_null());
    assert!(
        !lead["cwd"].as_str().unwrap().contains("worktrees"),
        "the lead stayed in the person's directory: {lead}"
    );
    // Nothing on the branch, in the index or in the tree was moved by the hub.
    assert_eq!(
        git_out(repo.path(), &["rev-parse", "--abbrev-ref", "HEAD"]),
        "main"
    );
    assert_eq!(git_out(repo.path(), &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git_out(repo.path(), &["diff", "--cached", "--name-only"]),
        "staged.txt",
        "the person's staging area is theirs"
    );
    assert_eq!(
        git_out(repo.path(), &["diff", "--name-only"]),
        "README.md",
        "the person's uncommitted edit is theirs"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("staged.txt")).unwrap(),
        "staged\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "edited by the person\n"
    );
    // The work is in that very directory, and no run branch was made for it.
    assert_eq!(
        std::fs::read_to_string(repo.path().join("notes.txt")).unwrap(),
        "from the lead"
    );
    let all = branches(repo.path());
    assert_eq!(all.trim(), "* main", "{all}");
    let stored = detail(&t, &id).await;
    assert_eq!(
        stored["run"]["checks_state"]["state"], "not_configured",
        "{stored}"
    );
    let human = t.get("/v1/questions").await.text().await.unwrap();
    assert!(human.contains("改动已直接写在"), "{human}");
}

/// docs/tasks/LEAD.md §7: a result is bound to the content it was about. Once the person's
/// directory changed, that result closes nothing — it is not a pass for the new content, and
/// the checks run again.
#[tokio::test]
async fn a_result_bound_to_other_content_does_not_close_the_run() {
    let repo = git_repo();
    std::fs::write(
        repo.path().join("crossagents.toml"),
        "[run]\nchecks = [\"git ls-files --error-unmatch ok.txt\"]\n",
    )
    .unwrap();
    commit(repo.path(), &["crossagents.toml"], "checks");
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;

    // The lead writes, then closes: the check fails (ok.txt is not tracked), so the run stays
    // open with that result on record.
    let run = start_in_place(
        &t,
        "boss",
        format!(
            "Add notes. [write notes.txt=x] {}",
            call("run_finish", json!({ "summary": "added notes" }))
        ),
    )
    .await;
    let id = run["id"].as_str().unwrap().to_owned();
    w.until("the lead's turn ended", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;
    let stored = detail(&t, &id).await;
    assert_eq!(stored["run"]["checks_state"]["state"], "ran", "{stored}");
    assert_eq!(stored["run"]["checks"][0]["passed"], false, "{stored}");
    assert!(stored["run"]["checks_snapshot"].is_string(), "{stored}");

    // The person changes the directory.
    std::fs::write(repo.path().join("ok.txt"), "yes\n").unwrap();
    commit(repo.path(), &["ok.txt"], "the person adds ok.txt");

    // That result was about other content: closing with it is refused, and it is not read as
    // a pass for what is there now.
    let resp = t.post(&format!("/v1/runs/{id}/finish"), json!({})).await;
    assert_eq!(resp.status(), 409, "{}", resp.text().await.unwrap());
    let said = resp.text().await.unwrap();
    assert!(said.contains("changed after the checks ran"), "{said}");
    assert_eq!(detail(&t, &id).await["run"]["status"], "planning");

    // Run again on what is there now: they pass, and the run closes.
    t.send(
        "boss",
        &call("run_finish", json!({ "summary": "added notes" })),
    )
    .await;
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;
    let stored = detail(&t, &id).await;
    assert_eq!(stored["run"]["checks"][0]["passed"], true, "{stored}");
    assert_eq!(stored["run"]["checks_state"]["state"], "ran", "{stored}");
}

/// docs/tasks/LEAD.md §7, multica §2.6: one in-place run per checkout. A second one is
/// refused, in the words a client shows it by, and the directory is free once the first ends.
#[tokio::test]
async fn a_second_run_in_the_same_directory_waits_for_it() {
    let repo = git_repo();
    let t = TestHub::start().await;
    t.add_agent_in("boss", Some("opus"), "planner", repo.path())
        .await;
    t.add_agent_in("second", Some("opus"), "planner", repo.path())
        .await;

    // A run whose lead works there and does not finish: it holds the directory.
    let first = start_in_place(&t, "boss", "Look around.".into()).await;
    let first_id = first["id"].as_str().unwrap().to_owned();

    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": "Look around.", "planner": "second", "lead_place": "in_place" }),
        )
        .await;
    assert_eq!(resp.status(), 409, "{}", resp.text().await.unwrap());
    let said = resp.text().await.unwrap();
    assert!(said.contains("waiting_local_directory"), "{said}");
    assert!(said.contains(&first_id), "{said}");

    // Ended, it lets go at once: an ended run must not keep the person out.
    let resp = t
        .post(&format!("/v1/runs/{first_id}/finish"), json!({}))
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let second = start_in_place(&t, "second", "Look around.".into()).await;
    assert_eq!(second["mode"]["lead"], "in_place", "{second}");
}

/// docs/tasks/LEAD.md §7: landing into that directory obeys the same occupancy — a merge
/// never starts in a working copy a run's lead is writing in.
#[tokio::test]
async fn landing_waits_while_a_run_works_in_the_directory() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    chat_first(&t, &mut w).await;

    // A run of its own, with a change waiting to be landed.
    let work = start(
        &t,
        format!(
            "One. [write one.txt=1] {}",
            call("run_finish", json!({ "summary": "ok" }))
        ),
    )
    .await;
    let work_id = work["id"].as_str().unwrap().to_owned();
    w.until(
        "run completed",
        run_status_is(&work_id, RunStatus::Completed),
    )
    .await;

    t.add_agent_in("second", Some("opus"), "planner", repo.path())
        .await;
    let held = start_in_place(&t, "second", "Look around.".into()).await;
    let held_id = held["id"].as_str().unwrap().to_owned();

    let resp = t
        .post(
            &format!("/v1/runs/{work_id}/land"),
            json!({ "mode": "merge" }),
        )
        .await;
    assert_eq!(resp.status(), 409, "{}", resp.text().await.unwrap());
    let said = resp.text().await.unwrap();
    assert!(said.contains("waiting_local_directory"), "{said}");
    assert!(!repo.path().join("one.txt").exists());

    // The run ends; the landing goes through.
    let resp = t
        .post(&format!("/v1/runs/{held_id}/finish"), json!({}))
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let resp = t
        .post(
            &format!("/v1/runs/{work_id}/land"),
            json!({ "mode": "merge" }),
        )
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    assert_eq!(
        std::fs::read_to_string(repo.path().join("one.txt")).unwrap(),
        "1"
    );
}

/// docs/tasks/LEAD.md §12 P2: no worker in the person's directory. A run whose lead works
/// there gives no task out, and says why instead of writing beside the lead.
#[tokio::test]
async fn a_run_in_place_gives_no_task_to_a_worker() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let plan = call(
        "plan_submit",
        json!({ "tasks": [{ "key": "a", "title": "A", "spec": { "goal": "[write a.txt=a]" } }] }),
    );
    let run = start_in_place(&t, "boss", format!("Split it. {plan}")).await;
    let id = run["id"].as_str().unwrap().to_owned();
    w.until(
        "awaiting approval",
        run_status_is(&id, RunStatus::AwaitingApproval),
    )
    .await;
    let resp = t.post(&format!("/v1/runs/{id}/approve"), json!({})).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    // The task never runs in the person's directory: it fails with the reason, and leaves
    // nothing behind there.
    let mut failure = String::new();
    let deadline = tokio::time::Instant::now() + common::wait_limit();
    loop {
        let stored = detail(&t, &id).await;
        failure = stored["tasks"][0]["report"]["failure"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if !failure.is_empty() || tokio::time::Instant::now() > deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(
        failure.contains("your own directory"),
        "the task says why it cannot start: {failure:?}"
    );
    assert!(!repo.path().join("a.txt").exists());
    assert_eq!(
        detail(&t, &id).await["tasks"][0]["status"],
        "failed",
        "nothing merged"
    );
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

    // The retained copy stays after finish. Explicit archival removes its checkout.
    let lead = agent(&t, landed["planner"].as_str().unwrap()).await;
    let lead_id = lead["id"].as_str().unwrap().to_owned();
    let dir = std::path::PathBuf::from(lead["cwd"].as_str().unwrap());
    let branch = landed["branch"].as_str().unwrap().to_owned();
    archive_and_wait(&t, &lead_id, &dir).await;
    // Whether the branch went with the checkout is `lead_left`'s call, and it races the
    // landing: a landed run's branch goes, one read before the landing may stay. Drop it
    // here so the restore is the same either way: built from the base.
    let _ = std::process::Command::new("git")
        .args(["branch", "-D", &branch])
        .current_dir(repo.path())
        .output();

    let archive_path = format!("/v1/agents/{lead_id}/archive");
    let archive = |on: bool| t.post(&archive_path, json!({ "archived": on }));
    // Restored, it gets its checkout back, from the base the run landed on.
    assert_eq!(archive(false).await.status(), 200);
    until_back(repo.path(), &dir, "one.txt").await;
    // What a cargo build there left in the checkout's own build directory
    // (`<run>/targets/integration`, docs/TODO.md §九 ②).
    let target = dir.parent().unwrap().join("targets").join("integration");
    std::fs::create_dir_all(target.join("debug")).unwrap();

    assert_eq!(archive(true).await.status(), 200);
    // Landed: nothing waits on the branch, so it goes with the checkout, build directory
    // and all.
    until_gone(&target).await;
    assert!(!dir.exists());
    assert!(
        !branches(repo.path()).contains(&branch),
        "{}",
        branches(repo.path())
    );

    let from = w.seen.len();
    assert_eq!(archive(false).await.status(), 200);
    until_back(repo.path(), &dir, "one.txt").await;
    // Landed or not, the person is told the run's code was not checked out again: a landing
    // does not prove the base still has it.
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
    until_gone(&dir).await;
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
    let lead_id = lead["id"].as_str().unwrap().to_owned();
    let dir = std::path::PathBuf::from(lead["cwd"].as_str().unwrap());
    let branch = run["branch"].as_str().unwrap().to_owned();
    // Explicitly archive before testing recovery from a missing branch.
    archive_and_wait(&t, &lead_id, &dir).await;
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

/// A command the hub only knows of from the lead's tool-call events no longer holds the checks
/// back (docs/tasks/LOCKS.md §6): the build directory is guarded by the executor's lock, which
/// the checks wait for, and the event record is only shown. Closing goes through at once.
#[tokio::test]
async fn a_command_seen_running_does_not_refuse_the_checks() {
    let repo = git_repo();
    std::fs::write(
        repo.path().join("crossagents.toml"),
        "[run]\nchecks = [\"git --version\"]\n",
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
    t.agent("boss").await;
    let mut w = t.watch().await;

    let finish = call("run_finish", json!({ "summary": "added notes" }));
    let request = format!("Add notes. [write notes.txt=x] [running] {finish}");
    let id = start(&t, request).await["id"].as_str().unwrap().to_owned();
    w.until("run completed", run_status_is(&id, RunStatus::Completed))
        .await;
    let output = w.output();
    assert!(!output.contains("is still running"), "{output}");
    let run: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    assert_eq!(run["run"]["checks"].as_array().unwrap().len(), 1, "{run}");
}
