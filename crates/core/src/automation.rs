//! When an automation has to stop itself (docs/research/multica.md §2.1, §2.14): a wakeup that
//! runs again and again, or fails almost every time, is broken. Pausing it is cheaper than
//! letting it burn the person's attention and the run's quota. The hub only counts; what to do
//! with a paused rule is the person's decision.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// A waiting rule (watch, reminder) lasts at most this long before it is woken once and closed
/// ("the thing you waited for did not happen").
pub const WAIT_MAX_DAYS: i64 = 7;
/// A wakeup chain that passes the same rule this many times with nobody in between pauses it.
pub const CHAIN_LIMIT: u32 = 3;
/// More triggers than this within an hour pause the rule as well.
pub const TRIGGERS_PER_HOUR: usize = 12;
/// Under this many runs in the window, a failure rate says nothing yet.
pub const MIN_RUNS: usize = 50;
/// At or above this failure rate the automation is broken.
pub const FAILURE_PERCENT: u64 = 90;
/// The window the breaker looks at.
pub const WINDOW_DAYS: i64 = 7;

/// [`WAIT_MAX_DAYS`] as a duration.
pub fn wait_max() -> Duration {
    Duration::days(WAIT_MAX_DAYS)
}

/// One run of an automation: when, and whether it did what it was for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RunRecord {
    pub at: DateTime<Utc>,
    pub ok: bool,
}

/// What an automation (a repeating rule, a run's continuation) has done lately, and whether it
/// is paused. Kept in the rule's or the run's own record, so a restarted hub reads the same
/// counts instead of forgetting them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Health {
    /// One entry per run inside the window; older ones are pruned as it is written.
    #[serde(default)]
    pub runs: Vec<RunRecord>,
    /// Set when the automation stopped itself; it stays paused until a person clears it.
    #[serde(default)]
    pub paused_at: Option<DateTime<Utc>>,
    /// Why it paused, for the note the person gets.
    #[serde(default)]
    pub pause_reason: Option<String>,
}

impl Health {
    pub fn is_paused(&self) -> bool {
        self.paused_at.is_some()
    }

    /// Runs still inside the window, and how many of them failed.
    pub fn counts(&self, now: DateTime<Utc>) -> (usize, usize) {
        let since = now - Duration::days(WINDOW_DAYS);
        let mut total = 0;
        let mut failed = 0;
        for run in self.runs.iter().filter(|run| run.at >= since) {
            total += 1;
            if !run.ok {
                failed += 1;
            }
        }
        (total, failed)
    }

    /// Runs inside the last hour.
    pub fn last_hour(&self, now: DateTime<Utc>) -> usize {
        let since = now - Duration::hours(1);
        self.runs.iter().filter(|run| run.at >= since).count()
    }

    /// Counts one run; returns why the breaker tripped when this run tripped it, and pauses the
    /// automation. A paused automation never trips again: its reason is already with the person.
    pub fn record(&mut self, now: DateTime<Utc>, ok: bool) -> Option<String> {
        if self.is_paused() {
            return None;
        }
        let since = now - Duration::days(WINDOW_DAYS);
        self.runs.retain(|run| run.at >= since);
        self.runs.push(RunRecord { at: now, ok });
        let (total, failed) = self.counts(now);
        if total < MIN_RUNS || (failed as u64) * 100 < (total as u64) * FAILURE_PERCENT {
            return None;
        }
        let reason = format!(
            "{failed} of {total} runs failed in {WINDOW_DAYS} days (at least {FAILURE_PERCENT}%)"
        );
        self.pause(now, reason.clone());
        Some(reason)
    }

    /// Stops the automation, with the reason the person is told.
    pub fn pause(&mut self, now: DateTime<Utc>, reason: String) {
        if self.paused_at.is_none() {
            self.paused_at = Some(now);
            self.pause_reason = Some(reason);
        }
    }
}

/// What a rule's trigger means now that it has been counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// The rule may wake its owner.
    Wake,
    /// The rule stopped itself with this reason, and wakes nobody.
    Pause(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed_runs(health: &mut Health, now: DateTime<Utc>, count: usize, ok: bool) {
        for _ in 0..count {
            health.record(now, ok);
        }
    }

    #[test]
    fn a_burst_of_failed_runs_pauses_the_automation() {
        let now = Utc::now();
        let mut health = Health::default();
        // 49 failed runs say nothing yet, however bad they look: the sample is too small.
        failed_runs(&mut health, now, MIN_RUNS - 1, false);
        assert!(!health.is_paused());
        assert_eq!(health.counts(now), (MIN_RUNS - 1, MIN_RUNS - 1));
        // The fiftieth trips it, and the reason names the counts.
        let reason = health.record(now, false).expect("the breaker trips");
        assert!(health.is_paused(), "{health:?}");
        assert!(reason.contains("50 of 50"), "{reason}");
        assert_eq!(health.pause_reason.as_deref(), Some(reason.as_str()));
        // It stays paused: a later run is not another trip.
        assert_eq!(health.record(now, false), None);
    }

    #[test]
    fn the_rate_matters_not_only_the_count() {
        let now = Utc::now();
        // 44 of 50 failed is 88%: below the line.
        let mut below = Health::default();
        failed_runs(&mut below, now, 44, false);
        failed_runs(&mut below, now, 6, true);
        assert!(!below.is_paused(), "{:?}", below.counts(now));
        // 45 of 50 is exactly 90%: at or above the line. The five good runs come first, so
        // the small-sample guard holds until the window is full.
        let mut exact = Health::default();
        failed_runs(&mut exact, now, 5, true);
        failed_runs(&mut exact, now, 45, false);
        assert!(exact.is_paused(), "{:?}", exact.counts(now));
    }

    #[test]
    fn runs_outside_the_window_do_not_count() {
        let now = Utc::now();
        let mut health = Health::default();
        let old = now - Duration::days(WINDOW_DAYS + 1);
        failed_runs(&mut health, old, 60, true);
        assert_eq!(health.counts(now), (0, 0));
        // Recording a new run prunes them, so the record cannot grow without bound.
        health.record(now, true);
        assert_eq!(health.runs.len(), 1);
    }
}
