//! Runs (attempts) of a task, kept apart from the task itself (Multica §2.4–2.5): a run ends
//! when a worker's turn does, and that is not the task ending. Each run is recorded with its
//! start, its end and how it went; a failure is sorted into what the platform can fix by
//! trying again (offline, a timeout, a directory that could not be prepared, the network) and
//! what it cannot (`agent_error`: auth, quota, model). Only the first is retried, up to a cap;
//! a run whose session is unfit to carry on (context overflow) is retried in a new session,
//! in the same directory.

use chrono::Utc;
use crossagents_core::{
    AgentId, EventKind, HandoffOutcome, MessageId, Run, Task, TaskRun, TaskRunOutcome, TaskStatus,
};

use crate::brief;
use crate::engine::Orchestrator;

/// How many times the hub retries a run that failed on the platform's side (multica §2.5).
pub const MAX_INFRA_RETRIES: u32 = 2;
/// A network fault is worth one more try than the rest: it says the least about the task.
pub const MAX_NETWORK_RETRIES: u32 = 3;

/// Why a run failed, as far as retrying is concerned (multica §2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// Offline, a timeout, a directory that could not be prepared: worth another try.
    Infrastructure,
    /// The network went away: worth another try, with a little more patience.
    Network,
    /// The session cannot carry on (context overflow): a new one is worth a try.
    SessionLost,
    /// Auth, quota, model: a retry fails the same way. The task stops and says why.
    AgentError,
    /// Nothing says it is the platform's fault: do not retry blindly.
    Unknown,
}

impl Fault {
    /// How the run is recorded.
    pub fn outcome(self) -> TaskRunOutcome {
        match self {
            Self::Infrastructure | Self::Network => TaskRunOutcome::Infrastructure,
            Self::SessionLost => TaskRunOutcome::SessionLost,
            Self::AgentError | Self::Unknown => TaskRunOutcome::AgentError,
        }
    }

    /// A word for the notice.
    pub fn word(self) -> &'static str {
        match self {
            Self::Infrastructure => "a platform fault",
            Self::Network => "a network fault",
            Self::SessionLost => "a session that could not carry on",
            Self::AgentError => "an agent or account fault",
            Self::Unknown => "a fault the hub cannot place",
        }
    }

    /// Whether trying again may help, and how many tries that is worth.
    fn allowance(self) -> Option<u32> {
        match self {
            Self::Infrastructure | Self::SessionLost => Some(MAX_INFRA_RETRIES),
            Self::Network => Some(MAX_NETWORK_RETRIES),
            Self::AgentError | Self::Unknown => None,
        }
    }
}

/// What to do when a run ends in `fault`, given how many retries it already had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Stop: the task stalls and its issuer decides.
    Stop,
    /// Deliver the task to the same session again, with what went wrong.
    RetrySame,
    /// Replace the worker's session (same directory and worktree) and start again.
    RetryNewSession,
}

/// Whether `error` reads like a fault the platform can fix (`retryable`) or not
/// (multica §2.5). Words, not codes: a driver says what went wrong in its own words.
pub fn classify(error: &str) -> Fault {
    let text = error.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| text.contains(n));
    // First: the session itself is unfit, whatever the vendor calls it.
    if has(&[
        "context length",
        "context window",
        "context overflow",
        "context_limit",
        "too many tokens",
        "prompt is too long",
        "maximum context",
        "上下文",
    ]) {
        return Fault::SessionLost;
    }
    // Then what a retry does not fix (multica §2.5, `agent_error.*`).
    if has(&[
        "agent_error",
        "unauthor",
        "authenticat",
        "forbidden",
        "invalid api key",
        "not logged in",
        "鉴权",
        "quota",
        "usage limit",
        "rate limit",
        "insufficient",
        "credit",
        "额度",
        "余额",
        "model not found",
        "invalid model",
        "unknown model",
    ]) {
        return Fault::AgentError;
    }
    if has(&[
        "network",
        "connection",
        "connect",
        "econnreset",
        "econnrefused",
        "dns",
        "socket",
        "tls",
        "proxy",
        "stream disconnected",
        "网络",
    ]) {
        return Fault::Network;
    }
    if has(&[
        "timeout",
        "timed out",
        "deadline",
        "offline",
        "超时",
        "离线",
        "no such file or directory",
        "cannot create",
        "could not create",
        "cannot prepare",
        "worktree",
        "目录",
    ]) {
        return Fault::Infrastructure;
    }
    Fault::Unknown
}

