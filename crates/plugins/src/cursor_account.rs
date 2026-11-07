//! Cursor accounts (docs/research/cursor-cli.md). The CLI keeps one login per user of the
//! machine: `agent status --format json` reads it and says who it is (`userInfo.email`).
//! The quota comes from the same login: the CLI's login file (`auth.json`) carries an
//! access token, and Cursor's private dashboard endpoint
//! (`aiserver.v1.DashboardService/GetCurrentPeriodUsage`) answers with how much of the
//! billing period is used. That endpoint is undocumented, so every field in its answer is
//! optional here: a field that is missing or renamed leaves that part of the quota empty
//! rather than failing the account page.
//!
//! Login is `agent login` with `NO_OPEN_BROWSER=1`: it prints the page on stdout (measured
//! through a pipe, so the hub can read it) and then polls the Cursor API until the page is
//! done, then exits. Models are `agent models`, whose text list the description's fallback
//! list stands in for when the login cannot list them.
//!
//! An explicit account moves only the CLI's own files: `CURSOR_DATA_DIR` (settings, chats)
//! and, with `AGENT_CLI_CREDENTIAL_STORE=file`, the login file beside the platform's config
//! directory — `%APPDATA%\Cursor\auth.json` on Windows (only `APPDATA` moves), and
//! `$XDG_CONFIG_HOME/cursor/auth.json` on Linux (only `XDG_CONFIG_HOME` moves). On macOS the
//! login file is `~/.cursor/auth.json`, which `CURSOR_DATA_DIR` does not move, so a second
//! login there has to move `HOME` too (and with it the git/ssh/npm files of the agent). The
//! agent's own profile stays the user's everywhere else.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crossagents_core::{
    Account, AccountProvider, AccountStatus, CoreError, DataSource, LimitGroup, LoginCommand,
    ModelInfo, RateWindow, SessionUsage, mask_email,
};
use crossagents_drivers::locate::os_name;
use crossagents_drivers::process::{hide_console, resolve_program, system_proxy_env};
use serde_json::Value;

/// The CLI is asked at most this often per account.
const LIVE_TTL: Duration = Duration::from_secs(60);
/// A status or model list that takes longer is treated as failed: both answer offline.
const CLI_TIMEOUT: Duration = Duration::from_secs(20);

/// Cursor's private dashboard read: the plan's usage for the current billing period. It is
/// not part of the CLI's documented surface, so the caller treats every answer as optional.
const USAGE_URL: &str = "https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage";
const USER_AGENT: &str = concat!("CrossAgents/", env!("CARGO_PKG_VERSION"));
/// A billing cycle longer than a year is not taken for a window length.
const MAX_CYCLE_SECONDS: u64 = 366 * 24 * 60 * 60;

const NOT_INSTALLED: &str =
    "the Cursor CLI is not installed (irm 'https://cursor.com/install?win32=true' | iex)";

/// A status read, with when and the models listed alongside.
type LiveEntry = (Instant, AccountStatus, Vec<ModelInfo>);

pub struct CursorAccounts {
    /// How to run the CLI; `None` when it is not installed.
    command: Option<Vec<String>>,
    live: Mutex<HashMap<PathBuf, LiveEntry>>,
}

impl Default for CursorAccounts {
    fn default() -> Self {
        Self::new(crate::builtin::cli("cursor").and_then(|cli| cli.command()))
    }
}

impl CursorAccounts {
    pub fn new(command: Option<Vec<String>>) -> Self {
        Self {
            command,
            live: Mutex::default(),
        }
    }

    /// The environment that points the CLI at this account's own files, except for the
    /// machine-wide default login. Empty overrides are removal markers: the CLI commands
    /// started here and the login drop those variables, and an agent process gets them as
    /// empty, which the CLI reads as unset — so a hub-wide API key cannot win over this
    /// account's login.
    fn env_for(&self, account: &Account) -> Vec<(String, String)> {
        if self
            .default_home()
            .is_some_and(|home| same_path(&home, &account.home))
        {
            return Vec::new();
        }
        isolated_env(&account.home, os_name())
    }

