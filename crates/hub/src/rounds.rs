//! Agent turns as rounds: what started each turn, what it used and
//! whether it decided something. The hub watches the event stream; when a turn ends it
//! publishes one `agent.round` for it. Nothing here changes what an agent does.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use crossagents_core::{
    AgentId, Event, EventBus, EventKind, EventSink, MessageId, MessageSource, NoticeClass,
    RoundNotice, RoundSource, TokenUsage,
};
use tokio::sync::mpsc;

/// Whether a hub tool, once it succeeds, counts as a decision: it changes a task's or a run's
/// state, sends a message, asks a person, submits a plan, lands work, and so on. Reads do not
/// (`task_list`, `task_get`, `workspace_read`, `workspace_diff`, `usage_get`, `note_read`,
/// `note_list`, `lesson_search`, `lesson_get`, `agent_list`, `whoami`, `inbox_read`,
/// `watch_list`, ...), and neither do notes, lessons, progress reports or checks that only
/// record what they saw.
pub fn decides(tool: &str) -> bool {
    matches!(
        tool,
        // Messages and questions to people and teammates.
        "agent_send"
            | "message_withdraw"
            | "human_ask"
            | "human_notify"
            | "task_ask"
            | "task_answer"
            // Reminders and watches the agent sets for itself.
            | "remind_set"
            | "remind_cancel"
            | "watch"
            | "watch_cancel"
            // Plans, runs and landing.
            | "work_start"
            | "run_next"
            | "plan_submit"
            | "run_land"
            | "run_finish"
            | "run_apply"
            | "run_retro"
            // Task state.
            | "task_create"
            | "task_update"
            | "task_assign"
            | "task_cancel"
            | "task_complete"
            | "task_fail"
            | "task_accept"
            | "task_verify"
            | "task_rework"
            | "task_drop"
            | "review_request"
            | "review_submit"
            // Other agents and issues.
            | "agent_spawn"
            | "agent_retire"
            | "agent_set_model"
            | "agent_interrupt"
            | "issue_keep"
            | "expertise_triage"
            | "extension_request_enable"
    )
}

/// What the turn gathered so far.
#[derive(Default)]
struct Round {
    message_ids: Vec<MessageId>,
    sources: Vec<RoundSource>,
    notices: Vec<RoundNotice>,
    tools: Vec<String>,
    decided: bool,
    prompt_via_file: bool,
}

impl Round {
    fn join(&mut self, message_id: MessageId, waiting: Option<Waiting>) {
        if !self.message_ids.contains(&message_id) {
            self.message_ids.push(message_id);
        }
        if let Some(waiting) = waiting {
            if !self.sources.contains(&waiting.source) {
                self.sources.push(waiting.source);
            }
            self.notices.extend(waiting.notices);
        }
    }
}

/// A message that waits for its turn, as the hub accepted it.
struct Waiting {
    source: RoundSource,
    notices: Vec<RoundNotice>,
}

#[derive(Default)]
struct Lane {
    round: Round,
    /// The session's cumulative counts as last reported.
    last_tokens: Option<TokenUsage>,
    /// Those counts when the current turn started.
    start_tokens: Option<TokenUsage>,
    /// Whether the agent reported usage since the current turn started.
    usage_in_turn: bool,
}

/// Notice classes are kept per agent, up to this many lines, then the table starts over.
const CLASS_TABLE_LIMIT: usize = 10_000;

/// The per-agent state behind the rounds. Feed it events in order with [`Rounds::observe`].
#[derive(Default)]
pub struct Rounds {
    lanes: HashMap<AgentId, Lane>,
    waiting: HashMap<MessageId, Waiting>,
    classes: HashMap<AgentId, HashMap<String, NoticeClass>>,
    reminded: HashSet<AgentId>,
}

