//! Claude Code accounts: one config directory per login (`CLAUDE_CONFIG_DIR`; the default is
//! `~/.claude`). Login state comes from `claude auth status`; plan, quota and models from
//! Claude Code itself (`initialize` and `get_usage` over its stream-json protocol, see
//! docs/research/claude-stream-json.md); token use from its transcripts under `projects/`.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crossagents_core::{
    Account, AccountProvider, AccountStatus, CoreError, DataSource, LoginCommand, ModelInfo,
    SessionUsage, TokenTotals, mask_email,
};
use crossagents_drivers::claude_native::{self, wire};
use crossagents_drivers::process::system_proxy_env;
use serde_json::Value;

/// Live figures are asked of Claude Code at most this often per account.
const LIVE_TTL: Duration = Duration::from_secs(60);

type ScanEntry = (SystemTime, u64, Option<SessionUsage>);
/// A status read, with when and the models listed alongside.
type LiveEntry = (Instant, AccountStatus, Vec<ModelInfo>);

pub struct ClaudeAccounts {
    /// How to run Claude Code (`claude`, found like the driver finds it).
    program: String,
    live: Mutex<HashMap<PathBuf, LiveEntry>>,
    scanned: Mutex<HashMap<PathBuf, ScanEntry>>,
}

impl Default for ClaudeAccounts {
    fn default() -> Self {
        Self::new("claude")
    }
}

