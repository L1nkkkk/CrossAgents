//! Smoke tests of `qoder-native` against the real Qoder CLI (`qodercli`,
//! docs/research/qoder-cli.md). Ignored by default: they need a signed-in `qodercli` and
//! spend a little quota.
//!
//! ```text
//! CROSSAGENTS_SMOKE_QODER_CONFIG_DIR=<dir>   optional; defaults to `~/.qoder`
//! CROSSAGENTS_SMOKE_QODER_MODEL=<model>      optional, default `lite`
//! CROSSAGENTS_SMOKE_QODER=<launch command>   optional, default `qodercli`
//! cargo test -p crossagents-drivers --test qoder_smoke -- --ignored --test-threads=1
//! ```
//!
//! Compile only: `cargo test -p crossagents-drivers --test qoder_smoke --no-run`.

use std::time::Duration;

use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AgentStatus, Driver, DriverSession, OutputStream, PermissionDecision,
    PermissionPolicy, Role, SessionEvent, SessionEventKind, SessionSpec, StopReason, TurnOutcome,
};
use crossagents_drivers::locate::Cli;
use crossagents_drivers::qoder_native::QoderNativeDriver;
use serde_json::json;
use tokio::sync::mpsc;

const TURN_TIMEOUT: Duration = Duration::from_secs(300);

fn driver() -> QoderNativeDriver {
    let command = std::env::var("CROSSAGENTS_SMOKE_QODER")
        .map(|c| c.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_else(|_| vec!["qodercli".into()]);
    QoderNativeDriver::new("qoder", Cli::at(command), vec![])
}

fn spec(
    cwd: &std::path::Path,
    permission: PermissionPolicy,
) -> (SessionSpec, mpsc::UnboundedReceiver<SessionEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let spec = SessionSpec {
        agent: Agent {
            node: None,
            id: AgentId::new(),
            name_source: crossagents_core::NameSource::Chosen,
            topic: None,
            archived_at: None,
            home: std::path::PathBuf::new(),
            name: "smoke".into(),
            plugin: "qoder".into(),
            driver: Some("qoder-native".into()),
            model: Some(
                std::env::var("CROSSAGENTS_SMOKE_QODER_MODEL").unwrap_or_else(|_| "lite".into()),
            ),
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
        env: std::env::var("CROSSAGENTS_SMOKE_QODER_CONFIG_DIR")
            .map(|dir| vec![("QODER_CONFIG_DIR".to_owned(), dir)])
            .unwrap_or_default(),
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
#[ignore = "needs a signed-in qodercli"]
async fn smoke_prompt_tools_usage_and_context() {
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
            SessionEventKind::Usage { tokens: Some(t), size, .. } if t.total > 0 && *size > 0
        )),
        "{events:?}"
    );
    session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs a signed-in qodercli"]
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
            SessionEventKind::PermissionRequested { request, decision: PermissionDecision::Deny }
                if request.kind == "file_change"
        )),
        "{events:?}"
    );
    assert!(!dir.path().join("made.txt").exists());
    session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs a signed-in qodercli"]
async fn smoke_resume_and_move_cwd() {
    let dir1 = tempfile::tempdir().unwrap();
    let dir2 = tempfile::tempdir().unwrap();
    let (spec1, mut rx1) = spec(dir1.path(), PermissionPolicy::Bypass);
    let session = driver().open(spec1).await.unwrap();
    let id = session.session_id().to_owned();
    let (_, _, _) = turn(
        &*session,
        &mut rx1,
        "Remember the codeword AMBER-42. Do not use tools. Reply OK.",
    )
    .await;
    session.suspend().await.unwrap();

    let (spec2, mut rx2) = spec(dir2.path(), PermissionPolicy::Bypass);
    let resumed = driver().resume(&id, spec2).await.unwrap();
    let (_, text, _) = turn(
        &*resumed,
        &mut rx2,
        "What was the codeword? Do not use tools. Reply with just the codeword.",
    )
    .await;
    assert!(text.contains("AMBER-42"), "{text}");
    resumed.shutdown().await;
}

#[tokio::test]
#[ignore = "needs a signed-in qodercli"]
async fn smoke_set_model_and_effort() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, mut rx) = spec(dir.path(), PermissionPolicy::Bypass);
    let session = driver().open(spec).await.unwrap();
    session.set_model(Some("efficient"), Some("low")).await.unwrap();
    let (outcome, text, _) = turn(&*session, &mut rx, "Do not use tools. Reply OK.").await;
    assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    assert!(text.contains("OK"), "{text}");
    session.shutdown().await;
}