impl Rounds {
    /// Feeds one event; returns the `agent.round` it ends, if it ends a turn.
    pub fn observe(&mut self, kind: &EventKind) -> Option<EventKind> {
        match kind {
            EventKind::NoticeRecorded {
                id, line, class, ..
            } => {
                let table = self.classes.entry(*id).or_default();
                if table.len() >= CLASS_TABLE_LIMIT {
                    table.clear();
                }
                table.insert(line.clone(), *class);
            }
            EventKind::ReminderFired { owner, .. } | EventKind::WatchTriggered { owner, .. } => {
                self.reminded.insert(*owner);
            }
            EventKind::AgentInput {
                id,
                message_id,
                from,
                notices,
                ..
            } => {
                let source = self.source_of(*id, from.as_ref());
                let notices = notices
                    .iter()
                    .map(|line| RoundNotice {
                        line: line.clone(),
                        class: self
                            .classes
                            .get(id)
                            .and_then(|table| table.get(line))
                            .copied(),
                    })
                    .collect();
                self.waiting
                    .insert(*message_id, Waiting { source, notices });
            }
            EventKind::AgentInputWithdrawn { message_id, .. }
            | EventKind::AgentInputRefused { message_id, .. } => {
                self.waiting.remove(message_id);
            }
            EventKind::AgentTurnStarted { id, message_id } => {
                let waiting = self.waiting.remove(message_id);
                let lane = self.lanes.entry(*id).or_default();
                lane.start_tokens = lane.last_tokens.clone();
                lane.usage_in_turn = false;
                lane.round.join(*message_id, waiting);
            }
            EventKind::AgentSteerRead { id, message_id } => {
                let waiting = self.waiting.remove(message_id);
                self.lanes
                    .entry(*id)
                    .or_default()
                    .round
                    .join(*message_id, waiting);
            }
            EventKind::AgentToolUsed { id, tool, ok } => {
                let round = &mut self.lanes.entry(*id).or_default().round;
                if !round.tools.contains(tool) {
                    round.tools.push(tool.clone());
                }
                round.decided |= *ok && decides(tool);
            }
            EventKind::AgentPromptViaFile { id } => {
                self.lanes.entry(*id).or_default().round.prompt_via_file = true;
            }
            EventKind::AgentUsage {
                id,
                tokens: Some(tokens),
                ..
            } => {
                let lane = self.lanes.entry(*id).or_default();
                lane.last_tokens = Some(tokens.clone());
                lane.usage_in_turn = true;
            }
            EventKind::AgentTurnCompleted { id, message_id, .. } => {
                let waiting = self.waiting.remove(message_id);
                let lane = self.lanes.entry(*id).or_default();
                lane.round.join(*message_id, waiting);
                let tokens = if lane.usage_in_turn {
                    growth(lane.start_tokens.as_ref(), lane.last_tokens.as_ref())
                } else {
                    None
                };
                let round = std::mem::take(&mut lane.round);
                lane.start_tokens = None;
                lane.usage_in_turn = false;
                return Some(EventKind::AgentRound {
                    id: *id,
                    message_ids: round.message_ids,
                    sources: round.sources,
                    notices: round.notices,
                    tokens,
                    tools: round.tools,
                    decided: round.decided,
                    prompt_via_file: round.prompt_via_file,
                });
            }
            _ => {}
        }
        None
    }

    fn source_of(&mut self, id: AgentId, from: Option<&MessageSource>) -> RoundSource {
        match from {
            // Events written before the field existed were a person's.
            None | Some(MessageSource::Human) => RoundSource::Human,
            Some(MessageSource::Agent { agent_id }) => RoundSource::Teammate {
                agent_id: *agent_id,
            },
            Some(MessageSource::Orchestrator { run_id, task_id }) => RoundSource::Orchestrator {
                run_id: *run_id,
                task_id: *task_id,
            },
            Some(MessageSource::Notice) if self.reminded.remove(&id) => RoundSource::Reminder,
            Some(MessageSource::Notice) => RoundSource::Notice,
        }
    }
}