/// [`classify`], for a run that failed while it was being started (its directory, say).
pub fn start_is_retryable(error: &str) -> bool {
    matches!(
        classify(error),
        Fault::Infrastructure | Fault::Network | Fault::SessionLost
    )
}

/// What to do about `fault` after `retries` retries already.
pub fn action_for(fault: Fault, retries: u32) -> Action {
    match fault.allowance() {
        Some(cap) if retries < cap => match fault {
            Fault::SessionLost => Action::RetryNewSession,
            _ => Action::RetrySame,
        },
        _ => Action::Stop,
    }
}

/// How many retries the runs before the last one were: the ones this failure adds to.
pub fn retries_so_far(task: &Task) -> u32 {
    task.runs
        .iter()
        .rev()
        .skip(1)
        .take_while(|run| run.retried)
        .count() as u32
}

/// Whether `agent`'s turn `turn` is this task's open run: the guard that keeps a duplicate
/// event, or another run's turn, from being taken for this one.
fn open_run(task: &Task, agent: AgentId, turn: Option<&MessageId>) -> bool {
    let Some(run) = task.runs.last() else {
        return false;
    };
    if run.outcome != TaskRunOutcome::Running || run.agent != Some(agent) {
        return false;
    }
    // A run recorded without its message (a retry, a hand-over) takes the next turn.
    match (run.message, turn) {
        (Some(recorded), Some(turn)) => recorded == *turn,
        _ => true,
    }
}

/// Starts a run for `agent`: the task's next entry. One left open (a hand-over whose end was
/// never seen) is closed first, so at most one run is ever open.
pub fn begin(task: &mut Task, agent: Option<AgentId>, message: Option<MessageId>) -> &mut TaskRun {
    if let Some(open) = task.runs.last_mut()
        && open.outcome == TaskRunOutcome::Running
    {
        open.outcome = TaskRunOutcome::NoReport;
        open.ended_at = Some(Utc::now());
        open.note = Some("a new run started".into());
    }
    let index = task.runs.len() as u32 + 1;
    task.runs.push(TaskRun {
        index,
        started_at: Utc::now(),
        ended_at: None,
        outcome: TaskRunOutcome::Running,
        agent,
        message,
        note: None,
        retried: false,
    });
    task.runs.last_mut().expect("just pushed")
}

/// Closes the open run: how it ended, why, and whether a retry follows. `false` when there
/// is no open run (a duplicate event, or a task from before runs were kept).
pub fn close(
    task: &mut Task,
    outcome: TaskRunOutcome,
    note: Option<String>,
    retried: bool,
) -> bool {
    let Some(run) = task.runs.last_mut() else {
        return false;
    };
    if run.outcome != TaskRunOutcome::Running {
        return false;
    }
    run.outcome = outcome;
    run.ended_at = Some(Utc::now());
    run.note = note.map(|note| clip(&note));
    run.retried = retried;
    true
}

/// A run or a note is a line, not a transcript: keep it readable.
fn clip(text: &str) -> String {
    let text = text.trim();
    let mut end = text.len().min(600);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = text[..end].to_owned();
    if end < text.len() {
        out.push('…');
    }
    out
}

impl Orchestrator {
    /// A worker's turn ended while its task still waits for a report (engine's `on_idle`).
    /// The run is recorded; a platform fault is retried, an `agent_error` stops the task and
    /// says why (multica §2.5).
    pub(crate) async fn turn_ended(
        &self,
        run: &Run,
        task: &mut Task,
        agent: AgentId,
        turn: Option<MessageId>,
        error: Option<String>,
    ) {
        let issuer = task.issuer_or(run.planner);
        let name = self
            .name_of(agent)
            .await
            .unwrap_or_else(|| agent.to_string());
        if task.runs.is_empty() {
            // A task recorded before runs were kept: the old rule, unchanged.
            self.stall(run, task, &name, error, None).await;
            return;
        }
        if !open_run(task, agent, turn.as_ref()) {
            // A duplicate of an event already handled, or another run's turn.
            return;
        }
        let Some(error) = error else {
            close(task, TaskRunOutcome::NoReport, None, false);
            self.stall(run, task, &name, None, None).await;
            return;
        };
        let fault = classify(&error);
        let retries = retries_so_far(task);
        let action = action_for(fault, retries);
        close(
            task,
            fault.outcome(),
            Some(error.clone()),
            action != Action::Stop,
        );
        let run_no = task.runs.len() as u32;
        match action {
            Action::Stop => {
                let why = match fault.allowance() {
                    Some(cap) => {
                        format!("it was retried {cap} time(s) already; its allowance is used up")
                    }
                    None => "a retry would fail the same way".into(),
                };
                self.stall(run, task, &name, Some(error), Some(why)).await;
            }
            Action::RetrySame => {
                self.retry_same(run, task, agent, issuer, fault, &error, run_no, retries + 1)
                    .await;
            }
            Action::RetryNewSession => {
                self.retry_new_session(run, task, issuer, fault, &error, run_no, retries + 1)
                    .await;
            }
        }
    }

