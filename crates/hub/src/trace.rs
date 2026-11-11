//! A run's story by task (docs/tasks/DOGFOOD.md §8), folded from events only: from the store
//! for the API, or from `logs/events-*.jsonl` with no hub running. Workers are removed when
//! their task ends; their events stay, and so does this.

use std::collections::HashMap;

use crossagents_api_types::{
    CheckTrace, CommandTrace, DispatchTrace, LaneMark, LaneMarkKind, LaneTrace, RunTrace, Spent,
    StatusTrace, TaskTrace, TurnTrace,
};
use crossagents_core::{
    AgentId, AgentStatus, Event, EventKind, HumanItem, HumanItemKind, RunId, StopReason, TaskId,
};

/// A context below this many tokens is too small for a drop to mean a compaction.
const COMPACT_FROM: u64 = 20_000;

/// Characters of a failed command's output kept.
const TAIL: usize = 600;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Lane {
    Lead,
    Task(TaskId),
}

/// An agent's session totals, to turn cumulative usage reports into increments.
#[derive(Default)]
struct Session {
    tokens: u64,
    usd: f64,
    /// Context tokens in use at the last report, to spot a compaction.
    used: u64,
}

/// Folds events into the trace of `run`.
pub struct Tracer {
    run: RunId,
    trace: RunTrace,
    lanes: HashMap<AgentId, Lane>,
    tasks: HashMap<TaskId, usize>,
    sessions: HashMap<AgentId, Session>,
    /// Commands still waiting for their details: tool call id → (lane, index).
    commands: HashMap<String, (Lane, usize)>,
}

impl Tracer {
    pub fn new(run: RunId) -> Self {
        Self {
            run,
            trace: RunTrace {
                run_id: Some(run),
                ..RunTrace::default()
            },
            lanes: HashMap::new(),
            tasks: HashMap::new(),
            sessions: HashMap::new(),
            commands: HashMap::new(),
        }
    }

    fn task_mut(&mut self, id: TaskId, key: &str) -> &mut TaskTrace {
        let index = *self.tasks.entry(id).or_insert_with(|| {
            self.trace.tasks.push(TaskTrace {
                task_id: Some(id),
                key: key.to_owned(),
                ..TaskTrace::default()
            });
            self.trace.tasks.len() - 1
        });
        &mut self.trace.tasks[index]
    }

    fn lane_mut(&mut self, lane: Lane) -> &mut LaneTrace {
        match lane {
            Lane::Lead => &mut self.trace.lead,
            Lane::Task(id) => {
                let index = self.tasks[&id];
                &mut self.trace.tasks[index].lane
            }
        }
    }

    fn join(&mut self, agent: AgentId, lane: Lane) {
        self.lanes.insert(agent, lane);
        let lane = self.lane_mut(lane);
        if !lane.agents.contains(&agent) {
            lane.agents.push(agent);
        }
    }

