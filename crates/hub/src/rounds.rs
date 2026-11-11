//! Agent turns as rounds: what started each turn, what it used and whether it decided
//! something. The hub watches the event stream; when a turn ends it publishes one
//! `agent.round` for it. Nothing here changes what an agent does.
//!
//! What a round can and cannot say:
//! - An external agent (`take_external`) has its turn started and ended at the moment the hub
//!   hands it its messages. The tools it calls after that count in the next round it has, so
//!   its rounds lag by one message, and it reports no usage (`tokens` null).
//! - The counts of a session are measured from the session's start (`agent.status_changed` to
//!   `starting`, the same boundary `usage.rs` uses). A hub that restarted or took over an agent
//!   has not seen that start: the agent's first turn then reports no usage, and later turns
//!   report their growth.
//! - Notices come from `notice.carried`, written when the hub marks them carried. A round takes
//!   a notice only if that event reaches it while its message is still in the round or waiting
//!   for its turn; one that arrives after the turn has ended is dropped.
//! - A hub restart loses what it had in memory. A message queued before the restart and run
//!   after it has no source and no notices in its round, and a turn the hub ends during
//!   recovery (the `TurnCompleted` it writes for the interrupted turn) is recorded as a round
//!   with no source either. Those rounds say less than the others; nothing else changes.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use crossagents_core::{
    AgentId, AgentStatus, Event, EventBus, EventKind, EventSink, MessageId, MessageSource,
    RoundNotice, RoundSource, TokenUsage,
};
use tokio::sync::mpsc;

/// Carried notices kept for messages not yet in a turn. Past this the table is emptied: a
/// fallback against a leak, since a normal wake takes its entry within moments.
const CARRIED_LIMIT: usize = 10_000;

/// Messages whose turn has ended, remembered so a `notice.carried` that arrives late is
/// dropped instead of kept for ever.
const ENDED_LIMIT: usize = 4_096;

/// Whether a hub tool, once it succeeds, counts as a decision: it changes a task's, a run's or
/// the shared notes' state, sends a message, asks a person, submits a plan, lands work, and so
/// on. A round in which none did is a round without a decision.
///
/// Only the tools listed here count. Every other tool counts for nothing, extension tools
/// (`<ext>__<tool>`) included, so a turn that changed state only through one of those is
/// recorded as undecided: that miss is accepted. A new hub tool that changes state is listed
/// here when it is added. Reads (`task_list`, `workspace_read`, ...), progress reports
/// (`task_progress`), and checks (`checks_run`, `workspace_run`) that only record what they saw
/// do not count.
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
            // Shared notes and lessons, and branch sync.
            | "note_write"
            | "lesson_create"
            | "lesson_merge"
            | "lesson_report"
            | "branch_sync"
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

/// Reminders and watches reach an agent as notices under these topics (`notices.rs`).
fn is_reminder_topic(topic: &str) -> bool {
    topic.starts_with("reminder:") || topic.starts_with("watch:")
}

/// Who sent a message, for the round's sources. A notice that a reminder or watch woke is a
/// reminder, not a plain notice.
fn source_of(from: Option<&MessageSource>, reminder: bool) -> RoundSource {
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
        Some(MessageSource::Notice) if reminder => RoundSource::Reminder,
        Some(MessageSource::Notice) => RoundSource::Notice,
    }
}

/// What the hub accepted for a message that waits for its turn.
struct Waiting {
    id: AgentId,
    from: Option<MessageSource>,
}

/// The notices a message carries, from `notice.carried`.
struct Carry {
    id: AgentId,
    /// Some of them are a reminder's or a watch's.
    reminder: bool,
    notices: Vec<RoundNotice>,
}

/// What a turn gathered so far.
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
    /// Takes a message into the round: its source the first time, and its notices. A message
    /// already in the round only adds its notices.
    fn join(&mut self, message: MessageId, waiting: Option<Waiting>, carry: Option<Carry>) {
        if self.message_ids.contains(&message) {
            if let Some(carry) = carry {
                self.take_notices(carry.notices);
            }
            return;
        }
        self.message_ids.push(message);
        if let Some(waiting) = waiting {
            let reminder = carry.as_ref().is_some_and(|c| c.reminder);
            let source = source_of(waiting.from.as_ref(), reminder);
            if !self.sources.contains(&source) {
                self.sources.push(source);
            }
        }
        if let Some(carry) = carry {
            self.take_notices(carry.notices);
        }
    }

    /// Adds notices, each (line, class) once.
    fn take_notices(&mut self, notices: Vec<RoundNotice>) {
        for notice in notices {
            if !self.notices.contains(&notice) {
                self.notices.push(notice);
            }
        }
    }
}

