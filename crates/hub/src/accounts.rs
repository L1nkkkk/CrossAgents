//! Account management (docs/tasks/M5.md): accounts, live status, token usage and login.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crossagents_core::process::{NoWindow, Tree, spawn_tree};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use crossagents_api_types::{
    AccountUsageGroup, AccountUsageResponse, AccountUsageRow, CreateAccountRequest,
    EgressFingerprint, LoginStarted, UsageSeriesGroup, UsageSeriesResponse,
};
use crossagents_core::{
    Account, AccountProvider, AccountStatus, Agent, AgentId, AgentKey, AgentStatus, CoreError,
    DataSource, EGRESS_ENV, EgressPolicy, Event, EventBus, EventKind, RateLimits, Role, RunId,
    Store, TokenTotals, validate_slug,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Mutex, mpsc};

/// The `originator` Claude Code and Codex record for a session CrossAgents started.
pub(crate) const ORIGIN_CROSSAGENTS: &str = "crossagents";
const LOGIN_URL_WAIT: Duration = Duration::from_secs(20);
/// How long a login served as a local page may take to start answering.
const SERVE_READY_WAIT: Duration = Duration::from_secs(30);
/// How often the account is read while its login page is up.
const SERVE_LOGIN_POLL: Duration = Duration::from_secs(3);
/// When a login nobody finishes is stopped, page or not.
const LOGIN_LIMIT: Duration = Duration::from_secs(15 * 60);
/// A status older than this is refreshed in the background while it is still served.
const STATUS_TTL: Duration = Duration::from_secs(60);

/// The last status read per account, with when.
type Statuses = Arc<std::sync::Mutex<HashMap<String, (Instant, AccountStatus)>>>;

/// The per-account risk record ([`crossagents_core::Health`]), shared with background reads.
type RiskRecords = std::sync::Mutex<HashMap<String, crossagents_core::Health>>;

/// `[accounts]` egress overrides from `config.toml`, resolved against each account's own
/// choice and its provider default (docs/ACCOUNT-RISK.md).
#[derive(Clone, Default)]
struct EgressSettings {
    default: Option<EgressPolicy>,
    by_provider: BTreeMap<String, EgressPolicy>,
    by_account: BTreeMap<String, EgressPolicy>,
}

#[derive(Clone)]
struct Observation {
    at: DateTime<Utc>,
    limits: RateLimits,
    top_group: Option<String>,
    refusal: Option<String>,
}

/// A running login tool with everything it started. On Windows a tool found as a `.cmd`
/// runs under `cmd.exe`, so killing the child alone would leave the real tool (`node`)
/// running; the [`Tree`] ends them all, and does when the hub exits too.
struct LoginProcess {
    child: tokio::process::Child,
    tree: Tree,
}

impl LoginProcess {
    fn kill(&mut self) {
        self.tree.end();
        let _ = self.child.start_kill();
    }
}

pub struct Accounts {
    bus: Arc<EventBus>,
    providers: BTreeMap<String, Arc<dyn AccountProvider>>,
    /// Where new accounts' homes go: `<crossagents home>/accounts`.
    root: PathBuf,
    /// Running login processes by account id.
    logins: Arc<Mutex<HashMap<String, LoginProcess>>>,
    /// The quota agents last reported per account, with when.
    observed: std::sync::Mutex<HashMap<String, Observation>>,
    /// Status reads, served at once: a provider read can take seconds (it may start the
    /// vendor's CLI), so only an account's first one is waited for.
    statuses: Statuses,
    /// Accounts whose status is being read in the background.
    refreshing: Arc<std::sync::Mutex<HashSet<String>>>,
    refresh_attempts: std::sync::Mutex<HashMap<String, Instant>>,
    /// Resolved defaults, populated by provider probes, never by a blocking health read.
    default_models: Arc<std::sync::Mutex<HashMap<String, String>>>,
    /// Serialize directory claims so concurrent site accounts cannot share a profile.
    creating: Mutex<()>,
    /// `[accounts]` egress overrides; empty unless the hub is given them.
    egress: std::sync::RwLock<EgressSettings>,
    fingerprint_settings: FingerprintSettings,
    fingerprint_lock: Mutex<()>,
    fingerprint_proxy: std::sync::Mutex<HashMap<String, bool>>,
    /// Per-account risk record (`crossagents_core::Health`): counting and the pause live in
    /// [`crossagents_orchestrator::risk`]. An `Arc` so a background status read can record a
    /// login that fell away without holding `Accounts`. A paused account is excluded from
    /// routing and only a person re-arms it (docs/ACCOUNT-RISK.md 「风控熔断」).
    risk: Arc<RiskRecords>,
    /// Accounts a session may not start on until a person confirms the exit country change:
    /// why, for the GUI. Set where the country change is refused, cleared when it is approved
    /// or when a later exit passes without one (docs/ACCOUNT-RISK.md「出口指纹」).
    egress_blocks: std::sync::Mutex<HashMap<String, String>>,
}

#[derive(Default)]
struct FingerprintSettings {
    echo_url: Option<String>,
    history_days: u32,
    block_country_change: bool,
}

impl Accounts {
    pub fn with_fingerprint(
        mut self,
        echo_url: Option<String>,
        history_days: u32,
        block_country_change: bool,
    ) -> Self {
        self.fingerprint_settings = FingerprintSettings {
            echo_url,
            history_days,
            block_country_change,
        };
        self
    }

    /// The latest observation, without assuming an unknown proxy state survived a restart.
    pub async fn fingerprint(&self, account: &str) -> Option<EgressFingerprint> {
        let rows = self.store().list_egress_records(account).await.ok()?;
        let latest = rows.last()?;
        let changed_at = rows
            .iter()
            .rev()
            .take_while(|r| r.egress_ip == latest.egress_ip)
            .last()
            .map(|r| r.at);
        Some(EgressFingerprint {
            ip: latest.egress_ip.clone(),
            asn: latest.asn.clone(),
            country: latest.country.clone(),
            via_proxy: self.fingerprint_proxy.lock().unwrap().get(account).copied(),
            changed_at,
        })
    }

    /// Called after account and project environments are merged, before starting a session.
    pub async fn check_fingerprint(
        &self,
        agent: &Agent,
        env: &[(String, String)],
    ) -> Result<(), CoreError> {
        let Some(account) = self.account_for(agent).await else {
            return Ok(());
        };
        let proxy = crossagents_drivers::process::effective_proxy_url(env);
        // A direct fallback or NO_PROXY can make the configured proxy differ from the
        // vendor's route. Do not present an unrelated echo as the session's exit.
        let no_proxy = env
            .iter()
            .any(|(key, value)| key.eq_ignore_ascii_case("NO_PROXY") && !value.is_empty())
            || ["NO_PROXY", "no_proxy"]
                .iter()
                .any(|key| std::env::var(key).is_ok_and(|value| !value.is_empty()));
        let certain_proxy = EgressPolicy::from_env(env)
            .unwrap_or_else(|| EgressPolicy::default_for_provider(&agent.plugin))
            == EgressPolicy::ProxyOnly
            && !no_proxy;
        let _guard = self.fingerprint_lock.lock().await;
        {
            let mut states = self.fingerprint_proxy.lock().unwrap();
            if proxy.is_none() {
                states.insert(account.clone(), false);
            } else if certain_proxy {
                states.insert(account.clone(), true);
            } else {
                states.remove(&account);
            }
        }
        let (ip, asn, country) = match (
            &self.fingerprint_settings.echo_url,
            proxy.as_deref().filter(|_| certain_proxy),
        ) {
            (Some(url), Some(proxy)) => lookup_fingerprint(url, proxy).await.unwrap_or_default(),
            _ => Default::default(),
        };
        let record = crossagents_core::EgressRecord {
            account: account.clone(),
            egress_ip: ip,
            asn,
            country,
            at: Utc::now(),
        };
        self.apply_fingerprint(agent.id, record).await
    }

    async fn apply_fingerprint(
        &self,
        agent_id: AgentId,
        mut record: crossagents_core::EgressRecord,
    ) -> Result<(), CoreError> {
        if record.asn.is_none() {
            record.country = None;
        }
        if record.egress_ip.is_none() && record.asn.is_none() && record.country.is_none() {
            return Ok(());
        }
        let account = &record.account;
        let history = self
            .store()
            .list_egress_records(&account)
            .await
            .unwrap_or_default();
        let previous_country_record = history.iter().rev().find(|r| r.country.is_some());
        let previous_country = previous_country_record.and_then(|r| r.country.as_deref());
        let country_changed = record.asn.is_some()
            && previous_country
                .zip(record.country.as_deref())
                .is_some_and(|(a, b)| a != b);
        if country_changed && self.fingerprint_settings.block_country_change {
            let text = format!(
                "账号 {account} 的出口国家从 {} 变为 {}（IP {}）。确认后重试启动。",
                previous_country.unwrap(),
                record.country.as_deref().unwrap(),
                record.egress_ip.as_deref().unwrap_or("unknown")
            );
            let existing = self
                .store()
                .list_human_items(false)
                .await
                .unwrap_or_default()
                .into_iter()
                .rev()
                .find(|i| {
                    i.text == text
                        && i.kind == crossagents_core::HumanItemKind::Question
                        && previous_country_record.is_some_and(|r| i.created_at > r.at)
                });
            let approved = existing.as_ref().is_some_and(|i| {
                i.status == crossagents_core::HumanItemStatus::Answered
                    && i.answer.as_deref() == Some("允许")
            });
            if !approved {
                if existing
                    .as_ref()
                    .is_none_or(|i| i.status != crossagents_core::HumanItemStatus::Open)
                {
                    let item = fingerprint_item(agent_id, text.clone(), true);
                    if self.store().put_human_item(&item).await.is_ok() {
                        let _ = self.bus.publish(EventKind::HumanAsked { item }).await;
                    }
                }
                // Kept for the GUI, which shows that this account is held rather than
                // parsing the notice text: the question carries no account id.
                self.egress_blocks
                    .lock()
                    .unwrap()
                    .insert(account.clone(), text.clone());
                return Err(CoreError::Agent(
                    "出口国家已变化，请先在通知中确认，再重试启动".into(),
                ));
            }
        }
        let cutoff =
            Utc::now() - chrono::Duration::days(i64::from(self.fingerprint_settings.history_days));
        let new_ip = record.egress_ip.as_ref().is_some_and(|ip| {
            !history.iter().any(|r| {
                r.egress_ip.as_ref() == Some(ip)
                    && (self.fingerprint_settings.history_days == 0 || r.at >= cutoff)
            })
        });
        if new_ip {
            let item = fingerprint_item(
                agent_id,
                format!(
                    "账号 {account} 使用了新出口 IP {}。",
                    record.egress_ip.as_deref().unwrap()
                ),
                false,
            );
            if self.store().put_human_item(&item).await.is_ok() {
                let _ = self.bus.publish(EventKind::HumanAsked { item }).await;
            }
        }
        let _ = self.store().insert_egress_record(&record).await;
        // A passing start means there is no pending country change to hold it for any more.
        self.egress_blocks.lock().unwrap().remove(account);
        Ok(())
    }

    pub fn new(
        bus: Arc<EventBus>,
        providers: Vec<Arc<dyn AccountProvider>>,
        root: PathBuf,
    ) -> Self {
        Self {
            bus,
            providers: providers
                .into_iter()
                .map(|p| (p.name().to_owned(), p))
                .collect(),
            root,
            logins: Arc::default(),
            observed: std::sync::Mutex::default(),
            statuses: Arc::default(),
            refreshing: Arc::default(),
            refresh_attempts: std::sync::Mutex::default(),
            default_models: Arc::default(),
            creating: Mutex::new(()),
            egress: std::sync::RwLock::default(),
            fingerprint_settings: FingerprintSettings {
                block_country_change: true,
                ..Default::default()
            },
            fingerprint_lock: Mutex::new(()),
            fingerprint_proxy: std::sync::Mutex::default(),
            risk: Arc::default(),
            egress_blocks: std::sync::Mutex::default(),
        }
    }

    /// Adds the `[accounts]` egress overrides from `config.toml` (docs/ACCOUNT-RISK.md).
    pub fn with_egress(
        mut self,
        default: Option<EgressPolicy>,
        by_provider: BTreeMap<String, EgressPolicy>,
        by_account: BTreeMap<String, EgressPolicy>,
    ) -> Self {
        self.egress = std::sync::RwLock::new(EgressSettings {
            default,
            by_provider,
            by_account,
        });
        self
    }

