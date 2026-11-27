//! `qoder-native`: Qoder CLI (`qodercli`) over its stream-json control protocol
//! (docs/research/qoder-cli.md).
//!
//! Qoder CLI speaks the full Claude Code / Agent SDK stream-json control protocol
//! (`initialize`, `can_use_tool`, `interrupt`, `set_model`, `apply_flag_settings`,
//! `get_context_usage`, `get_usage_info`, `--strict-mcp-config`,
//! `--permission-prompt-tool stdio`, `--replay-user-messages`, `--resume`,
//! `--fork-session`, `--resume-session-at`). Sessions therefore run on
//! [`crate::claude_native`] with [`Flavor::Qoder`]; this module adds Qoder's CLI lookup,
//! `qodercli --list-models` discovery, `qodercli status -o json` / `get_usage_info`
//! account probes, environment isolation, and cross-directory transcript adoption.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use crossagents_core::{
    Capabilities, CliStatus, CoreError, Driver, DriverKind, DriverSession, EgressPolicy,
    SessionSpec,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::claude_native::ClaudeNativeDriver;
use crate::claude_native::wire::{self, Flavor};
use crate::locate::Cli;
use crate::process::{check_proxy, hide_console, system_proxy_env, without_internal_env};

/// How long `qodercli status -o json` and `qodercli --list-models` may take.
const QUERY_TIMEOUT: Duration = Duration::from_secs(20);
/// How long [`probe_usage`] waits for `initialize` and `get_usage_info`.
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

/// Built-in Qoder model preset keys (`qodercli --list-models` prints their display names).
const PRESETS: [(&str, &str); 5] = [
    ("auto", "Auto"),
    ("ultimate", "Ultimate"),
    ("performance", "Performance"),
    ("efficient", "Efficient"),
    ("lite", "Lite"),
];

/// Auth env vars whose empty value means "unset / use saved login" rather than an explicit key.
const AUTH_ENV_VARS: [&str; 4] = [
    "QODER_PERSONAL_ACCESS_TOKEN",
    "QODER_SERVICE_ACCOUNT_KEY",
    "QODERCN_SERVICE_ACCOUNT_KEY",
    "QODER_SDK_ACCESS_TOKEN",
];

/// The `qoder-native` driver for one Profile.
pub struct QoderNativeDriver {
    inner: ClaudeNativeDriver,
    cli: Cli,
    /// Models explicitly pinned by configuration: skip replacing with `--list-models`.
    pinned: bool,
    /// Model ids discovered from `qodercli --list-models` during [`Driver::probe_cli`].
    discovered_models: Mutex<Option<Vec<String>>>,
    /// What to say when a session is asked of a CLI this machine does not have.
    missing: Option<String>,
}

impl QoderNativeDriver {
    /// `cli` is the description's CLI: `QODER_CLI` when set, else `qodercli` on `PATH` or
    /// its native install location (`~/.qoder/bin/qodercli/qodercli.exe` on Windows,
    /// `~/.local/bin/qodercli` on macOS/Linux).
    pub fn new(name: impl Into<String>, cli: Cli, models: Vec<String>) -> Self {
        let name = name.into();
        let missing = cli.missing_message(&name);
        Self {
            inner: ClaudeNativeDriver::with_flavor(name, cli.clone(), models, Flavor::Qoder),
            cli,
            pinned: false,
            discovered_models: Mutex::new(None),
            missing,
        }
    }

    /// Keep the configured `models` list rather than replacing it with `--list-models`.
    pub fn pinned(mut self, pinned: bool) -> Self {
        self.pinned = pinned;
        self
    }

    fn check(&self, spec: &SessionSpec) -> Result<(), CoreError> {
        if spec.command.is_none() && !self.inner.installed() {
            let message = self
                .missing
                .clone()
                .unwrap_or_else(|| format!("the {} CLI is not installed", self.inner.name()));
            return Err(CoreError::Agent(message));
        }
        Ok(())
    }
}

