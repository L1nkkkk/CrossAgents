//! Cursor accounts (docs/research/cursor-cli.md). The CLI keeps one login per user of the
//! machine: `agent status --format json` reads it and says who it is (`userInfo.email`).
//! What the CLI does not say is the quota: Cursor bills a plan on its web pages, and the
//! CLI has no usage command, so an account here shows its login, its email and its models,
//! and the quota stays empty ("暂无额度数据").
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
use chrono::Utc;
use crossagents_core::{
    Account, AccountProvider, AccountStatus, CoreError, DataSource, LoginCommand, ModelInfo,
    SessionUsage, mask_email,
};
use crossagents_drivers::locate::os_name;
use crossagents_drivers::process::{hide_console, resolve_program, system_proxy_env};
use serde_json::Value;

/// The CLI is asked at most this often per account.
const LIVE_TTL: Duration = Duration::from_secs(60);
/// A status or model list that takes longer is treated as failed: both answer offline.
const CLI_TIMEOUT: Duration = Duration::from_secs(20);

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
}
