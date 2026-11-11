//! Smoke tests of `with-native` against the real `with-cli` (docs/DESIGN.md §9.4,
//! docs/research/with-cli.md). Ignored by default: they need With's CLI, signed in through
//! the With app, and Node.js for the MCP test; they spend a little quota.
//!
//! ```text
//! CROSSAGENTS_SMOKE_MODEL=<model>        optional, default claude-haiku-5.5
//! CROSSAGENTS_SMOKE_WITH=<launch command> optional, default `with-cli` where With puts it
//! cargo test -p crossagents-drivers --test with_smoke -- --ignored --test-threads=1
//! ```
//!
//! Each test leaves a few conversations (with a temp working directory) in that account's
//! history.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AgentStatus, Driver, DriverSession, McpServerSpec, OutputStream,
    PermissionPolicy, Role, SessionEvent, SessionEventKind, SessionSpec, StopReason, ToolStatus,
    TurnOutcome,
};
use crossagents_drivers::locate::{Cli, Locator};
use crossagents_drivers::with_native::WithNativeDriver;
use serde_json::json;
use tokio::sync::mpsc;

const TURN_TIMEOUT: Duration = Duration::from_secs(300);

fn driver() -> WithNativeDriver {
    let command: Vec<String> = std::env::var("CROSSAGENTS_SMOKE_WITH")
        .map(|c| c.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default();
    let locator: Locator = toml::from_str(
        r#"commands = ["with-cli"]
           paths = ["~/background_agent_cli/bin/with-cli"]"#,
    )
    .unwrap();
    WithNativeDriver::new(
        "with",
        Cli::new(command, locator, Default::default()),
        vec![],
    )
}

fn spec(
    cwd: &Path,
    mcp_servers: Vec<McpServerSpec>,
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
            plugin: "with".into(),
            driver: Some("with-native".into()),
            model: Some(
                std::env::var("CROSSAGENTS_SMOKE_MODEL")
                    .unwrap_or_else(|_| "claude-haiku-5.5".into()),
            ),
            role: Role::Worker,
            cwd: cwd.to_path_buf(),
            config: json!({ "effort": "low" }),
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
#[ignore = "needs with-cli signed in through the With app"]
async fn smoke_tools_rules_mcp_and_usage() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, mut rx) = spec(dir.path(), vec![probe_server(dir.path())]);
    let session = driver().open(spec).await.unwrap();
    assert!(matches!(
        rx.try_recv().map(|e| e.kind),
        Ok(SessionEventKind::Started { .. })
    ));

    let (outcome, _, events) = turn(
        &*session,
        &mut rx,
        "Create the file note.txt in the workspace containing exactly v1, then run the \
         terminal command `echo hi`. Reply with exactly: DONE",
    )
    .await;
    assert!(outcome.turn_id.is_some());
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
            SessionEventKind::ToolDetail { detail, .. }
                if detail.command.as_deref().is_some_and(|c| c.contains("echo"))
                    && detail.output.as_deref().is_some_and(|o| o.contains("hi"))
        )),
        "{events:?}"
    );
    assert!(
        !events.iter().any(|k| matches!(
            k,
            SessionEventKind::ToolCallUpdated {
                status: Some(ToolStatus::Failed),
                ..
            }
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

    let (_, text, _) = turn(&*session, &mut rx, "What is the fixture word?").await;
    assert!(text.contains("BLUEBERRY"), "{text}");

    let (_, text, _) = turn(
        &*session,
        &mut rx,
        "Call the probe_ping MCP tool and reply with exactly what it returned.",
    )
    .await;
    assert!(text.contains("banana77"), "{text}");
    session.shutdown().await;
}

#[tokio::test]
#[ignore = "needs with-cli signed in through the With app"]
async fn smoke_resume_and_model_switch() {
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
    session.suspend().await.unwrap();
    drop(session);

    let (spec_b, mut rx) = spec(dir.path(), vec![]);
    let resumed = driver.resume(&id, spec_b).await.unwrap();
    assert_eq!(resumed.session_id(), id);
    resumed
        .set_model(Some("claude-sonnet-5.5"), Some("low"))
        .await
        .unwrap();
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
#[ignore = "needs with-cli signed in through the With app"]
async fn smoke_interrupt() {
    let dir = tempfile::tempdir().unwrap();
    let (spec, mut rx) = spec(dir.path(), vec![]);
    let session: std::sync::Arc<dyn DriverSession> = driver().open(spec).await.unwrap().into();
    let running = tokio::spawn({
        let session = session.clone();
        async move {
            session
                .prompt("Run the terminal command `Start-Sleep -Seconds 120` and then reply SLEPT.")
                .await
        }
    });
    // Wait for the command to start.
    let started = tokio::time::timeout(TURN_TIMEOUT, async {
        while let Some(event) = rx.recv().await {
            if matches!(
                &event.kind,
                SessionEventKind::ToolCall { kind: Some(kind), .. } if kind == "execute"
            ) {
                return;
            }
        }
    })
    .await;
    assert!(started.is_ok(), "the command never started");
    tokio::time::sleep(Duration::from_secs(2)).await;
    session.interrupt().await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(60), running)
        .await
        .expect("the turn did not stop")
        .unwrap()
        .unwrap();
    assert_eq!(outcome.stop_reason, StopReason::Cancelled);
    session.shutdown().await;
}
