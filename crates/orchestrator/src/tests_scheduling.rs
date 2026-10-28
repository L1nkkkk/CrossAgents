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
    Agent, AgentId, AgentStatus, CoreError, Envelope, EventBus, LandMode, MessageId,
    MessageSource, Role, Run, RunConfig, RunId, RunPolicy, RunStatus, Store, Task, TaskId,
    TaskReport, TaskSpec, TaskStatus,
};
use crossagents_store_sqlite::SqliteStore;
use serde_json::{Value, json};

use crate::engine::{Orchestrator, OrchestratorConfig};
use crate::host::AgentHost;
use crate::verify::Verifier;
use crate::workspace::{Synced, WorkspaceProvider};

// ---------------------------------------------------------------------------------------
// Doubles

/// Delivers nothing anywhere; its agents move into a task's worktree when asked.
struct MovingHost;

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
    async fn notify(&self, _: AgentId, _: &str, _: &str) -> Result<(), CoreError> {
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
        false
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
        let orchestrator = Orchestrator::new(
            store.clone(),
            Arc::new(EventBus::new(store.clone())),
            Arc::new(MovingHost),
            Arc::new(NoopWorkspaces),
            Arc::new(NoChecks),
            OrchestratorConfig {
                worktrees: home.path().join("worktrees"),
                locks: home.path().join("locks"),
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
            note: None,
            plan: Vec::new(),
            lead_note: None,
            landed: None,
            summary: None,
            checks: Vec::new(),
            checks_state: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        store.insert_run(&run).await.unwrap();
        Self {
            store,
            orchestrator,
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
    assert_eq!(task.status, TaskStatus::Pending, "the control case: {task:?}");
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
