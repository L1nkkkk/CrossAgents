//! Session-scoped background jobs owned by the hub.
//!
//! Unlike commands launched inside an agent CLI's own tool harness, a hub-managed job
//! survives turn boundaries, image turns, model switches, and CLI restarts. While active,
//! the GUI displays the owning session in a continuous running state with live tail output
//! and a stop button; when the command exits, the hub delivers a `NoticeClass::Decide`
//! wakeup to the owning session so the agent resumes automatically and reports the result.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use axum::extract::{Extension, Path, Query, State};
use axum::routing::{delete, get};
use axum::{Json, Router};
use chrono::Utc;
use crossagents_api_types::{ListJobsResponse, StartJobRequest, ToolInfo};
use crossagents_core::{
    Agent, AgentId, AgentKey, CoreError, EventBus, EventKind, NoticeClass, SessionJob,
    SessionJobStatus,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::api::{ApiResult, AppError, AppState};
use crate::runtime::Runtime;

const MAX_TAIL_LINES: usize = 30;
const MAX_TAIL_BYTES: usize = 4096;
const MAX_STORED_FINISHED: usize = 100;
const WAKEUP_TAIL_LINES: usize = 12;

#[derive(Default)]
struct RollingTail {
    lines: VecDeque<String>,
}

impl RollingTail {
    fn push_line(&mut self, line: String) {
        self.lines.push_back(line);
        while self.lines.len() > MAX_TAIL_LINES {
            self.lines.pop_front();
        }
        while self.byte_len() > MAX_TAIL_BYTES && self.lines.len() > 1 {
            self.lines.pop_front();
        }
    }

    fn byte_len(&self) -> usize {
        self.lines.iter().map(|l| l.len() + 1).sum()
    }

    fn text(&self) -> String {
        let joined = self
            .lines
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        if joined.len() <= MAX_TAIL_BYTES {
            return joined;
        }
        let mut start = joined.len() - MAX_TAIL_BYTES;
        while start < joined.len() && !joined.is_char_boundary(start) {
            start += 1;
        }
        joined[start..].to_owned()
    }

    fn last_lines(&self, n: usize) -> String {
        let skip = self.lines.len().saturating_sub(n);
        self.lines
            .iter()
            .skip(skip)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

struct JobEntry {
    job: SessionJob,
    cancel: CancellationToken,
    done: Arc<tokio::sync::Notify>,
    tail: Arc<Mutex<RollingTail>>,
}

impl JobEntry {
    fn snapshot(&self) -> SessionJob {
        let mut job = self.job.clone();
        if job.status.is_active() {
            job.tail = self.tail.lock().expect("job tail").text();
        }
        job
    }
}

pub struct SessionJobs {
    bus: Arc<EventBus>,
    runtime: Arc<Runtime>,
    logs_dir: PathBuf,
    entries: RwLock<HashMap<String, JobEntry>>,
}

impl SessionJobs {
    pub fn new(bus: Arc<EventBus>, runtime: Arc<Runtime>, logs_dir: PathBuf) -> Self {
        Self {
            bus,
            runtime,
            logs_dir,
            entries: RwLock::new(HashMap::new()),
        }
    }

    pub async fn start(
        self: &Arc<Self>,
        agent: &Agent,
        command: String,
        label: Option<String>,
        cwd: Option<PathBuf>,
    ) -> Result<SessionJob, CoreError> {
        let command = command.trim().to_owned();
        if command.is_empty() {
            return Err(CoreError::InvalidName("job command cannot be empty".into()));
        }
        let cwd = match cwd {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => agent.cwd.clone(),
        };
        if !cwd.is_dir() {
            return Err(CoreError::InvalidName(format!(
                "job working directory does not exist: {}",
                cwd.display()
            )));
        }

        let label = label
            .map(|l| l.trim().to_owned())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| default_label(&command));

        std::fs::create_dir_all(&self.logs_dir).map_err(|e| {
            CoreError::Agent(format!(
                "cannot create jobs log directory {}: {e}",
                self.logs_dir.display()
            ))
        })?;

        let raw_uuid = uuid::Uuid::now_v7().simple().to_string();
        let id = format!("job-{}", &raw_uuid[..10]);
        let log_path = self.logs_dir.join(format!("{id}.log"));
        let log_file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&log_path)
            .await
            .map_err(|e| {
                CoreError::Agent(format!(
                    "cannot open job log file {}: {e}",
                    log_path.display()
                ))
            })?;

        let mut child_cmd = shell_command(&command);
        child_cmd
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = child_cmd.spawn().map_err(|e| {
            CoreError::Agent(format!("cannot spawn background job `{label}`: {e}"))
        })?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let started_at = Utc::now();
        let job = SessionJob {
            id: id.clone(),
            agent_id: agent.id,
            label: label.clone(),
            command: command.clone(),
            cwd: cwd.display().to_string(),
            status: SessionJobStatus::Running,
            started_at,
            finished_at: None,
            exit_code: None,
            log_path: log_path.display().to_string(),
            tail: String::new(),
        };

        let cancel = CancellationToken::new();
        let done = Arc::new(tokio::sync::Notify::new());
        let tail = Arc::new(Mutex::new(RollingTail::default()));
        {
            let mut entries = self.entries.write().await;
            entries.insert(
                id.clone(),
                JobEntry {
                    job: job.clone(),
                    cancel: cancel.clone(),
                    done: done.clone(),
                    tail: tail.clone(),
                },
            );
        }
        let _ = self
            .bus
            .publish(EventKind::AgentJobUpdated {
                id: agent.id,
                job: job.clone(),
            })
            .await;

        let manager = Arc::clone(self);
        let agent_id = agent.id;
        let job_id = id.clone();
        tokio::spawn(async move {
            let log_writer = Arc::new(tokio::sync::Mutex::new(log_file));
            let stdout_task = stdout.map(|out| {
                let tail = tail.clone();
                let log = log_writer.clone();
                tokio::spawn(async move {
                    pump_stream(out, tail, log).await;
                })
            });
            let stderr_task = stderr.map(|err| {
                let tail = tail.clone();
                let log = log_writer.clone();
                tokio::spawn(async move {
                    pump_stream(err, tail, log).await;
                })
            });

            let (status, exit_code, was_cancelled) = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    kill_child_tree(&mut child).await;
                    let code = child.wait().await.ok().and_then(|s| s.code());
                    (SessionJobStatus::Cancelled, code, true)
                }
                res = child.wait() => match res {
                    Ok(exit) => {
                        let ok = exit.success();
                        (
                            if ok { SessionJobStatus::Succeeded } else { SessionJobStatus::Failed },
                            exit.code(),
                            false,
                        )
                    }
                    Err(err) => {
                        tail.lock().expect("job tail").push_line(format!("[hub] wait error: {err}"));
                        (SessionJobStatus::Failed, None, false)
                    }
                }
            };

            if let Some(t) = stdout_task {
                let _ = t.await;
            }
            if let Some(t) = stderr_task {
                let _ = t.await;
            }
            let _ = log_writer.lock().await.flush().await;

            let finished_at = Utc::now();
            let (full_tail, notice_tail) = {
                let guard = tail.lock().expect("job tail");
                (guard.text(), guard.last_lines(WAKEUP_TAIL_LINES))
            };

            let updated = {
                let mut entries = manager.entries.write().await;
                let updated = entries.get_mut(&job_id).map(|entry| {
                    entry.job.status = status;
                    entry.job.finished_at = Some(finished_at);
                    entry.job.exit_code = exit_code;
                    entry.job.tail = full_tail;
                    entry.job.clone()
                });
                prune_finished(&mut entries);
                updated
            };
            done.notify_waiters();

            if let Some(job) = updated {
                let _ = manager
                    .bus
                    .publish(EventKind::AgentJobUpdated {
                        id: agent_id,
                        job: job.clone(),
                    })
                    .await;

                let elapsed = (finished_at - started_at).num_seconds().max(0);
                let wakeup_text = format_wakeup_notice(&job, elapsed, &notice_tail);
                // Natural completion wakes the owning session immediately (`Decide`) so the agent
                // always knows the background command finished and reports or continues.
                // User-initiated cancellation records a `Silent` notice so the agent sees it next
                // turn without starting an unwanted extra turn.
                let class = if was_cancelled {
                    NoticeClass::Silent
                } else {
                    NoticeClass::Decide
                };
                if let Err(err) = manager
                    .runtime
                    .notify(
                        &AgentKey::Id(agent_id),
                        &format!("job:{}", job.id),
                        &wakeup_text,
                        class,
                    )
                    .await
                {
                    tracing::warn!(
                        job_id = %job.id,
                        agent_id = %agent_id,
                        error = %err,
                        "cannot deliver background job completion notice"
                    );
                }
            }
        });

        Ok(job)
    }

    pub async fn list(&self, agent_id: Option<AgentId>) -> Vec<SessionJob> {
        let entries = self.entries.read().await;
        let mut jobs: Vec<SessionJob> = entries
            .values()
            .filter(|e| agent_id.is_none_or(|id| e.job.agent_id == id))
            .map(JobEntry::snapshot)
            .collect();
        jobs.sort_by(|a, b| {
            b.status
                .is_active()
                .cmp(&a.status.is_active())
                .then_with(|| b.started_at.cmp(&a.started_at))
        });
        jobs
    }

    pub async fn get(&self, id: &str) -> Option<SessionJob> {
        self.entries.read().await.get(id).map(JobEntry::snapshot)
    }

    pub async fn cancel(
        &self,
        id: &str,
        expected_agent: Option<AgentId>,
    ) -> Result<SessionJob, CoreError> {
        let (cancel, done, snapshot) = {
            let entries = self.entries.read().await;
            let Some(entry) = entries.get(id) else {
                return Err(CoreError::NotFound {
                    kind: "job",
                    id: id.to_owned(),
                });
            };
            if let Some(expected) = expected_agent
                && entry.job.agent_id != expected
            {
                return Err(CoreError::NotFound {
                    kind: "job",
                    id: id.to_owned(),
                });
            }
            (entry.cancel.clone(), entry.done.clone(), entry.snapshot())
        };
        if snapshot.status.is_active() {
            let notified = done.notified();
            cancel.cancel();
            if self.get(id).await.is_none_or(|j| j.status.is_active()) {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(5), notified).await;
            }
        }
        Ok(self.get(id).await.unwrap_or(snapshot))
    }

    pub async fn shutdown_all(&self) {
        let tokens: Vec<CancellationToken> = self
            .entries
            .read()
            .await
            .values()
            .filter(|e| e.job.status.is_active())
            .map(|e| e.cancel.clone())
            .collect();
        for token in tokens {
            token.cancel();
        }
    }
}

