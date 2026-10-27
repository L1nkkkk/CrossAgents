//! An agent's home and where it works (docs/tasks/S1.md §3.1): moving a session between
//! directories with `place`, refusing while it is busy, never replacing a session that has
//! to carry its conversation, and giving agents from before homes the workspace they belong
//! to when the hub starts.

mod common;

use std::path::Path;

use common::{TestHub, Watcher, native_plugin};
use crossagents_api_types::{AgentId, EventKind};
use crossagents_core::{AgentKey, CoreError, MessageSource, Store};
use serde_json::json;

fn same_dir(a: &Path, b: &Path) -> bool {
    std::fs::canonicalize(a).unwrap() == std::fs::canonicalize(b).unwrap()
}

async fn turn_done(w: &mut Watcher, agent: AgentId) {
    w.until("turn completed", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { id, .. } if id == agent)
    })
    .await;
}

/// What the mock said for `[session]` and `[cwd]` in `output`: the session id, whether it
/// was resumed, and the directory.
fn session_and_cwd(output: &str) -> (String, String, String) {
    let session = output
        .split("session=")
        .nth(1)
        .unwrap_or_else(|| panic!("no session in {output}"));
    let (id, rest) = session.split_once(" (").unwrap();
    let how = rest.split(')').next().unwrap();
    (id.to_owned(), how.to_owned(), cwd_in(output))
}