    /// The egress policy for an account: a per-account override, then what the account
    /// itself stores, then a per-provider override, then `[accounts] egress_default`, then
    /// the provider default. The finer key wins: a broad provider table must not overturn
    /// an account's explicit choice.
    fn egress_for(&self, account: &Account) -> EgressPolicy {
        let settings = self
            .egress
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        settings
            .by_account
            .get(&account.id)
            .copied()
            .or(account.egress)
            .or_else(|| settings.by_provider.get(&account.provider).copied())
            .or(settings.default)
            .unwrap_or_else(|| EgressPolicy::default_for_provider(&account.provider))
    }

    /// Registers each provider's own login (e.g. `~/.codex`) as an account, if it is
    /// logged in and no account uses that home yet. Agents without an account run there.
    pub async fn detect_defaults(&self) -> Result<(), CoreError> {
        let existing = self.store().list_accounts().await?;
        for (name, provider) in &self.providers {
            let Some(home) = provider.default_home() else {
                continue;
            };
            if existing.iter().any(|a| same_path(&a.home, &home)) {
                continue;
            }
            let probe = Account {
                codebuddy_site: None,
                egress: None,
                id: name.clone(),
                provider: name.clone(),
                label: String::new(),
                home: home.clone(),
                created_at: Utc::now(),
            };
            if !provider.status(&probe).await.logged_in {
                continue;
            }
            let id = if existing.iter().any(|a| a.id == *name) {
                format!("{name}-local")
            } else {
                name.clone()
            };
            let account = Account {
                id,
                label: format!("本机 {} 登录", capitalize(name)),
                ..probe
            };
            self.store().insert_account(&account).await?;
            self.bus
                .publish(EventKind::AccountCreated { account })
                .await?;
        }
        Ok(())
    }

    /// The account an agent runs on: the one it names, else its provider's own login.
    pub async fn account_for(&self, agent: &Agent) -> Option<String> {
        if let Some(id) = account_of(agent) {
            return Some(id);
        }
        let home = self.providers.get(&agent.plugin)?.default_home()?;
        let accounts = self.store().list_accounts().await.ok()?;
        accounts
            .into_iter()
            .find(|a| a.provider == agent.plugin && same_path(&a.home, &home))
            .map(|a| a.id)
    }

    /// Records the quota an agent saw. Returns the account it belongs to when the figures
    /// changed.
    async fn observe(&self, agent: AgentId, limits: &RateLimits) -> Option<String> {
        let agent = self.store().get_agent(&AgentKey::Id(agent)).await.ok()?;
        let id = self.account_for(&agent).await?;
        let changed = {
            let mut observed = self.observed.lock().unwrap();
            // A per-call update names only the limit in use: merge it into what a full read
            // reported, by limit id; reset credits stay until a read brings new ones.
            let merged = match observed.get(&id) {
                Some(old) => {
                    let mut merged = limits.clone();
                    let mut groups = old.limits.by_limit.clone();
                    for group in &limits.by_limit {
                        match groups.iter_mut().find(|g| g.id == group.id) {
                            Some(slot) => *slot = group.clone(),
                            None => groups.push(group.clone()),
                        }
                    }
                    merged.by_limit = groups;
                    if merged.reset_credits.is_none() {
                        merged.reset_credits = old.limits.reset_credits.clone();
                    }
                    merged
                }
                None => limits.clone(),
            };
            let changed = observed
                .get(&id)
                .is_none_or(|old| old.limits != merged || old.refusal.is_some());
            let top_group = if limits.by_limit.len() == 1
                && !limits.windows.is_empty()
                && limits.by_limit[0].windows == limits.windows
            {
                Some(limits.by_limit[0].id.clone())
            } else {
                None
            };
            observed.insert(
                id.clone(),
                Observation {
                    at: Utc::now(),
                    limits: merged,
                    top_group,
                    refusal: None,
                },
            );
            changed
        };
        // The vendor's wording for why it refused can name an identity or human check; a
        // quota refusal ("rate limit", "overloaded") is not a risk signal and says None.
        if let Some(signal) = limits
            .reached
            .as_deref()
            .and_then(crossagents_orchestrator::risk::classify_refusal)
        {
            // This stops the calling agent too, on the way out.
            self.record_risk(&id, signal).await;
        }
        changed.then_some(id)
    }

    fn store(&self) -> &Arc<dyn Store> {
        self.bus.store()
    }

