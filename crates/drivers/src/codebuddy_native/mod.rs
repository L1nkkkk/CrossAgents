//! `codebuddy-native`: WorkBuddy's CodeBuddy CLI over its stream-json control protocol
//! (docs/DESIGN.md §9).
//!
//! CodeBuddy Code is a fork of Claude Code and keeps its control protocol: `control_request`
//! / `control_response`, `can_use_tool` permission requests, `--resume`, `--fork-session`,
//! `--mcp-config`, `--permission-mode`. Sessions therefore run on [`crate::claude_native`]'s
//! implementation unchanged; what this module adds is the program to launch and the models
//! it accepts.
//!
//! It needs the standalone CLI (`npm install -g @tencent-ai/codebuddy-code`), signed in once
//! with `/login` or given `CODEBUDDY_API_KEY`. The copy inside WorkBuddy is not used: it is a
//! build without the interactive CLI or `/login`, and it only gets credentials from the
//! WorkBuddy app that launches it, so started on its own it waits forever without a word.
//! A standalone CLI that is not signed in ends the turn at once with "Authentication
//! required", which the session reports as the turn's error.

use std::path::{Path, PathBuf};
use std::process::Stdio;
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

/// How long [`probe`] waits for the CLI to answer `initialize`.
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

/// The `codebuddy-native` driver for one Profile.
pub struct CodebuddyNativeDriver {
    inner: ClaudeNativeDriver,
    /// What to say when a session is asked of a CLI this machine does not have.
    missing: Option<String>,
}

impl CodebuddyNativeDriver {
    /// `cli` is the description's CLI: `CODEBUDDY_CLI` when it is set, else `codebuddy` on
    /// `PATH` (run through `node` when npm installed it as a `.cmd` wrapper). The
    /// stream-json arguments are added to it. Where it is is found per session, not here.
    pub fn new(name: impl Into<String>, cli: Cli, models: Vec<String>) -> Self {
        let name = name.into();
        let missing = cli.missing_message(&name);
        Self {
            inner: ClaudeNativeDriver::with_flavor(name, cli, models, Flavor::Codebuddy),
            missing,
        }
    }

    /// Refuses a session no CLI can run, saying what to install, instead of failing to
    /// spawn `codebuddy`. Asked per session: the CLI may have been installed (or removed)
    /// since the hub started.
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
impl Driver for CodebuddyNativeDriver {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn kind(&self) -> DriverKind {
        DriverKind::CodebuddyNative
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn models(&self) -> Vec<String> {
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
    }

    /// The wrapper must forward every default-implemented method too, or takeover, refresh
    /// and the lookup report silently stop working for CodeBuddy (docs/DESIGN.md §9.1).
    fn refresh_cli(&self) {
        self.inner.refresh_cli();
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

/// Makes session `session_id` resumable from `cwd`. CodeBuddy keeps each session under
/// `projects/<cwd as a folder name>/` of its home and looks only in the folder of the
/// directory it runs in, so resuming or forking into another working directory (a fork
/// gets its own worktree) answers "No conversation found". The transcript is copied there
/// from wherever it is; a session already there is left alone. `env` is the session's
/// own, which may move the home.
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
            "cannot copy CodeBuddy session {session_id} to {}: {err}",
            target.display()
        );
    }
}

/// Writes session `from`, up to and including its message `at` (all of it without one), as
/// session `id` in `cwd`'s folder, ready to be resumed: a fork made by hand, since the
/// CLI's own `--fork-session` names the fork itself.
pub(crate) fn fork_transcript(
    from: &str,
    id: &str,
    at: Option<&str>,
    cwd: &Path,
    env: &[(String, String)],
) -> Result<(), CoreError> {
    let projects = home(env)
        .map(|home| home.join("projects"))
        .ok_or_else(|| CoreError::Agent("cannot find CodeBuddy's home".into()))?;
    let source = find_transcript(&projects, from).ok_or_else(|| CoreError::NotFound {
        kind: "codebuddy session",
        id: from.to_owned(),
    })?;
    let text = std::fs::read_to_string(&source)
        .map_err(|e| CoreError::Agent(format!("cannot read {}: {e}", source.display())))?;
    let mut lines = Vec::new();
    let mut found = at.is_none();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(mut entry) = serde_json::from_str::<Value>(line) else {
            lines.push(line.to_owned());
            continue;
        };
        if entry.get("sessionId").and_then(Value::as_str) == Some(from) {
            entry["sessionId"] = id.into();
        }
        lines.push(entry.to_string());
        if at.is_some() && entry.get("id").and_then(Value::as_str) == at {
            found = true;
            break;
        }
    }
    if !found {
        return Err(CoreError::NotFound {
            kind: "message",
            id: at.unwrap_or_default().to_owned(),
        });
    }
    let target = projects
        .join(project_folder(cwd))
        .join(format!("{id}.jsonl"));
    target
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&target, lines.join("\n") + "\n"))
        .map_err(|e| CoreError::Agent(format!("cannot write {}: {e}", target.display())))
}

