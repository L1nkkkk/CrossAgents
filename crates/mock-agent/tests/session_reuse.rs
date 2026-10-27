//! Sessions belong to the workspace, runs only record who took part (docs/tasks/S1.md): a run
//! is led by the agent picked, itself, and lets go of it when it ends; deleting a run touches
//! nobody another run holds.

mod common;

use std::path::Path;

use common::{TestHub, Watcher, git_repo, native_plugin};
use crossagents_api_types::{AgentId, EventKind, RunStatus};
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

fn same_dir(a: &Path, b: &Path) -> bool {
    std::fs::canonicalize(a).ok() == std::fs::canonicalize(b).ok()
}

/// `Orchestrator::short`: the id without dashes, last 8 characters.
fn short(id: &str) -> String {
    let id = id.replace('-', "");
    id[id.len() - 8..].to_owned()
}

async fn start_run(t: &TestHub, planner: &str, request: &str) -> Value {
    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": request, "planner": planner }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    resp.json().await.unwrap()
}

async fn run_ends(w: &mut Watcher, run: &Value) {
    let id = run["id"].as_str().unwrap().to_owned();
    w.until("run completed", |e| {
        matches!(&e.kind, EventKind::RunStatusChanged { id: rid, to: RunStatus::Completed, .. }
            if rid.to_string() == id)
    })
    .await;
}

/// Waits for `agent` to be back in `home`.
async fn back_home(w: &mut Watcher, agent: AgentId, home: &Path) {
    w.find("back home", |e| {
        matches!(&e.kind, EventKind::AgentUpdated { agent: a } if a.id == agent && same_dir(&a.cwd, home))
    })
    .await;
}

async fn agent_count(t: &TestHub) -> usize {
    let all: Value = t.get("/v1/agents?archived=true").await.json().await.unwrap();
    all["agents"].as_array().unwrap().len()
}

fn finish() -> String {
    call("run_finish", json!({ "summary": "done" }))
}

#[tokio::test]
async fn run_b_is_led_by_run_as_lead_itself() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    let pia = t.add_native("Pia", "planner", repo.path()).await;
    let mut w = t.watch().await;

    let a = start_run(&t, "Pia", &format!("Run A. [session] {}", finish())).await;
    assert_eq!(a["planner"], json!(pia.id), "the lead is Pia, not a copy");
    run_ends(&mut w, &a).await;
    // Run A ended and let go of her: she is back in the workspace.
    back_home(&mut w, pia.id, repo.path()).await;
    let first = w.output();
    let session = first
        .split("session=")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .unwrap_or_else(|| panic!("no session in {first}"))
        .to_owned();
    let before = agent_count(&t).await;

    let b = start_run(&t, "Pia", &format!("Run B. [session] [cwd] {}", finish())).await;
    // Pia herself, with run A's conversation: the same session resumed in run B's checkout.
    assert_eq!(b["planner"], json!(pia.id), "{b}");
    assert_eq!(agent_count(&t).await, before, "no agent was added");
    assert!(
        b["lead_note"].is_null(),
        "she was not copied: {}",
        b["lead_note"]
    );
    run_ends(&mut w, &b).await;
    let second = w.output()[first.len()..].to_owned();
    assert!(
        second.contains(&format!("session={session} (resumed)")),
        "{second}"
    );
    let cwd = second
        .split("cwd=")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .unwrap();
    assert!(
        cwd.contains(&short(b["id"].as_str().unwrap())) && cwd.contains("integration"),
        "{cwd}"
    );
    back_home(&mut w, pia.id, repo.path()).await;
    let claims = t.hub.bus.store().list_claims(None).await.unwrap();
    assert!(claims.is_empty(), "{claims:?}");
}

#[tokio::test]
async fn deleting_an_old_run_leaves_the_run_that_holds_its_lead_now() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    let pia = t.add_native("Pia", "planner", repo.path()).await;
    let mut w = t.watch().await;

    let a = start_run(&t, "Pia", &format!("Run A. {}", finish())).await;
    run_ends(&mut w, &a).await;
    back_home(&mut w, pia.id, repo.path()).await;

    // Run B holds her now, working in its checkout.
    let b = start_run(&t, "Pia", "Run B. Wait for my word.").await;
    let b_id = b["id"].as_str().unwrap().to_owned();
    w.until("run B's first turn", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { id, .. } if id == pia.id)
    })
    .await;
    let in_b = t.agent("Pia").await.cwd;
    assert!(in_b.display().to_string().contains(&short(&b_id)), "{in_b:?}");

    // Run A took part in her past, not her present: deleting it leaves her where she is.
    let resp = t
        .delete(&format!("/v1/runs/{}", a["id"].as_str().unwrap()))
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let done: Value = resp.json().await.unwrap();
    assert_eq!(
        done["agents"],
        json!({ "returned": [], "archived": [] }),
        "{done}"
    );
    assert_eq!(t.agent("Pia").await.cwd, in_b);
    let claim = t
        .hub
        .bus
        .store()
        .claim_of(pia.id)
        .await
        .unwrap()
        .expect("run B still holds her");
    assert_eq!(claim.run_id.to_string(), b_id);

    // Run B goes on and finishes as usual.
    t.send("Pia", &format!("Go ahead. {}", finish())).await;
    run_ends(&mut w, &b).await;
    back_home(&mut w, pia.id, repo.path()).await;
}
