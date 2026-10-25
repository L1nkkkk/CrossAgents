//! A session's conversation, rebuilt from its events (`GET /v1/agents/{key}/transcript`).

use std::collections::HashMap;

use crossagents_api_types::{
    AgentExit, ModelSwitch, TranscriptItem, TranscriptTurn, TranscriptUsage, TurnDiffStat,
};
use crossagents_core::{Event, EventKind, MessageId};

/// Folds an agent's events, oldest first, into turns.
#[derive(Debug, Default)]
pub struct Fold {
    pub turns: Vec<TranscriptTurn>,
    pub usage: Option<TranscriptUsage>,
    pub switches: Vec<ModelSwitch>,
    pub last_seq: u64,
    /// Each turn's latest unified diff; the transcript carries only its stat.
    pub diffs: HashMap<MessageId, String>,
    /// The latest `agent.error`, inside a turn or not.
    pub last_error: Option<String>,
    /// How its process ended, while `last_error` is that it did.
    pub last_exit: Option<AgentExit>,
    /// `agent.exited` was just folded; the `agent.error` the runtime sends after it says the
    /// same thing.
    exit_pending: bool,
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
                reply_to,
                thread,
                reply_excerpt,
                attachments,
                ..
            } => self.turns.push(TranscriptTurn {
                message_id: *message_id,
                input: text.clone(),
                from: *from,
                reply_to: *reply_to,
                thread: *thread,
                reply_excerpt: reply_excerpt.clone(),
                sent_at: event.ts,
                started_at: None,
                completed_at: None,
                stop_reason: None,
                items: Vec::new(),
                diff: None,
                attachments: attachments.clone(),
            }),
            EventKind::AgentInputUpdated {
                message_id, text, ..
            } => {
                if let Some(turn) = self.turn_mut(*message_id)
                    && turn.started_at.is_none()
                {
                    turn.input.clone_from(text);
                }
            }
            EventKind::AgentModelChanged {
                model,
                effort,
                from_model,
                from_effort,
                ..
            } => self.switches.push(ModelSwitch {
                at: event.ts,
                model: model.clone(),
                effort: effort.clone(),
                from_model: from_model.clone(),
                from_effort: from_effort.clone(),
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
                        output: None,
                        detail: None,
                    });
                }
            }
            EventKind::AgentToolOutput {
                tool_call_id, text, ..
            } => {
                if let Some(TranscriptItem::ToolCall { output, .. }) = self.call_mut(tool_call_id) {
                    output.get_or_insert_with(String::new).push_str(text);
                }
            }
            EventKind::AgentToolDetail {
                tool_call_id,
                detail: new,
                ..
            } => {
                if let Some(TranscriptItem::ToolCall { detail, .. }) = self.call_mut(tool_call_id) {
                    *detail = Some(Box::new(new.clone()));
                }
            }
            EventKind::AgentTurnDiff {
                diff,
                message_id,
                source,
                ..
            } => {
                let index = match (*message_id, self.current) {
                    (Some(id), _) | (None, Some(id)) => {
                        self.turns.iter().rposition(|t| t.message_id == id)
                    }
                    (None, None) => self.turns.iter().rposition(|t| t.started_at.is_some()),
                };
                if let Some(index) = index {
                    let turn = &mut self.turns[index];
                    let stat = TurnDiffStat::from_diff(diff, source.unwrap_or_default());
                    turn.diff = (!stat.files.is_empty()).then_some(stat);
                    self.diffs.insert(turn.message_id, diff.clone());
                }
            }
            EventKind::AgentPermission {
                kind,
                title,
                decision,
                ..
            } => {
                if let Some(items) = self.active_items() {
                    items.push(TranscriptItem::Permission {
                        kind: kind.clone(),
                        title: title.clone(),
                        decision: *decision,
                    });
                }
            }
            EventKind::AgentWarning { message, .. } => {
                if let Some(items) = self.active_items() {
                    items.push(TranscriptItem::Warning {
                        message: message.clone(),
                    });
                }
            }
            EventKind::AgentAdopted {
                message_id: Some(_),
                ..
            } => {
                // Same words as the GUI's live view (apps/gui/src/session.ts).
                if let Some(items) = self.active_items() {
                    items.push(TranscriptItem::Warning {
                        message: "hub 已重启，这一轮由新的 hub 接管，继续运行".into(),
                    });
                }
            }
            EventKind::AgentToolCallUpdated {
                tool_call_id,
                title: new_title,
                status: new_status,
                ..
            } => {
                if let Some(TranscriptItem::ToolCall { title, status, .. }) =
                    self.call_mut(tool_call_id)
                {
                    if let Some(new_title) = new_title {
                        title.clone_from(new_title);
                    }
                    if let Some(new_status) = new_status {
                        *status = *new_status;
                    }
                }
            }
            EventKind::AgentExited { cause, code, .. } => {
                self.last_exit = Some(AgentExit {
                    cause: *cause,
                    code: *code,
                });
                self.exit_pending = true;
            }
            EventKind::AgentError { message, .. } => {
                self.last_error = Some(message.clone());
                if !std::mem::take(&mut self.exit_pending) {
                    self.last_exit = None;
                }
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
                tokens,
                ..
            } => {
                // The breakdown arrives after the turn; keep the last one until then.
                let parts = self.usage.as_mut().and_then(|u| u.parts.take());
                self.usage = Some(TranscriptUsage {
                    used: *used,
                    size: *size,
                    cost_usd: *cost_usd,
                    tokens: tokens.clone(),
                    parts,
                });
            }
            EventKind::AgentContext { parts, .. } => {
                if let Some(usage) = self.usage.as_mut() {
                    usage.parts = Some(parts.clone());
                }
            }
            _ => {}
        }
    }

    /// A tool call by id. Updates usually land in the current turn, so search back from the
    /// newest.
    fn call_mut(&mut self, tool_call_id: &str) -> Option<&mut TranscriptItem> {
        self.turns
            .iter_mut()
            .rev()
            .flat_map(|t| t.items.iter_mut())
            .find(|item| matches!(item, TranscriptItem::ToolCall { tool_call_id: id, .. } if id == tool_call_id))
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
        AgentId, EVENT_SCHEMA_VERSION, MessageSource, OutputStream, StopReason, ToolDetail,
        ToolStatus,
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
                reply_to: None,
                thread: None,
                reply_excerpt: None,
                attachments: Vec::new(),
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
                reply_to: None,
                thread: None,
                reply_excerpt: None,
                attachments: Vec::new(),
            },
            // One taken back before it started.
            EventKind::AgentInput {
                id,
                message_id: m3,
                text: "never mind".into(),
                queued: true,
                from: Some(MessageSource::Human),
                reply_to: None,
                thread: None,
                reply_excerpt: None,
                attachments: Vec::new(),
            },
            EventKind::AgentInputWithdrawn { id, message_id: m3 },
            EventKind::AgentToolCallUpdated {
                id,
                tool_call_id: "t1".into(),
                title: Some("Read a.rs".into()),
                status: Some(ToolStatus::Completed),
            },
            EventKind::AgentToolOutput {
                id,
                tool_call_id: "t1".into(),
                text: "fn main".into(),
            },
            EventKind::AgentToolDetail {
                id,
                tool_call_id: "t1".into(),
                detail: ToolDetail {
                    exit_code: Some(0),
                    ..ToolDetail::default()
                },
            },
            EventKind::AgentWarning {
                id,
                message: "retrying".into(),
            },
            EventKind::AgentTurnDiff {
                id,
                diff: "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n"
                    .into(),
                message_id: None,
                source: None,
            },
            out("done", OutputStream::Text),
            EventKind::AgentUsage {
                id,
                used: 10,
                size: 100,
                cost_usd: None,
                tokens: None,
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

        assert_eq!(fold.last_seq, 17);
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
                    output: Some("fn main".into()),
                    detail: Some(Box::new(ToolDetail {
                        exit_code: Some(0),
                        ..ToolDetail::default()
                    })),
                },
                TranscriptItem::Warning {
                    message: "retrying".into()
                },
                TranscriptItem::Output {
                    stream: OutputStream::Text,
                    text: "done".into()
                },
            ]
        );
        let stat = first.diff.as_ref().expect("turn diff");
        assert_eq!(stat.files.len(), 1);
        assert_eq!(stat.files[0].path, "a.rs");
        assert_eq!((stat.added, stat.removed), (1, 1));
        assert_eq!(stat.source, crossagents_core::DiffSource::Driver);
        assert!(fold.diffs[&m1].contains("+y"));
        let second = &fold.turns[1];
        assert_eq!(second.input, "next");
        assert!(second.started_at.is_none() && second.items.is_empty());
        assert_eq!(fold.usage.as_ref().map(|u| u.used), Some(10));
    }

    #[test]
    fn keeps_how_the_process_ended_until_another_error() {
        let id = AgentId::new();
        let error = |message: &str| EventKind::AgentError {
            id,
            message: message.into(),
        };
        let mut fold = Fold::default();
        fold.push(&ev(
            1,
            EventKind::AgentExited {
                id,
                cause: Some(crossagents_core::ExitCause::Crashed),
                code: Some(3),
                message: "claude crashed (exit code 3)".into(),
            },
        ));
        fold.push(&ev(2, error("claude crashed (exit code 3)")));
        let exit = fold.last_exit.clone().unwrap();
        assert_eq!(exit.cause, Some(crossagents_core::ExitCause::Crashed));
        assert_eq!(exit.code, Some(3));

        // A later error is about something else: the exit no longer explains it.
        fold.push(&ev(3, error("not logged in")));
        assert!(fold.last_exit.is_none());
    }
}