    pub fn provider_names(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    /// Whether `plugin` runs on accounts the hub manages (Claude, Codex); others need none.
    pub fn manages(&self, plugin: &str) -> bool {
        self.providers.contains_key(plugin)
    }

    fn provider(&self, name: &str) -> Result<&Arc<dyn AccountProvider>, CoreError> {
        self.providers.get(name).ok_or_else(|| {
            CoreError::Conflict(format!(
                "no account support for {name:?} (available: {})",
                self.provider_names().join(", ")
            ))
        })
    }

    pub async fn create(&self, req: CreateAccountRequest) -> Result<Account, CoreError> {
        let _creating = self.creating.lock().await;
        validate_slug(&req.id)?;
        if req.codebuddy_site.is_some() && (req.provider != "codebuddy" || req.import_default) {
            return Err(CoreError::Conflict("a CodeBuddy site requires a new, independent CodeBuddy login; it cannot import the machine login".into()));
        }
        let provider = self.provider(&req.provider)?;
        let home = match (req.home, req.import_default) {
            (Some(home), _) => PathBuf::from(home),
            (None, true) => provider.default_home().ok_or_else(|| {
                CoreError::Conflict(format!("{} has no default home to import", req.provider))
            })?,
            (None, false) => self.root.join(&req.id),
        };
        std::fs::create_dir_all(&home)
            .map_err(|e| CoreError::Storage(format!("cannot create {}: {e}", home.display())))?;
        let home = if req.codebuddy_site.is_some() {
            let home = dunce::canonicalize(&home)
                .map_err(|e| CoreError::Storage(format!("cannot resolve account home: {e}")))?;
            if provider
                .default_home()
                .is_some_and(|default| same_path(&default, &home))
                || self
                    .store()
                    .list_accounts()
                    .await?
                    .iter()
                    .any(|a| same_path(&a.home, &home))
            {
                return Err(CoreError::Conflict(
                    "use a separate home for each CodeBuddy site account".into(),
                ));
            }
            for directory in ["profile/AppData/Roaming", "profile/AppData/Local"] {
                std::fs::create_dir_all(home.join(directory)).map_err(|e| {
                    CoreError::Storage(format!("cannot create private login profile: {e}"))
                })?;
            }
            home
        } else {
            home
        };
        let account = Account {
            codebuddy_site: req.codebuddy_site,
            egress: None,
            label: req.label.unwrap_or_else(|| req.id.clone()),
            id: req.id,
            provider: req.provider,
            home,
            created_at: Utc::now(),
        };
        self.store().insert_account(&account).await?;
        self.bus
            .publish(EventKind::AccountCreated {
                account: account.clone(),
            })
            .await?;
        Ok(account)
    }

    /// Forgets the account. Its files stay on disk (they may be the user's own `~/.codex`).
    pub async fn remove(&self, id: &str) -> Result<(), CoreError> {
        self.store().delete_account(id).await?;
        self.statuses.lock().unwrap().remove(id);
        self.egress_blocks.lock().unwrap().remove(id);
        if let Some(mut login) = self.logins.lock().await.remove(id) {
            login.kill();
        }
        self.bus
            .publish(EventKind::AccountRemoved { id: id.to_owned() })
            .await?;
        Ok(())
    }

    /// The provider's view of the account, with the quota agents reported since if that is
    /// newer. The last read is served at once; when it is older than [`STATUS_TTL`] a new one
    /// starts in the background (one per account) and `account.quota_updated` says when it
    /// changed something. Only an account's first read is waited for.
    pub async fn status(&self, id: &str) -> Result<AccountStatus, CoreError> {
        // A paused account is never probed again: serve the last read, else a paused stub.
        // Reading it again would be exactly the retry the breaker exists to stop.
        if let Some(reason) = self.risk_reason(id) {
            let cached = self
                .statuses
                .lock()
                .unwrap()
                .get(id)
                .map(|(_, status)| status.clone());
            return Ok(cached.unwrap_or_else(|| paused_status(reason)));
        }
        let account = self.store().get_account(id).await?;
        let provider = self.provider(&account.provider)?.clone();
        let cached = self.statuses.lock().unwrap().get(id).cloned();
        let mut status = match cached {
            Some((read_at, status)) => {
                if read_at.elapsed() >= STATUS_TTL {
                    self.refresh_status(account, provider);
                }
                status
            }
            None => {
                let fresh = provider.status(&account).await;
                let (status, _, signal) = record_status(&self.statuses, id, fresh);
                if let Some(signal) = signal {
                    self.record_risk(id, signal).await;
                }
                status
            }
        };
        let observed = self.observed.lock().unwrap().get(id).cloned();
        if let Some(observation) = observed
            && status.as_of.is_none_or(|as_of| observation.at > as_of)
        {
            let at = observation.at;
            let limits = observation.limits;
            status.rate_limits = limits.windows;
            if limits.credits.is_some() {
                status.credits = limits.credits;
            }
            if limits.plan.is_some() {
                status.plan = limits.plan;
            }
            status.limit_reached = limits.reached.is_some();
            status.limits = limits.by_limit;
            status.reset_credits = limits.reset_credits.unwrap_or_default();
            status.source = Some(DataSource::Live);
            status.as_of = Some(at);
            status.note = None;
        }
        Ok(status)
    }

    /// Whether the account has quota left (docs/tasks/DOGFOOD.md §5), from what is already
    /// known: the cached status and what agents reported since. Never waits for a provider;
    /// an account read for the first time is `Unknown` and a read starts in the background.
    pub async fn health(&self, id: &str, model: Option<&str>) -> Health {
        self.fit(id, model).await.health
    }

    /// What routing weighs an account by, from what is already known (see [`Fit`]).
    pub async fn fit(&self, id: &str, model: Option<&str>) -> Fit {
        // A paused account is out of the running, whatever its quota says.
        if let Some(reason) = self.risk_reason(id) {
            return Fit {
                health: Health::Exhausted(reason.clone()),
                at_risk: Some(reason),
                ..Fit::unknown()
            };
        }
        let Ok(account) = self.store().get_account(id).await else {
            return Fit::unknown();
        };
        let cached = self.statuses.lock().unwrap().get(id).cloned();
        let observed = self.observed.lock().unwrap().get(id).cloned();
        if cached
            .as_ref()
            .is_none_or(|(read_at, _)| read_at.elapsed() >= STATUS_TTL)
            && let Ok(provider) = self.provider(&account.provider)
        {
            self.refresh_status_throttled(account.clone(), provider.clone());
        }
        let mut windows = Vec::new();
        let mut groups = Vec::new();
        let mut reached = None;
        let mut top_group = None;
        let mut refusal = None;
        let mut credits = None;
        if let Some((_, status)) = &cached {
            windows.extend(status.rate_limits.iter().cloned());
            groups = status.limits.clone();
            top_group = matching_top_group(&windows, &groups);
            if status.limit_reached {
                reached = Some("limit reached".to_owned());
            }
            credits = status.credits.clone();
        }
        // What an agent saw last wins over an older read.
        if let Some(observation) = &observed
            && cached
                .as_ref()
                .is_none_or(|(_, s)| s.as_of.is_none_or(|as_of| observation.at > as_of))
        {
            let limits = &observation.limits;
            windows = limits.windows.clone();
            groups = limits.by_limit.clone();
            reached = limits.reached.clone();
            top_group = observation.top_group.clone();
            refusal = observation.refusal.clone();
            if limits.credits.is_some() {
                credits = limits.credits.clone();
            }
        }
        if cached.is_none() && observed.is_none() {
            return Fit::unknown();
        }
        let default_model = self.default_models.lock().unwrap().get(id).cloned();
        let model = model
            .filter(|m| !matches!(*m, "" | "default" | "auto"))
            .or(default_model.as_deref());
        let health = judge_for_model_with_top(
            &windows,
            &groups,
            model,
            reached.as_deref(),
            credits.as_ref(),
            top_group.as_deref(),
            refusal.as_deref(),
            &account.provider,
        );
        let now = Utc::now();
        let running: Vec<&crossagents_core::RateWindow> = windows
            .iter()
            .filter(|w| w.resets_at.is_none_or(|at| at > now))
            .collect();
        let fullest = running
            .iter()
            .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent));
        let resets_at = running.iter().filter_map(|w| w.resets_at).min();
        // A window that resets soon with most of it unused is about to be lost
        // (magpie-lancer §2.5): the run's lead can still put work on it.
        let expiring = expiring_of(&windows);
        Fit {
            health,
            fullest: fullest.map(|w| w.used_percent),
            fullest_window: fullest.map(|w| w.name.clone()),
            resets_at,
            failed: refusal.is_some(),
            expiring,
            at_risk: None,
        }
    }

    /// Like [`Self::fit`], but an account nothing is known about is read once first, so
    /// routing can weigh its quota instead of guessing (magpie-lancer §2.2: an account
    /// whose quota is unknown gets one try, which also learns what it has left).
    pub async fn weigh(&self, id: &str, model: Option<&str>) -> Fit {
        // Never probe a paused account: reading it again is a retry it must not get.
        if let Some(reason) = self.risk_reason(id) {
            return Fit {
                health: Health::Exhausted(reason.clone()),
                at_risk: Some(reason),
                ..Fit::unknown()
            };
        }
        let unknown = {
            let statuses = self.statuses.lock().unwrap();
            let observed = self.observed.lock().unwrap();
            !statuses.contains_key(id) && !observed.contains_key(id)
        };
        if unknown {
            // The blocking read goes in the cache, so the `fit` below starts no other.
            let _ = self.status(id).await;
        }
        self.fit(id, model).await
    }

    /// Marks the account as out of quota now (an agent on it was refused).
    pub fn mark_reached(&self, id: &str, why: &str) {
        let mut observed = self.observed.lock().unwrap();
        let mut limits = observed
            .get(id)
            .map(|o| o.limits.clone())
            .unwrap_or_default();
        let top_group = observed.get(id).and_then(|o| o.top_group.clone());
        limits.reached = Some(why.to_owned());
        observed.insert(
            id.to_owned(),
            Observation {
                at: Utc::now(),
                limits,
                top_group,
                refusal: Some(why.to_owned()),
            },
        );
    }

    /// Why the account is paused for risk, if it is: the note a person reads.
    pub fn risk_reason(&self, id: &str) -> Option<String> {
        self.risk
            .lock()
            .unwrap()
            .get(id)
            .filter(|health| health.is_paused())
            .and_then(|health| health.pause_reason.clone())
    }

    /// Whether the account paused itself on a risk signal.
    pub fn is_at_risk(&self, id: &str) -> bool {
        self.risk
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(crossagents_core::Health::is_paused)
    }

    /// Why a session on the account is held until a person confirms the exit country
    /// change, if one is pending: the reason the GUI shows.
    pub fn egress_block(&self, id: &str) -> Option<String> {
        self.egress_blocks.lock().unwrap().get(id).cloned()
    }

    /// Records one risk signal for the account and pauses it when
    /// [`crossagents_orchestrator::risk`] says the threshold is crossed. A paused account
    /// stops its agents and is never retried; this returns the reason on the call that
    /// tripped it, `None` otherwise.
    pub async fn record_risk(
        &self,
        id: &str,
        signal: crossagents_orchestrator::risk::Signal,
    ) -> Option<String> {
        record_risk_signal(&self.bus, &self.risk, id, signal).await
    }

    /// Marks the account at risk: it is paused (only a person can re-arm it) and every agent
    /// bound to it is stopped. Nothing here retries, reconnects or moves the work to another
    /// account: a session that keeps going only deepens the profile the vendor already
    /// doubts (docs/ACCOUNT-RISK.md 「风控熔断」).
    pub async fn mark_at_risk(&self, id: &str, why: &str) {
        pause_and_stop(&self.bus, &self.risk, id, why).await;
    }

    /// A person re-arms the account by hand; nothing else clears a risk pause.
    pub fn clear_risk(&self, id: &str) {
        self.risk.lock().unwrap().remove(id);
    }

    fn refresh_status_throttled(&self, account: Account, provider: Arc<dyn AccountProvider>) {
        let mut attempts = self.refresh_attempts.lock().unwrap();
        if attempts
            .get(&account.id)
            .is_some_and(|at| at.elapsed() < STATUS_TTL)
        {
            return;
        }
        attempts.insert(account.id.clone(), Instant::now());
        drop(attempts);
        self.refresh_status(account, provider);
    }

    /// Reads `account`'s status in the background, unless a read is already running.
    fn refresh_status(&self, account: Account, provider: Arc<dyn AccountProvider>) {
        if !self.refreshing.lock().unwrap().insert(account.id.clone()) {
            return;
        }
        let statuses = self.statuses.clone();
        let refreshing = self.refreshing.clone();
        let bus = self.bus.clone();
        let defaults = self.default_models.clone();
        let risk = self.risk.clone();
        tokio::spawn(async move {
            let fresh = provider.status(&account).await;
            let (_, changed, signal) = record_status(&statuses, &account.id, fresh);
            if let Some(signal) = signal {
                // A login that fell away is a risk signal: pause and stop, never retry.
                record_risk_signal(&bus, &risk, &account.id, signal).await;
            }
            refreshing.lock().unwrap().remove(&account.id);
            if changed {
                let _ = bus
                    .publish(EventKind::AccountQuotaUpdated {
                        id: account.id.clone(),
                    })
                    .await;
            }
            // Claude's initialize list is explicitly default-first; aliases such as
            // `default` do not identify a model and must not exclude other groups.
            if account.provider == "claude" {
                let models = provider.models(&account).await;
                let resolved = models
                    .first()
                    .map(|m| m.id.as_str())
                    .filter(|m| !matches!(*m, "" | "default" | "auto"));
                let mut defaults = defaults.lock().unwrap();
                match resolved {
                    Some(model) => {
                        defaults.insert(account.id, model.to_owned());
                    }
                    None => {
                        defaults.remove(&account.id);
                    }
                }
            }
        });
    }

    pub async fn models(&self, id: &str) -> Result<Vec<crossagents_core::ModelInfo>, CoreError> {
        let account = self.store().get_account(id).await?;
        let models = self.provider(&account.provider)?.models(&account).await;
        for model in &models {
            self.store()
                .clear_unavailable_model(&account.provider, &account.id, &model.id)
                .await?;
        }
        Ok(models)
    }

    /// The model ids `plugin`'s logins report, each once, in the order the first login to
    /// report it lists them. Empty when the plugin has no accounts or none reported yet.
    pub async fn offered_models(&self, plugin: &str) -> Vec<String> {
        let Ok(provider) = self.provider(plugin) else {
            return Vec::new();
        };
        let Ok(accounts) = self.store().list_accounts().await else {
            return Vec::new();
        };
        let mut models: Vec<String> = Vec::new();
        for account in accounts.iter().filter(|a| a.provider == plugin) {
            for model in provider.models(account).await {
                if !models.contains(&model.id) {
                    models.push(model.id);
                }
            }
        }
        models
    }

    /// Environment for an agent bound to an account through `config.account`.
    ///
    /// Carries the account's egress policy too, in [`EGRESS_ENV`]: drivers take it from the
    /// session's environment rather than reading configuration themselves.
    pub async fn agent_env(&self, agent: &Agent) -> Result<Vec<(String, String)>, CoreError> {
        let Some(id) = account_of(agent) else {
            return Ok(Vec::new());
        };
        let account = self.store().get_account(&id).await?;
        let mut env = self.provider(&account.provider)?.agent_env(&account);
        env.push((
            EGRESS_ENV.to_owned(),
            self.egress_for(&account).as_str().to_owned(),
        ));
        Ok(env)
    }

    /// Checks that `agent` may use the account it names.
    pub async fn check_binding(
        &self,
        agent_plugin: &str,
        account_id: &str,
    ) -> Result<(), CoreError> {
        let account = self.store().get_account(account_id).await?;
        if account.provider != agent_plugin {
            return Err(CoreError::Conflict(format!(
                "account {account_id:?} is a {} account; it cannot run a {agent_plugin} agent",
                account.provider
            )));
        }
        Ok(())
    }

    pub async fn usage(
        &self,
        id: &str,
        by: AccountUsageGroup,
    ) -> Result<AccountUsageResponse, CoreError> {
        let account = self.store().get_account(id).await?;
        let sessions = self.provider(&account.provider)?.sessions(&account).await?;
        let owners = if matches!(
            by,
            AccountUsageGroup::Agent | AccountUsageGroup::Role | AccountUsageGroup::Run
        ) {
            session_owners(self.store().as_ref()).await?
        } else {
            HashMap::new()
        };
        let mut groups: BTreeMap<String, TokenTotals> = BTreeMap::new();
        let mut total = TokenTotals::default();
        for session in &sessions {
            let ours = session.originator.as_deref() == Some(ORIGIN_CROSSAGENTS);
            let owner = owners.get(&session.session_id);
            let key = match by {
                AccountUsageGroup::Day => session
                    .started_at
                    .with_timezone(&Local)
                    .format("%Y-%m-%d")
                    .to_string(),
                AccountUsageGroup::Model => {
                    session.model.clone().unwrap_or_else(|| "unknown".into())
                }
                AccountUsageGroup::Origin => if ours {
                    "crossagents"
                } else {
                    "outside CrossAgents"
                }
                .into(),
                AccountUsageGroup::Agent => owner.map_or_else(|| outside(ours), |o| o.name.clone()),
                AccountUsageGroup::Role => {
                    owner.map_or_else(|| outside(ours), |o| role_name(o.role))
                }
                AccountUsageGroup::Run => owner
                    .and_then(|o| o.run)
                    .map_or_else(|| outside(ours), |r| r.to_string()),
            };
            groups.entry(key).or_default().add(&session.totals);
            total.add(&session.totals);
        }
        let mut rows: Vec<AccountUsageRow> = groups
            .into_iter()
            .map(|(key, totals)| AccountUsageRow {
                cache_hit_rate: totals.cache_hit_rate(),
                key,
                totals,
            })
            .collect();
        if by == AccountUsageGroup::Day {
            rows.reverse(); // newest day first
        }
        Ok(AccountUsageResponse {
            by,
            rows,
            total_cache_hit_rate: total.cache_hit_rate(),
            total,
        })
    }

    /// `GET /v1/usage/series` (see [`crate::usage_series`]): token use and cost day by day,
    /// across every account or one. An account whose logs cannot be read is reported in
    /// `sources` with its reason; the rest of the answer stands.
    pub(crate) async fn usage_series(
        &self,
        query: crate::usage_series::SeriesQuery,
    ) -> Result<UsageSeriesResponse, CoreError> {
        let picked: Vec<Account> = if query.source == "all" {
            self.store().list_accounts().await?
        } else {
            vec![self.store().get_account(&query.source).await?]
        };
        let owners = if matches!(query.group, UsageSeriesGroup::Agent | UsageSeriesGroup::Run) {
            session_owners(self.store().as_ref()).await?
        } else {
            HashMap::new()
        };
        let mut sources = Vec::with_capacity(picked.len());
        for account in &picked {
            // Each vendor's own logs, with every request timed where it records them.
            let sessions = match self.provider(&account.provider) {
                Ok(provider) => provider
                    .usage_sessions(account)
                    .await
                    .map_err(|err| err.to_string()),
                Err(err) => Err(err.to_string()),
            };
            sources.push(crate::usage_series::SourceData {
                id: account.id.clone(),
                label: if account.label.is_empty() {
                    account.id.clone()
                } else {
                    account.label.clone()
                },
                provider: account.provider.clone(),
                sessions,
            });
        }
        Ok(crate::usage_series::build(
            &query,
            &sources,
            &owners,
            Local::now().date_naive(),
            &|at| at.with_timezone(&Local).date_naive(),
        ))
    }

    /// Starts the provider's login tool for this account and returns the page to open.
    /// The outcome arrives later as `account.login_finished`.
    pub async fn login(&self, id: &str) -> Result<LoginStarted, CoreError> {
        let account = self.store().get_account(id).await?;
        let provider = self.provider(&account.provider)?;
        if let Some(steps) = provider.manual_login(&account) {
            return Ok(LoginStarted {
                url: None,
                command: steps.join(" → "),
                manual: Some(steps),
            });
        }
        let login = provider.login_command(&account);
        let (program, args) = login
            .command
            .split_first()
            .ok_or_else(|| CoreError::Agent("empty login command".into()))?;
        let display = login.command.join(" ");
        if let Some(mut old) = self.logins.lock().await.remove(id) {
            old.kill();
        }
        let mut command = tokio::process::Command::new(resolve_program(program));
        command.args(args).no_window();
        for key in &login.env_remove {
            command.env_remove(key);
        }
        command
            .envs(&login.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let (mut child, tree) = spawn_tree(&mut command)
            .map_err(|e| CoreError::Agent(format!("cannot start `{display}`: {e}")))?;
        let mut lines_rx = output_lines(&mut child);
        let process = LoginProcess { child, tree };
        if let Some(page) = login.serves {
            return self
                .serve_login(account, provider.clone(), process, lines_rx, page, display)
                .await;
        }
        let pid = process.child.id();
        self.logins.lock().await.insert(id.to_owned(), process);

        let mut url = None;
        let mut tail = Vec::new();
        let deadline = tokio::time::Instant::now() + LOGIN_URL_WAIT;
        while url.is_none() {
            match tokio::time::timeout_at(deadline, lines_rx.recv()).await {
                Ok(Some(line)) => {
                    url = find_url(&line);
                    tail.push(line);
                }
                _ => break,
            }
        }
        self.bus
            .publish(EventKind::AccountLoginPending {
                id: id.to_owned(),
                url: url.clone(),
            })
            .await?;

        // Wait for the login to finish in the background.
        let bus = self.bus.clone();
        let id_owned = id.to_owned();
        let provider = self.provider(&account.provider)?.clone();
        let logins = self.logins.clone();
        let statuses = self.statuses.clone();
        tokio::spawn(async move {
            let mut rest = Vec::new();
            let limit = tokio::time::sleep(LOGIN_LIMIT);
            tokio::pin!(limit);
            let timed_out = loop {
                tokio::select! {
                    line = lines_rx.recv() => match line {
                        Some(line) => rest.push(line),
                        None => break false,
                    },
                    () = &mut limit => break true,
                }
            };
            let login = {
                let mut logins = logins.lock().await;
                match logins.get(&id_owned) {
                    Some(login) if login.child.id() == pid => logins.remove(&id_owned),
                    _ => None, // replaced or cancelled
                }
            };
            let Some(mut login) = login else { return };
            if timed_out {
                login.kill();
                rest.push(format!(
                    "not logged in after {} minutes; the login was stopped",
                    LOGIN_LIMIT.as_secs() / 60
                ));
            }
            let exit = login.child.wait().await.ok();
            let status = provider.status(&account).await;
            record_status(&statuses, &id_owned, status.clone());
            let ok = exit.is_some_and(|e| e.success()) && status.logged_in;
            let message = if ok {
                None
            } else {
                let all: Vec<&str> = tail.iter().chain(rest.iter()).map(String::as_str).collect();
                Some(all[all.len().saturating_sub(5)..].join("\n"))
            };
            let _ = bus
                .publish(EventKind::AccountLoginFinished {
                    id: id_owned,
                    ok,
                    message,
                })
                .await;
        });
        Ok(LoginStarted {
            url,
            command: display,
            manual: None,
        })
    }

    /// A login whose tool serves its page locally ([`LoginCommand::serves`]): waits until
    /// the page answers and returns it, then watches the account in the background and
    /// stops the server once it reports the login, the server stops, or
    /// [`LOGIN_LIMIT`] passes. Logging in again an account that already is logged in
    /// counts as done once a read sees it logged out and then in, else at the limit.
    async fn serve_login(
        &self,
        account: Account,
        provider: Arc<dyn AccountProvider>,
        mut login: LoginProcess,
        lines: mpsc::UnboundedReceiver<String>,
        page: String,
        display: String,
    ) -> Result<LoginStarted, CoreError> {
        let id = account.id.clone();
        let tail = keep_tail(lines);
        let address = page_address(&page);
        let deadline = Instant::now() + SERVE_READY_WAIT;
        loop {
            match &address {
                Some(at) if tokio::net::TcpStream::connect(at.as_str()).await.is_ok() => break,
                None => break,
                Some(_) => {}
            }
            if let Ok(Some(exit)) = login.child.try_wait() {
                tokio::time::sleep(Duration::from_millis(200)).await;
                return Err(CoreError::Agent(format!(
                    "`{display}` stopped ({exit}) before serving its login page{}",
                    tail_text(&tail)
                )));
            }
            if Instant::now() >= deadline {
                login.kill();
                return Err(CoreError::Agent(format!(
                    "`{display}` did not serve its login page within {} s{}",
                    SERVE_READY_WAIT.as_secs(),
                    tail_text(&tail)
                )));
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        let was_logged_in = self
            .statuses
            .lock()
            .unwrap()
            .get(&id)
            .is_some_and(|(_, status)| status.logged_in);
        let pid = login.child.id();
        self.logins.lock().await.insert(id.clone(), login);
        self.bus
            .publish(EventKind::AccountLoginPending {
                id: id.clone(),
                url: Some(page.clone()),
            })
            .await?;

        let bus = self.bus.clone();
        let logins = self.logins.clone();
        let statuses = self.statuses.clone();
        let shown = display.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            let mut seen_logged_out = !was_logged_in;
            let (status, stopped) = loop {
                tokio::time::sleep(SERVE_LOGIN_POLL).await;
                let stopped = {
                    let mut logins = logins.lock().await;
                    let Some(login) = logins.get_mut(&id).filter(|l| l.child.id() == pid) else {
                        return; // replaced or cancelled
                    };
                    let stopped = login.child.try_wait().ok().flatten();
                    if stopped.is_some() {
                        logins.remove(&id);
                    }
                    stopped
                };
                provider.forget_status(&account);
                let status = provider.status(&account).await;
                seen_logged_out |= !status.logged_in;
                let done = status.logged_in && seen_logged_out;
                if done || stopped.is_some() || started.elapsed() >= LOGIN_LIMIT {
                    break (status, stopped);
                }
            };
            {
                let mut logins = logins.lock().await;
                if logins.get(&id).is_some_and(|l| l.child.id() == pid)
                    && let Some(mut login) = logins.remove(&id)
                {
                    login.kill();
                }
            }
            record_status(&statuses, &id, status.clone());
            let ok = status.logged_in;
            let message = (!ok).then(|| match stopped {
                Some(exit) => format!("`{shown}` stopped ({exit}){}", tail_text(&tail)),
                None => format!(
                    "not logged in after {} minutes; the login page was closed",
                    LOGIN_LIMIT.as_secs() / 60
                ),
            });
            let _ = bus
                .publish(EventKind::AccountLoginFinished { id, ok, message })
                .await;
        });
        Ok(LoginStarted {
            url: Some(page),
            command: display,
            manual: None,
        })
    }
}

