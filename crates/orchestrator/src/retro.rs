//! A run's retro (docs/tasks/SELF.md C1): what got in the way, in the shape of
//! docs/tasks/DOGFOOD.md §9 — 现象 / 影响 / 方向 — folded from the run's events and its
//! `issue/` notes, and written to `docs/tasks/RETRO-<run>.md`. It writes that document only,
//! never code.
//!
//! The folding is done here rather than from the hub's trace so the collaboration tool
//! (`run_retro`, a lead's) and the command line (`crossagents run retro`) share one view: both
//! feed it the run's events, and the same run gives the same document.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crossagents_core::{
    Agent, AgentId, AgentStatus, CoreError, Event, EventKind, ISSUE_PREFIX, Note, NoteScope, Run,
    RunId, RunStatus, StopReason, Store, Task, TaskStatus,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::Orchestrator;
use crate::tools::args;

/// A context below this many tokens is too small for a drop to mean a compaction.
const COMPACT_FROM: u64 = 20_000;
/// Longest a table cell keeps before `…`.
const CELL_MAX: usize = 200;
/// Longest the run's request keeps in the header.
const REQUEST_MAX: usize = 400;
/// A failing command's output kept, in the cell that mentions it.
const TAIL_MAX: usize = 80;
/// Failing commands listed per lane, worst first.
const COMMANDS_SHOWN: usize = 3;
/// Events read from the store at a time.
const EVENT_PAGE: u32 = 500;

/// The document a retro writes, and what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetroDoc {
    pub markdown: String,
    /// Rows of the 现象 / 影响 / 方向 table.
    pub findings: usize,
    /// `issue/` notes that made it into the table.
    pub issues: usize,
}

/// `docs/tasks/RETRO-<run>.md`, the file a retro writes.
pub fn file_name(run: RunId) -> String {
    format!("RETRO-{}.md", short_id(run))
}

/// The last 8 hex characters of a run id: how the command line shows a run.
pub fn short_id(run: RunId) -> String {
    let hex = run.to_string().replace('-', "");
    hex[hex.len().saturating_sub(8)..].to_owned()
}

/// Writes `doc` under `<checkout>/docs/tasks/`, making the directory when the checkout has no
/// documents yet, and gives back the file's path. The path is canonicalized and must stay
/// inside `checkout`. An existing retro is kept unless `force`: a second look at a run should
/// not silently replace a document the person may have written on.
pub fn write(
    checkout: &Path,
    run: RunId,
    doc: &RetroDoc,
    force: bool,
) -> Result<PathBuf, CoreError> {
    let root = dunce::canonicalize(checkout)
        .map_err(|e| CoreError::Storage(format!("cannot read {}: {e}", checkout.display())))?;
    let dir = root.join("docs").join("tasks");
    std::fs::create_dir_all(&dir)
        .map_err(|e| CoreError::Storage(format!("cannot make {}: {e}", dir.display())))?;
    let dir = dunce::canonicalize(&dir)
        .map_err(|e| CoreError::Storage(format!("cannot read {}: {e}", dir.display())))?;
    if !dir.starts_with(&root) {
        return Err(CoreError::Conflict(format!(
            "{} resolves outside {}: not writing the retro there",
            dir.display(),
            root.display()
        )));
    }
    let path = dir.join(file_name(run));
    if path.exists() {
        let real = dunce::canonicalize(&path)
            .map_err(|e| CoreError::Storage(format!("cannot read {}: {e}", path.display())))?;
        if !real.starts_with(&root) {
            return Err(CoreError::Conflict(format!(
                "{} resolves outside {}: not writing the retro there",
                path.display(),
                root.display()
            )));
        }
        if !force {
            return Err(CoreError::Conflict(format!(
                "{} already exists; read it, or call again with force",
                path.display()
            )));
        }
    }
    std::fs::write(&path, &doc.markdown)
        .map_err(|e| CoreError::Storage(format!("cannot write {}: {e}", path.display())))?;
    Ok(path)
}

