//! Qoder CLI accounts: one config directory per login (`QODER_CONFIG_DIR`; the default is
//! `~/.qoder`). Login state, email and plan come from `qodercli status -o json`; available
//! models from `qodercli --list-models`; live quota from `get_usage_info` over stream-json;
//! token usage from session transcripts under `projects/*/*.jsonl`. Login runs `qodercli login`
//! with `NO_BROWSER=1`, which prints the browser OAuth URL to stdout and waits until auth
//! finishes.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crossagents_core::{
    Account, AccountProvider, AccountStatus, CoreError, DataSource, LoginCommand, ModelInfo,
    RequestUsage, SessionUsage, TokenTotals, mask_email,
};
use crossagents_drivers::claude_native::wire;
use crossagents_drivers::locate::Cli;
use crossagents_drivers::process::system_proxy_env;
use crossagents_drivers::qoder_native;
use serde_json::Value;

/// Live status and quota are asked at most once a minute per account.
const LIVE_TTL: Duration = Duration::from_secs(60);

type ScanEntry = (SystemTime, u64, Option<SessionUsage>);
type LiveEntry = (Instant, AccountStatus, Vec<ModelInfo>);

enum CliLookup {
    /// Look up `qodercli` on demand so an install while the hub runs is picked up.
    Live(Option<Cli>),
    /// Fixed command (or `None` to simulate an uninstalled CLI) for tests.
    Fixed(Option<Vec<String>>),
}

pub struct QoderAccounts {
    command: CliLookup,
    live: Mutex<HashMap<PathBuf, LiveEntry>>,
    scanned: Mutex<HashMap<PathBuf, ScanEntry>>,
}

impl Default for QoderAccounts {
    fn default() -> Self {
        Self {
            command: CliLookup::Live(crate::builtin::cli("qoder")),
            live: Mutex::default(),
            scanned: Mutex::default(),
        }
    }
}

impl QoderAccounts {
    pub fn new(command: Option<Vec<String>>) -> Self {
        Self {
            command: CliLookup::Fixed(command),
            live: Mutex::default(),
            scanned: Mutex::default(),
        }
    }

    fn command(&self) -> Option<Vec<String>> {
        match &self.command {
            CliLookup::Live(cli) => cli.as_ref().and_then(Cli::command),
            CliLookup::Fixed(command) => command.clone(),
        }
    }

    /// `QODER_CONFIG_DIR` for non-default accounts; empty for `~/.qoder`.
    fn env_for(&self, account: &Account) -> Vec<(String, String)> {
        if self
            .default_home()
            .is_some_and(|home| same_path(&home, &account.home))
        {
            return Vec::new();
        }
        vec![(
            "QODER_CONFIG_DIR".to_owned(),
            account.home.display().to_string(),
        )]
    }

