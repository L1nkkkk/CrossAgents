//! Which account or teammate a task's worker runs on, by quota (docs/tasks/DOGFOOD.md §5,
//! magpie-lancer §2.2).
//!
//! Accounts of the same vendor are used in this order: the one whose quota resets soonest
//! (its leftover disappears at the reset), then ones nothing is known about (read once
//! first), then accounts already full enough for the worker to hand over, and finally
//! accounts that failed (kept at the tail, never dropped).
//!
//! A new execution session hands over at [`NEW_WORKER_PERCENT`]; a worker that is keeping
//! its conversation (a rework) only at [`REWORK_PERCENT`], because moving it throws away the
//! prompt cache it is building on.

use chrono::{DateTime, Utc};
use crossagents_core::{AccountCandidate, AgentId, AgentKey, CoreError};
use crossagents_orchestrator::{ExpiringQuota, Route, RouteDecision, RouteMode};

use crate::accounts::{Fit, Health};
use crate::collab::HubHost;

/// A new execution session hands the task to another account at this window fullness: what
/// is left would not finish a turn, so starting there only wastes the switch.
pub const NEW_WORKER_PERCENT: f64 = 90.0;

/// A worker that keeps its conversation (a rework) is kept until 98%: it is building a
/// prompt cache, and a switch would pay for the whole conversation again.
pub const REWORK_PERCENT: f64 = 98.0;

/// Which accounts are worth a new worker, given what it is.
fn give_at(mode: RouteMode) -> f64 {
    match mode {
        RouteMode::Fresh => NEW_WORKER_PERCENT,
        RouteMode::Rework => REWORK_PERCENT,
    }
}

/// One account weighed for a dispatch, with why it sits where it does.
#[derive(Debug, Clone, PartialEq)]
struct Weighed {
    id: String,
    fit: Fit,
}

/// Where an account ranks with the vendor's others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    /// Worth a new worker (and first in line by its reset).
    Ready,
    /// Nothing known: worth one try, after the ready ones.
    Untried,
    /// Too full for this kind of worker: it would hand over at once.
    Spent,
    /// Out of quota or refused: last, and only when nothing else is left.
    Failed,
}

impl Tier {
    fn of(fit: &Fit, mode: RouteMode) -> Self {
        match fit.health {
            Health::Unknown => Tier::Untried,
            Health::Ok => Tier::Ready,
            Health::Low(_) if fit.fullest.is_some_and(|used| used < give_at(mode)) => Tier::Ready,
            Health::Low(_) => Tier::Spent,
            Health::Exhausted(_) => Tier::Failed,
        }
    }

    fn order(self) -> u8 {
        match self {
            Tier::Ready => 0,
            Tier::Untried => 1,
            Tier::Spent => 2,
            Tier::Failed => 3,
        }
    }

    /// Whether a worker may go here before any teammate is considered.
    fn before_teammates(self) -> bool {
        matches!(self, Tier::Ready | Tier::Untried)
    }
}

/// One account in the dispatch order.
#[derive(Debug, Clone)]
struct Ordered {
    id: String,
    tier: Tier,
    reason: String,
}