#[async_trait]
impl Driver for QoderNativeDriver {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn kind(&self) -> DriverKind {
        DriverKind::QoderNative
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn models(&self) -> Vec<String> {
        if !self.pinned
            && let Some(models) = self
                .discovered_models
                .lock()
                .expect("discovered_models lock")
                .as_ref()
            && !models.is_empty()
        {
            return models.clone();
        }
        self.inner.models()
    }

    fn installed(&self) -> bool {
        self.inner.installed()
    }

    async fn cli_status(&self) -> Option<CliStatus> {
        Some(self.inner.status().await)
    }

    /// Asked at hub startup, in the background: `/v1/plugins` never waits for it.
    async fn probe_cli(&self) {
        self.inner.probe_cli().await;
        if self.pinned {
            return;
        }
        let Some(command) = self.cli.command() else {
            return;
        };
        if let Ok(models) = list_models(&command, &[]).await
            && !models.is_empty()
        {
            let ids = models.into_iter().map(|(id, _)| id).collect();
            *self
                .discovered_models
                .lock()
                .expect("discovered_models lock") = Some(ids);
        }
    }

    /// Forward every default-implemented method so takeover, refresh, and lookup reports work.
    fn refresh_cli(&self) {
        self.inner.refresh_cli();
        *self
            .discovered_models
            .lock()
            .expect("discovered_models lock") = None;
    }

    fn cli_places(&self) -> Vec<String> {
        self.inner.cli_places()
    }

    async fn open(&self, spec: SessionSpec) -> Result<Box<dyn DriverSession>, CoreError> {
        self.check(&spec)?;
        self.inner.open(spec).await
    }

    async fn resume(
        &self,
        session_id: &str,
        spec: SessionSpec,
    ) -> Result<Box<dyn DriverSession>, CoreError> {
        self.check(&spec)?;
        self.inner.resume(session_id, spec).await
    }

    async fn adopt(
        &self,
        session_id: &str,
        turn_running: bool,
        spec: SessionSpec,
    ) -> Result<Option<Box<dyn DriverSession>>, CoreError> {
        self.inner.adopt(session_id, turn_running, spec).await
    }
}

/// Makes session `session_id` resumable from `cwd`. Qoder stores transcripts under
/// `<QODER_CONFIG_DIR>/projects/<project_folder(cwd)>/<session_id>.jsonl`, so resuming or
/// forking into another working directory (such as a run worktree) may not find a transcript
/// written in the original directory. Copy it into the new directory's folder when absent.
pub(crate) fn adopt_transcript(session_id: &str, cwd: &Path, env: &[(String, String)]) {
    let Some(projects) = home(env).map(|home| home.join("projects")) else {
        return;
    };
    let target = projects
        .join(project_folder(cwd))
        .join(format!("{session_id}.jsonl"));
    if target.is_file() {
        return;
    }
    let Some(source) = find_transcript(&projects, session_id) else {
        return;
    };
    let copied = target
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::copy(&source, &target));
    if let Err(err) = copied {
        tracing::warn!(
            "cannot copy Qoder session {session_id} to {}: {err}",
            target.display()
        );
    }
}

fn find_transcript(projects: &Path, session_id: &str) -> Option<PathBuf> {
    let file = format!("{session_id}.jsonl");
    std::fs::read_dir(projects)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path().join(&file))
        .find(|path| path.is_file())
}

/// Qoder's config directory for a session with environment `env`: its `QODER_CONFIG_DIR`,
/// else `~/.qoder` (under `env`'s `USERPROFILE`/`HOME` when overridden, else [`default_home`]).
fn home(env: &[(String, String)]) -> Option<PathBuf> {
    let set = |key: &str| {
        env.iter()
            .rev()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| PathBuf::from(v))
            .filter(|v| !v.as_os_str().is_empty())
    };
    if let Some(dir) = set("QODER_CONFIG_DIR") {
        return Some(dir);
    }
    match set(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        Some(user) => Some(user.join(".qoder")),
        None => default_home(),
    }
}

/// `~/.qoder`: where `qodercli` keeps its settings and transcripts when nothing moves it.
/// Does not read an inherited `QODER_CONFIG_DIR` because a hub started inside another Qoder
/// session withholds enclosing `QODER_*` runtime variables.
pub fn default_home() -> Option<PathBuf> {
    let user = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(user).join(".qoder"))
}