/// The retro of a run: its events, folded by task and by lane, plus the problems its agents
/// recorded as `issue/` notes. Every event of the run is read; other runs' are ignored.
pub fn render(run: &Run, tasks: &[Task], events: &[Event], issues: &[Note]) -> RetroDoc {
    let mut ordered: Vec<&Event> = events.iter().collect();
    ordered.sort_by_key(|e| e.seq);
    let mut fold = Fold::new(run.id);
    for event in ordered {
        fold.push(event);
    }

    let mut rows: Vec<Row> = Vec::new();
    for task in tasks {
        task_rows(task, fold.lane(&Lane::Task(task.key.clone())), &mut rows);
    }
    if let Some(stat) = fold.lane(&Lane::Lead) {
        work_rows("主导", stat, &mut rows);
    }
    run_rows(run, &fold, &mut rows);
    for note in issues {
        rows.push(Row::new(
            format!("[{}] {}", note.key, one_line(&note.text, CELL_MAX)),
            "run 里被记成问题：至少有一个 agent 认为它挡了路",
            "读 issue 原文；影响大、改动小的挑进下一轮",
        ));
    }

    let findings = rows.len();
    if rows.is_empty() {
        rows.push(Row::new(
            "trace 和 issue 笔记里没有看出卡点",
            "这次跑得顺，或者说该记的没记",
            "下次跑更长的任务再看；让 agent 把卡点写成 issue 笔记",
        ));
    }

    let mut out = String::new();
    let header = |out: &mut String| {
        let _ = writeln!(
            out,
            "# RETRO-{} — {}",
            short_id(run.id),
            one_line(&run.title, 120)
        );
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "> `crossagents run retro` 从已结束 run 的 trace 与 issue 笔记生成；只写这份文档，不改代码。"
        );
        let _ = writeln!(
            out,
            "> 表格沿用 [DOGFOOD §9](DOGFOOD.md) 的「现象 / 影响 / 方向」。"
        );
        let _ = writeln!(out);
        let _ = writeln!(out, "## 怎么跑的");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "- run：`{}`（{}）",
            run.id,
            run_status_word(run.status)
        );
        if run.in_place || run.branch.trim().is_empty() {
            let _ = writeln!(
                out,
                "- 目录：`{}`（不在版本库里，直接改工作目录）",
                run.repo.display()
            );
        } else {
            let _ = writeln!(out, "- 分支：`{}`，从 `{}`", run.branch, run.base_branch);
        }
        let minutes = (run.updated_at - run.created_at).num_minutes().max(0);
        let _ = writeln!(
            out,
            "- 时间：{} → {}（{minutes} 分钟）",
            run.created_at.format("%Y-%m-%d %H:%M"),
            run.updated_at.format("%Y-%m-%d %H:%M")
        );
        let count = |status: TaskStatus| tasks.iter().filter(|t| t.status == status).count();
        let merged = count(TaskStatus::Merged);
        let failed = count(TaskStatus::Failed);
        let cancelled = count(TaskStatus::Cancelled);
        let other = tasks.len().saturating_sub(merged + failed + cancelled);
        let _ = writeln!(
            out,
            "- 任务：{} 个（{merged} 合并 / {failed} 失败 / {cancelled} 取消 / {other} 其他）",
            tasks.len()
        );
        let _ = writeln!(
            out,
            "- 花费：{} tokens，${:.2}",
            thousands(fold.spent_tokens),
            fold.spent_usd
        );
        let _ = writeln!(out, "- 请求：{}", one_line(&run.request, REQUEST_MAX));
        if let Some(note) = &run.note {
            let _ = writeln!(out, "- 备注：{}", one_line(note, CELL_MAX));
        }
        match &run.landed {
            Some(landed) => {
                let _ = writeln!(out, "- 落地：`{}`（{}）", landed.reference, landed.by);
            }
            None if run.in_place => {
                let _ = writeln!(out, "- 落地：不适用（不在版本库里）");
            }
            None => {
                let _ = writeln!(out, "- 落地：没有（改动只在 `{}` 上）", run.branch);
            }
        }
    };
    header(&mut out);

    let _ = writeln!(out);
    let _ = writeln!(out, "## 结果");
    let _ = writeln!(out);
    if let Some(summary) = &run.summary {
        let _ = writeln!(out, "主导收尾：{}", one_line(summary, REQUEST_MAX));
        let _ = writeln!(out);
    }
    if tasks.is_empty() {
        let _ = writeln!(out, "- 没有拆任务：主导在集成目录里做完。");
    }
    for task in tasks {
        let summary = task.report.summary.as_deref().unwrap_or("没有留 summary");
        let _ = writeln!(
            out,
            "- #{}「{}」— {}",
            task.key,
            task_status_word(task.status),
            one_line(summary, 160)
        );
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "## 卡住或别扭的地方");
    let _ = writeln!(out);
    let _ = writeln!(out, "| # | 现象 | 影响 | 方向 |");
    let _ = writeln!(out, "|---|---|---|---|");
    for (i, row) in rows.iter().enumerate() {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            i + 1,
            cell(&row.phenomenon),
            cell(&row.impact),
            cell(&row.direction)
        );
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "## 这次没覆盖到");
    let _ = writeln!(out);
    for line in coverage(run, tasks, &fold, issues) {
        let _ = writeln!(out, "- {line}");
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "## 来源");
    let _ = writeln!(out);
    let _ = writeln!(out, "- 事件：{} 条（按 run 折叠）", fold.kept);
    let _ = writeln!(
        out,
        "- issue 笔记：{} 条（`{}` 前缀）",
        issues.len(),
        ISSUE_PREFIX
    );
    let _ = writeln!(
        out,
        "- 重新生成：`crossagents run retro {}`",
        short_id(run.id)
    );

    RetroDoc {
        markdown: out,
        findings,
        issues: issues.len(),
    }
}

