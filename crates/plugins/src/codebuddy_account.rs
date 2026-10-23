//! CodeBuddy accounts. The CLI keeps one login per user of the machine (under the user's
//! `AppData`, shared with the CodeBuddy IDE extension), so the account is that login;
//! `CODEBUDDY_CONFIG_DIR` (the default is `~/.codebuddy`) only moves settings and
//! transcripts. Login state and models come from the CLI's `initialize` answer, token use
//! from its transcripts under `projects/`. CodeBuddy reports no plan or quota. Its CLI logs
//! in only inside its TUI, so a login serves the CLI's web UI locally (`--serve`) and the
//! login is done on that page.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crossagents_core::{
    Account, AccountProvider, AccountStatus, CoreError, DataSource, LoginCommand, ModelInfo,
    SessionUsage, TokenTotals,
};
use crossagents_drivers::codebuddy_native;
use crossagents_drivers::process::system_proxy_env;
use serde_json::Value;

/// The CLI is asked at most this often per account.
const LIVE_TTL: Duration = Duration::from_secs(60);

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
        Self::new(codebuddy_native::default_command())
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
        read_initialize(&mut status, &init, !self.env_for(account).is_empty());
        (status, models_of(&init))
    }
}

/// Login state from an `initialize` answer: `account` is null when signed out. Only
/// harmless fields are read; the answer also carries the login's token.
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
            "CodeBuddy reports no plan or quota"
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
/// OpenAI shape) is repeated on every line the call produced, so it counts once per
/// `providerData.messageId`.
fn scan_transcript(path: &Path) -> Option<SessionUsage> {
    let file = std::fs::File::open(path).ok()?;
    let mut started_at: Option<DateTime<Utc>> = None;
    let mut model = None;
    let mut calls: HashMap<String, Value> = HashMap::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if started_at.is_none() {
            started_at = entry
                .get("timestamp")
                .and_then(Value::as_i64)
                .and_then(DateTime::from_timestamp_millis);
        }
        let Some(provider) = entry.get("providerData") else {
            continue;
        };
        if model.is_none() {
            model = provider
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        if let (Some(id), Some(usage)) = (
            provider.get("messageId").and_then(Value::as_str),
            provider.get("rawUsage"),
        ) {
            calls.insert(id.to_owned(), usage.clone());
        }
    }
    if calls.is_empty() {
        return None;
    }
    let mut totals = TokenTotals {
        sessions: 1,
        ..TokenTotals::default()
    };
    for usage in calls.values() {
        let num = |pointer: &str| usage.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
        totals.input_tokens += num("/prompt_tokens");
        totals.cached_input_tokens += num("/prompt_tokens_details/cached_tokens");
        totals.output_tokens += num("/completion_tokens");
        totals.reasoning_output_tokens += num("/completion_tokens_details/reasoning_tokens");
    }
    Some(SessionUsage {
        session_id: path.file_stem()?.to_string_lossy().into_owned(),
        started_at: started_at.unwrap_or_default(),
        model,
        // Transcripts do not say who started the session.
        originator: None,
        totals,
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
        LoginCommand {
            command,
            env: system_proxy_env()
                .into_iter()
                .chain(self.env_for(account))
                .collect::<BTreeMap<_, _>>(),
            env_remove: codebuddy_native::withheld_env(),
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
    use super::*;
    use serde_json::json;

    #[test]
    fn counts_each_model_call_once_from_transcripts() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("projects").join("c-w");
        std::fs::create_dir_all(&dir).unwrap();
        let line = |kind: &str, call: &str, prompt: u64, out: u64| {
            json!({ "type": kind, "timestamp": 1792711297756_i64, "providerData": {
                "model": "hy4-preview", "messageId": call, "agent": "cli", "rawUsage": {
                    "prompt_tokens": prompt, "completion_tokens": out,
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
        assert!(scan_transcript(&dir.join("empty.jsonl")).is_none());
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
        assert!(login.command.starts_with(&["codebuddy".to_owned(), "--serve".to_owned()]));
        assert_eq!(
            login.env.get("CODEBUDDY_CONFIG_DIR"),
            Some(&other.home.display().to_string())
        );
    }
}