/// The inherited variables a Qoder process is started without (see [`Flavor::withholds`]).
pub fn withheld_env() -> Vec<String> {
    std::env::vars_os()
        .map(|(key, _)| key.to_string_lossy().into_owned())
        .filter(|key| Flavor::Qoder.withholds(key))
        .collect()
}

/// Prepare a Qoder child's environment: withhold enclosing `QODER_*` session variables and
/// convert empty credential overrides into `env_remove` so saved logins win cleanly.
pub fn prepare_env(env: &mut Vec<(String, String)>) -> Vec<String> {
    let mut remove = withheld_env();
    env.retain(|(key, value)| {
        let empty_auth = value.is_empty()
            && AUTH_ENV_VARS
                .iter()
                .any(|auth| key.eq_ignore_ascii_case(auth));
        if empty_auth {
            remove.push(key.clone());
        }
        !empty_auth
    });
    remove
}

/// The folder name `qodercli` uses under `projects/` for `cwd`: matches `tf(cwd)` in
/// `@qoder-ai/qodercli` 1.1.99 — every non-ASCII-alphanumeric character becomes `-`, and
/// names longer than 200 characters are truncated to 200 plus `-{djb2_base36}`.
pub(crate) fn project_folder(cwd: &Path) -> String {
    let raw = cwd.display().to_string();
    let path = raw.strip_prefix(r"\\?\").unwrap_or(&raw);
    let sanitized: String = path
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    if sanitized.chars().count() <= 200 {
        return sanitized;
    }
    let prefix: String = sanitized.chars().take(200).collect();
    let mut hash: i32 = 5381;
    for unit in path.encode_utf16() {
        hash = hash.wrapping_mul(33) ^ (unit as i32);
    }
    let abs = (hash as i64).unsigned_abs();
    format!("{prefix}-{}", base36(abs))
}

fn base36(mut n: u64) -> String {
    if n == 0 {
        return "0".into();
    }
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize] as char);
        n /= 36;
    }
    out.into_iter().rev().collect()
}

/// Runs `qodercli status -o json` for an account's environment. `None` when `qodercli`
/// cannot run or prints non-JSON output.
pub async fn auth_status(command: &[String], env: &[(String, String)]) -> Option<Value> {
    let (program, extra) = command.split_first()?;
    let mut process_env = system_proxy_env();
    process_env.extend(without_internal_env(env));
    let remove = prepare_env(&mut process_env);
    let mut cmd = Command::new(program);
    for key in remove {
        cmd.env_remove(key);
    }
    cmd.args(extra)
        .args(["status", "-o", "json"])
        .envs(process_env)
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    hide_console(&mut cmd);
    let output = tokio::time::timeout(QUERY_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    serde_json::from_slice(&output.stdout).ok()
}

/// Runs `qodercli --list-models` and parses `(model_id, display_name)` pairs.
pub async fn list_models(
    command: &[String],
    env: &[(String, String)],
) -> Result<Vec<(String, String)>, CoreError> {
    let (program, extra) = command
        .split_first()
        .ok_or_else(|| CoreError::Agent("empty qodercli launch command".into()))?;
    let mut process_env = system_proxy_env();
    process_env.extend(without_internal_env(env));
    let remove = prepare_env(&mut process_env);
    let mut cmd = Command::new(program);
    for key in remove {
        cmd.env_remove(key);
    }
    cmd.args(extra)
        .arg("--list-models")
        .envs(process_env)
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    hide_console(&mut cmd);
    let output = tokio::time::timeout(QUERY_TIMEOUT, cmd.output())
        .await
        .map_err(|_| CoreError::Agent("qodercli --list-models timed out".into()))?
        .map_err(|e| CoreError::Agent(format!("cannot run {program}: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        return Err(CoreError::Agent(if detail.is_empty() {
            format!("qodercli --list-models exited with {}", output.status)
        } else {
            detail
        }));
    }
    Ok(parse_models(&String::from_utf8_lossy(&output.stdout)))
}

