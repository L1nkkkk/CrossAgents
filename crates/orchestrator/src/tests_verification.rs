//! The edges of the verification gate (docs/TODO.md §九 ①, ②): what happens when the checks
//! themselves cannot run, when the hub restarts in the middle of one, when a run is cancelled
//! under one, and what an agent that cannot be let go of keeps.
//!
//! These go through the orchestrator directly, with a fake host and a fake workspace: the
//! failure they need (a commit that cannot be made, an agent that will not go home) is not
//! something a real git or a real hub can be made to do on demand.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use chrono::Utc;
use crossagents_core::{
    Agent, AgentClaim, AgentId, AgentStatus, ClaimRole, CoreError, Envelope, EventBus, LandMode,
    MessageId, MessageSource, Role, Run, RunConfig, RunId, RunPolicy, RunStatus, Store, Task,
    TaskId, TaskReport, TaskSpec, TaskStatus,
};
use crossagents_store_sqlite::SqliteStore;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::engine::{Orchestrator, OrchestratorConfig};
use crate::host::AgentHost;
use crate::verify::Verifier;
use crate::workspace::{Synced, WorkspaceProvider};

// ---------------------------------------------------------------------------------------
// Doubles

/// Answers messages, notices and moves; fails the move or the archive when told to.
#[derive(Default)]
struct FakeHost {
    will_not_go_home: AtomicBool,
    will_not_archive: AtomicBool,
    busy: AtomicBool,
    notices: Mutex<Vec<String>>,
    delivered: Mutex<Vec<(AgentId, String)>>,
    /// Kept for the agent's next message, by topic; a newer one on a topic replaces it.
    later: Mutex<std::collections::HashMap<(AgentId, String), String>>,
}

impl FakeHost {
    async fn delivered_to(&self, agent: AgentId) -> Vec<String> {
        self.delivered
            .lock()
            .await
            .iter()
            .filter(|(to, _)| *to == agent)
            .map(|(_, text)| text.clone())
            .collect()
    }