    /// The run stops here: the task is `Stalled` and its issuer reads why (the rule before
    /// runs were kept, now saying whether a retry was even possible).
    async fn stall(
        &self,
        run: &Run,
        task: &mut Task,
        name: &str,
        error: Option<String>,
        why_not: Option<String>,
    ) {
        let issuer = task.issuer_or(run.planner);
        // The task's failure says why it stopped, not just that a turn ended.
        let mut related = String::new();
        if let Some(error) = &error {
            task.report.failure = Some(error.clone());
            related = self.attach_related(&run.repo, task, error).await;
        }
        if task.status == TaskStatus::Running {
            if let Err(err) = self.transition(task, TaskStatus::Stalled).await {
                tracing::error!(task = %task.key, error = %err, "cannot mark a task stalled");
                return;
            }
        } else {
            // Already out of `Running` (the report raced the turn's end): only the record goes.
            task.updated_at = Utc::now();
            if let Err(err) = self.store.update_task(task).await {
                tracing::error!(task = %task.key, error = %err, "cannot record a finished run");
                return;
            }
        }
        let line = match (error, why_not) {
            (Some(error), Some(why)) => format!(
                "Task {} is stalled: {name}'s turn failed before it could report: {error}\nIt is not retried automatically: {why}. Fix it, or task_assign the task to someone else.",
                brief::task_ref(task)
            ),
            (Some(error), None) => format!(
                "Task {} is stalled: {name}'s turn failed before it could report: {error}\nA nudge fails the same way while the cause stays: fix it, or task_assign the task to someone else.",
                brief::task_ref(task)
            ),
            _ => format!(
                "Task {} is stalled: {name} ended a turn without reporting.",
                brief::task_ref(task)
            ),
        };
        self.notice(issuer, &format!("task:{}", task.id), &format!("{line}{related}"))
            .await;
    }

    /// The fault is one the platform can fix: the same session gets the task again, with what
    /// went wrong. The run after this one is the retry.
    async fn retry_same(
        &self,
        run: &Run,
        task: &mut Task,
        agent: AgentId,
        issuer: AgentId,
        fault: Fault,
        error: &str,
        run_no: u32,
        retry: u32,
    ) {
        let cap = fault.allowance().unwrap_or(MAX_INFRA_RETRIES);
        let text = format!(
            "Your last turn on {} failed before it reported: {}\n\nThe hub is retrying it ({retry}/{cap}): {}. The task is still yours and your worktree is as you left it. Carry on and call task_complete when it is done, or task_ask if you are stuck.",
            brief::task_ref(task),
            clip(error),
            fault.word()
        );
        let envelope = self.relay_envelope(issuer, text);
        begin(task, Some(agent), Some(envelope.id));
        task.progress = Some(format!(
            "run {run_no} failed ({}); retrying it with the same session ({retry}/{cap})",
            fault.word()
        ));
        task.updated_at = Utc::now();
        if let Err(err) = self.store.update_task(task).await {
            tracing::error!(task = %task.key, error = %err, "cannot record a retry");
            return;
        }
        self.publish(EventKind::TaskProgress {
            run_id: run.id,
            id: task.id,
            key: task.key.clone(),
            text: task.progress.clone().unwrap_or_default(),
        })
        .await;
        self.send_in_background(agent, envelope);
        self.notice(
            issuer,
            &format!("task:{}", task.id),
            &format!(
                "Task {}: its run failed on {}; retrying ({retry}/{cap}) in the same session.",
                brief::task_ref(task),
                fault.word()
            ),
        )
        .await;
    }

