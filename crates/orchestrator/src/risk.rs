//! When an account has to stop itself before the vendor does (docs/ACCOUNT-RISK.md
//! 「风控熔断」): an identity check, a captcha, a rejected login or a login that fell away is
//! not a failure to retry. Retrying only deepens the profile the vendor is already wary of,
//! so the account pauses here and a person decides what happens next.
//!
//! The shape is [`crate::breaker`]'s — a window, a minimum sample, a threshold and a pause
//! that only a person clears — but the metric is risk signals, not failures. The record is
//! [`crossagents_core::Health`]: `ok` runs are calls that showed nothing, `!ok` runs are
//! risk signals. This module is the policy over it; [`crate::breaker`] is left untouched.

use chrono::{Duration, Utc};
use crossagents_core::{Health, RunRecord};

/// The window the risk breaker looks at.
pub const WINDOW_DAYS: i64 = 7;
/// Under this many risk signals in the window, there is not enough to act on yet.
pub const MIN_SIGNALS: usize = 2;
/// At or above this share of calls in the window being risk signals, the account pauses.
pub const SIGNAL_PERCENT: u64 = 50;

/// The three things an account is not retried after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// The vendor refused with KYC, identity verification, a captcha or a human check.
    IdentityCheck,
    /// An abnormal 401 or 403: the login is no longer accepted.
    Auth,
    /// The account went from `logged_in: true` to `false`.
    Logout,
}

impl Signal {
    /// For the reason a person reads.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IdentityCheck => "an identity check (KYC / captcha / human verification)",
            Self::Auth => "an abnormal 401/403",
            Self::Logout => "the login dropped to logged out",
        }
    }

    /// The words a vendor's refusal uses for a human check. Matched case-insensitively.
    fn in_refusal(text: &str) -> Option<Self> {
        let text = text.to_ascii_lowercase();
        const MARKERS: &[&str] = &[
            "kyc",
            "identity verification",
            "identity check",
            "captcha",
            "human verification",
            "human check",
            "verify you are human",
        ];
        MARKERS
            .iter()
            .any(|marker| text.contains(marker))
            .then_some(Self::IdentityCheck)
    }

    /// A 401 or 403 is the login being refused, not a quota problem: do not retry.
    fn from_status(status: u16) -> Option<Self> {
        matches!(status, 401 | 403).then_some(Self::Auth)
    }

    /// `true -> false` is the login falling away; any other change is not a risk signal.
    fn from_login(was_logged_in: bool, logged_in: bool) -> Option<Self> {
        (was_logged_in && !logged_in).then_some(Self::Logout)
    }
}

/// A vendor refusal that names an identity check or a human check, if it does.
pub fn classify_refusal(text: &str) -> Option<Signal> {
    Signal::in_refusal(text)
}

/// An abnormal 401/403, if `status` is one.
pub fn classify_status(status: u16) -> Option<Signal> {
    Signal::from_status(status)
}

/// A login that was there and is not any more, if that is the change.
pub fn classify_login(was_logged_in: bool, logged_in: bool) -> Option<Signal> {
    Signal::from_login(was_logged_in, logged_in)
}

/// Calls still inside the window, and how many of them showed a risk signal.
pub fn counts(health: &Health, now: chrono::DateTime<Utc>) -> (usize, usize) {
    let since = now - Duration::days(WINDOW_DAYS);
    let mut total = 0;
    let mut signals = 0;
    for run in health.runs.iter().filter(|run| run.at >= since) {
        total += 1;
        if !run.ok {
            signals += 1;
        }
    }
    (total, signals)
}

/// Counts one observed vendor answer (`None` when it showed nothing) and returns why the
/// breaker tripped when it did. Tripping pauses the account; a paused account records
/// nothing more and is never retried by this module — only a person re-arms it.
pub fn record(
    health: &mut Health,
    now: chrono::DateTime<Utc>,
    signal: Option<Signal>,
) -> Option<String> {
    if health.is_paused() {
        return None;
    }
    let since = now - Duration::days(WINDOW_DAYS);
    health.runs.retain(|run| run.at >= since);
    health.runs.push(RunRecord {
        at: now,
        ok: signal.is_none(),
    });
    let (total, signals) = counts(health, now);
    if signals < MIN_SIGNALS || (signals as u64) * 100 < (total as u64) * SIGNAL_PERCENT {
        return None;
    }
    let latest = signal.map_or("an unflagged call", Signal::as_str);
    let reason = format!(
        "{signals} risk signals in {total} calls over the last {WINDOW_DAYS} days \
         (latest: {latest}); the account is paused until a person re-arms it"
    );
    health.pause(now, reason.clone());
    Some(reason)
}

