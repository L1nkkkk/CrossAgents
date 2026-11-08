//! Changing a running run's tasks starts what can start at once (docs/TODO.md §十六):
//! `task_create`, `task_assign` and `task_update` schedule after they write, so a task they
//! make ready does not sit pending until some other task next ends.
//!
//! These go through the orchestrator directly, with a fake host whose agents can move (so a
//! task given to a session itself, `fresh: false`, really starts) and a fake workspace.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AgentStatus, CoreError, Envelope, EventBus, LandMode, MessageId, MessageSource,
    Role, Run, RunConfig, RunId, RunPolicy, RunStatus, Store, Task, TaskId, TaskReport, TaskSpec,
    TaskStatus,
};
use crossagents_store_sqlite::SqliteStore;
use serde_json::{Value, json};

use crate::engine::{Orchestrator, OrchestratorConfig, worker_prefix};
use crate::host::AgentHost;
use crate::verify::Verifier;
use crate::workspace::{Synced, WorkspaceProvider};

// ---------------------------------------------------------------------------------------
// Doubles

/// Delivers nothing anywhere; its agents move into a task's worktree when asked.
#[derive(Default)]
struct MovingHost {
    busy: std::sync::atomic::AtomicBool,
    panic_on_busy: std::sync::atomic::AtomicBool,
    busy_panicked: tokio::sync::Notify,
    person_waiting: std::sync::atomic::AtomicBool,
    notices: tokio::sync::Mutex<Vec<String>>,
    block_notice: std::sync::atomic::AtomicBool,
    notice_started: tokio::sync::Notify,
    notice_release: tokio::sync::Notify,
    notice_dropped: tokio::sync::Notify,
}

#[async_trait]
impl AgentHost for MovingHost {
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
        line: &str,
        _: crossagents_core::NoticeClass,
    ) -> Result<(), CoreError> {
        struct DropNotice<'a>(&'a tokio::sync::Notify);
        impl Drop for DropNotice<'_> {
            fn drop(&mut self) {
                self.0.notify_one();
            }
        }
        if self.block_notice.load(std::sync::atomic::Ordering::SeqCst) {
            let _dropped = DropNotice(&self.notice_dropped);
            self.notice_started.notify_one();
            self.notice_release.notified().await;
        }
        self.notices.lock().await.push(line.to_owned());
        Ok(())
    }
    async fn clone_agent(
        &self,
        _: AgentId,
        _: &str,
        _: Role,
        _: &Path,
    ) -> Result<Agent, CoreError> {
        Err(CoreError::Unsupported("nothing is recruited here".into()))
    }
    async fn spawn_agent(&self, _: Agent) -> Result<Agent, CoreError> {
        Err(CoreError::Unsupported("nothing is recruited here".into()))
    }
    async fn available_models(&self, _: &str) -> Result<Vec<String>, CoreError> {
        Ok(Vec::new())
    }
    fn denied_name_words(&self) -> Vec<String> {
        Vec::new()
    }
    async fn set_model(&self, _: AgentId, _: Option<String>) -> Result<Agent, CoreError> {
        Err(CoreError::Unsupported("no models here".into()))
    }
    async fn usage_report(&self, _: Option<RunId>) -> Result<Value, CoreError> {
        Ok(json!({}))
    }
    async fn take_inbox(&self, _: AgentId) -> Result<Vec<Value>, CoreError> {
        Ok(Vec::new())
    }
    async fn archive_agent(&self, _: AgentId) -> Result<(), CoreError> {
        Ok(())
    }
    async fn stop_agent(&self, _: AgentId) -> Result<(), CoreError> {
        Ok(())
    }
    async fn place(&self, _: AgentId, _: &Path, _: bool) -> Result<(), CoreError> {
        Ok(())
    }
    async fn send_home(&self, _: AgentId) -> Result<bool, CoreError> {
        Ok(true)
    }
    async fn is_busy(&self, _: AgentId) -> bool {
        if self
            .panic_on_busy
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.busy_panicked.notify_one();
            panic!("injected is_busy panic");
        }
        self.busy.load(std::sync::atomic::Ordering::SeqCst)
    }
    async fn serving_person(&self, _: AgentId) -> bool {
        self.person_waiting
            .load(std::sync::atomic::Ordering::SeqCst)
    }
    async fn can_move(&self, _: &Agent) -> bool {
        true
    }
}

