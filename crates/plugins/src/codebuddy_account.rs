//! CodeBuddy accounts. The CLI keeps one login per user of the machine (under the user's
//! `AppData`, shared with the CodeBuddy IDE extension), so the account is that login;
//! `CODEBUDDY_CONFIG_DIR` (the default is `~/.codebuddy`) only moves settings and
//! transcripts. Login state and models come from the CLI's `initialize` answer, token use
//! and the credits each call cost from its transcripts under `projects/`. CodeBuddy bills in
//! credits: the plan and what is left come from the billing summary WorkBuddy shows (asked
//! with the login's token, which goes nowhere else), each model's multiplier from the model
//! list the CLI caches. Its CLI logs in only inside its TUI, so a login serves the CLI's web
//! UI locally (`--serve`) and the login is done on that page.
//!
//! Explicit site accounts additionally move the child process's user-data profile, so
//! mainland and international credentials cannot overwrite the machine's shared login.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crossagents_core::{
    Account, AccountProvider, AccountStatus, CodebuddySite, CoreError, DataSource, LoginCommand,
    ModelInfo, RateWindow, RequestUsage, SessionUsage, TokenTotals,
};
use crossagents_drivers::codebuddy_native;
use crossagents_drivers::process::system_proxy_env;
use serde_json::Value;

/// The CLI is asked at most this often per account.
const LIVE_TTL: Duration = Duration::from_secs(60);

/// Where the billing summary is asked, in order: the API of CodeBuddy logins in mainland
/// China and its fallback (the CLI's `CN_SAAS_CORE_API_*` origins). WorkBuddy calls the same
/// path with the same login.
const BILLING_ORIGINS: [&str; 2] = ["https://www.workbuddy.cn", "https://copilot.tencent.com"];
const BILLING_PATH: &str = "/billing/meter/get-user-resource-summary";
const BILLING_TIMEOUT: Duration = Duration::from_secs(15);

type ScanEntry = (SystemTime, u64, Option<SessionUsage>);
/// A status read, with when and the models listed alongside.
type LiveEntry = (Instant, AccountStatus, Vec<ModelInfo>);

pub struct CodebuddyAccounts {
    /// How to run the CLI; `None` when it is not installed.
    command: Option<Vec<String>>,
    live: Mutex<HashMap<PathBuf, LiveEntry>>,
    scanned: Mutex<HashMap<PathBuf, ScanEntry>>,
}

impl Default for CodebuddyAccounts {
    fn default() -> Self {
        Self::new(crate::builtin::cli("codebuddy").and_then(|cli| cli.command()))
    }
}

impl CodebuddyAccounts {
    pub fn new(command: Option<Vec<String>>) -> Self {
        Self {
            command,
            live: Mutex::default(),
            scanned: Mutex::default(),
        }
    }

