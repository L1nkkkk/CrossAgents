//! Commands agents have started and not finished, followed from their tool-call events, for
//! display: what is going on in a checkout right now.
//!
//! This is not a lock. The hub's checks and `workspace_run` hold the checkout's resources
//! through the executor and wait for whoever holds them (docs/tasks/LOCKS.md §6), and an
//! event cannot prove that a command bypassed the executor — so what is recorded here never
//! decides whether a check may start. Kept in memory only; after a restart nothing is known
//! to be running.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use crossagents_core::{AgentId, Event, EventKind, OutputStream, ToolStatus};

/// Activity for current turns only. Kept in memory; after a restart nothing is known.
#[derive(Debug, Default)]
pub struct AgentActivity {
    by_agent: HashMap<AgentId, TurnActivity>,
}

/// Two hours without any activity suggests an end event that never arrived, while a turn that
/// is long but alive keeps its activity: that is why this counts from the last event and not
/// from the turn's start.
const ACTIVITY_STALE_AFTER_HOURS: i64 = 2;

#[derive(Debug)]
struct TurnActivity {
    started: DateTime<Utc>,
    last: DateTime<Utc>,
    kind: &'static str,
    files: HashSet<String>,
    calls: HashMap<String, &'static str>,
}

impl TurnActivity {
    fn new(ts: DateTime<Utc>) -> Self {
        Self {
            started: ts,
            last: ts,
            kind: "thinking",
            files: HashSet::new(),
            calls: HashMap::new(),
        }
    }
}

impl AgentActivity {
    pub fn push(&mut self, event: &Event) {
        match &event.kind {
            EventKind::AgentTurnCompleted { id, .. } | EventKind::AgentExited { id, .. } => {
                self.by_agent.remove(id);
            }
            EventKind::AgentTurnStarted { id, .. } => {
                self.by_agent.retain(|_, turn| {
                    event.ts - turn.last < Duration::hours(ACTIVITY_STALE_AFTER_HOURS)
                });
                self.by_agent.insert(*id, TurnActivity::new(event.ts));
            }
            EventKind::AgentOutput { id, stream, .. } => {
                let turn = self
                    .by_agent
                    .entry(*id)
                    .or_insert_with(|| TurnActivity::new(event.ts));
                turn.last = event.ts;
                turn.kind = match stream {
                    OutputStream::Thought => "thinking",
                    OutputStream::Text => "replying",
                };
            }
            EventKind::AgentToolCall {
                id,
                tool_call_id,
                kind,
                ..
            } => {
                let turn = self
                    .by_agent
                    .entry(*id)
                    .or_insert_with(|| TurnActivity::new(event.ts));
                let kind = match kind.as_deref() {
                    Some("execute") => "run_command",
                    Some("edit" | "delete" | "move") => "edit_files",
                    Some("read") => "read_files",
                    Some("search" | "fetch") => "search",
                    Some("think") => "thinking",
                    _ => "tool_call",
                };
                turn.calls.insert(tool_call_id.clone(), kind);
                turn.last = event.ts;
                turn.kind = kind;
            }
            EventKind::AgentToolDetail {
                id,
                tool_call_id,
                detail,
            } => {
                if let Some(turn) = self.by_agent.get_mut(id)
                    && let Some(kind) = turn.calls.get(tool_call_id).copied()
                {
                    turn.last = event.ts;
                    turn.kind = kind;
                    if kind == "edit_files" {
                        turn.files
                            .extend(detail.changes.iter().map(|change| change.path.clone()));
                    }
                }
            }
            EventKind::AgentToolOutput {
                id, tool_call_id, ..
            }
            | EventKind::AgentToolCallUpdated {
                id, tool_call_id, ..
            } => {
                if let Some(turn) = self.by_agent.get_mut(id)
                    && let Some(kind) = turn.calls.get(tool_call_id).copied()
                {
                    turn.last = event.ts;
                    turn.kind = kind;
                }
            }
            _ => {}
        }
    }

    pub fn of(&self, agent: AgentId, now: DateTime<Utc>) -> Option<serde_json::Value> {
        self.by_agent
            .get(&agent)
            .filter(|turn| now - turn.last < Duration::hours(ACTIVITY_STALE_AFTER_HOURS))
            .map(|turn| {
                serde_json::json!({
                    "turn_elapsed_seconds": (now - turn.started).num_seconds().max(0),
                    "last_activity_kind": turn.kind,
                    "last_activity_ago_seconds": (now - turn.last).num_seconds().max(0),
                    "files_touched": turn.files.len(),
                })
            })
    }
}

/// A command still going.
#[derive(Debug, Clone, PartialEq)]
pub struct Running {
    pub tool_call_id: String,
    pub command: String,
    pub since: DateTime<Utc>,
}