    async fn read_status(&self, account: &Account) -> (AccountStatus, Vec<ModelInfo>) {
        let mut status = AccountStatus {
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
            source: None,
            as_of: None,
            note: None,
        };
        let Some(command) = self.command() else {
            status.note = Some(
                "the Qoder CLI is not installed (npm install -g @qoder-ai/qodercli)".into(),
            );
            return (status, Vec::new());
        };
        let env = self.env_for(account);
        let Some(auth) = qoder_native::auth_status(&command, &env).await else {
            status.note = Some("cannot run qodercli status".into());
            return (status, Vec::new());
        };
        apply_auth_status(&mut status, &auth);
        if !status.logged_in {
            return (status, Vec::new());
        }
        let (models_res, usage_res) = tokio::join!(
            qoder_native::list_models(&command, &env),
            qoder_native::probe_usage(&command, &env),
        );
        let models = match models_res {
            Ok(list) => list
                .into_iter()
                .map(|(id, name)| ModelInfo {
                    id,
                    name,
                    description: None,
                    context_window: None,
                    credit_multiplier: None,
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        match usage_res {
            Ok(usage) => {
                if let Some(limits) = wire::qoder_usage_limits(&usage) {
                    status.rate_limits = limits.windows;
                    status.limit_reached = limits.reached.is_some();
                    status.credits = limits.credits;
                    status.limits = limits.by_limit;
                    status.plan = status.plan.or(limits.plan);
                } else if let Some(err) = usage.get("usage_error").and_then(Value::as_str) {
                    status.note = Some(err.to_owned());
                }
                if status.subscription_until.is_none() {
                    status.subscription_until = wire::qoder_subscription_until(&usage);
                }
                status.source = Some(DataSource::Live);
                status.as_of = Some(Utc::now());
            }
            Err(err) => {
                status.note = Some(format!("cannot read quota: {err}"));
            }
        }
        (status, models)
    }
}

fn apply_auth_status(status: &mut AccountStatus, auth: &Value) {
    let text = |key: &str| {
        auth.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    status.logged_in = auth.get("logged_in").and_then(Value::as_bool) == Some(true);
    status.auth_mode = text("login_method")
        .or_else(|| text("auth_source"))
        .or_else(|| text("account_type"))
        .map(str::to_owned);
    status.email = text("email").map(mask_email);
    status.plan = text("plan")
        .or_else(|| text("user_type"))
        .map(str::to_owned);
    status.source = Some(DataSource::Live);
    status.as_of = Some(Utc::now());
    if !status.logged_in {
        status.note = Some(
            text("auth_error_message")
                .or_else(|| text("auth_error_reason"))
                .unwrap_or("not logged in")
                .to_owned(),
        );
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

/// Transcripts: `<home>/projects/<directory>/<session id>.jsonl`.
fn transcript_files(home: &Path) -> Vec<PathBuf> {
    let Ok(projects) = std::fs::read_dir(home.join("projects")) else {
        return Vec::new();
    };
    projects
        .flatten()
        .filter_map(|dir| std::fs::read_dir(dir.path()).ok())
        .flat_map(|files| files.flatten().map(|f| f.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect()
}

type Reply = (Option<DateTime<Utc>>, Option<String>, Value);

fn parse_timestamp(value: Option<&Value>) -> Option<DateTime<Utc>> {
    match value {
        Some(Value::String(s)) => s.parse().ok(),
        Some(v) => v.as_i64().and_then(|n| {
            if n > 1_000_000_000_000 {
                DateTime::from_timestamp_millis(n)
            } else {
                DateTime::from_timestamp(n, 0)
            }
        }),
        None => None,
    }
}

/// Token use of one Qoder session transcript. Each assistant message may appear on multiple
/// lines; the last entry per `message.id` carries its complete usage.
fn scan_transcript(path: &Path) -> Option<SessionUsage> {
    let file = std::fs::File::open(path).ok()?;
    let mut started_at: Option<DateTime<Utc>> = None;
    let mut model = None;
    let mut originator = None;
    let mut replies: HashMap<String, Reply> = HashMap::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let at = parse_timestamp(entry.get("timestamp"));
        if started_at.is_none() {
            started_at = at;
        }
        if originator.is_none() {
            originator = entry
                .get("entrypoint")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        if entry.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let message = entry.get("message").unwrap_or(&Value::Null);
        let asked = message
            .get("model")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty() && !m.starts_with('<'))
            .map(str::to_owned);
        if model.is_none() {
            model = asked.clone();
        }
        if let (Some(id), Some(usage)) = (
            message.get("id").and_then(Value::as_str),
            message.get("usage"),
        ) {
            replies.insert(id.to_owned(), (at, asked, usage.clone()));
        }
    }
    if replies.is_empty() {
        return None;
    }
    let mut totals = TokenTotals {
        sessions: 1,
        ..TokenTotals::default()
    };
    let mut requests = Vec::with_capacity(replies.len());
    for (at, asked, usage) in replies.into_values() {
        let (input, cached, _cache_write) = wire::qoder_input_tokens(&usage);
        let output = usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if input == 0 && output == 0 {
            continue;
        }
        let reasoning = usage
            .pointer("/output_tokens_details/thinking_tokens")
            .or_else(|| usage.pointer("/completion_tokens_details/reasoning_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let credits = usage.get("credits").and_then(Value::as_f64).unwrap_or(0.0);
        let call = TokenTotals {
            sessions: 0,
            input_tokens: input,
            cached_input_tokens: cached,
            output_tokens: output,
            reasoning_output_tokens: reasoning,
            credits,
        };
        totals.input_tokens += call.input_tokens;
        totals.cached_input_tokens += call.cached_input_tokens;
        totals.output_tokens += call.output_tokens;
        totals.reasoning_output_tokens += call.reasoning_output_tokens;
        totals.credits += call.credits;
        requests.push(RequestUsage {
            at: at.or(started_at).unwrap_or_default(),
            model: asked.clone().or_else(|| model.clone()),
            totals: call,
        });
    }
    if requests.is_empty() {
        return None;
    }
    requests.sort_by_key(|r| r.at);
    Some(SessionUsage {
        session_id: path.file_stem()?.to_string_lossy().into_owned(),
        started_at: started_at.unwrap_or_default(),
        model,
        originator,
        totals,
        requests,
    })
}

#[async_trait]
impl AccountProvider for QoderAccounts {
    fn name(&self) -> &str {
        "qoder"
    }

    fn default_home(&self) -> Option<PathBuf> {
        qoder_native::default_home()
    }

    async fn status(&self, account: &Account) -> AccountStatus {
        if let Some((at, status, _)) = self.live.lock().unwrap().get(&account.home)
            && at.elapsed() < LIVE_TTL
        {
            return status.clone();
        }
        let (status, models) = self.read_status(account).await;
        self.live.lock().unwrap().insert(
            account.home.clone(),
            (Instant::now(), status.clone(), models),
        );
        status
    }

    async fn sessions(&self, account: &Account) -> Result<Vec<SessionUsage>, CoreError> {
        let files = transcript_files(&account.home);
        let mut result = Vec::with_capacity(files.len());
        let mut to_scan = Vec::new();
        {
            let cache = self.scanned.lock().unwrap();
            for path in files {
                let Ok(meta) = path.metadata() else { continue };
                let key = (
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    meta.len(),
                );
                match cache.get(&path) {
                    Some((m, l, usage)) if (*m, *l) == key => result.extend(usage.clone()),
                    _ => to_scan.push((path, key)),
                }
            }
        }
        let scanned = tokio::task::spawn_blocking(move || {
            to_scan
                .into_iter()
                .map(|(path, key)| {
                    let usage = scan_transcript(&path);
                    (path, key, usage)
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|e| CoreError::Storage(format!("cannot scan Qoder transcripts: {e}")))?;
        let mut cache = self.scanned.lock().unwrap();
        for (path, (modified, len), usage) in scanned {
            result.extend(usage.clone());
            cache.insert(path, (modified, len, usage));
        }
        result.sort_by_key(|s| std::cmp::Reverse(s.started_at));
        Ok(result)
    }

    async fn models(&self, account: &Account) -> Vec<ModelInfo> {
        if !self.live.lock().unwrap().contains_key(&account.home) {
            self.status(account).await;
        }
        self.live
            .lock()
            .unwrap()
            .get(&account.home)
            .map(|(_, _, models)| models.clone())
            .unwrap_or_default()
    }

    /// Runs `qodercli login` with `NO_BROWSER=1`: the CLI prints the OAuth URL to stdout
    /// (which the hub extracts for the GUI) and exits 0 once browser login finishes.
    fn login_command(&self, account: &Account) -> LoginCommand {
        let mut command = self.command().unwrap_or_else(|| vec!["qodercli".into()]);
        command.push("login".into());
        let mut env: Vec<_> = system_proxy_env()
            .into_iter()
            .chain(self.env_for(account))
            .chain([("NO_BROWSER".to_owned(), "1".to_owned())])
            .collect();
        let env_remove = qoder_native::prepare_env(&mut env);
        LoginCommand {
            command,
            env: env.into_iter().collect::<BTreeMap<_, _>>(),
            env_remove,
            serves: None,
            opens_browser: false,
        }
    }

    fn forget_status(&self, account: &Account) {
        self.live.lock().unwrap().remove(&account.home);
    }

    fn agent_env(&self, account: &Account) -> Vec<(String, String)> {
        self.env_for(account)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn account(home: PathBuf) -> Account {
        Account {
            id: "qoder".into(),
            provider: "qoder".into(),
            label: String::new(),
            home,
            codebuddy_site: None,
            egress: None,
            proxy: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn default_home_keeps_qoder_config_dir_unset_and_login_sets_no_browser() {
        let provider = QoderAccounts::new(Some(vec!["qodercli".into()]));
        if let Some(default) = provider.default_home() {
            assert!(provider.agent_env(&account(default)).is_empty());
        }
        let other = account(PathBuf::from("/accounts/qoder-work"));
        assert_eq!(
            provider.agent_env(&other),
            [(
                "QODER_CONFIG_DIR".to_owned(),
                other.home.display().to_string()
            )]
        );
        let login = provider.login_command(&other);
        assert_eq!(login.command, ["qodercli", "login"]);
        assert_eq!(login.env.get("NO_BROWSER").map(String::as_str), Some("1"));
        assert_eq!(
            login.env.get("QODER_CONFIG_DIR").map(String::as_str),
            Some(other.home.display().to_string().as_str())
        );
        assert!(login.serves.is_none());
        assert!(!login.opens_browser);
    }

    #[test]
    fn auth_status_parses_signed_in_and_signed_out_json() {
        let mut status = AccountStatus {
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
            source: None,
            as_of: None,
            note: None,
        };
        apply_auth_status(
            &mut status,
            &json!({
                "logged_in": true,
                "version": "1.1.99",
                "auth_source": "oauth",
                "account_type": "user",
                "login_method": "browser",
                "username": "alice",
                "email": "alice@example.com",
                "user_type": "pro",
                "plan": "Pro",
            }),
        );
        assert!(status.logged_in);
        assert_eq!(status.auth_mode.as_deref(), Some("browser"));
        assert_eq!(status.email.as_deref(), Some("a***@example.com"));
        assert_eq!(status.plan.as_deref(), Some("Pro"));
        assert!(status.note.is_none());

        let mut signed_out = AccountStatus {
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
            source: None,
            as_of: None,
            note: None,
        };
        apply_auth_status(
            &mut signed_out,
            &json!({
                "logged_in": false,
                "version": "1.1.99",
                "auth_error_reason": "not_authenticated",
                "auth_error_message": "Please run /login",
            }),
        );
        assert!(!signed_out.logged_in);
        assert_eq!(signed_out.note.as_deref(), Some("Please run /login"));
    }

    #[test]
    fn scans_transcript_deduping_replies_for_gateway_and_byok_usage() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("projects").join("C--work");
        std::fs::create_dir_all(&dir).unwrap();
        // Gateway style (input_tokens includes cached) and Anthropic BYOK style (cache > input).
        let text = [
            json!({ "type": "user", "timestamp": "2026-11-20T08:00:00Z", "entrypoint": "cli" })
                .to_string(),
            json!({ "type": "assistant", "timestamp": "2026-11-20T08:00:02Z",
                "message": { "id": "m1", "model": "auto", "usage": {
                    "input_tokens": 1000, "cache_read_input_tokens": 400, "output_tokens": 10 } } })
            .to_string(),
            json!({ "type": "assistant", "timestamp": "2026-11-20T08:00:03Z",
                "message": { "id": "m1", "model": "auto", "usage": {
                    "input_tokens": 1000, "cache_read_input_tokens": 400, "output_tokens": 25,
                    "credits": 0.5 } } })
            .to_string(),
            json!({ "type": "assistant", "timestamp": "2026-11-20T08:00:10Z",
                "message": { "id": "m2", "model": "claude-sonnet-4-5", "usage": {
                    "input_tokens": 12, "cache_read_input_tokens": 1200, "cache_creation_input_tokens": 88,
                    "output_tokens": 30, "output_tokens_details": { "thinking_tokens": 10 } } } })
            .to_string(),
        ]
        .join("\n");
        std::fs::write(dir.join("s1.jsonl"), text).unwrap();
        std::fs::write(dir.join("empty.jsonl"), "{\"type\":\"user\"}\n").unwrap();

        let usage = scan_transcript(&dir.join("s1.jsonl")).unwrap();
        assert_eq!(usage.session_id, "s1");
        assert_eq!(usage.originator.as_deref(), Some("cli"));
        assert_eq!(usage.requests.len(), 2);
        // m1: 1000 input (400 cached), 25 out; m2: 12+1200+88=1300 input (1200 cached), 30 out
        assert_eq!(usage.totals.input_tokens, 2300);
        assert_eq!(usage.totals.cached_input_tokens, 1600);
        assert_eq!(usage.totals.output_tokens, 55);
        assert_eq!(usage.totals.reasoning_output_tokens, 10);
        assert!((usage.totals.credits - 0.5).abs() < 1e-9);
        assert!(scan_transcript(&dir.join("empty.jsonl")).is_none());
    }
}
