//! Smoke tests of `codex-native` against the real `codex app-server` (docs/DESIGN.md §9.4).
//! Ignored by default: they need a logged-in Codex and spend a little quota.
//!
//! ```text
//! CROSSAGENTS_SMOKE_CODEX_HOME=<a logged-in CODEX_HOME>     required, never defaulted
//! CROSSAGENTS_SMOKE_MODEL=<model>                           optional; set it when that
//!                                                           home's config.toml names a model
//!                                                           the account cannot use
//! CROSSAGENTS_SMOKE_CODEX=<launch command>                  optional, default `codex app-server`
//! cargo test -p crossagents-drivers --test codex_smoke -- --ignored --test-threads=1
//! ```
//!
//! Run them before and after upgrading Codex. Each test leaves a few threads (with a temp
//! working directory) in that CODEX_HOME.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AgentStatus, Driver, DriverSession, OutputStream, PermissionDecision,
    PermissionPolicy, Role, SessionEvent, SessionEventKind, SessionSpec, StopReason, TurnOutcome,
};
use crossagents_drivers::codex_native::CodexNativeDriver;
use serde_json::{Value, json};
use tokio::sync::mpsc;

const TURN_TIMEOUT: Duration = Duration::from_secs(300);

fn codex_home() -> String {
    std::env::var("CROSSAGENTS_SMOKE_CODEX_HOME").expect(
        "set CROSSAGENTS_SMOKE_CODEX_HOME to a logged-in CODEX_HOME; the smoke tests never \
         use ~/.codex on their own",
    )
}

