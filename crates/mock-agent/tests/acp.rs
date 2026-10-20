//! The ACP driver against the mock agent process.

use std::time::Duration;

use crossagents_core::{
    AgentSession, OutputStream, PermissionPolicy, SessionEvent, StopReason, ToolStatus,
};
use crossagents_drivers::acp::{AcpLaunch, AcpOptions, AcpSession};
use tokio::sync::mpsc;

fn launch() -> AcpLaunch {
    AcpLaunch {
        program: env!("CARGO_BIN_EXE_crossagents-mock-agent").into(),
        args: vec![],
        env: vec![],
        cwd: std::env::temp_dir(),
    }
}

async fn start(
    permission: PermissionPolicy,
    model: Option<&str>,
    instructions: Option<&str>,
) -> (AcpSession, mpsc::UnboundedReceiver<SessionEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let session = AcpSession::start(AcpOptions {
        launch: launch(),
        mcp_servers: vec![],
        permission,
        model: model.map(Into::into),
        instructions: instructions.map(Into::into),
        events: tx,
    })
    .await
    .unwrap();
    (session, rx)
}

fn drain_text(rx: &mut mpsc::UnboundedReceiver<SessionEvent>) -> (String, Vec<SessionEvent>) {
    let mut text = String::new();
    let mut others = Vec::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            SessionEvent::Output {
                stream: OutputStream::Text,
                text: t,
            } => text.push_str(&t),
            other => others.push(other),
        }
    }
    (text, others)
}

#[tokio::test]
async fn prompt_selects_model_and_mode_and_streams_output() {
    let (session, mut rx) = start(
        PermissionPolicy::Bypass,
        Some("haiku"),
        Some("You are a worker."),
    )
    .await;
    assert_eq!(
        session.agent_info.as_ref().unwrap().name,
        "crossagents-mock-agent"
    );

    let stop = session.prompt("hello").await.unwrap();
    assert_eq!(stop, StopReason::EndTurn);
    let (text, others) = drain_text(&mut rx);
    assert_eq!(
        text,
        "[instructions received] echo(model=mock-haiku,mode=bypassPermissions): hello"
    );
    assert!(
        others
            .iter()
            .any(|e| matches!(e, SessionEvent::Usage { used: 42, .. }))
    );

    // Instructions are only sent once.
    session.prompt("again").await.unwrap();
    let (text, _) = drain_text(&mut rx);
    assert!(!text.contains("instructions"), "{text}");
    session.shutdown().await;
}

#[tokio::test]
async fn unknown_model_is_a_warning() {
    let (session, mut rx) = start(PermissionPolicy::Bypass, Some("gpt-9"), None).await;
    let (_, others) = drain_text(&mut rx);
    assert!(
        others
            .iter()
            .any(|e| matches!(e, SessionEvent::Warning { message } if message.contains("gpt-9"))),
        "{others:?}"
    );
    session.shutdown().await;
}

#[tokio::test]
async fn permissions_follow_policy() {
    let (session, mut rx) = start(PermissionPolicy::Bypass, None, None).await;
    session.prompt("[tool] go").await.unwrap();
    let (text, others) = drain_text(&mut rx);
    assert!(text.starts_with("permission=yes"), "{text}");
    assert!(others.iter().any(|e| matches!(
        e,
        SessionEvent::ToolCallUpdated {
            status: Some(ToolStatus::Completed),
            ..
        }
    )));
    session.shutdown().await;

    let (session, mut rx) = start(PermissionPolicy::Deny, None, None).await;
    session.prompt("[tool] go").await.unwrap();
    let (text, _) = drain_text(&mut rx);
    assert!(text.starts_with("permission=no"), "{text}");
    session.shutdown().await;
}

#[tokio::test]
async fn cancel_stops_a_running_turn() {
    let (session, _rx) = start(PermissionPolicy::Bypass, None, None).await;
    let session = std::sync::Arc::new(session);
    let turn = tokio::spawn({
        let session = session.clone();
        async move { session.prompt("[slow]").await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    session.cancel().await.unwrap();
    let stop = tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(stop, StopReason::Cancelled);
    session.shutdown().await;
}

#[tokio::test]
async fn crash_is_reported() {
    let (session, mut rx) = start(PermissionPolicy::Bypass, None, None).await;
    assert!(session.prompt("[crash]").await.is_err());
    let exited = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(SessionEvent::Exited { .. }) = rx.recv().await {
                return;
            }
        }
    })
    .await;
    assert!(exited.is_ok(), "no Exited event");
}

#[tokio::test]
async fn shutdown_is_not_reported_as_exit() {
    let (session, mut rx) = start(PermissionPolicy::Bypass, None, None).await;
    session.shutdown().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (_, others) = drain_text(&mut rx);
    assert!(
        !others
            .iter()
            .any(|e| matches!(e, SessionEvent::Exited { .. })),
        "{others:?}"
    );
}