/// Every git operation succeeds and does nothing.
struct NoopWorkspaces;

#[async_trait]
impl WorkspaceProvider for NoopWorkspaces {
    async fn versioned(&self, _: &Path) -> bool {
        true
    }
    async fn current_branch(&self, _: &Path) -> Result<String, CoreError> {
        Ok("main".into())
    }
    async fn create(&self, _: &Path, _: &str, _: &str, _: &Path) -> Result<(), CoreError> {
        Ok(())
    }
    async fn commit_all(&self, _: &Path, _: &str) -> Result<bool, CoreError> {
        Ok(false)
    }
    async fn diff_stat(&self, _: &Path, _: &str) -> Result<String, CoreError> {
        Ok(String::new())
    }
    async fn diff(&self, _: &Path, _: &str, _: &[String]) -> Result<String, CoreError> {
        Ok(String::new())
    }
    async fn branch_diff(&self, _: &Path, _: &str, _: &str) -> Result<String, CoreError> {
        Ok(String::new())
    }
    async fn merge(&self, _: &Path, _: &str, _: &str) -> Result<(), CoreError> {
        Ok(())
    }
    async fn land(
        &self,
        repo: &Path,
        _: &str,
        _: &str,
        _: LandMode,
        _: &str,
    ) -> Result<String, CoreError> {
        Ok(repo.display().to_string())
    }
    async fn checkout(&self, _: &Path, _: &str, _: &Path) -> Result<(), CoreError> {
        Ok(())
    }
    async fn sync_base(&self, _: &Path, _: &str) -> Result<Synced, CoreError> {
        Ok(Synced::UpToDate)
    }
    async fn remove(&self, _: &Path, _: &Path, _: Option<&str>) -> Result<(), CoreError> {
        Ok(())
    }
    async fn delete_branch(&self, _: &Path, _: &str) -> Result<(), CoreError> {
        Ok(())
    }
    async fn uncommitted(&self, _: &Path) -> Result<Vec<String>, CoreError> {
        Ok(Vec::new())
    }
    async fn head(&self, _: &Path) -> Result<String, CoreError> {
        Ok(String::new())
    }
}

struct NoChecks;

