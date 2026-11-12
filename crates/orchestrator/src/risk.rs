//! When an account has to stop itself before the vendor does (docs/ACCOUNT-RISK.md
//! 「风控熔断」): an identity check, a captcha, a rejected login or a login that fell away is
//! not a failure to retry. Retrying only deepens the profile the vendor is already wary of,
//! so the account pauses here and a person decides what happens next.
//!
//! The shape is [`crate::breaker`]'s — a window, a threshold and a pause that only a person
//! clears — but the metric is the count of risk signals in the window, not a failure rate:
//! counting risk signals against all calls would let a noisy account hide the one signal
//! that matters, so two signals in the window pause it whatever the call volume. The record
//! is [`crossagents_core::Health`]: a run recorded with `ok: false` is one risk signal.
//! This module is the policy over it; [`crate::breaker`] is left untouched.

use chrono::{Duration, Utc};
use crossagents_core::{Health, RunRecord};

/// The window the risk breaker looks at.
pub const WINDOW_DAYS: i64 = 7;
/// This many risk signals in the window pause the account. Deliberately a raw count, not a
/// share: one identity check is already one too many, and a busy account must not dilute it.
pub const MIN_SIGNALS: usize = 2;

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

/// A vendor refusal that names an identity check or a human check, if it does. A quota
/// refusal ("rate limit", "overloaded") names none and is therefore not a risk signal.
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

/// Risk signals still inside the window.
pub fn signals(health: &Health, now: chrono::DateTime<Utc>) -> usize {
    let since = now - Duration::days(WINDOW_DAYS);
    health
        .runs
        .iter()
        .filter(|run| run.at >= since && !run.ok)
        .count()
}

/// Records one risk signal and returns why the breaker tripped when it did. Tripping pauses
/// the account; a paused account records nothing more and is never retried by this module —
/// only a person re-arms it.
pub fn record(health: &mut Health, now: chrono::DateTime<Utc>, signal: Signal) -> Option<String> {
    if health.is_paused() {
        return None;
    }
    let since = now - Duration::days(WINDOW_DAYS);
    health.runs.retain(|run| run.at >= since);
    health.runs.push(RunRecord { at: now, ok: false });
    let count = signals(health, now);
    if count < MIN_SIGNALS {
        return None;
    }
    let reason = format!(
        "{count} risk signals within {WINDOW_DAYS} days (latest: {}); \
         the account is paused until a person re-arms it",
        signal.as_str()
    );
    health.pause(now, reason.clone());
    Some(reason)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(classify_refusal("quota exceeded, try again later"), None);
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
    fn two_signals_pause_the_account() {
        let now = Utc::now();
        let mut health = Health::default();
        // One signal is not enough yet, however bad it looks.
        record(&mut health, now, Signal::Auth);
        assert!(!health.is_paused());
        assert_eq!(signals(&health, now), 1);
        // The second trips it, and the reason names the count and the latest one.
        let reason = record(&mut health, now, Signal::Logout).expect("the breaker trips");
        assert!(health.is_paused(), "{health:?}");
        assert!(reason.contains("2 risk signals"), "{reason}");
        assert!(reason.contains("login dropped"), "{reason}");
        assert_eq!(health.pause_reason.as_deref(), Some(reason.as_str()));
    }

    /// The count is what trips it: a busy account cannot dilute two signals by making many
    /// more calls, which is the whole reason this is not a failure rate.
    #[test]
    fn the_signal_count_matters_not_a_rate() {
        let now = Utc::now();
        let mut health = Health::default();
        record(&mut health, now, Signal::Auth);
        record(&mut health, now, Signal::IdentityCheck);
        assert!(health.is_paused(), "{:?}", signals(&health, now));
        // Quiet calls would have been recorded elsewhere; they are not signals.
        let mut quiet = Health::default();
        quiet.runs.push(RunRecord { at: now, ok: true });
        quiet.runs.push(RunRecord { at: now, ok: true });
        assert_eq!(signals(&quiet, now), 0);
        assert!(!quiet.is_paused());
    }

    #[test]
    fn signals_outside_the_window_do_not_count() {
        let now = Utc::now();
        let mut health = Health::default();
        let old = now - Duration::days(WINDOW_DAYS + 1);
        // One stale signal is outside today's window and below the floor anyway.
        record(&mut health, old, Signal::Auth);
        assert_eq!(signals(&health, now), 0);
        assert!(!health.is_paused());
        // Recording a new signal prunes it, so the record cannot grow without bound.
        record(&mut health, now, Signal::Auth);
        assert_eq!(health.runs.len(), 1);
    }

    /// After a hit there is no retry: the record is closed, later signals change nothing and
    /// the reason stays the first one until a person re-arms the account.
    #[test]
    fn a_paused_account_records_nothing_and_is_never_retried() {
        let now = Utc::now();
        let mut health = Health::default();
        record(&mut health, now, Signal::Auth);
        let reason = record(&mut health, now, Signal::IdentityCheck).expect("trips");
        let runs = health.runs.len();
        let paused_at = health.paused_at;
        assert!(health.is_paused());

        // Every later vendor answer is ignored, not counted as a retry.
        assert_eq!(record(&mut health, now, Signal::Logout), None);
        assert_eq!(health.runs.len(), runs);
        assert_eq!(signals(&health, now), runs);
        assert_eq!(health.paused_at, paused_at);
        assert_eq!(health.pause_reason.as_deref(), Some(reason.as_str()));
    }
}
