//! Codex accounts (docs/tasks/M5.md): login state and plan from `auth.json`, live quota from
//! the ChatGPT usage endpoint, token use from Codex's own session logs.
//!
//! Tokens are read from `auth.json` and used only for the usage request; they are never
//! returned, logged or refreshed here (Codex refreshes them itself when it runs).

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, TimeZone, Utc};
use crossagents_core::{
    Account, AccountProvider, AccountStatus, CoreError, Credits, DataSource, LoginCommand,
    ModelInfo, RateWindow, RequestUsage, SessionUsage, TokenTotals, mask_email,
};
use serde_json::Value;

pub const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const LIVE_TTL: Duration = Duration::from_secs(60);

/// A parsed session log and the (mtime, size) it was parsed at.
type ScanEntry = (SystemTime, u64, Option<SessionUsage>);

pub struct CodexAccounts {
    usage_url: String,
    http: reqwest::Client,
    live: Mutex<HashMap<PathBuf, (Instant, AccountStatus)>>,
    /// Parsed session logs by path, valid while size and mtime are unchanged.
    scanned: Mutex<HashMap<PathBuf, ScanEntry>>,
    /// The same logs with every request timed (see [`scan_requests`]), for the usage series.
    /// Read whole, so kept apart from `scanned`, which serves [`Self::sessions`] cheaply.
    detailed: Mutex<HashMap<PathBuf, ScanEntry>>,
}

impl Default for CodexAccounts {
    fn default() -> Self {
        Self::with_usage_url(USAGE_URL)
    }
}

impl CodexAccounts {
    /// `usage_url` is configurable so tests can point it at a local server.
    pub fn with_usage_url(usage_url: impl Into<String>) -> Self {
        Self {
            usage_url: usage_url.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            live: Mutex::default(),
            scanned: Mutex::default(),
            detailed: Mutex::default(),
        }
    }
}

/// The parts of `auth.json` we use. Secrets stay inside this struct.
struct Auth {
    mode: Option<String>,
    api_key: bool,
    access_token: Option<String>,
    account_id: Option<String>,
    claims: Value,
}

impl Auth {
    fn read(home: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(home.join("auth.json")).ok()?;
        let json: Value = serde_json::from_str(&text).ok()?;
        let tokens = &json["tokens"];
        let claims = tokens["id_token"]
            .as_str()
            .and_then(jwt_claims)
            .unwrap_or(Value::Null);
        Some(Self {
            mode: json["auth_mode"].as_str().map(str::to_owned),
            api_key: json["OPENAI_API_KEY"]
                .as_str()
                .is_some_and(|k| !k.is_empty()),
            access_token: tokens["access_token"].as_str().map(str::to_owned),
            account_id: tokens["account_id"].as_str().map(str::to_owned),
            claims,
        })
    }

    fn openai_claim(&self, key: &str) -> Option<&Value> {
        self.claims
            .get("https://api.openai.com/auth")
            .and_then(|c| c.get(key))
            .filter(|v| !v.is_null())
    }

    fn access_token_expired(&self) -> bool {
        let exp = self
            .access_token
            .as_deref()
            .and_then(jwt_claims)
            .and_then(|c| c["exp"].as_i64());
        exp.is_some_and(|exp| exp <= Utc::now().timestamp())
    }
}

/// The payload of a JWT, without verifying it (we only read our own local file).
fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn epoch(secs: Option<i64>) -> Option<DateTime<Utc>> {
    secs.and_then(|s| Utc.timestamp_opt(s, 0).single())
}

fn parse_time(value: &Value) -> Option<DateTime<Utc>> {
    let s = value.as_str()?;
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .ok()
}

fn credits(value: &Value) -> Option<Credits> {
    if !value.is_object() {
        return None;
    }
    Some(Credits {
        has_credits: value["has_credits"].as_bool().unwrap_or(false),
        unlimited: value["unlimited"].as_bool().unwrap_or(false),
        balance: value["balance"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| value["balance"].as_f64().map(|b| b.to_string())),
    })
}