#[async_trait]
impl Verifier for NoChecks {
    async fn check(&self, _: &Path, _: &[String]) -> Vec<crossagents_core::CheckResult> {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------------------
// A running run with a lead and two workers, and no tasks yet

struct Fixture {
    store: Arc<dyn Store>,
    orchestrator: Arc<Orchestrator>,
    host: Arc<MovingHost>,
    run: Run,
    lead: Agent,
    rex: Agent,
    max: Agent,
    _home: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let repo = home.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().await.unwrap());
        let host = Arc::new(MovingHost::default());
        let orchestrator = Orchestrator::new(
            store.clone(),
            Arc::new(EventBus::new(store.clone())),
            host.clone(),
            Arc::new(NoopWorkspaces),
            Arc::new(NoChecks),
            OrchestratorConfig {
                worktrees: home.path().join("worktrees"),
                locks: home.path().join("locks"),
                shims: None,
                max_nudges: 0,
                build_cache_limit: u64::MAX,
            },
        );
        let lead = agent("Ada", Role::Planner, &repo);
        let rex = agent("Rex", Role::Worker, &repo);
        let max = agent("Max", Role::Worker, &repo);
        for a in [&lead, &rex, &max] {
            store.insert_agent(a).await.unwrap();
        }
        let run = Run {
            origin_chat_id: None,
            work_id: None,
            continuation: None,
            id: RunId::new(),
            title: "a run".into(),
            request: "build it".into(),
            planner: lead.id,
            status: RunStatus::Running,
            config: RunConfig {
                worker: rex.id,
                escalate: None,
                reviewer: None,
                auto_approve: true,
                max_parallel: 2,
                max_attempts: 3,
                team: vec![rex.id, max.id],
                policy: RunPolicy::default(),
            },
            repo,
            base_branch: "main".into(),
            branch: "crossagents/run".into(),
            in_place: false,
            mode: None,
            note: None,
            plan: Vec::new(),
            lead_note: None,
            landed: None,
            summary: None,
            checks: Vec::new(),
            checks_state: None,
            checks_snapshot: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        store.insert_run(&run).await.unwrap();
        Self {
            store,
            orchestrator,
            host,
            run,
            lead,
            rex,
            max,
            _home: home,
        }
    }

    /// Puts a task straight into the store, as a run left it: nothing schedules it.
    async fn insert(
        &self,
        key: &str,
        status: TaskStatus,
        depends_on: &[&str],
        worker: Option<AgentId>,
    ) -> Task {
        let task = Task {
            id: TaskId::new(),
            run_id: self.run.id,
            key: key.into(),
            title: format!("task {key}"),
            spec: TaskSpec {
                goal: format!("do {key}"),
                ..TaskSpec::default()
            },
            depends_on: depends_on.iter().map(|d| (*d).to_owned()).collect(),
            issuer: Some(self.lead.id),
            assignee: None,
            reviewer: None,
            worker_template: worker,
            reviewer_template: None,
            worker_fresh: false,
            worker_restore: false,
            reviewer_fresh: true,
            status,
            attempts: 0,
            runs: Vec::new(),
            escalated: false,
            nudges: 0,
            branch: None,
            worktree: None,
            report: TaskReport::default(),
            parent: None,
            progress: None,
            handoffs: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        self.store.insert_task(&task).await.unwrap();
        task
    }

    async fn task(&self, id: TaskId) -> Task {
        self.store.get_task(id).await.unwrap()
    }

    async fn set_run_status(&self, status: RunStatus) {
        let mut run = self.store.get_run(self.run.id).await.unwrap();
        run.status = status;
        self.store.update_run(&run).await.unwrap();
    }
}

fn agent(name: &str, role: Role, home: &Path) -> Agent {
    Agent {
        id: AgentId::new(),
        name: name.into(),
        name_source: Default::default(),
        plugin: "mock".into(),
        driver: None,
        model: None,
        role,
        home: home.to_path_buf(),
        cwd: home.to_path_buf(),
        config: json!({}),
        status: AgentStatus::Idle,
        created_at: Utc::now(),
        profile: Default::default(),
        topic: None,
        archived_at: None,
    }
}

async fn scheduled_fixture() -> (Fixture, crossagents_core::WorkId) {
    let f = Fixture::new().await;
    let work = f
        .orchestrator
        .create_work_group("rounds", f.run.repo.clone(), f.lead.id)
        .await
        .unwrap();
    let mut old = f.run.clone();
    old.work_id = Some(work.id);
    old.status = RunStatus::Completed;
    f.store.update_run(&old).await.unwrap();
    *f.orchestrator.round_wait_limit.lock().unwrap() = std::time::Duration::from_millis(250);
    f.host.busy.store(true, std::sync::atomic::Ordering::SeqCst);
    let result = f
        .orchestrator
        .run_next(&f.lead, json!({"title":"next", "request":"new work"}))
        .await
        .unwrap();
    assert_eq!(result["status"], "scheduled");
    (f, work.id)
}

async fn end_lead_turn(f: &Fixture) {
    f.orchestrator
        .publish(crossagents_core::EventKind::AgentTurnCompleted {
            id: f.lead.id,
            message_id: MessageId::new(),
            stop_reason: crossagents_core::StopReason::EndTurn,
        })
        .await;
}

async fn scheduled_notice(f: &Fixture) -> String {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(line) = f.host.notices.lock().await.first().cloned() {
                return line;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("scheduled notice")
}

#[tokio::test]
async fn scheduled_long_turn_is_cancelled_with_notice() {
    let (f, _) = scheduled_fixture().await;
    let line = scheduled_notice(&f).await;
    assert!(
        line.contains("cancelled") && line.contains("turn to end"),
        "{line}"
    );
    assert_eq!(f.store.list_runs().await.unwrap().len(), 1);
}

#[tokio::test]
async fn scheduled_when_lead_cwd_names_a_missing_checkout() {
    let f = Fixture::new().await;
    let work = f
        .orchestrator
        .create_work_group("rounds", f.run.repo.clone(), f.lead.id)
        .await
        .unwrap();
    let mut old = f.run.clone();
    old.work_id = Some(work.id);
    old.status = RunStatus::Completed;
    f.store.update_run(&old).await.unwrap();
    let mut caller = f.lead.clone();
    caller.cwd = f.orchestrator.integration_path(&old);
    assert!(!caller.cwd.exists());
    f.store
        .update_agent_cwd(caller.id, &caller.cwd)
        .await
        .unwrap();
    f.host.busy.store(true, std::sync::atomic::Ordering::SeqCst);
    let result = f
        .orchestrator
        .run_next(&caller, json!({"title":"next", "request":"new work"}))
        .await
        .unwrap();
    assert_eq!(result["status"], "scheduled");
    f.host
        .busy
        .store(false, std::sync::atomic::Ordering::SeqCst);
    end_lead_turn(&f).await;
    let next = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(run) = f
                .store
                .list_runs()
                .await
                .unwrap()
                .into_iter()
                .find(|run| run.id != old.id)
            {
                break run;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the lead_in path started the new round");
    assert_eq!(next.planner, caller.id);
}

#[tokio::test]
async fn scheduled_waiting_person_is_cancelled_with_notice() {
    let (f, _) = scheduled_fixture().await;
    f.host
        .person_waiting
        .store(true, std::sync::atomic::Ordering::SeqCst);
    end_lead_turn(&f).await;
    let line = scheduled_notice(&f).await;
    assert!(
        line.contains("cancelled") && line.contains("idle"),
        "{line}"
    );
    assert_eq!(f.store.list_runs().await.unwrap().len(), 1);
}

#[tokio::test]
async fn scheduled_creation_watchdog_does_not_cancel() {
    let (f, work) = scheduled_fixture().await;
    let guard = f.orchestrator.lock_card(&format!("work:{work}")).await;
    f.host
        .busy
        .store(false, std::sync::atomic::Ordering::SeqCst);
    end_lead_turn(&f).await;
    let line = scheduled_notice(&f).await;
    assert!(
        line.contains("still starting") && line.contains("not been cancelled"),
        "{line}"
    );
    drop(guard);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while f.store.list_runs().await.unwrap().len() != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("creation continued after watchdog");
}

#[tokio::test]
async fn completed_creation_cancels_a_blocked_watchdog_notice() {
    let (f, work) = scheduled_fixture().await;
    let guard = f.orchestrator.lock_card(&format!("work:{work}")).await;
    f.host
        .block_notice
        .store(true, std::sync::atomic::Ordering::SeqCst);
    f.host
        .busy
        .store(false, std::sync::atomic::Ordering::SeqCst);
    end_lead_turn(&f).await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        f.host.notice_started.notified(),
    )
    .await
    .expect("watchdog entered notify");
    drop(guard);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        f.host.notice_dropped.notified(),
    )
    .await
    .expect("blocked notice cancelled");
    f.host.notice_release.notify_one();
    assert!(f.host.notices.lock().await.is_empty());
    assert_eq!(f.store.list_runs().await.unwrap().len(), 2);
}

#[tokio::test]
async fn successful_schedule_releases_its_key_for_same_source() {
    let (f, work) = scheduled_fixture().await;
    f.host
        .busy
        .store(false, std::sync::atomic::Ordering::SeqCst);
    end_lead_turn(&f).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while f
            .orchestrator
            .scheduled_rounds
            .lock()
            .unwrap()
            .contains(&(work, f.run.id))
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first schedule finished");
    let next = f
        .store
        .list_runs()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.id != f.run.id)
        .expect("first round was created");
    f.store.delete_run(next.id).await.unwrap();
    f.host.busy.store(true, std::sync::atomic::Ordering::SeqCst);
    let result = f
        .orchestrator
        .run_next(&f.lead, json!({"title":"again", "request":"new work"}))
        .await
        .unwrap();
    assert_eq!(result["status"], "scheduled");
    f.host
        .busy
        .store(false, std::sync::atomic::Ordering::SeqCst);
    end_lead_turn(&f).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let runs = f.store.list_runs().await.unwrap();
            if runs.iter().any(|r| r.id != f.run.id && r.id != next.id) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a second round was created");
}

#[tokio::test]
async fn panicked_schedule_releases_its_key_and_can_be_started_again() {
    let (f, work) = scheduled_fixture().await;
    f.host
        .panic_on_busy
        .store(true, std::sync::atomic::Ordering::SeqCst);
    end_lead_turn(&f).await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        f.host.busy_panicked.notified(),
    )
    .await
    .expect("background reached injected panic");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while f
            .orchestrator
            .scheduled_rounds
            .lock()
            .unwrap()
            .contains(&(work, f.run.id))
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("panic released the scheduled key");
    let result = f
        .orchestrator
        .run_next(&f.lead, json!({"title":"retry", "request":"new work"}))
        .await
        .unwrap();
    assert_eq!(result["status"], "scheduled");
    assert!(
        f.orchestrator
            .scheduled_rounds
            .lock()
            .unwrap()
            .contains(&(work, f.run.id))
    );
    f.host
        .busy
        .store(false, std::sync::atomic::Ordering::SeqCst);
    end_lead_turn(&f).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !f
            .store
            .list_runs()
            .await
            .unwrap()
            .iter()
            .any(|run| run.title == "retry")
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("retry created a round");
}

