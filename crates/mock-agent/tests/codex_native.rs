//! The `codex-native` driver against the mock app-server (`crossagents-mock-agent
//! app-server`). No Codex involved; every CODEX_HOME is a temp dir.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AgentStatus, CoreError, Driver, DriverSession, McpServerSpec, OutputStream,
    PermissionDecision, PermissionHandler, PermissionPolicy, PermissionRequest, Role, SessionEvent,
    SessionEventKind, SessionSpec, StopReason, ToolStatus, TurnOutcome,
};
use crossagents_drivers::codex_native::CodexNativeDriver;
use serde_json::{Value, json};
use tokio::sync::mpsc;

const MOCK: &str = env!("CARGO_BIN_EXE_crossagents-mock-agent");

fn driver() -> CodexNativeDriver {
    CodexNativeDriver::new("codex", vec![MOCK.into(), "app-server".into()], vec![])
}

struct Opts<'a> {
    home: &'a Path,
    cwd: &'a Path,
    permission: PermissionPolicy,
    permissions: Option<Arc<dyn PermissionHandler>>,
    config: Value,
}

impl<'a> Opts<'a> {
    fn new(home: &'a Path, cwd: &'a Path) -> Self {
        Self {
            home,
            cwd,
            permission: PermissionPolicy::Bypass,
            permissions: None,
            config: json!({}),
        }
    }
}

fn spec(opts: Opts<'_>) -> (SessionSpec, mpsc::UnboundedReceiver<SessionEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let agent = Agent {
        id: AgentId::new(),
        name: "w1".into(),
        plugin: "codex".into(),
        driver: Some("codex-native".into()),
        model: Some("mock-codex".into()),
        role: Role::Worker,
        cwd: opts.cwd.to_path_buf(),
        config: opts.config,
        status: AgentStatus::Stopped,
        created_at: Utc::now(),
    };
    let spec = SessionSpec {
        agent,
        mcp_servers: vec![McpServerSpec {
            name: "crossagents".into(),
            command: "crossagents".into(),
            args: vec!["mcp".into()],
            env: BTreeMap::from([("CROSSAGENTS_AGENT_TOKEN".into(), "t".into())]),
        }],
        permission: opts.permission,
        permissions: opts.permissions,
        command: None,
        instructions: Some("You are w1.".into()),
        env: vec![("CODEX_HOME".into(), opts.home.display().to_string())],
        events: tx,
    };
    (spec, rx)
}

/// Runs a turn and returns its outcome, the text it streamed and every other event.
async fn turn(
    session: &dyn DriverSession,
    rx: &mut mpsc::UnboundedReceiver<SessionEvent>,
    text: &str,
) -> (TurnOutcome, String, Vec<SessionEvent>) {
    let outcome = tokio::time::timeout(Duration::from_secs(20), session.prompt(text))
        .await
        .expect("turn timed out")
        .expect("turn failed");
    let (streamed, others) = drain(rx);
    (outcome, streamed, others)
}

fn drain(rx: &mut mpsc::UnboundedReceiver<SessionEvent>) -> (String, Vec<SessionEvent>) {
    let mut text = String::new();
    let mut others = Vec::new();
    while let Ok(event) = rx.try_recv() {
        match &event.kind {
            SessionEventKind::Output {
                stream: OutputStream::Text,
                text: t,
            } => text.push_str(t),
            _ => others.push(event),
        }
    }
    (text, others)
}

fn kinds(events: &[SessionEvent]) -> Vec<&SessionEventKind> {
    events.iter().map(|e| &e.kind).collect()
}