    pub fn push(&mut self, event: &Event) {
        match &event.kind {
            EventKind::RunCreated { run } if run.id == self.run => {
                self.join(run.planner, Lane::Lead);
            }
            // The lead goes on to lead another run: what it does next is not ours.
            EventKind::RunCreated { run } => {
                if self.lanes.get(&run.planner) == Some(&Lane::Lead) {
                    self.lanes.remove(&run.planner);
                }
            }
            EventKind::TaskStatusChanged {
                run_id,
                id,
                key,
                to,
                assignee,
                from,
            } if *run_id == self.run => {
                let task = self.task_mut(*id, key);
                task.status = Some(*to);
                task.statuses.push(StatusTrace {
                    at: event.ts,
                    from: *from,
                    to: *to,
                    agent: *assignee,
                });
                if let Some(agent) = assignee {
                    self.join(*agent, Lane::Task(*id));
                }
            }
            EventKind::TaskRerouted {
                run_id,
                id,
                key,
                to,
                reason,
            } if *run_id == self.run => {
                self.task_mut(*id, key).rerouted.push(reason.clone());
                self.join(*to, Lane::Task(*id));
            }
            // Every dispatch: which accounts were weighed, best first, and why (§2.8).
            EventKind::TaskRouted {
                run_id,
                id,
                key,
                account,
                candidates,
            } if *run_id == self.run => {
                self.task_mut(*id, key).dispatches.push(DispatchTrace {
                    at: event.ts,
                    account: account.clone(),
                    candidates: candidates.clone(),
                });
            }
            EventKind::TaskHandedOff { run_id, handoff } if *run_id == self.run => {
                let (id, to) = (handoff.task_id, handoff.to);
                self.task_mut(id, &handoff.task_key)
                    .handoffs
                    .push(handoff.clone());
                self.join(to, Lane::Task(id));
            }
            EventKind::TaskChecked {
                run_id,
                id,
                key,
                passed,
                checks,
            } if *run_id == self.run => {
                self.task_mut(*id, key).checks.push(CheckTrace {
                    at: event.ts,
                    passed: *passed,
                    checks: checks.clone(),
                });
            }
            EventKind::TaskCommand {
                run_id,
                id,
                key,
                by,
                command,
                exit_code,
                duration_ms,
            } if *run_id == self.run => {
                self.task_mut(*id, key);
                self.lane_mut(Lane::Task(*id)).commands.push(CommandTrace {
                    agent: *by,
                    command: command.clone(),
                    exit_code: *exit_code,
                    duration_ms: Some(*duration_ms),
                    at: event.ts,
                    checker: true,
                    output_tail: None,
                });
            }
            EventKind::RunLanded { id, landing } if *id == self.run => {
                self.trace.landed = Some(landing.clone());
            }
            EventKind::HumanAsked { item } | EventKind::HumanAnswered { item }
                if item.run_id == Some(self.run) && item.kind == HumanItemKind::Question =>
            {
                let asked = matches!(event.kind, EventKind::HumanAsked { .. });
                let (kind, text) = if asked {
                    (LaneMarkKind::Asked, Some(item.text.clone()))
                } else {
                    (LaneMarkKind::Answered, item.answer.clone())
                };
                let lane = self.human_lane(item);
                self.lane_mut(lane).marks.push(LaneMark {
                    at: event.ts,
                    agent: item.from,
                    kind,
                    text,
                });
            }
            _ => self.push_agent(event),
        }
    }

    /// Where a question to the person goes: its task's lane, else its asker's, else the lead's.
    fn human_lane(&self, item: &HumanItem) -> Lane {
        if let Some(id) = item.task_id
            && self.tasks.contains_key(&id)
        {
            return Lane::Task(id);
        }
        self.lanes.get(&item.from).copied().unwrap_or(Lane::Lead)
    }

