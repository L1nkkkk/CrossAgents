//! Gemini CLI accounts (docs/research/gemini-cli.md). The CLI keeps one login per home:
//! `~/.gemini/oauth_creds.json` (Google sign-in) or `~/.gemini/settings.json` naming an API
//! key. There is no quota API to read, so the account page shows who is signed in and which
//! models the CLI offers, and nothing about the plan.
//!
//! Signing in is the ACP round trip in `crossagents gemini-login`: the CLI has no `login`
//! subcommand, but its ACP mode takes `authenticate` and opens the browser itself. A second
//! account gets its own home (`GEMINI_CLI_HOME`), which is where the CLI reads `.gemini`
//! from — the machine login stays untouched.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::Utc;

use async_trait::async_trait;
use crossagents_core::{
    Account, AccountProvider, AccountStatus, CoreError, LoginCommand, ModelInfo, RequestUsage,
    SessionUsage, TokenTotals, mask_email,
};
use crossagents_drivers::gemini_native::wire::parse_models;
use crossagents_drivers::locate::Cli;
use crossagents_drivers::process::{apply_proxy_env, hide_console, resolve_program};
use serde_json::Value;

/// The CLI is asked at most this often.
const LIVE_TTL: Duration = Duration::from_secs(60);
/// `--list-models` that takes longer is treated as failed: the description's list stands in.
const CLI_TIMEOUT: Duration = Duration::from_secs(20);
/// Where the CLI keeps its files inside a home.
const CONFIG_DIR: &str = ".gemini";

const NOT_INSTALLED: &str = "the Gemini CLI is not installed (npm install -g @google/gemini-cli)";

enum CliLookup {
    /// Look up the CLI on demand so an install while the hub runs is picked up.
    Live(Option<Cli>),
    /// A fixed command (or `None` to simulate an uninstalled CLI) for tests.
    Fixed(Option<Vec<String>>),
}

pub struct GeminiAccounts {
    /// How to run the CLI.
    cli: CliLookup,
    /// How to start the login helper (`<crossagents> gemini-login`), when the hub has one.
    login: Option<Vec<String>>,
    /// The description's model list, for when the CLI cannot be asked.
    models: Vec<String>,
    live: Mutex<Option<(Instant, AccountStatus)>>,
}

impl Default for GeminiAccounts {
    fn default() -> Self {
        Self::with_login(None)
    }
}

impl GeminiAccounts {
    pub fn new(cli: Option<Vec<String>>, login: Option<Vec<String>>, models: Vec<String>) -> Self {
        Self {
            cli: CliLookup::Fixed(cli),
            login,
            models,
            live: Mutex::default(),
        }
    }

    /// Uses the built-in Gemini description, resolving the CLI on demand so a CLI installed
    /// after the hub started is found without restarting the hub.
    pub fn with_login(login: Option<Vec<String>>) -> Self {
        Self {
            cli: CliLookup::Live(crate::builtin::cli("gemini")),
            login,
            models: Vec::new(),
            live: Mutex::default(),
        }
    }

    fn command(&self) -> Option<Vec<String>> {
        match &self.cli {
            CliLookup::Live(cli) => cli.as_ref().and_then(Cli::command),
            CliLookup::Fixed(cli) => cli.clone(),
        }
    }

    /// `GEMINI_CLI_HOME` is the directory that *contains* `.gemini`, so an account's home is
    /// a home, not a config directory. The machine's own login is the default one.
    fn env_for(&self, account: &Account) -> Vec<(String, String)> {
        if self
            .default_home()
            .is_some_and(|home| same_path(&home, &account.home))
        {
            return Vec::new();
        }
        vec![(
            "GEMINI_CLI_HOME".to_owned(),
            account.home.display().to_string(),
        )]
    }

    fn config_dir(&self, account: &Account) -> PathBuf {
        account.home.join(CONFIG_DIR)
    }

    /// What the CLI wrote down about this account's sign-in, read without starting it.
    fn standing(&self, account: &Account) -> (bool, Option<String>, Option<String>) {
        let dir = self.config_dir(account);
        let read = |name: &str| {
            std::fs::read_to_string(dir.join(name))
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        };
        let mut logged_in = false;
        let mut auth_mode = None;
        if let Some(creds) = read("oauth_creds.json") {
            let token = |key: &str| {
                creds[key]
                    .as_str()
                    .filter(|value| !value.trim().is_empty())
                    .is_some()
            };
            if token("access_token") || token("refresh_token") {
                logged_in = true;
                auth_mode = Some("oauth".to_owned());
            }
        }
        if let Some(settings) = read("settings.json") {
            let selected = settings["security"]["auth"]["selectedType"].as_str();
            if let Some(kind) = selected {
                auth_mode.get_or_insert_with(|| kind.to_owned());
                if kind == "gemini-api-key" || kind == "vertex-ai" {
                    // The key itself lives in the settings or the environment; the CLI
                    // decides whether it works, so this only says which way it signs in.
                    logged_in = true;
                }
            }
        }
        let email = read("google_accounts.json")
            .and_then(|accounts| accounts["active"].as_str().map(str::to_owned))
            .filter(|email| !email.trim().is_empty());
        (logged_in, auth_mode, email)
    }

