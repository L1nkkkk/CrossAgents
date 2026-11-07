//! Smoke tests against the real Cursor CLI. All tests are ignored by default.
//! Requires an installed, signed-in Cursor CLI; the MCP test also needs Node.js.
//! These tests spend quota and leave conversations with temporary workspaces in history.
//! Install the CLI only with the user's approval; compilation needs no CLI.
//!
//! ```text
//! CROSSAGENTS_SMOKE_CURSOR=<launch command> optional, default agent
//! CROSSAGENTS_SMOKE_MODEL=<model>           optional, default auto
//! CROSSAGENTS_SMOKE_NEXT_MODEL=<model>      optional model-switch target (see below)
//! cargo test -p crossagents-drivers --test cursor_smoke -- --ignored --test-threads=1
//! ```
//!
//! Launch commands are split on whitespace, as in with_smoke.rs (no shell quoting).
//! The switch target defaults to sonnet when the initial model is auto, otherwise auto;
//! set NEXT_MODEL to a different model available to your account when necessary.
//! Compile only: `cargo test -p crossagents-drivers --test cursor_smoke --no-run`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AgentStatus, Driver, DriverSession, McpServerSpec, OutputStream,
    PermissionPolicy, Role, SessionEvent, SessionEventKind, SessionSpec, StopReason, TurnOutcome,
};
use crossagents_drivers::cursor_native::CursorNativeDriver;
use crossagents_drivers::locate::Cli;
use serde_json::json;
use tokio::sync::mpsc;

const TURN_TIMEOUT: Duration = Duration::from_secs(300);

fn driver() -> CursorNativeDriver {
    let command = std::env::var("CROSSAGENTS_SMOKE_CURSOR")
        .unwrap_or_else(|_| "agent".into())
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    CursorNativeDriver::new("cursor", Cli::at(command), vec![])
}

fn spec(
    cwd: &Path,
    mcp_servers: Vec<McpServerSpec>,
) -> (SessionSpec, mpsc::UnboundedReceiver<SessionEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let spec = SessionSpec {
        agent: Agent {
            id: AgentId::new(),
            name_source: crossagents_core::NameSource::Chosen,
            topic: None,
            archived_at: None,
            home: std::path::PathBuf::new(),
            name: "smoke".into(),
            plugin: "cursor".into(),
            driver: Some("cursor-native".into()),
            model: Some(std::env::var("CROSSAGENTS_SMOKE_MODEL").unwrap_or_else(|_| "auto".into())),
            role: Role::Worker,
            cwd: cwd.to_path_buf(),
            config: json!({}),
            status: AgentStatus::Stopped,
            created_at: Utc::now(),
            profile: Default::default(),
        },
        mcp_servers,
        permission: PermissionPolicy::Bypass,
        permissions: None,
        command: None,
        instructions: Some(
            "You are a test fixture. Answer as briefly as possible. When asked for the \
             fixture word, answer BLUEBERRY."
                .into(),
        ),
        env: vec![],
        events: tx,
    };
    (spec, rx)
}

fn drain(rx: &mut mpsc::UnboundedReceiver<SessionEvent>) -> (String, Vec<SessionEventKind>) {
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
    (streamed, others)
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
    let (streamed, others) = drain(rx);
    assert_eq!(
        outcome.stop_reason,
        StopReason::EndTurn,
        "{streamed} {others:?}"
    );
    (outcome, streamed, others)
}

/// A stdio MCP server with one tool, `probe_ping`, that answers with `PROBE_SECRET`.
fn probe_server(dir: &Path) -> McpServerSpec {
    let script = dir.join("probe.js");
    std::fs::write(
        &script,
        r#"let buf = '';
const send = m => process.stdout.write(JSON.stringify(m) + '\n');
process.stdin.on('data', d => {
  buf += d; let i;
  while ((i = buf.indexOf('\n')) >= 0) {
    const line = buf.slice(0, i).trim(); buf = buf.slice(i + 1);
    if (!line) continue;
    const m = JSON.parse(line);
    if (m.method === 'initialize') send({ jsonrpc: '2.0', id: m.id, result: { protocolVersion: m.params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: 'probe', version: '0' } } });
    else if (m.method === 'tools/list') send({ jsonrpc: '2.0', id: m.id, result: { tools: [{ name: 'probe_ping', description: 'Returns the probe secret.', inputSchema: { type: 'object', properties: {} } }] } });
    else if (m.method === 'tools/call') send({ jsonrpc: '2.0', id: m.id, result: { content: [{ type: 'text', text: 'secret=' + process.env.PROBE_SECRET }] } });
    else if (m.id != null) send({ jsonrpc: '2.0', id: m.id, result: {} });
  }
});
"#,
    )
    .unwrap();
    McpServerSpec {
        name: "probe".into(),
        command: PathBuf::from("node"),
        args: vec![script.display().to_string()],
        env: BTreeMap::from([("PROBE_SECRET".to_owned(), "banana77".to_owned())]),
    }
}

#[tokio::test]
#[ignore = "needs an installed, signed-in Cursor CLI"]
async fn smoke_text_and_opening_instructions() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, mut rx) = spec(dir.path(), vec![]);
    let session = driver().open(spec).await.unwrap();
    assert!(matches!(
        rx.try_recv().map(|e| e.kind),
        Ok(SessionEventKind::Started { session_id }) if session_id == session.session_id()
    ));
    let (_, text, _) = turn(&*session, &mut rx, "What is the fixture word?").await;
    assert!(text.contains("BLUEBERRY"), "{text}");
    session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs an installed, signed-in Cursor CLI"]
