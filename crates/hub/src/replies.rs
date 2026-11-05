//! Answers that would otherwise stay where they were written. A teammate's message starts a
//! turn; an agent that answers it only in that turn's text (no `agent_send` back) leaves the
//! answer in its own conversation, and the sender never hears it. The hub forwards that text
//! to the sender as a message from the agent that wrote it.
//!
//! Only the answer to a message a teammate sent on purpose is forwarded: the answer to a
//! forwarded one is not, so two agents never bounce text between them. Agents working on a
//! run's task report through the task tools, so [`Runtime`](crate::runtime::Runtime) skips
//! them; this module only decides from the event stream.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crossagents_core::{AgentId, EventKind, MessageId, MessageSource, OutputStream, StopReason};

/// Longest answer forwarded whole; the rest stays in the agent's conversation.
const MAX_FORWARD: usize = 6000;
/// Messages that never started a turn are forgotten after this.
const FORGET: Duration = Duration::from_secs(24 * 3600);

/// An answer to forward: `text`, written by `from`, to `to`, answering `answers`.
#[derive(Debug, Clone, PartialEq)]
pub struct Forward {
    pub from: AgentId,
    pub to: AgentId,
    pub answers: MessageId,
    pub text: String,
}

/// A turn that answers a teammate's message.
struct Turn {
    message: MessageId,
    asker: AgentId,
    started: Instant,
    /// What the agent said since its last tool call: the answer it ends with.
    text: String,
}

#[derive(Default)]
pub struct ReplyTracker {
    /// Teammates' messages not started yet: recipient, sender, when delivered.
    asked: HashMap<MessageId, (AgentId, AgentId, Instant)>,
    /// Messages the hub forwarded: their answers are not forwarded again.
    forwarded: HashSet<MessageId>,
    turns: HashMap<AgentId, Turn>,
    /// The last message each agent sent each other agent, by (from, to).
    sent: HashMap<(AgentId, AgentId), Instant>,
}

impl ReplyTracker {
    /// `from` sent `to` a message (through the hub, at `now`).
    pub fn note_send(&mut self, from: AgentId, to: AgentId, now: Instant) {
        self.sent.insert((from, to), now);
    }

    /// The hub is about to deliver `id` as a forwarded answer.
    pub fn mark_forwarded(&mut self, id: MessageId) {
        self.forwarded.insert(id);
    }

    /// Follows one event; says what to forward when a turn that answers a teammate ends.
    pub fn on_event(&mut self, kind: &EventKind, now: Instant) -> Option<Forward> {
        match kind {
            EventKind::AgentInput {
                id,
                message_id,
                from: Some(MessageSource::Agent { agent_id }),
                ..
            } => {
                if agent_id != id && !self.forwarded.remove(message_id) {
                    self.asked
                        .retain(|_, (_, _, at)| now.duration_since(*at) < FORGET);
                    self.asked.insert(*message_id, (*id, *agent_id, now));
                }
                None
            }
            EventKind::AgentTurnStarted { id, message_id } => {
                match self.asked.remove(message_id) {
                    Some((recipient, asker, _)) if recipient == *id => {
                        self.turns.insert(
                            *id,
                            Turn {
                                message: *message_id,
                                asker,
                                started: now,
                                text: String::new(),
                            },
                        );
                    }
                    _ => {
                        self.turns.remove(id);
                    }
                }
                None
            }
            EventKind::AgentOutput {
                id,
                stream: OutputStream::Text,
                text,
            } => {
                if let Some(turn) = self.turns.get_mut(id) {
                    turn.text.push_str(text);
                }
                None
            }
            EventKind::AgentToolCall { id, .. } => {
                if let Some(turn) = self.turns.get_mut(id) {
                    turn.text.clear();
                }
                None
            }
            EventKind::AgentExited { id, .. } => {
                self.turns.remove(id);
                None
            }
            EventKind::AgentTurnCompleted {
                id,
                message_id,
                stop_reason,
            } => {
                let turn = self.turns.remove(id)?;
                if turn.message != *message_id || *stop_reason != StopReason::EndTurn {
                    return None;
                }
                // It answered on its own (to the right agent) during the turn.
                if self
                    .sent
                    .get(&(*id, turn.asker))
                    .is_some_and(|at| *at >= turn.started)
                {
                    return None;
                }
                let text = turn.text.trim();
                if text.is_empty() {
                    return None;
                }
                Some(Forward {
                    from: *id,
                    to: turn.asker,
                    answers: turn.message,
                    text: clip(text),
                })
            }
            _ => None,
        }
    }
}