/// The retro of `run`, read from the store: its tasks, its `issue/` notes and every event
/// (the fold keeps this run's). A run that has not ended has no retro: there is nothing to
/// learn from work still in flight.
pub async fn document(store: &dyn Store, run: &Run) -> Result<RetroDoc, CoreError> {
    if !run.status.is_terminal() {
        return Err(CoreError::Conflict(format!(
            "run {} has not ended ({}); a retro is for a run that ended",
            short_id(run.id),
            run_status_word(run.status)
        )));
    }
    let tasks = store.list_tasks(run.id).await?;
    let issues = store
        .list_notes(NoteScope::Run, Some(run.id), ISSUE_PREFIX)
        .await?;
    let mut events = Vec::new();
    let mut after = 0;
    loop {
        let page = store.events_since(after, EVENT_PAGE).await?;
        let Some(last) = page.last().map(|e| e.seq) else {
            break;
        };
        after = last;
        events.extend(page);
    }
    Ok(render(run, &tasks, &events, &issues))
}

// ---------------------------------------------------------------------------------------
// Folding

/// One line of the table.
struct Row {
    phenomenon: String,
    impact: String,
    direction: String,
}

impl Row {
    fn new(
        phenomenon: impl Into<String>,
        impact: impl Into<String>,
        direction: impl Into<String>,
    ) -> Self {
        Self {
            phenomenon: phenomenon.into(),
            impact: impact.into(),
            direction: direction.into(),
        }
    }
}

/// Who the events belong to: the lead itself, or one task's lane.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Lane {
    Lead,
    Task(String),
}

/// What one lane did, as far as the table cares.
#[derive(Default)]
struct LaneStat {
    commands: HashMap<String, CommandStat>,
    failed_turns: u32,
    turn_errors: u32,
    first_error: Option<String>,
    reroutes: u32,
    reroute_reason: Option<String>,
    handoffs: Vec<String>,
    compacted: u32,
    model_changes: Vec<String>,
    checks_runs: u32,
    checks_failed: u32,
    failed_check: Option<String>,
}

#[derive(Default)]
struct CommandStat {
    failures: u32,
    last_exit: Option<i32>,
    tail: Option<String>,
}

/// An agent's session totals, as `agent.usage` reports them cumulatively.
#[derive(Default)]
struct Session {
    used: u64,
    tokens: u64,
    usd: f64,
}

struct Fold {
    run: RunId,
    lanes: HashMap<AgentId, Lane>,
    stats: HashMap<Lane, LaneStat>,
    /// Commands still waiting for their details: tool call id → (lane, title).
    calls: HashMap<String, (Lane, String)>,
    sessions: HashMap<AgentId, Session>,
    landed: Option<String>,
    /// Events of this run (others are read and skipped).
    kept: usize,
    spent_tokens: u64,
    spent_usd: f64,
}

impl Fold {
    fn new(run: RunId) -> Self {
        Self {
            run,
            lanes: HashMap::new(),
            stats: HashMap::new(),
            calls: HashMap::new(),
            sessions: HashMap::new(),
            landed: None,
            kept: 0,
            spent_tokens: 0,
            spent_usd: 0.0,
        }
    }

    fn stat(&mut self, lane: Lane) -> &mut LaneStat {
        self.stats.entry(lane).or_default()
    }

