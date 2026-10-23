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
use crossagents_core::{Capabilities, CoreError, Driver, DriverKind, DriverSession, SessionSpec};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::claude_native::ClaudeNativeDriver;
use crate::claude_native::wire::{self, Flavor};
use crate::process::{find_in_path, hide_console, system_proxy_env};

/// How long [`probe`] waits for the CLI to answer `initialize`.
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

/// The `codebuddy-native` driver for one Profile.
pub struct CodebuddyNativeDriver {
    inner: ClaudeNativeDriver,
    /// No command was configured and no CLI was found: sessions cannot start.
    missing: bool,
}

impl CodebuddyNativeDriver {
    /// `command` launches the CLI — `codebuddy`, or `node <script>`. Empty means
    /// [`default_command`]. The stream-json arguments are added to it.
    pub fn new(name: impl Into<String>, command: Vec<String>, models: Vec<String>) -> Self {
        let (command, missing) = if command.is_empty() {
            match default_command() {
                Some(found) => (found, false),
                None => (vec!["codebuddy".into()], true),
            }
        } else {
            (command, false)
        };
        Self {
            inner: ClaudeNativeDriver::with_flavor(name, command, models, Flavor::Codebuddy),
            missing,
        }
    }

    /// Refuses a session no CLI can run, saying what to install, instead of failing to
    /// spawn `codebuddy`.
    fn check(&self, spec: &SessionSpec) -> Result<(), CoreError> {
        if self.missing && spec.command.is_none() {
            return Err(CoreError::Agent(
                "the CodeBuddy CLI is not installed: run `npm install -g @tencent-ai/codebuddy-code`, \
                 then sign in once with `codebuddy` and `/login` (or set CODEBUDDY_API_KEY). \
                 The copy inside WorkBuddy cannot run outside the WorkBuddy app."
                    .into(),
            ));
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
        !self.missing
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

/// The CodeBuddy CLI to run: `CODEBUDDY_CLI`, else `codebuddy` on `PATH`; `None` when
/// there is neither.
pub fn default_command() -> Option<Vec<String>> {
    if let Some(cli) = std::env::var_os("CODEBUDDY_CLI").filter(|v| !v.is_empty()) {
        return Some(vec![cli.to_string_lossy().into_owned()]);
    }
    let on_path = find_in_path("codebuddy");
    if !on_path.is_file() {
        return None;
    }
    Some(match npm_script(&on_path) {
        Some(script) => vec![
            find_in_path("node").display().to_string(),
            script.display().to_string(),
        ],
        None => vec![on_path.display().to_string()],
    })
}

/// The script behind npm's `codebuddy.cmd` shim. Run through `node` directly, the
/// arguments skip `cmd.exe`'s quoting, which the JSON of `--mcp-config` does not survive.
fn npm_script(shim: &Path) -> Option<PathBuf> {
    let is_cmd = shim
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd"));
    let script = shim
        .parent()?
        .join("node_modules")
        .join("@tencent-ai")
        .join("codebuddy-code")
        .join("bin")
        .join("codebuddy");
    (is_cmd && script.is_file()).then_some(script)
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
    let mut child = Command::new(program);
    for (key, _) in std::env::vars_os() {
        if Flavor::Codebuddy.withholds(&key.to_string_lossy()) {
            child.env_remove(key);
        }
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
        .envs(system_proxy_env())
        .envs(env.iter().cloned())
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
    fn runs_on_the_codebuddy_driver_and_has_a_command() {
        let configured =
            CodebuddyNativeDriver::new("codebuddy", vec!["my-cbc".into()], vec!["hy3".into()]);
        assert_eq!(configured.kind(), DriverKind::CodebuddyNative);
        assert_eq!(configured.name(), "codebuddy");
        assert_eq!(configured.models(), ["hy3"]);
        assert!(!configured.missing);
    }

    #[test]
    fn npm_shims_run_their_script_on_node() {
        let dir = std::env::temp_dir().join(format!("cbc-shim-{}", std::process::id()));
        let bin = dir.join("node_modules/@tencent-ai/codebuddy-code/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("codebuddy"), "").unwrap();
        let shim = dir.join("codebuddy.cmd");
        assert_eq!(npm_script(&shim), Some(bin.join("codebuddy")));
        // Anything but the cmd shim is run as it is.
        assert_eq!(npm_script(&dir.join("codebuddy.exe")), None);
        let _ = std::fs::remove_dir_all(&dir);
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