fn pid_of(text: &str) -> String {
    text.split("pid=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .expect("no pid in reply")
        .to_owned()
}

async fn wait_for_processes(driver: &CodexNativeDriver, count: usize) {
    for _ in 0..100 {
        if driver.process_count().await == count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "expected {count} app-server processes, have {}",
        driver.process_count().await
    );
}

#[tokio::test]
async fn opens_a_thread_streams_and_reports_turns() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let driver = driver();
    assert!(driver.capabilities().resume && driver.capabilities().fork);
    assert!(!driver.capabilities().revert_files);

    let (spec, mut rx) = spec(Opts::new(home.path(), work.path()));
    let session = driver.open(spec).await.unwrap();
    let (_, opened) = drain(&mut rx);
    assert!(matches!(
        &opened[0],
        SessionEvent { kind: SessionEventKind::Started { session_id }, raw: Some(_) }
            if session_id == session.session_id()
    ));

    let (outcome, text, events) = turn(&*session, &mut rx, "hello [mcp] [instructions]").await;
    assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    let turn_id = outcome.turn_id.clone().unwrap();
    // The collaboration server is mounted with tool approval on, plus its environment.
    assert!(text.contains("mcp=crossagents:approve:1"), "{text}");
    assert!(text.contains("instructions=You are w1."), "{text}");
    assert!(
        text.ends_with("echo(model=mock-codex,effort=medium): hello [mcp] [instructions]"),
        "{text}"
    );
    let kinds = kinds(&events);
    assert!(
        kinds.iter().any(
            |k| matches!(k, SessionEventKind::TurnStarted { turn_id: Some(t) } if *t == turn_id)
        )
    );
    assert!(kinds.iter().any(|k| matches!(
        k,
        SessionEventKind::TurnCompleted { turn_id: Some(t), stop_reason: StopReason::EndTurn } if *t == turn_id
    )));
    assert!(kinds.iter().any(|k| matches!(
        k,
        SessionEventKind::Usage { used: 1100, size: 200000, tokens: Some(tokens), .. }
            if tokens.cached_input == 400
    )));
    assert!(kinds.iter().any(|k| matches!(
        k,
        SessionEventKind::RateLimits { limits } if limits.plan.as_deref() == Some("pro")
            && limits.windows[0].used_percent == 7.0
    )));
    // The vendor's message is kept alongside the translation.
    assert!(events.iter().all(|e| e.raw.is_some()));
    session.shutdown().await;
}

#[tokio::test]
async fn sessions_share_one_process_per_account() {
    let home = tempfile::tempdir().unwrap();
    let other_home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let driver = driver();

    let (a_spec, mut a_rx) = spec(Opts::new(home.path(), work.path()));
    let (b_spec, mut b_rx) = spec(Opts::new(home.path(), work.path()));
    let (c_spec, mut c_rx) = spec(Opts::new(other_home.path(), work.path()));
    let a = driver.open(a_spec).await.unwrap();
    let b = driver.open(b_spec).await.unwrap();
    let c = driver.open(c_spec).await.unwrap();
    assert_ne!(a.session_id(), b.session_id());
    assert_eq!(driver.process_count().await, 2);

    // Turns of two threads in one process run at the same time.
    let (ra, rb) = tokio::join!(
        turn(&*a, &mut a_rx, "[pid] a"),
        turn(&*b, &mut b_rx, "[pid] b")
    );
    let (_, rc_text, _) = turn(&*c, &mut c_rx, "[pid] c").await;
    assert!(ra.1.ends_with("): [pid] a") && rb.1.ends_with("): [pid] b"));
    assert_eq!(pid_of(&ra.1), pid_of(&rb.1));
    assert_ne!(pid_of(&ra.1), pid_of(&rc_text));

    // The process ends with its last session.
    a.shutdown().await;
    drop(a);
    assert_eq!(driver.process_count().await, 2);
    b.shutdown().await;
    drop(b);
    c.shutdown().await;
    drop(c);
    wait_for_processes(&driver, 0).await;
}