/// `<projects>/<any folder>/<session_id>.jsonl`.
fn find_transcript(projects: &Path, session_id: &str) -> Option<PathBuf> {
    let file = format!("{session_id}.jsonl");
    std::fs::read_dir(projects)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path().join(&file))
        .find(|path| path.is_file())
}

/// CodeBuddy's home for a session with environment `env`: its `CODEBUDDY_CONFIG_DIR`, else
/// [`default_home`] (of its user's home, when `env` moves that). An inherited
/// `CODEBUDDY_CONFIG_DIR` does not count: sessions are started without it.
fn home(env: &[(String, String)]) -> Option<PathBuf> {
    let set = |key: &str| {
        env.iter()
            .rev()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| PathBuf::from(v))
            .filter(|v| !v.as_os_str().is_empty())
    };
    if let Some(dir) = set("CODEBUDDY_CONFIG_DIR") {
        return Some(dir);
    }
    match set(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        Some(user) => Some(user.join(".codebuddy")),
        None => default_home(),
    }
}

/// `.codebuddy` in the user's home: where the CLI keeps its settings and transcripts when
/// nothing moves it.
pub fn default_home() -> Option<PathBuf> {
    let user = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(user).join(".codebuddy"))
}

/// The inherited variables a CodeBuddy process is started without (see
/// [`Flavor::withholds`]), for processes this module does not start itself.
pub fn withheld_env() -> Vec<String> {
    std::env::vars_os()
        .map(|(key, _)| key.to_string_lossy().into_owned())
        .filter(|key| Flavor::Codebuddy.withholds(key))
        .collect()
}

/// Prepare a CodeBuddy child's environment. Empty auth overrides mean "use this
/// profile's saved login", but the CLI treats even an empty AUTH_TOKEN as an
/// explicit credential. Remove those variables, including inherited values,
/// rather than passing empty strings. Nonempty credentials remain supported.
pub fn prepare_env(env: &mut Vec<(String, String)>) -> Vec<String> {
    let mut remove = withheld_env();
    env.retain(|(key, value)| {
        let empty_auth = value.is_empty()
            && ["CODEBUDDY_API_KEY", "CODEBUDDY_AUTH_TOKEN"]
                .iter()
                .any(|auth| key.eq_ignore_ascii_case(auth));
        if empty_auth {
            remove.push(key.clone());
        }
        !empty_auth
    });
    remove
}

/// The arguments that make the CLI serve its web UI on `port` of this machine without a
/// password. The UI carries the login (the CLI itself logs in only inside its TUI).
pub fn serve_args(port: u16) -> Vec<String> {
    vec![
        "--serve".into(),
        "--port".into(),
        port.to_string(),
        "--auth".into(),
        "none".into(),
    ]
}

