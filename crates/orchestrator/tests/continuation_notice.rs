use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use crossagents_core::{
    Agent, AgentId, AgentStatus, ContinuationPolicy, CoreError, Envelope, EventBus, HumanItemKind,
    MessageId, MessageSource, Role, Run, RunConfig, RunId, RunPolicy, RunStatus, Store,
};
use crossagents_orchestrator::{
    AgentHost, GitWorkspaces, Orchestrator, OrchestratorConfig, ShellVerifier,
};
use crossagents_store_sqlite::SqliteStore;
use serde_json::{Value, json};

// A minimal host for the orchestrator. The wrapper can fail only notification delivery.
struct IdleHost;
#[async_trait]
impl AgentHost for IdleHost {
    async fn send(&self, _: AgentId, _: MessageSource, _: String) -> Result<(), CoreError> {
        Ok(())
    }
    async fn send_envelope(&self, _: AgentId, _: Envelope, _: bool) -> Result<(), CoreError> {
        Ok(())
    }
    async fn interrupt(&self, _: AgentId) -> Result<(), CoreError> {
        Ok(())
    }
    async fn withdraw(&self, _: AgentId, _: MessageId) -> Result<(), CoreError> {
        Ok(())
    }
    async fn notify(
        &self,
        _: AgentId,
        _: &str,
        _: &str,
        _: crossagents_core::NoticeClass,
    ) -> Result<(), CoreError> {
        Ok(())
    }
    async fn clone_agent(
        &self,
        _: AgentId,
        _: &str,
        _: Role,
        _: &Path,
    ) -> Result<Agent, CoreError> {
        Err(CoreError::Unsupported("clone".into()))
    }
    async fn spawn_agent(&self, _: Agent) -> Result<Agent, CoreError> {
        Err(CoreError::Unsupported("spawn".into()))
    }
    async fn available_models(&self, _: &str) -> Result<Vec<String>, CoreError> {
        Ok(vec![])
    }
    fn denied_name_words(&self) -> Vec<String> {
        vec![]
    }
    async fn set_model(&self, _: AgentId, _: Option<String>) -> Result<Agent, CoreError> {
        Err(CoreError::Unsupported("model".into()))
    }
    async fn usage_report(&self, _: Option<RunId>) -> Result<Value, CoreError> {
        Ok(json!({}))
    }
    async fn take_inbox(&self, _: AgentId) -> Result<Vec<Value>, CoreError> {
        Ok(vec![])
    }
    async fn archive_agent(&self, _: AgentId) -> Result<(), CoreError> {
        Ok(())
    }
    async fn stop_agent(&self, _: AgentId) -> Result<(), CoreError> {
        Ok(())
    }
    async fn is_busy(&self, _: AgentId) -> bool {
        false
    }
}

struct FailingNotify {
    inner: IdleHost,
    fail: AtomicBool,
}
#[async_trait]
impl AgentHost for FailingNotify {
    async fn send(&self, a: AgentId, f: MessageSource, t: String) -> Result<(), CoreError> {
        self.inner.send(a, f, t).await
    }
    async fn send_envelope(&self, a: AgentId, e: Envelope, n: bool) -> Result<(), CoreError> {
        self.inner.send_envelope(a, e, n).await
    }
    async fn interrupt(&self, a: AgentId) -> Result<(), CoreError> {
        self.inner.interrupt(a).await
    }
    async fn withdraw(&self, a: AgentId, m: MessageId) -> Result<(), CoreError> {
        self.inner.withdraw(a, m).await
    }
    async fn notify(
        &self,
        a: AgentId,
        topic: &str,
        line: &str,
        class: crossagents_core::NoticeClass,
    ) -> Result<(), CoreError> {
        if self.fail.load(Ordering::SeqCst) {
            Err(CoreError::Unsupported("injected notify failure".into()))
        } else {
            self.inner.notify(a, topic, line, class).await
        }
    }
    async fn clone_agent(
        &self,
        a: AgentId,
        n: &str,
        r: Role,
        p: &Path,
    ) -> Result<Agent, CoreError> {
        self.inner.clone_agent(a, n, r, p).await
    }
    async fn spawn_agent(&self, a: Agent) -> Result<Agent, CoreError> {
        self.inner.spawn_agent(a).await
    }
    async fn available_models(&self, p: &str) -> Result<Vec<String>, CoreError> {
        self.inner.available_models(p).await
    }
    fn denied_name_words(&self) -> Vec<String> {
        self.inner.denied_name_words()
    }
    async fn set_model(&self, a: AgentId, m: Option<String>) -> Result<Agent, CoreError> {
        self.inner.set_model(a, m).await
    }
    async fn usage_report(&self, r: Option<RunId>) -> Result<Value, CoreError> {
        self.inner.usage_report(r).await
    }
    async fn take_inbox(&self, a: AgentId) -> Result<Vec<Value>, CoreError> {
        self.inner.take_inbox(a).await
    }
    async fn archive_agent(&self, a: AgentId) -> Result<(), CoreError> {
        self.inner.archive_agent(a).await
    }
    async fn stop_agent(&self, a: AgentId) -> Result<(), CoreError> {
        self.inner.stop_agent(a).await
    }
    async fn is_busy(&self, a: AgentId) -> bool {
        self.inner.is_busy(a).await
    }
}