#[tokio::test]
async fn interrupt_cancels_the_running_turn() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let driver = driver();
    let (spec, mut rx) = spec(Opts::new(home.path(), work.path()));
    let session: Arc<dyn DriverSession> = Arc::from(driver.open(spec).await.unwrap());

    let running = tokio::spawn({
        let session = session.clone();
        async move { session.prompt("[slow]").await }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    // A second prompt is refused rather than merged into the running turn.
    assert!(matches!(
        session.prompt("more").await,
        Err(CoreError::Conflict(_))
    ));
    session.interrupt().await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(outcome.stop_reason, StopReason::Cancelled);
    let (_, events) = drain(&mut rx);
    assert!(kinds(&events).iter().any(|k| matches!(
        k,
        SessionEventKind::TurnCompleted {
            stop_reason: StopReason::Cancelled,
            ..
        }
    )));
    // Interrupting an idle session does nothing.
    session.interrupt().await.unwrap();
    let (outcome, _, _) = turn(&*session, &mut rx, "after").await;
    assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    session.shutdown().await;
}

struct Always(PermissionDecision);

/// Policy, handler, the answer Codex should get, the decision reported.
type Case = (
    PermissionPolicy,
    Option<Arc<dyn PermissionHandler>>,
    &'static str,
    PermissionDecision,
);

#[async_trait]
impl PermissionHandler for Always {
    async fn decide(&self, request: &PermissionRequest) -> PermissionDecision {
        assert_eq!(request.kind, "command");
        self.0
    }
}

#[tokio::test]
async fn permission_requests_follow_policy_or_handler() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let driver = driver();
    let cases: [Case; 3] = [
        (
            PermissionPolicy::Bypass,
            None,
            "accept",
            PermissionDecision::Allow,
        ),
        (
            PermissionPolicy::Deny,
            None,
            "decline",
            PermissionDecision::Deny,
        ),
        (
            PermissionPolicy::Deny,
            Some(Arc::new(Always(PermissionDecision::AllowForSession))),
            "acceptForSession",
            PermissionDecision::AllowForSession,
        ),
    ];
    for (permission, handler, answer, decision) in cases {
        let mut opts = Opts::new(home.path(), work.path());
        opts.permission = permission;
        opts.permissions = handler;
        let (spec, mut rx) = spec(opts);
        let session = driver.open(spec).await.unwrap();
        let (_, text, events) = turn(&*session, &mut rx, "[tool]").await;
        assert!(text.starts_with(&format!("permission={answer} ")), "{text}");
        assert!(kinds(&events).iter().any(|k| matches!(
            k,
            SessionEventKind::PermissionRequested { request, decision: d }
                if *d == decision && request.title == "mock-tool --go" && request.turn_id.is_some()
        )));
        let failed = matches!(answer, "decline");
        assert!(kinds(&events).iter().any(|k| matches!(
            k,
            SessionEventKind::ToolCallUpdated { status: Some(s), .. }
                if *s == if failed { ToolStatus::Failed } else { ToolStatus::Completed }
        )));
        session.shutdown().await;
    }
}

#[tokio::test]
async fn suspended_session_resumes_in_a_new_process() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let driver = driver();
    let (first_spec, mut rx) = spec(Opts::new(home.path(), work.path()));
    let session = driver.open(first_spec).await.unwrap();
    let thread_id = session.session_id().to_owned();
    turn(&*session, &mut rx, "first").await;
    let (_, text, _) = turn(&*session, &mut rx, "second [pid]").await;
    let old_pid = pid_of(&text);

    session.suspend().await.unwrap();
    assert!(matches!(
        session.prompt("too late").await,
        Err(CoreError::Conflict(_))
    ));
    drop(session);
    wait_for_processes(&driver, 0).await;

    let (resume_spec, mut rx) = spec(Opts::new(home.path(), work.path()));
    let session = driver.resume(&thread_id, resume_spec).await.unwrap();
    assert_eq!(session.session_id(), thread_id);
    let (_, opened) = drain(&mut rx);
    assert!(
        matches!(&opened[0].kind, SessionEventKind::Started { session_id } if *session_id == thread_id)
    );
    let (_, text, _) = turn(&*session, &mut rx, "[history] [pid]").await;
    assert!(text.contains("history=first|second [pid] "), "{text}");
    assert_ne!(pid_of(&text), old_pid);
    session.shutdown().await;

    let (unknown_spec, _rx) = spec(Opts::new(home.path(), work.path()));
    let err = driver
        .resume("thread-does-not-exist", unknown_spec)
        .await
        .err()
        .expect("resumed an unknown thread");
    assert!(err.to_string().contains("no rollout found"), "{err}");
}