/// Asks the CLI who is signed in, without starting a session: its `initialize` answer,
/// which carries `account` (null when signed out) and `models`. `command` launches the CLI;
/// `env` is added to what it inherits, of which the enclosing session's `CODEBUDDY_*` is
/// withheld as for sessions.
pub async fn probe(command: &[String], env: &[(String, String)]) -> Result<Value, CoreError> {
    let (program, extra) = command
        .split_first()
        .ok_or_else(|| CoreError::Agent("empty codebuddy launch command".into()))?;
    // The CLI cannot answer `initialize` without reaching its API through the proxy.
    let api_host = Flavor::Codebuddy.api_host(env);
    // The hub passes the account's policy in `env`; a CodeBuddy login is domestic and keeps
    // the direct fallback unless the account says otherwise (docs/ACCOUNT-RISK.md).
    let egress = EgressPolicy::from_env(env)
        .unwrap_or_else(|| EgressPolicy::default_for_provider(Flavor::Codebuddy.program()));
    let direct = check_proxy(env, Some(&api_host), egress).await?;
    let mut process_env = system_proxy_env();
    process_env.extend(without_internal_env(env));
    process_env.extend(direct);
    let mut child = Command::new(program);
    for key in prepare_env(&mut process_env) {
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
            "--verbose",
            "--strict-mcp-config",
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
        let mut line =
            wire::control_request("init", json!({ "subtype": "initialize" })).to_string();
        line.push('\n');
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await? {
            if let Ok(message) = serde_json::from_str::<Value>(&line)
                && let Some((id, answer)) = wire::control_answer(&message)
                && id == "init"
            {
                return Ok(answer);
            }
        }
        Err(std::io::Error::other("codebuddy exited before answering"))
    };
    let answer = tokio::time::timeout(PROBE_TIMEOUT, ask).await;
    let _ = child.start_kill();
    match answer {
        Ok(Ok(Ok(init))) => Ok(init),
        Ok(Ok(Err(e))) => Err(CoreError::Agent(e)),
        Ok(Err(e)) => Err(CoreError::Agent(e.to_string())),
        Err(_) => Err(CoreError::Agent("codebuddy did not answer".into())),
    }
}