/// Parses `qodercli --list-models` output (`MODEL\n<displayName>\n<displayName> (<key>)\n...`)
/// into deduplicated `(id, display_name)` pairs.
pub fn parse_models(text: &str) -> Vec<(String, String)> {
    let clean = strip_ansi(text);
    let mut out: Vec<(String, String)> = Vec::new();
    for raw in clean.lines() {
        let line = raw.trim();
        if line.is_empty() || line.eq_ignore_ascii_case("MODEL") {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("not logged in")
            || lower.starts_with("failed to ")
            || lower.starts_with("no models ")
            || lower.starts_with("warning:")
        {
            continue;
        }
        let (id, name) = if let Some((prefix, rest)) = line.rsplit_once(" (")
            && let Some(key) = rest.strip_suffix(')')
            && !key.is_empty()
            && !key.contains('…')
        {
            let name = prefix.trim();
            (
                key.trim().to_owned(),
                if name.is_empty() {
                    key.trim().to_owned()
                } else {
                    name.to_owned()
                },
            )
        } else if let Some((preset_id, preset_name)) = PRESETS
            .iter()
            .find(|(id, name)| line.eq_ignore_ascii_case(id) || line.eq_ignore_ascii_case(name))
        {
            ((*preset_id).to_owned(), (*preset_name).to_owned())
        } else if !line.contains(char::is_whitespace) {
            (line.to_owned(), line.to_owned())
        } else {
            continue;
        };
        if !id.is_empty() && !out.iter().any(|(seen, _)| seen == &id) {
            out.push((id, name));
        }
    }
    out
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Queries Qoder's live account quota without starting a conversation: spawns headless
/// stream-json, sends `initialize` + `get_usage_info`, and returns the `get_usage_info` answer.
pub async fn probe_usage(command: &[String], env: &[(String, String)]) -> Result<Value, CoreError> {
    let (program, extra) = command
        .split_first()
        .ok_or_else(|| CoreError::Agent("empty qodercli launch command".into()))?;
    let api_host = Flavor::Qoder.api_host(env);
    let egress = EgressPolicy::from_env(env)
        .unwrap_or_else(|| EgressPolicy::default_for_provider(Flavor::Qoder.program()));
    let direct = check_proxy(env, Some(&api_host), egress).await?;
    let mut process_env = system_proxy_env();
    process_env.extend(without_internal_env(env));
    process_env.extend(direct);
    let remove = prepare_env(&mut process_env);
    let mut child = Command::new(program);
    for key in remove {
        child.env_remove(key);
    }
    child
        .args(extra)
        .args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--strict-mcp-config",
            "--setting-sources",
            wire::DEFAULT_SETTING_SOURCES,
        ])
        .envs(process_env)
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    hide_console(&mut child);
    let mut child = child
        .spawn()
        .map_err(|e| CoreError::Agent(format!("cannot start {program}: {e}")))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let ask = async {
        for (id, request) in [
            ("init", json!({ "subtype": "initialize" })),
            ("usage", json!({ "subtype": "get_usage_info" })),
        ] {
            let mut line = wire::control_request(id, request).to_string();
            line.push('\n');
            stdin.write_all(line.as_bytes()).await?;
        }
        stdin.flush().await?;
        let mut lines = BufReader::new(stdout).lines();
        let (mut init_ok, mut usage) = (false, None);
        while let Some(line) = lines.next_line().await? {
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match wire::control_answer(&message) {
                Some((id, Ok(_))) if id == "init" => init_ok = true,
                Some((id, Err(err))) if id == "init" => {
                    return Ok::<_, std::io::Error>(Err(err));
                }
                Some((id, answer)) if id == "usage" => usage = Some(answer),
                _ => {}
            }
            if init_ok && let Some(usage) = &usage {
                return Ok(usage.clone());
            }
        }
        Err(std::io::Error::other("qodercli exited before answering"))
    };
    let answer = tokio::time::timeout(PROBE_TIMEOUT, ask).await;
    let _ = child.start_kill();
    match answer {
        Ok(Ok(Ok(usage))) => Ok(usage),
        Ok(Ok(Err(err))) => Err(CoreError::Agent(err)),
        Ok(Err(err)) => Err(CoreError::Agent(err.to_string())),
        Err(_) => Err(CoreError::Agent("qodercli did not answer".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_auth_overrides_remove_credentials_instead_of_setting_empty_values() {
        let mut env = vec![
            ("QODER_PERSONAL_ACCESS_TOKEN".into(), String::new()),
            ("qoder_sdk_access_token".into(), String::new()),
            ("QODER_CONFIG_DIR".into(), "/private/qoder".into()),
            ("OTHER_EMPTY".into(), String::new()),
        ];
        let remove = prepare_env(&mut env);
        let mut command = std::process::Command::new("unused");
        command.env("QODER_PERSONAL_ACCESS_TOKEN", "parent-pat");
        command.env("qoder_sdk_access_token", "parent-sdk");
        for key in remove {
            command.env_remove(key);
        }
        command.envs(env);
        let overrides: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        for key in ["QODER_PERSONAL_ACCESS_TOKEN", "qoder_sdk_access_token"] {
            assert_eq!(overrides.get(std::ffi::OsStr::new(key)), Some(&None));
        }
        assert_eq!(
            overrides[std::ffi::OsStr::new("QODER_CONFIG_DIR")],
            Some(std::ffi::OsStr::new("/private/qoder"))
        );
        assert_eq!(
            overrides[std::ffi::OsStr::new("OTHER_EMPTY")],
            Some(std::ffi::OsStr::new(""))
        );
    }

    #[test]
    fn explicit_qoder_credentials_are_preserved() {
        let mut env = vec![
            ("QODER_PERSONAL_ACCESS_TOKEN".into(), "pat-123".into()),
            ("QODER_REGION".into(), "cn".into()),
        ];
        let original = env.clone();
        let remove = prepare_env(&mut env);
        assert_eq!(env, original);
        for key in ["QODER_PERSONAL_ACCESS_TOKEN", "QODER_REGION"] {
            assert!(
                !remove
                    .iter()
                    .any(|removed| removed.eq_ignore_ascii_case(key))
            );
        }
    }

    #[test]
    fn runs_on_the_qoder_driver_and_exposes_capabilities() {
        let driver = QoderNativeDriver::new(
            "qoder",
            Cli::at(vec!["my-qodercli".into()]),
            vec!["auto".into(), "ultimate".into()],
        );
        assert_eq!(driver.kind(), DriverKind::QoderNative);
        assert_eq!(driver.name(), "qoder");
        assert_eq!(driver.models(), ["auto", "ultimate"]);
        let caps = driver.capabilities();
        assert!(
            caps.streaming
                && caps.interrupt
                && caps.mcp
                && caps.resume
                && caps.move_cwd
                && caps.fork
                && caps.set_model
                && caps.usage
                && caps.rate_limits
                && caps.permission_callback
                && caps.compaction
        );
        assert!(!caps.steer);
    }

    #[test]
    fn project_folder_matches_qoder_tf_algorithm() {
        assert_eq!(
            project_folder(Path::new(r"C:\Users\Link\Dev\CrossAgents")),
            "C--Users-Link-Dev-CrossAgents"
        );
        assert_eq!(
            project_folder(Path::new("/home/me/my_repo.v2")),
            "-home-me-my-repo-v2"
        );
        // Long path (> 200 chars) truncates to 200 + `-{djb2_base36}`.
        let long = format!("/{}", "a".repeat(250));
        let folder = project_folder(Path::new(&long));
        assert!(folder.starts_with(&format!("-{}", "a".repeat(199))));
        let (prefix, suffix) = folder.rsplit_once('-').unwrap();
        assert_eq!(prefix.len(), 200);
        assert!(!suffix.is_empty());
        assert!(suffix.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn adopt_transcript_copies_session_across_directories() {
        let home = tempfile::tempdir().unwrap();
        let env = [(
            "QODER_CONFIG_DIR".to_owned(),
            home.path().display().to_string(),
        )];
        let old = home.path().join("projects").join("C--old-repo");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("s1.jsonl"), "{\"type\":\"user\"}\n").unwrap();
        let cwd = Path::new(r"C:\work\wt-1");
        adopt_transcript("s1", cwd, &env);
        let adopted = home.path().join("projects").join(project_folder(cwd));
        assert_eq!(
            std::fs::read_to_string(adopted.join("s1.jsonl")).unwrap(),
            "{\"type\":\"user\"}\n"
        );
        adopt_transcript("missing", cwd, &env);
        assert!(!adopted.join("missing.jsonl").exists());
    }

    #[test]
    fn parses_list_models_presets_and_custom_models() {
        let output = "\x1b[1mMODEL\x1b[0m\n\
            Auto\n\
            Ultimate\n\
            Performance\n\
            Efficient\n\
            Lite\n\
            Claude Sonnet 4.5 (claude-sonnet-4-5)\n\
            qwen3-coder-plus\n\
            Auto\n";
        let parsed = parse_models(output);
        assert_eq!(
            parsed,
            vec![
                ("auto".into(), "Auto".into()),
                ("ultimate".into(), "Ultimate".into()),
                ("performance".into(), "Performance".into()),
                ("efficient".into(), "Efficient".into()),
                ("lite".into(), "Lite".into()),
                ("claude-sonnet-4-5".into(), "Claude Sonnet 4.5".into()),
                ("qwen3-coder-plus".into(), "qwen3-coder-plus".into()),
            ]
        );
        assert!(parse_models("Not logged in. Run `qodercli login` to authenticate.\n").is_empty());
    }

    #[tokio::test]
    async fn mock_qoder_session_round_trips_protocol_env_usage_context_and_model_switch() {
        // Skip only if `node` is unavailable in the test environment.
        if std::process::Command::new("node")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_err()
        {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("mock-qodercli.mjs");
        std::fs::write(
            &script,
            r#"
import readline from 'node:readline';
const argv = process.argv.slice(2);
if (argv.includes('--verbose')) {
  console.error('unexpected --verbose');
  process.exit(2);
}
if (process.env.QODER_EXPOSE_TOKEN_USAGE !== '1') {
  console.error('missing QODER_EXPOSE_TOKEN_USAGE=1');
  process.exit(3);
}
const rl = readline.createInterface({ input: process.stdin });
const send = (obj) => process.stdout.write(JSON.stringify(obj) + '\n');
let lastSetModel = null;
rl.on('line', (line) => {
  if (!line.trim()) return;
  const msg = JSON.parse(line);
  if (msg.type === 'control_request') {
    const { request_id: id, request: req } = msg;
    if (req.subtype === 'initialize') {
      send({ type: 'control_response', response: { subtype: 'success', request_id: id, response: { commands: [], skills: [] } } });
      send({ type: 'system', subtype: 'init', session_id: 's1', slash_commands: ['compact'], skills: ['review'] });
    } else if (req.subtype === 'set_model') {
      lastSetModel = req;
      send({ type: 'control_response', response: { subtype: 'success', request_id: id, response: { ok: true } } });
    } else if (req.subtype === 'apply_flag_settings') {
      send({ type: 'control_response', response: { subtype: 'success', request_id: id, response: { applied: { effort: req.settings?.model?.reasoningEffort ?? null } } } });
    } else if (req.subtype === 'get_usage_info') {
      send({
        type: 'control_response',
        response: {
          subtype: 'success',
          request_id: id,
          response: {
            usage: {
              userId: 'u1',
              userType: 'pro',
              totalUsagePercentage: 25,
              expiresAt: 1793084400,
              isQuotaExceeded: false,
              usageType: 'credits',
              userQuota: { total: 1000, used: 250, remaining: 750, percentage: 25, unit: 'credits' }
            },
            session: { total_credits: 0.5, model_usage: {} }
          }
        }
      });
    } else if (req.subtype === 'get_context_usage') {
      send({
        type: 'control_response',
        response: {
          subtype: 'success',
          request_id: id,
          response: {
            model: lastSetModel?.model ?? 'auto',
            contextWindow: { usedPercentage: 5.0 },
            categories: [
              { type: 'system_prompt', percentage: 2.0 },
              { type: 'messages', percentage: 3.0 },
              { type: 'free_space', percentage: 95.0 }
            ]
          }
        }
      });
    } else if (req.subtype === 'interrupt') {
      send({ type: 'control_response', response: { subtype: 'success', request_id: id, response: { still_queued: [] } } });
    }
  } else if (msg.type === 'user') {
    send({ type: 'stream_event', parent_tool_use_id: null, event: { type: 'message_start', message: { id: 'm1', usage: { input_tokens: 10000, context_usage_ratio: 0.05 } } } });
    send({ type: 'stream_event', parent_tool_use_id: null, event: { type: 'content_block_delta', delta: { type: 'text_delta', text: 'hello from qoder' } } });
    send({ type: 'stream_event', parent_tool_use_id: null, event: { type: 'message_delta', usage: { input_tokens: 10000, cache_read_input_tokens: 4000, output_tokens: 20, context_usage_ratio: 0.05 } } });
    send({
      type: 'result',
      subtype: 'success',
      is_error: false,
      result: 'hello from qoder',
      usage: { input_tokens: 10000, cache_read_input_tokens: 4000, cache_creation_input_tokens: 0, output_tokens: 20, context_usage_ratio: 0.05 },
      modelUsage: { auto: { contextWindow: 0 } }
    });
  }
});
"#,
        )
        .unwrap();

        let driver = QoderNativeDriver::new(
            "qoder",
            Cli::at(vec!["node".into(), script.display().to_string()]),
            vec!["auto".into()],
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let spec = SessionSpec {
            agent: crossagents_core::Agent {
                node: None,
                id: crossagents_core::AgentId::new(),
                name_source: crossagents_core::NameSource::Chosen,
                topic: None,
                archived_at: None,
                home: PathBuf::new(),
                name: "qoder-test".into(),
                plugin: "qoder".into(),
                driver: Some("qoder-native".into()),
                model: Some("auto".into()),
                role: crossagents_core::Role::Worker,
                cwd: dir.path().to_path_buf(),
                config: json!({ "effort": "high", "permission_mode": "acceptEdits" }),
                status: crossagents_core::AgentStatus::Stopped,
                created_at: chrono::Utc::now(),
                profile: Default::default(),
            },
            mcp_servers: vec![],
            permission: crossagents_core::PermissionPolicy::Bypass,
            permissions: None,
            command: None,
            instructions: None,
            env: vec![
                ("QODER_CONFIG_DIR".into(), dir.path().display().to_string()),
                ("HTTP_PROXY".into(), String::new()),
                ("HTTPS_PROXY".into(), String::new()),
                ("ALL_PROXY".into(), String::new()),
            ],
            events: tx,
        };
        let session = driver.open(spec).await.unwrap();
        let outcome = session.prompt("hi").await.unwrap();
        assert_eq!(outcome.stop_reason, crossagents_core::StopReason::EndTurn);
        session.set_model(Some("ultimate"), Some("xhigh")).await.unwrap();
        session.set_model(None, Some("low")).await.unwrap();
        session.shutdown().await;

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev.kind);
        }
        assert!(events.iter().any(|k| matches!(
            k,
            crossagents_core::SessionEventKind::Output { stream: crossagents_core::OutputStream::Text, text } if text == "hello from qoder"
        )));
        assert!(events.iter().any(|k| matches!(
            k,
            crossagents_core::SessionEventKind::Usage { used: 10_020, size: 200_000, tokens: Some(t), .. }
                if t.input == 10_000 && t.cached_input == 4_000 && t.output == 20
        )), "{events:?}");
        assert!(events.iter().any(|k| matches!(
            k,
            crossagents_core::SessionEventKind::ContextBreakdown { parts }
                if parts.len() == 3 && parts[0].id == "system_prompt" && parts[0].tokens == 4_000
        )), "{events:?}");
        assert!(events.iter().any(|k| matches!(
            k,
            crossagents_core::SessionEventKind::RateLimits { limits }
                if limits.plan.as_deref() == Some("pro") && limits.windows.len() == 1
        )), "{events:?}");
    }
}