    /// The CLI's answer to `args`, as text.
    async fn output(&self, account: &Account, args: &[&str]) -> Result<String, String> {
        let Some(command) = self.command() else {
            return Err(NOT_INSTALLED.into());
        };
        let (program, rest) = command
            .split_first()
            .ok_or_else(|| "empty Gemini command".to_owned())?;
        let mut child = tokio::process::Command::new(resolve_program(program));
        child.args(rest).args(args);
        for (key, value) in self.env_for(account) {
            child.env(key, value);
        }
        apply_proxy_env(&mut child, account.proxy.as_deref());
        child
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        hide_console(&mut child);
        let output = tokio::time::timeout(CLI_TIMEOUT, child.output())
            .await
            .map_err(|_| {
                format!(
                    "`gemini {}` did not answer in {} s",
                    args.join(" "),
                    CLI_TIMEOUT.as_secs()
                )
            })?
            .map_err(|e| format!("cannot run the Gemini CLI: {e}"))?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            let err = err.trim();
            return Err(if err.is_empty() {
                format!("`gemini {}` failed ({})", args.join(" "), output.status)
            } else {
                format!("`gemini {}` failed: {err}", args.join(" "))
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

#[async_trait]
impl AccountProvider for GeminiAccounts {
    fn name(&self) -> &str {
        "gemini"
    }

    fn default_home(&self) -> Option<PathBuf> {
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
        Some(PathBuf::from(home))
    }

    async fn status(&self, account: &Account) -> AccountStatus {
        let installed = self.command().is_some();
        if let Some((at, status)) = self.live.lock().unwrap().as_ref()
            && at.elapsed() < LIVE_TTL
            && !(status.note.as_deref() == Some(NOT_INSTALLED) && installed)
        {
            return status.clone();
        }
        let (logged_in, auth_mode, email) = self.standing(account);
        let note = if !installed {
            Some(NOT_INSTALLED.to_owned())
        } else if !logged_in {
            Some(
                "还没有登录：点「登录」会在浏览器里打开 Google 授权页；自动登录不成功时，也可以在终端运行 gemini 登录一次"
                    .to_owned(),
            )
        } else {
            None
        };
        let status = AccountStatus {
            logged_in,
            auth_mode,
            email: email.as_deref().map(mask_email),
            plan: None,
            subscription_until: None,
            rate_limits: Vec::new(),
            limit_reached: false,
            credits: None,
            limits: Vec::new(),
            reset_credits: Vec::new(),
            source: None,
            as_of: None,
            note,
        };
        *self.live.lock().unwrap() = Some((Instant::now(), status.clone()));
        status
    }

    /// The CLI's per-project session files (`<home>/.gemini/tmp/<project>/chats/`): each
    /// message of the model carries the token counts the API reported, so a session adds up
    /// to real numbers. Files that do not parse are skipped: another version may write
    /// something else, and a wrong sum is worse than none.
    async fn sessions(&self, account: &Account) -> Result<Vec<SessionUsage>, CoreError> {
        let dir = self.config_dir(account).join("tmp");
        let scanned = tokio::task::spawn_blocking(move || {
            let mut found = Vec::new();
            for file in chat_files(&dir) {
                if let Some(session) = scan_chat(&file) {
                    found.push(session);
                }
            }
            found.sort_by_key(|s| std::cmp::Reverse(s.started_at));
            found
        })
        .await
        .map_err(|e| CoreError::Storage(format!("cannot scan Gemini sessions: {e}")))?;
        Ok(scanned)
    }

    async fn models(&self, account: &Account) -> Vec<ModelInfo> {
        let listed = self
            .output(account, &["--list-models"])
            .await
            .map(|text| parse_models(&text))
            .unwrap_or_default();
        let ids = if listed.is_empty() {
            self.models.clone()
        } else {
            listed
        };
        ids.into_iter()
            .map(|id| ModelInfo {
                name: id.clone(),
                id,
                description: None,
                context_window: None,
                credit_multiplier: None,
                efforts: None,
            })
            .collect()
    }

    /// `<crossagents> gemini-login --command <gemini>`: the ACP handshake that makes the CLI
    /// open its browser page. The hub watches the account until the credential file appears.
    fn login_command(&self, account: &Account) -> LoginCommand {
        let mut command = self.login.clone().unwrap_or_else(|| {
            vec![
                std::env::current_exe()
                    .map(|exe| exe.display().to_string())
                    .unwrap_or_else(|_| "crossagents".into()),
                "gemini-login".into(),
            ]
        });
        command.push("--command".into());
        match self.command() {
            Some(cli) => command.extend(cli),
            None => command.push("gemini".into()),
        }
        let mut env: Vec<(String, String)> = self.env_for(account);
        // The browser's login is the point; a hub-wide key would only hide that it ran.
        env.push(("GEMINI_API_KEY".to_owned(), String::new()));
        LoginCommand {
            command,
            env: env
                .iter()
                .filter(|(_, value)| !value.is_empty())
                .cloned()
                .collect(),
            env_remove: vec!["GEMINI_API_KEY".to_owned()],
            serves: None,
            opens_browser: true,
        }
    }

    fn forget_status(&self, _account: &Account) {
        *self.live.lock().unwrap() = None;
    }

    fn agent_env(&self, account: &Account) -> Vec<(String, String)> {
        self.env_for(account)
    }
}

/// Every session file under `<home>/.gemini/tmp`: one directory per project, `chats/` inside.
fn chat_files(tmp: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(projects) = std::fs::read_dir(tmp) else {
        return files;
    };
    for project in projects.flatten() {
        let Ok(entries) = std::fs::read_dir(project.path().join("chats")) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let extension = path.extension().and_then(|ext| ext.to_str());
            if matches!(extension, Some("json" | "jsonl")) {
                files.push(path);
            }
        }
    }
    files
}

/// One session file as usage. Each message of the model carries the token counts the API
/// reported (`tokens: {input, output, cached, thoughts, tool, total}`); the person's own
/// messages carry none. A file that does not look like that is skipped, not guessed at.
fn scan_chat(path: &Path) -> Option<SessionUsage> {
    let text = std::fs::read_to_string(path).ok()?;
    let record: Value = serde_json::from_str(&text).ok().or_else(|| {
        // A JSON-lines file keeps the conversation on a single line.
        text.lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|value| value.get("messages").is_some())
    })?;
    let messages = record["messages"].as_array()?;
    let mut requests = Vec::new();
    let mut totals = TokenTotals::default();
    for message in messages {
        if message["type"] != "gemini" {
            continue;
        }
        let tokens = &message["tokens"];
        if !tokens.is_object() {
            continue;
        }
        let Some(at) = message["timestamp"]
            .as_str()
            .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
            .map(|at| at.with_timezone(&Utc))
        else {
            continue;
        };
        let num = |key: &str| tokens[key].as_u64().unwrap_or(0);
        let input = num("input");
        let output = num("output");
        let usage = TokenTotals {
            sessions: 0,
            input_tokens: input,
            cached_input_tokens: num("cached").min(input),
            output_tokens: output,
            reasoning_output_tokens: num("thoughts").min(output),
            credits: 0.0,
        };
        totals.add(&usage);
        requests.push(RequestUsage {
            at,
            model: message["model"].as_str().map(str::to_owned),
            totals: usage,
        });
    }
    if requests.is_empty() {
        return None;
    }
    let started_at = record["startTime"]
        .as_str()
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .map(|at| at.with_timezone(&Utc))
        .unwrap_or(requests[0].at);
    let session_id = record["sessionId"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })?;
    Some(SessionUsage {
        session_id,
        started_at,
        model: requests.last().and_then(|request| request.model.clone()),
        // Nothing in the file says whether CrossAgents or the terminal started it.
        originator: None,
        totals: TokenTotals {
            sessions: 1,
            ..totals
        },
        requests,
    })
}

fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn account(home: &Path) -> Account {
        Account {
            codebuddy_site: None,
            egress: None,
            proxy: None,
            id: "gemini-main".into(),
            provider: "gemini".into(),
            label: "Gemini".into(),
            home: home.to_path_buf(),
            created_at: Utc::now(),
        }
    }