impl ClaudeAccounts {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            live: Mutex::default(),
            scanned: Mutex::default(),
        }
    }

    /// `CLAUDE_CONFIG_DIR` for an account, except for the default directory: Claude Code
    /// keeps that login's `.claude.json` beside it, not inside, so it must stay unset.
    fn env_for(&self, account: &Account) -> Vec<(String, String)> {
        if self
            .default_home()
            .is_some_and(|home| same_path(&home, &account.home))
        {
            return Vec::new();
        }
        vec![(
            "CLAUDE_CONFIG_DIR".to_owned(),
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
        let env = self.env_for(account);
        let Some(auth) = claude_native::auth_status(&self.program, &env).await else {
            status.note = Some("cannot run claude (install Claude Code or the Claude app)".into());
            return (status, Vec::new());
        };
        let text = |key: &str| auth.get(key).and_then(Value::as_str);
        status.logged_in = auth.get("loggedIn").and_then(Value::as_bool) == Some(true);
        status.auth_mode = text("authMethod")
            .filter(|m| *m != "none")
            .map(str::to_owned);
        status.email = text("email").map(mask_email);
        status.plan = text("subscriptionType").map(str::to_owned);
        if !status.logged_in {
            status.note = Some("not logged in".into());
            return (status, Vec::new());
        }
        match claude_native::probe_account(&self.program, &env).await {
            Ok((init, usage)) => {
                if let Some(limits) = wire::usage_limits(&usage) {
                    status.rate_limits = limits.windows;
                    status.limit_reached = limits.reached.is_some();
                    status.credits = limits.credits;
                    status.limits = limits.by_limit;
                    status.plan = limits.plan.or(status.plan);
                } else {
                    status.note = Some("no plan limits (API key or another provider)".into());
                }
                status.source = Some(DataSource::Live);
                status.as_of = Some(Utc::now());
                (status, models_of(&init))
            }
            Err(err) => {
                status.note = Some(format!("cannot read the quota: {err}"));
                (status, Vec::new())
            }
        }
    }
}

/// The models Claude Code offers (`initialize`), its default first.
fn models_of(init: &Value) -> Vec<ModelInfo> {
    init.get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m.get("value")?.as_str()?.to_owned();
            Some(ModelInfo {
                name: m
                    .get("displayName")
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
        .collect()
}

fn same_path(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
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

/// Token use of one transcript. Claude Code writes each content block of a reply as its
/// own line with the reply's usage so far; the last line of each reply counts.
fn scan_transcript(path: &Path) -> Option<SessionUsage> {
    let file = std::fs::File::open(path).ok()?;
    let mut started_at: Option<DateTime<Utc>> = None;
    let mut model = None;
    let mut originator = None;
    let mut replies: HashMap<String, Value> = HashMap::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if started_at.is_none() {
            started_at = entry
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(|t| t.parse().ok());
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
        if model.is_none() {
            model = message
                .get("model")
                .and_then(Value::as_str)
                .filter(|m| !m.starts_with('<'))
                .map(str::to_owned);
        }
        if let (Some(id), Some(usage)) = (
            message.get("id").and_then(Value::as_str),
            message.get("usage"),
        ) {
            replies.insert(id.to_owned(), usage.clone());
        }
    }
    if replies.is_empty() {
        return None;
    }
    let mut totals = TokenTotals {
        sessions: 1,
        ..TokenTotals::default()
    };
    for usage in replies.values() {
        let num = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        totals.input_tokens += num("input_tokens")
            + num("cache_read_input_tokens")
            + num("cache_creation_input_tokens");
        totals.cached_input_tokens += num("cache_read_input_tokens");
        totals.output_tokens += num("output_tokens");
        totals.reasoning_output_tokens += usage
            .pointer("/output_tokens_details/thinking_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
    }
    Some(SessionUsage {
        session_id: path.file_stem()?.to_string_lossy().into_owned(),
        started_at: started_at.unwrap_or_default(),
        model,
        originator,
        totals,
    })
}

#[async_trait]
impl AccountProvider for ClaudeAccounts {
    fn name(&self) -> &str {
        "claude"
    }

    fn default_home(&self) -> Option<PathBuf> {
        if let Some(home) = std::env::var_os("CLAUDE_CONFIG_DIR") {
            return Some(PathBuf::from(home));
        }
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
        Some(PathBuf::from(home).join(".claude"))
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
        .map_err(|e| CoreError::Storage(format!("cannot scan Claude transcripts: {e}")))?;
        let mut cache = self.scanned.lock().unwrap();
        for (path, (modified, len), usage) in scanned {
            result.extend(usage.clone());
            cache.insert(path, (modified, len, usage));
        }
        result.sort_by_key(|s| std::cmp::Reverse(s.started_at));
        Ok(result)
    }

    /// The models Claude Code listed when the status was last read.
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

    fn login_command(&self, account: &Account) -> LoginCommand {
        let program = claude_native::locate_claude(&self.program);
        LoginCommand {
            command: vec![program.display().to_string(), "auth".into(), "login".into()],
            env: system_proxy_env()
                .into_iter()
                .chain(self.env_for(account))
                .collect::<BTreeMap<_, _>>(),
            env_remove: Vec::new(),
            serves: None,
        }
    }

    fn agent_env(&self, account: &Account) -> Vec<(String, String)> {
        self.env_for(account)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_each_reply_once_from_transcripts() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("projects").join("C--w");
        std::fs::create_dir_all(&dir).unwrap();
        let line = |id: &str, out: u64| {
            serde_json::json!({ "type": "assistant", "timestamp": "2026-10-22T04:32:11.128Z", "entrypoint": "crossagents",
                "message": { "id": id, "model": "claude-haiku-5-5", "usage": {
                    "input_tokens": 2, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 10, "output_tokens": out,
                    "output_tokens_details": { "thinking_tokens": 1 } } } })
            .to_string()
        };
        let text = [
            r#"{"type":"user","timestamp":"2026-10-22T04:32:10.000Z","entrypoint":"crossagents"}"#
                .to_owned(),
            line("m1", 5),
            line("m1", 20),
            line("m2", 7),
        ]
        .join("\n");
        std::fs::write(dir.join("s1.jsonl"), text).unwrap();
        std::fs::write(dir.join("empty.jsonl"), "{\"type\":\"user\"}\n").unwrap();

        let files = transcript_files(home.path());
        assert_eq!(files.len(), 2);
        let usage = scan_transcript(&dir.join("s1.jsonl")).unwrap();
        assert_eq!(usage.session_id, "s1");
        assert_eq!(usage.originator.as_deref(), Some("crossagents"));
        assert_eq!(usage.model.as_deref(), Some("claude-haiku-5-5"));
        assert_eq!(usage.totals.input_tokens, 224);
        assert_eq!(usage.totals.cached_input_tokens, 200);
        assert_eq!(usage.totals.output_tokens, 27);
        assert!(scan_transcript(&dir.join("empty.jsonl")).is_none());
    }

    #[test]
    fn the_default_directory_keeps_claude_config_dir_unset() {
        let accounts = ClaudeAccounts::default();
        let account = |home: PathBuf| Account {
            id: "claude".into(),
            provider: "claude".into(),
            label: String::new(),
            home,
            created_at: Utc::now(),
        };
        if let Some(default) = accounts.default_home() {
            assert!(accounts.agent_env(&account(default)).is_empty());
        }
        let other = account(PathBuf::from("/accounts/claude-work"));
        assert_eq!(
            accounts.agent_env(&other),
            [(
                "CLAUDE_CONFIG_DIR".to_owned(),
                other.home.display().to_string()
            )]
        );
    }

    #[test]
    fn models_come_from_initialize() {
        let init = serde_json::json!({ "models": [
            { "value": "default", "displayName": "Default (recommended)", "description": "Opus 5.5" },
            { "value": "haiku", "displayName": "Haiku 5.5" } ] });
        let models = models_of(&init);
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["default", "haiku"]
        );
        assert_eq!(models[1].name, "Haiku 5.5");
    }
}