fn default_label(command: &str) -> String {
    let first = command.lines().next().unwrap_or(command).trim();
    if first.chars().count() <= 64 {
        first.to_owned()
    } else {
        let truncated: String = first.chars().take(63).collect();
        format!("{truncated}…")
    }
}

fn format_duration_secs(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn format_wakeup_notice(job: &SessionJob, elapsed_secs: i64, tail: &str) -> String {
    let verdict = match job.status {
        SessionJobStatus::Succeeded => "succeeded",
        SessionJobStatus::Failed => "failed",
        SessionJobStatus::Cancelled => "was cancelled",
        SessionJobStatus::Running => "updated",
    };
    let code = job
        .exit_code
        .map(|c| format!("exit {c}"))
        .unwrap_or_else(|| "no exit code".into());
    let dur = format_duration_secs(elapsed_secs);
    let mut line = format!(
        "Background job {} (`{}`) {verdict} ({code}, {dur}). Full log: {}",
        job.id, job.label, job.log_path
    );
    let trimmed = tail.trim();
    if !trimmed.is_empty() {
        line.push_str("\nOutput tail:\n");
        line.push_str(trimmed);
    }
    line
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("powershell");
        let wrapped = format!(
            "$OutputEncoding = [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); {command}"
        );
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", &wrapped]);
        crossagents_drivers::process::hide_console(&mut cmd);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", command]);
        cmd
    }
}

