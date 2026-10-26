//! A run's story by task (docs/tasks/DOGFOOD.md §8), folded from events only: from the store
//! for the API, or from `logs/events-*.jsonl` with no hub running. Workers are removed when
//! their task ends; their events stay, and so does this.

use std::collections::HashMap;

use crossagents_api_types::{
    CheckTrace, CommandTrace, LaneTrace, RunTrace, Spent, TaskTrace, TurnTrace,
};
use crossagents_core::{AgentId, AgentStatus, Event, EventKind, RunId, StopReason, TaskId};

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
                ..
            } if *run_id == self.run => {
                self.task_mut(*id, key).status = Some(*to);
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
            _ => self.push_agent(event),
        }
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
            EventKind::AgentUsage {
                cost_usd, tokens, ..
            } => {
                let session = self.sessions.entry(agent).or_default();
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
                to_name: "ada".into(),
                reason: None,
                goal: "g".into(),
                files: vec!["a.rs".into()],
                summary: None,
                decisions: Vec::new(),
                open_questions: Vec::new(),
                context_tokens: None,
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
}
