//! When an automation has to stop itself (docs/research/multica.md §2.14): one that runs again
//! and again, or fails almost every time, is broken. Pausing it is cheaper than letting it burn
//! the person's attention and the run's quota. The hub only counts; what to do with a paused
//! automation is the person's decision.
//!
//! The record itself is [`crossagents_core::Health`], kept in the rule's or the run's own JSON
//! so a restarted hub reads the same counts. This module is the policy over it.

use chrono::{Duration, Utc};
use crossagents_core::Health;

/// The window the breaker looks at.
pub const WINDOW_DAYS: i64 = 7;
/// Under this many runs in the window, a failure rate says nothing yet.
pub const MIN_RUNS: usize = 50;
/// At or above this failure rate the automation is broken.
pub const FAILURE_PERCENT: u64 = 90;

/// Runs still inside the window, and how many of them failed.
pub fn counts(health: &Health, now: chrono::DateTime<Utc>) -> (usize, usize) {
    let since = now - Duration::days(WINDOW_DAYS);
    let mut total = 0;
    let mut failed = 0;
    for run in health.runs.iter().filter(|run| run.at >= since) {
        total += 1;
        if !run.ok {
            failed += 1;
        }
    }
    (total, failed)
}

/// Runs inside the last hour.
pub fn last_hour(health: &Health, now: chrono::DateTime<Utc>) -> usize {
    let since = now - Duration::hours(1);
    health.runs.iter().filter(|run| run.at >= since).count()
}

/// Counts one run of the automation; returns why the breaker tripped when this run tripped it,
/// and pauses the automation. A paused automation never trips again: its reason is already with
/// the person, and only their re-arming it clears the record.
pub fn record(health: &mut Health, now: chrono::DateTime<Utc>, ok: bool) -> Option<String> {
    if health.is_paused() {
        return None;
    }
    let since = now - Duration::days(WINDOW_DAYS);
    health.runs.retain(|run| run.at >= since);
    health
        .runs
        .push(crossagents_core::RunRecord { at: now, ok });
    let (total, failed) = counts(health, now);
    if total < MIN_RUNS || (failed as u64) * 100 < (total as u64) * FAILURE_PERCENT {
        return None;
    }
    let reason = format!(
        "{failed} of {total} runs failed in {WINDOW_DAYS} days (at least {FAILURE_PERCENT}%)"
    );
    health.pause(now, reason.clone());
    Some(reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed_runs(health: &mut Health, now: chrono::DateTime<Utc>, count: usize, ok: bool) {
        for _ in 0..count {
            record(health, now, ok);
        }
    }

    #[test]
    fn a_burst_of_failed_runs_pauses_the_automation() {
        let now = Utc::now();
        let mut health = Health::default();
        // 49 failed runs say nothing yet, however bad they look: the sample is too small.
        failed_runs(&mut health, now, MIN_RUNS - 1, false);
        assert!(!health.is_paused());
        assert_eq!(counts(&health, now), (MIN_RUNS - 1, MIN_RUNS - 1));
        // The fiftieth trips it, and the reason names the counts.
        let reason = record(&mut health, now, false).expect("the breaker trips");
        assert!(health.is_paused(), "{health:?}");
        assert!(reason.contains("50 of 50"), "{reason}");
        assert_eq!(health.pause_reason.as_deref(), Some(reason.as_str()));
        // It stays paused: a later run is not another trip.
        assert_eq!(record(&mut health, now, false), None);
    }

    #[test]
    fn the_rate_matters_not_only_the_count() {
        let now = Utc::now();
        // 44 of 50 failed is 88%: below the line.
        let mut below = Health::default();
        failed_runs(&mut below, now, 44, false);
        failed_runs(&mut below, now, 6, true);
        assert!(!below.is_paused(), "{:?}", counts(&below, now));
        // 45 of 50 is exactly 90%: at or above the line. The five good runs come first, so
        // the small-sample guard holds until the window is full.
        let mut exact = Health::default();
        failed_runs(&mut exact, now, 5, true);
        failed_runs(&mut exact, now, 45, false);
        assert!(exact.is_paused(), "{:?}", counts(&exact, now));
    }

    #[test]
    fn runs_outside_the_window_do_not_count() {
        let now = Utc::now();
        let mut health = Health::default();
        let old = now - Duration::days(WINDOW_DAYS + 1);
        failed_runs(&mut health, old, 60, true);
        assert_eq!(counts(&health, now), (0, 0));
        // Recording a new run prunes them, so the record cannot grow without bound.
        record(&mut health, now, true);
        assert_eq!(health.runs.len(), 1);
    }
}