    /// The session is unfit to carry on (context overflow): a new one takes the task in the
    /// same directory, and the worktree is kept as it is (multica §2.5).
    async fn retry_new_session(
        &self,
        run: &Run,
        task: &mut Task,
        issuer: AgentId,
        fault: Fault,
        error: &str,
        run_no: u32,
        retry: u32,
    ) {
        let cap = fault.allowance().unwrap_or(MAX_INFRA_RETRIES);
        let old = task.assignee.take();
        crossagents_core::Handoff::end(&mut task.handoffs, old, HandoffOutcome::Failed);
        if let Some(old) = old
            && let Err(err) = self.let_go(run.id, old).await
        {
            tracing::warn!(task = %task.key, error = %err, "cannot let go of the polluted session");
        }
        // A new execution session, in the worktree that is already there.
        task.worker_fresh = true;
        task.worker_restore = false;
        task.progress = Some(format!(
            "run {run_no} failed ({fault_word}); starting a new session in the same directory ({retry}/{cap})",
            fault_word = fault.word()
        ));
        task.updated_at = Utc::now();
        if let Err(err) = self.transition(task, TaskStatus::Pending).await {
            tracing::error!(task = %task.key, error = %err, "cannot requeue a task for a new session");
            return;
        }
        match self.dispatch(run, task).await {
            Ok(()) => {
                self.notice(
                    issuer,
                    &format!("task:{}", task.id),
                    &format!(
                        "Task {}: its session could not carry on ({}); a new session has the task in the same directory (retry {retry}/{cap}).",
                        brief::task_ref(task),
                        clip(error)
                    ),
                )
                .await;
            }
            Err(err) => {
                // The new session could not start: it is the issuer's now, told why.
                let why = format!("a new session could not take it over: {err}");
                task.report.failure = Some(why.clone());
                let related = self
                    .attach_related(&run.repo, task, &format!("{error}\n{why}"))
                    .await;
                self.force_stalled(run, task).await;
                self.notice(
                    issuer,
                    &format!("task:{}", task.id),
                    &format!("Task {} is stalled: {why}{related}", brief::task_ref(task)),
                )
                .await;
            }
        }
    }

    /// Leaves the task `Stalled` when no transition leads there (it is `Pending`).
    async fn force_stalled(&self, run: &Run, task: &mut Task) {
        let from = task.status;
        task.status = TaskStatus::Stalled;
        task.updated_at = Utc::now();
        if let Err(err) = self.store.update_task(task).await {
            tracing::error!(task = %task.key, error = %err, "cannot mark a task stalled");
            return;
        }
        self.publish(EventKind::TaskStatusChanged {
            run_id: run.id,
            id: task.id,
            key: task.key.clone(),
            from,
            to: TaskStatus::Stalled,
            assignee: task.assignee,
        })
        .await;
    }
}

/// A run that could not start at all (its directory, say): recorded as one that failed, so
/// the count and the reason show in the task's history (multica §2.4–2.5).
pub fn record_start_failure(task: &mut Task, error: &str, retried: bool) {
    let fault = classify(error);
    if let Some(open) = task.runs.last_mut()
        && open.outcome == TaskRunOutcome::Running
    {
        open.outcome = TaskRunOutcome::NoReport;
        open.ended_at = Some(Utc::now());
        open.note = Some("a run could not start".into());
    }
    let index = task.runs.len() as u32 + 1;
    task.runs.push(TaskRun {
        index,
        started_at: Utc::now(),
        ended_at: Some(Utc::now()),
        outcome: fault.outcome(),
        agent: None,
        message: None,
        note: Some(clip(error)),
        retried,
    });
}