    fn write(home: &Path, name: &str, text: &str) {
        let dir = home.join(CONFIG_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), text).unwrap();
    }

    #[tokio::test]
    async fn a_credential_file_is_a_signed_in_account() {
        let home = tempfile::tempdir().unwrap();
        let accounts = GeminiAccounts::new(None, None, Vec::new());
        let status = accounts.status(&account(home.path())).await;
        assert!(!status.logged_in);
        assert!(status.note.is_some());

        write(
            home.path(),
            "oauth_creds.json",
            r#"{"access_token":"a","refresh_token":"r"}"#,
        );
        write(
            home.path(),
            "google_accounts.json",
            r#"{"active":"zhang@gmail.com","old":[]}"#,
        );
        accounts.forget_status(&account(home.path()));
        let status = accounts.status(&account(home.path())).await;
        assert!(status.logged_in);
        assert_eq!(status.auth_mode.as_deref(), Some("oauth"));
        assert_eq!(status.email.as_deref(), Some("z***@gmail.com"));
    }

    #[tokio::test]
    async fn an_api_key_in_the_settings_is_signed_in_too() {
        let home = tempfile::tempdir().unwrap();
        write(
            home.path(),
            "settings.json",
            r#"{"security":{"auth":{"selectedType":"gemini-api-key"}}}"#,
        );
        let status = GeminiAccounts::new(None, None, Vec::new())
            .status(&account(home.path()))
            .await;
        assert!(status.logged_in);
        assert_eq!(status.auth_mode.as_deref(), Some("gemini-api-key"));
    }

    #[tokio::test]
    async fn the_login_runs_the_helper_against_this_accounts_home() {
        let home = tempfile::tempdir().unwrap();
        let accounts = GeminiAccounts::new(
            Some(vec!["node".into(), "gemini.js".into()]),
            Some(vec!["crossagents".into(), "gemini-login".into()]),
            Vec::new(),
        );
        let login = accounts.login_command(&account(home.path()));
        assert_eq!(
            login.command,
            [
                "crossagents",
                "gemini-login",
                "--command",
                "node",
                "gemini.js"
            ]
        );
        assert!(login.opens_browser);
        assert_eq!(
            login.env.get("GEMINI_CLI_HOME").map(String::as_str),
            Some(home.path().display().to_string().as_str())
        );
        assert_eq!(login.env_remove, ["GEMINI_API_KEY"]);
    }

    #[test]
    fn a_session_file_adds_up_the_models_messages() {
        let dir = tempfile::tempdir().unwrap();
        let chats = dir.path().join("tmp/proj/chats");
        std::fs::create_dir_all(&chats).unwrap();
        std::fs::write(
            chats.join("session-1.json"),
            r#"{
              "sessionId": "s-1",
              "startTime": "2026-11-15T01:00:00.000Z",
              "messages": [
                {"type": "user", "timestamp": "2026-11-15T01:00:01.000Z", "content": "hi"},
                {"type": "gemini", "timestamp": "2026-11-15T01:00:05.000Z", "model": "gemini-2.5-pro",
                 "tokens": {"input": 100, "output": 20, "cached": 40, "thoughts": 5, "total": 120}},
                {"type": "gemini", "timestamp": "2026-11-15T01:00:09.000Z", "model": "gemini-2.5-pro",
                 "tokens": {"input": 50, "output": 10, "cached": 0, "thoughts": 0, "total": 60}}
              ]
            }"#,
        )
        .unwrap();
        // A file with nothing to add up (an older CLI, or a session that never answered).
        std::fs::write(
            chats.join("session-2.json"),
            r#"{"sessionId": "s-2", "messages": [{"type": "user", "content": "hi"}]}"#,
        )
        .unwrap();

        let files = chat_files(&dir.path().join("tmp"));
        assert_eq!(files.len(), 2);
        let usage = files.iter().filter_map(|file| scan_chat(file)).next();
        let usage = usage.expect("one session with tokens");
        assert_eq!(usage.session_id, "s-1");
        assert_eq!(usage.requests.len(), 2);
        assert_eq!(usage.totals.sessions, 1);
        assert_eq!(usage.totals.input_tokens, 150);
        assert_eq!(usage.totals.cached_input_tokens, 40);
        assert_eq!(usage.totals.output_tokens, 30);
        assert_eq!(usage.totals.reasoning_output_tokens, 5);
        assert_eq!(usage.model.as_deref(), Some("gemini-2.5-pro"));
        assert_eq!(usage.started_at.to_rfc3339(), "2026-11-15T01:00:00+00:00");
    }

    #[tokio::test]
    async fn the_machine_login_is_used_without_an_override() {
        let accounts = GeminiAccounts::new(None, None, Vec::new());
        let default = accounts.default_home().expect("a home");
        assert!(accounts.env_for(&account(&default)).is_empty());
    }
}