    fn lane(&self, lane: &Lane) -> Option<&LaneStat> {
        self.stats.get(lane)
    }

    fn push(&mut self, event: &Event) {
        let mine = match &event.kind {
            EventKind::RunCreated { run } => run.id == self.run,
            EventKind::RunLanded { id, .. } => *id == self.run,
            EventKind::TaskStatusChanged { run_id, .. }
            | EventKind::TaskRerouted { run_id, .. }
            | EventKind::TaskHandedOff { run_id, .. }
            | EventKind::TaskChecked { run_id, .. } => *run_id == self.run,
            _ => event
                .kind
                .agent_id()
                .is_some_and(|a| self.lanes.contains_key(&a)),
        };
        if mine {
            self.kept += 1;
        }
        match &event.kind {
            EventKind::RunCreated { run } if run.id == self.run => {
                self.lanes.insert(run.planner, Lane::Lead);
            }
            // The lead goes on to lead another run: what it does next is not ours.
            EventKind::RunCreated { run } => {
                if self.lanes.get(&run.planner) == Some(&Lane::Lead) {
                    self.lanes.remove(&run.planner);
                }
            }
            EventKind::TaskStatusChanged {
                run_id,
                key,
                assignee,
                ..
            } if *run_id == self.run => {
                if let Some(agent) = assignee {
                    self.lanes.insert(*agent, Lane::Task(key.clone()));
                }
            }
            EventKind::TaskRerouted {
                run_id,
                key,
                to,
                reason,
                ..
            } if *run_id == self.run => {
                self.lanes.insert(*to, Lane::Task(key.clone()));
                let stat = self.stat(Lane::Task(key.clone()));
                stat.reroutes += 1;
                stat.reroute_reason.get_or_insert_with(|| reason.clone());
            }
            EventKind::TaskHandedOff { run_id, handoff } if *run_id == self.run => {
                self.lanes
                    .insert(handoff.to, Lane::Task(handoff.task_key.clone()));
                let from = handoff.from_name.clone().unwrap_or_else(|| "?".to_owned());
                let stat = self.stat(Lane::Task(handoff.task_key.clone()));
                stat.handoffs.push(format!("{from} → {}", handoff.to_name));
            }
            EventKind::TaskChecked {
                run_id,
                key,
                passed,
                checks,
                ..
            } if *run_id == self.run => {
                let stat = self.stat(Lane::Task(key.clone()));
                stat.checks_runs += 1;
                if !passed {
                    stat.checks_failed += 1;
                    if let Some(failed) = checks.iter().find(|c| !c.passed) {
                        stat.failed_check.get_or_insert_with(|| {
                            format!("`{}`（exit {}）", failed.command, exit(failed.exit_code))
                        });
                    }
                }
            }
            EventKind::RunLanded { id, landing } if *id == self.run => {
                self.landed = Some(landing.reference.clone());
            }
            _ => self.push_agent(event),
        }
    }