/// The folder CodeBuddy files a working directory's sessions under: the path with its
/// separators as `-`, the drive letter in lower case and its colon dropped
/// (`C:\Users\me\repo` → `c-Users-me-repo`).
fn project_folder(cwd: &Path) -> String {
    let path = cwd.display().to_string();
    let path = path.strip_prefix(r"\\?\").unwrap_or(&path);
    let mut chars = path.chars();
    let (first, second) = (chars.next(), chars.next());
    let path = match (first, second) {
        (Some(drive), Some(':')) if drive.is_ascii_alphabetic() => {
            format!("{}{}", drive.to_ascii_lowercase(), chars.as_str())
        }
        _ => path.to_owned(),
    };
    path.replace(['\\', '/'], "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_auth_overrides_remove_credentials_instead_of_setting_empty_values() {
        let mut env = vec![
            ("CODEBUDDY_API_KEY".into(), String::new()),
            ("codebuddy_auth_token".into(), String::new()),
            ("CODEBUDDY_CONFIG_DIR".into(), "/private/login".into()),
            ("OTHER_EMPTY".into(), String::new()),
        ];
        let remove = prepare_env(&mut env);
        let mut command = std::process::Command::new("unused");
        // Simulate credentials inherited from the parent without mutating this
        // test process's environment (other account tests run concurrently).
        command.env("CODEBUDDY_API_KEY", "parent-key");
        command.env("codebuddy_auth_token", "parent-token");
        for key in remove {
            command.env_remove(key);
        }
        command.envs(env);
        let overrides: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        for key in ["CODEBUDDY_API_KEY", "codebuddy_auth_token"] {
            assert_eq!(overrides.get(std::ffi::OsStr::new(key)), Some(&None));
        }
        assert_eq!(
            overrides[std::ffi::OsStr::new("CODEBUDDY_CONFIG_DIR")],
            Some(std::ffi::OsStr::new("/private/login"))
        );
        assert_eq!(
            overrides[std::ffi::OsStr::new("OTHER_EMPTY")],
            Some(std::ffi::OsStr::new(""))
        );
    }

    #[test]
    fn explicit_and_default_api_logins_are_preserved() {
        let mut env = vec![
            ("CODEBUDDY_API_KEY".into(), "explicit-key".into()),
            ("CODEBUDDY_AUTH_TOKEN".into(), "explicit-token".into()),
        ];
        let original = env.clone();
        let remove = prepare_env(&mut env);
        assert_eq!(env, original);
        for key in ["CODEBUDDY_API_KEY", "CODEBUDDY_AUTH_TOKEN"] {
            assert!(
                !remove
                    .iter()
                    .any(|removed| removed.eq_ignore_ascii_case(key))
            );
        }
        // No account overrides: the machine's normal API login is inherited.
        let remove = prepare_env(&mut Vec::new());
        for key in ["CODEBUDDY_API_KEY", "CODEBUDDY_AUTH_TOKEN"] {
            assert!(
                !remove
                    .iter()
                    .any(|removed| removed.eq_ignore_ascii_case(key))
            );
        }
    }

    #[test]
    fn runs_on_the_codebuddy_driver_and_has_a_command() {
        let configured = CodebuddyNativeDriver::new(
            "codebuddy",
            Cli::at(vec!["my-cbc".into()]),
            vec!["hy3".into()],
        );
        assert_eq!(configured.kind(), DriverKind::CodebuddyNative);
        assert_eq!(configured.name(), "codebuddy");
        assert_eq!(configured.models(), ["hy3"]);
        assert!(configured.missing.is_none());
    }

    /// A session asked of a CLI this machine does not have says what the description says:
    /// what to install, how to sign in, and why not WorkBuddy's copy.
    #[test]
    fn a_missing_cli_says_how_to_install_itself() {
        let install: crate::locate::Install = toml::from_str(
            r#"
            label = "CodeBuddy"
            hint = "npm install -g @tencent-ai/codebuddy-code"
            login = "then sign in once with `codebuddy` and `/login` (or set CODEBUDDY_API_KEY)"
            notes = "The copy inside WorkBuddy cannot run outside the WorkBuddy app."
            "#,
        )
        .unwrap();
        let locator: crate::locate::Locator =
            toml::from_str(r#"commands = ["crossagents-no-such-codebuddy"]"#).unwrap();
        let driver = CodebuddyNativeDriver::new(
            "codebuddy",
            Cli::new(Vec::new(), locator, install),
            Vec::new(),
        );
        assert!(!driver.installed());
        assert_eq!(
            driver.missing.as_deref(),
            Some(
                "the CodeBuddy CLI is not installed: run `npm install -g @tencent-ai/codebuddy-code`, \
                 then sign in once with `codebuddy` and `/login` (or set CODEBUDDY_API_KEY). \
                 The copy inside WorkBuddy cannot run outside the WorkBuddy app."
            )
        );
    }

    #[test]
    fn project_folders_are_named_as_codebuddy_names_them() {
        // As found under ~/.codebuddy/projects.
        assert_eq!(
            project_folder(Path::new(r"C:\Users\Link\AppData\Local\Temp\.tmpHvXO7x")),
            "c-Users-Link-AppData-Local-Temp-.tmpHvXO7x"
        );
        assert_eq!(
            project_folder(Path::new(r"\\?\C:\Users\Link\Temp\cbc a.b_c~é")),
            "c-Users-Link-Temp-cbc a.b_c~é"
        );
    }

    #[test]
    fn a_session_is_copied_to_the_folder_of_its_new_directory() {
        let home = tempfile::tempdir().unwrap();
        let env = [(
            "CODEBUDDY_CONFIG_DIR".to_owned(),
            home.path().display().to_string(),
        )];
        let old = home.path().join("projects").join("c-old");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("s1.jsonl"), "{}\n").unwrap();
        let cwd = Path::new(r"C:\work\fork");
        adopt_transcript("s1", cwd, &env);
        let adopted = home.path().join("projects").join(project_folder(cwd));
        assert_eq!(
            std::fs::read_to_string(adopted.join("s1.jsonl")).unwrap(),
            "{}\n"
        );
        // An unknown session is nobody's business.
        adopt_transcript("s2", cwd, &env);
        assert!(!adopted.join("s2.jsonl").exists());
    }

    #[test]
    fn a_fork_is_the_transcript_up_to_its_message_under_the_new_id() {
        let home = tempfile::tempdir().unwrap();
        let env = [(
            "CODEBUDDY_CONFIG_DIR".to_owned(),
            home.path().display().to_string(),
        )];
        let old = home.path().join("projects").join("c-old");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(
            old.join("s1.jsonl"),
            [
                r#"{"id":"u1","sessionId":"s1","role":"user"}"#,
                r#"{"type":"file-history-snapshot"}"#,
                r#"{"id":"a1","sessionId":"s1","role":"assistant"}"#,
                r#"{"id":"u2","sessionId":"s1","role":"user"}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let cwd = Path::new(r"C:\work\fork");
        fork_transcript("s1", "f1", Some("a1"), cwd, &env).unwrap();
        let fork = std::fs::read_to_string(
            home.path()
                .join("projects")
                .join(project_folder(cwd))
                .join("f1.jsonl"),
        )
        .unwrap();
        let ids: Vec<Value> = fork
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(ids.len(), 3);
        assert_eq!(ids[2]["id"], "a1");
        assert!(
            ids.iter()
                .all(|l| l.get("sessionId").is_none() || l["sessionId"] == "f1")
        );
        assert!(matches!(
            fork_transcript("s1", "f2", Some("nope"), cwd, &env),
            Err(CoreError::NotFound { .. })
        ));
    }
}