/// The model usage between two readings of a session's cumulative counts. A count that went
/// down means a new session started in between, so the later reading is all of its growth.
/// No start reading means the session began at zero.
fn growth(start: Option<&TokenUsage>, end: Option<&TokenUsage>) -> Option<TokenUsage> {
    let end = end?;
    let Some(start) = start else {
        return Some(end.clone());
    };
    let fell = end.input < start.input
        || end.cached_input < start.cached_input
        || end.cache_write_input < start.cache_write_input
        || end.output < start.output
        || end.reasoning_output < start.reasoning_output
        || end.total < start.total;
    if fell {
        return Some(end.clone());
    }
    let requests = match (end.requests, start.requests) {
        (Some(end), Some(start)) if end >= start => Some(end - start),
        _ => None,
    };
    Some(TokenUsage {
        input: end.input - start.input,
        cached_input: end.cached_input - start.cached_input,
        cache_write_input: end.cache_write_input - start.cache_write_input,
        output: end.output - start.output,
        reasoning_output: end.reasoning_output - start.reasoning_output,
        total: end.total - start.total,
        requests,
    })
}

/// The event sink that keeps the rounds. It sends each round it ends to the task that
/// publishes it ([`publish_rounds`]); the bus calls it after the event is stored, so nothing
/// is held across a publish.
pub struct RoundSink {
    rounds: Mutex<Rounds>,
    tx: mpsc::UnboundedSender<EventKind>,
}

impl RoundSink {
    /// The sink, and the receiver for the rounds it ends.
    pub fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<EventKind>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let sink = Self {
            rounds: Mutex::new(Rounds::default()),
            tx,
        };
        (Arc::new(sink), rx)
    }
}

#[async_trait]
impl EventSink for RoundSink {
    async fn handle(&self, event: &Event) {
        let ended = self
            .rounds
            .lock()
            .expect("rounds lock")
            .observe(&event.kind);
        if let Some(kind) = ended {
            // The receiver ends with the hub; a round then has nowhere to go.
            let _ = self.tx.send(kind);
        }
    }
}