/// Which account or teammate a fresh worker should go to, with the accounts weighed and
/// why (magpie-lancer §2.8).
pub async fn route(
    host: &HubHost,
    template: AgentId,
    team: &[AgentId],
    limited: Option<AgentId>,
    mode: RouteMode,
) -> Result<RouteDecision, CoreError> {
    let store = host.bus.store();
    let accounts = &host.accounts;
    let template = store.get_agent(&AgentKey::Id(template)).await?;
    let mut failed_now = None;
    if let Some(limited) = limited
        && let Ok(agent) = store.get_agent(&AgentKey::Id(limited)).await
        && let Some(id) = accounts.account_for(&agent).await
    {
        accounts.mark_reached(&id, "rate limited");
        failed_now = Some(id);
    }
    if !accounts.manages(&template.plugin) {
        return Ok(RouteDecision::default());
    }

    // The same vendor's accounts, each weighed on what is known. One nothing is known about
    // is read once first: routing learns its quota instead of guessing. An account that
    // paused itself on a risk signal is left out entirely — excluded, not ranked lower — and
    // is never probed or retried (docs/ACCOUNT-RISK.md 「风控熔断」).
    let current = accounts.account_for(&template).await;
    let mut weighed = Vec::new();
    for account in store.list_accounts().await? {
        if account.provider != template.plugin {
            continue;
        }
        weighed.push(Weighed {
            id: account.id.clone(),
            fit: accounts.weigh(&account.id, template.model.as_deref()).await,
        });
    }
    let (weighed, paused_for_risk) = split_at_risk(weighed);
    if weighed.is_empty() {
        if let Some(id) = current.as_deref()
            && let Some(reason) = accounts.risk_reason(id)
        {
            // The template's own account is paused: stop here instead of trying anything.
            return Ok(RouteDecision {
                route: Route::Exhausted {
                    reason: format!(
                        "account {id} is paused for risk ({reason}); nothing is retried"
                    ),
                },
                chosen: current,
                candidates: Vec::new(),
                current_usable: false,
                expiring: Vec::new(),
            });
        }
        // Nothing to choose between: the clone runs as planned, on the vendor's own home.
        return Ok(RouteDecision {
            route: Route::Same,
            chosen: None,
            candidates: Vec::new(),
            current_usable: !paused_for_risk,
            expiring: Vec::new(),
        });
    }
    let ordered = order(&weighed, mode, current.as_deref());

    let current_usable = current
        .as_ref()
        .and_then(|id| weighed.iter().find(|w| &w.id == id))
        .is_some_and(|w| Tier::of(&w.fit, mode).before_teammates());
    let why = match &current {
        Some(id) => {
            let fit = &weighed
                .iter()
                .find(|w| &w.id == id)
                .map(|w| w.fit.clone())
                .unwrap_or_else(Fit::unknown);
            format!(
                "account {id} ({})",
                fit.health.reason().unwrap_or(match fit.health {
                    Health::Unknown => "its quota is not known",
                    _ => "out of quota",
                })
            )
        }
        None => "no account is bound to it".to_owned(),
    };

    let candidates: Vec<AccountCandidate> = ordered
        .iter()
        .map(|entry| AccountCandidate {
            account: entry.id.clone(),
            reason: entry.reason.clone(),
        })
        .collect();
    let expiring = weighed
        .iter()
        .filter(|w| !matches!(w.fit.health, Health::Exhausted(_)))
        .filter_map(|w| w.fit.expiring.as_ref().map(|e| (w, e)))
        .map(|(w, e)| ExpiringQuota {
            account: w.id.clone(),
            provider: template.plugin.clone(),
            window: e.window.clone(),
            remaining_percent: e.remaining_percent,
            resets_at: e.resets_at,
        })
        .collect();

    if let Some(entry) = ordered
        .iter()
        .find(|e| e.tier.before_teammates() && Some(&e.id) != failed_now.as_ref())
    {
        if current.as_deref() == Some(entry.id.as_str()) {
            return Ok(RouteDecision {
                route: Route::Same,
                chosen: current,
                candidates,
                current_usable,
                expiring,
            });
        }
        let account = entry.id.clone();
        return Ok(RouteDecision {
            route: Route::Account {
                account: account.clone(),
                reason: format!("runs on account {account} instead: {why}"),
            },
            chosen: Some(account),
            candidates,
            current_usable,
            expiring,
        });
    }

    // Every account of the vendor is done for now: a teammate with quota is next.
    for mate in team.iter().filter(|m| **m != template.id) {
        let Ok(mate) = store.get_agent(&AgentKey::Id(*mate)).await else {
            continue;
        };
        let mate_account = accounts.account_for(&mate).await;
        if mate_account.is_some() && mate_account == current {
            continue;
        }
        let ok = match &mate_account {
            Some(id) => accounts.health(id, mate.model.as_deref()).await == Health::Ok,
            // A plugin whose accounts the hub does not manage has no quota to run out of.
            None => !accounts.manages(&mate.plugin),
        };
        if ok {
            return Ok(RouteDecision {
                route: Route::Teammate {
                    agent: mate.id,
                    reason: format!(
                        "given to {} ({} {}) instead: {why}",
                        mate.name,
                        mate.plugin,
                        mate.model.as_deref().unwrap_or("default model")
                    ),
                },
                chosen: mate_account,
                candidates,
                current_usable,
                expiring,
            });
        }
    }

    // Then the accounts that failed: skipped while anything better was left, never removed.
    // The best of them is still better than no worker at all — but not the one that just
    // failed this task, which would only fail again.
    if let Some(entry) = ordered
        .iter()
        .find(|e| !e.tier.before_teammates() && Some(&e.id) != failed_now.as_ref())
        && current.as_deref() != Some(entry.id.as_str())
    {
        let account = entry.id.clone();
        return Ok(RouteDecision {
            route: Route::Account {
                account: account.clone(),
                reason: format!(
                    "runs on account {account} as a last resort ({}); {why}",
                    entry.reason
                ),
            },
            chosen: Some(account),
            candidates,
            current_usable,
            expiring,
        });
    }
    Ok(RouteDecision {
        route: Route::Exhausted {
            reason: format!("{why}; no other account or teammate has quota left"),
        },
        chosen: current,
        candidates,
        current_usable,
        expiring,
    })
}

