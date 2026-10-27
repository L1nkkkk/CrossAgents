//! Deleting a run (docs/tasks/R1.md §3): a run that ended goes with its tasks, its working
//! copies and — by choice — its branches. A hard delete: nothing comes back, so a run that has
//! not ended must be cancelled first. Sessions are not the run's (docs/tasks/S1.md §3.4): none
//! is deleted.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{TestHub, git_repo};
use crossagents_api_types::EventKind;
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

async fn agent(t: &TestHub, id: &str) -> Value {
    t.get(&format!("/v1/agents/{id}"))
        .await
        .json()
        .await
        .unwrap()
}

/// The run the Work card started, from its beginning to its first detail.
async fn start(t: &TestHub) -> Value {
    let mut w = t.watch().await;
    let proposal = call(
        "work_start",
        json!({ "request": "Add notes.txt.", "title": "Notes" }),
    );
    t.send("lyra", &format!("On a branch. {proposal}")).await;
    w.until(
        "card",
        |e| matches!(&e.kind, EventKind::HumanAsked { item } if item.text == "Notes"),
    )
    .await;
    let all: Value = t.get("/v1/questions").await.json().await.unwrap();
    let item = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["action"] == "work" && i["status"] == "open")
        .unwrap_or_else(|| panic!("no work card in {all}"))
        .clone();
    let resp = t
        .post(
            &format!("/v1/questions/{}/answer", item["id"].as_str().unwrap()),
            json!({ "answer": "开始" }),
        )
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let all: Value = t.get("/v1/questions?all=true").await.json().await.unwrap();
    let id = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == item["id"])
        .and_then(|i| i["run_id"].as_str())
        .unwrap_or_else(|| panic!("the card started no run: {all}"))
        .to_owned();
    let detail: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    let run = detail["run"].clone();
    let lead = agent(t, run["planner"].as_str().unwrap()).await;
    assert!(
        Path::new(lead["cwd"].as_str().unwrap()).is_dir(),
        "the lead has no checkout: {lead}"
    );
    run
}