#[tokio::test]
async fn continuation_delivery_failure_notice_rearms_after_success() {
    let home = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().await.unwrap());
    let host = Arc::new(FailingNotify {
        inner: IdleHost,
        fail: AtomicBool::new(true),
    });
    let orchestrator = Orchestrator::new(
        store.clone(),
        Arc::new(EventBus::new(store.clone())),
        host.clone(),
        Arc::new(GitWorkspaces),
        Arc::new(ShellVerifier::default()),
        OrchestratorConfig {
            worktrees: home.path().join("worktrees"),
            locks: home.path().join("locks"),
            shims: None,
            max_nudges: 0,
            build_cache_limit: u64::MAX,
        },
    );
    let lead = Agent {
        id: AgentId::new(),
        name: "lead".into(),
        name_source: Default::default(),
        plugin: "mock".into(),
        driver: None,
        model: None,
        role: Role::Planner,
        home: home.path().into(),
        cwd: home.path().into(),
        config: json!({}),
        status: AgentStatus::Idle,
        created_at: Utc::now(),
        profile: Default::default(),
        topic: None,
        archived_at: None,
    };
    store.insert_agent(&lead).await.unwrap();
    let run = Run {
        origin_chat_id: None,
        work_id: None,
        continuation: None,
        id: RunId::new(),
        title: "test run".into(),
        request: "test".into(),
        planner: lead.id,
        status: RunStatus::Running,
        config: RunConfig {
            worker: lead.id,
            escalate: None,
            reviewer: None,
            auto_approve: true,
            max_parallel: 1,
            max_attempts: 3,
            team: vec![],
            policy: RunPolicy {
                continuation: Some(ContinuationPolicy {
                    max_attempts: 10,
                    interval_seconds: 3600,
                    stuck_minutes: None,
                }),
                ..Default::default()
            },
        },
        repo: home.path().into(),
        base_branch: "main".into(),
        branch: "test".into(),
        in_place: false,
        mode: None,
        note: None,
        plan: vec![],
        lead_note: None,
        landed: None,
        summary: None,
        checks: vec![],
        checks_state: None,
        checks_snapshot: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    store.insert_run(&run).await.unwrap();
    orchestrator.continue_runs().await; // Establish the phase and first due time.
    for (fail, expected_notices, notified) in [
        (true, 1, true),
        (true, 1, true),
        (false, 1, false),
        (true, 2, true),
    ] {
        host.fail.store(fail, Ordering::SeqCst);
        let mut current = store.get_run(run.id).await.unwrap();
        current.continuation.as_mut().unwrap().next_at = Some(Utc::now() - Duration::seconds(1));
        store.update_run(&current).await.unwrap();
        orchestrator.continue_runs().await;
        let state = store.get_run(run.id).await.unwrap().continuation.unwrap();
        assert_eq!(state.delivery_error_notified, notified);
        assert_eq!(state.delivery_error.is_some(), fail);
        let notices: Vec<_> = store
            .list_human_items(false)
            .await
            .unwrap()
            .into_iter()
            .filter(|item| item.run_id == Some(run.id) && item.kind == HumanItemKind::Notice)
            .collect();
        assert_eq!(notices.len(), expected_notices);
        assert!(
            notices
                .iter()
                .all(|item| item.text.contains("injected notify failure"))
        );
    }
}