/// Publishes the rounds the sink ends, in order, until the bus is gone. Holds the bus weakly:
/// the bus holds the sink, which holds the sender this task waits on.
pub fn publish_rounds(bus: Weak<EventBus>, mut rx: mpsc::UnboundedReceiver<EventKind>) {
    tokio::spawn(async move {
        while let Some(kind) = rx.recv().await {
            let Some(bus) = bus.upgrade() else { break };
            if let Err(err) = bus.publish(kind).await {
                tracing::warn!(error = %err, "cannot publish an agent round");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use crossagents_core::{AgentId, MessageId, RunId};

    use super::*;

    fn usage(input: u64, output: u64, requests: Option<u64>) -> TokenUsage {
        TokenUsage {
            input,
            cached_input: input / 2,
            cache_write_input: 1,
            output,
            reasoning_output: 0,
            total: input + output,
            requests,
        }
    }

    fn turn_round(rounds: &mut Rounds, kinds: Vec<EventKind>) -> Vec<EventKind> {
        kinds.iter().filter_map(|k| rounds.observe(k)).collect()
    }

    #[test]
    fn reads_do_not_decide_and_changes_do() {
        for read in [
            "task_list",
            "task_get",
            "workspace_read",
            "workspace_diff",
            "usage_get",
            "note_read",
            "note_list",
            "lesson_search",
            "lesson_get",
            "agent_list",
            "whoami",
            "inbox_read",
            "watch_list",
            "task_progress",
        ] {
            assert!(!decides(read), "{read} is a read");
        }
        for change in [
            "agent_send",
            "human_ask",
            "plan_submit",
            "run_land",
            "task_complete",
        ] {
            assert!(decides(change), "{change} decides");
        }
    }

    #[test]
    fn a_turn_collects_its_message_notices_tools_and_usage() {
        let id = AgentId::new();
        let message = MessageId::new();
        let run = RunId::new();
        let mut rounds = Rounds::default();
        let kinds = vec![
            EventKind::NoticeRecorded {
                id,
                topic: "progress".into(),
                line: "task t1 ready".into(),
                class: NoticeClass::Unblock,
            },
            EventKind::AgentUsage {
                id,
                used: 10,
                size: 0,
                cost_usd: None,
                tokens: Some(usage(100, 20, Some(2))),
            },
            EventKind::AgentInput {
                id,
                message_id: message,
                text: "go".into(),
                queued: false,
                from: Some(MessageSource::Orchestrator {
                    run_id: run,
                    task_id: None,
                }),
                reply_to: None,
                thread: None,
                reply_excerpt: None,
                attachments: vec![],
                notices: vec!["task t1 ready".into(), "not recorded".into()],
                queued_reason: None,
            },
            EventKind::AgentTurnStarted {
                id,
                message_id: message,
            },
            EventKind::AgentPromptViaFile { id },
            EventKind::AgentToolUsed {
                id,
                tool: "workspace_read".into(),
                ok: true,
            },
            EventKind::AgentToolUsed {
                id,
                tool: "task_complete".into(),
                ok: true,
            },
            EventKind::AgentToolUsed {
                id,
                tool: "task_complete".into(),
                ok: true,
            },
            EventKind::AgentUsage {
                id,
                used: 30,
                size: 0,
                cost_usd: None,
                tokens: Some(usage(160, 35, Some(5))),
            },
            EventKind::AgentTurnCompleted {
                id,
                message_id: message,
                stop_reason: crossagents_core::StopReason::EndTurn,
            },
        ];
        let ended = turn_round(&mut rounds, kinds);
        assert_eq!(ended.len(), 1);
        let EventKind::AgentRound {
            id: round_id,
            message_ids,
            sources,
            notices,
            tokens,
            tools,
            decided,
            prompt_via_file,
        } = ended[0].clone()
        else {
            panic!("a round")
        };
        assert_eq!(round_id, id);
        assert_eq!(message_ids, vec![message]);
        assert_eq!(
            sources,
            vec![RoundSource::Orchestrator {
                run_id: run,
                task_id: None
            }]
        );
        assert_eq!(
            notices,
            vec![
                RoundNotice {
                    line: "task t1 ready".into(),
                    class: Some(NoticeClass::Unblock),
                },
                RoundNotice {
                    line: "not recorded".into(),
                    class: None,
                },
            ]
        );
        // The turn's usage is the growth between its start and its end.
        let tokens = tokens.expect("usage was reported during the turn");
        assert_eq!(tokens.input, 60);
        assert_eq!(tokens.output, 15);
        assert_eq!(tokens.requests, Some(3));
        assert_eq!(tools, vec!["workspace_read", "task_complete"]);
        assert!(decided);
        assert!(prompt_via_file);
    }

    #[test]
    fn usage_is_the_growth_of_the_turn_and_unknown_without_a_report() {
        let id = AgentId::new();
        let mut rounds = Rounds::default();
        let first = MessageId::new();
        let second = MessageId::new();
        let third = MessageId::new();
        let mut kinds = vec![
            EventKind::AgentUsage {
                id,
                used: 0,
                size: 0,
                cost_usd: None,
                tokens: Some(usage(100, 10, Some(1))),
            },
            EventKind::AgentTurnStarted {
                id,
                message_id: first,
            },
            EventKind::AgentUsage {
                id,
                used: 0,
                size: 0,
                cost_usd: None,
                tokens: Some(usage(250, 40, Some(3))),
            },
            EventKind::AgentTurnCompleted {
                id,
                message_id: first,
                stop_reason: crossagents_core::StopReason::EndTurn,
            },
            // A turn in which the agent reports nothing.
            EventKind::AgentTurnStarted {
                id,
                message_id: second,
            },
            EventKind::AgentTurnCompleted {
                id,
                message_id: second,
                stop_reason: crossagents_core::StopReason::EndTurn,
            },
        ];
        // A session that restarted (its counts fell) and reports requests the agent does
        // not count.
        kinds.push(EventKind::AgentTurnStarted {
            id,
            message_id: third,
        });
        kinds.push(EventKind::AgentUsage {
            id,
            used: 0,
            size: 0,
            cost_usd: None,
            tokens: Some(usage(40, 4, None)),
        });
        kinds.push(EventKind::AgentTurnCompleted {
            id,
            message_id: third,
            stop_reason: crossagents_core::StopReason::EndTurn,
        });
        let ended = turn_round(&mut rounds, kinds);
        let tokens: Vec<Option<TokenUsage>> = ended
            .iter()
            .map(|k| match k {
                EventKind::AgentRound { tokens, .. } => tokens.clone(),
                _ => panic!("a round"),
            })
            .collect();
        assert_eq!(tokens.len(), 3);
        let first = tokens[0].as_ref().unwrap();
        assert_eq!(
            (first.input, first.output, first.requests),
            (150, 30, Some(2))
        );
        assert!(tokens[1].is_none(), "no usage was reported");
        let restarted = tokens[2].as_ref().unwrap();
        assert_eq!((restarted.input, restarted.output), (40, 4));
        assert_eq!(restarted.requests, None);
    }

    #[test]
    fn a_reminder_is_its_own_source_and_a_failed_decision_does_not_count() {
        let id = AgentId::new();
        let message = MessageId::new();
        let mut rounds = Rounds::default();
        let kinds = vec![
            EventKind::ReminderFired {
                owner: id,
                reminder_id: "r1".into(),
                note: "check".into(),
            },
            EventKind::AgentInput {
                id,
                message_id: message,
                text: "check".into(),
                queued: false,
                from: Some(MessageSource::Notice),
                reply_to: None,
                thread: None,
                reply_excerpt: None,
                attachments: vec![],
                notices: vec![],
                queued_reason: None,
            },
            EventKind::AgentTurnStarted {
                id,
                message_id: message,
            },
            EventKind::AgentToolUsed {
                id,
                tool: "agent_send".into(),
                ok: false,
            },
            EventKind::AgentTurnCompleted {
                id,
                message_id: message,
                stop_reason: crossagents_core::StopReason::EndTurn,
            },
        ];
        let ended = turn_round(&mut rounds, kinds);
        let EventKind::AgentRound {
            sources,
            decided,
            tools,
            ..
        } = &ended[0]
        else {
            panic!("a round")
        };
        assert_eq!(sources, &vec![RoundSource::Reminder]);
        assert!(!decided, "a send that failed decided nothing");
        assert_eq!(tools, &vec!["agent_send".to_owned()]);
    }

    #[test]
    fn tools_before_the_turn_and_steered_messages_join_the_round() {
        let id = AgentId::new();
        let first = MessageId::new();
        let steered = MessageId::new();
        let mut rounds = Rounds::default();
        let kinds = vec![
            EventKind::AgentToolUsed {
                id,
                tool: "task_update".into(),
                ok: true,
            },
            EventKind::AgentInput {
                id,
                message_id: steered,
                text: "also".into(),
                queued: true,
                from: Some(MessageSource::Human),
                reply_to: None,
                thread: None,
                reply_excerpt: None,
                attachments: vec![],
                notices: vec![],
                queued_reason: None,
            },
            EventKind::AgentTurnStarted {
                id,
                message_id: first,
            },
            EventKind::AgentSteerRead {
                id,
                message_id: steered,
            },
            EventKind::AgentTurnCompleted {
                id,
                message_id: first,
                stop_reason: crossagents_core::StopReason::EndTurn,
            },
            // The next turn starts with nothing carried over.
            EventKind::AgentTurnStarted {
                id,
                message_id: MessageId::new(),
            },
        ];
        let ended = turn_round(&mut rounds, kinds);
        assert_eq!(ended.len(), 1);
        let EventKind::AgentRound {
            message_ids,
            sources,
            decided,
            tools,
            ..
        } = &ended[0]
        else {
            panic!("a round")
        };
        assert_eq!(message_ids, &vec![first, steered]);
        assert_eq!(sources, &vec![RoundSource::Human]);
        assert!(decided, "task_update changes a task");
        assert_eq!(tools, &vec!["task_update".to_owned()]);
        assert!(rounds.lanes[&id].round.tools.is_empty());
    }

    #[test]
    fn the_round_serializes_its_unknown_tokens_as_null() {
        let kind = EventKind::AgentRound {
            id: AgentId::new(),
            message_ids: vec![],
            sources: vec![RoundSource::Human],
            notices: vec![],
            tokens: None,
            tools: vec![],
            decided: false,
            prompt_via_file: false,
        };
        let value = serde_json::to_value(&kind).unwrap();
        assert_eq!(value["type"], "agent.round");
        assert!(value["data"]["tokens"].is_null());
        assert_eq!(value["data"]["sources"][0]["kind"], "human");
    }
}
