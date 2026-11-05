//! Account management (docs/tasks/M5.md): accounts, live status, token usage and login.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crossagents_core::process::{NoWindow, Tree, spawn_tree};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use crossagents_api_types::{
    AccountUsageGroup, AccountUsageResponse, AccountUsageRow, CreateAccountRequest, LoginStarted,
    UsageSeriesGroup, UsageSeriesResponse,
};
use crossagents_core::{
    Account, AccountProvider, AccountStatus, Agent, AgentId, AgentKey, CoreError, DataSource,
    Event, EventBus, EventKind, RateLimits, Role, RunId, Store, TokenTotals, validate_slug,
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
}

impl Accounts {
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
        }
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
                record_status(&self.statuses, id, fresh).0
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
        let Ok(account) = self.store().get_account(id).await else {
            return Health::Unknown;
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
            return Health::Unknown;
        }
        let default_model = self.default_models.lock().unwrap().get(id).cloned();
        let model = model
            .filter(|m| !matches!(*m, "" | "default" | "auto"))
            .or(default_model.as_deref());
        judge_for_model_with_top(
            &windows,
            &groups,
            model,
            reached.as_deref(),
            credits.as_ref(),
            top_group.as_deref(),
            refusal.as_deref(),
            &account.provider,
        )
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

    /// Health checks may be frequent; limit their background probes even after a failure.
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
        tokio::spawn(async move {
            let fresh = provider.status(&account).await;
            let (_, changed) = record_status(&statuses, &account.id, fresh);
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
        Ok(self.provider(&account.provider)?.models(&account).await)
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
    pub async fn agent_env(&self, agent: &Agent) -> Result<Vec<(String, String)>, CoreError> {
        let Some(id) = account_of(agent) else {
            return Ok(Vec::new());
        };
        let account = self.store().get_account(&id).await?;
        Ok(self.provider(&account.provider)?.agent_env(&account))
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

/// Stores a fresh read of account `id` (see [`keep_last_quota`]). Returns what is now
/// served, and whether it differs from what was (apart from when it was read).
fn record_status(statuses: &Statuses, id: &str, fresh: AccountStatus) -> (AccountStatus, bool) {
    let mut statuses = statuses.lock().unwrap();
    let previous = statuses.get(id).map(|(_, status)| status.clone());
    let status = keep_last_quota(previous.as_ref(), fresh);
    let changed = previous.is_none_or(|old| {
        let undated = |s: &AccountStatus| AccountStatus {
            as_of: None,
            ..s.clone()
        };
        undated(&old) != undated(&status)
    });
    statuses.insert(id.to_owned(), (Instant::now(), status.clone()));
    (status, changed)
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
        let (first, changed) = record_status(&statuses, "claude", status(true, Some(52.0), None));
        assert!(changed && first.rate_limits.len() == 1);

        // The usage call failed this time: the 52% stays, marked as the earlier read's.
        let (kept, changed) = record_status(
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
        let (newer, _) = record_status(&statuses, "claude", status(true, Some(60.0), None));
        assert_eq!(newer.rate_limits[0].used_percent, 60.0);
        let (_, changed) = record_status(&statuses, "claude", status(true, Some(60.0), None));
        assert!(!changed);

        // Logged out is believed.
        let (out, _) = record_status(&statuses, "claude", status(false, None, None));
        assert!(!out.logged_in && out.rate_limits.is_empty());
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
            Vec::new()
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

    async fn accounts_with(
        provider: PrintLogin,
    ) -> (Arc<Accounts>, Arc<EventBus>, tempfile::TempDir, Account) {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().await.unwrap());
        let bus = Arc::new(EventBus::new(store.clone()));
        let accounts = Arc::new(Accounts::new(
            bus.clone(),
            vec![Arc::new(provider)],
            dir.path().join("accounts"),
        ));
        let account = Account {
            codebuddy_site: None,
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
    if matches!(scope.as_str(), "opus" | "sonnet" | "haiku") {
        let parts: Vec<_> = model.split(|c: char| !c.is_ascii_alphanumeric()).collect();
        return parts.contains(&scope.as_str())
            || !parts
                .iter()
                .any(|part| matches!(*part, "opus" | "sonnet" | "haiku" | "fable" | "gpt"));
    }
    if (provider == "claude" || provider == "codebuddy") && group.model.is_none() {
        let parts: Vec<_> = model.split(|c: char| !c.is_ascii_alphanumeric()).collect();
        if parts.contains(&scope.as_str()) {
            return true;
        }
        // A display name is not always a model slug. Only rule it out when
        // both names identify distinct, known families.
        let families = ["opus", "sonnet", "haiku", "fable"];
        return !(families.contains(&scope.as_str())
            && parts.iter().any(|part| families.contains(part)));
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
                    &[group.clone()],
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