impl Accounts {
    /// Reads every account's status in the background, so the first page to ask does not
    /// wait for the vendors' CLIs.
    pub fn warm(self: &Arc<Self>) {
        let accounts = self.clone();
        tokio::spawn(async move {
            let Ok(list) = accounts.store().list_accounts().await else {
                return;
            };
            for account in list {
                if let Ok(provider) = accounts.provider(&account.provider) {
                    accounts.refresh_status(account, provider.clone());
                }
            }
        });
    }

    /// Follows agents' quota reports and announces changes per account.
    pub fn watch(self: &Arc<Self>) {
        let mut events = self.bus.subscribe();
        let accounts = self.clone();
        tokio::spawn(async move {
            loop {
                let event = match events.recv().await {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                };
                accounts.on_event(&event).await;
            }
        });
    }

    async fn on_event(&self, event: &Event) {
        if let EventKind::AgentRateLimits { id, limits } = &event.kind
            && let Some(account) = self.observe(*id, limits).await
        {
            let _ = self
                .bus
                .publish(EventKind::AccountQuotaUpdated { id: account })
                .await;
        }
    }
}

/// What an account reads as when it paused before any status was cached: logged out, with
/// the risk reason as the note a person sees.
fn paused_status(reason: String) -> AccountStatus {
    AccountStatus {
        logged_in: false,
        auth_mode: None,
        email: None,
        plan: None,
        subscription_until: None,
        rate_limits: Vec::new(),
        limit_reached: false,
        credits: None,
        limits: Vec::new(),
        reset_credits: Vec::new(),
        source: Some(DataSource::Snapshot),
        as_of: None,
        note: Some(reason),
    }
}

/// Counts one risk signal and, when it trips, pauses the account and stops its agents. Free
/// of `Accounts` so a spawned status read can call it too.
async fn record_risk_signal(
    bus: &EventBus,
    risk: &RiskRecords,
    id: &str,
    signal: crossagents_orchestrator::risk::Signal,
) -> Option<String> {
    let tripped = {
        let mut risk = risk.lock().unwrap();
        let health = risk.entry(id.to_owned()).or_default();
        crossagents_orchestrator::risk::record(health, Utc::now(), signal)
    };
    if let Some(reason) = &tripped {
        pause_and_stop(bus, risk, id, reason).await;
    }
    tripped
}

/// Pauses the account and stops every agent bound to it. `pause` keeps the first reason; the
/// agents are stopped either way. Store-only, so a background task can call it.
async fn pause_and_stop(bus: &EventBus, risk: &RiskRecords, id: &str, why: &str) {
    {
        let mut risk = risk.lock().unwrap();
        let health = risk.entry(id.to_owned()).or_default();
        health.pause(Utc::now(), why.to_owned());
    }
    let Ok(agents) = bus.store().list_agents().await else {
        return;
    };
    for agent in agents {
        if account_of(&agent).as_deref() != Some(id) || agent.status == AgentStatus::Stopped {
            continue;
        }
        if bus
            .store()
            .update_agent_status(agent.id, AgentStatus::Stopped)
            .await
            .is_ok()
        {
            let _ = bus
                .publish(EventKind::AgentStatusChanged {
                    id: agent.id,
                    from: agent.status,
                    to: AgentStatus::Stopped,
                })
                .await;
        }
    }
}

/// Stores a fresh read of account `id` (see [`keep_last_quota`]). Returns what is now
/// served, whether it differs from what was (apart from when it was read), and the risk
/// signal the change itself carried (a login that fell from `true` to `false`).
fn record_status(
    statuses: &Statuses,
    id: &str,
    fresh: AccountStatus,
) -> (
    AccountStatus,
    bool,
    Option<crossagents_orchestrator::risk::Signal>,
) {
    let mut statuses = statuses.lock().unwrap();
    let previous = statuses.get(id).map(|(_, status)| status.clone());
    let signal = previous.as_ref().and_then(|old| {
        crossagents_orchestrator::risk::classify_login(old.logged_in, fresh.logged_in)
    });
    let status = keep_last_quota(previous.as_ref(), fresh);
    let changed = previous.is_none_or(|old| {
        let undated = |s: &AccountStatus| AccountStatus {
            as_of: None,
            ..s.clone()
        };
        undated(&old) != undated(&status)
    });
    statuses.insert(id.to_owned(), (Instant::now(), status.clone()));
    (status, changed, signal)
}

/// A read that comes back logged in but without the quota (the vendor's usage call failed
/// or was refused this time) keeps the quota the last read had, marked as that read's
/// figures, instead of wiping it until the next good read. A logged-out read is taken as
/// it is.
fn keep_last_quota(previous: Option<&AccountStatus>, fresh: AccountStatus) -> AccountStatus {
    let has_quota = |s: &AccountStatus| !s.rate_limits.is_empty() || !s.limits.is_empty();
    match previous {
        Some(last) if fresh.logged_in && !has_quota(&fresh) && has_quota(last) => AccountStatus {
            rate_limits: last.rate_limits.clone(),
            limits: last.limits.clone(),
            limit_reached: last.limit_reached,
            credits: fresh.credits.or_else(|| last.credits.clone()),
            reset_credits: last.reset_credits.clone(),
            plan: fresh.plan.or_else(|| last.plan.clone()),
            source: Some(DataSource::Snapshot),
            as_of: last.as_of,
            note: Some(match fresh.note {
                Some(why) => format!("{why}; showing the last read quota"),
                None => "showing the last read quota".into(),
            }),
            ..fresh
        },
        _ => fresh,
    }
}

