//! A session's conversation, rebuilt from its events (`GET /v1/agents/{key}/transcript`).

use crossagents_api_types::{TranscriptItem, TranscriptTurn, TranscriptUsage};
use crossagents_core::{Event, EventKind, MessageId};

/// Folds an agent's events, oldest first, into turns.
#[derive(Debug, Default)]
pub struct Fold {
    pub turns: Vec<TranscriptTurn>,
    pub usage: Option<TranscriptUsage>,
    pub last_seq: u64,
    /// The turn the agent is working on.
    current: Option<MessageId>,
}

impl Fold {
    pub fn push(&mut self, event: &Event) {
        self.last_seq = self.last_seq.max(event.seq);
        match &event.kind {
            EventKind::AgentInput {
                message_id,
                text,
                from,
                ..
            } => self.turns.push(TranscriptTurn {
                message_id: *message_id,
                input: text.clone(),
                from: *from,
                sent_at: event.ts,
                started_at: None,
                completed_at: None,
                stop_reason: None,
                items: Vec::new(),
            }),
            EventKind::AgentInputWithdrawn { message_id, .. } => {
                self.turns
                    .retain(|t| t.message_id != *message_id || t.started_at.is_some());
            }
            EventKind::AgentTurnStarted { message_id, .. } => {
                self.current = Some(*message_id);
                if let Some(turn) = self.turn_mut(*message_id) {
                    turn.started_at = Some(event.ts);
                }
            }
            EventKind::AgentTurnCompleted {
                message_id,
                stop_reason,
                ..
            } => {
                if self.current == Some(*message_id) {
                    self.current = None;
                }
                if let Some(turn) = self.turn_mut(*message_id) {
                    turn.completed_at = Some(event.ts);
                    turn.stop_reason = Some(*stop_reason);
                }
            }
            EventKind::AgentOutput { stream, text, .. } => {
                if let Some(items) = self.active_items() {
                    match items.last_mut() {
                        Some(TranscriptItem::Output {
                            stream: last,
                            text: buf,
                        }) if last == stream => buf.push_str(text),
                        _ => items.push(TranscriptItem::Output {
                            stream: *stream,
                            text: text.clone(),
                        }),
                    }
                }
            }
            EventKind::AgentToolCall {
                tool_call_id,
                title,
                kind,
                status,
                ..
            } => {
                if let Some(items) = self.active_items() {
                    items.push(TranscriptItem::ToolCall {
                        tool_call_id: tool_call_id.clone(),
                        title: title.clone(),
                        kind: kind.clone(),
                        status: *status,
                    });
                }
            }
            EventKind::AgentToolCallUpdated {
                tool_call_id,
                title: new_title,
                status: new_status,
                ..
            } => {
                // Updates usually land in the current turn; search back from the newest.
                let call = self.turns.iter_mut().rev().flat_map(|t| t.items.iter_mut()).find(
                    |item| matches!(item, TranscriptItem::ToolCall { tool_call_id: id, .. } if id == tool_call_id),
                );
                if let Some(TranscriptItem::ToolCall { title, status, .. }) = call {
                    if let Some(new_title) = new_title {
                        title.clone_from(new_title);
                    }
                    if let Some(new_status) = new_status {
                        *status = *new_status;
                    }
                }
            }
            EventKind::AgentError { message, .. } => {
                if let Some(items) = self.active_items() {
                    items.push(TranscriptItem::Error {
                        message: message.clone(),
                    });
                }
            }
            EventKind::AgentUsage {
                used,
                size,
                cost_usd,
                ..
            } => {
                self.usage = Some(TranscriptUsage {
                    used: *used,
                    size: *size,
                    cost_usd: *cost_usd,
                });
            }
            _ => {}
        }
    }

    fn turn_mut(&mut self, id: MessageId) -> Option<&mut TranscriptTurn> {
        self.turns.iter_mut().rev().find(|t| t.message_id == id)
    }