    fn push_agent(&mut self, event: &Event) {
        let Some(agent) = event.kind.agent_id() else {
            return;
        };
        if let EventKind::AgentStatusChanged {
            to: AgentStatus::Starting,
            ..
        } = &event.kind
        {
            self.sessions.remove(&agent);
        }
        let Some(lane) = self.lanes.get(&agent).cloned() else {
            return;
        };
        match &event.kind {
            EventKind::AgentTurnCompleted {
                stop_reason: StopReason::Error | StopReason::Interrupted,
                ..
            } => self.stat(lane).failed_turns += 1,
            EventKind::AgentError { message, .. } | EventKind::AgentExited { message, .. } => {
                let stat = self.stat(lane);
                stat.turn_errors += 1;
                stat.first_error.get_or_insert_with(|| message.clone());
            }
            EventKind::AgentToolCall {
                tool_call_id,
                title,
                kind,
                ..
            } if kind.as_deref() == Some("execute") => {
                self.calls
                    .insert(tool_call_id.clone(), (lane, title.clone()));
            }
            EventKind::AgentToolDetail {
                tool_call_id,
                detail,
                ..
            } => {
                let Some((lane, title)) = self.calls.get(tool_call_id).cloned() else {
                    return;
                };
                let Some(code) = detail.exit_code.filter(|c| *c != 0) else {
                    return;
                };
                let command = detail.command.clone().unwrap_or(title);
                let stat = self.stat(lane);
                let command = stat.commands.entry(command).or_default();
                command.failures += 1;
                command.last_exit = Some(code);
                if let Some(output) = &detail.output {
                    command.tail = Some(tail(output));
                }
            }
            EventKind::AgentUsage {
                used,
                cost_usd,
                tokens,
                ..
            } => {
                let (compacted, added_tokens, added_usd) = {
                    let session = self.sessions.entry(agent).or_default();
                    let before = session.used;
                    session.used = *used;
                    let now_tokens = tokens.as_ref().map_or(session.tokens, |t| t.total);
                    let now_usd = cost_usd.unwrap_or(session.usd);
                    let added_tokens = now_tokens.saturating_sub(session.tokens);
                    let added_usd = (now_usd - session.usd).max(0.0);
                    session.tokens = now_tokens;
                    session.usd = now_usd;
                    // No event says a context was compacted: a sharp drop within one session
                    // is one.
                    let compacted = before >= COMPACT_FROM && *used > 0 && *used * 10 <= before * 6;
                    (compacted, added_tokens, added_usd)
                };
                self.spent_tokens += added_tokens;
                self.spent_usd += added_usd;
                if compacted {
                    self.stat(lane).compacted += 1;
                }
            }
            EventKind::AgentModelChanged {
                model,
                effort,
                from_model,
                from_effort,
                ..
            } => {
                let text = format!(
                    "{} → {}",
                    model_name(from_model, from_effort),
                    model_name(model, effort)
                );
                self.stat(lane).model_changes.push(text);
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------------------
// Rows

fn task_rows(task: &Task, stat: Option<&LaneStat>, rows: &mut Vec<Row>) {
    let key = task.key.as_str();
    if matches!(
        task.status,
        TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Stalled
    ) {
        let why = task
            .report
            .failure
            .clone()
            .or_else(|| stat.and_then(|s| s.first_error.clone()))
            .unwrap_or_else(|| "没有留下原因".to_owned());
        let (impact, direction) = match task.status {
            TaskStatus::Cancelled => (
                "主导取消了它，这一块没有进 run 的分支",
                "取消前先把能并的部分并进来，或把原因写进收尾说明",
            ),
            TaskStatus::Stalled => (
                "worker 没上报就停了，问题要人接住",
                "先问 worker 卡在哪（task_ask），别直接重派",
            ),
            _ => (
                "这一块没有合并，run 的产出少一部分",
                "把 spec 拆小；开工前让 worker 复述 goal 和验收命令",
            ),
        };
        rows.push(Row::new(
            format!(
                "#{key} 以「{}」结束：{}",
                task_status_word(task.status),
                one_line(&why, CELL_MAX)
            ),
            impact,
            direction,
        ));
    }
    let Some(stat) = stat else {
        return;
    };
    if stat.checks_failed > 0 {
        let what = stat
            .failed_check
            .clone()
            .unwrap_or_else(|| "一条检查".to_owned());
        rows.push(Row::new(
            format!(
                "#{key} 的验收 {}/{} 次没过：{what}",
                stat.checks_failed, stat.checks_runs
            ),
            "hub 拒绝任务收尾或合并，主导要自己重跑",
            "验收命令写进 spec 的 acceptance，worker 开工前先跑一遍",
        ));
    }
    work_rows(&format!("#{key}"), stat, rows);
    if stat.reroutes > 0 {
        let why = stat
            .reroute_reason
            .as_deref()
            .map(|r| format!("：{}", one_line(r, 120)))
            .unwrap_or_default();
        rows.push(Row::new(
            format!("#{key} 因额度被改派 {} 次{why}", stat.reroutes),
            "多一次分发和上下文重建",
            "把账号顺序和额度提前写进 run 策略，别等执行中才发现",
        ));
    }
    if !stat.handoffs.is_empty() {
        rows.push(Row::new(
            format!(
                "#{key} 换了 {} 次手：{}",
                stat.handoffs.len(),
                stat.handoffs.join("，")
            ),
            "每次换手都要重新交代上下文，容易丢决定",
            "换手时把 decisions 和 files 一起交过去，别只写 goal",
        ));
    }
    if stat.compacted > 0 {
        rows.push(Row::new(
            format!("#{key} 的上下文压缩了 {} 次", stat.compacted),
            "压缩后细节容易丢，后面几步可能重复查同一处",
            "把任务拆小，或把关键结论写进 run note",
        ));
    }
    if !stat.model_changes.is_empty() {
        rows.push(Row::new(
            format!(
                "#{key} 中途换了 {} 次模型：{}",
                stat.model_changes.len(),
                stat.model_changes.join("，")
            ),
            "换模型后要重新摸清上下文",
            "在 run 开始前定好模型；要换就写清为什么",
        ));
    }
}

/// Failing commands and failed turns, for the lead or one task's lane.
fn work_rows(label: &str, stat: &LaneStat, rows: &mut Vec<Row>) {
    let mut commands: Vec<(&String, &CommandStat)> = stat
        .commands
        .iter()
        .filter(|(_, c)| c.failures > 0)
        .collect();
    commands.sort_by(|a, b| b.1.failures.cmp(&a.1.failures).then_with(|| a.0.cmp(b.0)));
    for (command, stat) in commands.into_iter().take(COMMANDS_SHOWN) {
        let impact = match stat.failures {
            1 => "一个回合白跑".to_owned(),
            n => format!("同一处白跑 {n} 轮"),
        };
        let tail = stat
            .tail
            .as_deref()
            .map(|t| format!("；最后的输出：{}", one_line(t, TAIL_MAX)))
            .unwrap_or_default();
        rows.push(Row::new(
            format!(
                "{label}：`{command}` 失败 {} 次（最近 exit {}）",
                stat.failures,
                exit(stat.last_exit)
            ),
            impact,
            format!("把最后的失败输出带进下一次 brief{tail}"),
        ));
    }
    let bad = stat.failed_turns.max(stat.turn_errors);
    if bad > 0 {
        let error = stat
            .first_error
            .as_deref()
            .map(|e| format!("（{}）", one_line(e, 100)))
            .unwrap_or_default();
        rows.push(Row::new(
            format!("{label}：{bad} 个回合失败或中断{error}"),
            "worker 白跑，错误没被下一步接住",
            "开场说明里核对本机工具链与权限；失败时把错误原文带回给同一个 worker",
        ));
    }
}

fn run_rows(run: &Run, fold: &Fold, rows: &mut Vec<Row>) {
    if run.status == RunStatus::Failed {
        let note = run
            .note
            .as_deref()
            .map(|n| format!("（{}）", one_line(n, CELL_MAX)))
            .unwrap_or_default();
        rows.push(Row::new(
            format!("run：以失败收尾{note}"),
            "看状态会以为没做成；主导接手做完的也不一定算进产出",
            "收尾时把「谁接了哪一块」写清楚，或让 run 状态跟着实际产出走",
        ));
    }
    if let Some(failed) = run.checks.iter().find(|c| !c.passed) {
        let total = run.checks.len();
        let failed_count = run.checks.iter().filter(|c| !c.passed).count();
        rows.push(Row::new(
            format!(
                "run：项目验收 {failed_count}/{total} 没过：`{}`（exit {}）",
                failed.command,
                exit(failed.exit_code)
            ),
            "hub 拒绝 run_land 或 run_finish，要修好再调一次",
            "先把验收命令在本地跑通；spec 的 acceptance 与项目 checks 用同一条",
        ));
    }
    if !run.in_place
        && run.landed.is_none()
        && run.status == RunStatus::Completed
        && fold.landed.is_none()
    {
        rows.push(Row::new(
            "run：结束了但没有落地",
            "改动只在 run 分支上，别人看不到",
            "收尾前调 run_land；要人落地就在收尾通知里写清楚",
        ));
    }
}

/// What the run did not exercise, so the next one knows what is still unknown.
fn coverage(run: &Run, tasks: &[Task], fold: &Fold, issues: &[Note]) -> Vec<String> {
    let mut lines = Vec::new();
    let handoffs: usize = fold.stats.values().map(|s| s.handoffs.len()).sum();
    if !tasks.is_empty() && handoffs == 0 {
        lines.push("任务没有换手：第一次做对，或失败后没有换人重试。".to_owned());
    }
    if issues.is_empty() {
        lines.push(
            "没有 issue 笔记：没有 agent 把卡点写下来，表里只有 trace 看得到的一半。".to_owned(),
        );
    }
    if run.checks.is_empty() {
        lines.push(
            "项目没有声明验收（crossagents.toml 的 [run] checks）：对错只凭主导自述。".to_owned(),
        );
    }
    let failures: u32 = fold
        .stats
        .values()
        .flat_map(|s| s.commands.values())
        .map(|c| c.failures)
        .sum();
    if failures == 0 {
        lines.push("没有命令失败：这次没踩到工具或测试的坑。".to_owned());
    }
    if lines.is_empty() {
        lines.push("没有明显的空档：任务、换手、验收和 issue 都有记录。".to_owned());
    }
    lines
}

// ---------------------------------------------------------------------------------------
// The tool and the text helpers

impl Orchestrator {
    /// `run_retro`: writes a finished run's retro into the caller's own checkout (a lead's
    /// tool; its run's checkout, not the person's base one).
    pub(crate) async fn run_retro(&self, caller: &Agent, input: Value) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            #[serde(default)]
            run: Option<String>,
            /// Write again over a retro that is already there.
            #[serde(default)]
            force: bool,
        }
        let a: A = args(input)?;
        let run = self.retro_run(caller, a.run).await?;
        let doc = document(&*self.store, &run).await?;
        let path = write(&caller.cwd, run.id, &doc, a.force)?;
        Ok(json!({
            "path": path.display().to_string(),
            "checkout": caller.cwd.display().to_string(),
            "run": run.id,
            "findings": doc.findings,
            "issues": doc.issues,
            "next": "The file is for the person: 现象 / 影响 / 方向. The next run takes one row."
        }))
    }

    /// The run a retro names (an id, or a start or end of one), else the latest of the
    /// caller's that ended, and only one of the caller's project.
    async fn retro_run(&self, caller: &Agent, key: Option<String>) -> Result<Run, CoreError> {
        let run = match key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
            Some(key) => find_run(&self.store, key).await?,
            None => {
                // A lead's retro is about its own work: the newest of its runs that ended.
                let mut mine: Vec<Run> = self
                    .store
                    .list_runs()
                    .await?
                    .into_iter()
                    .filter(|r| r.status.is_terminal() && r.planner == caller.id)
                    .collect();
                mine.sort_by_key(|r| r.created_at);
                match mine.pop() {
                    Some(run) => run,
                    None => {
                        return Err(CoreError::Conflict(
                            "none of your runs has ended; name the run to review".into(),
                        ));
                    }
                }
            }
        };
        let theirs = self.project_for(&run.repo).await?;
        let here = self.project_for(&caller.cwd).await?;
        if theirs.id != here.id {
            return Err(CoreError::Conflict(
                "that run belongs to another project".into(),
            ));
        }
        Ok(run)
    }
}

/// A run by id, or by the start or end of one.
async fn find_run(
    store: &std::sync::Arc<dyn crossagents_core::Store>,
    key: &str,
) -> Result<Run, CoreError> {
    if let Ok(id) = key.parse::<RunId>() {
        return store.get_run(id).await;
    }
    let key = key.to_lowercase().replace('-', "");
    if key.len() < 4 {
        return Err(CoreError::Conflict(
            "give at least 4 characters of the run id".into(),
        ));
    }
    let mut found = store.list_runs().await?.into_iter().filter(|r| {
        let hex = r.id.to_string().replace('-', "");
        hex.starts_with(&key) || hex.ends_with(&key)
    });
    match (found.next(), found.next()) {
        (Some(run), None) => Ok(run),
        (None, _) => Err(CoreError::NotFound {
            kind: "run",
            id: key,
        }),
        _ => Err(CoreError::Conflict(format!(
            "{key} matches more than one run"
        ))),
    }
}

fn run_status_word(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Planning => "规划中",
        RunStatus::AwaitingApproval => "等批计划",
        RunStatus::Running => "进行中",
        RunStatus::Paused => "暂停",
        RunStatus::Completed => "已完成",
        RunStatus::Failed => "失败",
        RunStatus::Cancelled => "已取消",
    }
}