/// Rate windows from the live usage response.
fn live_windows(rate_limit: &Value) -> Vec<RateWindow> {
    [
        ("primary", "primary_window"),
        ("secondary", "secondary_window"),
    ]
    .into_iter()
    .filter_map(|(name, key)| {
        let w = rate_limit.get(key).filter(|w| w.is_object())?;
        Some(RateWindow {
            name: name.into(),
            used_percent: w["used_percent"].as_f64().unwrap_or(0.0),
            window_seconds: w["limit_window_seconds"].as_u64(),
            resets_at: epoch(w["reset_at"].as_i64()),
            remaining: None,
            total: None,
        })
    })
    .collect()
}

/// Rate windows from a session log's `rate_limits` snapshot.
fn snapshot_windows(rate_limits: &Value) -> Vec<RateWindow> {
    ["primary", "secondary"]
        .into_iter()
        .filter_map(|name| {
            let w = rate_limits.get(name).filter(|w| w.is_object())?;
            Some(RateWindow {
                name: name.into(),
                used_percent: w["used_percent"].as_f64().unwrap_or(0.0),
                window_seconds: w["window_minutes"].as_u64().map(|m| m * 60),
                resets_at: epoch(w["resets_at"].as_i64()),
                remaining: None,
                total: None,
            })
        })
        .collect()
}

/// What one session log says.
struct Scanned {
    usage: Option<SessionUsage>,
    /// The last `rate_limits` snapshot and when it was taken.
    limits: Option<(DateTime<Utc>, Value)>,
}

/// Fields collected from session-log lines; later lines overwrite earlier ones.
#[derive(Default)]
struct Fields {
    session_id: Option<String>,
    started_at: Option<DateTime<Utc>>,
    originator: Option<String>,
    model: Option<String>,
    totals: Option<TokenTotals>,
    limits: Option<(DateTime<Utc>, Value)>,
}

