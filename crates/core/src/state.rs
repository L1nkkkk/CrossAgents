//! Task state machine. See docs/DESIGN.md §7.1.

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    WaitingAnswer,
    Verifying,
    Retrying,
    Escalated,
    Reviewing,
    /// Reported done; waits for its issuer to check it.
    Passed,
    Merged,
    Failed,
    Cancelled,
    /// The assignee ended a turn without reporting, or gave up; its issuer decides.
    Stalled,
}

impl TaskStatus {
    pub const ALL: [TaskStatus; 12] = [
        Self::Pending,
        Self::Running,
        Self::WaitingAnswer,
        Self::Verifying,
        Self::Retrying,
        Self::Escalated,
        Self::Reviewing,
        Self::Passed,
        Self::Merged,
        Self::Failed,
        Self::Cancelled,
        Self::Stalled,
    ];

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Merged | Self::Failed | Self::Cancelled)
    }

    pub fn can_transition_to(self, next: TaskStatus) -> bool {
        use TaskStatus::*;
        if self.is_terminal() {
            return false;
        }
        // Failing, cancelling and putting back in the queue work from anywhere.
        if matches!(next, Failed | Cancelled | Pending) {
            return true;
        }
        matches!(
            (self, next),
            // The issuer-driven flow (docs/tasks/H1.md §5.7–5.10).
            (Running, Passed | Stalled)
                | (WaitingAnswer, Stalled | Passed)
                | (Stalled, Running | Passed | WaitingAnswer)
                | (Passed, Running)
                | (Reviewing, Running)
                // Earlier pipeline states, still found in older runs.
                | (Pending, Running)
                | (Running, WaitingAnswer | Verifying | Retrying | Escalated)
                | (WaitingAnswer, Running)
                | (Verifying, Passed | Retrying | Escalated)
                | (Retrying, Running)
                | (Escalated, Running)
                | (Passed, Reviewing | Merged)
                | (Reviewing, Passed | Retrying | Escalated)
        )
    }

    /// Returns `next` if the transition is allowed.
    pub fn transition(self, next: TaskStatus) -> Result<TaskStatus, CoreError> {
        if self.can_transition_to(next) {
            Ok(next)
        } else {
            Err(CoreError::InvalidTransition {
                from: self,
                to: next,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TaskStatus::{self, *};

    const ALLOWED: &[(TaskStatus, TaskStatus)] = &[
        (Running, Passed),
        (Running, Stalled),
        (WaitingAnswer, Stalled),
        (WaitingAnswer, Passed),
        (Stalled, Running),
        (Stalled, Passed),
        (Stalled, WaitingAnswer),
        (Passed, Running),
        (Reviewing, Running),
        (Pending, Running),
        (Running, WaitingAnswer),
        (Running, Verifying),
        (Running, Retrying),
        (Running, Escalated),
        (WaitingAnswer, Running),
        (Verifying, Passed),
        (Verifying, Retrying),
        (Verifying, Escalated),
        (Retrying, Running),
        (Escalated, Running),
        (Passed, Reviewing),
        (Passed, Merged),
        (Reviewing, Passed),
        (Reviewing, Retrying),
        (Reviewing, Escalated),
    ];

    #[test]
    fn transition_table_matches_design() {
        for from in TaskStatus::ALL {
            for to in TaskStatus::ALL {
                let expected = !from.is_terminal()
                    && (matches!(to, Failed | Cancelled | Pending)
                        || ALLOWED.contains(&(from, to)));
                assert_eq!(from.can_transition_to(to), expected, "{from:?} -> {to:?}");
            }
        }
    }

    #[test]
    fn rejects_shortcuts() {
        assert!(!Pending.can_transition_to(Passed));
        assert!(!Running.can_transition_to(Merged));
        assert!(!Stalled.can_transition_to(Merged));
        assert!(!Verifying.can_transition_to(Merged));
        assert!(Pending.transition(Merged).is_err());
    }

    #[test]
    fn terminal_states_are_final() {
        for from in [Merged, Failed, Cancelled] {
            assert!(
                TaskStatus::ALL
                    .iter()
                    .all(|&to| !from.can_transition_to(to))
            );
        }
    }
}