    fn push_agent(&mut self, event: &Event) {
        let Some(agent) = event.kind.agent_id() else {
            return;
        };
        let Some(&lane) = self.lanes.get(&agent) else {
            return;
        };
        match &event.kind {
            EventKind::AgentTurnStarted { message_id, .. } => {
                self.lane_mut(lane).turns.push(TurnTrace {
                    agent,
                    message_id: *message_id,
                    started_at: event.ts,
                    completed_at: None,
                    stop_reason: None,
                    error: None,
                });
            }
            EventKind::AgentTurnCompleted {
                message_id,
                stop_reason,
                ..
            } => {
                if let Some(turn) = self
                    .lane_mut(lane)
                    .turns
                    .iter_mut()
                    .rev()
                    .find(|t| t.message_id == *message_id)
                {
                    turn.completed_at = Some(event.ts);
                    turn.stop_reason = Some(*stop_reason);
                }
            }
            EventKind::AgentError { message, .. } | EventKind::AgentExited { message, .. } => {
                if let Some(turn) = self
                    .lane_mut(lane)
                    .turns
                    .iter_mut()
                    .rev()
                    .find(|t| t.agent == agent)
                {
                    turn.error.get_or_insert_with(|| message.clone());
                }
            }
            EventKind::AgentToolCall {
                tool_call_id,
                title,
                kind,
                ..
            } if kind.as_deref() == Some("execute") => {
                let commands = &mut self.lane_mut(lane).commands;
                commands.push(CommandTrace {
                    agent,
                    command: title.clone(),
                    exit_code: None,
                    duration_ms: None,
                    at: event.ts,
                    checker: false,
                    output_tail: None,
                });
                let index = commands.len() - 1;
                self.commands.insert(tool_call_id.clone(), (lane, index));
            }
            EventKind::AgentToolDetail {
                tool_call_id,
                detail,
                ..
            } => {
                let Some(&(lane, index)) = self.commands.get(tool_call_id) else {
                    return;
                };
                let command = &mut self.lane_mut(lane).commands[index];
                if let Some(text) = &detail.command {
                    command.command = text.clone();
                }
                command.exit_code = detail.exit_code.or(command.exit_code);
                command.duration_ms = detail.duration_ms.or(command.duration_ms);
                if detail.exit_code.is_some_and(|c| c != 0)
                    && let Some(output) = &detail.output
                {
                    command.output_tail = Some(tail(output));
                }
            }
            EventKind::AgentStatusChanged {
                to: AgentStatus::Starting,
                ..
            } => {
                self.sessions.remove(&agent);
            }
            EventKind::AgentModelChanged {
                model,
                effort,
                from_model,
                from_effort,
                ..
            } => {
                let name = |m: &Option<String>, e: &Option<String>| {
                    let m = m.as_deref().unwrap_or("默认");
                    match e {
                        Some(e) => format!("{m} ({e})"),
                        None => m.to_owned(),
                    }
                };
                let text = format!(
                    "{} → {}",
                    name(from_model, from_effort),
                    name(model, effort)
                );
                self.lane_mut(lane).marks.push(LaneMark {
                    at: event.ts,
                    agent,
                    kind: LaneMarkKind::ModelChanged,
                    text: Some(text),
                });
            }
            EventKind::AgentUsage {
                used,
                cost_usd,
                tokens,
                ..
            } => {
                let session = self.sessions.entry(agent).or_default();
                // No event says a context was compacted: a sharp drop of what is in use within
                // one session is one.
                let before = session.used;
                session.used = *used;
                let compacted = before >= COMPACT_FROM && *used > 0 && *used * 10 <= before * 6;
                let now_tokens = tokens.as_ref().map_or(session.tokens, |t| t.total);
                let now_usd = cost_usd.unwrap_or(session.usd);
                let add = Spent {
                    tokens: now_tokens.saturating_sub(session.tokens),
                    cost_usd: (now_usd - session.usd).max(0.0),
                };
                session.tokens = now_tokens;
                session.usd = now_usd;
                let spent = &mut self.lane_mut(lane).spent;
                spent.tokens += add.tokens;
                spent.cost_usd += add.cost_usd;
                self.trace.spent.tokens += add.tokens;
                self.trace.spent.cost_usd += add.cost_usd;
                if compacted {
                    self.lane_mut(lane).marks.push(LaneMark {
                        at: event.ts,
                        agent,
                        kind: LaneMarkKind::Compacted,
                        text: Some(format!("{}k → {}k", before / 1000, used / 1000)),
                    });
                }
            }
            _ => {}
        }
    }

    /// The trace, each task with its first failure.
    pub fn finish(mut self) -> RunTrace {
        for task in &mut self.trace.tasks {
            task.first_failure = first_failure(task);
        }
        self.trace
    }
}