    /// Waits until a notice containing `what` was given to anyone.
    async fn wait_for_notice(&self, what: &str) {
        tokio::time::timeout(std::time::Duration::from_secs(4), async {
            while !self.notices.lock().await.iter().any(|n| n.contains(what)) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no notice said {what:?}"));
    }

    async fn wait_for_delivery(&self, agent: AgentId) {
        tokio::time::timeout(std::time::Duration::from_secs(4), async {
            while self.delivered_to(agent).await.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the report was not delivered");
    }
}

#[async_trait]
impl AgentHost for FakeHost {
    async fn tell_later(&self, agent: AgentId, topic: &str, line: &str) -> Result<(), CoreError> {
        self.later
            .lock()
            .await
            .insert((agent, topic.to_owned()), line.to_owned());
        Ok(())
    }
    async fn send(&self, agent: AgentId, _: MessageSource, text: String) -> Result<(), CoreError> {
        self.delivered.lock().await.push((agent, text));
        Ok(())
    }
    async fn send_envelope(
        &self,
        agent: AgentId,
        envelope: Envelope,
        _: bool,
    ) -> Result<(), CoreError> {
        self.delivered.lock().await.push((agent, envelope.text));
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
        topic: &str,
        line: &str,
        _: crossagents_core::NoticeClass,
    ) -> Result<(), CoreError> {
        self.notices.lock().await.push(format!("{topic}: {line}"));
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
        if self.will_not_archive.load(Ordering::SeqCst) {
            return Err(CoreError::Storage("the session cannot be stopped".into()));
        }
        Ok(())
    }
    async fn stop_agent(&self, _: AgentId) -> Result<(), CoreError> {
        Ok(())
    }
    async fn send_home(&self, _: AgentId) -> Result<bool, CoreError> {
        if self.will_not_go_home.load(Ordering::SeqCst) {
            return Err(CoreError::Agent("its turn will not end".into()));
        }
        Ok(true)
    }
    async fn is_busy(&self, _: AgentId) -> bool {
        self.busy.load(Ordering::SeqCst)
    }
}

/// Records what it was asked to do; fails the commit when told to.
#[derive(Default)]
struct FakeWorkspaces {
    cannot_commit: AtomicBool,
    head: Mutex<Option<String>>,
    /// What `uncommitted` reports: work left in the checkout, not committed.
    dirty: Mutex<Vec<String>>,
    merged: Mutex<Vec<String>>,
    removed: Mutex<Vec<PathBuf>>,
}

impl FakeWorkspaces {
    async fn removed(&self) -> Vec<PathBuf> {
        self.removed.lock().await.clone()
    }

    async fn merged(&self) -> Vec<String> {
        self.merged.lock().await.clone()
    }
}

#[async_trait]
impl WorkspaceProvider for FakeWorkspaces {
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
        if self.cannot_commit.load(Ordering::SeqCst) {
            return Err(CoreError::Storage("index.lock is held".into()));
        }
        Ok(true)
    }
    async fn diff_stat(&self, _: &Path, _: &str) -> Result<String, CoreError> {
        Ok(" a.txt | 1 +\n".into())
    }
    async fn diff(&self, _: &Path, _: &str, _: &[String]) -> Result<String, CoreError> {
        Ok(String::new())
    }
    async fn branch_diff(&self, _: &Path, _: &str, _: &str) -> Result<String, CoreError> {
        Ok(String::new())
    }
    async fn merge(&self, _: &Path, branch: &str, _: &str) -> Result<(), CoreError> {
        self.merged.lock().await.push(branch.to_owned());
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
    async fn remove(&self, _: &Path, path: &Path, _: Option<&str>) -> Result<(), CoreError> {
        self.removed.lock().await.push(path.to_path_buf());
        Ok(())
    }
    async fn delete_branch(&self, _: &Path, _: &str) -> Result<(), CoreError> {
        Ok(())
    }
    async fn uncommitted(&self, _: &Path) -> Result<Vec<String>, CoreError> {
        Ok(self.dirty.lock().await.clone())
    }
    async fn head(&self, _: &Path) -> Result<String, CoreError> {
        Ok(self.head.lock().await.clone().unwrap_or_default())
    }
}

/// Answers every check the way the test says, and counts the rounds it ran.
#[derive(Default)]
struct FixedChecks {
    answers: Vec<bool>,
    rounds: Mutex<u32>,
}

#[async_trait]
impl Verifier for FixedChecks {
    async fn check(&self, _: &Path, commands: &[String]) -> Vec<crossagents_core::CheckResult> {
        {
            let mut rounds = self.rounds.lock().await;
            *rounds += 1;
        }
        commands
            .iter()
            .enumerate()
            .map(|(i, command)| crossagents_core::CheckResult {
                command: command.clone(),
                exit_code: Some(if self.answers.get(i).copied().unwrap_or(true) {
                    0
                } else {
                    1
                }),
                passed: self.answers.get(i).copied().unwrap_or(true),
                output: String::new(),
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------------------
// A run with one task in it

struct Fixture {
    store: Arc<dyn Store>,
    orchestrator: Arc<Orchestrator>,
    host: Arc<FakeHost>,
    ws: Arc<FakeWorkspaces>,
    checks: Arc<FixedChecks>,
    worktrees: PathBuf,
    run: Run,
    task: Task,
    lead: Agent,
    worker: Agent,
    _home: tempfile::TempDir,
}

impl Fixture {
    async fn new(acceptance: &[&str]) -> Self {
        Fixture::with_checks(acceptance, Vec::new()).await
    }

    async fn with_checks(acceptance: &[&str], checks: Vec<bool>) -> Self {
        let home = tempfile::tempdir().unwrap();
        let worktrees = home.path().join("worktrees");
        let repo = home.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().await.unwrap());
        let host = Arc::new(FakeHost::default());
        let ws = Arc::new(FakeWorkspaces::default());
        let checks = Arc::new(FixedChecks {
            answers: checks,
            rounds: Mutex::new(0),
        });
        let orchestrator = Orchestrator::new(
            store.clone(),
            Arc::new(EventBus::new(store.clone())),
            host.clone(),
            ws.clone(),
            checks.clone(),
            OrchestratorConfig {
                worktrees: worktrees.clone(),
                locks: home.path().join("locks"),
                shims: None,
                max_nudges: 0,
                build_cache_limit: u64::MAX,
            },
        );
        let lead = agent("Ada", Role::Planner, &repo);
        let worker = agent("Rex", Role::Worker, &repo);
        for a in [&lead, &worker] {
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
                worker: worker.id,
                escalate: None,
                reviewer: None,
                auto_approve: true,
                max_parallel: 1,
                max_attempts: 3,
                team: vec![worker.id],
                policy: RunPolicy::default(),
            },
            repo: repo.clone(),
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
        let worktree = worktrees.join(short(&run.id)).join("a");
        std::fs::create_dir_all(&worktree).unwrap();
        let task = Task {
            id: TaskId::new(),
            run_id: run.id,
            key: "a".into(),
            title: "task a".into(),
            spec: TaskSpec {
                goal: "do a".into(),
                acceptance: acceptance.iter().map(|c| (*c).to_owned()).collect(),
                ..TaskSpec::default()
            },
            depends_on: Vec::new(),
            issuer: Some(lead.id),
            assignee: Some(worker.id),
            reviewer: None,
            worker_template: Some(worker.id),
            reviewer_template: None,
            worker_fresh: false,
            worker_restore: false,
            reviewer_fresh: true,
            status: TaskStatus::Running,
            attempts: 1,
            runs: Vec::new(),
            escalated: false,
            nudges: 0,
            branch: Some("crossagents/run-a".into()),
            worktree: Some(worktree),
            report: TaskReport::default(),
            parent: None,
            progress: None,
            handoffs: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        store.insert_task(&task).await.unwrap();
        // The worker works on it (fresh: false: the agent itself, so letting go sends it home).
        store
            .claim_agent(&AgentClaim::new(
                worker.id,
                run.id,
                Some(task.id),
                ClaimRole::Worker,
                false,
            ))
            .await
            .unwrap();
        // And the lead leading it, as a copy (archived when let go of).
        store
            .claim_agent(&AgentClaim::new(
                lead.id,
                run.id,
                None,
                ClaimRole::Lead,
                true,
            ))
            .await
            .unwrap();
        Self {
            store,
            orchestrator,
            host,
            ws,
            checks,
            worktrees,
            run,
            task,
            lead,
            worker,
            _home: home,
        }
    }

    async fn task(&self) -> Task {
        self.store.get_task(self.task.id).await.unwrap()
    }

    async fn run(&self) -> Run {
        self.store.get_run(self.run.id).await.unwrap()
    }

    /// Waits for the background verification to settle the task.
    async fn settled(&self) -> Task {
        for _ in 0..200 {
            let task = self.task().await;
            if task.status != TaskStatus::Verifying {
                return task;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("the task is still verifying: {:?}", self.task().await);
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

/// The `short` id a run's directory is named by (eight hex digits of the id).
fn short(id: &RunId) -> String {
    let s = id.to_string().replace('-', "");
    s[s.len() - 8..].to_owned()
}

// ---------------------------------------------------------------------------------------
// 1. A verification that cannot finish leaves Verifying, as an unverified report

#[tokio::test]
async fn a_verification_whose_commit_fails_leaves_verifying() {
    let f = Fixture::new(&["cargo test"]).await;
    f.ws.cannot_commit.store(true, Ordering::SeqCst);

    f.orchestrator
        .complete(&f.worker, f.task.id, "wrote a".into())
        .await
        .unwrap();
    let task = f.settled().await;

    assert_eq!(task.status, TaskStatus::Passed, "{task:?}");
    let failure = task.report.failure.as_deref().unwrap_or_default();
    assert!(failure.contains("could not run"), "{failure}");
    assert!(failure.contains("index.lock"), "{failure}");
    assert!(task.report.verification.is_none(), "{task:?}");
    assert!(task.report.verified_sha.is_none(), "{task:?}");
    assert!(task.report.checks.is_empty(), "{task:?}");

    // Not stuck: a plain accept is refused (it counts as a failed check), and the issuer can
    // rework it or drop it.
    let refused = f
        .orchestrator
        .task_accept(&f.lead, json!({ "task": "a" }))
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("not verified"), "{refused}");
    let reworked = f
        .orchestrator
        .task_rework(&f.lead, json!({ "task": "a", "notes": "try again" }))
        .await
        .unwrap();
    assert_eq!(reworked["task"]["status"], "running", "{reworked}");
}

#[tokio::test]
async fn a_verification_whose_worktree_is_gone_leaves_verifying_and_can_be_dropped() {
    let f = Fixture::new(&["cargo test"]).await;
    let mut task = f.task().await;
    task.worktree = None;
    f.store.update_task(&task).await.unwrap();

    f.orchestrator
        .complete(&f.worker, f.task.id, "wrote a".into())
        .await
        .unwrap();
    let task = f.settled().await;

    assert_eq!(task.status, TaskStatus::Passed, "{task:?}");
    assert!(
        task.report
            .failure
            .as_deref()
            .is_some_and(|f| f.contains("no worktree")),
        "{task:?}"
    );
    // ...or drop it.
    let dropped = f
        .orchestrator
        .task_drop(&f.lead, json!({ "task": "a" }))
        .await
        .unwrap();
    assert_eq!(dropped["task"]["status"], "cancelled", "{dropped}");
}

#[tokio::test]
async fn a_second_verification_that_fails_also_leaves_verifying() {
    let f = Fixture::new(&["cargo test"]).await;
    // A report that already waits, as after a restart.
    let mut task = f.task().await;
    task.status = TaskStatus::Passed;
    task.report.interrupted = true;
    task.report.summary = Some("wrote a".into());
    f.store.update_task(&task).await.unwrap();
    f.ws.cannot_commit.store(true, Ordering::SeqCst);

    let asked = f
        .orchestrator
        .task_verify(&f.lead, json!({ "task": "a" }))
        .await
        .unwrap();
    assert_eq!(asked["status"], "verifying", "{asked}");
    let task = f.settled().await;

    assert_eq!(task.status, TaskStatus::Passed, "{task:?}");
    assert!(
        task.report
            .failure
            .as_deref()
            .is_some_and(|f| f.contains("could not run")),
        "{task:?}"
    );
    // Its issuer is told what to decide, so the report does not sit there silently. The report
    // itself is not handed over: what needs deciding is the notice.
    f.host.wait_for_notice("nothing verified it").await;
    // The verification holds the run lock until it has queued all its relays.
    drop(f.orchestrator.lock(f.run.id).await);
    f.orchestrator.drain_relays().await;
    assert!(f.host.delivered_to(f.lead.id).await.is_empty());
    let told = f.host.notices.lock().await.join("\n");
    assert!(told.contains("task_verify"), "{told}");
}

// ---------------------------------------------------------------------------------------
// 2. A restart settles Verifying whether or not the worker is busy

#[tokio::test]
async fn a_restart_settles_verifying_even_with_continuations_and_a_busy_worker() {
    let f = Fixture::new(&["cargo test"]).await;
    let mut run = f.run().await;
    run.config.policy = serde_json::from_value::<RunPolicy>(json!({ "continuation": {} })).unwrap();
    f.store.update_run(&run).await.unwrap();
    f.host.busy.store(true, Ordering::SeqCst);
    // A second task still running: the one that must keep its own rule (busy + continuation
    // leaves it alone).
    let mut running = f.task().await;
    running.id = TaskId::new();
    running.key = "b".into();
    running.status = TaskStatus::Running;
    running.worktree = None;
    let running_id = running.id;
    f.store.insert_task(&running).await.unwrap();
    let mut verifying = f.task().await;
    verifying.status = TaskStatus::Verifying;
    verifying.report.verification = Some("token".into());
    f.store.update_task(&verifying).await.unwrap();

    f.orchestrator.resume().await.unwrap();

    let settled = f.store.get_task(verifying.id).await.unwrap();
    assert_eq!(settled.status, TaskStatus::Passed, "{settled:?}");
    assert!(settled.report.interrupted, "{settled:?}");
    assert!(settled.report.verification.is_none(), "{settled:?}");
    // The busy worker's running task is left for the continuation clock, as before.
    assert_eq!(
        f.store.get_task(running_id).await.unwrap().status,
        TaskStatus::Running
    );
}

// ---------------------------------------------------------------------------------------
// 3. A run is not cancelled under a verification

#[tokio::test]
async fn a_run_is_not_cancelled_while_a_task_is_verified() {
    let f = Fixture::new(&["cargo test"]).await;
    let mut task = f.task().await;
    task.status = TaskStatus::Verifying;
    task.report.verification = Some("token".into());
    f.store.update_task(&task).await.unwrap();

    let refused = f.orchestrator.cancel(f.run.id).await.unwrap_err();
    let why = refused.to_string();
    assert!(why.contains("still being verified"), "{why}");
    assert!(why.contains("then cancel the run"), "{why}");
    // Nothing happened: the task, its checkout and the run are as they were.
    assert_eq!(f.task().await.status, TaskStatus::Verifying);
    assert_eq!(f.run().await.status, RunStatus::Running);
    assert!(
        f.task().await.worktree.unwrap().exists(),
        "the checkout the checks run in"
    );
}

// ---------------------------------------------------------------------------------------
// 4. An agent that cannot be let go of keeps its checkout and its build directory

#[tokio::test]
async fn an_agent_that_will_not_go_home_keeps_its_checkout_and_build_directory() {
    let f = Fixture::new(&["cargo test"]).await;
    let dir = f.task().await.worktree.unwrap();
    let target = f.worktrees.join(short(&f.run.id)).join("targets").join("a");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("lib.rlib"), b"built").unwrap();
    f.host.will_not_go_home.store(true, Ordering::SeqCst);

    let mut task = f.task().await;
    let problems = f.orchestrator.release_agents(&mut task).await;
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("Rex"), "{problems:?}");
    f.orchestrator
        .cleanup_worktree(&f.run().await, &task, true, problems.is_empty())
        .await;

    assert!(f.ws.removed().await.is_empty(), "nothing was removed");
    assert!(dir.exists(), "the checkout an agent may still use");
    assert!(target.join("lib.rlib").exists(), "and what it built in");
}

#[tokio::test]
async fn an_agent_that_cannot_be_archived_keeps_its_checkout() {
    let f = Fixture::new(&["cargo test"]).await;
    let dir = f.task().await.worktree.unwrap();
    // An execution session (fresh), which is archived rather than sent home.
    let session = agent("Rex-a", Role::Worker, &f.run.repo);
    f.store.insert_agent(&session).await.unwrap();
    let mut task = f.task().await;
    task.assignee = Some(session.id);
    f.store.update_task(&task).await.unwrap();
    f.store.release_agent(f.worker.id, f.run.id).await.unwrap();
    f.store
        .claim_agent(&AgentClaim::new(
            session.id,
            f.run.id,
            Some(task.id),
            ClaimRole::Worker,
            true,
        ))
        .await
        .unwrap();
    assert!(
        f.store.claim_of(session.id).await.unwrap().is_some(),
        "the session holds the task"
    );
    f.host.will_not_archive.store(true, Ordering::SeqCst);

    let mut task = f.task().await;
    let problems = f.orchestrator.release_agents(&mut task).await;
    assert_eq!(problems.len(), 1, "{problems:?}");
    f.orchestrator
        .cleanup_worktree(&f.run().await, &task, true, problems.is_empty())
        .await;

    assert!(f.ws.removed().await.is_empty(), "nothing was removed");
    assert!(dir.exists(), "the checkout the session may still hold");
}

#[tokio::test]
async fn an_agent_that_goes_home_takes_its_checkout_with_it() {
    let f = Fixture::new(&["cargo test"]).await;
    let dir = f.task().await.worktree.unwrap();
    let mut task = f.task().await;

    let problems = f.orchestrator.release_agents(&mut task).await;
    assert!(problems.is_empty(), "{problems:?}");
    f.orchestrator
        .cleanup_worktree(&f.run().await, &task, true, problems.is_empty())
        .await;

    assert_eq!(f.ws.removed().await, vec![dir], "the control case");
}

// ---------------------------------------------------------------------------------------
// 5. A forced accept reports the commit it merged, not the one the checks saw

#[tokio::test]
async fn a_forced_accept_reports_the_commit_it_merged() {
    let f = Fixture::with_checks(&["cargo test"], vec![false]).await;
    let mut task = f.task().await;
    task.status = TaskStatus::Passed;
    task.report.summary = Some("wrote a".into());
    task.report.checks = vec![crossagents_core::CheckResult {
        command: "cargo test".into(),
        exit_code: Some(1),
        passed: false,
        output: "boom".into(),
    }];
    task.report.verified_sha = Some("aaaaaaa".into());
    f.store.update_task(&task).await.unwrap();
    // What the forced merge commits is not what the checks ran on.
    *f.ws.head.lock().await = Some("bbbbbbb".into());

    let accepted = f
        .orchestrator
        .task_accept(
            &f.lead,
            json!({ "task": "a", "force": true, "reason": "the failure is unrelated" }),
        )
        .await
        .unwrap();

    assert_eq!(accepted["merged"], true, "{accepted}");
    assert_eq!(accepted["as"], "forced", "{accepted}");
    assert_eq!(
        accepted["merged_sha"], "bbbbbbb",
        "the commit the merge brought in, not the verified one: {accepted}"
    );
    assert_eq!(f.ws.merged().await, vec!["crossagents/run-a".to_owned()]);
}

// ---------------------------------------------------------------------------------------
// 6. Reporting again after a pass: a new round of work, or the same report again

#[tokio::test]
async fn reporting_new_work_after_a_pass_checks_it_again_and_relays_the_new_summary() {
    let f = Fixture::new(&["cargo test"]).await;
    *f.ws.head.lock().await = Some("1111111".into());
    f.orchestrator
        .complete(&f.worker, f.task.id, "wrote a".into())
        .await
        .unwrap();
    let first = f.settled().await;
    assert_eq!(first.status, TaskStatus::Passed, "{first:?}");
    assert_eq!(first.report.verified_sha.as_deref(), Some("1111111"));
    assert_eq!(*f.checks.rounds.lock().await, 1);
    f.host.wait_for_notice("waits for your acceptance").await;

    // The worker keeps working and reports the work it did after the checks.
    *f.ws.head.lock().await = Some("2222222".into());
    f.orchestrator
        .complete(&f.worker, f.task.id, "also fixed b".into())
        .await
        .unwrap();
    let second = f.settled().await;

    assert_eq!(second.status, TaskStatus::Passed, "{second:?}");
    assert_eq!(second.report.summary.as_deref(), Some("also fixed b"));
    assert_eq!(
        second.report.verified_sha.as_deref(),
        Some("2222222"),
        "the new work is what was checked"
    );
    assert!(second.report.failure.is_none(), "{second:?}");
    assert_eq!(
        *f.checks.rounds.lock().await,
        2,
        "a new round, checked again"
    );
    f.host.wait_for_notice("also fixed b").await;
    let told = f.host.notices.lock().await.clone();
    assert!(
        told.iter()
            .filter(|n| n.contains("waits for your acceptance"))
            .last()
            .is_some_and(|n| n.contains("also fixed b")),
        "the issuer's notice carries the new summary: {told:?}"
    );
}

#[tokio::test]
async fn reporting_the_same_report_again_does_not_check_it_again() {
    let f = Fixture::new(&["cargo test"]).await;
    *f.ws.head.lock().await = Some("1111111".into());
    f.orchestrator
        .complete(&f.worker, f.task.id, "wrote a".into())
        .await
        .unwrap();
    let first = f.settled().await;
    assert_eq!(first.status, TaskStatus::Passed, "{first:?}");
    // The status is stored before the asynchronous issuer notification is delivered.
    // Wait for that first notification before measuring whether a retry sends another.
    f.host.wait_for_notice("waits for your acceptance").await;

    // The same call again, with nothing done since: a retried call, not a new round.
    f.orchestrator
        .complete(&f.worker, f.task.id, "wrote a".into())
        .await
        .unwrap();
    // The retry takes the run lock after the verifier has queued its relays.
    f.orchestrator.drain_relays().await;
    let again = f.task().await;

    assert_eq!(again.status, TaskStatus::Passed, "{again:?}");
    assert_eq!(again.report.summary.as_deref(), Some("wrote a"));
    assert_eq!(again.report.verified_sha.as_deref(), Some("1111111"));
    assert_eq!(*f.checks.rounds.lock().await, 1, "nothing to check again");
    let accepts = f
        .host
        .notices
        .lock()
        .await
        .iter()
        .filter(|n| n.contains("waits for your acceptance"))
        .count();
    assert_eq!(accepts, 1, "its issuer is not told the same report twice");
}

#[tokio::test]
async fn uncommitted_work_after_a_pass_is_checked_again_even_with_the_same_summary() {
    let f = Fixture::new(&["cargo test"]).await;
    *f.ws.head.lock().await = Some("1111111".into());
    f.orchestrator
        .complete(&f.worker, f.task.id, "wrote a".into())
        .await
        .unwrap();
    assert_eq!(f.settled().await.status, TaskStatus::Passed);

    // A fix left in the checkout, not committed: the old checks did not see it.
    f.ws.dirty.lock().await.push("src/lib.rs".into());
    f.orchestrator
        .complete(&f.worker, f.task.id, "wrote a".into())
        .await
        .unwrap();
    let second = f.settled().await;

    assert_eq!(second.status, TaskStatus::Passed, "{second:?}");
    assert_eq!(*f.checks.rounds.lock().await, 2);
}

#[tokio::test]
async fn deleting_after_release_failure_keeps_checkout_and_targets() {
    let f = Fixture::new(&[]).await;
    let dir = f.task().await.worktree.unwrap();
    let target = f.worktrees.join(short(&f.run.id)).join("targets/a");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("cache"), "compiled").unwrap();
    f.host.will_not_go_home.store(true, Ordering::SeqCst);
    let mut run = f.run().await;
    run.status = RunStatus::Cancelled;
    f.store.update_run(&run).await.unwrap();
    let done = f
        .orchestrator
        .delete_run(run.id, crate::DeleteRun::default())
        .await
        .unwrap();
    assert!(!done.warnings.is_empty());
    assert!(f.ws.removed().await.is_empty());
    assert!(dir.exists());
    assert!(target.join("cache").exists());
}

/// run-cleanup ①: a run that ended loses its build directories, except the tree of a task
/// still being verified, whose checks may be writing there.
#[tokio::test]
async fn a_finished_run_loses_its_targets_but_not_a_verifying_tree() {
    let f = Fixture::new(&[]).await;
    let targets = f.worktrees.join(short(&f.run.id)).join("targets");
    for tree in ["a", "integration", "gone"] {
        std::fs::create_dir_all(targets.join(tree)).unwrap();
        std::fs::write(targets.join(tree).join("cache"), "compiled").unwrap();
    }
    let mut task = f.task().await;
    task.status = TaskStatus::Verifying;
    f.store.update_task(&task).await.unwrap();

    // Still open: nothing goes.
    f.orchestrator.remove_finished_targets(&f.run().await).await;
    assert!(targets.join("integration/cache").exists());

    let mut run = f.run().await;
    run.status = RunStatus::Completed;
    f.store.update_run(&run).await.unwrap();
    f.orchestrator.remove_finished_targets(&run).await;
    assert!(targets.join("a/cache").exists(), "#a is being verified");
    assert!(!targets.join("integration").exists());
    assert!(!targets.join("gone").exists());
    assert!(
        task.worktree.unwrap().exists(),
        "only build directories go, not checkouts"
    );

    // Its checks done, the last one goes too.
    let mut task = f.task().await;
    task.status = TaskStatus::Passed;
    f.store.update_task(&task).await.unwrap();
    f.orchestrator.remove_finished_targets(&run).await;
    assert!(!targets.exists());
}

/// run-cleanup ③: at startup, a `<short>` directory no run owns (a delete Windows did not let
/// finish) goes, and so do the build directories of runs that ended; an open run's, an
/// agent's directory and a tree being verified stay.
#[tokio::test]
async fn the_startup_sweep_retries_what_a_cleanup_left_behind() {
    let f = Fixture::new(&[]).await;
    let orphan = f.worktrees.join("deadbeef");
    std::fs::create_dir_all(orphan.join("integration/src")).unwrap();
    std::fs::create_dir_all(orphan.join("targets/integration")).unwrap();
    // An agent still lives in another orphaned directory: never pulled out from under it.
    let lived_in = f.worktrees.join("cafef00d").join("integration");
    std::fs::create_dir_all(&lived_in).unwrap();
    let mut resident = agent("Mo", Role::Worker, &f.run.repo);
    resident.cwd = lived_in.clone();
    f.store.insert_agent(&resident).await.unwrap();
    // The fixture's run is open: its build directory stays.
    let open_target = f.worktrees.join(short(&f.run.id)).join("targets/a");
    std::fs::create_dir_all(&open_target).unwrap();
    // A run that ended, with one tree still being verified.
    let mut ended = f.run().await;
    ended.id = RunId::new();
    ended.status = RunStatus::Failed;
    ended.branch = "crossagents/ended".into();
    f.store.insert_run(&ended).await.unwrap();
    let ended_targets = f.worktrees.join(short(&ended.id)).join("targets");
    let mut verifying = f.task().await;
    verifying.id = TaskId::new();
    verifying.run_id = ended.id;
    verifying.key = "v".into();
    verifying.status = TaskStatus::Verifying;
    verifying.worktree = Some(f.worktrees.join(short(&ended.id)).join("v"));
    f.store.insert_task(&verifying).await.unwrap();
    for tree in ["v", "integration"] {
        std::fs::create_dir_all(ended_targets.join(tree)).unwrap();
    }

    f.orchestrator.sweep_worktrees().await;

    assert!(!orphan.exists(), "no run owns it");
    assert!(lived_in.exists(), "an agent's directory stays");
    assert!(open_target.exists(), "an open run's build directory stays");
    assert!(
        ended_targets.join("v").exists(),
        "a tree being verified stays"
    );
    assert!(!ended_targets.join("integration").exists());
}

/// USAGE-TRIM §3.3: a child run that ended before its start notice was written still leaves
/// its result for the source chat, not a stale "started"; nothing wakes the chat.
#[tokio::test]
async fn a_run_that_ended_before_its_start_notice_leaves_its_result() {
    let f = Fixture::new(&[]).await;
    let chat = AgentId::new();
    let mut run = f.run.clone();
    run.origin_chat_id = Some(chat);
    let started = run.clone();
    run.status = RunStatus::Completed;
    run.summary = Some("Added notes.txt".into());
    f.store.update_run(&run).await.unwrap();

    f.orchestrator.tell_origin_started(&started).await;

    let later = f.host.later.lock().await;
    let line = &later[&(chat, format!("run:{}", run.id))];
    assert!(
        line.contains("completed") && line.contains("Added notes.txt"),
        "{line}"
    );
    assert!(!line.contains("started"), "{line}");
    assert!(f.host.delivered_to(chat).await.is_empty());
}

/// USAGE-TRIM §3.3: while the run goes on the chat keeps the start notice, and the closing
/// result replaces it on the same topic.
#[tokio::test]
async fn the_closing_result_replaces_the_start_notice() {
    let f = Fixture::new(&[]).await;
    let chat = AgentId::new();
    let mut run = f.run.clone();
    run.origin_chat_id = Some(chat);
    f.store.update_run(&run).await.unwrap();

    f.orchestrator.tell_origin_started(&run).await;
    let key = (chat, format!("run:{}", run.id));
    assert!(f.host.later.lock().await[&key].contains("work started"));

    run.status = RunStatus::Completed;
    run.summary = Some("Added notes.txt".into());
    f.orchestrator.tell_origin_later(&run).await;
    let line = f.host.later.lock().await[&key].clone();
    assert!(
        line.contains("completed") && !line.contains("work started"),
        "{line}"
    );
    assert!(f.host.delivered_to(chat).await.is_empty());
}

/// A run that ends leaves its history in the workspace, and deleting it afterwards keeps that
/// record (docs/tasks/R1.md §3): runs do not come back, what they did stays readable.
#[tokio::test]
async fn a_finished_run_leaves_its_history_behind() {
    let f = Fixture::new(&[]).await;
    let mut run = f.run().await;
    run.summary = Some("did a".into());
    f.store.update_run(&run).await.unwrap();

    f.orchestrator
        .set_run_status(&mut run, RunStatus::Completed, None)
        .await
        .unwrap();

    let history = f.store.list_run_history(Some(&run.repo)).await.unwrap();
    assert_eq!(history.len(), 1);
    let entry = &history[0];
    assert_eq!(entry.run_id, run.id);
    assert_eq!(entry.status, RunStatus::Completed);
    assert_eq!(entry.summary.as_deref(), Some("did a"));
    assert!(entry.ended_at.is_some(), "{entry:?}");
    // Its task as it was, with the worker's name rather than an id that would mean nothing.
    assert_eq!(entry.tasks.len(), 1);
    assert_eq!(entry.tasks[0].key, "a");
    assert_eq!(entry.tasks[0].assignee.as_deref(), Some("Rex"));
    assert!(!entry.deleted);

    // The landing is part of what it did: a later write keeps the record, with the landing.
    run.landed = Some(crossagents_core::Landing {
        mode: crossagents_core::LandMode::Merge,
        by: "Ada".into(),
        reference: "c0ffee1".into(),
        at: Utc::now(),
    });
    f.store.update_run(&run).await.unwrap();
    f.orchestrator.record_history(&run).await;
    let history = f.store.list_run_history(Some(&run.repo)).await.unwrap();
    assert_eq!(history.len(), 1, "one record, updated in place");
    assert_eq!(
        history[0].landed.as_ref().map(|l| l.reference.as_str()),
        Some("c0ffee1")
    );

    // Deleting the run keeps the record, marked as a run that is gone.
    f.store.delete_run(run.id).await.unwrap();
    let history = f.store.list_run_history(Some(&run.repo)).await.unwrap();
    assert_eq!(history.len(), 1);
    assert!(history[0].deleted);
    assert_eq!(history[0].tasks.len(), 1);
}

#[tokio::test]
async fn a_repeated_review_is_refused_and_changes_nothing() {
    use crate::review::Submission;
    let f = Fixture::new(&[]).await;
    let reviewer = agent("critic", Role::Reviewer, f._home.path());
    // A task in review round 1, reading the verified commit `abc`.
    let mut task = f.task().await;
    task.status = TaskStatus::Reviewing;
    task.reviewer = Some(reviewer.id);
    task.report.verified_sha = Some("abc".into());
    task.report.reviewed_sha = Some("abc".into());
    task.report.review_attempt = 1;
    f.store.update_task(&task).await.unwrap();

    // The first verdict lands.
    f.orchestrator
        .review(
            &reviewer,
            task.id,
            Submission {
                approve: true,
                notes: "lgtm".into(),
                findings: Vec::new(),
                verdict: None,
                reviewed_sha: Some("abc".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(f.task().await.status, TaskStatus::Passed);

    // The same round again, with a change request this time: refused, nothing moves.
    let finding = crate::review::FindingInput {
        id: None,
        severity: crossagents_core::model::ReviewSeverity::P0,
        confidence: 0.9,
        file: None,
        line: None,
        title: "late".into(),
        detail: None,
        blocking: None,
        basis: None,
        verify: None,
    }
    .into_finding();
    let err = f
        .orchestrator
        .review(
            &reviewer,
            task.id,
            Submission {
                approve: false,
                notes: "again".into(),
                findings: vec![finding],
                verdict: None,
                reviewed_sha: Some("abc".into()),
            },
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("already came in"), "{err}");
    let after = f.task().await;
    assert_eq!(after.status, TaskStatus::Passed);
    assert_eq!(after.report.review.as_deref(), Some("lgtm"));
    assert_eq!(after.report.review_returns, 0);
    assert_eq!(after.attempts, task.attempts);
}

// ---------------------------------------------------------------------------------------
// 9. A review is bound to its commit; a reviewed task is accepted only on that approval; the
//    acceptance notice is settled on every path that leaves it; a restart gives the work back

/// A task in review round `round`, reading `commit`, with `reviewer` as its reviewer.
async fn in_review(f: &Fixture, reviewer: &Agent, round: u32, commit: &str) {
    let mut task = f.task().await;
    task.status = TaskStatus::Reviewing;
    task.reviewer = Some(reviewer.id);
    task.report.verified_sha = Some(commit.into());
    task.report.reviewed_sha = Some(commit.into());
    task.report.review_attempt = round;
    f.store.update_task(&task).await.unwrap();
}

#[tokio::test]
async fn a_verdict_that_names_no_commit_is_refused_and_changes_nothing() {
    use crate::review::Submission;
    let f = Fixture::new(&[]).await;
    let reviewer = agent("critic", Role::Reviewer, f._home.path());
    in_review(&f, &reviewer, 1, "abc").await;

    let err = f
        .orchestrator
        .review(
            &reviewer,
            f.task.id,
            Submission {
                approve: true,
                notes: "lgtm".into(),
                findings: Vec::new(),
                verdict: None,
                reviewed_sha: None,
            },
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("say which commit you reviewed"), "{err}");
    let after = f.task().await;
    assert_eq!(after.status, TaskStatus::Reviewing);
    assert_eq!(after.reviewer, Some(reviewer.id));
    assert!(after.report.review.is_none(), "{after:?}");
}

#[tokio::test]
async fn a_late_verdict_for_an_earlier_commit_is_refused_in_the_next_round() {
    use crate::review::Submission;
    let f = Fixture::new(&[]).await;
    let reviewer = agent("critic", Role::Reviewer, f._home.path());
    // Round 2 reads `new`; a verdict that was written for round 1's `old` must not land on it.
    in_review(&f, &reviewer, 2, "new").await;

    let err = f
        .orchestrator
        .review(
            &reviewer,
            f.task.id,
            Submission {
                approve: true,
                notes: "late".into(),
                findings: Vec::new(),
                verdict: None,
                reviewed_sha: Some("old".into()),
            },
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("stale"), "{err}");
    let after = f.task().await;
    assert_eq!(after.status, TaskStatus::Reviewing);
    assert!(after.report.review.is_none(), "{after:?}");
}

#[tokio::test]
async fn a_reviewed_task_is_accepted_plainly_only_on_the_commit_its_review_approved() {
    let f = Fixture::with_checks(&["cargo test"], vec![true]).await;
    let mut task = f.task().await;
    task.status = TaskStatus::Passed;
    task.report.summary = Some("wrote a".into());
    task.report.checks = vec![crossagents_core::CheckResult {
        command: "cargo test".into(),
        exit_code: Some(0),
        passed: true,
        output: String::new(),
    }];
    task.report.verified_sha = Some("aaaaaaa".into());
    task.report.review_attempt = 1;
    task.report.verdict = Some(crossagents_core::model::ReviewVerdict::Merge);
    // The review read another commit than the one the checks verified.
    task.report.reviewed_sha = Some("bbbbbbb".into());
    f.store.update_task(&task).await.unwrap();
    *f.ws.head.lock().await = Some("aaaaaaa".into());

    let err = f
        .orchestrator
        .task_accept(&f.lead, json!({ "task": "a" }))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("its review did not approve this commit"),
        "{err}"
    );

    // The review read the verified commit: a plain accept goes through.
    task.report.reviewed_sha = Some("aaaaaaa".into());
    f.store.update_task(&task).await.unwrap();
    let accepted = f
        .orchestrator
        .task_accept(&f.lead, json!({ "task": "a" }))
        .await
        .unwrap();
    assert_eq!(accepted["merged"], true, "{accepted}");
    assert_eq!(accepted["as"], "verified", "{accepted}");
}

/// The acceptance notices of the task, in the order they were given.
async fn accept_notices(f: &Fixture) -> Vec<String> {
    let prefix = format!("task:{}:accept: ", f.task.id);
    f.host
        .notices
        .lock()
        .await
        .iter()
        .filter(|n| n.starts_with(&prefix))
        .cloned()
        .collect()
}

#[tokio::test]
async fn a_task_that_waited_to_be_accepted_gets_its_notice_again_after_a_restart() {
    let f = Fixture::new(&[]).await;
    let mut task = f.task().await;
    task.status = TaskStatus::Passed;
    task.report.summary = Some("wrote a".into());
    task.report.verified_sha = Some("aaaaaaa".into());
    f.store.update_task(&task).await.unwrap();

    f.orchestrator.resume().await.unwrap();

    let notices = accept_notices(&f).await;
    assert!(
        notices.last().is_some_and(|n| n.contains("#a waits for your acceptance")),
        "{notices:?}"
    );
}

#[tokio::test]
async fn a_review_whose_reviewer_is_gone_after_a_restart_is_the_lead_s_decision() {
    let f = Fixture::new(&[]).await;
    // The reviewer's session is not in the store any more.
    let reviewer = agent("critic", Role::Reviewer, f._home.path());
    in_review(&f, &reviewer, 1, "abc").await;

    f.orchestrator.resume().await.unwrap();

    let notices = f.host.notices.lock().await.join("\n");
    assert!(notices.contains("its reviewer is gone"), "{notices}");
}

#[tokio::test]
async fn re_reporting_a_waiting_task_settles_its_acceptance_notice() {
    let f = Fixture::with_checks(&[], vec![]).await;
    let mut task = f.task().await;
    task.status = TaskStatus::Passed;
    task.report.summary = Some("wrote a".into());
    task.report.verified_sha = Some("1111111".into());
    f.store.update_task(&task).await.unwrap();
    f.orchestrator
        .ready_to_accept(&f.run, &task, f.lead.id)
        .await;
    assert!(accept_notices(&f).await.last().is_some_and(|n| n.contains("waits for your acceptance")));

    f.orchestrator
        .complete(&f.worker, f.task.id, "wrote again".into())
        .await
        .unwrap();
    f.settled().await;

    // The old notice was settled by the new report, and the check's own result came after it.
    let notices = accept_notices(&f).await;
    let settled = notices.iter().position(|n| n.contains("reported again")).expect("settled");
    assert!(
        notices[settled + 1..].iter().all(|n| !n.contains("#a waits") || n.contains("wrote again")),
        "{notices:?}"
    );
}

#[tokio::test]
async fn sending_a_waiting_task_back_by_hand_settles_its_acceptance_notice() {
    let f = Fixture::new(&[]).await;
    let mut task = f.task().await;
    task.status = TaskStatus::Passed;
    task.report.summary = Some("wrote a".into());
    task.report.verified_sha = Some("1111111".into());
    f.store.update_task(&task).await.unwrap();
    f.orchestrator
        .ready_to_accept(&f.run, &task, f.lead.id)
        .await;

    f.orchestrator
        .review_task(
            f.run.id,
            f.task.id,
            vec![crate::review::Remark {
                path: "a.txt".into(),
                line: Some(1),
                old_side: false,
                text: "say beta".into(),
            }],
            None,
            true,
        )
        .await
        .unwrap();

    let notices = accept_notices(&f).await;
    assert!(
        notices.last().is_some_and(|n| n.contains("sent back to its worker by the person")),
        "{notices:?}"
    );
}
