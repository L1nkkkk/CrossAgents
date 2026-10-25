//! Smoke tests of `codebuddy-native` against the real CodeBuddy CLI (docs/DESIGN.md §9.4).
//! Ignored by default: they need the standalone CLI (`npm install -g
//! @tencent-ai/codebuddy-code`), signed in with `/login`, and spend a little quota.
//!
//! ```text
//! CROSSAGENTS_SMOKE_MODEL=<model>                optional, default the account's default
//! CROSSAGENTS_SMOKE_CODEBUDDY=<launch command>   optional, default `codebuddy` on PATH
//! cargo test -p crossagents-drivers --test codebuddy_smoke -- --ignored --test-threads=1
//! ```
//!
//! Each test leaves a few sessions (with a temp working directory) in that login's history.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AgentStatus, Driver, DriverSession, OutputStream, PermissionDecision,
    PermissionPolicy, Role, SessionEvent, SessionEventKind, SessionSpec, StopReason, TurnOutcome,
};
use crossagents_drivers::codebuddy_native::CodebuddyNativeDriver;
use serde_json::json;
use tokio::sync::mpsc;

const TURN_TIMEOUT: Duration = Duration::from_secs(300);

fn driver() -> CodebuddyNativeDriver {
    let command = std::env::var("CROSSAGENTS_SMOKE_CODEBUDDY")
        .map(|c| c.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default();
    CodebuddyNativeDriver::new("codebuddy", command, vec![])
}

fn spec(
    cwd: &std::path::Path,
    permission: PermissionPolicy,
) -> (SessionSpec, mpsc::UnboundedReceiver<SessionEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let spec = SessionSpec {
        agent: Agent {
            id: AgentId::new(),
            name_source: crossagents_core::NameSource::Chosen,
            topic: None,
            archived_at: None,
            name: "smoke".into(),
            plugin: "codebuddy".into(),
            driver: Some("codebuddy-native".into()),
            model: std::env::var("CROSSAGENTS_SMOKE_MODEL").ok(),
            role: Role::Worker,
            cwd: cwd.to_path_buf(),
            config: json!({}),
            status: AgentStatus::Stopped,
            created_at: Utc::now(),
            profile: Default::default(),
        },
        mcp_servers: vec![],
        permission,
        permissions: None,
        command: None,
        instructions: Some("You are a test fixture. Answer as briefly as possible.".into()),
        env: vec![],
        events: tx,
    };
    (spec, rx)
}

/// A turn's outcome, streamed text and other events, without asserting it succeeded.
async fn run(
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
    (outcome, streamed, others)
}

async fn turn(
    session: &dyn DriverSession,
    rx: &mut mpsc::UnboundedReceiver<SessionEvent>,
    text: &str,
) -> (TurnOutcome, String, Vec<SessionEventKind>) {
    let (outcome, streamed, others) = run(session, rx, text).await;
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
#[ignore = "needs a signed-in CodeBuddy CLI"]
async fn smoke_prompt_tools_and_usage() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, mut rx) = spec(dir.path(), PermissionPolicy::Bypass);
    let session = driver().open(spec).await.unwrap();
    let (outcome, text, events) = turn(
        &*session,
        &mut rx,
        "Create the file note.txt containing exactly v1, then reply with exactly: PONG",
    )
    .await;
    assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    assert!(text.contains("PONG"), "{text}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("note.txt"))
            .unwrap()
            .trim(),
        "v1"
    );
    assert!(
        events.iter().any(|k| matches!(
            k,
            SessionEventKind::ToolDetail { detail, .. }
                if detail.changes.iter().any(|c| c.path.ends_with("note.txt"))
        )),
        "{events:?}"
    );
    assert!(
        events.iter().any(|k| matches!(
            k,
            SessionEventKind::Usage { tokens: Some(t), .. } if t.total > 0
        )),
        "{events:?}"
    );
    session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs a signed-in CodeBuddy CLI"]
async fn smoke_permission_request_denied() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, mut rx) = spec(dir.path(), PermissionPolicy::Deny);
    let session = driver().open(spec).await.unwrap();
    let (_, _, events) = turn(
        &*session,
        &mut rx,
        "Create the file made.txt containing smoke in the current directory with your file \
         writing tool, then reply DONE.",
    )
    .await;
    assert!(
        events.iter().any(|k| matches!(
            k,
            SessionEventKind::PermissionRequested {
                decision: PermissionDecision::Deny,
                ..
            }
        )),
        "{events:?}"
    );
    assert!(!dir.path().join("made.txt").exists());
    session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs a signed-in CodeBuddy CLI"]