#[derive(Default)]
struct Lane {
    round: Round,
    /// The session's cumulative counts as last reported. Zero from the session's start; unknown
    /// until the hub has seen one (after a restart or a takeover).
    last: Option<TokenUsage>,
    /// `last` when the current turn started.
    start: Option<TokenUsage>,
    /// Whether the agent reported usage since the current turn started.
    usage_in_turn: bool,
}

/// The per-agent state behind the rounds. Feed it events in order with [`Rounds::observe`].
#[derive(Default)]
pub struct Rounds {
    lanes: HashMap<AgentId, Lane>,
    waiting: HashMap<MessageId, Waiting>,
    carried: HashMap<MessageId, Carry>,
    /// Messages whose turn has ended, oldest first.
    ended: VecDeque<MessageId>,
    ended_set: HashSet<MessageId>,
}

/// The counts of a session that has just started.
fn session_start() -> TokenUsage {
    TokenUsage {
        requests: Some(0),
        ..TokenUsage::default()
    }
}

impl Rounds {
    /// Feeds one event; returns the `agent.round` it ends, if it ends a turn.
    pub fn observe(&mut self, kind: &EventKind) -> Option<EventKind> {
        match kind {
            EventKind::AgentInput {
                id,
                message_id,
                from,
                ..
            } => {
                self.waiting.insert(
                    *message_id,
                    Waiting {
                        id: *id,
                        from: *from,
                    },
                );
            }
            EventKind::NoticeCarried {
                id,
                message_id,
                notices,
            } => {
                let reminder = notices.iter().any(|n| is_reminder_topic(&n.topic));
                let carry = Carry {
                    id: *id,
                    reminder,
                    notices: notices
                        .iter()
                        .map(|n| RoundNotice {
                            line: n.line.clone(),
                            class: n.class,
                        })
                        .collect(),
                };
                // A message already in its agent's round takes these now; otherwise they wait
                // for its turn. A later one for the same message replaces this one. One that
                // comes after its turn has ended is dropped: nothing would take it.
                match self
                    .lanes
                    .get_mut(id)
                    .filter(|lane| lane.round.message_ids.contains(message_id))
                {
                    Some(lane) => lane.round.take_notices(carry.notices),
                    None if self.ended_set.contains(message_id) => {}
                    None => {
                        if self.carried.len() >= CARRIED_LIMIT {
                            self.carried.clear();
                        }
                        self.carried.insert(*message_id, carry);
                    }
                }
            }
            EventKind::AgentSteered {
                id,
                message_id,
                from,
                ..
            } => {
                // A steered message takes its source from here, as an input does.
                self.waiting.insert(
                    *message_id,
                    Waiting {
                        id: *id,
                        from: *from,
                    },
                );
            }
            EventKind::AgentInputWithdrawn { message_id, .. }
            | EventKind::AgentInputRefused { message_id, .. } => {
                self.waiting.remove(message_id);
                self.carried.remove(message_id);
            }
            EventKind::AgentTurnStarted { id, message_id } => {
                let (waiting, carry) = (
                    self.waiting.remove(message_id),
                    self.carried.remove(message_id),
                );
                let lane = self.lanes.entry(*id).or_default();
                lane.start = lane.last.clone();
                lane.usage_in_turn = false;
                lane.round.join(*message_id, waiting, carry);
            }
            EventKind::AgentSteerRead { id, message_id } => {
                let (waiting, carry) = (
                    self.waiting.remove(message_id),
                    self.carried.remove(message_id),
                );
                self.lanes
                    .entry(*id)
                    .or_default()
                    .round
                    .join(*message_id, waiting, carry);
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
            EventKind::AgentStatusChanged {
                id,
                to: AgentStatus::Starting,
                ..
            } => {
                self.lanes.entry(*id).or_default().last = Some(session_start());
            }
            EventKind::AgentUsage {
                id,
                tokens: Some(tokens),
                ..
            } => {
                let lane = self.lanes.entry(*id).or_default();
                lane.last = Some(tokens.clone());
                lane.usage_in_turn = true;
            }
            EventKind::AgentTurnCompleted { id, message_id, .. } => {
                let (waiting, carry) = (
                    self.waiting.remove(message_id),
                    self.carried.remove(message_id),
                );
                let lane = self.lanes.entry(*id).or_default();
                lane.round.join(*message_id, waiting, carry);
                let tokens = if lane.usage_in_turn {
                    growth(lane.start.as_ref(), lane.last.as_ref())
                } else {
                    None
                };
                let round = std::mem::take(&mut lane.round);
                lane.start = None;
                lane.usage_in_turn = false;
                self.end_messages(&round.message_ids);
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
            EventKind::AgentRemoved { id, .. } => self.forget(*id),
            EventKind::AgentUpdated { agent } if agent.archived_at.is_some() => {
                self.forget(agent.id)
            }
            _ => {}
        }
        None
    }

    /// Remembers that these messages' turn has ended.
    fn end_messages(&mut self, messages: &[MessageId]) {
        for message in messages {
            if self.ended_set.insert(*message) {
                self.ended.push_back(*message);
            }
        }
        while self.ended.len() > ENDED_LIMIT {
            if let Some(old) = self.ended.pop_front() {
                self.ended_set.remove(&old);
            }
        }
    }

    /// Drops what an agent that is gone (removed, or archived) still had in the table.
    fn forget(&mut self, id: AgentId) {
        self.lanes.remove(&id);
        self.waiting.retain(|_, waiting| waiting.id != id);
        self.carried.retain(|_, carry| carry.id != id);
    }
}

/// The model usage between two readings of a session's cumulative counts. Without a reading
/// from before the turn there is no baseline, so the turn reports none. A count that went down
/// means a new session started in between, so the later reading is all of its growth.
fn growth(start: Option<&TokenUsage>, end: Option<&TokenUsage>) -> Option<TokenUsage> {
    let (start, end) = (start?, end?);
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
    use crossagents_core::{AgentId, CarriedNotice, MessageId, NoticeClass, RunId, StopReason};

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

    fn carried(
        id: AgentId,
        message: MessageId,
        topic: &str,
        line: &str,
        class: NoticeClass,
    ) -> EventKind {
        EventKind::NoticeCarried {
            id,
            message_id: message,
            notices: vec![CarriedNotice {
                line: line.into(),
                class,
                topic: topic.into(),
            }],
        }
    }

    fn input(id: AgentId, message: MessageId, from: Option<MessageSource>) -> EventKind {
        EventKind::AgentInput {
            id,
            message_id: message,
            text: "go".into(),
            queued: false,
            from,
            reply_to: None,
            thread: None,
            reply_excerpt: None,
            attachments: vec![],
            notices: vec![],
            queued_reason: None,
        }
    }

    fn started(id: AgentId, message: MessageId) -> EventKind {
        EventKind::AgentTurnStarted {
            id,
            message_id: message,
        }
    }

    fn completed(id: AgentId, message: MessageId) -> EventKind {
        EventKind::AgentTurnCompleted {
            id,
            message_id: message,
            stop_reason: StopReason::EndTurn,
        }
    }

    fn session_started(id: AgentId) -> EventKind {
        EventKind::AgentStatusChanged {
            id,
            from: AgentStatus::Idle,
            to: AgentStatus::Starting,
        }
    }

    fn usage_event(id: AgentId, tokens: TokenUsage) -> EventKind {
        EventKind::AgentUsage {
            id,
            used: 0,
            size: 0,
            cost_usd: None,
            tokens: Some(tokens),
        }
    }

    /// Feeds `kinds` and returns the rounds they end.
    fn run(rounds: &mut Rounds, kinds: Vec<EventKind>) -> Vec<EventKind> {
        kinds.iter().filter_map(|k| rounds.observe(k)).collect()
    }

    fn tokens_of(kind: &EventKind) -> Option<TokenUsage> {
        match kind {
            EventKind::AgentRound { tokens, .. } => tokens.clone(),
            _ => panic!("a round"),
        }
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
            "ext__tool",
            "workspace_run",
            "checks_run",
        ] {
            assert!(!decides(read), "{read} is not a decision");
        }
        for change in [
            "agent_send",
            "human_ask",
            "plan_submit",
            "run_land",
            "task_complete",
            "note_write",
            "lesson_create",
            "lesson_merge",
            "lesson_report",
            "branch_sync",
        ] {
            assert!(decides(change), "{change} decides");
        }
    }

    #[test]
    fn a_turn_collects_its_message_notices_tools_and_usage() {
        let id = AgentId::new();
        let message = MessageId::new();
        let run_id = RunId::new();
        let mut rounds = Rounds::default();
        let ended = run(
            &mut rounds,
            vec![
                session_started(id),
                usage_event(id, usage(100, 20, Some(2))),
                carried(
                    id,
                    message,
                    "progress",
                    "task t1 ready",
                    NoticeClass::Unblock,
                ),
                // The same notice twice counts once.
                carried(
                    id,
                    message,
                    "progress",
                    "task t1 ready",
                    NoticeClass::Unblock,
                ),
                input(
                    id,
                    message,
                    Some(MessageSource::Orchestrator {
                        run_id,
                        task_id: None,
                    }),
                ),
                started(id, message),
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
                usage_event(id, usage(160, 35, Some(5))),
                completed(id, message),
            ],
        );
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
                run_id,
                task_id: None
            }]
        );
        assert_eq!(
            notices,
            vec![RoundNotice {
                line: "task t1 ready".into(),
                class: NoticeClass::Unblock,
            }]
        );
        // The reading before the turn (100 in, 20 out, 2 requests) is its baseline: the turn used
        // what the session counted after it.
        let tokens = tokens.expect("usage was reported during the turn");
        assert_eq!(tokens.input, 60);
        assert_eq!(tokens.output, 15);
        assert_eq!(tokens.requests, Some(3));
        assert_eq!(tools, vec!["workspace_read", "task_complete"]);
        assert!(decided);
        assert!(prompt_via_file);
    }

    #[test]
    fn consecutive_turns_report_their_own_growth() {
        let id = AgentId::new();
        let (first, second) = (MessageId::new(), MessageId::new());
        let mut rounds = Rounds::default();
        let ended = run(
            &mut rounds,
            vec![
                session_started(id),
                started(id, first),
                usage_event(id, usage(100, 10, Some(1))),
                completed(id, first),
                started(id, second),
                usage_event(id, usage(160, 25, Some(3))),
                completed(id, second),
            ],
        );
        let (a, b) = (tokens_of(&ended[0]).unwrap(), tokens_of(&ended[1]).unwrap());
        assert_eq!((a.input, a.output, a.requests), (100, 10, Some(1)));
        assert_eq!((b.input, b.output, b.requests), (60, 15, Some(2)));
    }

    #[test]
    fn a_new_session_counts_from_zero_and_a_turn_without_usage_reports_none() {
        let id = AgentId::new();
        let (first, second, third) = (MessageId::new(), MessageId::new(), MessageId::new());
        let mut rounds = Rounds::default();
        let ended = run(
            &mut rounds,
            vec![
                session_started(id),
                started(id, first),
                usage_event(id, usage(500, 50, Some(5))),
                completed(id, first),
                // A new session: its counts start again, from zero.
                session_started(id),
                started(id, second),
                usage_event(id, usage(30, 3, Some(1))),
                completed(id, second),
                started(id, third),
                completed(id, third),
            ],
        );
        assert_eq!(ended.len(), 3);
        let a = tokens_of(&ended[0]).unwrap();
        assert_eq!((a.input, a.output, a.requests), (500, 50, Some(5)));
        let b = tokens_of(&ended[1]).unwrap();
        assert_eq!((b.input, b.output, b.requests), (30, 3, Some(1)));
        assert!(tokens_of(&ended[2]).is_none(), "no usage was reported");
    }

    #[test]
    fn without_a_baseline_the_first_turn_reports_none_and_the_next_grows() {
        // A hub that took over an agent never saw its session start: its counts have no
        // baseline, so the first turn cannot say what it used.
        let id = AgentId::new();
        let (first, second) = (MessageId::new(), MessageId::new());
        let mut rounds = Rounds::default();
        let ended = run(
            &mut rounds,
            vec![
                started(id, first),
                usage_event(id, usage(250, 40, Some(3))),
                completed(id, first),
                started(id, second),
                usage_event(id, usage(300, 45, Some(4))),
                completed(id, second),
            ],
        );
        assert!(
            tokens_of(&ended[0]).is_none(),
            "no baseline: null, not the session's total"
        );
        let next = tokens_of(&ended[1]).unwrap();
        assert_eq!((next.input, next.output, next.requests), (50, 5, Some(1)));
    }

    #[test]
    fn a_count_that_fell_means_a_new_session_and_its_reading_is_all_growth() {
        let id = AgentId::new();
        let (first, second) = (MessageId::new(), MessageId::new());
        let mut rounds = Rounds::default();
        let ended = run(
            &mut rounds,
            vec![
                session_started(id),
                started(id, first),
                usage_event(id, usage(250, 40, Some(3))),
                completed(id, first),
                // Reset without a status change the hub saw: the counts fell.
                started(id, second),
                usage_event(id, usage(40, 4, None)),
                completed(id, second),
            ],
        );
        let restarted = tokens_of(&ended[1]).unwrap();
        assert_eq!((restarted.input, restarted.output), (40, 4));
        assert_eq!(restarted.requests, None);
    }

    #[test]
    fn a_reminder_or_watch_notice_is_a_reminder_source_and_other_notices_are_not() {
        let id = AgentId::new();
        let (reminded, plain, aside) = (MessageId::new(), MessageId::new(), MessageId::new());
        let mut rounds = Rounds::default();
        let ended = run(
            &mut rounds,
            vec![
                carried(
                    id,
                    reminded,
                    "reminder:r1",
                    "Reminder (r1): check",
                    NoticeClass::Decide,
                ),
                input(id, reminded, Some(MessageSource::Notice)),
                started(id, reminded),
                completed(id, reminded),
                carried(id, plain, "progress", "task t1 ready", NoticeClass::Unblock),
                input(id, plain, Some(MessageSource::Notice)),
                started(id, plain),
                completed(id, plain),
                // A person's message with a watch's notice riding along is a person's round.
                carried(id, aside, "watch:w1", "news", NoticeClass::Decide),
                input(id, aside, Some(MessageSource::Human)),
                started(id, aside),
                completed(id, aside),
            ],
        );
        let source = |kind: &EventKind| match kind {
            EventKind::AgentRound { sources, .. } => sources.clone(),
            _ => panic!("a round"),
        };
        assert_eq!(source(&ended[0]), vec![RoundSource::Reminder]);
        assert_eq!(source(&ended[1]), vec![RoundSource::Notice]);
        assert_eq!(source(&ended[2]), vec![RoundSource::Human]);
    }

    #[test]
    fn a_failed_decision_does_not_count() {
        let id = AgentId::new();
        let message = MessageId::new();
        let mut rounds = Rounds::default();
        let ended = run(
            &mut rounds,
            vec![
                input(id, message, Some(MessageSource::Notice)),
                started(id, message),
                EventKind::AgentToolUsed {
                    id,
                    tool: "agent_send".into(),
                    ok: false,
                },
                completed(id, message),
            ],
        );
        let EventKind::AgentRound { decided, tools, .. } = &ended[0] else {
            panic!("a round")
        };
        assert!(!decided, "a send that failed decided nothing");
        assert_eq!(tools, &vec!["agent_send".to_owned()]);
    }

    fn steer(id: AgentId, message: MessageId, from: Option<MessageSource>) -> EventKind {
        EventKind::AgentSteered {
            id,
            message_id: message,
            text: "also".into(),
            from,
            attachments: vec![],
        }
    }

    #[test]
    fn tools_before_the_turn_and_steered_messages_join_the_round() {
        let id = AgentId::new();
        let (first, steered) = (MessageId::new(), MessageId::new());
        let teammate = AgentId::new();
        let mut rounds = Rounds::default();
        // A steered message comes with its own event only: no input is written for it.
        let ended = run(
            &mut rounds,
            vec![
                EventKind::AgentToolUsed {
                    id,
                    tool: "task_update".into(),
                    ok: true,
                },
                started(id, first),
                steer(
                    id,
                    steered,
                    Some(MessageSource::Agent { agent_id: teammate }),
                ),
                EventKind::AgentSteerRead {
                    id,
                    message_id: steered,
                },
                completed(id, first),
                // The next turn starts with nothing carried over.
                started(id, MessageId::new()),
            ],
        );
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
        assert_eq!(
            sources,
            &vec![RoundSource::Teammate { agent_id: teammate }],
            "the steered message's source is recorded"
        );
        assert!(decided, "task_update changes a task");
        assert_eq!(tools, &vec!["task_update".to_owned()]);
    }

    #[test]
    fn a_steer_from_before_the_field_existed_is_a_persons() {
        let id = AgentId::new();
        let (first, steered) = (MessageId::new(), MessageId::new());
        let mut rounds = Rounds::default();
        let ended = run(
            &mut rounds,
            vec![
                started(id, first),
                steer(id, steered, None),
                EventKind::AgentSteerRead {
                    id,
                    message_id: steered,
                },
                completed(id, first),
            ],
        );
        let EventKind::AgentRound { sources, .. } = &ended[0] else {
            panic!("a round")
        };
        assert_eq!(sources, &vec![RoundSource::Human]);
    }

    #[test]
    fn a_notice_carried_during_its_turn_is_in_the_round() {
        // The hub may write `notice.carried` after the turn has started (a person's message is
        // queued before its notices are marked): the round still takes them.
        let id = AgentId::new();
        let message = MessageId::new();
        let mut rounds = Rounds::default();
        let ended = run(
            &mut rounds,
            vec![
                input(id, message, Some(MessageSource::Human)),
                started(id, message),
                carried(id, message, "watch:w1", "news", NoticeClass::Decide),
                completed(id, message),
            ],
        );
        let EventKind::AgentRound { notices, .. } = &ended[0] else {
            panic!("a round")
        };
        assert_eq!(
            notices,
            &vec![RoundNotice {
                line: "news".into(),
                class: NoticeClass::Decide,
            }]
        );
    }

    #[test]
    fn a_notice_carried_after_its_turn_ended_stays_out_of_the_table() {
        let id = AgentId::new();
        let message = MessageId::new();
        let mut rounds = Rounds::default();
        let ended = run(
            &mut rounds,
            vec![
                input(id, message, Some(MessageSource::Human)),
                started(id, message),
                completed(id, message),
                // Too late for its round: dropped, not kept for a message that will not start.
                carried(id, message, "progress", "late", NoticeClass::Batch),
            ],
        );
        assert_eq!(ended.len(), 1);
        assert!(rounds.carried.is_empty(), "the late notice was not kept");
        assert!(rounds.ended_set.contains(&message));
        // Nothing of it reaches a later round either.
        let later = MessageId::new();
        let next = run(&mut rounds, vec![started(id, later), completed(id, later)]);
        let EventKind::AgentRound { notices, .. } = &next[0] else {
            panic!("a round")
        };
        assert!(notices.is_empty());
    }

    #[test]
    fn a_removed_agent_leaves_nothing_behind() {
        let id = AgentId::new();
        let message = MessageId::new();
        let mut rounds = Rounds::default();
        // Its waiting message and its carried notices are dropped with it.
        run(
            &mut rounds,
            vec![
                carried(id, message, "watch:w1", "news", NoticeClass::Decide),
                input(id, message, Some(MessageSource::Notice)),
                EventKind::AgentToolUsed {
                    id,
                    tool: "agent_send".into(),
                    ok: true,
                },
                EventKind::AgentRemoved {
                    id,
                    name: "kai".into(),
                },
            ],
        );
        assert!(rounds.lanes.is_empty());
        assert!(rounds.waiting.is_empty());
        assert!(rounds.carried.is_empty());
        // A turn that starts afterwards knows nothing of the removed agent's message.
        let ended = run(
            &mut rounds,
            vec![started(id, message), completed(id, message)],
        );
        let EventKind::AgentRound {
            sources, notices, ..
        } = &ended[0]
        else {
            panic!("a round")
        };
        assert!(sources.is_empty() && notices.is_empty());
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