async fn kill_child_tree(child: &mut tokio::process::Child) {
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let mut kill = Command::new("taskkill");
        kill.args(["/F", "/T", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        crossagents_drivers::process::hide_console(&mut kill);
        let _ = kill.status().await;
    }
    let _ = child.start_kill();
}

async fn pump_stream<R: tokio::io::AsyncRead + Unpin>(
    reader: R,
    tail: Arc<Mutex<RollingTail>>,
    log: Arc<tokio::sync::Mutex<tokio::fs::File>>,
) {
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        {
            let mut file = log.lock().await;
            let _ = file.write_all(line.as_bytes()).await;
            let _ = file.write_all(b"\n").await;
        }
        tail.lock().expect("job tail").push_line(line);
    }
}

fn prune_finished(entries: &mut HashMap<String, JobEntry>) {
    let mut finished: Vec<(String, chrono::DateTime<Utc>)> = entries
        .iter()
        .filter(|(_, e)| !e.job.status.is_active())
        .map(|(k, e)| (k.clone(), e.job.finished_at.unwrap_or(e.job.started_at)))
        .collect();
    if finished.len() <= MAX_STORED_FINISHED {
        return;
    }
    finished.sort_by_key(|(_, ts)| *ts);
    let drop_count = finished.len() - MAX_STORED_FINISHED;
    for (id, _) in finished.into_iter().take(drop_count) {
        entries.remove(&id);
    }
}