async fn smoke_permission_request_allowed() {
    // Asked and allowed, the write goes through: an answer the CLI misreads fails here,
    // where the denied case cannot tell (CodeBuddy once read every answer as a denial).
    let dir = tempfile::tempdir().unwrap();
    let (spec, mut rx) = spec(dir.path(), PermissionPolicy::Guard);
    let session = driver().open(spec).await.unwrap();
    let (_, _, events) = turn(
        &*session,
        &mut rx,
        "Create the file made.txt containing smoke in the current directory with your file          writing tool, then reply DONE.",
    )
    .await;
    assert!(
        events.iter().any(|k| matches!(
            k,
            SessionEventKind::PermissionRequested {
                decision: PermissionDecision::Allow,
                ..
            }
        )),
        "{events:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("made.txt"))
            .unwrap_or_default()
            .trim(),
        "smoke",
        "{events:?}"
    );
    session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs a signed-in CodeBuddy CLI"]
async fn smoke_interrupt() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, mut rx) = spec(dir.path(), PermissionPolicy::Bypass);
    let session: Arc<dyn DriverSession> = Arc::from(driver().open(spec).await.unwrap());
    let running = tokio::spawn({
        let session = session.clone();
        async move {
            session
                .prompt("Ignore brevity. Write the numbers 1 to 400, one per line, as plain text.")
                .await
        }
    });
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
#[ignore = "needs a signed-in CodeBuddy CLI"]
async fn smoke_suspend_resume_and_fork() {
    let dir = tempfile::tempdir().unwrap();
    let fork_dir = tempfile::tempdir().unwrap();
    let driver = driver();
    let (first, mut rx) = spec(dir.path(), PermissionPolicy::Bypass);
    let session = driver.open(first).await.unwrap();
    let id = session.session_id().to_owned();
    let (t1, _, _) = turn(&*session, &mut rx, "Remember: the color is BLUE. Reply OK.").await;
    turn(
        &*session,
        &mut rx,
        "Correction: the color is RED, not blue. Reply OK.",
    )
    .await;

    // A fork after the first turn has not heard the correction.
    let (fork_spec, mut fork_rx) = spec(fork_dir.path(), PermissionPolicy::Bypass);
    let fork = session
        .fork(t1.turn_id.as_deref(), fork_spec)
        .await
        .unwrap();
    assert_ne!(fork.session_id(), id);
    let (_, text, _) = turn(
        &*fork,
        &mut fork_rx,
        "Without using tools: what is the color? Reply with just the color.",
    )
    .await;
    assert!(text.to_uppercase().contains("BLUE"), "fork: {text}");
    fork.shutdown().await;

    // The original, in a new process.
    session.suspend().await.unwrap();
    drop(session);
    let (again, mut rx) = spec(dir.path(), PermissionPolicy::Bypass);
    let session = driver.resume(&id, again).await.unwrap();
    assert_eq!(session.session_id(), id);
    let (_, text, _) = turn(
        &*session,
        &mut rx,
        "Without using tools: what is the color now? Reply with just the color.",
    )
    .await;
    assert!(text.to_uppercase().contains("RED"), "resume: {text}");
    session.shutdown().await;
}

/// Not signed in (an empty home), the turn ends at once with the CLI's sign-in error
/// instead of waiting.
#[tokio::test]
#[ignore = "needs the CodeBuddy CLI"]
async fn smoke_signed_out_fails_fast() {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (mut spec, mut rx) = spec(dir.path(), PermissionPolicy::Bypass);
    let home = home.path().display().to_string();
    spec.env = vec![("USERPROFILE".into(), home.clone()), ("HOME".into(), home)];
    let session = driver().open(spec).await.unwrap();
    let (outcome, _, events) = tokio::time::timeout(
        Duration::from_secs(60),
        run(&*session, &mut rx, "Reply PONG"),
    )
    .await
    .expect("a signed-out CLI should not hang");
    assert_eq!(outcome.stop_reason, StopReason::Error);
    assert!(
        events.iter().any(|k| matches!(
            k,
            SessionEventKind::Error { message } if message.contains("Authentication")
        )),
        "{events:?}"
    );
    session.shutdown().await;
}
