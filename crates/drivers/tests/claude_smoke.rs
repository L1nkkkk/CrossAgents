//! Smoke tests of `claude-native` against the real Claude Code (docs/DESIGN.md §9.4).
//! Ignored by default: they need a signed-in Claude Code and spend a little quota.
//!
//! ```text
//! CROSSAGENTS_SMOKE_CLAUDE_CONFIG_DIR=<dir>   optional; the default is Claude Code's own login
//! CROSSAGENTS_SMOKE_MODEL=<model>             optional, default haiku
//! CROSSAGENTS_SMOKE_CLAUDE=<launch command>   optional, default `claude` (or the app's copy)
//! CROSSAGENTS_SMOKE_HOST=<crossagents exe>    optional, enables the hosted smoke test
//! cargo test -p crossagents-drivers --test claude_smoke -- --ignored --test-threads=1
//! ```
//!
//! Behind a proxy the Windows system proxy is passed on; elsewhere set `HTTPS_PROXY`. Each
//! test leaves a few sessions (with a temp working directory) in that login's transcripts.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AgentStatus, Driver, DriverSession, MessageId, OutputStream,
    PermissionDecision, PermissionPolicy, Prompt, Role, SessionEvent, SessionEventKind,
    SessionSpec, StopReason, TurnOutcome,
};
use crossagents_drivers::claude_native::ClaudeNativeDriver;
use crossagents_drivers::locate::Cli;
use serde_json::json;
use tokio::sync::mpsc;

const TURN_TIMEOUT: Duration = Duration::from_secs(300);

#[tokio::test]
#[ignore = "needs signed-in Claude and CROSSAGENTS_SMOKE_HOST"]
async fn smoke_hosted_prompt() {
    let Ok(exe) = std::env::var("CROSSAGENTS_SMOKE_HOST") else {
        eprintln!("CROSSAGENTS_SMOKE_HOST not set; hosted smoke not run");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let hosts = tempfile::tempdir().unwrap();
    crossagents_drivers::host::enable(crossagents_drivers::host::Settings {
        command: vec![exe, "runtime-host".into()],
        dir: hosts.path().into(),
    });
    let (spec, mut rx) = spec(dir.path(), PermissionPolicy::Bypass);
    let session = driver().open(spec).await.unwrap();
    assert!(session.outlives_hub(), "session did not use runtime host");
    let (outcome, text, _) =
        turn(&*session, &mut rx, "Do not use tools. Reply exactly PONG.").await;
    assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    assert!(text.contains("PONG"), "{text}");
    session.shutdown().await;
    crossagents_drivers::host::disable();
}

fn driver() -> ClaudeNativeDriver {
    let command = std::env::var("CROSSAGENTS_SMOKE_CLAUDE")
        .map(|c| c.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default();
    ClaudeNativeDriver::new("claude", Cli::at(command), vec![])
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
            plugin: "claude".into(),
            driver: Some("claude-native".into()),
            model: Some(
                std::env::var("CROSSAGENTS_SMOKE_MODEL").unwrap_or_else(|_| "haiku".into()),
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
        env: std::env::var("CROSSAGENTS_SMOKE_CLAUDE_CONFIG_DIR")
            .map(|dir| vec![("CLAUDE_CONFIG_DIR".to_owned(), dir)])
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
#[ignore = "needs a signed-in Claude Code"]
async fn smoke_prompt_tools_usage_and_quota() {
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
                if detail.changes.iter().any(|c| c.path.ends_with("note.txt") && c.diff.contains("+v1"))
        )),
        "{events:?}"
    );
    assert!(events.iter().any(|k| matches!(
        k,
        SessionEventKind::Usage { tokens: Some(t), size, cost_usd: Some(_), .. } if t.total > 0 && *size > 0
    )));
    // The turn's rate_limit_event and the full read after it.
    assert!(
        events.iter().any(|k| matches!(
            k,
            SessionEventKind::RateLimits { limits } if limits.plan.is_some() && !limits.windows.is_empty()
        )),
        "{events:?}"
    );
    session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs a signed-in Claude Code"]
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
#[ignore = "needs a signed-in Claude Code"]
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
#[ignore = "needs a signed-in Claude Code"]
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

/// A message written while a tool runs reaches the model in the same turn: its read is
/// reported, it changes the answer, and the turn ends once.
#[tokio::test]
#[ignore = "needs a signed-in Claude Code"]
async fn smoke_steer_during_a_tool() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, mut rx) = spec(dir.path(), PermissionPolicy::Bypass);
    let session: Arc<dyn DriverSession> = Arc::from(driver().open(spec).await.unwrap());
    let running = tokio::spawn({
        let session = session.clone();
        async move {
            session
                .prompt("Use the Bash tool to run `sleep 8`, then answer with the word FINISHED.")
                .await
        }
    });
    let id = MessageId::new();
    let prompt = Prompt {
        text: "Change of plan: answer with the single word STEERED instead.".into(),
        attachments: vec![],
    };
    let (mut steered, mut read, mut text) = (false, false, String::new());
    tokio::time::timeout(TURN_TIMEOUT, async {
        while !running.is_finished() {
            let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await
            else {
                continue;
            };
            match event.kind {
                SessionEventKind::ToolCall { .. } if !steered => {
                    steered = true;
                    session.steer(id, &prompt).await.unwrap();
                }
                SessionEventKind::SteerRead { message_id } => {
                    assert_eq!(message_id, id);
                    read = true;
                }
                SessionEventKind::Output {
                    stream: OutputStream::Text,
                    text: more,
                } if read => text.push_str(&more),
                _ => {}
            }
        }
    })
    .await
    .expect("turn timed out");
    while let Ok(event) = rx.try_recv() {
        match event.kind {
            SessionEventKind::SteerRead { .. } => read = true,
            SessionEventKind::Output {
                stream: OutputStream::Text,
                text: more,
            } if read => text.push_str(&more),
            _ => {}
        }
    }
    let outcome = running.await.unwrap().unwrap();
    assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    assert!(steered, "no tool call to steer into");
    assert!(read, "the read was never reported");
    assert!(
        text.contains("STEERED"),
        "the answer did not change: {text}"
    );
    session.shutdown().await;
}