#[tokio::test]
async fn scheduled_source_deleted_or_superseded_is_cancelled() {
    for deleted in [true, false] {
        let (f, work) = scheduled_fixture().await;
        if deleted {
            f.store.delete_run(f.run.id).await.unwrap();
        } else {
            let mut newer = f.run.clone();
            newer.id = RunId::new();
            newer.work_id = Some(work);
            newer.status = RunStatus::Completed;
            newer.created_at = chrono::Utc::now() + chrono::Duration::seconds(1);
            f.store.insert_run(&newer).await.unwrap();
        }
        f.host
            .busy
            .store(false, std::sync::atomic::Ordering::SeqCst);
        end_lead_turn(&f).await;
        let line = scheduled_notice(&f).await;
        assert!(
            line.contains("cancelled") && line.contains("source round"),
            "{line}"
        );
        assert_eq!(
            f.store.list_runs().await.unwrap().len(),
            if deleted { 0 } else { 2 }
        );
    }
}

// ---------------------------------------------------------------------------------------
// (a) task_create

#[tokio::test]
async fn a_created_task_with_nothing_to_wait_for_starts_at_once() {
    let f = Fixture::new().await;

    let out = f
        .orchestrator
        .task_create(
            &f.lead,
            json!({ "tasks": [{
                "key": "a",
                "title": "task a",
                "spec": { "goal": "do a" },
                "assignee": "Rex",
                "fresh": false
            }] }),
        )
        .await
        .unwrap();

    assert_eq!(out["not_started"], json!([]), "{out}");
    let id: TaskId = serde_json::from_value(out["created"][0]["task_id"].clone()).unwrap();
    let task = f.task(id).await;
    assert_eq!(task.status, TaskStatus::Running, "{task:?}");
    assert_eq!(task.assignee, Some(f.rex.id), "{task:?}");
    assert!(task.worktree.is_some(), "{task:?}");
    // The answer shows it as scheduling left it: the state before read as "assignee
    // ignored", and a lead assigned it a second time.
    assert_eq!(out["created"][0]["status"], "running", "{out}");
    assert_eq!(out["created"][0]["assignee"], json!(f.rex.id), "{out}");
}

