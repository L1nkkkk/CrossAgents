//! Handoff cards (docs/research/magpie-lancer.md §2.1): each time a task changes hands, what
//! went with it — who gave it to whom and why, the files already changed, the last word of
//! the one before, the decisions to keep and the questions still open. Facts the hub has;
//! it writes no summary of its own.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, HandoffId, RunId, TaskId};

/// Why the task changed hands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum HandoffKind {
    /// Its first worker.
    Assign,
    /// Its issuer gave it to another teammate (`task_assign`).
    Reassign,
    /// Quota: the worker's account ran out and the task went to another account or teammate.
    Reroute,
    /// It moved to the run's stronger escalation teammate.
    Escalate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Handoff {
    pub id: HandoffId,
    pub run_id: RunId,
    pub task_id: TaskId,
    pub task_key: String,
    pub kind: HandoffKind,
    /// The issuer on a first assignment; the worker before otherwise. `None` when there was
    /// none (a task whose worker had already gone).
    pub from: Option<AgentId>,
    /// Who `from` is to the person: the issuer, or the teammate the worker was cloned from
    /// (worker clones are removed with their task, their names mean nothing later).
    #[serde(default)]
    pub from_name: Option<String>,
    /// The worker that has the task now.
    pub to: AgentId,
    /// The teammate `to` was cloned from.
    #[serde(default)]
    pub to_name: String,
    /// Why it changed hands; always given for `reroute` and `escalate`.
    #[serde(default)]
    pub reason: Option<String>,
    /// The task's goal, from its spec.
    pub goal: String,
    /// On a first assignment, the files the spec allows; otherwise the files the worker
    /// before changed in the worktree (committed or not). Empty when that could not be read.
    #[serde(default)]
    pub files: Vec<String>,
    /// The worker before's summary or last progress line.
    #[serde(default)]
    pub summary: Option<String>,
    /// The spec's interfaces, then the issuer's rework notes.
    #[serde(default)]
    pub decisions: Vec<String>,
    /// Questions the worker before asked (`task_ask`) and nobody answered.
    #[serde(default)]
    pub open_questions: Vec<String>,
    /// Context tokens the worker before had in use, as its agent last reported; `None` when
    /// it reported none.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub context_tokens: Option<u64>,
    pub at: DateTime<Utc>,
}

impl Handoff {
    /// `Pia → Rex`, with the reason in brackets when there is one.
    pub fn link(&self) -> String {
        let to = if self.to_name.is_empty() {
            "?"
        } else {
            &self.to_name
        };
        let kind = match self.kind {
            HandoffKind::Assign => "assign",
            HandoffKind::Reassign => "reassign",
            HandoffKind::Reroute => "reroute",
            HandoffKind::Escalate => "escalate",
        };
        match &self.reason {
            Some(reason) => format!("{to} ({kind}: {reason})"),
            None => format!("{to} ({kind})"),
        }
    }
}

/// The task's handoffs as one line: `Pia → Rex (assign) → Ada (reroute: …)`.
pub fn chain(handoffs: &[Handoff]) -> String {
    let Some(first) = handoffs.first() else {
        return String::new();
    };
    let mut out = first.from_name.clone().unwrap_or_else(|| "?".into());
    for h in handoffs {
        out.push_str(" → ");
        out.push_str(&h.link());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(kind: HandoffKind, to: &str, reason: Option<&str>) -> Handoff {
        Handoff {
            id: HandoffId::new(),
            run_id: RunId::new(),
            task_id: TaskId::new(),
            task_key: "a".into(),
            kind,
            from: None,
            from_name: Some("Pia".into()),
            to: AgentId::new(),
            to_name: to.into(),
            reason: reason.map(str::to_owned),
            goal: "g".into(),
            files: Vec::new(),
            summary: None,
            decisions: Vec::new(),
            open_questions: Vec::new(),
            context_tokens: None,
            at: Utc::now(),
        }
    }

    #[test]
    fn chain_reads_left_to_right() {
        let cards = [
            card(HandoffKind::Assign, "Rex", None),
            card(HandoffKind::Reroute, "Ada", Some("quota")),
        ];
        assert_eq!(chain(&cards), "Pia → Rex (assign) → Ada (reroute: quota)");
        assert_eq!(chain(&[]), "");
        let value = serde_json::to_value(&cards[1]).unwrap();
        assert_eq!(value["kind"], "reroute");
    }
}
