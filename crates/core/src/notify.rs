//! Reminders and watches an agent sets for itself (docs/tasks/H1.md §5.3). The hub never
//! nudges anyone on its own: an agent asks to be woken, and what wakes it goes into its
//! notification digest.
//!
//! A rule is bounded so that it cannot wake its owner forever (docs/research/multica.md §2.1,
//! §2.14): a chain of triggers with nobody in between, a burst within an hour, or a long run of
//! failures pauses it, and a waiting rule lasts at most [`WAIT_MAX_DAYS`](crate::automation::WAIT_MAX_DAYS)
//! before it wakes its owner once and closes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::automation::{self, Health, Trigger};
use crate::ids::{AgentId, TaskId};

/// At most this many reminders, and as many watches, per agent.
pub const MAX_PER_AGENT: usize = 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Reminder {
    pub id: String,
    pub owner: AgentId,
    pub due_at: DateTime<Utc>,
    pub note: String,
    #[serde(default)]
    pub task: Option<TaskId>,
    /// Only fire if `task` has not ended by then.
    #[serde(default)]
    pub only_if_open: bool,
    /// The time asked for was beyond [`automation::WAIT_MAX_DAYS`] and was cut to it: the wake
    /// says that the thing waited for did not happen.
    #[serde(default)]
    pub wait_expired: bool,
    pub created_at: DateTime<Utc>,
}

/// What a watch follows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WatchTarget {
    Task { task_id: TaskId },
    Agent { agent_id: AgentId },
}

/// A change a watch can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum WatchOn {
    /// A task finished (merged, passed) or an agent ended a turn normally.
    Done,
    /// A task failed or was cancelled; an agent's turn or process failed.
    Failed,
    /// A task's assignee asked a question.
    Asked,
    /// A task's assignee ended a turn without reporting.
    Stalled,
    /// An agent has nothing left to do.
    Idle,
    /// Any status change.
    Any,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Watch {
    pub id: String,
    pub owner: AgentId,
    pub target: WatchTarget,
    pub on: Vec<WatchOn>,
    pub created_at: DateTime<Utc>,
    /// When the wait is over: absent means `created_at` plus `WAIT_MAX_DAYS`. At that moment the
    /// owner is woken once ("the thing you waited for did not happen") and the watch closes.
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    /// Triggers in a row with nobody in between. A person speaking to the owner clears it.
    #[serde(default)]
    pub chain: u32,
    /// What this rule has done lately, and whether it paused itself.
    #[serde(default)]
    pub health: Health,
}

impl Watch {
    pub fn wants(&self, what: WatchOn) -> bool {
        self.on.contains(&WatchOn::Any) || self.on.contains(&what)
    }

    /// The default wait is [`automation::WAIT_MAX_DAYS`].
    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
            .unwrap_or_else(|| self.created_at + automation::wait_max())
    }

    /// Counts one trigger and says whether the rule may wake its owner. The chain that reaches
    /// [`automation::CHAIN_LIMIT`], or more than [`automation::TRIGGERS_PER_HOUR`] triggers in an
    /// hour, pauses the rule instead: nobody took part between them, so the next wake only spins.
    pub fn account_trigger(&mut self, now: DateTime<Utc>) -> Trigger {
        if self.health.is_paused() {
            let reason = self
                .health
                .pause_reason
                .clone()
                .unwrap_or_else(|| "paused".into());
            return Trigger::Pause(reason);
        }
        self.chain += 1;
        if self.chain >= automation::CHAIN_LIMIT {
            let reason = format!(
                "it woke its owner {} times in a row with nobody in between",
                self.chain
            );
            self.health.pause(now, reason.clone());
            return Trigger::Pause(reason);
        }
        let in_hour = self.health.last_hour(now);
        if in_hour >= automation::TRIGGERS_PER_HOUR {
            let reason = format!("it fired {in_hour} times within an hour");
            self.health.pause(now, reason.clone());
            return Trigger::Pause(reason);
        }
        Trigger::Wake
    }

    /// A person took part: the chain starts over.
    pub fn human_involved(&mut self) {
        self.chain = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::{CHAIN_LIMIT, TRIGGERS_PER_HOUR};

    fn watch() -> Watch {
        Watch {
            id: "w-1".into(),
            owner: AgentId::new(),
            target: WatchTarget::Agent {
                agent_id: AgentId::new(),
            },
            on: vec![WatchOn::Any],
            created_at: Utc::now(),
            expires_at: None,
            chain: 0,
            health: Health::default(),
        }
    }

    #[test]
    fn the_third_trigger_in_a_row_pauses_the_rule() {
        let now = Utc::now();
        let mut rule = watch();
        for _ in 1..CHAIN_LIMIT {
            assert_eq!(rule.account_trigger(now), Trigger::Wake);
        }
        let trigger = rule.account_trigger(now);
        assert!(
            matches!(&trigger, Trigger::Pause(reason) if reason.contains("in a row")),
            "{trigger:?}"
        );
        assert!(rule.health.is_paused());
        // It stays paused, with its reason, and never wakes anyone again.
        assert!(matches!(rule.account_trigger(now), Trigger::Pause(_)));
        // A person taking part clears the chain, so the rule may wake again.
        let mut again = watch();
        again.account_trigger(now);
        again.account_trigger(now);
        again.human_involved();
        assert_eq!(again.chain, 0);
        assert_eq!(again.account_trigger(now), Trigger::Wake);
    }

    #[test]
    fn a_burst_within_an_hour_pauses_the_rule() {
        let now = Utc::now();
        let mut watch = watch();
        // Twelve triggers in the hour, each delivered as a run of the rule, with a person in
        // between so the chain never reaches its own limit.
        for _ in 0..TRIGGERS_PER_HOUR {
            assert_eq!(watch.account_trigger(now), Trigger::Wake);
            watch.health.record(now, true);
            watch.human_involved();
        }
        let trigger = watch.account_trigger(now);
        assert!(
            matches!(&trigger, Trigger::Pause(reason) if reason.contains("within an hour")),
            "{trigger:?}"
        );
    }

    #[test]
    fn a_burst_a_day_ago_does_not_pause_today() {
        let now = Utc::now();
        let mut watch = watch();
        let yesterday = now - chrono::Duration::days(1);
        for _ in 0..TRIGGERS_PER_HOUR {
            watch.health.record(yesterday, true);
        }
        assert_eq!(watch.account_trigger(now), Trigger::Wake);
    }
}