fn task_status_word(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "待办",
        TaskStatus::Running => "进行中",
        TaskStatus::WaitingAnswer => "等回答",
        TaskStatus::Verifying => "验收中",
        TaskStatus::Retrying => "重试中",
        TaskStatus::Escalated => "已升级",
        TaskStatus::Reviewing => "审查中",
        TaskStatus::Passed => "已通过",
        TaskStatus::Merged => "已合并",
        TaskStatus::Failed => "失败",
        TaskStatus::Cancelled => "已取消",
        TaskStatus::Stalled => "停滞",
    }
}

fn model_name(model: &Option<String>, effort: &Option<String>) -> String {
    let model = model.as_deref().unwrap_or("默认");
    match effort {
        Some(effort) => format!("{model} ({effort})"),
        None => model.to_owned(),
    }
}

fn exit(code: Option<i32>) -> String {
    code.map_or("?".into(), |c| c.to_string())
}

/// The end of a failed command's output, in one line.
fn tail(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() <= TAIL_MAX {
        return flat;
    }
    format!(
        "…{}",
        chars[chars.len() - TAIL_MAX..].iter().collect::<String>()
    )
}

fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    format!("{}…", flat.chars().take(max).collect::<String>())
}

/// A table cell: one line, no bare `|`.
fn cell(text: &str) -> String {
    one_line(text, CELL_MAX).replace('|', "\\|")
}