/// The accounts, best first, each with the reason for its place.
///
/// Purposely pure: the routing rules (earliest reset first, the two fullness thresholds,
/// unknown quota after known-ready, failures at the tail) are decided here and can be tested
/// without a store.
/// One account, with what its place is decided on.
struct Row<'a> {
    /// Its place in the store's list, so equal accounts keep a stable order.
    index: usize,
    account: &'a Weighed,
    tier: Tier,
    reset: Option<DateTime<Utc>>,
    reason: String,
}

/// Drops accounts that paused themselves on a risk signal: excluded from the choice, not
/// ranked lower, so no later branch can still pick one. Returns whether any was dropped.
fn split_at_risk(weighed: Vec<Weighed>) -> (Vec<Weighed>, bool) {
    let mut usable = Vec::with_capacity(weighed.len());
    let mut any = false;
    for account in weighed {
        if account.fit.at_risk.is_some() {
            any = true;
        } else {
            usable.push(account);
        }
    }
    (usable, any)
}

fn order(weighed: &[Weighed], mode: RouteMode, prefer: Option<&str>) -> Vec<Ordered> {
    let mut rows: Vec<Row> = weighed
        .iter()
        .enumerate()
        .map(|(index, account)| {
            let tier = Tier::of(&account.fit, mode);
            Row {
                index,
                account,
                tier,
                reset: account.fit.resets_at,
                reason: reason(account, tier, mode),
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        let preferred = |row: &Row| Some(row.account.id.as_str()) == prefer;
        a.tier
            .order()
            .cmp(&b.tier.order())
            .then_with(|| reset_key(a.reset).cmp(&reset_key(b.reset)))
            // Among equals, stay where the template already is.
            .then_with(|| preferred(b).cmp(&preferred(a)))
            .then_with(|| a.index.cmp(&b.index))
    });
    rows.into_iter()
        .map(|row| Ordered {
            id: row.account.id.clone(),
            tier: row.tier,
            reason: row.reason,
        })
        .collect()
}

/// `(true, time)` sorts after `(false, any)`: no reset time is the least urgent.
fn reset_key(reset: Option<DateTime<Utc>>) -> (bool, Option<DateTime<Utc>>) {
    (reset.is_none(), reset)
}

/// Why the account sits in its tier, in words.
fn reason(account: &Weighed, tier: Tier, mode: RouteMode) -> String {
    let fit = &account.fit;
    let fullness = match (&fit.fullest, &fit.fullest_window) {
        (Some(used), Some(window)) => format!("its {window} window is {used:.0}% used"),
        (Some(used), None) => format!("its fullest window is {used:.0}% used"),
        _ => "its quota is not known".to_owned(),
    };
    match tier {
        Tier::Ready => match fit.resets_at {
            Some(_) => format!(
                "{fullness}, {}: used first so its quota is not lost at the reset",
                describe_reset(fit.resets_at)
            ),
            None => format!("{fullness}; no reset time known"),
        },
        Tier::Untried => {
            format!("{fullness}: try it once, which also reads what it has left")
        }
        Tier::Spent => format!(
            "{fullness}: {} hands over at {:.0}%",
            mode_word(mode),
            give_at(mode)
        ),
        Tier::Failed => {
            let why = fit
                .health
                .reason()
                .map(str::to_owned)
                .unwrap_or_else(|| "out of quota".into());
            if fit.failed {
                format!("it failed ({why}): last resort, not dropped")
            } else {
                format!("out of quota ({why}): last resort, not dropped")
            }
        }
    }
}

fn describe_reset(reset: Option<DateTime<Utc>>) -> String {
    match reset {
        Some(at) => format!("resets {}", at.format("%Y-%m-%d %H:%M UTC")),
        None => "no reset time known".to_owned(),
    }
}

fn mode_word(mode: RouteMode) -> &'static str {
    match mode {
        RouteMode::Fresh => "a new worker",
        RouteMode::Rework => "a worker in rework",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;
    use crossagents_core::{
        Account, AccountProvider, AccountStatus, DataSource, EventBus, LoginCommand, ModelInfo,
        RateWindow, SessionUsage, Store,
    };
    use crossagents_store_sqlite::SqliteStore;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fit(health: Health, used: Option<f64>, resets_in: Option<i64>, failed: bool) -> Fit {
        Fit {
            health,
            fullest: used,
            fullest_window: used.map(|_| "five_hour".to_owned()),
            resets_at: resets_in.map(|secs| Utc::now() + ChronoDuration::seconds(secs)),
            failed,
            expiring: None,
            at_risk: None,
        }
    }

    fn weighed(id: &str, fit: Fit) -> Weighed {
        Weighed { id: id.into(), fit }
    }

    fn ids(ordered: &[Ordered]) -> Vec<&str> {
        ordered.iter().map(|entry| entry.id.as_str()).collect()
    }

    #[test]
    fn an_at_risk_account_is_excluded_not_ranked_last() {
        // A paused account is not a last resort: it is out of the choice entirely.
        let (usable, dropped) = split_at_risk(vec![
            weighed(
                "risky",
                Fit {
                    at_risk: Some("captcha".into()),
                    ..fit(Health::Ok, Some(5.0), None, false)
                },
            ),
            weighed("healthy", fit(Health::Ok, Some(5.0), None, false)),
        ]);
        assert!(dropped);
        assert_eq!(usable.len(), 1);
        assert_eq!(usable[0].id, "healthy");

        // Even alone it stays out, so no branch can pick it.
        let (none, dropped) = split_at_risk(vec![weighed(
            "risky",
            Fit {
                at_risk: Some("abnormal 403".into()),
                ..fit(Health::Ok, Some(5.0), None, false)
            },
        )]);
        assert!(dropped);
        assert!(none.is_empty());
    }

    #[test]
    fn the_account_that_resets_first_is_used_first() {
        // Its leftover quota is lost at the reset, so spend it before the later one.
        let accounts = [
            weighed("later", fit(Health::Ok, Some(10.0), Some(5 * 3600), false)),
            weighed("sooner", fit(Health::Ok, Some(80.0), Some(600), false)),
        ];
        let ordered = order(&accounts, RouteMode::Fresh, None);
        assert_eq!(ids(&ordered), ["sooner", "later"]);
        assert!(
            ordered[0].reason.contains("not lost at the reset"),
            "{}",
            ordered[0].reason
        );
    }

    #[test]
    fn with_nothing_to_choose_by_it_stays_on_its_own_account() {
        // No reset time on either, same fullness: switching accounts would be pure churn.
        let accounts = [
            weighed("a", fit(Health::Ok, Some(5.0), None, false)),
            weighed("b", fit(Health::Ok, Some(5.0), None, false)),
        ];
        assert_eq!(
            ids(&order(&accounts, RouteMode::Fresh, Some("b"))),
            ["b", "a"]
        );
        assert_eq!(
            ids(&order(&accounts, RouteMode::Fresh, Some("a"))),
            ["a", "b"]
        );
    }

    #[test]
    fn a_new_worker_hands_over_at_ninety_but_a_rework_keeps_it_to_ninety_eight() {
        let accounts = [
            weighed(
                "almost-full",
                fit(Health::Low("90%".into()), Some(95.0), None, false),
            ),
            weighed("fresh", fit(Health::Ok, Some(5.0), None, false)),
        ];
        // A new clone leaves the 95% account for the empty one.
        let fresh = order(&accounts, RouteMode::Fresh, None);
        assert_eq!(ids(&fresh), ["fresh", "almost-full"]);
        assert!(
            fresh[1].reason.contains("hands over at 90%"),
            "{}",
            fresh[1].reason
        );

        // A rework keeps it: two more points of window are cheaper than rebuilding the cache.
        let rework = order(&accounts, RouteMode::Rework, None);
        assert_eq!(ids(&rework), ["almost-full", "fresh"]);
    }

    #[test]
    fn a_failed_account_goes_last_but_is_not_dropped() {
        let accounts = [
            weighed(
                "failed",
                fit(
                    Health::Exhausted("its limit is reached".into()),
                    Some(100.0),
                    None,
                    true,
                ),
            ),
            weighed("healthy", fit(Health::Ok, Some(20.0), Some(3600), false)),
        ];
        let ordered = order(&accounts, RouteMode::Fresh, None);
        assert_eq!(ids(&ordered), ["healthy", "failed"]);
        assert!(
            ordered[1].reason.contains("last resort, not dropped"),
            "{}",
            ordered[1].reason
        );
    }

    #[test]
    fn an_unknown_account_is_tried_after_known_ones_and_before_failures() {
        let accounts = [
            weighed(
                "failed",
                fit(Health::Exhausted("out".into()), Some(100.0), None, true),
            ),
            weighed("unknown", fit(Health::Unknown, None, None, false)),
            weighed("known", fit(Health::Ok, Some(5.0), Some(3600), false)),
        ];
        let ordered = order(&accounts, RouteMode::Fresh, None);
        assert_eq!(ids(&ordered), ["known", "unknown", "failed"]);
        assert!(
            ordered[1].reason.contains("try it once"),
            "{}",
            ordered[1].reason
        );
    }

    /// A provider that answers each status read with `used` percent and counts the reads.
    struct Counting {
        reads: Arc<AtomicUsize>,
        used: f64,
    }

    #[async_trait::async_trait]
    impl AccountProvider for Counting {
        fn name(&self) -> &str {
            "codex"
        }
        fn default_home(&self) -> Option<PathBuf> {
            None
        }
        async fn status(&self, _account: &Account) -> AccountStatus {
            self.reads.fetch_add(1, Ordering::SeqCst);
            AccountStatus {
                logged_in: true,
                auth_mode: None,
                email: None,
                plan: Some("pro".into()),
                subscription_until: None,
                rate_limits: vec![RateWindow {
                    name: "five_hour".into(),
                    used_percent: self.used,
                    window_seconds: Some(5 * 3600),
                    resets_at: Some(Utc::now() + ChronoDuration::hours(4)),
                    remaining: None,
                    total: None,
                }],
                limit_reached: false,
                credits: None,
                limits: Vec::new(),
                reset_credits: Vec::new(),
                source: Some(DataSource::Live),
                as_of: Some(Utc::now()),
                note: None,
            }
        }
        async fn sessions(&self, _account: &Account) -> Result<Vec<SessionUsage>, CoreError> {
            Ok(Vec::new())
        }
        async fn models(&self, _account: &Account) -> Vec<ModelInfo> {
            Vec::new()
        }
        fn login_command(&self, _account: &Account) -> LoginCommand {
            LoginCommand {
                command: Vec::new(),
                env: Default::default(),
                env_remove: Vec::new(),
                serves: None,
            }
        }
        fn agent_env(&self, _account: &Account) -> Vec<(String, String)> {
            Vec::new()
        }
    }

    #[tokio::test]
    async fn an_unknown_account_is_read_once_to_learn_its_quota() {
        let reads = Arc::new(AtomicUsize::new(0));
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().await.unwrap());
        let bus = Arc::new(EventBus::new(store.clone()));
        let accounts = crate::accounts::Accounts::new(
            bus,
            vec![Arc::new(Counting {
                reads: reads.clone(),
                used: 42.0,
            })],
            dir.path().join("accounts"),
        );
        store
            .insert_account(&Account {
                codebuddy_site: None,
                egress: None,
                id: "codex-a".into(),
                provider: "codex".into(),
                label: String::new(),
                home: dir.path().join("a"),
                created_at: Utc::now(),
            })
            .await
            .unwrap();

        // Nothing known: one read, and now routing knows how much is left.
        let first = accounts.weigh("codex-a", None).await;
        assert_eq!(first.health, Health::Ok);
        assert_eq!(first.fullest, Some(42.0));
        assert_eq!(reads.load(Ordering::SeqCst), 1);

        // Known already: weighing is a lookup, not another read.
        let _ = accounts.weigh("codex-a", None).await;
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn an_expiring_window_says_what_resets_and_when() {
        let quota = ExpiringQuota {
            account: "codex-b".into(),
            provider: "codex".into(),
            window: "five_hour".into(),
            remaining_percent: 60.0,
            resets_at: Utc::now() + ChronoDuration::minutes(45),
        };
        let line = quota.line();
        assert!(
            line.contains("codex-b") && line.contains("60%") && line.contains("five_hour"),
            "{line}"
        );
        assert!(line.contains("resets in 0h4"), "{line}");
    }

    /// The fit marks a window that is about to reset with most of it unused, and not one
    /// whose reset is far off (magpie-lancer §2.5).
    #[test]
    fn only_a_soon_and_largely_unused_window_is_expiring() {
        let status = |used: f64, resets_in: ChronoDuration| AccountStatus {
            logged_in: true,
            auth_mode: None,
            email: None,
            plan: None,
            subscription_until: None,
            rate_limits: vec![RateWindow {
                name: "five_hour".into(),
                used_percent: used,
                window_seconds: None,
                resets_at: Some(Utc::now() + resets_in),
                remaining: None,
                total: None,
            }],
            limit_reached: false,
            credits: None,
            limits: Vec::new(),
            reset_credits: Vec::new(),
            source: Some(DataSource::Live),
            as_of: Some(Utc::now()),
            note: None,
        };
        // The window is about to reset with 60% left: the lead is told.
        let soon =
            crate::accounts::expiring_of(&status(40.0, ChronoDuration::minutes(45)).rate_limits);
        assert_eq!(soon.map(|e| e.remaining_percent), Some(60.0));
        // Far from its reset: nothing is lost yet.
        assert!(
            crate::accounts::expiring_of(&status(40.0, ChronoDuration::hours(9)).rate_limits)
                .is_none()
        );
        // Almost used up: not worth re-planning for.
        assert!(
            crate::accounts::expiring_of(&status(95.0, ChronoDuration::minutes(30)).rate_limits)
                .is_none()
        );
    }
}