/// The directory the mock said for `[cwd]` (test directories have no spaces).
fn cwd_in(output: &str) -> String {
    output
        .split("cwd=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("no cwd in {output}"))
        .to_owned()
}

#[tokio::test]
async fn place_moves_an_idle_session_and_it_resumes_there() {
    let t = TestHub::start_with(native_plugin).await;
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let pia = t.add_native("Pia", "worker", a.path()).await;
    let mut w = t.watch().await;
    t.send("Pia", "[cwd] [session] first").await;
    turn_done(&mut w, pia.id).await;
    let first = w.output();
    let (session, how, cwd) = session_and_cwd(&first);
    assert_eq!(how, "new", "{first}");
    assert!(same_dir(Path::new(&cwd), a.path()), "{cwd}");

    t.hub
        .runtime
        .place(&AgentKey::Id(pia.id), b.path(), false)
        .await
        .unwrap();
    let moved = t.agent("Pia").await;
    assert!(same_dir(&moved.cwd, b.path()));
    // It still belongs where it lives.
    assert!(same_dir(&moved.home, a.path()));

    t.send("Pia", "[cwd] [session] second").await;
    turn_done(&mut w, pia.id).await;
    let second = w.output()[first.len()..].to_owned();
    let (again, how, cwd) = session_and_cwd(&second);
    // The driver was asked to resume the same session (`--resume <id>`), in the new place.
    assert_eq!(again, session, "{second}");
    assert_eq!(how, "resumed", "{second}");
    assert!(same_dir(Path::new(&cwd), b.path()), "{cwd}");
}

#[tokio::test]
async fn place_is_busy_while_a_turn_runs_or_a_persons_message_waits() {
    let t = TestHub::start_with(native_plugin).await;
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let pia = t.add_native("Pia", "worker", a.path()).await;
    let key = AgentKey::Id(pia.id);
    let mut w = t.watch().await;

    // Answering a person.
    t.send("Pia", "[sleep 1500] one").await;
    w.until("turn started", |e| {
        matches!(e.kind, EventKind::AgentTurnStarted { id, .. } if id == pia.id)
    })
    .await;
    let err = t.hub.runtime.place(&key, b.path(), false).await.unwrap_err();
    assert!(
        matches!(&err, CoreError::Busy(why) if why.contains("answering a person's message")),
        "{err}"
    );
    turn_done(&mut w, pia.id).await;

    // Busy with something else, a person's message in line behind it.
    t.hub
        .runtime
        .send(&key, "[sleep 1500] notice".into(), MessageSource::Notice)
        .await
        .unwrap();
    t.send("Pia", "two").await;
    assert!(t.hub.runtime.serving_person(pia.id).await);
    let err = t.hub.runtime.place(&key, b.path(), false).await.unwrap_err();
    assert!(
        matches!(&err, CoreError::Busy(why) if why.contains("a person's message is waiting")),
        "{err}"
    );
    turn_done(&mut w, pia.id).await;
    turn_done(&mut w, pia.id).await;
    assert!(!t.hub.runtime.serving_person(pia.id).await);

    // Idle: it moves, and nothing it was asked was cut off.
    t.hub.runtime.place(&key, b.path(), false).await.unwrap();
    assert!(w.output().contains("echo(model=mock-native): two"), "{}", w.output());
    assert!(
        !w.seen
            .iter()
            .any(|e| matches!(e.kind, EventKind::AgentInputWithdrawn { .. })),
        "a message was withdrawn"
    );
}

#[tokio::test]
async fn sending_a_busy_agent_home_waits_for_its_turn_to_end() {
    let t = TestHub::start_with(native_plugin).await;
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let pia = t.add_native("Pia", "worker", a.path()).await;
    let key = AgentKey::Id(pia.id);
    t.hub.runtime.place(&key, b.path(), false).await.unwrap();
    let mut w = t.watch().await;
    t.send("Pia", "[sleep 1500] [cwd] working").await;
    w.until("turn started", |e| {
        matches!(e.kind, EventKind::AgentTurnStarted { id, .. } if id == pia.id)
    })
    .await;
    // Busy: it goes once the turn is over, not in the middle of it.
    assert!(!t.hub.runtime.send_home(&key).await.unwrap());
    assert!(same_dir(&t.agent("Pia").await.cwd, b.path()));
    turn_done(&mut w, pia.id).await;
    assert!(
        same_dir(Path::new(&cwd_in(&w.output())), b.path()),
        "the turn ran where it was"
    );
    w.until("moved home", |e| {
        matches!(&e.kind, EventKind::AgentUpdated { agent } if agent.id == pia.id && same_dir(&agent.cwd, a.path()))
    })
    .await;
}

#[tokio::test]
async fn carrying_a_session_that_cannot_resume_is_an_error_not_a_new_session() {
    let t = TestHub::start_with(native_plugin).await;
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    // Resuming fails in `b`.
    std::fs::write(b.path().join("mock-no-resume"), "").unwrap();
    let pia = t.add_native("Pia", "worker", a.path()).await;
    let key = AgentKey::Id(pia.id);
    let mut w = t.watch().await;
    t.send("Pia", "[session] first").await;
    turn_done(&mut w, pia.id).await;
    let session = t
        .hub
        .bus
        .store()
        .last_session_id(pia.id)
        .await
        .unwrap()
        .unwrap();

    let err = t.hub.runtime.place(&key, b.path(), true).await.unwrap_err();
    assert!(
        matches!(&err, CoreError::Agent(why) if why.contains("its context was not carried over")),
        "{err}"
    );
    // No session took its place.
    assert_eq!(
        t.hub.bus.store().last_session_id(pia.id).await.unwrap(),
        Some(session.clone())
    );
    assert!(!t.hub.runtime.is_live(pia.id).await);

    // A plain chat keeps its fallback: its next message starts a new session, and says so.
    t.send("Pia", "[session] again").await;
    turn_done(&mut w, pia.id).await;
    assert!(w.seen.iter().any(|e| matches!(&e.kind,
        EventKind::AgentWarning { id, message } if *id == pia.id && message.contains("started a new one"))));
    let latest = t
        .hub
        .bus
        .store()
        .last_session_id(pia.id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(latest, session);
}

#[tokio::test]
async fn agents_from_before_homes_get_their_workspace_at_start() {
    use crossagents_store_sqlite::SqliteStore;

    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let paths = crossagents_hub::Paths::new(home.path());
    std::fs::create_dir_all(&paths.home).unwrap();
    let run_id = crossagents_core::RunId::new();
    let short = {
        let s = run_id.to_string().replace('-', "");
        s[s.len() - 8..].to_owned()
    };
    let worktrees = paths.home.join("worktrees").join(&short);
    for dir in ["integration", "auth"] {
        std::fs::create_dir_all(worktrees.join(dir)).unwrap();
    }
    {
        let store = SqliteStore::open(&paths.db()).await.unwrap();
        let now = chrono::Utc::now();
        let agent = |name: &str, cwd: &Path, config: serde_json::Value| {
            serde_json::from_value::<crossagents_core::Agent>(json!({
                "id": AgentId::new(), "name": name, "plugin": "mock", "driver": null,
                "model": null, "role": "worker", "cwd": cwd, "config": config,
                "status": "stopped", "created_at": now,
            }))
            .unwrap()
        };
        // A lead moved into the run's checkout, a worker in a task's, a chat at home.
        let lead = agent(
            "Pia",
            &worktrees.join("integration"),
            json!({ "lead_home": repo.path() }),
        );
        let worker = agent("Rex", &worktrees.join("auth"), json!({}));
        let chat = agent("Ada", repo.path(), json!({}));
        for a in [&lead, &worker, &chat] {
            store.insert_agent(a).await.unwrap();
        }
        let run: crossagents_core::Run = serde_json::from_value(json!({
            "id": run_id, "title": "t", "request": "r", "planner": lead.id,
            "status": "completed", "repo": repo.path(), "base_branch": "main",
            "branch": format!("crossagents/{short}"), "created_at": now, "updated_at": now,
            "config": { "worker": worker.id, "max_parallel": 1, "max_attempts": 1 },
        }))
        .unwrap();
        store.insert_run(&run).await.unwrap();
        // The run ended without letting go of its worker.
        store
            .claim_agent(&crossagents_core::AgentClaim::new(
                worker.id,
                run_id,
                None,
                crossagents_core::ClaimRole::Worker,
                true,
            ))
            .await
            .unwrap();
        store.close().await;
    }

    let t = TestHub::start_in(home, |_| {}).await;
    for name in ["Pia", "Rex", "Ada"] {
        let agent = t.agent(name).await;
        assert!(same_dir(&agent.home, repo.path()), "{name}: {:?}", agent.home);
    }
    // Where they work is left as it was; the lead's mark is gone.
    let pia = t.agent("Pia").await;
    assert!(same_dir(&pia.cwd, &worktrees.join("integration")));
    assert!(pia.config.get("lead_home").is_none(), "{}", pia.config);
    // The ended run's claim is gone.
    assert_eq!(t.hub.bus.store().list_claims(None).await.unwrap(), vec![]);
}