/// `1234567` → `1,234,567`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use crossagents_core::{RunConfig, RunPolicy, TaskId, TaskReport, TaskSpec};

    fn run_at(start: chrono::DateTime<Utc>) -> Run {
        Run {
            origin_chat_id: None,
            work_id: None,
            continuation: None,
            id: RunId::new(),
            title: "给 send 加附件".into(),
            request: "给 crossagents send 加 --file".into(),
            planner: AgentId::new(),
            status: RunStatus::Completed,
            config: RunConfig {
                worker: AgentId::new(),
                escalate: None,
                reviewer: None,
                auto_approve: false,
                max_parallel: RunConfig::DEFAULT_MAX_PARALLEL,
                max_attempts: RunConfig::DEFAULT_MAX_ATTEMPTS,
                team: Vec::new(),
                policy: RunPolicy::default(),
            },
            repo: std::path::PathBuf::from("/repo"),
            base_branch: "main".into(),
            branch: "crossagents/a1b2c3d4".into(),
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
            created_at: start,
            updated_at: start + Duration::minutes(12),
        }
    }

    fn task(run: RunId, key: &str, status: TaskStatus) -> Task {
        Task {
            id: TaskId::new(),
            run_id: run,
            key: key.into(),
            title: format!("task {key}"),
            spec: TaskSpec {
                goal: "g".into(),
                ..TaskSpec::default()
            },
            depends_on: Vec::new(),
            issuer: None,
            assignee: None,
            reviewer: None,
            worker_template: None,
            reviewer_template: None,
            worker_fresh: true,
            worker_restore: false,
            reviewer_fresh: true,
            status,
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
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn a_pipe_in_a_cell_does_not_split_the_table() {
        let run = run_at(Utc::now());
        let mut task = task(run.id, "api", TaskStatus::Failed);
        task.report.failure = Some("expected `a | b`\nsecond line".into());
        let doc = render(&run, &[task], &[], &[]);
        let md = &doc.markdown;
        let row = md
            .lines()
            .find(|line| line.starts_with("| 1 |"))
            .expect("the failed task has a row");
        assert!(row.contains("expected `a \\| b` second line"), "{row}");
        // Four cells (the number and the three columns) mean three separators.
        assert_eq!(row.matches(" | ").count(), 3, "{row}");
    }

    #[test]
    fn a_run_with_nothing_to_report_says_so() {
        let mut run = run_at(Utc::now());
        run.landed = Some(crossagents_core::Landing {
            mode: crossagents_core::LandMode::Squash,
            by: "person".into(),
            reference: "abc123".into(),
            at: run.updated_at,
        });
        let task = task(run.id, "api", TaskStatus::Merged);
        let doc = render(&run, &[task], &[], &[]);
        let md = &doc.markdown;
        // Nothing went wrong: the table says so rather than being empty.
        assert_eq!(doc.findings, 0);
        assert!(md.contains("| 1 | trace 和 issue 笔记里没有看出卡点 |"));
        // No event, no spend, and the absences are named.
        assert!(md.contains("- 花费：0 tokens，$0.00"));
        assert!(md.contains("任务没有换手"));
        assert!(md.contains("没有 issue 笔记"));
        assert!(md.contains("项目没有声明验收"));
    }

    #[test]
    fn the_short_id_is_the_tail_of_the_run_id() {
        let run = RunId::new();
        let hex = run.to_string().replace('-', "");
        assert_eq!(short_id(run), hex[hex.len() - 8..]);
        assert_eq!(
            file_name(run),
            format!("RETRO-{}.md", &hex[hex.len() - 8..])
        );
    }
}