#[tokio::test]
async fn asking_again_replaces_the_open_question() {
    let f = Fixture::new().await;
    let mut task = f
        .insert("a", TaskStatus::Running, &[], Some(f.rex.id))
        .await;
    task.assignee = Some(f.rex.id);
    f.store.update_task(&task).await.unwrap();

    let first = f
        .orchestrator
        .ask(&f.rex, task.id, "which file?".into())
        .await
        .unwrap();
    // Asked again in other words while the first waits: it used to fail with
    // "WaitingAnswer -> WaitingAnswer", so the worker thought it could not ask at all.
    let second = f
        .orchestrator
        .ask(&f.rex, task.id, "which file, after syncing?".into())
        .await
        .unwrap();
    assert_eq!(second["replaces"], first["question_id"], "{second}");
    let task = f.task(task.id).await;
    assert_eq!(task.status, TaskStatus::WaitingAnswer);
    assert_eq!(
        task.report.question.as_deref(),
        Some("which file, after syncing?")
    );
    assert_eq!(
        json!(task.report.question_message),
        second["question_id"],
        "{task:?}"
    );
}

#[tokio::test]
async fn a_created_task_that_waits_on_another_stays_pending() {
    let f = Fixture::new().await;
    f.insert("dep", TaskStatus::Pending, &["nothing-yet"], None)
        .await;

    let out = f
        .orchestrator
        .task_create(
            &f.lead,
            json!({ "tasks": [{
                "key": "a",
                "title": "task a",
                "spec": { "goal": "do a" },
                "depends_on": ["dep"],
                "assignee": "Rex",
                "fresh": false
            }] }),
        )
        .await
        .unwrap();

    let id: TaskId = serde_json::from_value(out["created"][0]["task_id"].clone()).unwrap();
    let task = f.task(id).await;
    assert_eq!(
        task.status,
        TaskStatus::Pending,
        "the control case: {task:?}"
    );
    assert_eq!(task.assignee, None, "{task:?}");
}