/// Polls the run until `done` accepts it; a lead's turn ends asynchronously.
async fn until_run(t: &TestHub, id: &str, done: impl Fn(&Value) -> bool) -> Value {
    let mut last = Value::Null;
    for _ in 0..100 {
        last = t
            .get(&format!("/v1/runs/{id}"))
            .await
            .json::<Value>()
            .await
            .unwrap();
        if done(&last["run"]) {
            return last["run"].clone();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("the run never got there: {last}");
}

/// The lead writes a file and closes the run; the run is `completed` and never landed.
async fn finish_with_a_change(t: &TestHub, run: &Value) -> Value {
    let lead = run["planner"].as_str().unwrap();
    let text = format!(
        "Wrap up. [write notes.txt=hello] {}",
        call("run_finish", json!({ "summary": "wrote notes" }))
    );
    t.send(lead, &text).await;
    until_run(t, run["id"].as_str().unwrap(), |r| {
        r["status"] == "completed"
    })
    .await
}

/// The run's branches in the repository, as `git branch --list` names them (`*` is the one
/// checked out, `+` one checked out in another working copy).
fn branches(repo: &Path) -> Vec<String> {
    let out = std::process::Command::new("git")
        .args(["branch", "--list", "crossagents/*"])
        .current_dir(repo)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| line.trim().trim_start_matches(['*', '+']).trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect()
}

async fn setup() -> (tempfile::TempDir, TestHub) {
    let repo = git_repo();
    let t = TestHub::start().await;
    t.add_agent_in("lyra", Some("opus"), "worker", repo.path())
        .await;
    (repo, t)
}

/// R1 §3.4: only a run that ended can go, and cancelling it first is the way.
#[tokio::test]
async fn a_run_that_is_still_open_must_be_cancelled_first() {
    let (repo, t) = setup().await;
    let run = start(&t).await;
    let id = run["id"].as_str().unwrap();

    let resp = t.delete(&format!("/v1/runs/{id}")).await;
    assert_eq!(resp.status(), 409, "{}", resp.text().await.unwrap());
    let refused: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    assert_eq!(refused["run"]["id"], run["id"]);

    let resp = t.post(&format!("/v1/runs/{id}/cancel"), json!({})).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let resp = t.delete(&format!("/v1/runs/{id}")).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    // Cancelled, so it never landed: its branch stays unless asked for.
    assert_eq!(
        branches(repo.path()).len(),
        1,
        "{:?}",
        branches(repo.path())
    );
}

/// R1 §3.4: a landed run takes its branches with it, leaves `/v1/runs`, and says so on the
/// event stream. The lead's copy was archived when the run ended, its checkout gone with it
/// (S1 §3.4); it is not deleted, and the delete puts it back in its workspace.
#[tokio::test]
async fn a_landed_run_goes_with_its_worktrees_branches_and_lead() {
    let (repo, t) = setup().await;
    let run = start(&t).await;
    let id = run["id"].as_str().unwrap().to_owned();
    let lead = run["planner"].as_str().unwrap().to_owned();
    let checkout = agent(&t, &lead).await["cwd"].as_str().unwrap().to_owned();
    assert!(Path::new(&checkout).is_dir(), "{checkout}");
    let branch = run["branch"].as_str().unwrap().to_owned();
    assert_eq!(branches(repo.path()), vec![branch.clone()]);

    finish_with_a_change(&t, &run).await;
    let copy = agent(&t, &lead).await;
    assert!(!copy["archived_at"].is_null(), "{copy}");
    assert_eq!(copy["cwd"], json!(checkout), "it keeps its directory: {copy}");
    assert!(!Path::new(&checkout).exists(), "{checkout}");
    let resp = t
        .post(&format!("/v1/runs/{id}/land"), json!({ "mode": "merge" }))
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    assert!(repo.path().join("notes.txt").exists(), "landed on main");

    let mut w = t.watch().await;
    let resp = t.delete(&format!("/v1/runs/{id}")).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let done: Value = resp.json().await.unwrap();
    assert_eq!(done["branches"], json!([branch]), "{done}");
    assert_eq!(done["warnings"], json!([]), "{done}");
    // The run held nobody any more: its copy was archived when it ended.
    assert_eq!(done["agents"]["archived"], json!([]), "{done}");
    assert_eq!(done["agents"]["returned"], json!([]), "{done}");

    w.until(
        "run deleted",
        |e| matches!(&e.kind, EventKind::RunDeleted { run_id } if run_id.to_string() == id),
    )
    .await;
    let runs: Value = t.get("/v1/runs").await.json().await.unwrap();
    assert!(
        runs["runs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["id"] != json!(id)),
        "{runs}"
    );
    // Still there, archived, back in the repository: its conversation can be restored.
    let kept = agent(&t, &lead).await;
    assert!(!kept["archived_at"].is_null(), "{kept}");
    assert_eq!(kept["cwd"], kept["home"], "{kept}");
    assert!(!Path::new(&checkout).exists(), "{checkout}");
    assert!(
        branches(repo.path()).is_empty(),
        "{:?}",
        branches(repo.path())
    );
}

/// R1 §3.4: an unlanded run's branch is all its changes exist in, so it stays unless asked
/// for; a second run deleted with `branches=true` takes its branch too.
#[tokio::test]
async fn an_unlanded_run_keeps_its_branch_unless_asked() {
    let (repo, t) = setup().await;
    let first = start(&t).await;
    let first_branch = first["branch"].as_str().unwrap().to_owned();
    finish_with_a_change(&t, &first).await;

    let resp = t
        .delete(&format!("/v1/runs/{}", first["id"].as_str().unwrap()))
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let done: Value = resp.json().await.unwrap();
    assert_eq!(done["branches"], json!([]), "{done}");
    assert_eq!(branches(repo.path()), vec![first_branch.clone()]);

    // The conversation is still there, so it proposes again; this one goes with its branch.
    let second = start(&t).await;
    let second_branch = second["branch"].as_str().unwrap().to_owned();
    finish_with_a_change(&t, &second).await;
    let resp = t
        .delete(&format!(
            "/v1/runs/{}?branches=true",
            second["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let done: Value = resp.json().await.unwrap();
    assert_eq!(done["branches"], json!([second_branch]), "{done}");
    assert_eq!(branches(repo.path()), vec![first_branch]);
}

/// R1 §3.4: work left uncommitted in the checkout would be thrown away, so it answers 409
/// with the files; `force` goes ahead.
#[tokio::test]
async fn a_dirty_checkout_asks_for_force() {
    let (repo, t) = setup().await;
    let run = start(&t).await;
    let id = run["id"].as_str().unwrap().to_owned();
    let lead = run["planner"].as_str().unwrap().to_owned();
    let checkout = std::path::PathBuf::from(agent(&t, &lead).await["cwd"].as_str().unwrap());
    std::fs::write(checkout.join("left.txt"), "the lead's\n").unwrap();
    // The person ends the run as it is: nothing merged, nothing landed. Its lead, a copy, is
    // archived and the checkout goes (S1 §3.4), what it left committed to the run branch.
    let resp = t.post(&format!("/v1/runs/{id}/finish"), json!({})).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let finished: Value = resp.json().await.unwrap();
    assert!(finished["run"]["landed"].is_null(), "{finished}");
    assert!(!checkout.exists(), "{checkout:?}");
    // The person restores the copy, which brings its checkout back, and something is left
    // there uncommitted.
    let resp = t
        .post(&format!("/v1/agents/{lead}/archive"), json!({ "archived": false }))
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    assert!(checkout.is_dir(), "{checkout:?}");
    assert!(checkout.join("left.txt").exists(), "{checkout:?}");
    std::fs::write(checkout.join("wip.txt"), "unfinished\n").unwrap();

    let resp = t.delete(&format!("/v1/runs/{id}")).await;
    assert_eq!(resp.status(), 409, "{}", resp.text().await.unwrap());
    let refused: Value = resp.json().await.unwrap();
    assert_eq!(refused["error"]["files"], json!(["wip.txt"]), "{refused}");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("wip.txt"),
        "{refused}"
    );
    // Still there, nothing removed.
    assert_eq!(t.get(&format!("/v1/runs/{id}")).await.status(), 200);
    assert!(checkout.is_dir(), "{checkout:?}");

    let resp = t.delete(&format!("/v1/runs/{id}?force=true")).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let done: Value = resp.json().await.unwrap();
    assert!(!checkout.exists(), "{checkout:?} is still there: {done}");
    assert!(t.get(&format!("/v1/runs/{id}")).await.status() == 404);
    // Unlanded and not asked for: the branch, with what was committed, stays.
    assert_eq!(
        branches(repo.path()).len(),
        1,
        "{:?}",
        branches(repo.path())
    );
}

/// R1 §3.4: the conversation that proposed the Work is not the run's to delete.
#[tokio::test]
async fn the_conversation_that_proposed_the_work_stays() {
    let (_repo, t) = setup().await;
    let run = start(&t).await;
    finish_with_a_change(&t, &run).await;
    let resp = t
        .delete(&format!("/v1/runs/{}", run["id"].as_str().unwrap()))
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());

    let me = agent(&t, "lyra").await;
    assert_eq!(me["name"], "lyra");
    let transcript: Value = t
        .get("/v1/agents/lyra/transcript")
        .await
        .json()
        .await
        .unwrap();
    assert!(
        !transcript["turns"].as_array().unwrap().is_empty(),
        "{transcript}"
    );
    assert!(
        transcript["turns"][0]["input"]
            .as_str()
            .unwrap()
            .contains("work_start"),
        "{transcript}"
    );
}

/// A lead that never had a session is moved into the run's checkout, not copied: it may be
/// someone the person picked from the roster. The run lets go of it when it ends (S1 §3.4),
/// so it is back where it lived before the run is deleted, and the delete has nobody left
/// to send home.
#[tokio::test]
async fn a_lead_moved_into_the_run_goes_back_home() {
    let (repo, t) = setup().await;
    let pia = t
        .add_agent_in("pia", Some("opus"), "planner", repo.path())
        .await;
    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": "Add notes.txt.", "planner": "pia" }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let run: Value = resp.json().await.unwrap();
    let id = run["id"].as_str().unwrap().to_owned();
    // Moved, not copied: the run's lead is pia itself, living in the run's checkout.
    assert_eq!(run["planner"], json!(pia.id.to_string()), "{run}");
    let moved = agent(&t, "pia").await;
    assert_ne!(
        moved["cwd"],
        json!(repo.path().display().to_string()),
        "{moved}"
    );

    let resp = t.post(&format!("/v1/runs/{id}/finish"), json!({})).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    // Ended: back home, once its turn (the kickoff) is over.
    let mut back = Value::Null;
    for _ in 0..100 {
        back = agent(&t, "pia").await;
        if back["cwd"] == json!(repo.path().display().to_string()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        back["cwd"],
        json!(repo.path().display().to_string()),
        "{back}"
    );
    let resp = t.delete(&format!("/v1/runs/{id}")).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let done: Value = resp.json().await.unwrap();
    // The run held nobody any more: pia's claim went when it ended.
    assert_eq!(
        done["agents"],
        json!({ "returned": [], "archived": [] }),
        "{done}"
    );

    let back = agent(&t, "pia").await;
    assert!(back["archived_at"].is_null(), "{back}");
    assert!(back["config"].get("lead_home").is_none(), "{back}");
    assert!(!Path::new(moved["cwd"].as_str().unwrap()).exists(), "{moved}");
}

/// S1 §7: the lead copied for a run has a driver that resumes a session only where it ran.
/// Once the run is deleted that directory is gone, so restoring the copy and talking to it
/// cannot carry on its conversation: both say so, and the next session is a new one — never
/// a resume that quietly starts over.
#[tokio::test]
async fn a_copy_restored_after_its_run_is_deleted_says_its_context_is_gone() {
    let (_repo, t) = setup().await;
    let run = start(&t).await;
    let id = run["id"].as_str().unwrap().to_owned();
    let lead = run["planner"].as_str().unwrap().to_owned();
    finish_with_a_change(&t, &run).await;
    let resp = t.delete(&format!("/v1/runs/{id}")).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let copy = agent(&t, &lead).await;
    assert_eq!(copy["cwd"], copy["home"], "{copy}");

    let mut w = t.watch().await;
    let resp = t
        .post(&format!("/v1/agents/{lead}/archive"), json!({ "archived": false }))
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    w.until("restore says the context is gone", |e| {
        matches!(&e.kind, EventKind::AgentWarning { id, message }
            if id.to_string() == lead && message.contains("不带原来的上下文") && message.contains("删除了"))
    })
    .await;

    t.send(&lead, "Still there?").await;
    w.until("the new session says so", |e| {
        matches!(&e.kind, EventKind::AgentWarning { id, message }
            if id.to_string() == lead && message.contains("这是新会话"))
    })
    .await;
    w.until("answered", |e| {
        matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if id.to_string() == lead)
    })
    .await;
    // Said once: the mark goes with the new session.
    let copy = agent(&t, &lead).await;
    assert!(copy["config"].get("context_lost").is_none(), "{copy}");
}