/// Running commands by agent.
#[derive(Debug, Default)]
pub struct RunningCommands {
    by_agent: HashMap<AgentId, Vec<Running>>,
    pub activity: AgentActivity,
}

/// Past it a command whose end was never reported is taken as gone: a command put in the
/// background reports its end long before the command does.
const STALE_AFTER_HOURS: i64 = 2;

impl RunningCommands {
    pub fn push(&mut self, event: &Event) {
        self.activity.push(event);
        match &event.kind {
            EventKind::AgentToolCall {
                id,
                tool_call_id,
                title,
                kind,
                status,
            } if kind.as_deref() == Some("execute") => {
                let commands = self.by_agent.entry(*id).or_default();
                commands.retain(|c| &c.tool_call_id != tool_call_id);
                if matches!(status, ToolStatus::Pending | ToolStatus::InProgress) {
                    commands.push(Running {
                        tool_call_id: tool_call_id.clone(),
                        command: title.clone(),
                        since: event.ts,
                    });
                }
            }
            EventKind::AgentToolCallUpdated {
                id,
                tool_call_id,
                title,
                status,
            } => {
                if matches!(status, Some(ToolStatus::Completed | ToolStatus::Failed)) {
                    self.end(*id, tool_call_id);
                } else if let Some(title) = title {
                    self.rename(*id, tool_call_id, title);
                }
            }
            EventKind::AgentToolDetail {
                id,
                tool_call_id,
                detail,
            } => {
                if detail.exit_code.is_some() {
                    self.end(*id, tool_call_id);
                } else if let Some(command) = &detail.command {
                    self.rename(*id, tool_call_id, command);
                }
            }
            // A turn's end, or its process's, ends what it ran.
            EventKind::AgentTurnCompleted { id, .. } | EventKind::AgentExited { id, .. } => {
                self.by_agent.remove(id);
            }
            _ => {}
        }
    }

    fn end(&mut self, agent: AgentId, tool_call_id: &str) {
        if let Some(commands) = self.by_agent.get_mut(&agent) {
            commands.retain(|c| c.tool_call_id != tool_call_id);
        }
    }

    fn rename(&mut self, agent: AgentId, tool_call_id: &str, command: &str) {
        if let Some(c) = self
            .by_agent
            .get_mut(&agent)
            .and_then(|cs| cs.iter_mut().find(|c| c.tool_call_id == tool_call_id))
        {
            c.command = command.to_owned();
        }
    }

    /// What `agent` is running now, for display (LOCKS.md §6: the hub no longer refuses its
    /// checks on this record; waiting for the executor's locks is what keeps two builds
    /// apart).
    pub fn of(&self, agent: AgentId, now: DateTime<Utc>) -> Vec<Running> {
        self.by_agent
            .get(&agent)
            .map(|cs| {
                cs.iter()
                    .filter(|c| now - c.since < Duration::hours(STALE_AFTER_HOURS))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Whether a failed check's output has the system saying a file was in use by another
/// process, rather than the code being wrong: what a build running at the same time causes.
/// Only what the operating system says, in any language's tools (docs/tasks/TOOLCHAIN.md §6).
pub fn in_use_failure(output: &str) -> bool {
    const SIGNS: [&str; 8] = [
        // Windows: ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION.
        "os error 32",
        "os error 33",
        "being used by another process",
        "另一个程序正在使用此文件",
        // Unix: ETXTBSY, EBUSY.
        "Text file busy",
        "Device or resource busy",
        "EBUSY",
        "ETXTBSY",
    ];
    SIGNS.iter().any(|sign| output.contains(sign))
}

/// The hints of `presets` that a failed check's output has: signs it may come from another
/// build at the same time, which never decide it.
pub fn hints<'a>(output: &str, presets: &[&'a crate::presets::Preset]) -> Vec<&'a str> {
    presets
        .iter()
        .flat_map(|p| p.hints.iter().copied())
        .filter(|hint| output.contains(hint))
        .collect()
}

#[cfg(test)]
mod tests {
    use crossagents_core::{EVENT_SCHEMA_VERSION, EventKind, ToolDetail};

    use super::*;

    fn event(kind: EventKind) -> Event {
        Event {
            seq: 0,
            id: uuid::Uuid::new_v4(),
            ts: Utc::now(),
            schema: EVENT_SCHEMA_VERSION,
            kind,
        }
    }