impl Fields {
    fn read(&mut self, line: &str) {
        // Cheap filter: almost every line is conversation content.
        let interesting = line.contains("\"session_meta\"")
            || line.contains("\"turn_context\"")
            || line.contains("\"token_count\"");
        if !interesting {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let payload = &v["payload"];
        match (v["type"].as_str(), payload["type"].as_str()) {
            (Some("session_meta"), _) => {
                self.session_id = payload["id"].as_str().map(str::to_owned);
                self.started_at =
                    parse_time(&payload["timestamp"]).or_else(|| parse_time(&v["timestamp"]));
                self.originator = payload["originator"].as_str().map(str::to_owned);
            }
            (Some("turn_context"), _) => {
                if let Some(m) = payload["model"].as_str() {
                    self.model = Some(m.to_owned());
                }
            }
            (Some("event_msg"), Some("token_count")) => {
                let t = &payload["info"]["total_token_usage"];
                if t.is_object() {
                    self.totals = Some(TokenTotals {
                        sessions: 1,
                        input_tokens: t["input_tokens"].as_u64().unwrap_or(0),
                        cached_input_tokens: t["cached_input_tokens"].as_u64().unwrap_or(0),
                        output_tokens: t["output_tokens"].as_u64().unwrap_or(0),
                        reasoning_output_tokens: t["reasoning_output_tokens"].as_u64().unwrap_or(0),
                        credits: 0.0,
                    });
                }
                if payload["rate_limits"].is_object()
                    && let Some(at) = parse_time(&v["timestamp"])
                {
                    self.limits = Some((at, payload["rate_limits"].clone()));
                }
            }
            _ => {}
        }
    }
}

/// Files up to this size are read whole.
const SMALL_FILE: u64 = 5 << 20;
/// Session metadata is at the very start of a log.
const HEAD_BYTES: u64 = 1 << 20;
/// Tail windows tried, in order, to find the last token count of a big log.
const TAIL_WINDOWS: [u64; 3] = [4 << 20, 16 << 20, 64 << 20];

/// Lines of `file` from byte `start` up to `end`, skipping a partial first line.
fn lines_between(file: &std::fs::File, start: u64, end: u64, fields: &mut Fields) {
    use std::io::{Read, Seek, SeekFrom};
    let mut reader = BufReader::new(file);
    if reader.seek(SeekFrom::Start(start)).is_err() {
        return;
    }
    let mut lines = BufReader::new(reader.take(end - start)).lines();
    if start > 0 {
        let _ = lines.next(); // starts mid-line
    }
    for line in lines.map_while(Result::ok) {
        fields.read(&line);
    }
}

/// Reads what a session log says. Logs can be gigabytes, so big ones are read only at the
/// start (session metadata) and the end (the cumulative token count is the last one).
fn scan_file(path: &Path) -> Scanned {
    let mut out = Scanned {
        usage: None,
        limits: None,
    };
    let Ok(file) = std::fs::File::open(path) else {
        return out;
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut fields = Fields::default();
    if len <= SMALL_FILE {
        lines_between(&file, 0, len, &mut fields);
    } else {
        lines_between(&file, 0, HEAD_BYTES, &mut fields);
        for window in TAIL_WINDOWS {
            let mut tail = Fields::default();
            lines_between(&file, len.saturating_sub(window), len, &mut tail);
            if tail.totals.is_some() || window >= len {
                fields.totals = tail.totals;
                fields.limits = tail.limits;
                fields.model = tail.model.or(fields.model);
                break;
            }
        }
    }
    let Fields {
        session_id,
        started_at,
        originator,
        model,
        totals,
        limits,
    } = fields;
    out.limits = limits;
    if let (Some(session_id), Some(started_at)) = (session_id, started_at) {
        out.usage = Some(SessionUsage {
            session_id,
            started_at,
            model,
            originator,
            totals: totals.unwrap_or(TokenTotals {
                sessions: 1,
                ..Default::default()
            }),
            requests: Vec::new(),
        });
    }
    out
}

/// The whole log, with each request timed. Codex reports `token_count` after every request
/// with the session's running totals, so the step from one report to the next is that
/// request's use; the report's own timestamp is when it finished. Unlike [`scan_file`] this
/// reads big logs to the end, so it is only used where every request is wanted.
fn scan_requests(path: &Path) -> Option<SessionUsage> {
    let file = std::fs::File::open(path).ok()?;
    let mut session_id = None;
    let mut started_at = None;
    let mut originator = None;
    let mut model: Option<String> = None;
    let mut previous = TokenTotals::default();
    let mut totals: Option<TokenTotals> = None;
    let mut requests = Vec::new();
    // These logs run to hundreds of megabytes, so the line buffer is reused and only the
    // few lines that carry session metadata or a token count are parsed.
    let mut reader = BufReader::with_capacity(1 << 20, file);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(text) = std::str::from_utf8(&line) else {
            continue;
        };
        let interesting = text.contains("\"session_meta\"")
            || text.contains("\"turn_context\"")
            || text.contains("\"token_count\"");
        if !interesting {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            continue;
        };
        let payload = &v["payload"];
        match (v["type"].as_str(), payload["type"].as_str()) {
            (Some("session_meta"), _) => {
                session_id = payload["id"].as_str().map(str::to_owned);
                started_at =
                    parse_time(&payload["timestamp"]).or_else(|| parse_time(&v["timestamp"]));
                originator = payload["originator"].as_str().map(str::to_owned);
            }
            (Some("turn_context"), _) => {
                if let Some(m) = payload["model"].as_str() {
                    model = Some(m.to_owned());
                }
            }
            (Some("event_msg"), Some("token_count")) => {
                let t = &payload["info"]["total_token_usage"];
                if !t.is_object() {
                    continue;
                }
                let num = |key: &str| t[key].as_u64().unwrap_or(0);
                let running = TokenTotals {
                    sessions: 1,
                    input_tokens: num("input_tokens"),
                    cached_input_tokens: num("cached_input_tokens"),
                    output_tokens: num("output_tokens"),
                    reasoning_output_tokens: num("reasoning_output_tokens"),
                    credits: 0.0,
                };
                let call = TokenTotals {
                    sessions: 0,
                    input_tokens: running.input_tokens.saturating_sub(previous.input_tokens),
                    cached_input_tokens: running
                        .cached_input_tokens
                        .saturating_sub(previous.cached_input_tokens),
                    output_tokens: running.output_tokens.saturating_sub(previous.output_tokens),
                    reasoning_output_tokens: running
                        .reasoning_output_tokens
                        .saturating_sub(previous.reasoning_output_tokens),
                    credits: 0.0,
                };
                previous = running.clone();
                totals = Some(running);
                // A report that adds nothing repeats an earlier one: not a request of its own.
                let used = call.input_tokens
                    + call.cached_input_tokens
                    + call.output_tokens
                    + call.reasoning_output_tokens;
                if used > 0
                    && let Some(at) = parse_time(&v["timestamp"])
                {
                    requests.push(RequestUsage {
                        at,
                        model: model.clone(),
                        totals: call,
                    });
                }
            }
            _ => {}
        }
    }
    requests.sort_by_key(|r| r.at);
    Some(SessionUsage {
        session_id: session_id?,
        started_at: started_at?,
        model,
        originator,
        totals: totals.unwrap_or(TokenTotals {
            sessions: 1,
            ..Default::default()
        }),
        requests,
    })
}

/// Every `rollout-*.jsonl` under `sessions/` and `archived_sessions/`.
fn session_files(home: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
            {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&home.join("sessions"), &mut files);
    walk(&home.join("archived_sessions"), &mut files);
    files
}

/// The newest file's rate-limit snapshot, scanning from the most recently modified.
fn latest_snapshot(home: &Path) -> Option<(DateTime<Utc>, Value)> {
    let mut files: Vec<(SystemTime, PathBuf)> = session_files(home)
        .into_iter()
        .filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p)))
        .collect();
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    files.iter().take(20).find_map(|(_, p)| scan_file(p).limits)
}