fn driver() -> CodexNativeDriver {
    let command = std::env::var("CROSSAGENTS_SMOKE_CODEX")
        .map(|c| c.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default();
    CodexNativeDriver::new("codex", command, vec![])
}

fn spec(
    cwd: &std::path::Path,
    permission: PermissionPolicy,
    config: Value,
) -> (SessionSpec, mpsc::UnboundedReceiver<SessionEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut config = config;
    config["effort"] = "low".into();
    let spec = SessionSpec {
        agent: Agent {
            id: AgentId::new(),
            name: "smoke".into(),
            plugin: "codex".into(),
            driver: Some("codex-native".into()),
            model: std::env::var("CROSSAGENTS_SMOKE_MODEL").ok(),
            role: Role::Worker,
            cwd: cwd.to_path_buf(),
            config,
            status: AgentStatus::Stopped,
            created_at: Utc::now(),
        },
        mcp_servers: vec![],
        permission,
        permissions: None,
        command: None,
        instructions: Some("You are a test fixture. Answer as briefly as possible.".into()),
        env: vec![("CODEX_HOME".into(), codex_home())],
        events: tx,
    };
    (spec, rx)
}

async fn turn(
    session: &dyn DriverSession,
    rx: &mut mpsc::UnboundedReceiver<SessionEvent>,
    text: &str,
) -> (TurnOutcome, String, Vec<SessionEventKind>) {
    let outcome = tokio::time::timeout(TURN_TIMEOUT, session.prompt(text))
        .await
        .expect("turn timed out")
        .expect("turn failed");
    let mut streamed = String::new();
    let mut others = Vec::new();
    while let Ok(event) = rx.try_recv() {
        match event.kind {
            SessionEventKind::Output {
                stream: OutputStream::Text,
                text,
            } => streamed.push_str(&text),
            other => others.push(other),
        }
    }
    let errors: Vec<_> = others
        .iter()
        .filter(|k| matches!(k, SessionEventKind::Error { .. }))
        .collect();
    assert!(
        outcome.stop_reason != StopReason::Error,
        "turn failed: {errors:?}"
    );
    (outcome, streamed, others)
}

#[tokio::test]
#[ignore = "needs a logged-in Codex"]
async fn smoke_open_and_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let driver = driver();
    let (spec, mut rx) = spec(dir.path(), PermissionPolicy::Bypass, json!({}));
    let session = driver.open(spec).await.unwrap();
    let (outcome, text, events) = turn(&*session, &mut rx, "Reply with exactly: PONG").await;
    assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    assert!(outcome.turn_id.is_some());
    assert!(text.contains("PONG"), "{text}");
    assert!(events.iter().any(|k| matches!(
        k,
        SessionEventKind::Usage { tokens: Some(t), size, .. } if t.total > 0 && *size > 0
    )));
    assert!(
        events
            .iter()
            .any(|k| matches!(k, SessionEventKind::TurnCompleted { .. }))
    );
    session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs a logged-in Codex"]
async fn smoke_permission_request() {
    let dir = tempfile::tempdir().unwrap();
    let driver = driver();
    // Ask before running anything, and refuse.
    let (spec, mut rx) = spec(
        dir.path(),
        PermissionPolicy::Deny,
        json!({ "sandbox": "read-only", "approval": "untrusted" }),
    );
    let session = driver.open(spec).await.unwrap();
    let (_, _, events) = turn(
        &*session,
        &mut rx,
        "Run the shell command `echo smoke > made.txt` in the current directory, then reply DONE.",
    )
    .await;
    assert!(
        events.iter().any(|k| matches!(
            k,
            SessionEventKind::PermissionRequested { request, decision: PermissionDecision::Deny }
                if request.kind == "command"
        )),
        "{events:?}"
    );
    assert!(!dir.path().join("made.txt").exists());
    session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs a logged-in Codex"]
async fn smoke_interrupt() {
    let dir = tempfile::tempdir().unwrap();
    let driver = driver();
    let (spec, mut rx) = spec(dir.path(), PermissionPolicy::Bypass, json!({}));
    let session: Arc<dyn DriverSession> = Arc::from(driver.open(spec).await.unwrap());
    let running = tokio::spawn({
        let session = session.clone();
        async move {
            session
                .prompt("Ignore brevity. Write the numbers 1 to 400, one per line, as plain text.")
                .await
        }
    });
    // Wait for output, then stop the turn.
    tokio::time::timeout(TURN_TIMEOUT, async {
        while let Some(event) = rx.recv().await {
            if matches!(event.kind, SessionEventKind::Output { .. }) {
                return;
            }
        }
    })
    .await
    .expect("no output");
    session.interrupt().await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(30), running)
        .await
        .expect("interrupt did not end the turn")
        .unwrap()
        .unwrap();
    assert_eq!(outcome.stop_reason, StopReason::Cancelled);
    session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs a logged-in Codex"]
async fn smoke_suspend_and_resume_in_a_new_process() {
    let dir = tempfile::tempdir().unwrap();
    let driver = driver();
    let (first, mut rx) = spec(dir.path(), PermissionPolicy::Bypass, json!({}));
    let session = driver.open(first).await.unwrap();
    let thread_id = session.session_id().to_owned();
    turn(
        &*session,
        &mut rx,
        "Remember the code word ZEBRA-42. Reply OK.",
    )
    .await;
    session.suspend().await.unwrap();
    drop(session);
    for _ in 0..100 {
        if driver.process_count().await == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(driver.process_count().await, 0, "app-server still running");

    let (again, mut rx) = spec(dir.path(), PermissionPolicy::Bypass, json!({}));
    let session = driver.resume(&thread_id, again).await.unwrap();
    let (_, text, _) = turn(
        &*session,
        &mut rx,
        "What is the code word I asked you to remember? Reply with just the word.",
    )
    .await;
    assert!(text.contains("ZEBRA-42"), "{text}");
    session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs a logged-in Codex"]
async fn smoke_rollback_and_fork() {
    let dir = tempfile::tempdir().unwrap();
    let fork_dir = tempfile::tempdir().unwrap();
    let driver = driver();
    let (first, mut rx) = spec(dir.path(), PermissionPolicy::Bypass, json!({}));
    let session = driver.open(first).await.unwrap();
    let (t1, _, _) = turn(&*session, &mut rx, "Remember: the color is BLUE. Reply OK.").await;
    let (t2, _, _) = turn(
        &*session,
        &mut rx,
        "Correction: the color is RED, not blue. Reply OK.",
    )
    .await;

    let (fork_spec, mut fork_rx) = spec(fork_dir.path(), PermissionPolicy::Bypass, json!({}));
    let fork = session
        .fork(t1.turn_id.as_deref(), fork_spec)
        .await
        .unwrap();
    let (_, text, _) = turn(
        &*fork,
        &mut fork_rx,
        "What is the color? Reply with just the color.",
    )
    .await;
    assert!(text.to_uppercase().contains("BLUE"), "fork: {text}");

    session
        .rollback(t2.turn_id.as_deref().unwrap(), false)
        .await
        .unwrap();
    let (_, text, _) = turn(
        &*session,
        &mut rx,
        "What is the color? Reply with just the color.",
    )
    .await;
    assert!(text.to_uppercase().contains("BLUE"), "rollback: {text}");
    fork.shutdown().await;
    session.shutdown().await;
}