/// `codex` -> `Codex`, `codebuddy` -> `CodeBuddy`.
fn capitalize(word: &str) -> String {
    if word == "codebuddy" {
        return "CodeBuddy".into();
    }
    let mut chars = word.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

fn same_path(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| dunce::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

async fn lookup_fingerprint(
    url: &str,
    proxy: &str,
) -> Option<(Option<String>, Option<String>, Option<String>)> {
    let body = crossagents_drivers::process::proxy_echo(url, proxy).await?;
    if body.len() > 16384 {
        return None;
    }
    let json = serde_json::from_str::<serde_json::Value>(&body).ok();
    let raw = json
        .as_ref()
        .and_then(|v| v.get("ip"))
        .and_then(|v| v.as_str())
        .unwrap_or(body.trim());
    let ip = raw.parse::<std::net::IpAddr>().ok()?.to_string();
    let asn = json
        .as_ref()
        .and_then(|v| v.get("asn"))
        .and_then(|v| v.as_str())
        .filter(|v| {
            let digits = v
                .strip_prefix("AS")
                .or_else(|| v.strip_prefix("as"))
                .unwrap_or(v);
            !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit())
                && digits.parse::<u32>().is_ok()
        })
        .map(str::to_owned);
    let country = json
        .as_ref()
        .and_then(|v| v.get("country"))
        .and_then(|v| v.as_str())
        .filter(|v| v.len() == 2 && v.bytes().all(|b| b.is_ascii_alphabetic()))
        .map(str::to_ascii_uppercase);
    Some((Some(ip), asn, country))
}

fn fingerprint_item(from: AgentId, text: String, question: bool) -> crossagents_core::HumanItem {
    crossagents_core::HumanItem {
        id: format!("egress-{}", uuid::Uuid::new_v4()),
        kind: if question {
            crossagents_core::HumanItemKind::Question
        } else {
            crossagents_core::HumanItemKind::Notice
        },
        from,
        run_id: None,
        task_id: None,
        text,
        detail: None,
        options: if question {
            vec!["允许".into(), "拒绝".into()]
        } else {
            Vec::new()
        },
        default: None,
        timeout_at: None,
        status: crossagents_core::HumanItemStatus::Open,
        answer: None,
        answered_at: None,
        action: None,
        created_at: Utc::now(),
    }
}

pub fn account_of(agent: &Agent) -> Option<String> {
    agent
        .config
        .get("account")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

fn outside(ours: bool) -> String {
    if ours {
        "(unknown CrossAgents agent)".into()
    } else {
        "outside CrossAgents".into()
    }
}

fn role_name(role: Role) -> String {
    match role {
        Role::Planner => "planner",
        Role::Worker => "worker",
        Role::Reviewer => "reviewer",
    }
    .into()
}

/// The agent behind a vendor session, from the event log.
pub(crate) struct Owner {
    pub(crate) name: String,
    pub(crate) role: Role,
    pub(crate) run: Option<RunId>,
}

async fn session_owners(store: &dyn Store) -> Result<HashMap<String, Owner>, CoreError> {
    let mut agents: HashMap<AgentId, (String, Role)> = HashMap::new();
    let mut runs: HashMap<AgentId, RunId> = HashMap::new();
    let mut sessions: Vec<(String, AgentId, Option<RunId>)> = Vec::new();
    let mut after = 0;
    loop {
        let page = store.events_since(after, 1000).await?;
        let Some(last) = page.last() else { break };
        after = last.seq;
        for event in page {
            match event.kind {
                EventKind::AgentCreated { agent } => {
                    agents.insert(agent.id, (agent.name, agent.role));
                }
                EventKind::RunCreated { run } => {
                    runs.insert(run.planner, run.id);
                }
                EventKind::TaskStatusChanged {
                    run_id,
                    assignee: Some(agent),
                    ..
                } => {
                    runs.insert(agent, run_id);
                }
                EventKind::AgentSessionStarted { id, session_id } => {
                    sessions.push((session_id, id, None));
                }
                _ => {}
            }
        }
    }
    Ok(sessions
        .into_iter()
        .filter_map(|(session_id, agent, _)| {
            let (name, role) = agents.get(&agent)?.clone();
            Some((
                session_id,
                Owner {
                    name,
                    role,
                    run: runs.get(&agent).copied(),
                },
            ))
        })
        .collect())
}

/// The lines a login tool writes to either stream, in arrival order.
fn output_lines(child: &mut tokio::process::Child) -> mpsc::UnboundedReceiver<String> {
    let (lines_tx, lines_rx) = mpsc::unbounded_channel::<String>();
    for stream in [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let tx = lines_tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }
    lines_rx
}

type Tail = Arc<std::sync::Mutex<VecDeque<String>>>;

/// Reads `lines` to the end in the background (so the tool never blocks on a full pipe),
/// keeping the last few for error reports.
fn keep_tail(mut lines: mpsc::UnboundedReceiver<String>) -> Tail {
    let tail = Tail::default();
    let kept = tail.clone();
    tokio::spawn(async move {
        while let Some(line) = lines.recv().await {
            let mut kept = kept.lock().unwrap();
            if kept.len() == 5 {
                kept.pop_front();
            }
            kept.push_back(line);
        }
    });
    tail
}

/// `:\n<last lines>`, or nothing when the tool said nothing.
fn tail_text(tail: &Tail) -> String {
    let tail = tail.lock().unwrap();
    if tail.is_empty() {
        String::new()
    } else {
        format!(":\n{}", tail.iter().cloned().collect::<Vec<_>>().join("\n"))
    }
}

/// `host:port` of an `http://` page, to see whether it answers yet.
fn page_address(page: &str) -> Option<String> {
    let rest = page.strip_prefix("http://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    authority.contains(':').then(|| authority.to_owned())
}

/// The first https URL in a line of tool output. Plain http links are local callback
/// servers (e.g. `http://localhost:1455`), not the page to open.
fn find_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let url: String = line[start..]
        .chars()
        .take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '\x27' | '<' | '>' | ')'))
        .collect();
    Some(url.trim_end_matches(['.', ',', ';']).to_owned())
}