    /// Where output goes: the running turn, else the last turn that started.
    fn active_items(&mut self) -> Option<&mut Vec<TranscriptItem>> {
        let index = match self.current {
            Some(id) => self.turns.iter().rposition(|t| t.message_id == id),
            None => self.turns.iter().rposition(|t| t.started_at.is_some()),
        }?;
        Some(&mut self.turns[index].items)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use crossagents_core::{
        AgentId, EVENT_SCHEMA_VERSION, MessageSource, OutputStream, StopReason, ToolStatus,
    };
    use uuid::Uuid;

    use super::*;

    fn ev(seq: u64, kind: EventKind) -> Event {
        Event {
            seq,
            id: Uuid::now_v7(),
            ts: Utc::now(),
            schema: EVENT_SCHEMA_VERSION,
            kind,
        }
    }

    #[test]
    fn groups_output_and_tools_by_turn() {
        let id = AgentId::new();
        let (m1, m2, m3) = (MessageId::new(), MessageId::new(), MessageId::new());
        let out = |text: &str, stream| EventKind::AgentOutput {
            id,
            stream,
            text: text.into(),
        };
        let kinds = vec![
            EventKind::AgentInput {
                id,
                message_id: m1,
                text: "hi".into(),
                queued: false,
                from: Some(MessageSource::Human),
            },
            EventKind::AgentTurnStarted { id, message_id: m1 },
            out("thinking", OutputStream::Thought),
            out("hel", OutputStream::Text),
            out("lo", OutputStream::Text),
            EventKind::AgentToolCall {
                id,
                tool_call_id: "t1".into(),
                title: "Read".into(),
                kind: Some("read".into()),
                status: ToolStatus::InProgress,
            },
            // A message queued while the first turn runs.
            EventKind::AgentInput {
                id,
                message_id: m2,
                text: "next".into(),
                queued: true,
                from: None,
            },
            // One taken back before it started.
            EventKind::AgentInput {
                id,
                message_id: m3,
                text: "never mind".into(),
                queued: true,
                from: Some(MessageSource::Human),
            },
            EventKind::AgentInputWithdrawn { id, message_id: m3 },
            EventKind::AgentToolCallUpdated {
                id,
                tool_call_id: "t1".into(),
                title: Some("Read a.rs".into()),
                status: Some(ToolStatus::Completed),
            },
            out("done", OutputStream::Text),
            EventKind::AgentUsage {
                id,
                used: 10,
                size: 100,
                cost_usd: None,
            },
            EventKind::AgentTurnCompleted {
                id,
                message_id: m1,
                stop_reason: StopReason::EndTurn,
            },
        ];
        let mut fold = Fold::default();
        for (i, kind) in kinds.into_iter().enumerate() {
            fold.push(&ev(i as u64 + 1, kind));
        }

        assert_eq!(fold.last_seq, 13);
        assert_eq!(fold.turns.len(), 2);
        let first = &fold.turns[0];
        assert_eq!(first.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(first.from, Some(MessageSource::Human));
        assert_eq!(
            first.items,
            vec![
                TranscriptItem::Output {
                    stream: OutputStream::Thought,
                    text: "thinking".into()
                },
                TranscriptItem::Output {
                    stream: OutputStream::Text,
                    text: "hello".into()
                },
                TranscriptItem::ToolCall {
                    tool_call_id: "t1".into(),
                    title: "Read a.rs".into(),
                    kind: Some("read".into()),
                    status: ToolStatus::Completed,
                },
                TranscriptItem::Output {
                    stream: OutputStream::Text,
                    text: "done".into()
                },
            ]
        );
        let second = &fold.turns[1];
        assert_eq!(second.input, "next");
        assert!(second.started_at.is_none() && second.items.is_empty());
        assert_eq!(fold.usage.as_ref().map(|u| u.used), Some(10));
    }
}