    /// The CLI's answer to `args`, as text. An empty-value env entry means "do not let the
    /// child see this variable".
    async fn output(&self, account: &Account, args: &[&str]) -> Result<String, String> {
        let Some(command) = &self.command else {
            return Err(NOT_INSTALLED.into());
        };
        let (program, rest) = command
            .split_first()
            .ok_or_else(|| "empty Cursor command".to_owned())?;
        let mut child = tokio::process::Command::new(resolve_program(program));
        child.args(rest).args(args);
        let env = self.env_for(account);
        for (key, value) in &env {
            if value.is_empty() {
                child.env_remove(key);
            }
        }
        child
            .envs(env.into_iter().filter(|(_, value)| !value.is_empty()))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        hide_console(&mut child);
        let output = tokio::time::timeout(CLI_TIMEOUT, child.output())
            .await
            .map_err(|_| {
                format!(
                    "`agent {}` did not answer in {} s",
                    args.join(" "),
                    CLI_TIMEOUT.as_secs()
                )
            })?
            .map_err(|e| format!("cannot run the Cursor CLI: {e}"))?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            let err = err.trim();
            return Err(if err.is_empty() {
                format!("`agent {}` failed ({})", args.join(" "), output.status)
            } else {
                format!("`agent {}` failed: {err}", args.join(" "))
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    async fn read_status(&self, account: &Account) -> (AccountStatus, Vec<ModelInfo>) {
        let mut status = blank_status();
        let answer = match self.output(account, &["status", "--format", "json"]).await {
            Ok(text) => text,
            Err(err) => {
                status.note = Some(err);
                return (status, Vec::new());
            }
        };
        let Ok(value) = serde_json::from_str::<Value>(&answer) else {
            status.note = Some(format!(
                "unexpected answer to `agent status`: {}",
                answer.trim()
            ));
            return (status, Vec::new());
        };
        read_status_json(&mut status, &value);
        if status.logged_in {
            apply_usage_result(&mut status, self.current_period_usage(account).await);
            // The plan is only a label here; a CLI that cannot say it leaves it empty.
            if status.plan.is_none() {
                status.plan = self.read_plan(account).await;
            }
        }
        let models = if status.logged_in {
            self.read_models(account).await
        } else {
            Vec::new()
        };
        (status, models)
    }

    /// The models the login may use, as the CLI lists them; the description's list when
    /// that fails (a login the CLI cannot ask, a CLI that does not know the command).
    async fn read_models(&self, account: &Account) -> Vec<ModelInfo> {
        match self.output(account, &["models"]).await {
            Ok(text) => {
                let models = models_of(&text);
                if models.is_empty() {
                    fallback_models()
                } else {
                    models
                }
            }
            Err(_) => fallback_models(),
        }
    }

    /// The CLI's login file for this account, under the same variables that move the login
    /// (see [`isolated_env`]); for the machine-wide login, the process's own values.
    fn auth_file(&self, account: &Account) -> Option<PathBuf> {
        let env: HashMap<String, String> = self.env_for(account).into_iter().collect();
        auth_file_in(os_name(), |key| {
            env.get(key)
                .filter(|value| !value.is_empty())
                .cloned()
                .or_else(|| std::env::var(key).ok())
        })
    }

    /// The plan's usage for the current billing period, from Cursor's dashboard endpoint.
    /// The login file's access token goes into this request's header and nowhere else;
    /// errors name the request, never the token.
    async fn current_period_usage(&self, account: &Account) -> Result<Value, String> {
        let path = self
            .auth_file(account)
            .ok_or("no Cursor login file (log in first)")?;
        let text = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => "no Cursor login file (log in first)".to_owned(),
                _ => format!("cannot read the Cursor login file: {e}"),
            })?;
        let login: Value = serde_json::from_str(&text)
            .map_err(|_| "the Cursor login file is not JSON".to_owned())?;
        let token = access_token(&login)
            .ok_or("the Cursor login file has no access token")?
            .to_owned();
        let http = reqwest::Client::builder()
            .timeout(CLI_TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| e.without_url().to_string())?;
        let answer = tokio::time::timeout(CLI_TIMEOUT, async {
            let response = http
                .post(USAGE_URL)
                .header("Content-Type", "application/json")
                .header("Connect-Protocol-Version", "1")
                .bearer_auth(&token)
                .json(&serde_json::json!({}))
                .send()
                .await
                .map_err(|e| e.without_url().to_string())?;
            let code = response.status();
            if code == reqwest::StatusCode::UNAUTHORIZED {
                return Err("the Cursor login has expired; log in again".to_owned());
            }
            if !code.is_success() {
                return Err(format!("the usage request failed: HTTP {code}"));
            }
            response
                .json::<Value>()
                .await
                .map_err(|e| e.without_url().to_string())
        })
        .await
        .map_err(|_| {
            format!(
                "the usage request did not answer in {} s",
                CLI_TIMEOUT.as_secs()
            )
        })??;
        Ok(answer)
    }

    /// `agent about`'s `subscriptionTier`, when the CLI knows the command. A label only:
    /// every failure leaves the plan empty rather than failing the account.
    async fn read_plan(&self, account: &Account) -> Option<String> {
        let text = self
            .output(account, &["about", "--format", "json"])
            .await
            .ok()?;
        let value: Value = serde_json::from_str(&text).ok()?;
        let tier = value.get("subscriptionTier")?.as_str()?.trim();
        (!tier.is_empty()).then(|| tier.to_owned())
    }
}

fn blank_status() -> AccountStatus {
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
        source: None,
        as_of: None,
        note: None,
    }
}