fn parse_agent_key(key: &str) -> AgentKey {
    key.parse::<AgentId>()
        .map(AgentKey::Id)
        .unwrap_or_else(|_| AgentKey::Name(key.to_owned()))
}

// ---------------------------------------------------------------------------------------
// HTTP routes (human/GUI + agent `/v1/me`)

#[derive(Debug, Deserialize)]
struct JobsQuery {
    agent: Option<String>,
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/jobs", get(list_jobs))
        .route("/v1/jobs/{id}", get(get_job).delete(cancel_job))
        .route(
            "/v1/agents/{key}/jobs",
            get(list_agent_jobs).post(start_agent_job),
        )
}

pub fn me_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/me/jobs", get(me_list_jobs).post(me_start_job))
        .route("/v1/me/jobs/{id}", delete(me_cancel_job))
}

async fn list_jobs(
    State(state): State<Arc<AppState>>,
    Query(query): Query<JobsQuery>,
) -> ApiResult<Json<ListJobsResponse>> {
    let agent_id = match query.agent {
        Some(key) if !key.trim().is_empty() => {
            Some(state.store().get_agent(&parse_agent_key(key.trim())).await?.id)
        }
        _ => None,
    };
    Ok(Json(ListJobsResponse {
        jobs: state.jobs.list(agent_id).await,
    }))
}

async fn get_job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<SessionJob>> {
    let Some(job) = state.jobs.get(&id).await else {
        return Err(AppError::from(CoreError::NotFound { kind: "job", id }));
    };
    Ok(Json(job))
}

async fn cancel_job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<SessionJob>> {
    Ok(Json(state.jobs.cancel(&id, None).await?))
}

async fn list_agent_jobs(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> ApiResult<Json<ListJobsResponse>> {
    let agent = state.store().get_agent(&parse_agent_key(&key)).await?;
    Ok(Json(ListJobsResponse {
        jobs: state.jobs.list(Some(agent.id)).await,
    }))
}

async fn start_agent_job(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(req): Json<StartJobRequest>,
) -> ApiResult<Json<SessionJob>> {
    let agent = state.store().get_agent(&parse_agent_key(&key)).await?;
    let cwd = req.cwd.map(PathBuf::from);
    Ok(Json(
        state.jobs.start(&agent, req.command, req.label, cwd).await?,
    ))
}

async fn me_list_jobs(
    State(state): State<Arc<AppState>>,
    Extension(agent): Extension<Agent>,
) -> ApiResult<Json<ListJobsResponse>> {
    Ok(Json(ListJobsResponse {
        jobs: state.jobs.list(Some(agent.id)).await,
    }))
}

async fn me_start_job(
    State(state): State<Arc<AppState>>,
    Extension(agent): Extension<Agent>,
    Json(req): Json<StartJobRequest>,
) -> ApiResult<Json<SessionJob>> {
    let cwd = req.cwd.map(PathBuf::from);
    Ok(Json(
        state.jobs.start(&agent, req.command, req.label, cwd).await?,
    ))
}

async fn me_cancel_job(
    State(state): State<Arc<AppState>>,
    Extension(agent): Extension<Agent>,
    Path(id): Path<String>,
) -> ApiResult<Json<SessionJob>> {
    Ok(Json(state.jobs.cancel(&id, Some(agent.id)).await?))
}

// ---------------------------------------------------------------------------------------
// MCP tools (`/v1/me/tools`)

pub fn mcp_tools() -> Vec<ToolInfo> {
    vec![
        ToolInfo {
            name: "job_start".into(),
            description: "Start a long-running shell command managed by the CrossAgents hub in the background for this session. The hub keeps the job alive across turns/CLI restarts, displays continuous running status and live output to the person, and automatically wakes this session with the exit code, log path, and tail output when the job finishes.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to execute in the background." },
                    "label": { "type": "string", "description": "Short human-readable label shown in the UI (optional)." },
                    "cwd": { "type": "string", "description": "Working directory (defaults to this session's cwd)." }
                },
                "required": ["command"]
            }),
        },
        ToolInfo {
            name: "job_list".into(),
            description: "List hub-managed background jobs for this session, including live tail output and status.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolInfo {
            name: "job_cancel".into(),
            description: "Cancel a running hub-managed background job belonging to this session.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "The job id returned by job_start (e.g. job-019a...)." }
                },
                "required": ["id"]
            }),
        },
    ]
}