impl CodexAccounts {
    async fn live_status(&self, auth: &Auth) -> Result<Value, String> {
        let token = auth
            .access_token
            .as_deref()
            .ok_or("no ChatGPT login (API-key accounts have no quota to show)")?;
        let mut request = self.http.get(&self.usage_url).bearer_auth(token).header(
            "user-agent",
            concat!("crossagents/", env!("CARGO_PKG_VERSION")),
        );
        if let Some(account) = &auth.account_id {
            request = request.header("chatgpt-account-id", account);
        }
        let response = request
            .send()
            .await
            .map_err(|e| format!("cannot reach ChatGPT: {e}"))?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err("the login has expired; log in again or run any Codex session".into());
        }
        if !status.is_success() {
            return Err(format!("usage request failed: HTTP {status}"));
        }
        response
            .json()
            .await
            .map_err(|e| format!("unexpected usage response: {e}"))
    }
}

#[async_trait]
impl AccountProvider for CodexAccounts {
    fn name(&self) -> &str {
        "codex"
    }

    fn default_home(&self) -> Option<PathBuf> {
        if let Some(home) = std::env::var_os("CODEX_HOME") {
            return Some(PathBuf::from(home));
        }
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
        Some(PathBuf::from(home).join(".codex"))
    }

    async fn status(&self, account: &Account) -> AccountStatus {
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
        let Some(auth) = Auth::read(&account.home) else {
            status.note = Some(format!(
                "not logged in (no {})",
                account.home.join("auth.json").display()
            ));
            return status;
        };
        status.logged_in = auth.access_token.is_some() || auth.api_key;
        status.auth_mode = auth.mode.clone().or(Some(
            if auth.api_key { "api_key" } else { "chatgpt" }.to_owned(),
        ));
        status.email = auth.claims["email"].as_str().map(mask_email);
        status.plan = auth
            .openai_claim("chatgpt_plan_type")
            .and_then(Value::as_str)
            .map(str::to_owned);
        status.subscription_until = auth
            .openai_claim("chatgpt_subscription_active_until")
            .and_then(parse_time);

        if let Some((at, cached)) = self.live.lock().unwrap().get(&account.home)
            && at.elapsed() < LIVE_TTL
        {
            return cached.clone();
        }

        let live = if auth.access_token_expired() {
            Err("the login has expired; log in again or run any Codex session".to_owned())
        } else {
            self.live_status(&auth).await
        };
        match live {
            Ok(usage) => {
                if let Some(plan) = usage["plan_type"].as_str() {
                    status.plan = Some(plan.to_owned());
                }
                status.rate_limits = live_windows(&usage["rate_limit"]);
                status.limit_reached = usage["rate_limit"]["limit_reached"]
                    .as_bool()
                    .unwrap_or(false);
                status.credits = credits(&usage["credits"]);
                status.source = Some(DataSource::Live);
                status.as_of = Some(Utc::now());
                self.live
                    .lock()
                    .unwrap()
                    .insert(account.home.clone(), (Instant::now(), status.clone()));
            }
            Err(reason) => {
                let home = account.home.clone();
                let snapshot = tokio::task::spawn_blocking(move || latest_snapshot(&home))
                    .await
                    .ok()
                    .flatten();
                match snapshot {
                    Some((at, limits)) => {
                        status.rate_limits = snapshot_windows(&limits);
                        status.credits = credits(&limits["credits"]);
                        if let Some(plan) = limits["plan_type"].as_str() {
                            status.plan = Some(plan.to_owned());
                        }
                        status.limit_reached = !limits["rate_limit_reached_type"].is_null();
                        status.source = Some(DataSource::Snapshot);
                        status.as_of = Some(at);
                        status.note = Some(format!("{reason}; showing the last recorded quota"));
                    }
                    None => status.note = Some(reason),
                }
            }
        }
        status
    }