// ---------------------------------------------------------------------------------------
// (b) task_assign

#[tokio::test]
async fn an_assigned_pending_task_starts_at_once() {
    let f = Fixture::new().await;
    // Left pending by an earlier change that did not schedule.
    let stuck = f.insert("b", TaskStatus::Pending, &[], None).await;

    let out = f
        .orchestrator
        .task_assign(&f.lead, json!({ "task": "b", "to": "Max", "fresh": false }))
        .await
        .unwrap();

    assert_eq!(out["not_started"], json!([]), "{out}");
    assert_eq!(out["task"]["status"], "running", "{out}");
    let task = f.task(stuck.id).await;
    assert_eq!(task.status, TaskStatus::Running, "{task:?}");
    assert_eq!(task.assignee, Some(f.max.id), "{task:?}");
    assert_eq!(task.worker_template, Some(f.max.id), "{task:?}");
}

/// A new execution session on the run's escalation teammate is named `e-`, any other worker
/// `w-` (docs/TODO.md §九): the prefix follows the role, whichever tool hands the task over.
#[tokio::test]
async fn a_new_session_is_named_e_on_the_escalation_teammate() {
    let f = Fixture::new().await;
    let mut run = f.run.clone();
    run.config.escalate = Some(f.max.id);
    let task = f.insert("a", TaskStatus::Pending, &[], None).await;

    assert_eq!(worker_prefix(&run, &task, f.max.id), "e");
    assert_eq!(worker_prefix(&run, &task, f.rex.id), "w");

    // Escalated by the plan rather than by a handoff: the flag alone decides it too.
    let escalated = Task {
        escalated: true,
        ..task.clone()
    };
    assert_eq!(worker_prefix(&run, &escalated, f.max.id), "e");
}