/// [`record`] for a call that showed `signal`.
pub fn record_signal(
    health: &mut Health,
    now: chrono::DateTime<Utc>,
    signal: Signal,
) -> Option<String> {
    record(health, now, Some(signal))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_calls(health: &mut Health, now: chrono::DateTime<Utc>, count: usize) {
        for _ in 0..count {
            record(health, now, None);
        }
    }

    #[test]
    fn a_vendor_refusal_naming_an_identity_check_is_a_signal() {
        for text in [
            "KYC required before continuing",
            "Identity verification is required",
            "Please complete the captcha",
            "Complete human verification to proceed",
            "Verify you are human",
            "identity check needed",
        ] {
            assert_eq!(
                classify_refusal(text),
                Some(Signal::IdentityCheck),
                "{text}"
            );
        }
        assert_eq!(classify_refusal("rate limit exceeded"), None);
        assert_eq!(classify_refusal("model overloaded"), None);
    }

    #[test]
    fn an_abnormal_401_or_403_is_a_signal_but_a_quota_refusal_is_not() {
        assert_eq!(classify_status(401), Some(Signal::Auth));
        assert_eq!(classify_status(403), Some(Signal::Auth));
        assert_eq!(classify_status(200), None);
        assert_eq!(classify_status(429), None);
        assert_eq!(classify_status(500), None);
    }

    #[test]
    fn only_a_login_that_falls_away_is_a_signal() {
        assert_eq!(classify_login(true, false), Some(Signal::Logout));
        assert_eq!(classify_login(false, true), None);
        assert_eq!(classify_login(true, true), None);
        assert_eq!(classify_login(false, false), None);
    }

    #[test]
    fn enough_signals_pause_the_account() {
        let now = Utc::now();
        let mut health = Health::default();
        // One signal is not a sample yet, however bad it looks.
        record_signal(&mut health, now, Signal::Auth);
        assert!(!health.is_paused());
        assert_eq!(counts(&health, now), (1, 1));
        // The second trips it, and the reason names the signals and the latest one.
        let reason = record_signal(&mut health, now, Signal::Logout).expect("the breaker trips");
        assert!(health.is_paused(), "{health:?}");
        assert!(reason.contains("2 risk signals"), "{reason}");
        assert!(reason.contains("login dropped"), "{reason}");
        assert_eq!(health.pause_reason.as_deref(), Some(reason.as_str()));
    }

    #[test]
    fn the_share_of_signals_matters_not_only_their_count() {
        let now = Utc::now();
        // Two signals among many quiet calls is below half: nothing to conclude.
        let mut below = Health::default();
        ok_calls(&mut below, now, 10);
        record_signal(&mut below, now, Signal::Auth);
        record_signal(&mut below, now, Signal::Auth);
        assert!(!below.is_paused(), "{:?}", counts(&below, now));
        // Half of the calls are signals: at the line.
        let mut exact = Health::default();
        ok_calls(&mut exact, now, 2);
        record_signal(&mut exact, now, Signal::IdentityCheck);
        record_signal(&mut exact, now, Signal::IdentityCheck);
        assert!(exact.is_paused(), "{:?}", counts(&exact, now));
    }

    #[test]
    fn signals_outside_the_window_do_not_count() {
        let now = Utc::now();
        let mut health = Health::default();
        let old = now - Duration::days(WINDOW_DAYS + 1);
        // One stale signal is outside today's window and below the sample floor anyway.
        record_signal(&mut health, old, Signal::Auth);
        assert_eq!(counts(&health, now), (0, 0));
        assert!(!health.is_paused());
        // Recording a new observation prunes it, so the record cannot grow without bound.
        record(&mut health, now, None);
        assert_eq!(health.runs.len(), 1);
    }

    /// After a hit there is no retry: the record is closed, later calls change nothing and
    /// the reason stays the first one until a person re-arms the account.
    #[test]
    fn a_paused_account_records_nothing_and_is_never_retried() {
        let now = Utc::now();
        let mut health = Health::default();
        record_signal(&mut health, now, Signal::Auth);
        let reason = record_signal(&mut health, now, Signal::IdentityCheck).expect("trips");
        let runs = health.runs.len();
        let paused_at = health.paused_at;
        assert!(health.is_paused());

        // Every later vendor answer — signal or not — is ignored, not counted as a retry.
        assert_eq!(record(&mut health, now, None), None);
        assert_eq!(record_signal(&mut health, now, Signal::Logout), None);
        assert_eq!(health.runs.len(), runs);
        assert_eq!(counts(&health, now), (runs, runs));
        assert_eq!(health.paused_at, paused_at);
        assert_eq!(health.pause_reason.as_deref(), Some(reason.as_str()));
    }
}