    async fn sessions(&self, account: &Account) -> Result<Vec<SessionUsage>, CoreError> {
        let files = session_files(&account.home);
        let mut result = Vec::with_capacity(files.len());
        let mut to_scan = Vec::new();
        {
            let cache = self.scanned.lock().unwrap();
            for path in files {
                let Ok(meta) = path.metadata() else { continue };
                let (modified, len) = (
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    meta.len(),
                );
                match cache.get(&path) {
                    Some((m, l, usage)) if *m == modified && *l == len => {
                        result.extend(usage.clone());
                    }
                    _ => to_scan.push((path, modified, len)),
                }
            }
        }
        let scanned = tokio::task::spawn_blocking(move || {
            to_scan
                .into_iter()
                .map(|(path, modified, len)| {
                    let usage = scan_file(&path).usage;
                    (path, modified, len, usage)
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|e| CoreError::Storage(format!("cannot scan Codex sessions: {e}")))?;
        let mut cache = self.scanned.lock().unwrap();
        for (path, modified, len, usage) in scanned {
            result.extend(usage.clone());
            cache.insert(path, (modified, len, usage));
        }
        result.sort_by_key(|s| std::cmp::Reverse(s.started_at));
        Ok(result)
    }

    /// The same sessions with each request timed (see [`scan_requests`]). These logs are read
    /// whole, so this has its own cache and leaves [`Self::sessions`] fast.
    async fn usage_sessions(&self, account: &Account) -> Result<Vec<SessionUsage>, CoreError> {
        let files = session_files(&account.home);
        let mut result = Vec::with_capacity(files.len());
        let mut to_scan = Vec::new();
        {
            let cache = self.detailed.lock().unwrap();
            for path in files {
                let Ok(meta) = path.metadata() else { continue };
                let (modified, len) = (
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    meta.len(),
                );
                match cache.get(&path) {
                    Some((m, l, usage)) if *m == modified && *l == len => {
                        result.extend(usage.clone());
                    }
                    _ => to_scan.push((path, modified, len)),
                }
            }
        }
        let scanned = tokio::task::spawn_blocking(move || {
            to_scan
                .into_iter()
                .map(|(path, modified, len)| {
                    let usage = scan_requests(&path);
                    (path, modified, len, usage)
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|e| CoreError::Storage(format!("cannot scan Codex sessions: {e}")))?;
        let mut cache = self.detailed.lock().unwrap();
        for (path, modified, len, usage) in scanned {
            result.extend(usage.clone());
            cache.insert(path, (modified, len, usage));
        }
        result.sort_by_key(|s| std::cmp::Reverse(s.started_at));
        Ok(result)
    }

    /// The models Codex itself lists for this login (`models_cache.json`, refreshed by
    /// Codex), ordered by its priority.
    async fn models(&self, account: &Account) -> Vec<ModelInfo> {
        let Ok(text) = std::fs::read_to_string(account.home.join("models_cache.json")) else {
            return Vec::new();
        };
        let Ok(cache) = serde_json::from_str::<Value>(&text) else {
            return Vec::new();
        };
        let mut models: Vec<(i64, ModelInfo)> = cache["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| m["visibility"].as_str() == Some("list"))
            .filter_map(|m| {
                let id = m["slug"].as_str()?.to_owned();
                Some((
                    m["priority"].as_i64().unwrap_or(i64::MAX),
                    ModelInfo {
                        name: m["display_name"].as_str().unwrap_or(&id).to_owned(),
                        description: m["description"].as_str().map(str::to_owned),
                        context_window: m["context_window"].as_u64(),
                        credit_multiplier: None,
                        id,
                    },
                ))
            })
            .collect();
        models.sort_by_key(|(priority, _)| *priority);
        models.into_iter().map(|(_, m)| m).collect()
    }

    fn login_command(&self, account: &Account) -> LoginCommand {
        LoginCommand {
            command: vec![
                "npx".into(),
                "-y".into(),
                "@openai/codex".into(),
                "login".into(),
            ],
            env: BTreeMap::from([("CODEX_HOME".to_owned(), account.home.display().to_string())]),
            env_remove: Vec::new(),
            serves: None,
        }
    }

    fn agent_env(&self, account: &Account) -> Vec<(String, String)> {
        vec![("CODEX_HOME".to_owned(), account.home.display().to_string())]
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn jwt(claims: Value) -> String {
        let enc = |v: &Value| URL_SAFE_NO_PAD.encode(v.to_string());
        format!(
            "{}.{}.sig",
            enc(&serde_json::json!({ "alg": "none" })),
            enc(&claims)
        )
    }

    /// A fake `CODEX_HOME`: a ChatGPT login and two sessions (one by CrossAgents).
    pub fn fixture(expired: bool) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let exp = Utc::now().timestamp() + if expired { -60 } else { 3600 };
        let auth = serde_json::json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": jwt(serde_json::json!({
                    "email": "someone@example.com",
                    "https://api.openai.com/auth": {
                        "chatgpt_plan_type": "plus",
                        "chatgpt_subscription_active_until": "2027-01-01T00:00:00Z"
                    }
                })),
                "access_token": jwt(serde_json::json!({ "exp": exp })),
                "refresh_token": "secret-refresh",
                "account_id": "acct-1"
            }
        });
        std::fs::write(home.path().join("auth.json"), auth.to_string()).unwrap();
        let day = home.path().join("sessions/2026/10/21");
        std::fs::create_dir_all(&day).unwrap();
        let session = |id: &str,
                       ts: &str,
                       originator: &str,
                       model: &str,
                       input: u64,
                       cached: u64| {
            [
                serde_json::json!({ "timestamp": ts, "type": "session_meta", "payload": { "id": id, "timestamp": ts, "originator": originator } }),
                serde_json::json!({ "timestamp": ts, "type": "response_item", "payload": { "type": "message", "content": "token_count is not here" } }),
                serde_json::json!({ "timestamp": ts, "type": "turn_context", "payload": { "model": model } }),
                serde_json::json!({ "timestamp": ts, "type": "event_msg", "payload": { "type": "token_count",
                    "info": { "total_token_usage": { "input_tokens": input, "cached_input_tokens": cached, "output_tokens": 10, "reasoning_output_tokens": 4 } },
                    "rate_limits": { "primary": { "used_percent": 12.5, "window_minutes": 300, "resets_at": 1800000000 },
                                     "credits": { "has_credits": true, "unlimited": false, "balance": "42.0" },
                                     "plan_type": "plus", "rate_limit_reached_type": null } } }),
            ]
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
        };
        std::fs::write(
            day.join("rollout-a.jsonl"),
            session(
                "s-own",
                "2026-10-21T01:00:00Z",
                "codex_cli_rs",
                "gpt-6-astra",
                1000,
                200,
            ),
        )
        .unwrap();
        std::fs::write(
            day.join("rollout-b.jsonl"),
            session(
                "s-ca",
                "2026-10-21T02:00:00Z",
                "crossagents",
                "gpt-5.6-luna",
                500,
                400,
            ),
        )
        .unwrap();
        home
    }

    fn account(home: &Path) -> Account {
        Account {
            id: "codex-main".into(),
            provider: "codex".into(),
            label: "main".into(),
            home: home.to_path_buf(),
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn reads_sessions_newest_first_and_caches() {
        let home = fixture(false);
        let provider = CodexAccounts::default();
        let sessions = provider.sessions(&account(home.path())).await.unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session_id, "s-ca");
        assert_eq!(sessions[0].originator.as_deref(), Some("crossagents"));
        assert_eq!(sessions[0].model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(sessions[0].totals.cached_input_tokens, 400);
        assert_eq!(sessions[1].totals.input_tokens, 1000);
        // Second call is served from the cache and gives the same answer.
        assert_eq!(
            provider.sessions(&account(home.path())).await.unwrap(),
            sessions
        );
    }

    #[tokio::test]
    async fn expired_login_falls_back_to_snapshot_without_network() {
        let home = fixture(true);
        // An unreachable URL proves the expired token is never sent.
        let provider = CodexAccounts::with_usage_url("http://127.0.0.1:1/never");
        let status = provider.status(&account(home.path())).await;
        assert!(status.logged_in);
        assert_eq!(status.email.as_deref(), Some("s***@example.com"));
        assert_eq!(status.plan.as_deref(), Some("plus"));
        assert_eq!(status.source, Some(DataSource::Snapshot));
        assert_eq!(status.rate_limits[0].used_percent, 12.5);
        assert_eq!(status.rate_limits[0].window_seconds, Some(18000));
        assert_eq!(
            status.credits.as_ref().unwrap().balance.as_deref(),
            Some("42.0")
        );
        assert!(status.note.as_deref().unwrap().contains("expired"));
        let json = serde_json::to_string(&status).unwrap();
        assert!(
            !json.contains("secret") && !json.contains("someone@"),
            "{json}"
        );
    }

    /// Serves one canned HTTP response and hands back the request it received.
    async fn fake_usage_server(body: Value) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/usage", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0; 8192];
            let n = socket.read(&mut buf).await.unwrap();
            let body = body.to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&buf[..n]).to_lowercase()
        });
        (url, task)
    }

    #[tokio::test]
    async fn live_status_uses_the_usage_endpoint() {
        let home = fixture(false);
        let (url, request) = fake_usage_server(serde_json::json!({
            "plan_type": "pro",
            "rate_limit": {
                "limit_reached": false,
                "primary_window": { "used_percent": 30, "limit_window_seconds": 18000, "reset_at": 1800000000 },
                "secondary_window": { "used_percent": 7, "limit_window_seconds": 604800, "reset_at": 1800500000 }
            },
            "credits": { "has_credits": true, "unlimited": false, "balance": "9.5" }
        }))
        .await;
        let provider = CodexAccounts::with_usage_url(url);
        let status = provider.status(&account(home.path())).await;
        let request = request.await.unwrap();
        assert!(request.contains("authorization: bearer "), "{request}");
        assert!(request.contains("chatgpt-account-id: acct-1"), "{request}");

        assert_eq!(status.source, Some(DataSource::Live));
        assert_eq!(status.plan.as_deref(), Some("pro"));
        let names: Vec<_> = status.rate_limits.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["primary", "secondary"]);
        assert_eq!(status.rate_limits[1].window_seconds, Some(604800));
        assert_eq!(status.credits.unwrap().balance.as_deref(), Some("9.5"));
        assert!(status.note.is_none());
        // Cached: a second call does not hit the (now closed) server.
        let again = provider.status(&account(home.path())).await;
        assert_eq!(again.source, Some(DataSource::Live));
    }

    #[test]
    fn times_every_request_from_the_running_token_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-two.jsonl");
        let report = |ts: &str, input: u64, cached: u64, out: u64| {
            serde_json::json!({ "timestamp": ts, "type": "event_msg", "payload": {
                "type": "token_count", "info": { "total_token_usage": {
                    "input_tokens": input, "cached_input_tokens": cached, "output_tokens": out } } } })
            .to_string()
        };
        let text = [
            serde_json::json!({ "timestamp": "2026-10-21T23:59:00Z", "type": "session_meta",
                "payload": { "id": "two", "timestamp": "2026-10-21T23:59:00Z", "originator": "codex_cli_rs" } })
            .to_string(),
            serde_json::json!({ "type": "turn_context", "payload": { "model": "gpt-5" } }).to_string(),
            report("2026-10-21T23:59:30Z", 100, 60, 10),
            report("2026-10-22T00:01:00Z", 300, 200, 25),
        ]
        .join("\n");
        std::fs::write(&path, text).unwrap();

        let usage = scan_requests(&path).unwrap();
        assert_eq!(usage.session_id, "two");
        assert_eq!(usage.totals.input_tokens, 300);
        // Two reports: the second one's step is its own request (200 in, 100 cached, 15 out).
        assert_eq!(usage.requests.len(), 2);
        assert_eq!(usage.requests[0].totals.input_tokens, 100);
        assert_eq!(usage.requests[0].totals.cached_input_tokens, 60);
        assert_eq!(usage.requests[1].totals.input_tokens, 200);
        assert_eq!(usage.requests[1].totals.cached_input_tokens, 140);
        assert_eq!(usage.requests[1].totals.output_tokens, 15);
        assert_eq!(
            usage.requests[1].at.to_rfc3339(),
            "2026-10-22T00:01:00+00:00"
        );
        assert_eq!(usage.requests[1].model.as_deref(), Some("gpt-5"));
    }

    #[test]
    fn big_logs_are_read_at_head_and_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-big.jsonl");
        let line = |v: Value| v.to_string() + "\n";
        let mut text = line(
            serde_json::json!({ "timestamp": "2026-10-21T00:00:00Z", "type": "session_meta",
            "payload": { "id": "big", "timestamp": "2026-10-21T00:00:00Z", "originator": "codex_cli_rs" } }),
        );
        text += &line(
            serde_json::json!({ "timestamp": "t", "type": "turn_context", "payload": { "model": "first" } }),
        );
        let count = |input: u64| {
            line(
                serde_json::json!({ "timestamp": "2026-10-21T00:00:01Z", "type": "event_msg",
            "payload": { "type": "token_count", "info": { "total_token_usage": { "input_tokens": input, "cached_input_tokens": 1 } } } }),
            )
        };
        text += &count(1);
        // ~6 MB of conversation in between, more than SMALL_FILE.
        let filler = line(
            serde_json::json!({ "type": "response_item", "payload": { "type": "message", "content": "x".repeat(1000) } }),
        );
        while (text.len() as u64) < SMALL_FILE + (1 << 20) {
            text += &filler;
        }
        text += &line(
            serde_json::json!({ "timestamp": "t", "type": "turn_context", "payload": { "model": "last" } }),
        );
        text += &count(99);
        std::fs::write(&path, text).unwrap();

        let usage = scan_file(&path).usage.unwrap();
        assert_eq!(usage.session_id, "big");
        assert_eq!(usage.totals.input_tokens, 99);
        assert_eq!(usage.model.as_deref(), Some("last"));
    }

    #[tokio::test]
    async fn lists_visible_models_by_priority() {
        let home = fixture(false);
        let cache = serde_json::json!({ "models": [
            { "slug": "old", "display_name": "Old", "visibility": "list", "priority": 9 },
            { "slug": "hidden", "visibility": "hide", "priority": 1 },
            { "slug": "best", "display_name": "Best", "description": "strong", "visibility": "list", "priority": 2, "context_window": 1000 }
        ]});
        std::fs::write(home.path().join("models_cache.json"), cache.to_string()).unwrap();
        let models = CodexAccounts::default().models(&account(home.path())).await;
        let ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["best", "old"]);
        assert_eq!(models[0].context_window, Some(1000));
    }

    #[tokio::test]
    async fn missing_login_is_reported() {
        let home = tempfile::tempdir().unwrap();
        let status = CodexAccounts::default().status(&account(home.path())).await;
        assert!(!status.logged_in);
        assert!(status.note.unwrap().contains("not logged in"));
    }
}