async fn smoke_resume_across_processes() {
    let dir = tempfile::tempdir().unwrap();
    let driver = driver();
    let (spec_a, mut rx) = spec(dir.path(), vec![]);
    let session = driver.open(spec_a).await.unwrap();
    turn(
        &*session,
        &mut rx,
        "Remember the code word OKAPI-31. Reply with exactly: OK",
    )
    .await;
    let id = session.session_id().to_owned();
    // Cursor's per-turn process has exited; discard the in-memory session too.
    drop(session);

    let (mut spec_b, mut rx) = spec(dir.path(), vec![]);
    spec_b.instructions = None;
    let resumed = driver.resume(&id, spec_b).await.unwrap();
    assert_eq!(resumed.session_id(), id);
    let (_, text, _) = turn(
        &*resumed,
        &mut rx,
        "What was the code word? Reply with it only.",
    )
    .await;
    assert!(text.contains("OKAPI-31"), "{text}");
    resumed.shutdown().await;
}

#[tokio::test]
#[ignore = "needs an installed, signed-in Cursor CLI and two available model choices"]
async fn smoke_model_switch() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, mut rx) = spec(dir.path(), vec![]);
    let initial = spec.agent.model.as_deref().unwrap();
    let next = std::env::var("CROSSAGENTS_SMOKE_NEXT_MODEL")
        .unwrap_or_else(|_| if initial == "auto" { "sonnet" } else { "auto" }.into());
    assert!(!next.trim().is_empty(), "switch target must not be empty");
    assert_ne!(initial, next, "choose a different model for the switch");
    let session = driver().open(spec).await.unwrap();
    turn(&*session, &mut rx, "Reply with exactly: READY").await;
    let id = session.session_id().to_owned();
    session.set_model(Some(&next), None).await.unwrap();
    let (_, text, _) = turn(&*session, &mut rx, "What is the fixture word?").await;
    assert!(text.contains("BLUEBERRY"), "{text}");
    assert_eq!(session.session_id(), id);
    session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs an installed, signed-in Cursor CLI and Node.js"]
async fn smoke_plugin_stdio_mcp_env() {
    let dir = tempfile::tempdir().unwrap();
    // CursorNativeDriver mounts this server using its ephemeral --plugin-dir.
    let (spec, mut rx) = spec(dir.path(), vec![probe_server(dir.path())]);
    let session = driver().open(spec).await.unwrap();
    let (_, text, events) = turn(
        &*session,
        &mut rx,
        "Call the probe_ping MCP tool and reply with exactly what it returned.",
    )
    .await;
    assert!(text.contains("secret=banana77"), "{text} {events:?}");
    assert!(
        events.iter().any(|event| matches!(
            event,
            SessionEventKind::ToolCall { title, .. } if title.contains("probe_ping")
        )),
        "probe_ping was not called: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            SessionEventKind::ToolDetail { detail, .. }
                if detail.result.as_deref().is_some_and(|r| r.contains("secret=banana77"))
        )),
        "MCP result did not contain the env secret: {events:?}"
    );
    session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs an installed, signed-in Cursor CLI"]
async fn smoke_interrupt_kills_process() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, mut rx) = spec(dir.path(), vec![]);
    let session: std::sync::Arc<dyn DriverSession> = driver().open(spec).await.unwrap().into();
    let running = tokio::spawn({
        let session = session.clone();
        async move {
            let prompt = if cfg!(windows) {
                "Run the terminal command `powershell -NoProfile -Command \"Start-Sleep -Seconds 120\"` and then reply SLEPT."
            } else {
                "Run the terminal command `sleep 120` and then reply SLEPT."
            };
            session.prompt(prompt).await
        }
    });
    // ToolDetail carries shell arguments; Cursor classifies shell tools as `other`.
    let started = tokio::time::timeout(TURN_TIMEOUT, async {
        while let Some(event) = rx.recv().await {
            match event.kind {
                SessionEventKind::ToolDetail { detail, .. }
                    if detail
                        .arguments
                        .as_deref()
                        .is_some_and(|a| a.contains("120")) =>
                {
                    return true;
                }
                SessionEventKind::TurnCompleted { .. } => return false,
                _ => {}
            }
        }
        false
    })
    .await;
    session.interrupt().await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(60), running)
        .await
        .expect("the interrupted process did not stop")
        .unwrap()
        .unwrap();
    assert!(
        matches!(started, Ok(true)),
        "sleep tool never started: {started:?}"
    );
    assert_eq!(outcome.stop_reason, StopReason::Cancelled);
    let (_, events) = drain(&mut rx);
    assert!(
        events.iter().any(|event| matches!(
            event,
            SessionEventKind::TurnCompleted {
                stop_reason: StopReason::Cancelled,
                ..
            }
        )),
        "{events:?}"
    );
    session.shutdown().await;
}

// Both installed CLI versions emit a `usage` object on the terminal `result`
// stream-json event; the driver turns it into an `agent.usage` event. This smoke
// proves that `result.usage` channel end-to-end against a signed-in CLI.
#[tokio::test]
#[ignore = "needs an installed, signed-in Cursor CLI"]
async fn smoke_reports_token_usage() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, mut rx) = spec(dir.path(), vec![]);
    let session = driver().open(spec).await.unwrap();
    let (_, streamed, events) = turn(&*session, &mut rx, "Reply with exactly: READY").await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            SessionEventKind::Usage {
                tokens: Some(tokens),
                ..
            } if tokens.total > 0
        )),
        "no agent.usage with tokens.total>0: {streamed} {events:?}"
    );
    session.shutdown().await;
}