/// The earliest failed check, failed command, or failed turn, in words.
fn first_failure(task: &TaskTrace) -> Option<String> {
    let checks = task.checks.iter().filter(|c| !c.passed).map(|c| {
        let failed = c.checks.iter().find(|r| !r.passed);
        let what = failed.map_or("a check".to_owned(), |r| {
            format!("`{}` (exit {})", r.command, exit(r.exit_code))
        });
        (c.at, format!("acceptance check failed: {what}"))
    });
    let commands = task
        .lane
        .commands
        .iter()
        .filter(|c| c.exit_code.is_some_and(|code| code != 0))
        .map(|c| {
            (
                c.at,
                format!("`{}` exited {}", c.command, exit(c.exit_code)),
            )
        });
    let turns = task.lane.turns.iter().filter_map(|t| {
        let failed = matches!(
            t.stop_reason,
            Some(StopReason::Error | StopReason::Interrupted)
        );
        match (&t.error, failed) {
            (Some(error), _) => Some((t.started_at, format!("a turn failed: {error}"))),
            (None, true) => Some((
                t.started_at,
                format!("a turn ended {:?}", t.stop_reason.unwrap()).to_lowercase(),
            )),
            _ => None,
        }
    });
    checks
        .chain(commands)
        .chain(turns)
        .min_by_key(|(at, _)| *at)
        .map(|(_, what)| what)
}

fn exit(code: Option<i32>) -> String {
    code.map_or("?".into(), |c| c.to_string())
}

fn tail(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= TAIL {
        return text.to_owned();
    }
    format!(
        "…{}",
        chars[chars.len() - TAIL..].iter().collect::<String>()
    )
}