/// A message written once the final answer has started gets a CLI turn of its own, with a
/// `result` of its own. That result stays with this turn: the next one ends on its own.
#[tokio::test]
#[ignore = "needs a signed-in Claude Code"]
async fn smoke_steer_too_late_leaves_no_orphan_result() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, mut rx) = spec(dir.path(), PermissionPolicy::Bypass);
    let session: Arc<dyn DriverSession> = Arc::from(driver().open(spec).await.unwrap());
    let running = tokio::spawn({
        let session = session.clone();
        async move { session.prompt("Answer with the word ONE.").await }
    });
    let id = MessageId::new();
    let prompt = Prompt {
        text: "Also answer with the word TWO.".into(),
        attachments: vec![],
    };
    let (mut steered, mut read) = (false, false);
    tokio::time::timeout(TURN_TIMEOUT, async {
        while !running.is_finished() {
            let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await
            else {
                continue;
            };
            match event.kind {
                SessionEventKind::Output {
                    stream: OutputStream::Text,
                    ..
                } if !steered => {
                    steered = true;
                    session.steer(id, &prompt).await.unwrap();
                }
                SessionEventKind::SteerRead { .. } => read = true,
                _ => {}
            }
        }
    })
    .await
    .expect("turn timed out");
    while let Ok(event) = rx.try_recv() {
        if matches!(event.kind, SessionEventKind::SteerRead { .. }) {
            read = true;
        }
    }
    let outcome = running.await.unwrap().unwrap();
    assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    assert!(steered, "no output to steer after");
    assert!(read, "the read was never reported");
    let (next, text, _) = turn(&*session, &mut rx, "Answer with the word THREE.").await;
    assert_eq!(next.stop_reason, StopReason::EndTurn);
    assert!(text.contains("THREE"), "the next turn got: {text}");
    session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs a signed-in Claude Code"]
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
    session.set_model(Some("haiku"), Some("low")).await.unwrap();
    let (_, text, _) = turn(
        &*session,
        &mut rx,
        "Without using tools: what is the color now? Reply with just the color.",
    )
    .await;
    assert!(text.to_uppercase().contains("RED"), "resume: {text}");
    session.shutdown().await;
}