/// On Windows a bare program name does not find `npx.cmd`; search `PATH`.
fn resolve_program(program: &str) -> PathBuf {
    let path = PathBuf::from(program);
    if !cfg!(windows) || path.extension().is_some() || path.components().count() > 1 {
        return path;
    }
    let Some(dirs) = std::env::var_os("PATH") else {
        return path;
    };
    for dir in std::env::split_paths(&dirs) {
        for ext in ["exe", "cmd", "bat"] {
            let candidate: &Path = &dir.join(program).with_extension(ext);
            if candidate.is_file() {
                return candidate.to_path_buf();
            }
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossagents_core::RateWindow;

    fn status(logged_in: bool, used: Option<f64>, note: Option<&str>) -> AccountStatus {
        AccountStatus {
            logged_in,
            auth_mode: None,
            email: None,
            plan: Some("pro".into()),
            subscription_until: None,
            rate_limits: used
                .map(|used_percent| RateWindow {
                    name: "five_hour".into(),
                    used_percent,
                    window_seconds: Some(5 * 3600),
                    resets_at: None,
                    remaining: None,
                    total: None,
                })
                .into_iter()
                .collect(),
            limit_reached: false,
            credits: None,
            limits: Vec::new(),
            reset_credits: Vec::new(),
            source: Some(DataSource::Live),
            as_of: Some(Utc::now()),
            note: note.map(str::to_owned),
        }
    }

    #[test]
    fn a_read_without_the_quota_keeps_the_last_one() {
        let statuses = Statuses::default();
        let (first, changed, _) =
            record_status(&statuses, "claude", status(true, Some(52.0), None));
        assert!(changed && first.rate_limits.len() == 1);

        // The usage call failed this time: the 52% stays, marked as the earlier read's.
        let (kept, changed, _) = record_status(
            &statuses,
            "claude",
            status(true, None, Some("no plan limits")),
        );
        assert_eq!(kept.rate_limits[0].used_percent, 52.0);
        assert_eq!(kept.source, Some(DataSource::Snapshot));
        assert_eq!(kept.as_of, first.as_of);
        assert!(kept.note.unwrap().contains("last read"));
        assert!(changed);

        // A new good read replaces it; the same figures again change nothing.
        let (newer, _, _) = record_status(&statuses, "claude", status(true, Some(60.0), None));
        assert_eq!(newer.rate_limits[0].used_percent, 60.0);
        let (_, changed, _) = record_status(&statuses, "claude", status(true, Some(60.0), None));
        assert!(!changed);

        // Logged out is believed, and the fall from `true` is a risk signal.
        let (out, _, signal) = record_status(&statuses, "claude", status(false, None, None));
        assert!(!out.logged_in && out.rate_limits.is_empty());
        assert_eq!(signal, Some(crossagents_orchestrator::risk::Signal::Logout));
    }

    #[test]
    fn finds_urls_in_tool_output() {
        assert_eq!(
            find_url("Open this link: https://auth.openai.com/oauth/authorize?x=1&y=2 to log in")
                .as_deref(),
            Some("https://auth.openai.com/oauth/authorize?x=1&y=2")
        );
        assert!(find_url("Starting local login server on http://localhost:1455.").is_none());
        assert_eq!(
            find_url("go to https://x.test/a.").as_deref(),
            Some("https://x.test/a")
        );
        assert!(find_url("no link here").is_none());
    }

    /// A login tool found as a `.cmd` runs under `cmd.exe`; stopping it must end what
    /// that started too (the leftover `codebuddy --serve` node processes of 10-30).
    #[cfg(windows)]
    #[tokio::test]
    async fn stopping_a_login_ends_what_its_wrapper_started() {
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("fake-login.cmd");
        std::fs::write(
            &tool,
            "@ping -n 60 127.0.0.1 >NUL
",
        )
        .unwrap();
        let mut command = tokio::process::Command::new(&tool);
        command.stdout(Stdio::null()).kill_on_drop(true);
        let (child, tree) = spawn_tree(&mut command).unwrap();
        let mut login = LoginProcess { child, tree };
        let deadline = Instant::now() + Duration::from_secs(10);
        while login.tree.active_processes().unwrap() < 2 {
            assert!(Instant::now() < deadline, "ping never started");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        login.kill();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !login.tree.is_empty().unwrap() {
            assert!(
                Instant::now() < deadline,
                "something of the login is still running"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[test]
    fn a_served_login_page_is_reached_at_its_host_and_port() {
        assert_eq!(
            page_address("http://localhost:8322/").as_deref(),
            Some("localhost:8322")
        );
        assert_eq!(
            page_address("http://127.0.0.1:9000/login?x=1").as_deref(),
            Some("127.0.0.1:9000")
        );
        assert!(page_address("https://x.test/").is_none());
        assert!(page_address("http://localhost/").is_none());
    }
}

#[cfg(test)]
mod login_tests {
    use super::*;
    use crossagents_core::{LoginCommand, ModelInfo, SessionUsage};
    use crossagents_store_sqlite::SqliteStore;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// A provider whose login tool prints a page and stops, as `agent login` does once the
    /// page is done. `logged_in` is what its account reports afterwards.
    struct PrintLogin {
        logged_in: bool,
        url: &'static str,
    }

    #[async_trait::async_trait]
    impl AccountProvider for PrintLogin {
        fn name(&self) -> &str {
            "cursor"
        }
        fn default_home(&self) -> Option<PathBuf> {
            None
        }
        async fn status(&self, _account: &Account) -> AccountStatus {
            AccountStatus {
                logged_in: self.logged_in,
                auth_mode: None,
                email: None,
                plan: None,
                subscription_until: None,
                rate_limits: Vec::new(),
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
            if self.url == "models-test" {
                vec![ModelInfo {
                    id: "restored".into(),
                    name: "restored".into(),
                    description: None,
                    context_window: None,
                    credit_multiplier: None,
                }]
            } else {
                Vec::new()
            }
        }
        fn login_command(&self, _account: &Account) -> LoginCommand {
            // `echo` on either system: the tool writes the link to stdout and exits.
            let command = if cfg!(windows) {
                vec![
                    "cmd.exe".into(),
                    "/c".into(),
                    format!("echo Open this page: {}", self.url),
                ]
            } else {
                vec![
                    "sh".into(),
                    "-c".into(),
                    format!("echo 'Open this page: {}'", self.url),
                ]
            };
            LoginCommand {
                command,
                env: BTreeMap::new(),
                env_remove: Vec::new(),
                serves: None,
            }
        }
        fn agent_env(&self, _account: &Account) -> Vec<(String, String)> {
            Vec::new()
        }
    }

    async fn wait_for_login(
        events: &mut tokio::sync::broadcast::Receiver<Event>,
    ) -> (Option<String>, Option<bool>) {
        let mut url = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let event = tokio::time::timeout_at(deadline, events.recv())
                .await
                .expect("the login should finish")
                .expect("the bus should stay up");
            match event.kind {
                EventKind::AccountLoginPending { url: pending, .. } => url = pending,
                EventKind::AccountLoginFinished { ok, message, .. } => {
                    if !ok {
                        eprintln!("login failed: {message:?}");
                    }
                    return (url, Some(ok));
                }
                _ => {}
            }
        }
    }

    /// A provider whose login state can be flipped between reads (and whose status reads are
    /// counted), for the login-risk wiring.
    struct FlipLogin {
        logged_in: Arc<AtomicBool>,
        reads: Arc<AtomicUsize>,
        base: PrintLogin,
    }

    impl FlipLogin {
        fn new(logged_in: bool) -> Self {
            Self {
                logged_in: Arc::new(AtomicBool::new(logged_in)),
                reads: Arc::new(AtomicUsize::new(0)),
                base: PrintLogin { logged_in, url: "" },
            }
        }

        fn set(&self, logged_in: bool) {
            self.logged_in.store(logged_in, Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl AccountProvider for FlipLogin {
        fn name(&self) -> &str {
            self.base.name()
        }
        fn default_home(&self) -> Option<PathBuf> {
            self.base.default_home()
        }
        async fn status(&self, account: &Account) -> AccountStatus {
            self.reads.fetch_add(1, Ordering::SeqCst);
            AccountStatus {
                logged_in: self.logged_in.load(Ordering::SeqCst),
                ..self.base.status(account).await
            }
        }
        async fn sessions(&self, account: &Account) -> Result<Vec<SessionUsage>, CoreError> {
            self.base.sessions(account).await
        }
        async fn models(&self, account: &Account) -> Vec<ModelInfo> {
            self.base.models(account).await
        }
        fn login_command(&self, account: &Account) -> LoginCommand {
            self.base.login_command(account)
        }
        fn agent_env(&self, account: &Account) -> Vec<(String, String)> {
            self.base.agent_env(account)
        }
    }

    async fn wait_for(mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(
                Instant::now() < deadline,
                "a background status read stalled"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn accounts_with(
        provider: PrintLogin,
    ) -> (Arc<Accounts>, Arc<EventBus>, tempfile::TempDir, Account) {
        accounts_with_any(Arc::new(provider)).await
    }

    async fn accounts_with_any(
        provider: Arc<dyn AccountProvider>,
    ) -> (Arc<Accounts>, Arc<EventBus>, tempfile::TempDir, Account) {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().await.unwrap());
        let bus = Arc::new(EventBus::new(store.clone()));
        let accounts = Arc::new(Accounts::new(
            bus.clone(),
            vec![provider],
            dir.path().join("accounts"),
        ));
        let account = Account {
            codebuddy_site: None,
            egress: None,
            id: "cursor-work".into(),
            provider: "cursor".into(),
            label: "work".into(),
            home: dir.path().join("accounts/cursor-work"),
            created_at: Utc::now(),
        };
        store.insert_account(&account).await.unwrap();
        (accounts, bus, dir, account)
    }

    #[tokio::test]
    async fn model_refresh_clears_only_the_models_returned_for_the_account() {
        let (accounts, bus, _dir, account) = accounts_with(PrintLogin {
            logged_in: true,
            url: "models-test",
        })
        .await;
        let store = bus.store();
        store
            .record_unavailable_model("cursor", &account.id, "restored", "invalid model")
            .await
            .unwrap();
        store
            .record_unavailable_model("cursor", &account.id, "still-missing", "invalid model")
            .await
            .unwrap();
        store
            .record_unavailable_model("cursor", "other", "restored", "invalid model")
            .await
            .unwrap();
        accounts.models(&account.id).await.unwrap();
        assert!(
            store
                .unavailable_model("cursor", &account.id, "restored")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .unavailable_model("cursor", &account.id, "still-missing")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .unavailable_model("cursor", "other", "restored")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_risk_hit_pauses_the_account_and_stops_its_agents() {
        let (accounts, bus, _dir, account) = accounts_with(PrintLogin {
            logged_in: true,
            url: "",
        })
        .await;
        let store = bus.store();
        let agent = Agent {
            id: AgentId::new(),
            name: "worker".into(),
            name_source: Default::default(),
            plugin: "cursor".into(),
            driver: None,
            model: None,
            role: Default::default(),
            home: _dir.path().join("worker"),
            cwd: _dir.path().join("worker"),
            config: serde_json::json!({ "account": account.id }),
            status: AgentStatus::Idle,
            created_at: Utc::now(),
            profile: Default::default(),
            topic: None,
            archived_at: None,
            node: None,
        };
        store.insert_agent(&agent).await.unwrap();
        assert!(accounts.fit(&account.id, None).await.at_risk.is_none());

        // Two risk signals trip the account-level breaker.
        accounts
            .record_risk(&account.id, crossagents_orchestrator::risk::Signal::Auth)
            .await;
        let tripped = accounts
            .record_risk(&account.id, crossagents_orchestrator::risk::Signal::Logout)
            .await;
        assert!(tripped.is_some(), "the account-level breaker trips");

        assert!(accounts.is_at_risk(&account.id));
        assert!(accounts.risk_reason(&account.id).is_some());
        // Every way of weighing it says at-risk, so routing cannot pick it.
        let fit = accounts.fit(&account.id, None).await;
        assert!(fit.at_risk.is_some());
        assert!(matches!(fit.health, Health::Exhausted(_)), "{fit:?}");
        assert!(accounts.weigh(&account.id, None).await.at_risk.is_some());

        // Every agent bound to it is stopped.
        let stopped = store
            .get_agent(&crossagents_core::AgentKey::Id(agent.id))
            .await
            .unwrap();
        assert_eq!(stopped.status, AgentStatus::Stopped);

        // No retry: a later signal records nothing and the first reason stands.
        let reason = accounts.risk_reason(&account.id);
        assert!(
            accounts
                .record_risk(
                    &account.id,
                    crossagents_orchestrator::risk::Signal::IdentityCheck,
                )
                .await
                .is_none()
        );
        assert_eq!(accounts.risk_reason(&account.id), reason);

        // Only a person re-arms it.
        accounts.clear_risk(&account.id);
        assert!(accounts.fit(&account.id, None).await.at_risk.is_none());
    }

    /// An agent bound to `account`, as `clone_agent_on` writes it.
    async fn worker_on(
        bus: &Arc<EventBus>,
        dir: &tempfile::TempDir,
        account: &Account,
        status: AgentStatus,
    ) -> Agent {
        let agent = Agent {
            id: AgentId::new(),
            name: "worker".into(),
            name_source: Default::default(),
            plugin: "cursor".into(),
            driver: None,
            model: None,
            role: Default::default(),
            home: dir.path().join("worker"),
            cwd: dir.path().join("worker"),
            config: serde_json::json!({ "account": account.id }),
            status,
            created_at: Utc::now(),
            profile: Default::default(),
            topic: None,
            archived_at: None,
            node: None,
        };
        bus.store().insert_agent(&agent).await.unwrap();
        agent
    }

    fn refused(reason: &str) -> RateLimits {
        RateLimits {
            reached: Some(reason.to_owned()),
            ..Default::default()
        }
    }

    /// The real observe entry: an agent's rate-limit report carrying the vendor's refusal.
    /// A quota refusal is not a risk signal; a human check is, and two of them pause.
    #[tokio::test]
    async fn a_human_check_in_a_rate_limit_report_pauses_through_observe() {
        let (accounts, bus, dir, account) = accounts_with(PrintLogin {
            logged_in: true,
            url: "",
        })
        .await;
        let agent = worker_on(&bus, &dir, &account, AgentStatus::Running).await;

        // "rate limit" / "overloaded" name no human check: nothing pauses, however often.
        for _ in 0..3 {
            accounts
                .observe(agent.id, &refused("rate limit exceeded, try again later"))
                .await;
        }
        assert!(!accounts.is_at_risk(&account.id));

        // One vendor refusal naming a human check is below the two-signal floor.
        accounts
            .observe(agent.id, &refused("Identity verification is required"))
            .await;
        assert!(!accounts.is_at_risk(&account.id));
        // The second pauses the account and stops the agent that reported it.
        accounts
            .observe(agent.id, &refused("Please complete the captcha"))
            .await;
        assert!(accounts.is_at_risk(&account.id));
        assert!(
            accounts
                .risk_reason(&account.id)
                .unwrap()
                .contains("captcha")
        );
        assert!(accounts.fit(&account.id, None).await.at_risk.is_some());
        assert!(accounts.weigh(&account.id, None).await.at_risk.is_some());
        assert_eq!(
            bus.store()
                .get_agent(&AgentKey::Id(agent.id))
                .await
                .unwrap()
                .status,
            AgentStatus::Stopped
        );
    }

    /// The real status-refresh path: a login that falls from true to false. It takes two
    /// falls in the window to pause, and each fall is classified where the read is stored.
    #[tokio::test]
    async fn a_login_that_falls_away_pauses_through_a_status_refresh() {
        let provider = Arc::new(FlipLogin::new(true));
        let (accounts, _bus, _dir, account) = accounts_with_any(provider.clone()).await;
        // First read caches "logged in"; there is no previous, so no signal.
        assert!(accounts.status(&account.id).await.unwrap().logged_in);

        // Falls once: one signal, below the floor.
        provider.set(false);
        accounts.refresh_status(account.clone(), provider.clone());
        wait_for(|| !accounts.statuses.lock().unwrap()[&account.id].1.logged_in).await;
        assert!(!accounts.is_at_risk(&account.id));

        // Back up, then falls again: the second signal pauses and excludes it.
        provider.set(true);
        accounts.refresh_status(account.clone(), provider.clone());
        wait_for(|| accounts.statuses.lock().unwrap()[&account.id].1.logged_in).await;
        provider.set(false);
        accounts.refresh_status(account.clone(), provider.clone());
        wait_for(|| accounts.is_at_risk(&account.id)).await;

        assert!(
            accounts
                .risk_reason(&account.id)
                .unwrap()
                .contains("login dropped")
        );
        assert!(accounts.fit(&account.id, None).await.at_risk.is_some());
        assert!(accounts.weigh(&account.id, None).await.at_risk.is_some());
    }

    /// After a hit nothing probes or retries the account: fit, weigh, status and further
    /// signals leave the provider untouched, and the pause stays until a person clears it.
    #[tokio::test]
    async fn an_at_risk_account_is_never_probed_or_retried() {
        let provider = Arc::new(FlipLogin::new(true));
        let (accounts, _bus, _dir, account) = accounts_with_any(provider.clone()).await;
        accounts
            .record_risk(&account.id, crossagents_orchestrator::risk::Signal::Auth)
            .await;
        accounts
            .record_risk(&account.id, crossagents_orchestrator::risk::Signal::Logout)
            .await;
        assert!(accounts.is_at_risk(&account.id));
        let reads = provider.reads.load(Ordering::SeqCst);

        assert!(accounts.fit(&account.id, None).await.at_risk.is_some());
        assert!(accounts.weigh(&account.id, None).await.at_risk.is_some());
        let status = accounts.status(&account.id).await.unwrap();
        assert!(!status.logged_in && status.note.is_some());
        // Another signal is ignored, not another try.
        assert!(
            accounts
                .record_risk(&account.id, crossagents_orchestrator::risk::Signal::Auth)
                .await
                .is_none()
        );
        assert_eq!(
            provider.reads.load(Ordering::SeqCst),
            reads,
            "a paused account must never be probed again"
        );

        // Only a person re-arms it.
        accounts.clear_risk(&account.id);
        assert!(!accounts.is_at_risk(&account.id));
    }

    #[tokio::test]
    async fn health_refreshes_a_refusal_without_a_running_agent() {
        let (accounts, _bus, _dir, account) = accounts_with(PrintLogin {
            logged_in: true,
            url: "",
        })
        .await;
        accounts.status(&account.id).await.unwrap();
        accounts.mark_reached(&account.id, "refused");
        {
            let mut statuses = accounts.statuses.lock().unwrap();
            statuses.get_mut(&account.id).unwrap().0 = Instant::now() - STATUS_TTL;
        }
        assert!(matches!(
            accounts.health(&account.id, None).await,
            Health::Exhausted(_)
        ));
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if accounts.health(&account.id, None).await == Health::Ok {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("background status read should clear the old refusal");
    }

    #[tokio::test]
    async fn health_uses_a_resolved_default_model() {
        let (accounts, _bus, _dir, account) = accounts_with(PrintLogin {
            logged_in: true,
            url: "",
        })
        .await;
        accounts.status(&account.id).await.unwrap();
        accounts
            .default_models
            .lock()
            .unwrap()
            .insert(account.id.clone(), "claude-sonnet-4".into());
        accounts
            .statuses
            .lock()
            .unwrap()
            .get_mut(&account.id)
            .unwrap()
            .1
            .limits = vec![crossagents_core::LimitGroup {
            id: "sonnet".into(),
            name: Some("Sonnet".into()),
            model: Some("sonnet".into()),
            windows: Vec::new(),
            reached: true,
        }];
        assert!(
            matches!(accounts.health(&account.id, None).await, Health::Exhausted(reason) if reason.contains("sonnet"))
        );
        assert_eq!(
            accounts.health(&account.id, Some("claude-opus-4")).await,
            Health::Ok
        );
    }

    /// `agent login` prints its page on stdout and keeps running; the hub hands the link to
    /// the GUI at once and says how the login ended when the tool stops.
    #[tokio::test]
    async fn a_login_prints_its_page_and_finishes() {
        let url = "https://cursor.com/loginDeepControl?challenge=abc";
        let (accounts, bus, _dir, account) = accounts_with(PrintLogin {
            logged_in: true,
            url,
        })
        .await;
        let mut events = bus.subscribe();
        let started = accounts.login(&account.id).await.unwrap();
        assert_eq!(started.url.as_deref(), Some(url));
        assert!(started.manual.is_none());
        assert!(started.command.contains("cmd.exe") || started.command.contains("sh"));
        assert_eq!(
            wait_for_login(&mut events).await,
            (Some(url.to_owned()), Some(true))
        );
    }

    /// The same tool that ends logged out ends the login as a failure, with what it said.
    #[tokio::test]
    async fn a_login_that_ends_logged_out_fails() {
        let (accounts, bus, _dir, account) = accounts_with(PrintLogin {
            logged_in: false,
            url: "https://cursor.com/loginDeepControl?challenge=def",
        })
        .await;
        let mut events = bus.subscribe();
        accounts.login(&account.id).await.unwrap();
        assert_eq!(
            wait_for_login(&mut events).await,
            (
                Some("https://cursor.com/loginDeepControl?challenge=def".to_owned()),
                Some(false)
            )
        );
    }
}

/// Whether an account has quota left.
#[derive(Debug, Clone, PartialEq)]
pub enum Health {
    /// Nothing known yet (first read still running): treated as usable.
    Unknown,
    Ok,
    /// About to run out: a window at [`LOW_PERCENT`] or more.
    Low(String),
    /// Out: a window at 100%, the vendor says the limit is reached, or no credits left.
    Exhausted(String),
}

impl Health {
    pub fn usable(&self) -> bool {
        matches!(self, Self::Unknown | Self::Ok)
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Low(why) | Self::Exhausted(why) => Some(why),
            _ => None,
        }
    }
}

/// A window this full counts as about to run out.
pub const LOW_PERCENT: f64 = 90.0;

/// How much of a window must be left, when it resets within [`EXPIRING_WITHIN`], for its
/// quota to be worth spending on more work now (magpie-lancer §2.5).
///
/// Half: below that, the quota left is about one turn, not enough to change what the run
/// plans, so a notice would only add noise. The reference example is 60% left.
pub const SPARE_PERCENT: f64 = 50.0;

/// How soon before its reset a window's leftover quota counts as about to be lost.
///
/// Two hours: long enough for a planner to read a notice and dispatch a task, short enough
/// that the notice is actionable. The reference example uses "2 hours".
pub const EXPIRING_WITHIN: chrono::Duration = chrono::Duration::hours(2);

/// What routing weighs an account by: its health, how full its fullest window is, when its
/// quota is replaced, and whether an expiry is about to waste a lot of it.
#[derive(Debug, Clone, PartialEq)]
pub struct Fit {
    pub health: Health,
    /// Percent used of the fullest window still running.
    pub fullest: Option<f64>,
    /// The name of that window, for saying why.
    pub fullest_window: Option<String>,
    /// When the running window that resets first is replaced (its unused quota is lost).
    pub resets_at: Option<DateTime<Utc>>,
    /// The vendor or an agent refused it since its last good reading: it goes last, but is
    /// still a candidate (magpie-lancer §2.2).
    pub failed: bool,
    /// A window that resets soon with much of it unused.
    pub expiring: Option<Expiring>,
    /// Why the account paused itself on a risk signal, if it did. Such an account is
    /// excluded from routing, not ranked lower (docs/ACCOUNT-RISK.md 「风控熔断」).
    pub at_risk: Option<String>,
}

impl Fit {
    pub(crate) fn unknown() -> Self {
        Self {
            health: Health::Unknown,
            fullest: None,
            fullest_window: None,
            resets_at: None,
            failed: false,
            expiring: None,
            at_risk: None,
        }
    }
}

/// The running window that resets soon with the most unused quota, if any: use it or lose
/// it (magpie-lancer §2.5).
pub(crate) fn expiring_of(windows: &[crossagents_core::RateWindow]) -> Option<Expiring> {
    let now = Utc::now();
    windows
        .iter()
        .filter(|w| {
            w.resets_at
                .is_some_and(|at| at > now && at - now <= EXPIRING_WITHIN)
                && 100.0 - w.used_percent >= SPARE_PERCENT
        })
        // Most left first: that is the quota worth planning for.
        .min_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
        .map(|w| Expiring {
            window: w.name.clone(),
            remaining_percent: 100.0 - w.used_percent,
            resets_at: w.resets_at.unwrap(),
        })
}

/// A quota window that resets soon with much of it unused: use it or lose it.
#[derive(Debug, Clone, PartialEq)]
pub struct Expiring {
    pub window: String,
    /// Percent of the window still unused.
    pub remaining_percent: f64,
    pub resets_at: DateTime<Utc>,
}

fn judge(
    windows: &[crossagents_core::RateWindow],
    reached: Option<&str>,
    credits: Option<&crossagents_core::Credits>,
) -> Health {
    if let Some(why) = reached
        && !windows_expired(windows)
    {
        return Health::Exhausted(format!("its limit is reached ({why})"));
    }
    if let Some(credits) = credits
        && !credits.unlimited
        && let Some(balance) = credits
            .balance
            .as_deref()
            .and_then(|b| b.trim().parse::<f64>().ok())
        && balance <= 0.0
    {
        return Health::Exhausted("no credits left".into());
    }
    let fullest = windows
        .iter()
        .filter(|w| w.resets_at.is_none_or(|at| at > Utc::now()))
        .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent));
    match fullest {
        Some(w) if w.used_percent >= 100.0 => {
            Health::Exhausted(format!("its {} window is used up", w.name))
        }
        Some(w) if w.used_percent >= LOW_PERCENT => Health::Low(format!(
            "its {} window is {:.0}% used",
            w.name, w.used_percent
        )),
        _ => Health::Ok,
    }
}

fn windows_expired(windows: &[crossagents_core::RateWindow]) -> bool {
    !windows.is_empty()
        && windows
            .iter()
            .all(|w| w.resets_at.is_some_and(|at| at <= Utc::now()))
}

// Claude's usage parser derives reached directly from the 100% windows. Other
// providers may report an independent refusal, so do not infer that it expired
// while any of their windows is still active or has no reset time.
fn reached_for_windows<'a>(
    reached: Option<&'a str>,
    windows: &[crossagents_core::RateWindow],
    provider: &str,
) -> Option<&'a str> {
    if provider == "claude" {
        let mut exhausted = windows
            .iter()
            .filter(|w| w.used_percent >= 100.0)
            .peekable();
        if exhausted.peek().is_some()
            && exhausted.all(|w| w.resets_at.is_some_and(|at| at <= Utc::now()))
        {
            return None;
        }
    }
    reached
}

fn matching_top_group(
    windows: &[crossagents_core::RateWindow],
    groups: &[crossagents_core::LimitGroup],
) -> Option<String> {
    if windows.is_empty() {
        return None;
    }
    let mut matches = groups.iter().filter(|g| g.windows == windows);
    let first = matches.next()?;
    matches.next().is_none().then(|| first.id.clone())
}

fn group_applies(
    group: &crossagents_core::LimitGroup,
    model: Option<&str>,
    provider: &str,
) -> bool {
    let scoped = group.model.as_deref().or_else(|| {
        ((provider == "claude" || provider == "codebuddy") && group.id != "all")
            .then_some(group.id.as_str())
    });
    let Some(scope) = scoped else {
        return true;
    };
    let Some(model) = model else {
        return true;
    };
    let scope = scope.to_ascii_lowercase();
    let model = model.to_ascii_lowercase();
    if matches!(model.as_str(), "default" | "auto" | "") {
        return true;
    }
    if scope == model {
        return true;
    }
    if matches!(scope.as_str(), "opus" | "sonnet" | "haiku" | "fable") {
        return model
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|part| part == scope);
    }
    if (provider == "claude" || provider == "codebuddy") && group.model.is_none() {
        let parts: Vec<_> = model.split(|c: char| !c.is_ascii_alphanumeric()).collect();
        return parts.contains(&scope.as_str());
    }
    false
}

#[cfg(test)]
fn judge_for_model(
    windows: &[crossagents_core::RateWindow],
    groups: &[crossagents_core::LimitGroup],
    model: Option<&str>,
    reached: Option<&str>,
    credits: Option<&crossagents_core::Credits>,
) -> Health {
    let top = matching_top_group(windows, groups);
    judge_for_model_with_top(
        windows,
        groups,
        model,
        reached,
        credits,
        top.as_deref(),
        None,
        "",
    )
}

// The health decision combines independent account and model quota inputs.
#[allow(clippy::too_many_arguments)]
fn judge_for_model_with_top(
    windows: &[crossagents_core::RateWindow],
    groups: &[crossagents_core::LimitGroup],
    model: Option<&str>,
    reached: Option<&str>,
    credits: Option<&crossagents_core::Credits>,
    top_group: Option<&str>,
    refusal: Option<&str>,
    provider: &str,
) -> Health {
    let top = top_group.and_then(|id| groups.iter().find(|g| g.id == id));
    let label = |group: &crossagents_core::LimitGroup| match &group.name {
        Some(name) if name != &group.id => format!("limit group {} ({name})", group.id),
        _ => format!("limit group {}", group.id),
    };
    let tagged = |health: Health, group: &crossagents_core::LimitGroup| match health {
        Health::Exhausted(why) => Health::Exhausted(format!("{}: {why}", label(group))),
        Health::Low(why) => Health::Low(format!("{}: {why}", label(group))),
        other => other,
    };
    if let Some(reason) = refusal {
        let health = Health::Exhausted(format!("its limit is reached ({reason})"));
        return match top {
            Some(group) => tagged(health, group),
            None => health,
        };
    }
    // Credits are account-wide, including when the last update belongs to another model.
    let mut result = judge(&[], None, credits);
    if matches!(result, Health::Exhausted(_)) {
        return result;
    }
    if top.is_none_or(|g| group_applies(g, model, provider)) {
        let health = judge(
            windows,
            reached_for_windows(reached, windows, provider),
            None,
        );
        result = match top {
            Some(group) => tagged(health, group),
            None => health,
        };
        if matches!(result, Health::Exhausted(_)) {
            return result;
        }
    }
    for group in groups.iter().filter(|g| group_applies(g, model, provider)) {
        let health = judge(
            &group.windows,
            reached_for_windows(
                group.reached.then_some("limit reached"),
                &group.windows,
                provider,
            ),
            None,
        );
        match health {
            Health::Exhausted(_) => return tagged(health, group),
            Health::Low(_) if !matches!(result, Health::Low(_)) => result = tagged(health, group),
            _ => {}
        }
    }
    result
}

#[cfg(test)]
mod health_tests {
    use super::*;
    use crossagents_core::{Credits, RateWindow};

    fn window(name: &str, used: f64) -> RateWindow {
        RateWindow {
            name: name.into(),
            used_percent: used,
            window_seconds: None,
            resets_at: None,
            remaining: None,
            total: None,
        }
    }

    #[test]
    fn expired_window_is_not_exhausted() {
        let mut old = window("primary", 100.0);
        old.resets_at = Some(Utc::now() - chrono::Duration::seconds(1));
        assert_eq!(judge(&[old], None, None), Health::Ok);
    }

    #[test]
    fn model_groups_only_affect_the_matching_model() {
        let group = crossagents_core::LimitGroup {
            id: "luna-limit".into(),
            name: None,
            model: Some("luna".into()),
            windows: vec![window("primary", 100.0)],
            reached: false,
        };
        let groups = [group];
        assert_eq!(
            judge_for_model(
                &[window("primary", 0.0)],
                &groups,
                Some("other"),
                None,
                None
            ),
            Health::Ok
        );
        assert!(
            matches!(judge_for_model(&[window("primary", 0.0)], &groups, Some("luna"), None, None), Health::Exhausted(reason) if reason.contains("luna-limit"))
        );
    }

    #[test]
    fn scoped_limits_do_not_block_unrelated_explicit_models() {
        for scope in ["fable", "opus", "sonnet", "haiku"] {
            for scoped_model in [None, Some(scope)] {
                let group = crossagents_core::LimitGroup {
                    id: scope.into(),
                    name: Some(scope.into()),
                    model: scoped_model.map(str::to_owned),
                    windows: vec![window("primary", 100.0)],
                    reached: true,
                };
                for model in ["hy3", "gpt-6"] {
                    assert_eq!(
                        judge_for_model_with_top(
                            &[],
                            std::slice::from_ref(&group),
                            Some(model),
                            None,
                            None,
                            None,
                            None,
                            "codebuddy"
                        ),
                        Health::Ok,
                        "{scope} must not block {model}"
                    );
                }
                assert!(
                    matches!(
                        judge_for_model_with_top(
                            &[],
                            std::slice::from_ref(&group),
                            Some(&format!("claude-{scope}-4")),
                            None,
                            None,
                            None,
                            None,
                            "codebuddy"
                        ),
                        Health::Exhausted(_)
                    ),
                    "matching family must remain exhausted for {scope}"
                );
                assert!(
                    matches!(
                        judge_for_model_with_top(
                            &[],
                            &[group],
                            None,
                            None,
                            None,
                            None,
                            None,
                            "codebuddy"
                        ),
                        Health::Exhausted(_)
                    ),
                    "unresolved default must remain conservative for {scope}"
                );
            }
        }
    }

    #[test]
    fn universal_and_family_limits_are_not_lost() {
        let group = |id: &str, model: Option<&str>| crossagents_core::LimitGroup {
            id: id.into(),
            name: Some(id.into()),
            model: model.map(str::to_owned),
            windows: vec![window("primary", 100.0)],
            reached: false,
        };
        for (g, model) in [
            (group("codex", None), Some("gpt-6")),
            (group("sonnet", Some("sonnet")), Some("claude-sonnet-4-5")),
            (group("fable", None), Some("fable")),
            (group("luna", Some("luna")), None),
        ] {
            assert!(matches!(
                judge_for_model(&[], &[g], model, None, None),
                Health::Exhausted(_)
            ));
        }
    }

    #[test]
    fn group_reached_and_expired_reached() {
        let g = crossagents_core::LimitGroup {
            id: "codex".into(),
            name: Some("Codex".into()),
            model: None,
            windows: vec![window("primary", 4.0)],
            reached: true,
        };
        assert!(
            matches!(judge_for_model(&[], &[g], Some("x"), None, None), Health::Exhausted(reason) if reason.contains("codex") && reason.contains("Codex"))
        );
        let mut expired = window("primary", 100.0);
        expired.resets_at = Some(Utc::now() - chrono::Duration::seconds(1));
        assert_eq!(judge(&[expired], Some("rate_limited"), None), Health::Ok);
    }

    #[test]
    fn scoped_groups_and_top_level_ownership() {
        let make = |id: &str, model: Option<&str>, used: f64, reached: bool| {
            crossagents_core::LimitGroup {
                id: id.into(),
                name: Some(format!("{id} plan")),
                model: model.map(str::to_owned),
                windows: vec![window("primary", used)],
                reached,
            }
        };
        let groups = [
            make("luna", Some("gpt-6-luna"), 100.0, true),
            make("other", Some("gpt-6"), 0.0, false),
        ];
        let top = &groups[0].windows;
        assert_eq!(
            judge_for_model_with_top(
                top,
                &groups,
                Some("gpt-6"),
                Some("rate_limited"),
                None,
                Some("luna"),
                None,
                "codex"
            ),
            Health::Ok
        );
        assert!(
            matches!(judge_for_model_with_top(top, &groups, Some("gpt-6-luna"), Some("rate_limited"), None, Some("luna"), None, "codex"), Health::Exhausted(reason) if reason.contains("luna") && reason.contains("luna plan"))
        );
        assert!(matches!(
            judge_for_model_with_top(
                top,
                &groups,
                Some("gpt-6"),
                Some("rate_limited"),
                None,
                Some("luna"),
                Some("refused"),
                "codex"
            ),
            Health::Exhausted(_)
        ));
        for provider in ["claude", "codebuddy"] {
            let group = make("fable", None, 100.0, true);
            assert!(matches!(
                judge_for_model_with_top(
                    &[],
                    std::slice::from_ref(&group),
                    Some("claude-fable-4"),
                    None,
                    None,
                    None,
                    None,
                    provider
                ),
                Health::Exhausted(_)
            ));
            assert_eq!(
                judge_for_model_with_top(
                    &[],
                    &[group],
                    Some("claude-sonnet-4"),
                    None,
                    None,
                    None,
                    None,
                    provider
                ),
                Health::Ok
            );
        }
    }

    #[test]
    fn reached_lives_until_all_known_windows_reset() {
        let mut old = window("primary", 100.0);
        old.resets_at = Some(Utc::now() - chrono::Duration::seconds(1));
        let mut group = crossagents_core::LimitGroup {
            id: "codex".into(),
            name: Some("Codex".into()),
            model: None,
            windows: vec![old.clone()],
            reached: true,
        };
        assert_eq!(
            judge_for_model(&[], &[group.clone()], Some("gpt"), None, None),
            Health::Ok
        );
        group.windows.push(window("secondary", 1.0));
        group.id = "all".into();
        assert_eq!(
            judge_for_model_with_top(
                &[],
                &[group.clone()],
                Some("gpt"),
                None,
                None,
                None,
                None,
                "claude"
            ),
            Health::Ok
        );
        assert!(matches!(
            judge_for_model(&[], &[group.clone()], Some("gpt"), None, None),
            Health::Exhausted(_)
        ));
        group.windows[1].used_percent = 100.0;
        assert!(matches!(
            judge_for_model(&[], &[group], Some("gpt"), None, None),
            Health::Exhausted(_)
        ));
        assert!(matches!(
            judge(&[window("primary", 100.0)], Some("reached"), None),
            Health::Exhausted(_)
        ));
    }
    #[test]
    fn judges_windows_limits_and_credits() {
        assert_eq!(judge(&[window("5h", 40.0)], None, None), Health::Ok);
        assert!(
            matches!(judge(&[window("5h", 40.0), window("weekly", 93.0)], None, None), Health::Low(why) if why.contains("weekly"))
        );
        assert!(matches!(
            judge(&[window("5h", 100.0)], None, None),
            Health::Exhausted(_)
        ));
        assert!(matches!(
            judge(&[], Some("rate_limited"), None),
            Health::Exhausted(_)
        ));
        let empty = Credits {
            has_credits: false,
            unlimited: false,
            balance: Some("0".into()),
        };
        assert!(matches!(
            judge(&[], None, Some(&empty)),
            Health::Exhausted(_)
        ));
        let unlimited = Credits {
            unlimited: true,
            ..empty
        };
        assert_eq!(judge(&[], None, Some(&unlimited)), Health::Ok);
    }
}

#[cfg(test)]
mod egress_tests {
    use super::*;
    use crossagents_store_sqlite::SqliteStore;

    fn observed(ip: Option<&str>, country: Option<&str>) -> crossagents_core::EgressRecord {
        crossagents_core::EgressRecord {
            account: "test".into(),
            egress_ip: ip.map(str::to_owned),
            asn: ip.map(|_| "AS1".to_owned()),
            country: country.map(str::to_owned),
            at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn new_ip_notifies_without_blocking() {
        let accounts = accounts(None, &[], &[]).await;
        accounts
            .apply_fingerprint(AgentId::new(), observed(Some("192.0.2.1"), Some("US")))
            .await
            .unwrap();
        let items = accounts.store().list_human_items(false).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, crossagents_core::HumanItemKind::Notice);
        assert_eq!(
            accounts
                .store()
                .list_egress_records("test")
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn changed_country_waits_for_explicit_confirmation() {
        let accounts = accounts(None, &[], &[]).await;
        let agent = AgentId::new();
        accounts
            .apply_fingerprint(agent, observed(Some("192.0.2.1"), Some("US")))
            .await
            .unwrap();
        assert!(
            accounts
                .apply_fingerprint(agent, observed(Some("192.0.2.2"), Some("GB")))
                .await
                .is_err()
        );
        // What the person sees on the card: held, and why. The hub says it, not the notice.
        let block = accounts
            .egress_block("test")
            .expect("the block is recorded");
        assert!(block.contains("US"), "{block}");
        assert!(block.contains("GB"), "{block}");
        let mut question = accounts
            .store()
            .list_human_items(false)
            .await
            .unwrap()
            .into_iter()
            .find(|i| i.kind == crossagents_core::HumanItemKind::Question)
            .unwrap();
        question.status = crossagents_core::HumanItemStatus::Answered;
        question.answer = Some("允许".into());
        accounts.store().put_human_item(&question).await.unwrap();
        accounts
            .apply_fingerprint(agent, observed(Some("192.0.2.2"), Some("GB")))
            .await
            .unwrap();
        // Approved: the next start goes through, so the card stops saying it is held.
        assert_eq!(accounts.egress_block("test"), None);
        assert_eq!(
            accounts
                .store()
                .list_egress_records("test")
                .await
                .unwrap()
                .last()
                .unwrap()
                .country
                .as_deref(),
            Some("GB")
        );
    }

    #[tokio::test]
    async fn unavailable_fingerprint_does_not_block() {
        let accounts = accounts(None, &[], &[]).await;
        accounts
            .apply_fingerprint(AgentId::new(), observed(None, None))
            .await
            .unwrap();
        assert!(
            accounts
                .store()
                .list_human_items(false)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            lookup_fingerprint("http://example.invalid", "http://127.0.0.1:1").await,
            None
        );
    }

    #[tokio::test]
    async fn unavailable_fingerprint_preserves_last_known_observation() {
        let accounts = accounts(None, &[], &[]).await;
        let agent = AgentId::new();
        accounts
            .apply_fingerprint(agent, observed(Some("192.0.2.1"), Some("US")))
            .await
            .unwrap();
        accounts
            .apply_fingerprint(agent, observed(None, None))
            .await
            .unwrap();

        let fingerprint = accounts.fingerprint("test").await.unwrap();
        assert_eq!(fingerprint.ip.as_deref(), Some("192.0.2.1"));
        assert_eq!(fingerprint.country.as_deref(), Some("US"));
        assert_eq!(
            accounts
                .store()
                .list_egress_records("test")
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn missing_asn_does_not_turn_a_country_lookup_into_a_block_or_baseline() {
        let accounts = accounts(None, &[], &[]).await;
        let agent = AgentId::new();
        accounts
            .apply_fingerprint(agent, observed(Some("192.0.2.1"), Some("US")))
            .await
            .unwrap();
        let mut incomplete = observed(Some("192.0.2.2"), Some("GB"));
        incomplete.asn = None;
        accounts.apply_fingerprint(agent, incomplete).await.unwrap();
        assert!(
            accounts
                .apply_fingerprint(agent, observed(Some("192.0.2.2"), Some("GB")))
                .await
                .is_err()
        );
    }

    fn account(provider: &str, id: &str, egress: Option<EgressPolicy>) -> Account {
        Account {
            codebuddy_site: None,
            egress,
            id: id.into(),
            provider: provider.into(),
            label: String::new(),
            home: PathBuf::from("/accounts").join(id),
            created_at: Utc::now(),
        }
    }

    async fn accounts(
        default: Option<EgressPolicy>,
        by_provider: &[(&str, EgressPolicy)],
        by_account: &[(&str, EgressPolicy)],
    ) -> Accounts {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().await.unwrap());
        let bus = Arc::new(EventBus::new(store));
        let map = |pairs: &[(&str, EgressPolicy)]| {
            pairs
                .iter()
                .map(|(key, policy)| ((*key).to_owned(), *policy))
                .collect()
        };
        Accounts::new(bus, Vec::new(), PathBuf::from("/accounts")).with_egress(
            default,
            map(by_provider),
            map(by_account),
        )
    }

    #[tokio::test]
    async fn the_account_then_the_config_then_the_provider_decides() {
        // No override: the provider default, and the account's own choice when it has one.
        let plain = accounts(None, &[], &[]).await;
        assert_eq!(
            plain.egress_for(&account("claude", "claude-work", None)),
            EgressPolicy::ProxyOnly
        );
        assert_eq!(
            plain.egress_for(&account("codebuddy", "buddy", None)),
            EgressPolicy::AllowDirect
        );
        assert_eq!(
            plain.egress_for(&account(
                "claude",
                "claude-work",
                Some(EgressPolicy::AllowDirect)
            )),
            EgressPolicy::AllowDirect
        );
        // The finer key wins: the per-account override beats everything, and a provider
        // table does not overturn an account's explicit choice.
        let overridden = accounts(
            Some(EgressPolicy::ProxyOnly),
            &[("claude", EgressPolicy::AllowDirect)],
            &[("claude-work", EgressPolicy::ProxyOnly)],
        )
        .await;
        assert_eq!(
            overridden.egress_for(&account("claude", "claude-work", None)),
            EgressPolicy::ProxyOnly
        );
        assert_eq!(
            overridden.egress_for(&account(
                "claude",
                "claude-other",
                Some(EgressPolicy::ProxyOnly)
            )),
            EgressPolicy::ProxyOnly
        );
        assert_eq!(
            overridden.egress_for(&account("claude", "claude-third", None)),
            EgressPolicy::AllowDirect
        );
        assert_eq!(
            overridden.egress_for(&account("codebuddy", "buddy", None)),
            EgressPolicy::ProxyOnly
        );
        // The variable an agent gets carries the resolved value, not the raw field.
        assert_eq!(
            overridden
                .egress_for(&account("codebuddy", "buddy", None))
                .as_str(),
            "proxy_only"
        );
    }
}