/// Login state and the account's identity from `agent status --format json`. The file holds
/// tokens next to these fields; none of them is read here.
fn read_status_json(status: &mut AccountStatus, value: &Value) {
    status.logged_in = value
        .get("isAuthenticated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    status.auth_mode = status.logged_in.then(|| "cursor".to_owned());
    status.email = value
        .pointer("/userInfo/email")
        .and_then(Value::as_str)
        .map(mask_email);
    status.source = Some(DataSource::Live);
    status.as_of = Some(Utc::now());
    status.note = if status.logged_in {
        // The CLI's own words when it could not fetch the profile ("Logged in (unable to
        // fetch user details)"); nothing when it could.
        value
            .get("message")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
            .map(str::to_owned)
    } else {
        Some("not logged in".to_owned())
    };
}

/// The CLI's login file on `os`, with `var` answering the variables that place it:
/// `%APPDATA%\Cursor\auth.json`, `$XDG_CONFIG_HOME/cursor/auth.json` (or
/// `~/.config/cursor/auth.json`), and `~/.cursor/auth.json`. `var` is the isolated
/// account's overrides when it has them, the process's own values otherwise.
fn auth_file_in(os: &str, var: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let (base, dir) = match os {
        "windows" => (PathBuf::from(var("APPDATA")?), "Cursor"),
        "macos" => (PathBuf::from(var("HOME")?), ".cursor"),
        _ => {
            let base = match var("XDG_CONFIG_HOME") {
                Some(dir) => PathBuf::from(dir),
                None => PathBuf::from(var("HOME")?).join(".config"),
            };
            (base, "cursor")
        }
    };
    Some(base.join(dir).join("auth.json"))
}

/// The token the CLI keeps in its login file. Only the access token is read: nothing here
/// refreshes or writes credentials.
fn access_token(login: &Value) -> Option<&str> {
    login
        .get("accessToken")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

/// A quota read's outcome: applied when the endpoint answered, a short note when it did
/// not. Login state, identity and provenance are never touched here.
fn apply_usage_result(status: &mut AccountStatus, usage: Result<Value, String>) {
    match usage {
        Ok(value) => apply_period_usage(status, &value),
        Err(reason) => {
            let note = format!("cannot read the quota: {reason}");
            status.note = Some(match status.note.take() {
                Some(existing) if !existing.is_empty() => format!("{existing}; {note}"),
                _ => note,
            });
        }
    }
}

/// The current billing period's usage: `planUsage.totalPercentUsed` is the main window,
/// `autoPercentUsed` and `apiPercentUsed` are separate limits. Every field is optional and a
/// missing or renamed one only leaves its part empty. Percentages are taken as reported:
/// nothing here derives one from another, and `totalSpend` (cents) is not needed for them.
fn apply_period_usage(status: &mut AccountStatus, value: &Value) {
    let Some(plan) = value.get("planUsage").filter(|plan| plan.is_object()) else {
        return;
    };
    let total = percent(plan.get("totalPercentUsed"));
    let auto = percent(plan.get("autoPercentUsed"));
    let api = percent(plan.get("apiPercentUsed"));
    if total.is_none() && auto.is_none() && api.is_none() {
        return;
    }
    // Milliseconds since the epoch, as a number or a numeric string.
    let start = millis(value.get("billingCycleStart"));
    let end = millis(value.get("billingCycleEnd"));
    let window_seconds = start
        .zip(end)
        .and_then(|(start, end)| end.checked_sub(start))
        .filter(|ms| *ms > 0)
        .and_then(|ms| u64::try_from(ms / 1000).ok())
        .filter(|seconds| *seconds > 0 && *seconds <= MAX_CYCLE_SECONDS);
    let resets_at = end.and_then(DateTime::<Utc>::from_timestamp_millis);
    let window = |name: &str, used: f64| RateWindow {
        name: name.to_owned(),
        used_percent: used.clamp(0.0, 100.0),
        window_seconds,
        resets_at,
        remaining: None,
        total: None,
    };

    let mut groups = Vec::new();
    if let Some(total) = total {
        let window = window("primary", total);
        status.rate_limits = vec![window.clone()];
        groups.push(LimitGroup {
            id: "all".into(),
            name: None,
            model: None,
            windows: vec![window],
            reached: total >= 100.0,
        });
    }
    for (id, name, used) in [("auto", "Auto", auto), ("api", "API", api)] {
        if let Some(used) = used {
            groups.push(LimitGroup {
                id: id.into(),
                name: Some(name.into()),
                model: None,
                windows: vec![window("primary", used)],
                reached: used >= 100.0,
            });
        }
    }
    status.limits = groups;
    status.limit_reached = [total, auto, api]
        .into_iter()
        .flatten()
        .any(|used| used >= 100.0);
}

/// A percentage as a number or a numeric string; anything else (absent, blank, not a
/// number) is skipped.
fn percent(value: Option<&Value>) -> Option<f64> {
    let used = match value? {
        Value::String(text) => text.trim().parse::<f64>().ok()?,
        value => value.as_f64()?,
    };
    (used.is_finite() && used >= 0.0).then_some(used)
}

/// An int64 as a number or a numeric string, for the epoch milliseconds the endpoint sends.
fn millis(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|f| f as i64)),
        Value::String(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// The models in `agent models`' list:
///
/// ```text
/// Available models
///
/// auto - Auto
/// gpt-5 (default)
/// sonnet-4-thinking - Sonnet 4 Thinking (current)
/// ```
///
/// Colours are dropped first (the CLI colours ids when it thinks it has a terminal).
fn models_of(text: &str) -> Vec<ModelInfo> {
    let text = plain(text);
    let mut models = Vec::new();
    let mut listed = false;
    for line in text.lines() {
        let line = line.trim();
        if !listed {
            listed = line.starts_with("Available models");
            continue;
        }
        // The list starts after a blank line and ends at one.
        if line.is_empty() {
            if models.is_empty() {
                continue;
            }
            break;
        }
        if line.starts_with("Tip:") || line.starts_with("No models available") {
            break;
        }
        // ` (current, default)` and the like say more about the login, not the model.
        let entry = match line.rsplit_once(" (") {
            Some((entry, flags)) if flags.ends_with(')') => entry,
            _ => line,
        };
        let (id, name) = match entry.split_once(" - ") {
            Some((id, name)) => (id.trim(), name.trim()),
            None => (entry.trim(), ""),
        };
        if id.is_empty() || id.split_whitespace().count() != 1 {
            continue;
        }
        models.push(ModelInfo {
            id: id.to_owned(),
            name: if name.is_empty() { id } else { name }.to_owned(),
            description: None,
            context_window: None,
            credit_multiplier: None,
        });
    }
    models
}

/// The list `crates/plugins/agents/cursor.toml` names, for a login the CLI cannot list.
fn fallback_models() -> Vec<ModelInfo> {
    crate::builtin::config("cursor")
        .map(|config| {
            config
                .models
                .iter()
                .map(|id| ModelInfo {
                    id: id.clone(),
                    name: id.clone(),
                    description: None,
                    context_window: None,
                    credit_multiplier: None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `text` without ANSI escape sequences (the model list's colours).
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn same_path(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

/// The variables that keep a second Cursor login in `home`: the data directory, the file
/// credential store, and — only where the CLI's login file follows it — the platform's
/// config directory:
///
/// - Windows: `APPDATA`, because the login file is `%APPDATA%\Cursor\auth.json`. The agent's
///   `USERPROFILE`/`HOME`/`LOCALAPPDATA` stay the user's, so the git, ssh and npm it runs
///   still see the real `~/.gitconfig`, `~/.ssh` and caches (measured).
/// - Linux: `XDG_CONFIG_HOME`, because the login file is `$XDG_CONFIG_HOME/cursor/auth.json`.
///   `HOME` stays the user's.
/// - macOS: `HOME`, because the login file is `~/.cursor/auth.json` and `CURSOR_DATA_DIR`
///   does not move it (the CLI hard-codes the home directory; see docs/research/cursor-cli.md).
///   With `HOME` moved, the agent's git/ssh/npm also see the profile rather than the user's
///   files: that is the price of a second login on macOS, and the only case where it is paid.
fn isolated_env(home: &Path, os: &str) -> Vec<(String, String)> {
    let profile = home.join("profile");
    let (key, value) = match os {
        "windows" => ("APPDATA", profile.join("AppData").join("Roaming")),
        "macos" => ("HOME", profile),
        _ => ("XDG_CONFIG_HOME", profile.join(".config")),
    };
    vec![
        ("CURSOR_DATA_DIR".to_owned(), home.display().to_string()),
        (key.to_owned(), value.display().to_string()),
        // The login file, rather than the system credential store, so it is this profile's
        // to keep (and to delete); on macOS this is what keeps it out of the keychain.
        ("AGENT_CLI_CREDENTIAL_STORE".to_owned(), "file".to_owned()),
        ("CURSOR_API_KEY".to_owned(), String::new()),
        ("CURSOR_AUTH_TOKEN".to_owned(), String::new()),
    ]
}

#[async_trait]
impl AccountProvider for CursorAccounts {
    fn name(&self) -> &str {
        "cursor"
    }

    /// `~/.cursor` (or `CURSOR_DATA_DIR`): the CLI's own directory, whose login is the
    /// machine's.
    fn default_home(&self) -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("CURSOR_DATA_DIR") {
            return Some(PathBuf::from(dir));
        }
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
        Some(PathBuf::from(home).join(".cursor"))
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

    /// Cursor keeps conversations, not token counts: the CLI reports none locally and its
    /// event stream has no usage, so there is nothing to count (docs/research/cursor-cli.md).
    async fn sessions(&self, _account: &Account) -> Result<Vec<SessionUsage>, CoreError> {
        Ok(Vec::new())
    }

    /// The models the CLI listed when the status was last read; the description's list
    /// when it could not.
    async fn models(&self, account: &Account) -> Vec<ModelInfo> {
        if !self.live.lock().unwrap().contains_key(&account.home) {
            self.status(account).await;
        }
        self.live
            .lock()
            .unwrap()
            .get(&account.home)
            .map(|(_, _, models)| models.clone())
            .filter(|models| !models.is_empty())
            .unwrap_or_else(fallback_models)
    }

    /// `agent login`, told not to open the browser: it prints the page on stdout and waits
    /// until it is done, so the hub can hand the link to the GUI.
    fn login_command(&self, account: &Account) -> LoginCommand {
        let mut command = self.command.clone().unwrap_or_else(|| vec!["agent".into()]);
        command.push("login".into());
        let mut env: Vec<_> = system_proxy_env()
            .into_iter()
            .chain(self.env_for(account))
            .chain([
                ("NO_OPEN_BROWSER".to_owned(), "1".to_owned()),
                // The login is the browser's, whatever the hub's environment says.
                ("CURSOR_API_KEY".to_owned(), String::new()),
                ("CURSOR_AUTH_TOKEN".to_owned(), String::new()),
            ])
            .collect();
        let mut env_remove: Vec<String> = env
            .iter()
            .filter(|(_, value)| value.is_empty())
            .map(|(key, _)| key.clone())
            .collect();
        env_remove.sort();
        env_remove.dedup();
        env.retain(|(_, value)| !value.is_empty());
        LoginCommand {
            command,
            env: env.into_iter().collect(),
            env_remove,
            serves: None,
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

    fn account(home: &Path) -> Account {
        Account {
            codebuddy_site: None,
            id: "cursor-work".into(),
            provider: "cursor".into(),
            label: String::new(),
            home: home.to_path_buf(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn login_state_email_and_note_come_from_the_status_answer() {
        let signed_in = json!({
            "status": "authenticated",
            "isAuthenticated": true,
            "hasAccessToken": true,
            "hasRefreshToken": true,
            "userInfo": { "email": "zhang@gmail.com", "userId": "user-1", "teamName": "Team" }
        });
        let mut status = blank_status();
        read_status_json(&mut status, &signed_in);
        assert!(status.logged_in);
        assert_eq!(status.auth_mode.as_deref(), Some("cursor"));
        assert_eq!(status.email.as_deref(), Some("z***@gmail.com"));
        assert_eq!(status.source, Some(DataSource::Live));
        assert!(status.note.is_none());
        assert!(status.rate_limits.is_empty());

        // No profile: logged in, and the CLI's own words are kept.
        let mut unknown = blank_status();
        read_status_json(
            &mut unknown,
            &json!({ "isAuthenticated": true, "message": "Logged in (unable to fetch user details)" }),
        );
        assert!(unknown.logged_in);
        assert!(unknown.email.is_none());
        assert_eq!(
            unknown.note.as_deref(),
            Some("Logged in (unable to fetch user details)")
        );

        let mut out = blank_status();
        read_status_json(
            &mut out,
            &json!({ "status": "unauthenticated", "isAuthenticated": false, "message": "Not logged in" }),
        );
        assert!(!out.logged_in);
        assert!(out.auth_mode.is_none() && out.email.is_none());
        assert_eq!(out.note.as_deref(), Some("not logged in"));

        // A half login (no refresh token) is not signed in.
        let mut partial = blank_status();
        read_status_json(
            &mut partial,
            &json!({ "status": "partially-authenticated", "isAuthenticated": false }),
        );
        assert!(!partial.logged_in);
    }

    #[test]
    fn models_come_from_the_cli_list_and_ignore_its_markings() {
        let text = "\u{1b}[2mAvailable models\u{1b}[22m\n\n\
            \u{1b}[36mauto\u{1b}[39m - Auto (default)\n\
            \u{1b}[32mgpt-5\u{1b}[39m\n\
            sonnet-4-thinking - Sonnet 4 Thinking (current)\n\
            \n\
            \u{1b}[2mTip: use --model <id> to switch.\u{1b}[22m\n";
        let models = models_of(text);
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["auto", "gpt-5", "sonnet-4-thinking"]
        );
        assert_eq!(models[0].name, "Auto");
        assert_eq!(models[1].name, "gpt-5");
        assert_eq!(models[2].name, "Sonnet 4 Thinking");
        assert!(models.iter().all(|m| m.credit_multiplier.is_none()));

        assert!(models_of("No models available for this account.").is_empty());
        assert!(models_of("").is_empty());
        assert!(models_of("Error: Authentication required.").is_empty());
    }

    #[test]
    fn the_fallback_is_the_descriptions_list() {
        let ids: Vec<String> = fallback_models().into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["auto", "sonnet-4-thinking", "gpt-5"]);
    }

    #[test]
    fn a_private_login_moves_only_the_variable_the_login_file_follows() {
        let provider = CursorAccounts::new(None);
        let Some(default) = provider.default_home() else {
            return;
        };
        assert!(provider.agent_env(&account(&default)).is_empty());
        let login = provider.login_command(&account(&default));
        assert!(!login.env.contains_key("CURSOR_DATA_DIR"));
        assert_eq!(
            login.env.get("NO_OPEN_BROWSER").map(String::as_str),
            Some("1")
        );
        assert!(login.env_remove.contains(&"CURSOR_API_KEY".to_owned()));

        let home = PathBuf::from("/accounts/cursor-work");
        let private = account(&home);
        let env: BTreeMap<_, _> = provider.agent_env(&private).into_iter().collect();
        assert_eq!(env["CURSOR_DATA_DIR"], home.display().to_string());
        assert_eq!(env["AGENT_CLI_CREDENTIAL_STORE"], "file");
        // Empty overrides reach an agent as empty strings, which the CLI reads as unset;
        // a login command drops them from the environment altogether.
        assert_eq!(env["CURSOR_API_KEY"], "");
        assert_eq!(env["CURSOR_AUTH_TOKEN"], "");
        // The agent's own git/ssh/npm files stay the user's: only the config directory the
        // login file lives in moves.
        if cfg!(windows) {
            assert_eq!(
                PathBuf::from(&env["APPDATA"]),
                home.join("profile").join("AppData").join("Roaming")
            );
            for untouched in ["USERPROFILE", "HOME", "LOCALAPPDATA", "XDG_CONFIG_HOME"] {
                assert!(!env.contains_key(untouched), "{untouched}");
            }
        } else if cfg!(target_os = "macos") {
            assert_eq!(PathBuf::from(&env["HOME"]), home.join("profile"));
            for untouched in ["APPDATA", "USERPROFILE", "LOCALAPPDATA", "XDG_CONFIG_HOME"] {
                assert!(!env.contains_key(untouched), "{untouched}");
            }
        } else {
            assert_eq!(
                PathBuf::from(&env["XDG_CONFIG_HOME"]),
                home.join("profile").join(".config")
            );
            for untouched in ["HOME", "APPDATA", "USERPROFILE", "LOCALAPPDATA"] {
                assert!(!env.contains_key(untouched), "{untouched}");
            }
        }

        let login = provider.login_command(&private);
        assert_eq!(login.command.last().map(String::as_str), Some("login"));
        assert_eq!(
            login.env.get("NO_OPEN_BROWSER").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            login.env.get("CURSOR_DATA_DIR"),
            Some(&home.display().to_string())
        );
        assert!(!login.env.contains_key("CURSOR_API_KEY"));
        assert!(login.env_remove.contains(&"CURSOR_API_KEY".to_owned()));
        assert!(login.env_remove.contains(&"CURSOR_AUTH_TOKEN".to_owned()));
        assert!(login.serves.is_none());

        // Two accounts never share their files.
        let other: BTreeMap<_, _> = provider
            .agent_env(&account(Path::new("/accounts/cursor-home")))
            .into_iter()
            .collect();
        assert_ne!(env["CURSOR_DATA_DIR"], other["CURSOR_DATA_DIR"]);
        // Windows keeps the login beside APPDATA; the others keep it beside the home.
        let where_login = |env: &BTreeMap<String, String>| {
            ["APPDATA", "HOME", "XDG_CONFIG_HOME"]
                .iter()
                .find_map(|key| env.get(*key).map(|v| (key.to_string(), v.clone())))
                .expect("one variable for the login file")
        };
        assert_ne!(where_login(&env), where_login(&other));
    }

    /// Each system moves exactly one variable, and the one its login file follows:
    /// `%APPDATA%\Cursor\auth.json`, `$XDG_CONFIG_HOME/cursor/auth.json`, and on macOS
    /// `~/.cursor/auth.json` (which `CURSOR_DATA_DIR` does not move, hence `HOME` there).
    #[test]
    fn only_the_platform_variable_the_login_file_follows_moves() {
        let home = Path::new("/accounts/cursor-work");
        let profile = home.join("profile");
        let env =
            |os: &str| -> BTreeMap<String, String> { isolated_env(home, os).into_iter().collect() };
        for os in ["windows", "macos", "linux"] {
            let env = env(os);
            assert_eq!(env["CURSOR_DATA_DIR"], home.display().to_string());
            assert_eq!(env["AGENT_CLI_CREDENTIAL_STORE"], "file");
            assert_eq!(env["CURSOR_API_KEY"], "");
            assert_eq!(env["CURSOR_AUTH_TOKEN"], "");
            let moved: Vec<&str> = ["APPDATA", "HOME", "XDG_CONFIG_HOME"]
                .into_iter()
                .filter(|key| env.contains_key(*key))
                .collect();
            assert_eq!(moved.len(), 1, "{os}: {moved:?}");
            for untouched in ["USERPROFILE", "LOCALAPPDATA"] {
                assert!(!env.contains_key(untouched), "{os}: {untouched}");
            }
        }
        let windows = env("windows");
        assert_eq!(
            PathBuf::from(&windows["APPDATA"]),
            profile.join("AppData").join("Roaming")
        );
        assert!(!windows.contains_key("HOME"));
        let macos = env("macos");
        assert_eq!(PathBuf::from(&macos["HOME"]), profile);
        assert!(!macos.contains_key("APPDATA"));
        let linux = env("linux");
        assert_eq!(
            PathBuf::from(&linux["XDG_CONFIG_HOME"]),
            profile.join(".config")
        );
        assert!(!linux.contains_key("HOME"));
    }

    #[tokio::test]
    async fn a_missing_cli_is_reported_and_models_fall_back() {
        let home = tempfile::tempdir().unwrap();
        let provider = CursorAccounts::new(None);
        let status = provider.status(&account(home.path())).await;
        assert!(!status.logged_in);
        assert!(status.note.unwrap().contains("not installed"));
        let models = provider.models(&account(home.path())).await;
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].id, "auto");
    }

    /// The endpoint sends epoch milliseconds and percentages as either JSON numbers or
    /// numeric strings, and the cycle dates are only used when they make a sane window.
    #[test]
    fn the_period_usage_accepts_strings_and_numbers() {
        let value = json!({
            "billingCycleStart": "1730419200000",
            "billingCycleEnd": 1_733_011_200_000i64,
            "planUsage": {
                "totalPercentUsed": "42.5",
                "autoPercentUsed": 10,
                "apiPercentUsed": "7.25",
                "totalSpend": 1234
            }
        });
        let mut status = blank_status();
        apply_period_usage(&mut status, &value);

        assert_eq!(status.rate_limits.len(), 1);
        let main = &status.rate_limits[0];
        assert_eq!(main.name, "primary");
        assert_eq!(main.used_percent, 42.5);
        assert_eq!(main.window_seconds, Some(30 * 24 * 60 * 60));
        assert_eq!(
            main.resets_at.map(|at| at.timestamp_millis()),
            Some(1_733_011_200_000)
        );
        assert!(main.remaining.is_none() && main.total.is_none());
        assert!(!status.limit_reached);

        let ids: Vec<&str> = status.limits.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["all", "auto", "api"]);
        assert_eq!(status.limits[1].name.as_deref(), Some("Auto"));
        assert_eq!(status.limits[2].name.as_deref(), Some("API"));
        assert_eq!(status.limits[1].windows[0].used_percent, 10.0);
        assert_eq!(status.limits[2].windows[0].used_percent, 7.25);
        assert!(status.limits.iter().all(|g| !g.reached));
    }

    /// A field that is missing, renamed, or not a number is skipped; nothing is invented and
    /// no percentage is hard-coded.
    #[test]
    fn missing_and_renamed_fields_leave_that_part_empty() {
        for value in [
            json!({}),
            json!({ "planUsage": {} }),
            json!({ "plan_usage": { "totalPercentUsed": 10 } }),
            json!({ "planUsage": "10" }),
            json!({ "planUsage": { "total_percent_used": 10 } }),
            json!({ "planUsage": { "totalPercentUsed": "soon", "autoPercentUsed": "  " } }),
            json!({ "planUsage": { "totalPercentUsed": -1 } }),
        ] {
            let mut status = blank_status();
            apply_period_usage(&mut status, &value);
            assert!(status.rate_limits.is_empty(), "{value}");
            assert!(status.limits.is_empty(), "{value}");
            assert!(!status.limit_reached, "{value}");
        }

        // One field present is enough for its own part only.
        let mut status = blank_status();
        apply_period_usage(
            &mut status,
            &json!({ "planUsage": { "apiPercentUsed": "12" } }),
        );
        assert!(status.rate_limits.is_empty());
        let ids: Vec<&str> = status.limits.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["api"]);
        assert_eq!(status.limits[0].windows[0].used_percent, 12.0);
    }

    #[test]
    fn a_bad_cycle_only_costs_the_window_length() {
        for cycle in [
            json!({ "billingCycleStart": "yesterday", "billingCycleEnd": "1733011200000" }),
            json!({ "billingCycleStart": "1733011200000", "billingCycleEnd": "1730419200000" }),
            json!({ "billingCycleStart": "1730419200000" }),
        ] {
            let mut value = cycle;
            value["planUsage"] = json!({ "totalPercentUsed": 5 });
            let mut status = blank_status();
            apply_period_usage(&mut status, &value);
            assert_eq!(status.rate_limits[0].used_percent, 5.0);
            assert!(status.rate_limits[0].window_seconds.is_none(), "{value}");
        }
    }

    #[test]
    fn any_percentage_at_a_hundred_reaches_the_limit() {
        let mut status = blank_status();
        apply_period_usage(
            &mut status,
            &json!({ "planUsage": { "totalPercentUsed": 99, "autoPercentUsed": "100" } }),
        );
        assert!(status.limit_reached);
        assert!(!status.limits[0].reached);
        assert!(status.limits[1].reached);

        // Over the cap the bar is full, and the limit still counts as reached.
        let mut over = blank_status();
        apply_period_usage(
            &mut over,
            &json!({ "planUsage": { "autoPercentUsed": 140 } }),
        );
        assert_eq!(over.limits[0].windows[0].used_percent, 100.0);
        assert!(over.limits[0].reached && over.limit_reached);
    }

    /// A failed read is a note, never a lost login: the account page keeps showing who is
    /// signed in with no quota rather than failing.
    #[test]
    fn a_failed_quota_read_keeps_the_login_and_the_windows_empty() {
        let mut status = blank_status();
        read_status_json(
            &mut status,
            &json!({ "isAuthenticated": true, "userInfo": { "email": "zhang@gmail.com" } }),
        );
        let before = status.clone();
        apply_usage_result(
            &mut status,
            Err("the Cursor login has expired; log in again".into()),
        );
        assert!(status.logged_in);
        assert_eq!(status.auth_mode.as_deref(), Some("cursor"));
        assert_eq!(status.email.as_deref(), Some("z***@gmail.com"));
        assert_eq!(status.source, Some(DataSource::Live));
        assert_eq!(status.as_of, before.as_of);
        assert!(status.rate_limits.is_empty() && status.limits.is_empty());
        assert!(!status.limit_reached);
        let note = status.note.unwrap();
        assert!(note.contains("cannot read the quota"), "{note}");
        assert!(note.contains("expired"), "{note}");
    }

    /// The login file sits where the CLI keeps it, following the same isolated variable as
    /// the CLI commands — never the machine's own directory.
    #[test]
    fn the_login_file_follows_the_isolated_variable_on_every_platform() {
        let home = Path::new("/accounts/cursor-work");
        let profile = home.join("profile");
        let machine = |os: &str, key: &str| match (os, key) {
            ("windows", "APPDATA") => Some("/home/user/AppData/Roaming".to_owned()),
            (_, "HOME") => Some("/home/user".to_owned()),
            ("linux", "XDG_CONFIG_HOME") => Some("/home/user/.config".to_owned()),
            _ => None,
        };
        for os in ["windows", "macos", "linux"] {
            let env: BTreeMap<String, String> = isolated_env(home, os).into_iter().collect();
            let file = auth_file_in(os, |key| env.get(key).cloned().or_else(|| machine(os, key)))
                .expect("a login file");
            let expected = match os {
                "windows" => profile
                    .join("AppData")
                    .join("Roaming")
                    .join("Cursor/auth.json"),
                "macos" => profile.join(".cursor").join("auth.json"),
                _ => profile.join(".config").join("cursor").join("auth.json"),
            };
            assert_eq!(file, expected, "{os}");
        }

        // The machine's own login, with no overrides.
        let at = |os: &str, key: &str| machine(os, key);
        assert_eq!(
            auth_file_in("windows", |key| at("windows", key)).unwrap(),
            Path::new("/home/user/AppData/Roaming/Cursor/auth.json")
        );
        assert_eq!(
            auth_file_in("macos", |key| at("macos", key)).unwrap(),
            Path::new("/home/user/.cursor/auth.json")
        );
        assert_eq!(
            auth_file_in("linux", |key| at("linux", key)).unwrap(),
            Path::new("/home/user/.config/cursor/auth.json")
        );
    }

    #[test]
    fn only_a_non_empty_access_token_is_read() {
        assert_eq!(access_token(&json!({ "accessToken": "x" })), Some("x"));
        for login in [
            json!({}),
            json!({ "accessToken": "" }),
            json!({ "accessToken": "  " }),
            json!({ "refreshToken": "x" }),
        ] {
            assert_eq!(access_token(&login), None, "{login}");
        }
    }

    #[tokio::test]
    async fn a_missing_login_file_is_an_error_and_not_a_request() {
        let home = tempfile::tempdir().unwrap();
        let provider = CursorAccounts::new(None);
        let err = provider
            .current_period_usage(&account(home.path()))
            .await
            .unwrap_err();
        assert!(err.contains("log in"), "{err}");
    }
}