    /// `CODEBUDDY_CONFIG_DIR` for an account, except for the default directory.
    fn env_for(&self, account: &Account) -> Vec<(String, String)> {
        if let Some(site) = account.codebuddy_site {
            let profile = account.home.join("profile");
            // The CLI's auth lives in CodeBuddyExtension outside CONFIG_DIR. Move the
            // child process's user profile too, without changing the hub's environment.
            return vec![
                (
                    "CODEBUDDY_CONFIG_DIR".into(),
                    account.home.display().to_string(),
                ),
                ("USERPROFILE".into(), profile.display().to_string()),
                ("HOME".into(), profile.display().to_string()),
                (
                    "APPDATA".into(),
                    profile.join("AppData/Roaming").display().to_string(),
                ),
                (
                    "LOCALAPPDATA".into(),
                    profile.join("AppData/Local").display().to_string(),
                ),
                (
                    "XDG_DATA_HOME".into(),
                    profile.join(".local/share").display().to_string(),
                ),
                (
                    "XDG_CONFIG_HOME".into(),
                    profile.join(".config").display().to_string(),
                ),
                (
                    "CODEBUDDY_INTERNET_ENVIRONMENT".into(),
                    site.environment().into(),
                ),
                (
                    "CODEBUDDY_COPILOT_INTERNET_ENVIRONMENT".into(),
                    site.environment().into(),
                ),
                // Empty overrides are removal markers; prepare_env turns them into
                // env_remove so a parent WorkBuddy/API login cannot win over this login.
                ("CODEBUDDY_API_KEY".into(), String::new()),
                ("CODEBUDDY_AUTH_TOKEN".into(), String::new()),
            ];
        }
        if self
            .default_home()
            .is_some_and(|home| same_path(&home, &account.home))
        {
            return Vec::new();
        }
        vec![(
            "CODEBUDDY_CONFIG_DIR".to_owned(),
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
        let Some(command) = &self.command else {
            status.note = Some(
                "the CodeBuddy CLI is not installed (npm install -g @tencent-ai/codebuddy-code)"
                    .into(),
            );
            return (status, Vec::new());
        };
        let init = match codebuddy_native::probe(command, &self.env_for(account)).await {
            Ok(init) => init,
            Err(err) => {
                status.note = Some(format!("cannot ask codebuddy: {err}"));
                return (status, Vec::new());
            }
        };
        read_initialize(
            &mut status,
            &init,
            account.codebuddy_site.is_none() && !self.env_for(account).is_empty(),
        );
        let mut models = models_of(&init);
        let login = init.get("account").filter(|a| a.is_object());
        if let Some(uid) = login.and_then(|a| a.get("uid")).and_then(Value::as_str) {
            let home = account.home.clone();
            let uid = uid.to_owned();
            let rates = tokio::task::spawn_blocking(move || multipliers(&home, &uid))
                .await
                .unwrap_or_default();
            for model in &mut models {
                model.credit_multiplier = rates.get(&model.id).copied();
            }
        }
        if account.codebuddy_site == Some(CodebuddySite::International) && status.logged_in {
            // Do not send an international credential to the mainland billing API.
            status.note = Some("International CodeBuddy login and models are available; its billing endpoint is not integrated".into());
        } else if account.codebuddy_site != Some(CodebuddySite::International)
            && let Some((token, uid)) =
                login.and_then(|a| Some((a.get("token")?.as_str()?, a.get("uid")?.as_str()?)))
        {
            match billing_summary(token, uid).await {
                Ok(summary) => read_summary(&mut status, &summary),
                Err(err) => status.note = Some(format!("cannot read credits: {err}")),
            }
        }
        (status, models)
    }
}

/// The login's billing summary (`data` of the answer). The token goes into this request's
/// header and nowhere else; errors name the origin, never the token.
async fn billing_summary(token: &str, uid: &str) -> Result<Value, String> {
    let http = reqwest::Client::builder()
        .timeout(BILLING_TIMEOUT)
        // Without one the API answers "请求不合法" (code 10085).
        .user_agent(concat!("CrossAgents/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let mut failures = Vec::new();
    for origin in BILLING_ORIGINS {
        let answer = http
            .post(format!("{origin}{BILLING_PATH}"))
            .bearer_auth(token)
            .header("X-User-Id", uid)
            .header("Accept-Language", "zh")
            .json(&serde_json::json!({}))
            .send()
            .await;
        let body = match answer {
            Ok(response) => response
                .json::<Value>()
                .await
                .map_err(|e| e.without_url().to_string()),
            Err(e) => Err(e.without_url().to_string()),
        };
        match body {
            Ok(body) if body.get("code").and_then(Value::as_i64) == Some(0) => {
                if let Some(data) = body.get("data").filter(|d| d.is_object()) {
                    return Ok(data.clone());
                }
                failures.push(format!("{origin}: no data"));
            }
            Ok(body) => failures.push(format!(
                "{origin}: code {} {}",
                body.get("code").map(Value::to_string).unwrap_or_default(),
                body.get("msg").and_then(Value::as_str).unwrap_or_default()
            )),
            Err(e) => failures.push(format!("{origin}: {e}")),
        }
    }
    Err(failures.join("; "))
}

/// Plan and credits from a billing summary: one window per credit group that has any
/// (`CreditGroups`, e.g. "套餐基础积分" 466 of 500), else per package.
fn read_summary(status: &mut AccountStatus, summary: &Value) {
    let number = |v: Option<&Value>| match v {
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        Some(v) => v.as_f64(),
        None => None,
    };
    let windows_of = |key: &str, name_key: &str| -> Vec<RateWindow> {
        summary
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|group| {
                let total = number(group.get("CycleTotalCapacity"))?;
                if total <= 0.0 {
                    return None;
                }
                let remaining = number(group.get("CycleRemainCapacity"))
                    .unwrap_or(0.0)
                    .max(0.0);
                let used = number(group.get("CycleUsedCapacity")).unwrap_or(total - remaining);
                Some(RateWindow {
                    name: group.get(name_key)?.as_str()?.to_owned(),
                    used_percent: (used / total * 100.0).clamp(0.0, 100.0),
                    window_seconds: None,
                    resets_at: None,
                    remaining: Some(round2(remaining)),
                    total: Some(round2(total)),
                })
            })
            .collect()
    };
    let mut windows = windows_of("CreditGroups", "Name");
    if windows.is_empty() {
        windows = windows_of("Packages", "PackageCode");
    }
    let left = number(summary.get("TotalCredits"))
        .unwrap_or_else(|| windows.iter().filter_map(|w| w.remaining).sum());
    status.plan = summary
        .get("SubscriptionPackageName")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    status.limit_reached = !windows.is_empty() && left <= 0.0;
    status.rate_limits = windows;
    status.source = Some(DataSource::Live);
    status.as_of = Some(Utc::now());
    // Said only when the login is shared; otherwise the figures speak for themselves.
    if status.note.as_deref() == Some(NO_QUOTA) {
        status.note = None;
    }
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Each model's credit multiplier for the user `uid` (`"x0.69 credits"` → 0.69), from the
/// model list the CLI caches under `local_storage/` (one JSON file per cached request, named
/// by a hash, holding `[{ userId, data: { models } }]`). Undocumented: anything that does not
/// fit is skipped, and no multipliers is a fine answer.
fn multipliers(home: &Path, uid: &str) -> HashMap<String, f64> {
    let mut rates = HashMap::new();
    let Ok(files) = std::fs::read_dir(home.join("local_storage")) else {
        return rates;
    };
    for file in files.flatten() {
        let Ok(text) = std::fs::read_to_string(file.path()) else {
            continue;
        };
        let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let models = entries
            .iter()
            .filter(|e| e.get("userId").and_then(Value::as_str) == Some(uid))
            .filter_map(|e| e.pointer("/data/models")?.as_array())
            .flatten();
        for model in models {
            let (Some(id), Some(credits)) = (
                model.get("id").and_then(Value::as_str),
                model.get("credits").and_then(Value::as_str),
            ) else {
                continue;
            };
            let rate = credits
                .trim()
                .trim_start_matches(['x', 'X', '×'])
                .split_whitespace()
                .next()
                .and_then(|n| n.parse::<f64>().ok());
            if let Some(rate) = rate {
                rates.insert(id.to_owned(), rate);
            }
        }
    }
    rates
}

/// The note when nothing says what is left; cleared once the billing summary does.
const NO_QUOTA: &str = "CodeBuddy reports no plan or quota";

/// Login state from an `initialize` answer: `account` is null when signed out. Only
/// harmless fields are read here; the login's token in it is used only to ask for the
/// billing summary.
fn read_initialize(status: &mut AccountStatus, init: &Value, shares_login: bool) {
    let account = init.get("account").filter(|a| a.is_object());
    status.logged_in = account.is_some();
    status.auth_mode = account
        .and_then(|a| a.get("type"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    status.source = Some(DataSource::Live);
    status.as_of = Some(Utc::now());
    status.note = Some(
        if !status.logged_in {
            "not logged in"
        } else if shares_login {
            "CodeBuddy keeps one login per user of this machine; this account uses it too"
        } else {
            NO_QUOTA
        }
        .into(),
    );
}

/// The models the CLI offers (`initialize`), its current one first.
fn models_of(init: &Value) -> Vec<ModelInfo> {
    let current = init.get("currentModelId").and_then(Value::as_str);
    let mut models: Vec<ModelInfo> = init
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?.to_owned();
            Some(ModelInfo {
                name: m
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_owned(),
                description: m
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                context_window: None,
                credit_multiplier: None,
                id,
            })
        })
        .collect();
    if let Some(at) = models.iter().position(|m| Some(m.id.as_str()) == current) {
        let first = models.remove(at);
        models.insert(0, first);
    }
    models
}

fn same_path(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

/// A port of this machine nobody listens on right now.
fn free_port() -> Option<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).ok()?;
    Some(listener.local_addr().ok()?.port())
}

/// Transcripts: `projects/<directory>/<session id>.jsonl`.
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

/// Token use of one transcript. Each model call's usage (`providerData.rawUsage`, in the
/// OpenAI shape, plus the `credit` it cost) is repeated on every line the call produced, so
/// it counts once per `providerData.messageId`.
fn scan_transcript(path: &Path) -> Option<SessionUsage> {
    let file = std::fs::File::open(path).ok()?;
    let mut started_at: Option<DateTime<Utc>> = None;
    let mut model = None;
    // Per call: when the line was written, the model it used and its usage.
    let mut calls: HashMap<String, (DateTime<Utc>, Option<String>, Value)> = HashMap::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let at = entry
            .get("timestamp")
            .and_then(Value::as_i64)
            .and_then(DateTime::from_timestamp_millis);
        if started_at.is_none() {
            started_at = at;
        }
        let Some(provider) = entry.get("providerData") else {
            continue;
        };
        let asked = provider
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if model.is_none() {
            model = asked.clone();
        }
        if let (Some(id), Some(usage)) = (
            provider.get("messageId").and_then(Value::as_str),
            provider.get("rawUsage"),
        ) {
            let at = at.or(started_at).unwrap_or_default();
            calls.insert(id.to_owned(), (at, asked, usage.clone()));
        }
    }
    if calls.is_empty() {
        return None;
    }
    let mut totals = TokenTotals {
        sessions: 1,
        ..TokenTotals::default()
    };
    let mut requests = Vec::with_capacity(calls.len());
    for (at, asked, usage) in calls.into_values() {
        let mut call = TokenTotals::default();
        let num = |pointer: &str| usage.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
        call.input_tokens += num("/prompt_tokens");
        call.cached_input_tokens += num("/prompt_tokens_details/cached_tokens");
        call.output_tokens += num("/completion_tokens");
        call.reasoning_output_tokens += num("/completion_tokens_details/reasoning_tokens");
        call.credits += usage.get("credit").and_then(Value::as_f64).unwrap_or(0.0);
        totals.input_tokens += call.input_tokens;
        totals.cached_input_tokens += call.cached_input_tokens;
        totals.output_tokens += call.output_tokens;
        totals.reasoning_output_tokens += call.reasoning_output_tokens;
        totals.credits += call.credits;
        requests.push(RequestUsage {
            at,
            model: asked.clone().or_else(|| model.clone()),
            totals: call,
        });
    }
    requests.sort_by_key(|r| r.at);
    Some(SessionUsage {
        session_id: path.file_stem()?.to_string_lossy().into_owned(),
        started_at: started_at.unwrap_or_default(),
        model,
        // Transcripts do not say who started the session.
        originator: None,
        totals,
        requests,
    })
}

#[async_trait]
impl AccountProvider for CodebuddyAccounts {
    fn name(&self) -> &str {
        "codebuddy"
    }

    /// `~/.codebuddy`. Not `CODEBUDDY_CONFIG_DIR`: a hub started from a WorkBuddy terminal
    /// inherits WorkBuddy's own, which agents never get.
    fn default_home(&self) -> Option<PathBuf> {
        codebuddy_native::default_home()
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
        .map_err(|e| CoreError::Storage(format!("cannot scan CodeBuddy transcripts: {e}")))?;
        let mut cache = self.scanned.lock().unwrap();
        for (path, (modified, len), usage) in scanned {
            result.extend(usage.clone());
            cache.insert(path, (modified, len, usage));
        }
        result.sort_by_key(|s| std::cmp::Reverse(s.started_at));
        Ok(result)
    }

    /// The models the CLI listed when the status was last read.
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

    /// The CLI's web UI, served on a free local port: the CLI itself logs in only inside
    /// its TUI, whose menu and login link do not survive being driven through a pipe. The
    /// hub stops the server once the account reports the login.
    fn login_command(&self, account: &Account) -> LoginCommand {
        let port = free_port().unwrap_or(8322);
        let mut command = self
            .command
            .clone()
            .unwrap_or_else(|| vec!["codebuddy".into()]);
        command.extend(codebuddy_native::serve_args(port));
        let mut env: Vec<_> = system_proxy_env()
            .into_iter()
            .chain(self.env_for(account))
            .collect();
        let env_remove = codebuddy_native::prepare_env(&mut env);
        LoginCommand {
            command,
            env: env.into_iter().collect(),
            env_remove,
            serves: Some(format!("http://localhost:{port}/")),
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
    use std::collections::BTreeMap;

    use super::*;
    use serde_json::json;

    #[test]
    fn site_logins_probes_and_agents_share_private_profiles() {
        let provider = CodebuddyAccounts::new(None);
        let accounts = [CodebuddySite::China, CodebuddySite::International].map(|site| Account {
            id: site.environment().into(),
            provider: "codebuddy".into(),
            label: String::new(),
            home: PathBuf::from(format!("/accounts/{}", site.environment())),
            codebuddy_site: Some(site),
            egress: None,
            created_at: Utc::now(),
        });
        for account in &accounts {
            let env: BTreeMap<_, _> = provider.agent_env(account).into_iter().collect();
            let login = provider.login_command(account);
            for (key, value) in &env {
                if ["CODEBUDDY_API_KEY", "CODEBUDDY_AUTH_TOKEN"].contains(&key.as_str()) {
                    assert!(!login.env.contains_key(key), "{key}");
                    assert!(login.env_remove.contains(key), "{key}");
                } else {
                    assert_eq!(login.env.get(key), Some(value), "{key}");
                }
            }
            assert_eq!(
                env["CODEBUDDY_CONFIG_DIR"],
                account.home.display().to_string()
            );
            assert_eq!(
                env["USERPROFILE"],
                account.home.join("profile").display().to_string()
            );
            assert!(env["LOCALAPPDATA"].starts_with(&env["USERPROFILE"]));
            assert_eq!(env["CODEBUDDY_API_KEY"], "");
            assert_eq!(env["CODEBUDDY_AUTH_TOKEN"], "");
            assert_eq!(
                env["CODEBUDDY_INTERNET_ENVIRONMENT"],
                account.codebuddy_site.unwrap().environment()
            );
        }
        let a: BTreeMap<_, _> = provider.agent_env(&accounts[0]).into_iter().collect();
        let b: BTreeMap<_, _> = provider.agent_env(&accounts[1]).into_iter().collect();
        assert_ne!(a["USERPROFILE"], b["USERPROFILE"]);
        assert_ne!(
            a["CODEBUDDY_INTERNET_ENVIRONMENT"],
            b["CODEBUDDY_INTERNET_ENVIRONMENT"]
        );
    }

    #[test]
    fn counts_each_model_call_once_from_transcripts() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("projects").join("c-w");
        std::fs::create_dir_all(&dir).unwrap();
        let line = |kind: &str, call: &str, prompt: u64, out: u64| {
            json!({ "type": kind, "timestamp": 1792711297756_i64, "providerData": {
                "model": "hy4-preview", "messageId": call, "agent": "cli", "rawUsage": {
                    "prompt_tokens": prompt, "completion_tokens": out, "credit": 1.25,
                    "prompt_tokens_details": { "cached_tokens": 100 },
                    "completion_tokens_details": { "reasoning_tokens": 2 } } } })
            .to_string()
        };
        let text = [
            r#"{"id":"u1","type":"message","role":"user","timestamp":1792711290000}"#.to_owned(),
            line("reasoning", "c1", 1000, 30),
            line("function_call", "c1", 1000, 30),
            line("message", "c2", 1200, 5),
        ]
        .join("\n");
        std::fs::write(dir.join("s1.jsonl"), text).unwrap();
        std::fs::write(dir.join("empty.jsonl"), "{\"type\":\"message\"}\n").unwrap();

        assert_eq!(transcript_files(home.path()).len(), 2);
        let usage = scan_transcript(&dir.join("s1.jsonl")).unwrap();
        assert_eq!(usage.session_id, "s1");
        assert_eq!(usage.model.as_deref(), Some("hy4-preview"));
        assert_eq!(
            usage.started_at,
            DateTime::from_timestamp_millis(1792711290000).unwrap()
        );
        assert_eq!(usage.totals.input_tokens, 2200);
        assert_eq!(usage.totals.cached_input_tokens, 200);
        assert_eq!(usage.totals.output_tokens, 35);
        assert_eq!(usage.totals.reasoning_output_tokens, 4);
        // Two calls, each counted once.
        assert!((usage.totals.credits - 2.5).abs() < 1e-9);
        // Each call is one timed request.
        assert_eq!(usage.requests.len(), 2);
        assert!(
            usage
                .requests
                .iter()
                .all(|r| r.model.as_deref() == Some("hy4-preview"))
        );
        let outputs: u64 = usage.requests.iter().map(|r| r.totals.output_tokens).sum();
        assert_eq!(outputs, 35);
        assert!(scan_transcript(&dir.join("empty.jsonl")).is_none());
    }

    #[test]
    fn credits_left_come_from_the_billing_summary() {
        let summary = json!({
            "SubscriptionPackageName": "体验版",
            "IsPaidUser": false,
            "TotalCredits": "2866.00000000999999999999",
            "Packages": [ { "PackageCode": "TCACA_code_008", "CycleTotalCapacity": "500",
                "CycleRemainCapacity": "466.00000001", "CycleUsedCapacity": "33.99999999" } ],
            "CreditGroups": [
                { "Kind": "plan_base", "Name": "套餐基础积分", "CycleTotalCapacity": "500",
                  "CycleRemainCapacity": "466.00000001", "CycleUsedCapacity": "33.99999999" },
                { "Kind": "addon", "Name": "购买积分", "CycleTotalCapacity": "0",
                  "CycleRemainCapacity": "0", "CycleUsedCapacity": "0" },
                { "Kind": "benefit", "Name": "平台奖励积分", "CycleTotalCapacity": "2400",
                  "CycleRemainCapacity": "2400", "CycleUsedCapacity": "0" },
            ],
        });
        let mut status = AccountStatus {
            logged_in: true,
            auth_mode: Some("personal".into()),
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
            note: Some(NO_QUOTA.into()),
        };
        read_summary(&mut status, &summary);
        assert_eq!(status.plan.as_deref(), Some("体验版"));
        assert!(status.note.is_none());
        assert!(!status.limit_reached);
        // Groups with nothing in them are left out.
        let names: Vec<_> = status.rate_limits.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["套餐基础积分", "平台奖励积分"]);
        let base = &status.rate_limits[0];
        assert_eq!((base.remaining, base.total), (Some(466.0), Some(500.0)));
        assert!((base.used_percent - 6.8).abs() < 0.01);

        // Without groups, each package is its own window.
        let mut packages_only = status.clone();
        read_summary(
            &mut packages_only,
            &json!({ "Packages": summary["Packages"], "TotalCredits": "0" }),
        );
        assert_eq!(packages_only.rate_limits[0].name, "TCACA_code_008");
        assert!(packages_only.limit_reached);
    }

    #[test]
    fn multipliers_come_from_the_cached_model_list_of_the_user() {
        let home = tempfile::tempdir().unwrap();
        let cache = home.path().join("local_storage");
        std::fs::create_dir_all(&cache).unwrap();
        let entry = |user: &str, rate: &str| {
            json!({ "userId": user, "data": { "models": [
                { "id": "glm-5.3", "credits": rate },
                { "id": "hunyuan-image-alpha", "credits": "" },
            ] } })
        };
        std::fs::write(
            cache.join("entry_a.info"),
            json!([entry("me", "x0.69 credits"), entry("other", "x9 credits")]).to_string(),
        )
        .unwrap();
        std::fs::write(cache.join("entry_b.info"), "not json").unwrap();
        std::fs::write(
            cache.join("entry_c.info"),
            json!({ "productFeatures": {} }).to_string(),
        )
        .unwrap();
        let rates = multipliers(home.path(), "me");
        assert_eq!(rates.get("glm-5.3"), Some(&0.69));
        assert!(!rates.contains_key("hunyuan-image-alpha"));
        assert!(multipliers(home.path(), "nobody").is_empty());
    }

    #[test]
    fn login_state_and_models_come_from_initialize() {
        let blank = || AccountStatus {
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
        let init = json!({
            "account": { "uid": "u", "type": "personal", "token": "secret" },
            "currentModelId": "hy3",
            "models": [ { "id": "hy4-preview", "name": "Hy4 preview" }, { "id": "hy3", "name": "Hy3" } ],
        });
        let mut status = blank();
        read_initialize(&mut status, &init, false);
        assert!(status.logged_in);
        assert_eq!(status.auth_mode.as_deref(), Some("personal"));
        assert!(!format!("{status:?}").contains("secret"));
        let models = models_of(&init);
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["hy3", "hy4-preview"]
        );
        assert_eq!(models[1].name, "Hy4 preview");

        let mut signed_out = blank();
        read_initialize(&mut signed_out, &json!({ "account": null }), false);
        assert!(!signed_out.logged_in);
        assert_eq!(signed_out.note.as_deref(), Some("not logged in"));
    }

    #[test]
    fn the_default_directory_keeps_codebuddy_config_dir_unset() {
        let accounts = CodebuddyAccounts::new(None);
        let account = |home: PathBuf| Account {
            codebuddy_site: None,
            egress: None,
            id: "codebuddy".into(),
            provider: "codebuddy".into(),
            label: String::new(),
            home,
            created_at: Utc::now(),
        };
        if let Some(default) = accounts.default_home() {
            assert!(accounts.agent_env(&account(default)).is_empty());
        }
        let other = account(PathBuf::from("/accounts/codebuddy-work"));
        assert_eq!(
            accounts.agent_env(&other),
            [(
                "CODEBUDDY_CONFIG_DIR".to_owned(),
                other.home.display().to_string()
            )]
        );
        assert!(accounts.manual_login(&other).is_none());
        let login = accounts.login_command(&other);
        let page = login.serves.expect("a login page");
        let port = login
            .command
            .iter()
            .skip_while(|a| *a != "--port")
            .nth(1)
            .expect("a port");
        assert_eq!(page, format!("http://localhost:{port}/"));
        assert!(
            login
                .command
                .starts_with(&["codebuddy".to_owned(), "--serve".to_owned()])
        );
        assert_eq!(
            login.env.get("CODEBUDDY_CONFIG_DIR"),
            Some(&other.home.display().to_string())
        );
    }
}