// ---------------------------------------------------------------------------------------
// (c) task_update

#[tokio::test]
async fn a_task_whose_dependencies_are_now_met_starts_at_once() {
    let f = Fixture::new().await;
    f.insert("done", TaskStatus::Merged, &[], None).await;
    let waiting = f
        .insert("c", TaskStatus::Pending, &["not-there"], Some(f.rex.id))
        .await;

    let out = f
        .orchestrator
        .task_update(&f.lead, json!({ "task": "c", "depends_on": ["done"] }))
        .await
        .unwrap();

    // Same shape as before, now showing the task as scheduling left it.
    assert_eq!(out["key"], "c", "{out}");
    assert_eq!(out["status"], "running", "{out}");
    assert_eq!(out["depends_on"], json!(["done"]), "{out}");
    let task = f.task(waiting.id).await;
    assert_eq!(task.status, TaskStatus::Running, "{task:?}");
    assert_eq!(task.assignee, Some(f.rex.id), "{task:?}");
}

#[tokio::test]
async fn a_task_updated_while_the_run_is_paused_does_not_start() {
    let f = Fixture::new().await;
    f.insert("done", TaskStatus::Merged, &[], None).await;
    let waiting = f
        .insert("c", TaskStatus::Pending, &["not-there"], Some(f.rex.id))
        .await;
    f.set_run_status(RunStatus::Paused).await;

    let out = f
        .orchestrator
        .task_update(&f.lead, json!({ "task": "c", "depends_on": ["done"] }))
        .await
        .unwrap();

    assert_eq!(out["status"], "pending", "{out}");
    let task = f.task(waiting.id).await;
    assert_eq!(task.status, TaskStatus::Pending, "{task:?}");
    assert_eq!(task.depends_on, vec!["done".to_owned()], "{task:?}");
    assert_eq!(task.assignee, None, "{task:?}");
}

/// The prefix says what the session is, so an escalated task's session is `e-…`. Three places
/// name a worker (engine's handout, `tasks.rs`'s reassignment, `quota.rs`'s move off a spent
/// account); they all go through `worker_name`, or the name stops matching the session.
#[tokio::test]
async fn an_escalated_task_names_its_session_with_e() {
    let f = Fixture::new().await;
    let mut task = f.insert("auth", TaskStatus::Pending, &[], None).await;

    let plain = crate::engine::worker_name(&f.run, &task, f.rex.id);
    assert!(plain.starts_with("w-"), "{plain}");
    assert!(plain.ends_with("-auth"), "{plain}");

    task.escalated = true;
    let escalated = crate::engine::worker_name(&f.run, &task, f.rex.id);
    assert!(escalated.starts_with("e-"), "{escalated}");
    assert_eq!(escalated[1..], plain[1..], "only the prefix differs");
}

#[tokio::test]
async fn learned_unavailable_model_refuses_assignments_until_cleared() {
    let f = Fixture::new().await;
    f.store
        .update_agent_model(f.max.id, Some("retired"))
        .await
        .unwrap();
    f.store
        .record_unavailable_model("mock", "", "retired", "invalid model")
        .await
        .unwrap();
    f.insert("b", TaskStatus::Pending, &[], None).await;
    let assign = f
        .orchestrator
        .task_assign(&f.lead, json!({"task":"b", "to":"Max", "fresh":false}))
        .await
        .unwrap_err();
    assert!(assign.to_string().contains("invalid model"));
    let create = f.orchestrator.task_create(&f.lead, json!({"tasks":[{"key":"c", "title":"c", "spec":{"goal":"c"}, "assignee":"Max", "fresh":false}]})).await.unwrap_err();
    assert!(create.to_string().contains("invalid model"));
    f.store
        .clear_unavailable_model("mock", "", "retired")
        .await
        .unwrap();
    f.orchestrator
        .task_assign(&f.lead, json!({"task":"b", "to":"Max", "fresh":false}))
        .await
        .unwrap();
}