    #[test]
    fn activity_expires_from_last_event_and_is_pruned_on_turn_start() {
        let agent = AgentId::new();
        let other = AgentId::new();
        let now = Utc::now();
        let mut activity = AgentActivity::default();
        let mut start = event(EventKind::AgentOutput {
            id: agent,
            stream: OutputStream::Thought,
            text: "work".into(),
        });
        start.ts = now - Duration::hours(3);
        activity.push(&start);
        let mut recent = event(EventKind::AgentOutput {
            id: agent,
            stream: OutputStream::Text,
            text: "still working".into(),
        });
        recent.ts = now - Duration::minutes(90);
        activity.push(&recent);
        assert_eq!(
            activity.of(agent, now).unwrap()["last_activity_kind"],
            "replying"
        );
        assert!(activity.of(agent, now + Duration::minutes(30)).is_none());
        let mut next = event(EventKind::AgentOutput {
            id: other,
            stream: OutputStream::Thought,
            text: "new".into(),
        });
        next.ts = now + Duration::minutes(30);
        activity.push(&next);
        assert!(activity.by_agent.contains_key(&agent));
        assert!(activity.of(other, next.ts).is_some());
        let mut new_turn = event(EventKind::AgentTurnStarted {
            id: other,
            message_id: crossagents_core::MessageId::new(),
        });
        new_turn.ts = next.ts;
        activity.push(&new_turn);
        assert!(!activity.by_agent.contains_key(&agent));
    }

    #[test]
    fn long_silent_tool_preserves_turn_and_file_count() {
        let agent = AgentId::new();
        let now = Utc::now();
        let mut activity = AgentActivity::default();
        let mut call = event(EventKind::AgentToolCall {
            id: agent,
            tool_call_id: "edit".into(),
            title: "edit".into(),
            kind: Some("edit".into()),
            status: ToolStatus::InProgress,
        });
        call.ts = now - Duration::hours(3);
        activity.push(&call);
        let mut update = event(EventKind::AgentToolCallUpdated {
            id: agent,
            tool_call_id: "edit".into(),
            title: None,
            status: Some(ToolStatus::InProgress),
        });
        update.ts = now;
        activity.push(&update);
        let state = activity.of(agent, now).unwrap();
        assert_eq!(state["turn_elapsed_seconds"], 10800);
        assert_eq!(state["last_activity_kind"], "edit_files");
    }

    #[test]
    fn a_command_runs_from_its_call_to_its_exit() {
        let agent = AgentId::new();
        let mut running = RunningCommands::default();
        running.push(&event(EventKind::AgentToolCall {
            id: agent,
            tool_call_id: "c1".into(),
            title: "cargo".into(),
            kind: Some("execute".into()),
            status: ToolStatus::InProgress,
        }));
        // Reads and edits are not commands.
        running.push(&event(EventKind::AgentToolCall {
            id: agent,
            tool_call_id: "r1".into(),
            title: "read".into(),
            kind: Some("read".into()),
            status: ToolStatus::InProgress,
        }));
        running.push(&event(EventKind::AgentToolDetail {
            id: agent,
            tool_call_id: "c1".into(),
            detail: ToolDetail {
                command: Some("cargo test --workspace".into()),
                ..ToolDetail::default()
            },
        }));
        let now = Utc::now();
        let seen = running.of(agent, now);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].command, "cargo test --workspace");
        // Long gone without an end: no longer holds anything up.
        assert!(running.of(agent, now + Duration::hours(3)).is_empty());
        running.push(&event(EventKind::AgentToolDetail {
            id: agent,
            tool_call_id: "c1".into(),
            detail: ToolDetail {
                exit_code: Some(0),
                ..ToolDetail::default()
            },
        }));
        assert!(running.of(agent, now).is_empty());
        // The end of the turn ends whatever was still listed.
        running.push(&event(EventKind::AgentToolCall {
            id: agent,
            tool_call_id: "c2".into(),
            title: "npm test".into(),
            kind: Some("execute".into()),
            status: ToolStatus::Pending,
        }));
        assert_eq!(running.of(agent, now).len(), 1);
        running.push(&event(EventKind::AgentToolCallUpdated {
            id: agent,
            tool_call_id: "c2".into(),
            title: None,
            status: Some(ToolStatus::Failed),
        }));
        assert!(running.of(agent, now).is_empty());
    }

    #[test]
    fn a_file_in_use_is_told_apart_from_a_failing_test() {
        assert!(in_use_failure(
            "error: failed to remove file: 另一个程序正在使用此文件，进程无法访问。 (os error 32)"
        ));
        assert!(in_use_failure("cannot execute: Text file busy"));
        assert!(!in_use_failure(
            "test pipeline::lands ... FAILED\nassertion `left == right` failed"
        ));
        // The linker could not open a file: maybe one in use, maybe not. A hint only.
        let lnk = "LINK : fatal error LNK1104: 无法打开文件“…\\adopt-4ea7e29f8329f9ed.exe”";
        assert!(!in_use_failure(lnk));
        let rust = crate::presets::get("rust").unwrap();
        assert_eq!(hints(lnk, &[rust]), ["LNK1104"]);
        assert!(hints(lnk, &[]).is_empty());
    }
}