/// Every event of `logs/` in order (`events-*.jsonl`, oldest day first). Lines that do not
/// parse (one half-written by a running hub) are skipped.
pub fn read_log(dir: &std::path::Path) -> std::io::Result<Vec<Event>> {
    let mut files: Vec<_> = std::fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            crate::event_log::date_of(&name).map(|date| (date, e.path()))
        })
        .collect();
    files.sort();
    let mut events = Vec::new();
    for (_, path) in files {
        let text = std::fs::read_to_string(path)?;
        events.extend(
            text.lines()
                .filter_map(|line| serde_json::from_str::<Event>(line).ok()),
        );
    }
    events.sort_by_key(|e| e.seq);
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use crossagents_core::{
        CheckResult, EVENT_SCHEMA_VERSION, MessageId, TaskStatus, TokenUsage, ToolDetail,
        ToolStatus,
    };

    struct Script {
        events: Vec<Event>,
        at: chrono::DateTime<Utc>,
    }

    impl Script {
        fn push(&mut self, kind: EventKind) {
            self.at += Duration::seconds(1);
            self.events.push(Event {
                seq: self.events.len() as u64 + 1,
                id: uuid::Uuid::now_v7(),
                ts: self.at,
                schema: EVENT_SCHEMA_VERSION,
                kind,
            });
        }
    }

    #[test]
    fn folds_a_task_with_its_turns_commands_checks_and_spend() {
        let (run, task, worker, other) =
            (RunId::new(), TaskId::new(), AgentId::new(), AgentId::new());
        let mut s = Script {
            events: Vec::new(),
            at: Utc::now(),
        };
        let turn = MessageId::new();
        s.push(EventKind::TaskStatusChanged {
            run_id: run,
            id: task,
            key: "a".into(),
            from: TaskStatus::Pending,
            to: TaskStatus::Running,
            assignee: Some(worker),
        });
        s.push(EventKind::AgentTurnStarted {
            id: worker,
            message_id: turn,
        });
        s.push(EventKind::AgentToolCall {
            id: worker,
            tool_call_id: "t1".into(),
            title: "cargo test".into(),
            kind: Some("execute".into()),
            status: ToolStatus::InProgress,
        });
        s.push(EventKind::AgentToolDetail {
            id: worker,
            tool_call_id: "t1".into(),
            detail: ToolDetail {
                command: Some("cargo test -p auth".into()),
                exit_code: Some(101),
                duration_ms: Some(9000),
                output: Some("test result: FAILED. 2 failed".into()),
                ..ToolDetail::default()
            },
        });
        let usage = |total: u64, usd: f64| EventKind::AgentUsage {
            id: worker,
            used: 0,
            size: 0,
            cost_usd: Some(usd),
            tokens: Some(TokenUsage {
                total,
                ..TokenUsage::default()
            }),
        };
        s.push(usage(1000, 0.10));
        s.push(usage(3000, 0.30));
        s.push(EventKind::AgentTurnCompleted {
            id: worker,
            message_id: turn,
            stop_reason: StopReason::EndTurn,
        });
        // Another agent's events are not this run's.
        s.push(EventKind::AgentTurnStarted {
            id: other,
            message_id: MessageId::new(),
        });
        s.push(EventKind::TaskChecked {
            run_id: run,
            id: task,
            key: "a".into(),
            passed: false,
            checks: vec![CheckResult {
                command: "cargo test".into(),
                exit_code: Some(101),
                passed: false,
                output: String::new(),
            }],
        });

        s.push(EventKind::TaskHandedOff {
            run_id: run,
            handoff: crossagents_core::Handoff {
                id: crossagents_core::HandoffId::new(),
                run_id: run,
                task_id: task,
                task_key: "a".into(),
                kind: crossagents_core::HandoffKind::Reassign,
                from: Some(worker),
                from_name: Some("rex".into()),
                to: other,
                to_name: "w-abc123-a".into(),
                template: Some("ada".into()),
                reason: None,
                goal: "g".into(),
                files: vec!["a.rs".into()],
                summary: None,
                decisions: Vec::new(),
                open_questions: Vec::new(),
                context_tokens: None,
                fresh: true,
                outcome: None,
                at: Utc::now(),
            },
        });

        let mut tracer = Tracer::new(run);
        for e in &s.events {
            tracer.push(e);
        }
        let trace = tracer.finish();
        assert_eq!(trace.tasks.len(), 1);
        let a = &trace.tasks[0];
        assert_eq!(a.key, "a");
        // Each handoff is a node of the task; its new worker joins the task's lane.
        assert_eq!(a.handoffs.len(), 1);
        assert_eq!(a.handoffs[0].files, ["a.rs"]);
        assert_eq!(a.lane.agents, [worker, other]);

        assert_eq!(a.lane.turns.len(), 1);
        assert_eq!(a.lane.turns[0].stop_reason, Some(StopReason::EndTurn));
        let cmd = &a.lane.commands[0];
        assert_eq!(
            (cmd.command.as_str(), cmd.exit_code),
            ("cargo test -p auth", Some(101))
        );
        assert!(cmd.output_tail.as_deref().unwrap().contains("FAILED"));
        assert_eq!(a.lane.spent.tokens, 3000);
        assert!((trace.spent.cost_usd - 0.30).abs() < 1e-9);
        // The command failed before the check did.
        assert_eq!(
            a.first_failure.as_deref(),
            Some("`cargo test -p auth` exited 101")
        );
    }

    /// Every dispatch says which accounts were weighed, best first, and why (§2.8): the
    /// trace keeps the order so `run trace` can explain the choice after the fact.
    #[test]
    fn a_dispatch_keeps_the_account_order_and_why() {
        let (run, task, worker) = (RunId::new(), TaskId::new(), AgentId::new());
        let mut s = Script {
            events: Vec::new(),
            at: Utc::now(),
        };
        s.push(EventKind::TaskStatusChanged {
            run_id: run,
            id: task,
            key: "a".into(),
            from: TaskStatus::Pending,
            to: TaskStatus::Running,
            assignee: Some(worker),
        });
        s.push(EventKind::TaskRouted {
            run_id: run,
            id: task,
            key: "a".into(),
            account: Some("codex-b".into()),
            candidates: vec![
                crossagents_core::AccountCandidate {
                    account: "codex-b".into(),
                    reason:
                        "its five_hour window resets first: used first so its quota is not lost"
                            .into(),
                },
                crossagents_core::AccountCandidate {
                    account: "codex-a".into(),
                    reason: "it failed (its limit is reached): last resort, not dropped".into(),
                },
            ],
        });

        let mut tracer = Tracer::new(run);
        for e in &s.events {
            tracer.push(e);
        }
        let trace = tracer.finish();
        let dispatch = &trace.tasks[0].dispatches[0];
        assert_eq!(dispatch.account.as_deref(), Some("codex-b"));
        let ranked: Vec<&str> = dispatch
            .candidates
            .iter()
            .map(|c| c.account.as_str())
            .collect();
        assert_eq!(ranked, ["codex-b", "codex-a"]);
        assert!(dispatch.candidates[0].reason.contains("resets first"));
        assert!(dispatch.candidates[1].reason.contains("last resort"));
    }

    /// The swimlane's nodes: each status change, a model switch, a compaction (a sharp drop
    /// of the context in use), a question to the person and its answer, and the landing.
    #[test]
    fn keeps_the_swimlane_nodes() {
        let (run, task, worker) = (RunId::new(), TaskId::new(), AgentId::new());
        let mut s = Script {
            events: Vec::new(),
            at: Utc::now(),
        };
        s.push(EventKind::TaskStatusChanged {
            run_id: run,
            id: task,
            key: "a".into(),
            from: TaskStatus::Pending,
            to: TaskStatus::Running,
            assignee: Some(worker),
        });
        let usage = |used: u64| EventKind::AgentUsage {
            id: worker,
            used,
            size: 200_000,
            cost_usd: None,
            tokens: None,
        };
        s.push(usage(150_000));
        s.push(usage(170_000));
        s.push(usage(40_000));
        s.push(usage(45_000));
        s.push(EventKind::AgentModelChanged {
            id: worker,
            model: Some("opus".into()),
            effort: Some("high".into()),
            from_model: Some("sonnet".into()),
            from_effort: None,
        });
        let item = |status, answer: Option<&str>| HumanItem {
            id: "q1".into(),
            kind: HumanItemKind::Question,
            from: worker,
            run_id: Some(run),
            task_id: Some(task),
            text: "用哪个库？".into(),
            detail: None,
            options: Vec::new(),
            default: None,
            timeout_at: None,
            status,
            answer: answer.map(Into::into),
            answered_at: None,
            action: None,
            created_at: Utc::now(),
        };
        s.push(EventKind::HumanAsked {
            item: item(crossagents_core::HumanItemStatus::Open, None),
        });
        s.push(EventKind::HumanAnswered {
            item: item(crossagents_core::HumanItemStatus::Answered, Some("serde")),
        });
        s.push(EventKind::TaskStatusChanged {
            run_id: run,
            id: task,
            key: "a".into(),
            from: TaskStatus::Running,
            to: TaskStatus::Verifying,
            assignee: Some(worker),
        });
        s.push(EventKind::RunLanded {
            id: run,
            landing: crossagents_core::Landing {
                mode: crossagents_core::LandMode::Squash,
                by: "person".into(),
                reference: "abc123".into(),
                at: Utc::now(),
            },
        });

        let mut tracer = Tracer::new(run);
        for e in &s.events {
            tracer.push(e);
        }
        let trace = tracer.finish();
        let a = &trace.tasks[0];
        let steps: Vec<_> = a.statuses.iter().map(|st| (st.from, st.to)).collect();
        assert_eq!(
            steps,
            [
                (TaskStatus::Pending, TaskStatus::Running),
                (TaskStatus::Running, TaskStatus::Verifying)
            ]
        );
        let marks: Vec<_> = a
            .lane
            .marks
            .iter()
            .map(|m| (m.kind, m.text.clone().unwrap_or_default()))
            .collect();
        assert_eq!(
            marks,
            [
                (LaneMarkKind::Compacted, "170k → 40k".to_owned()),
                (
                    LaneMarkKind::ModelChanged,
                    "sonnet → opus (high)".to_owned()
                ),
                (LaneMarkKind::Asked, "用哪个库？".to_owned()),
                (LaneMarkKind::Answered, "serde".to_owned()),
            ]
        );
        assert_eq!(trace.landed.map(|l| l.reference).as_deref(), Some("abc123"));
    }
}