fn clip(text: &str) -> String {
    if text.chars().count() <= MAX_FORWARD {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(MAX_FORWARD).collect();
    out.push_str("\n… (cut here; the rest is in the sender's conversation)");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(to: AgentId, from: AgentId, message_id: MessageId) -> EventKind {
        EventKind::AgentInput {
            id: to,
            message_id,
            text: "Message from a:\nhi".into(),
            queued: false,
            from: Some(MessageSource::Agent { agent_id: from }),
            reply_to: None,
            thread: None,
            reply_excerpt: None,
            attachments: Vec::new(),
        }
    }

    fn said(id: AgentId, text: &str) -> EventKind {
        EventKind::AgentOutput {
            id,
            stream: OutputStream::Text,
            text: text.into(),
        }
    }

    fn done(id: AgentId, message_id: MessageId, stop_reason: StopReason) -> EventKind {
        EventKind::AgentTurnCompleted {
            id,
            message_id,
            stop_reason,
        }
    }

    /// `b` gets a message from `a` and starts on it.
    fn asked(t: &mut ReplyTracker, a: AgentId, b: AgentId, now: Instant) -> MessageId {
        let m = MessageId::new();
        assert_eq!(t.on_event(&input(b, a, m), now), None);
        t.on_event(
            &EventKind::AgentTurnStarted {
                id: b,
                message_id: m,
            },
            now,
        );
        m
    }

    #[test]
    fn an_answer_only_written_goes_back_to_who_asked() {
        let (a, b) = (AgentId::new(), AgentId::new());
        let mut t = ReplyTracker::default();
        let now = Instant::now();
        let m = asked(&mut t, a, b, now);
        t.on_event(&said(b, "looking…"), now);
        t.on_event(
            &EventKind::AgentToolCall {
                id: b,
                tool_call_id: "1".into(),
                title: "ls".into(),
                kind: None,
                status: crossagents_core::ToolStatus::InProgress,
            },
            now,
        );
        t.on_event(&said(b, "Done: "), now);
        t.on_event(&said(b, "all tests pass."), now);
        let forward = t.on_event(&done(b, m, StopReason::EndTurn), now);
        assert_eq!(
            forward,
            Some(Forward {
                from: b,
                to: a,
                answers: m,
                text: "Done: all tests pass.".into(),
            }),
            "only what it said after its last tool call"
        );
    }

    #[test]
    fn an_answer_sent_back_is_not_forwarded_again() {
        let (a, b) = (AgentId::new(), AgentId::new());
        let mut t = ReplyTracker::default();
        let now = Instant::now();
        let m = asked(&mut t, a, b, now);
        t.note_send(b, a, now + Duration::from_secs(1));
        t.on_event(&said(b, "sent you the details"), now);
        assert_eq!(t.on_event(&done(b, m, StopReason::EndTurn), now), None);
    }

    #[test]
    fn a_message_to_someone_else_does_not_count_as_the_answer() {
        let (a, b, other) = (AgentId::new(), AgentId::new(), AgentId::new());
        let mut t = ReplyTracker::default();
        let now = Instant::now();
        let m = asked(&mut t, a, b, now);
        // The namesake mistake: the answer went to another agent.
        t.note_send(b, other, now + Duration::from_secs(1));
        // And an older send to `a`, from before this turn, is not an answer either.
        t.note_send(b, a, now - Duration::from_secs(5));
        t.on_event(&said(b, "here it is"), now);
        assert!(t.on_event(&done(b, m, StopReason::EndTurn), now).is_some());
    }

    #[test]
    fn the_answer_to_a_forwarded_answer_stays_put() {
        let (a, b) = (AgentId::new(), AgentId::new());
        let mut t = ReplyTracker::default();
        let now = Instant::now();
        let forwarded = MessageId::new();
        t.mark_forwarded(forwarded);
        t.on_event(&input(a, b, forwarded), now);
        t.on_event(
            &EventKind::AgentTurnStarted {
                id: a,
                message_id: forwarded,
            },
            now,
        );
        t.on_event(&said(a, "thanks"), now);
        assert_eq!(
            t.on_event(&done(a, forwarded, StopReason::EndTurn), now),
            None
        );
    }

    #[test]
    fn a_failed_or_silent_turn_forwards_nothing() {
        let (a, b) = (AgentId::new(), AgentId::new());
        let mut t = ReplyTracker::default();
        let now = Instant::now();
        let m = asked(&mut t, a, b, now);
        t.on_event(&said(b, "half an answer"), now);
        assert_eq!(t.on_event(&done(b, m, StopReason::Interrupted), now), None);
        let m = asked(&mut t, a, b, now);
        t.on_event(&said(b, "  \n"), now);
        assert_eq!(t.on_event(&done(b, m, StopReason::EndTurn), now), None);
    }

    #[test]
    fn a_person_s_message_is_not_a_teammate_s() {
        let b = AgentId::new();
        let mut t = ReplyTracker::default();
        let now = Instant::now();
        let m = MessageId::new();
        t.on_event(
            &EventKind::AgentInput {
                id: b,
                message_id: m,
                text: "hi".into(),
                queued: false,
                from: Some(MessageSource::Human),
                reply_to: None,
                thread: None,
                reply_excerpt: None,
                attachments: Vec::new(),
            },
            now,
        );
        t.on_event(
            &EventKind::AgentTurnStarted {
                id: b,
                message_id: m,
            },
            now,
        );
        t.on_event(&said(b, "hello"), now);
        assert_eq!(t.on_event(&done(b, m, StopReason::EndTurn), now), None);
    }

    #[test]
    fn a_long_answer_is_cut() {
        let long = "x".repeat(MAX_FORWARD + 10);
        let out = clip(&long);
        assert!(out.starts_with(&"x".repeat(MAX_FORWARD)));
        assert!(out.ends_with("conversation)"));
    }
}