#[tokio::test]
async fn fork_and_rollback() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let fork_dir = tempfile::tempdir().unwrap();
    let driver = driver();
    let (main_spec, mut rx) = spec(Opts::new(home.path(), work.path()));
    let session = driver.open(main_spec).await.unwrap();
    let mut turns = Vec::new();
    for text in ["one", "two", "three"] {
        let (outcome, _, _) = turn(&*session, &mut rx, text).await;
        turns.push(outcome.turn_id.unwrap());
    }

    // A fork through turn two, in its own directory.
    let (fork_spec, mut fork_rx) = spec(Opts::new(home.path(), fork_dir.path()));
    let fork = session.fork(Some(&turns[1]), fork_spec).await.unwrap();
    assert_ne!(fork.session_id(), session.session_id());
    let (_, text, _) = turn(&*fork, &mut fork_rx, "[history] [write f.txt=forked]").await;
    assert!(text.contains("history=one|two "), "{text}");
    assert!(fork_dir.path().join("f.txt").is_file());
    assert!(!work.path().join("f.txt").exists());

    // Rolling back drops the turn and everything after it.
    session.rollback(&turns[1], false).await.unwrap();
    let (_, text, _) = turn(&*session, &mut rx, "[history]").await;
    assert!(text.contains("history=one "), "{text}");
    assert!(matches!(
        session.rollback(&turns[0], true).await,
        Err(CoreError::Unsupported(_))
    ));
    fork.shutdown().await;
    session.shutdown().await;
}

#[tokio::test]
async fn model_effort_and_diffs() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let driver = driver();
    let mut opts = Opts::new(home.path(), work.path());
    opts.config = json!({ "effort": "low" });
    let (spec, mut rx) = spec(opts);
    let session = driver.open(spec).await.unwrap();

    let (_, text, _) = turn(&*session, &mut rx, "a").await;
    assert!(
        text.ends_with("echo(model=mock-codex,effort=low): a"),
        "{text}"
    );
    session
        .set_model(Some("mock-codex-mini"), Some("high"))
        .await
        .unwrap();
    let (_, text, events) = turn(&*session, &mut rx, "[write a.txt=hi]").await;
    assert!(
        text.contains("echo(model=mock-codex-mini,effort=high)"),
        "{text}"
    );
    assert_eq!(
        std::fs::read_to_string(work.path().join("a.txt")).unwrap(),
        "hi"
    );
    let kinds = kinds(&events);
    assert!(kinds.iter().any(|k| matches!(
        k,
        SessionEventKind::TurnDiff { turn_id: Some(_), diff } if diff.contains("+hi")
    )));
    assert!(kinds.iter().any(|k| matches!(
        k,
        SessionEventKind::ToolCall { kind: Some(kind), title, .. } if kind == "edit" && title.contains("a.txt")
    )));
    session.compact().await.unwrap();
    session.shutdown().await;
}

#[tokio::test]
async fn failed_turns_and_crashes_are_reported() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let driver = driver();
    let (first, mut rx) = spec(Opts::new(home.path(), work.path()));
    let session = driver.open(first).await.unwrap();

    let (outcome, _, events) = turn(&*session, &mut rx, "[fail]").await;
    assert_eq!(outcome.stop_reason, StopReason::Error);
    assert!(
        kinds(&events)
            .iter()
            .any(|k| matches!(k, SessionEventKind::Error { message } if message == "mock failure"))
    );

    assert!(session.prompt("[crash]").await.is_err());
    let exited = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = rx.recv().await {
            if matches!(event.kind, SessionEventKind::Exited { .. }) {
                return;
            }
        }
    })
    .await;
    assert!(exited.is_ok(), "no Exited event");
    assert!(session.prompt("again").await.is_err());
    wait_for_processes(&driver, 0).await;

    // The next session gets a fresh process, and the crashed thread can be resumed.
    let thread_id = session.session_id().to_owned();
    drop(session);
    let (again, mut rx) = spec(Opts::new(home.path(), work.path()));
    let session = driver.resume(&thread_id, again).await.unwrap();
    let (_, text, _) = turn(&*session, &mut rx, "[history]").await;
    assert!(text.contains("history=[fail]"), "{text}");
    session.shutdown().await;
}