/// The task is ending under its open run (cancelled, dropped, its run cancelled): close it
/// so the history says so rather than leaving it running forever.
pub fn close_open(task: &mut Task, outcome: TaskRunOutcome, note: Option<String>) {
    let _ = close(task, outcome, note, false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossagents_core::{TaskId, TaskReport, TaskSpec};

    fn task() -> Task {
        let now = Utc::now();
        Task {
            id: TaskId::new(),
            run_id: crossagents_core::RunId::new(),
            key: "a".into(),
            title: "t".into(),
            spec: TaskSpec::default(),
            depends_on: Vec::new(),
            issuer: None,
            assignee: None,
            reviewer: None,
            worker_template: None,
            reviewer_template: None,
            worker_fresh: true,
            worker_restore: false,
            reviewer_fresh: true,
            status: TaskStatus::Running,
            attempts: 1,
            runs: Vec::new(),
            escalated: false,
            nudges: 0,
            branch: None,
            worktree: None,
            report: TaskReport::default(),
            parent: None,
            progress: None,
            handoffs: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn platform_faults_are_retried_and_agent_errors_are_not() {
        for error in [
            "request timed out after 600s",
            "the host is offline",
            "could not create the worktree directory",
            "connection reset by peer",
            "stream disconnected before completion",
        ] {
            let fault = classify(error);
            assert!(fault.allowance().is_some(), "{error}: {fault:?}");
        }
        for error in [
            "agent_error: invalid api key",
            "401 unauthorized",
            "your quota is exhausted",
            "model not found: gpt-9",
            "账户额度不足",
        ] {
            assert_eq!(classify(error), Fault::AgentError, "{error}");
        }
        // No retry without a reason to think it helps.
        assert_eq!(classify("the worker gave up"), Fault::Unknown);
    }

    #[test]
    fn a_polluted_session_is_told_apart() {
        for error in [
            "context length exceeded: 200000 > 128000",
            "prompt is too long",
            "上下文溢出",
        ] {
            assert_eq!(classify(error), Fault::SessionLost, "{error}");
        }
    }

    #[test]
    fn retries_stop_at_the_cap() {
        assert_eq!(action_for(Fault::Infrastructure, 0), Action::RetrySame);
        assert_eq!(action_for(Fault::Infrastructure, 1), Action::RetrySame);
        assert_eq!(action_for(Fault::Infrastructure, 2), Action::Stop);
        assert_eq!(action_for(Fault::SessionLost, 0), Action::RetryNewSession);
        assert_eq!(action_for(Fault::SessionLost, 2), Action::Stop);
        // The network gets one more try.
        assert_eq!(action_for(Fault::Network, 2), Action::RetrySame);
        assert_eq!(action_for(Fault::Network, 3), Action::Stop);
        assert_eq!(action_for(Fault::AgentError, 0), Action::Stop);
        assert_eq!(action_for(Fault::Unknown, 0), Action::Stop);
    }

    #[test]
    fn runs_record_their_start_end_and_retry() {
        let mut t = task();
        let worker = AgentId::new();
        let message = MessageId::new();
        begin(&mut t, Some(worker), Some(message));
        assert_eq!(t.runs.len(), 1);
        assert_eq!(t.runs[0].index, 1);
        assert_eq!(t.runs[0].outcome, TaskRunOutcome::Running);
        assert!(t.runs[0].ended_at.is_none());
        assert_eq!(retries_so_far(&t), 0);

        assert!(close(
            &mut t,
            TaskRunOutcome::Infrastructure,
            Some("timed out".into()),
            true
        ));
        assert_eq!(t.runs[0].outcome, TaskRunOutcome::Infrastructure);
        assert!(t.runs[0].ended_at.is_some());
        assert!(t.runs[0].retried);
        // A closed run is not closed twice (a duplicate event changes nothing).
        assert!(!close(&mut t, TaskRunOutcome::NoReport, None, false));

        begin(&mut t, Some(worker), None);
        assert_eq!(t.runs.len(), 2);
        assert_eq!(t.runs[1].index, 2);
        // The retry before the last one counts.
        assert_eq!(retries_so_far(&t), 1);
    }

    #[test]
    fn a_new_run_closes_one_left_open() {
        let mut t = task();
        begin(&mut t, Some(AgentId::new()), None);
        begin(&mut t, Some(AgentId::new()), None);
        assert_eq!(t.runs.len(), 2);
        assert_eq!(t.runs[0].outcome, TaskRunOutcome::NoReport);
        assert_eq!(t.runs[0].note.as_deref(), Some("a new run started"));
        assert_eq!(t.runs[1].outcome, TaskRunOutcome::Running);
    }

    #[test]
    fn only_the_open_runs_turn_counts() {
        let mut t = task();
        let worker = AgentId::new();
        let turn = MessageId::new();
        begin(&mut t, Some(worker), Some(turn));
        assert!(open_run(&t, worker, Some(&turn)));
        // Another turn, or another agent, is not this run's.
        assert!(!open_run(&t, worker, Some(&MessageId::new())));
        assert!(!open_run(&t, AgentId::new(), Some(&turn)));
        // An event with no turn id (a process that died) is taken as this run's.
        assert!(open_run(&t, worker, None));
    }

    #[test]
    fn a_task_from_before_runs_has_none_and_reads_them_as_none() {
        let old = serde_json::json!({
            "id": TaskId::new(), "run_id": crossagents_core::RunId::new(),
            "key": "a", "title": "t", "spec": { "goal": "g" }, "status": "running",
            "attempts": 1, "created_at": Utc::now(), "updated_at": Utc::now(),
        });
        let task: Task = serde_json::from_value(old).unwrap();
        assert!(task.runs.is_empty());
        assert_eq!(retries_so_far(&task), 0);
        assert!(!close(
            &mut task.clone(),
            TaskRunOutcome::Reported,
            None,
            false
        ));
    }

    // -----------------------------------------------------------------------------------
    // The whole way through: a dispatch, a turn that fails, and what the hub does about it.

    use std::path::Path;
    use std::sync::Arc;

    use async_trait::async_trait;
    use crossagents_core::{
        Agent, AgentStatus, CheckResult, CoreError, Envelope, EventBus, LandMode, MessageSource,
        Role, RunConfig, RunPolicy, Store,
    };
    use crossagents_store_sqlite::SqliteStore;

    use crate::engine::{Orchestrator, OrchestratorConfig};
    use crate::host::AgentHost;
    use crate::verify::Verifier;
    use crate::workspace::{Synced, WorkspaceProvider};

    /// Records what was sent where; clones are real agents in the store.
    struct FakeHost {
        store: Arc<dyn Store>,
        sent: tokio::sync::Mutex<Vec<(AgentId, String)>>,
        notices: tokio::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl AgentHost for FakeHost {
        async fn send(&self, _: AgentId, _: MessageSource, _: String) -> Result<(), CoreError> {
            Ok(())
        }
        async fn send_envelope(
            &self,
            to: AgentId,
            envelope: Envelope,
            _: bool,
        ) -> Result<(), CoreError> {
            self.sent.lock().await.push((to, envelope.text));
            Ok(())
        }
        async fn interrupt(&self, _: AgentId) -> Result<(), CoreError> {
            Ok(())
        }
        async fn withdraw(&self, _: AgentId, _: MessageId) -> Result<(), CoreError> {
            Ok(())
        }
        async fn notify(&self, _: AgentId, _: &str, line: &str) -> Result<(), CoreError> {
            self.notices.lock().await.push(line.to_owned());
            Ok(())
        }
        async fn clone_agent(
            &self,
            _: AgentId,
            name: &str,
            role: Role,
            cwd: &Path,
        ) -> Result<Agent, CoreError> {
            let agent = Agent {
                id: AgentId::new(),
                name: name.into(),
                name_source: Default::default(),
                plugin: "mock".into(),
                driver: None,
                model: None,
                role,
                home: cwd.to_path_buf(),
                cwd: cwd.to_path_buf(),
                config: serde_json::json!({}),
                status: AgentStatus::Idle,
                created_at: Utc::now(),
                profile: Default::default(),
                topic: None,
                archived_at: None,
            };
            self.store.insert_agent(&agent).await?;
            Ok(agent)
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
        async fn usage_report(
            &self,
            _: Option<crossagents_core::RunId>,
        ) -> Result<serde_json::Value, CoreError> {
            Ok(serde_json::json!({}))
        }
        async fn take_inbox(&self, _: AgentId) -> Result<Vec<serde_json::Value>, CoreError> {
            Ok(Vec::new())
        }
        async fn archive_agent(&self, _: AgentId) -> Result<(), CoreError> {
            Ok(())
        }
        async fn stop_agent(&self, _: AgentId) -> Result<(), CoreError> {
            Ok(())
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
        async fn check(&self, _: &Path, _: &[String]) -> Vec<CheckResult> {
            Vec::new()
        }
    }

    struct Live {
        store: Arc<dyn Store>,
        orchestrator: Arc<Orchestrator>,
        host: Arc<FakeHost>,
        run: Run,
        lead: Agent,
        worker: Agent,
        _home: tempfile::TempDir,
    }

    async fn live() -> Live {
        let home = tempfile::tempdir().unwrap();
        let repo = home.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().await.unwrap());
        let host = Arc::new(FakeHost {
            store: store.clone(),
            sent: tokio::sync::Mutex::new(Vec::new()),
            notices: tokio::sync::Mutex::new(Vec::new()),
        });
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
        let worker = agent("Rex", Role::Worker, &repo);
        for a in [&lead, &worker] {
            store.insert_agent(a).await.unwrap();
        }
        let run = Run {
            origin_chat_id: None,
            work_id: None,
            continuation: None,
            id: crossagents_core::RunId::new(),
            title: "a run".into(),
            request: "build it".into(),
            planner: lead.id,
            status: crossagents_core::RunStatus::Running,
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
        Live {
            store,
            orchestrator,
            host,
            run,
            lead,
            worker,
            _home: home,
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
            config: serde_json::json!({}),
            status: AgentStatus::Idle,
            created_at: Utc::now(),
            profile: Default::default(),
            topic: None,
            archived_at: None,
        }
    }

    impl Live {
        async fn dispatched(&self, key: &str) -> Task {
            let task = task_in(&self.run, key);
            self.store.insert_task(&task).await.unwrap();
            let mut task = self.store.get_task(task.id).await.unwrap();
            self.orchestrator
                .dispatch(&self.run, &mut task)
                .await
                .unwrap();
            self.orchestrator.drain_relays().await;
            self.store.get_task(task.id).await.unwrap()
        }

        /// The turn of the run that is open now ends with `error`.
        async fn end_turn(&self, task: &mut Task, error: Option<&str>) -> Task {
            let turn = task.runs.last().and_then(|run| run.message);
            let agent = task.assignee.unwrap();
            self.orchestrator
                .turn_ended(&self.run, task, agent, turn, error.map(str::to_owned))
                .await;
            self.orchestrator.drain_relays().await;
            self.store.get_task(task.id).await.unwrap()
        }
    }

    fn task_in(run: &Run, key: &str) -> Task {
        let now = Utc::now();
        Task {
            id: TaskId::new(),
            run_id: run.id,
            key: key.into(),
            title: format!("task {key}"),
            spec: TaskSpec {
                goal: format!("do {key}"),
                ..TaskSpec::default()
            },
            depends_on: Vec::new(),
            issuer: Some(run.planner),
            assignee: None,
            reviewer: None,
            worker_template: Some(run.config.worker),
            reviewer_template: None,
            worker_fresh: true,
            worker_restore: false,
            reviewer_fresh: true,
            status: TaskStatus::Pending,
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
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn an_offline_fault_is_retried_in_the_same_session() {
        let f = live().await;
        let task = f.dispatched("a").await;
        assert_eq!(task.runs.len(), 1);
        assert_eq!(task.runs[0].outcome, TaskRunOutcome::Running);
        let first_turn = task.runs[0].message.expect("the opening was recorded");
        assert_eq!(f.host.sent.lock().await.len(), 1);

        let mut task = task;
        let same = task.assignee;
        let task = f
            .end_turn(&mut task, Some("the runtime host is offline"))
            .await;
        assert_eq!(task.status, TaskStatus::Running, "a retry, not a stall");
        assert_eq!(task.assignee, same, "same session");
        assert_eq!(task.runs.len(), 2);
        assert_eq!(task.runs[0].outcome, TaskRunOutcome::Infrastructure);
        assert!(task.runs[0].retried);
        assert_eq!(
            task.runs[0].note.as_deref(),
            Some("the runtime host is offline")
        );
        assert_eq!(task.runs[1].outcome, TaskRunOutcome::Running);
        assert_ne!(task.runs[1].message, Some(first_turn));
        let sent = f.host.sent.lock().await;
        assert_eq!(sent.len(), 2);
        assert!(sent[1].1.contains("retrying"), "{}", sent[1].1);
        assert_eq!(Some(sent[1].0), same);
    }

    #[tokio::test]
    async fn agent_errors_stop_the_task_and_say_why() {
        let f = live().await;
        let mut task = f.dispatched("a").await;
        let task = f
            .end_turn(&mut task, Some("agent_error: invalid api key"))
            .await;
        assert_eq!(task.status, TaskStatus::Stalled, "not retried");
        assert_eq!(task.runs.len(), 1);
        assert_eq!(task.runs[0].outcome, TaskRunOutcome::AgentError);
        assert!(!task.runs[0].retried);
        let notices = f.host.notices.lock().await;
        assert!(
            notices
                .iter()
                .any(|n| n.contains("not retried automatically")),
            "{notices:?}"
        );
    }

    #[tokio::test]
    async fn a_stalled_turn_carries_the_records_its_error_names() {
        use crossagents_core::{
            IssueRef, Lesson, LessonOutcome, LessonStatus, Project, ProjectPath, TriageStep,
        };
        let f = live().await;
        let project = Project {
            id: "p".into(),
            name: "repo".into(),
            remotes: Vec::new(),
            paths: vec![ProjectPath {
                node: crate::projects::LOCAL_NODE.into(),
                path: f.run.repo.clone(),
            }],
            unconfirmed_remote: None,
            created_at: chrono::Utc::now(),
        };
        f.store.put_project(&project).await.unwrap();
        let lesson = Lesson {
            id: "l1".into(),
            project_id: "p".into(),
            title: "the api key is read from the wrong profile".into(),
            symptom: "agent_error: invalid api key (code 40101)".into(),
            keywords: vec!["40101".into()],
            conditions: String::new(),
            files: Vec::new(),
            resolution: "log in again".into(),
            outcome: LessonOutcome::Resolved,
            status: LessonStatus::Active,
            superseded_by: None,
            evidence: Vec::new(),
            sources: Vec::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let issue = IssueRef {
            run_id: f.run.id,
            key: "issue/a/1".into(),
        };
        f.store
            .triage(&[issue], TriageStep::Create(lesson), f.lead.id)
            .await
            .unwrap();
        let mut task = f.dispatched("a").await;
        let task = f
            .end_turn(&mut task, Some("agent_error: invalid api key (code 40101)"))
            .await;
        // Classified and handled as before; the record only rides along.
        assert_eq!(task.status, TaskStatus::Stalled);
        assert_eq!(task.runs[0].outcome, TaskRunOutcome::AgentError);
        assert_eq!(task.report.related_lessons.len(), 1);
        assert_eq!(task.report.related_lessons[0].id, "l1");
        assert_eq!(task.report.related_lessons[0].matched[0], "40101");
        let notices = f.host.notices.lock().await;
        assert!(
            notices.iter().any(|n| n.contains("possibly related") && n.contains("l1")),
            "{notices:?}"
        );
    }

    #[tokio::test]
    async fn a_polluted_session_is_replaced_in_the_same_directory() {
        let f = live().await;
        let mut task = f.dispatched("a").await;
        let worktree = task.worktree.clone();
        let task = f
            .end_turn(&mut task, Some("context length exceeded: 400000 > 200000"))
            .await;
        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(task.runs.len(), 2);
        assert_eq!(task.runs[0].outcome, TaskRunOutcome::SessionLost);
        assert!(task.runs[0].retried);
        assert_ne!(task.assignee, Some(f.worker.id), "a new session");
        assert_eq!(task.worktree, worktree, "in the directory it had");
    }

    #[tokio::test]
    async fn a_duplicate_end_of_turn_changes_nothing() {
        let f = live().await;
        let mut task = f.dispatched("a").await;
        let turn = task.runs[0].message;
        let agent = task.assignee.unwrap();
        f.orchestrator
            .turn_ended(
                &f.run,
                &mut task,
                agent,
                turn,
                Some("request timed out".into()),
            )
            .await;
        f.orchestrator.drain_relays().await;
        // The same event again: no second retry.
        let mut stored = f.store.get_task(task.id).await.unwrap();
        f.orchestrator
            .turn_ended(
                &f.run,
                &mut stored,
                agent,
                turn,
                Some("request timed out".into()),
            )
            .await;
        f.orchestrator.drain_relays().await;
        assert_eq!(f.store.get_task(task.id).await.unwrap().runs.len(), 2);
    }

    #[tokio::test]
    async fn retries_stop_and_the_task_stalls_when_the_allowance_is_used_up() {
        let f = live().await;
        let mut task = f.dispatched("a").await;
        // Two retries (the cap), then a third failure that stops it.
        for _ in 0..MAX_INFRA_RETRIES {
            task = f.end_turn(&mut task, Some("request timed out")).await;
            assert_eq!(task.status, TaskStatus::Running, "{:?}", task.runs);
        }
        let task = f.end_turn(&mut task, Some("request timed out")).await;
        assert_eq!(task.status, TaskStatus::Stalled);
        // The first run and one per retry; the failure that used the allowance up is the last.
        assert_eq!(task.runs.len(), 1 + MAX_INFRA_RETRIES as usize);
        assert!(task.runs[..task.runs.len() - 1].iter().all(|r| r.retried));
        assert!(!task.runs.last().unwrap().retried);
        let notices = f.host.notices.lock().await;
        assert!(
            notices.iter().any(|n| n.contains("allowance is used up")),
            "{notices:?}"
        );
    }

    #[tokio::test]
    async fn a_turn_that_ends_without_a_report_does_not_end_the_task() {
        let f = live().await;
        let mut task = f.dispatched("a").await;
        let task = f.end_turn(&mut task, None).await;
        assert_eq!(task.status, TaskStatus::Stalled);
        assert_eq!(task.runs.len(), 1);
        assert_eq!(task.runs[0].outcome, TaskRunOutcome::NoReport);
        let _ = f.lead.id;
    }
}