pub async fn call_mcp_tool(
    state: &AppState,
    agent: &Agent,
    name: &str,
    input: Value,
) -> Option<Result<Value, CoreError>> {
    match name {
        "job_start" => {
            let parsed = serde_json::from_value::<StartJobRequest>(input)
                .map_err(|e| CoreError::InvalidName(format!("invalid job_start arguments: {e}")));
            let res = match parsed {
                Ok(req) => state
                    .jobs
                    .start(agent, req.command, req.label, req.cwd.map(PathBuf::from))
                    .await
                    .and_then(|job| {
                        serde_json::to_value(job).map_err(|e| CoreError::Agent(e.to_string()))
                    }),
                Err(err) => Err(err),
            };
            Some(res)
        }
        "job_list" => {
            let jobs = state.jobs.list(Some(agent.id)).await;
            Some(Ok(json!({ "jobs": jobs })))
        }
        "job_cancel" => {
            let id = input
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let res = match id {
                Some(id) => state
                    .jobs
                    .cancel(id, Some(agent.id))
                    .await
                    .and_then(|job| {
                        serde_json::to_value(job).map_err(|e| CoreError::Agent(e.to_string()))
                    }),
                None => Err(CoreError::InvalidName(
                    "missing required string field `id`".into(),
                )),
            };
            Some(res)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossagents_core::{AgentStatus, Role, Store};
    use crossagents_plugins::PluginRegistry;
    use crossagents_store_sqlite::SqliteStore;
    use std::time::Duration;

    async fn setup() -> (Arc<SessionJobs>, Arc<dyn Store>, Agent, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().await.unwrap());
        let bus = Arc::new(EventBus::new(store.clone()));
        let runtime = Arc::new(Runtime::new(bus.clone(), PluginRegistry::default()));
        let agent = Agent {
            node: None,
            id: AgentId::new(),
            name: "Worker".into(),
            name_source: Default::default(),
            plugin: "mock".into(),
            driver: None,
            model: None,
            role: Role::Worker,
            home: dir.path().to_path_buf(),
            cwd: dir.path().to_path_buf(),
            config: json!({}),
            status: AgentStatus::Idle,
            created_at: Utc::now(),
            profile: Default::default(),
            topic: None,
            archived_at: None,
        };
        store.insert_agent(&agent).await.unwrap();
        let jobs = Arc::new(SessionJobs::new(
            bus,
            runtime,
            dir.path().join("jobs"),
        ));
        (jobs, store, agent, dir)
    }

    #[tokio::test]
    async fn job_completes_writes_log_and_records_decide_wakeup_notice() {
        let (jobs, store, agent, _dir) = setup().await;
        let cmd = if cfg!(windows) {
            "Write-Output 'hello-from-job'; exit 0"
        } else {
            "echo hello-from-job"
        };
        let started = jobs
            .start(&agent, cmd.into(), Some("echo test".into()), None)
            .await
            .unwrap();
        assert_eq!(started.status, SessionJobStatus::Running);
        assert_eq!(started.label, "echo test");

        // Wait for completion.
        let mut final_job = started.clone();
        for _ in 0..80 {
            if let Some(j) = jobs.get(&started.id).await
                && !j.status.is_active()
            {
                final_job = j;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(final_job.status, SessionJobStatus::Succeeded);
        assert_eq!(final_job.exit_code, Some(0));
        assert!(final_job.tail.contains("hello-from-job"), "tail: {}", final_job.tail);

        let log_contents = std::fs::read_to_string(&final_job.log_path).unwrap();
        assert!(log_contents.contains("hello-from-job"));

        let notices = store.list_pending_notices(Some(agent.id)).await.unwrap();
        assert!(
            notices.iter().any(|n| n.topic == format!("job:{}", started.id)
                && n.class == NoticeClass::Decide
                && n.line.contains("succeeded")),
            "expected Decide wakeup notice, got: {notices:?}"
        );
    }

    #[tokio::test]
    async fn cancelling_running_job_terminates_it_and_marks_cancelled() {
        let (jobs, _store, agent, _dir) = setup().await;
        let cmd = if cfg!(windows) {
            "Start-Sleep -Seconds 30"
        } else {
            "sleep 30"
        };
        let started = jobs
            .start(&agent, cmd.into(), Some("sleep test".into()), None)
            .await
            .unwrap();
        let stopped = jobs.cancel(&started.id, Some(agent.id)).await.unwrap();
        assert_eq!(stopped.status, SessionJobStatus::Cancelled);
    }
}
