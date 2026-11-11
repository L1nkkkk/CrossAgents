//! `GET /v1/agents/{key}/resume-command` (docs/TODO.md §十五): the command that carries a
//! session on in its own CLI, by its id, in the directory it works in, and why there is none
//! when it cannot be done.

mod common;

use common::{TestHub, Watcher, native_plugin};
use crossagents_api_types::{AgentId, EventKind, ResumeCommandResponse};
use crossagents_core::AgentKey;

async fn turn_done(w: &mut Watcher, agent: AgentId) {
    w.until(
        "turn completed",
        |e| matches!(e.kind, EventKind::AgentTurnCompleted { id, .. } if id == agent),
    )
    .await;
}

async fn resume_command(t: &TestHub, agent: &str) -> ResumeCommandResponse {
    let resp = t.get(&format!("/v1/agents/{agent}/resume-command")).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    resp.json().await.unwrap()
}

#[tokio::test]
async fn a_session_that_ran_is_resumed_by_its_id_where_it_works() {
    let t = TestHub::start_with(native_plugin).await;
    let dir = tempfile::tempdir().unwrap();
    let rex = t.add_native("Rex", "worker", dir.path()).await;

    // No session yet: nothing to resume.
    let before = resume_command(&t, "Rex").await;
    assert!(before.command.is_none());
    assert!(
        before
            .unavailable
            .as_deref()
            .unwrap()
            .contains("还没开过会话"),
        "{before:?}"
    );

    let mut w = t.watch().await;
    t.send("Rex", "hello").await;
    turn_done(&mut w, rex.id).await;
    let id = t
        .hub
        .bus
        .store()
        .last_session_id(rex.id)
        .await
        .unwrap()
        .expect("a session id");

    let r = resume_command(&t, "Rex").await;
    let command = r.command.clone().unwrap_or_else(|| panic!("{r:?}"));
    assert_eq!(r.session_id.as_deref(), Some(id.as_str()));
    assert_eq!(r.cwd, rex.cwd.display().to_string());
    assert!(command.contains(&format!("--resume '{id}'")), "{command}");
    assert!(command.contains(&r.cwd), "{command}");
    // The program the hub launches, not a guess at the name.
    assert!(command.contains("crossagents-mock-agent"), "{command}");
    if cfg!(windows) {
        assert_eq!(r.shell, "powershell");
        assert!(
            command.starts_with("Set-Location -LiteralPath "),
            "{command}"
        );
    }
    // Only config directories and proxies, never the hub's or an agent's token.
    for e in &r.env {
        assert!(
            e.name.ends_with("_CONFIG_DIR")
                || e.name == "CODEX_HOME"
                || e.name.to_ascii_uppercase().contains("PROXY"),
            "{e:?}"
        );
    }
    assert!(!command.contains(&t.hub.token), "{command}");
    // It is open in the hub now: said, not refused.
    assert!(
        r.notes.iter().any(|n| n.contains("停掉它")),
        "{:?}",
        r.notes
    );

    // Its directory gone: refused, and why.
    t.hub.runtime.stop(&AgentKey::Id(rex.id)).await.unwrap();
    drop(dir);
    let gone = resume_command(&t, "Rex").await;
    assert!(gone.command.is_none());
    assert!(
        gone.unavailable.as_deref().unwrap().contains("不在了"),
        "{gone:?}"
    );
}

#[tokio::test]
async fn a_lost_context_is_not_offered_as_resumable() {
    let t = TestHub::start_with(native_plugin).await;
    let dir = tempfile::tempdir().unwrap();
    let pia = t.add_native("Pia", "worker", dir.path()).await;
    let mut w = t.watch().await;
    t.send("Pia", "hello").await;
    turn_done(&mut w, pia.id).await;
    t.hub
        .bus
        .store()
        .update_agent_config(
            pia.id,
            &serde_json::json!({ "context_lost": "run「x」删除了，原执行目录也不在了" }),
        )
        .await
        .unwrap();

    let r = resume_command(&t, "Pia").await;
    assert!(r.command.is_none());
    assert!(
        r.unavailable.as_deref().unwrap().contains("run「x」删除了"),
        "{r:?}"
    );
}

#[tokio::test]
async fn a_driver_that_cannot_resume_says_so() {
    // `mock` is an ACP agent: its driver resumes nothing.
    let t = TestHub::start().await;
    let ada = t.add_agent("Ada", Some("mock-opus")).await;
    let mut w = t.watch().await;
    t.send("Ada", "hello").await;
    turn_done(&mut w, ada.id).await;

    let r = resume_command(&t, "Ada").await;
    assert!(r.command.is_none());
    assert!(
        r.unavailable.as_deref().unwrap().contains("不能恢复会话"),
        "{r:?}"
    );

    assert_eq!(
        t.get("/v1/agents/Nobody/resume-command").await.status(),
        404
    );
}

/// A renewed session (a new round's lead) is the one that resumes: the agent is the same, the
/// old session stays in the event log, and starting the new one is not a lost-context warning.
#[tokio::test]
async fn a_renewed_session_is_the_one_resumed_and_the_old_one_stays_recorded() {
    let t = TestHub::start_with(native_plugin).await;
    let dir = tempfile::tempdir().unwrap();
    let rex = t.add_native("Rex", "worker", dir.path()).await;
    let mut w = t.watch().await;
    t.send("Rex", "hello").await;
    turn_done(&mut w, rex.id).await;
    let old = t
        .hub
        .bus
        .store()
        .last_session_id(rex.id)
        .await
        .unwrap()
        .expect("the first session");

    assert!(t.hub.runtime.renew(&AgentKey::Id(rex.id)).await.unwrap());
    t.send("Rex", "again").await;
    turn_done(&mut w, rex.id).await;
    let new = t
        .hub
        .bus
        .store()
        .last_session_id(rex.id)
        .await
        .unwrap()
        .expect("the renewed session");
    assert_ne!(new, old);

    let r = resume_command(&t, "Rex").await;
    assert_eq!(r.session_id.as_deref(), Some(new.as_str()), "{r:?}");
    let command = r.command.clone().unwrap_or_else(|| panic!("{r:?}"));
    assert!(command.contains(&format!("--resume '{new}'")), "{command}");

    let after = t.agent("Rex").await;
    assert_eq!(after.id, rex.id);
    assert_eq!(after.name, "Rex");
    let events = t
        .hub
        .bus
        .store()
        .agent_events(rex.id, 0, 10_000)
        .await
        .unwrap();
    let started: Vec<String> = events
        .into_iter()
        .filter_map(|event| match event.kind {
            EventKind::AgentSessionStarted { session_id, .. } => Some(session_id),
            _ => None,
        })
        .collect();
    assert!(started.contains(&old), "{started:?}");
    assert!(started.contains(&new), "{started:?}");
    let warnings = t
        .hub
        .bus
        .store()
        .agent_events(rex.id, 0, 10_000)
        .await
        .unwrap();
    assert!(
        warnings.iter().all(|event| !matches!(
            &event.kind,
            EventKind::AgentWarning { message, .. } if message.contains("没有带上之前的上下文")
        )),
        "a renewed session is not a lost context"
    );
}
