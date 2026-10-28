//! `with-native`: With's `with-cli` through `chat -o stream-json` (docs/DESIGN.md §9,
//! docs/research/with-cli.md).
//!
//! Each turn is one `with-cli chat` process. The conversation lives on With's side under
//! the session id passed as `--sessionId`, so a turn in a new process carries on where the
//! last one stopped: resuming a session is opening it again under the same id, and there is
//! no process to keep between turns (nor to hand over when the hub restarts).
//!
//! - Model and effort go as `--model` / `--reasoning-effort` on every turn, so changing them
//!   between turns needs nothing from the CLI. Thinking is asked for (`--enable-thinking`)
//!   unless the agent's `config.thinking` is `false`.
//! - The collaboration MCP servers go in a client configuration of the session's own
//!   (`-c`): a copy of the user's `~/.bg-agent/bg-agent-client.yaml` whose
//!   `mcp_config_path` names an MCP file listing the user's servers plus the session's. A
//!   configuration given with `-c` replaces the user's whole file, hence the copy.
//! - The opening instructions go as `--user-rules`.
//! - Stopping a turn is `with-cli cancel --session-id --request-id`; the process then exits
//!   without `RUN_FINISHED`. Killing it is the fallback.
//! - with-cli never asks before acting: permission requests cannot be answered, only its
//!   access mode chosen (`--access-mode`; full access when the session bypasses
//!   permissions).
//!
//! With's ACP server (`with-cli acp`) was the other way in; on v0.31.0 every tool call fails
//! there, so it is not used.

pub mod wire;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crossagents_core::{
    AttachmentKind, AttachmentSupport, Capabilities, CliStatus, CoreError, Driver, DriverKind,
    DriverSession, McpServerSpec, PermissionPolicy, Prompt, SessionEvent, SessionEventKind,
    SessionSpec, StopReason, TokenUsage, TurnOutcome,
};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::launch::{LaunchSpec, Launched, ProcessHandle};
use crate::locate::Cli;
use crate::process::{hide_console, resolve_program};
use wire::{ChatArgs, Line, Turn};

/// How long `with-cli model list` gets.
const MODEL_LIST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long `with-cli cancel` gets, and then the turn's process to end, before it is killed.
const CANCEL_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the process gets to exit once its output has ended.
const EXIT_TIMEOUT: Duration = Duration::from_secs(30);
/// What with-cli waits for an MCP tool call, in seconds: collaboration tools run commands
/// (`timeout_secs` up to 600 by default) and wait on other agents.
const MCP_TIMEOUT_SECS: u64 = 1800;
/// Plain lines kept for an error report.
const TEXT_LINES: usize = 20;

/// The `with-native` driver for one Profile.
pub struct WithNativeDriver {
    name: String,
    cli: Cli,
    /// From the description or `config.toml`.
    models: Vec<String>,
    /// `models` were written by hand: offered as they are.
    pinned: bool,
    /// What `with-cli model list` answered, once asked.
    listed: Arc<Mutex<Vec<String>>>,
    missing: Option<String>,
}

impl WithNativeDriver {
    pub fn new(name: impl Into<String>, cli: Cli, models: Vec<String>) -> Self {
        let name = name.into();
        let missing = cli.missing_message(&name);
        Self {
            name,
            cli,
            models,
            pinned: false,
            listed: Arc::default(),
            missing,
        }
    }

    /// `models` were listed by hand, so the CLI's own list does not replace them.
    pub fn pinned(mut self, pinned: bool) -> Self {
        self.pinned = pinned;
        self
    }

    /// What launches the CLI for `spec`: its own command, else the one found.
    fn command(&self, spec: &SessionSpec) -> Result<Vec<String>, CoreError> {
        if let Some(command) = spec.command.clone().filter(|c| !c.is_empty()) {
            return Ok(command);
        }
        self.cli.command().ok_or_else(|| {
            CoreError::Agent(
                self.missing
                    .clone()
                    .unwrap_or_else(|| format!("the {} CLI is not installed", self.name)),
            )
        })
    }

    async fn session(
        &self,
        session_id: String,
        spec: SessionSpec,
    ) -> Result<Box<dyn DriverSession>, CoreError> {
        let command = self.command(&spec)?;
        Ok(Box::new(WithSession::new(session_id, command, spec)?))
    }

    /// Asks the CLI which models it offers. Best effort.
    async fn list_models(&self) {
        let Some(command) = self.cli.command() else {
            return;
        };
        let Some((program, extra)) = command.split_first() else {
            return;
        };
        let mut child = Command::new(resolve_program(program));
        child
            .args(extra)
            .args(["model", "list"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        hide_console(&mut child);
        let Ok(Ok(output)) = tokio::time::timeout(MODEL_LIST_TIMEOUT, child.output()).await else {
            tracing::debug!("with-cli model list did not answer");
            return;
        };
        let models = wire::parse_model_list(&String::from_utf8_lossy(&output.stdout));
        if !models.is_empty() {
            *self.listed.lock().expect("model list") = models;
        }
    }
}

#[async_trait]
impl Driver for WithNativeDriver {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> DriverKind {
        DriverKind::WithNative
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            interrupt: true,
            mcp: true,
            multi_session: false,
            resume: true,
            // Not checked against With yet: S1 moves only the native CLIs it was tried with.
            move_cwd: false,
            fork: false,
            rollback: false,
            revert_files: false,
            set_model: true,
            structured_diff: false,
            usage: true,
            rate_limits: false,
            permission_callback: false,
            compaction: false,
            // `--images` and `--files`; a PDF would only be a path its file tools read as
            // text.
            attachments: Some(AttachmentSupport {
                image: true,
                pdf: false,
                text: true,
            }),
        }
    }

    fn models(&self) -> Vec<String> {
        if !self.pinned {
            let listed = self.listed.lock().expect("model list");
            if !listed.is_empty() {
                return listed.clone();
            }
        }
        self.models.clone()
    }

    fn installed(&self) -> bool {
        self.cli.installed()
    }

    async fn cli_status(&self) -> Option<CliStatus> {
        Some(self.cli.status().await)
    }

    /// Asked at hub startup, in the background: the version, then the model list.
    async fn probe_cli(&self) {
        let _ = self.cli.version().await;
        if !self.pinned {
            self.list_models().await;
        }
    }

    fn refresh_cli(&self) {
        self.cli.forget();
    }

    fn cli_places(&self) -> Vec<String> {
        self.cli.searched()
    }

    async fn open(&self, spec: SessionSpec) -> Result<Box<dyn DriverSession>, CoreError> {
        self.session(uuid::Uuid::new_v4().to_string(), spec).await
    }

    /// The conversation is With's to keep: the same id carries on.
    async fn resume(
        &self,
        session_id: &str,
        spec: SessionSpec,
    ) -> Result<Box<dyn DriverSession>, CoreError> {
        self.session(session_id.to_owned(), spec).await
    }
}

/// What the following turns are run with.
#[derive(Debug, Clone, Default)]
struct Settings {
    model: Option<String>,
    effort: Option<String>,
    thinking: Option<bool>,
    access_mode: Option<String>,
}

/// The turn running now.
#[derive(Clone)]
struct Running {
    process: Arc<ProcessHandle>,
    request_id: Arc<Mutex<Option<String>>>,
}

pub struct WithSession {
    id: String,
    command: Vec<String>,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    files: SessionFiles,
    settings: Mutex<Settings>,
    events: mpsc::UnboundedSender<SessionEvent>,
    totals: Mutex<TokenUsage>,
    turn: tokio::sync::Mutex<()>,
    running: Mutex<Option<Running>>,
    cancelled: AtomicBool,
    closed: AtomicBool,
}

fn emit(events: &mpsc::UnboundedSender<SessionEvent>, kind: SessionEventKind) {
    let _ = events.send(kind.into());
}

impl WithSession {
    fn new(id: String, command: Vec<String>, spec: SessionSpec) -> Result<Self, CoreError> {
        let config = &spec.agent.config;
        let text = |key: &str| {
            config
                .get(key)
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        let access_mode = text("access_mode").or_else(|| {
            (spec.permission == PermissionPolicy::Bypass).then(|| "full_access".to_owned())
        });
        if spec.permission != PermissionPolicy::Bypass && spec.permissions.is_some() {
            emit(
                &spec.events,
                SessionEventKind::Warning {
                    message: "with-cli does not ask before acting; its access mode limits it \
                              instead"
                        .into(),
                },
            );
        }
        let files = SessionFiles::write(
            &id,
            &spec.mcp_servers,
            spec.instructions.as_deref(),
            &spec.events,
        )?;
        emit(
            &spec.events,
            SessionEventKind::Started {
                session_id: id.clone(),
            },
        );
        Ok(Self {
            settings: Mutex::new(Settings {
                model: spec.agent.model.clone().filter(|m| !m.is_empty()),
                effort: text("effort"),
                // with-cli thinks only when asked (`--enable-thinking`, off by default), and
                // a session that shows no reasoning leaves the person guessing what it is
                // doing. Models without thinking ignore it; `thinking: false` turns it off.
                thinking: Some(config.get("thinking").and_then(Value::as_bool).unwrap_or(true)),
                access_mode,
            }),
            id,
            command,
            cwd: spec.agent.cwd.clone(),
            env: spec.env,
            files,
            events: spec.events,
            totals: Mutex::default(),
            turn: tokio::sync::Mutex::new(()),
            running: Mutex::new(None),
            cancelled: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        })
    }

    fn emit(&self, kind: SessionEventKind) {
        emit(&self.events, kind);
    }

    async fn run(&self, prompt: &Prompt) -> Result<TurnOutcome, CoreError> {
        let _turn = self.turn.lock().await;
        if self.closed.load(Ordering::SeqCst) {
            return Err(CoreError::Agent("the session has ended".into()));
        }
        self.cancelled.store(false, Ordering::SeqCst);

        let mut images = Vec::new();
        let mut files = Vec::new();
        for attachment in &prompt.attachments {
            match attachment.kind() {
                Some(AttachmentKind::Image) => images.push(attachment.path.clone()),
                _ => files.push(attachment.path.clone()),
            }
        }
        let mut text = prompt.text.clone();
        // A batch file cannot take a newline in its arguments (Rust refuses to start it), so
        // when the CLI is still behind one the message goes in a file like a long one does.
        let through_batch = self.command.first().is_some_and(|program| {
            let lower = program.to_ascii_lowercase();
            lower.ends_with(".cmd") || lower.ends_with(".bat")
        }) && text.contains(['\r', '\n']);
        if through_batch || text.encode_utf16().count() > wire::PROMPT_ARG_LIMIT {
            let path = self
                .files
                .dir
                .join(format!("message-{}.md", uuid::Uuid::new_v4()));
            std::fs::write(&path, &text)
                .map_err(|e| CoreError::Agent(format!("cannot write {}: {e}", path.display())))?;
            text = format!(
                "The message cannot go on the command line, so it is in the attached file {}. \
                 Read all of it and treat it as the message itself.",
                path.display()
            );
            files.push(path);
        }
        let settings = self.settings.lock().expect("settings").clone();
        let args = wire::chat_args(&ChatArgs {
            session_id: &self.id,
            cwd: &self.cwd,
            prompt: &text,
            model: settings.model.as_deref(),
            effort: settings.effort.as_deref(),
            thinking: settings.thinking,
            access_mode: settings.access_mode.as_deref(),
            config: self.files.config.as_deref(),
            rules: self.files.rules.as_deref(),
            images: &images,
            files: &files,
        });
        let (program, extra) = self
            .command
            .split_first()
            .ok_or_else(|| CoreError::Agent("empty with-cli launch command".into()))?;
        let mut all_args = extra.to_vec();
        all_args.extend(args);

        self.emit(SessionEventKind::TurnStarted { turn_id: None });
        let launched = crate::launch::launch(LaunchSpec {
            program: resolve_program(program),
            args: all_args,
            cwd: self.cwd.clone(),
            env: self.env.clone(),
            label: "with-cli".into(),
            ..LaunchSpec::default()
        })
        .await;
        let Launched {
            stdin,
            stdout,
            stderr,
            process,
        } = match launched {
            Ok(launched) => launched,
            Err(err) => {
                self.emit(SessionEventKind::Error {
                    message: err.to_string(),
                });
                self.emit(SessionEventKind::TurnCompleted {
                    turn_id: None,
                    stop_reason: StopReason::Error,
                });
                return Err(err);
            }
        };
        // The prompt is on the command line; nothing comes on stdin.
        drop(stdin);
        let running = Running {
            process: Arc::new(process),
            request_id: Arc::default(),
        };
        *self.running.lock().expect("running") = Some(running.clone());

        let mut turn = Turn::default();
        let mut text_lines = Vec::new();
        let mut lines = BufReader::new(stdout).lines();
        loop {
            let line = match lines.next_line().await {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(err) => {
                    tracing::debug!(%err, "with-cli output");
                    break;
                }
            };
            let translated = {
                let mut totals = self.totals.lock().expect("totals");
                turn.line(&line, &mut totals)
            };
            if let Some(id) = &turn.request_id {
                let mut known = running.request_id.lock().expect("request id");
                if known.is_none() {
                    *known = Some(id.clone());
                }
            }
            match translated {
                Line::Events(events) => {
                    for event in events {
                        let _ = self.events.send(event);
                    }
                }
                Line::Text(text) if text_lines.len() < TEXT_LINES => text_lines.push(text),
                _ => {}
            }
        }
        let exit = running.process.exit_within(EXIT_TIMEOUT).await;
        if exit.is_none() {
            running.process.start_kill();
        }
        *self.running.lock().expect("running") = None;

        let cancelled = self.cancelled.load(Ordering::SeqCst);
        let stop_reason = if cancelled && !turn.finished {
            StopReason::Cancelled
        } else if turn.error.is_some() {
            StopReason::Error
        } else if turn.finished {
            StopReason::EndTurn
        } else {
            let what = match exit {
                Some(exit) => exit.describe("with-cli"),
                None => "with-cli stopped answering".to_owned(),
            };
            let message = if text_lines.is_empty() {
                stderr
                    .describe(&format!("{what} before the turn ended"))
                    .await
            } else {
                format!("{what}: {}", text_lines.join("\n"))
            };
            self.emit(SessionEventKind::Error { message });
            StopReason::Error
        };
        let turn_id = turn.request_id.clone();
        self.emit(SessionEventKind::TurnCompleted {
            turn_id: turn_id.clone(),
            stop_reason,
        });
        Ok(TurnOutcome {
            stop_reason,
            turn_id,
        })
    }

    /// Asks with-cli to stop the turn, then makes sure its process ends.
    async fn stop(&self) {
        let Some(running) = self.running.lock().expect("running").clone() else {
            return;
        };
        self.cancelled.store(true, Ordering::SeqCst);
        let request_id = running.request_id.lock().expect("request id").clone();
        if let (Some(request_id), Some((program, extra))) = (request_id, self.command.split_first())
        {
            let mut cancel = Command::new(resolve_program(program));
            cancel
                .args(extra)
                .arg("cancel")
                .arg(format!("--session-id={}", self.id))
                .arg(format!("--request-id={request_id}"))
                .envs(self.env.iter().cloned())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            hide_console(&mut cancel);
            match tokio::time::timeout(CANCEL_TIMEOUT, cancel.status()).await {
                Ok(Ok(_)) => {}
                Ok(Err(err)) => tracing::debug!(%err, "with-cli cancel"),
                Err(_) => tracing::debug!("with-cli cancel did not answer"),
            }
        }
        if running.process.exit_within(CANCEL_TIMEOUT).await.is_none() {
            running.process.start_kill();
        }
    }
}

#[async_trait]
impl DriverSession for WithSession {
    fn session_id(&self) -> &str {
        &self.id
    }

    async fn prompt(&self, text: &str) -> Result<TurnOutcome, CoreError> {
        self.run(&Prompt::text(text)).await
    }

    async fn prompt_with(&self, prompt: &Prompt) -> Result<TurnOutcome, CoreError> {
        self.run(prompt).await
    }

    async fn interrupt(&self) -> Result<(), CoreError> {
        self.stop().await;
        Ok(())
    }

    /// Nothing runs between turns, so there is nothing to release.
    async fn suspend(&self) -> Result<(), CoreError> {
        if self.running.lock().expect("running").is_some() {
            return Err(CoreError::Agent("a turn is running".into()));
        }
        Ok(())
    }

    async fn set_model(&self, model: Option<&str>, effort: Option<&str>) -> Result<(), CoreError> {
        let mut settings = self.settings.lock().expect("settings");
        if let Some(model) = model {
            settings.model = Some(model.to_owned()).filter(|m| !m.is_empty());
        }
        if let Some(effort) = effort {
            settings.effort = Some(effort.to_owned()).filter(|e| !e.is_empty());
        }
        Ok(())
    }

    async fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.stop().await;
        let _guard = self.turn.lock().await;
        self.files.remove();
    }
}

/// The files a session hands with-cli: its client configuration and its rules.
#[derive(Debug, Default)]
struct SessionFiles {
    dir: PathBuf,
    config: Option<PathBuf>,
    rules: Option<PathBuf>,
}

impl SessionFiles {
    /// Writes them under `~/.bg-agent/crossagents/<session id>/`: beside the configuration
    /// they copy, which holds the user's With credentials.
    fn write(
        id: &str,
        mcp_servers: &[McpServerSpec],
        instructions: Option<&str>,
        events: &mpsc::UnboundedSender<SessionEvent>,
    ) -> Result<Self, CoreError> {
        let home = with_home()
            .ok_or_else(|| CoreError::Agent("cannot find the user's home folder".into()))?;
        Self::write_in(&home, id, mcp_servers, instructions, events)
    }

    fn write_in(
        home: &Path,
        id: &str,
        mcp_servers: &[McpServerSpec],
        instructions: Option<&str>,
        events: &mpsc::UnboundedSender<SessionEvent>,
    ) -> Result<Self, CoreError> {
        let dir = home.join("crossagents").join(id);
        let failed = |what: &Path, e: std::io::Error| {
            CoreError::Agent(format!("cannot write {}: {e}", what.display()))
        };
        std::fs::create_dir_all(&dir).map_err(|e| failed(&dir, e))?;
        let mut files = Self {
            dir: dir.clone(),
            ..Self::default()
        };
        if let Some(instructions) = instructions.filter(|i| !i.trim().is_empty()) {
            let path = dir.join("crossagents.md");
            std::fs::write(&path, instructions).map_err(|e| failed(&path, e))?;
            files.rules = Some(path);
        }
        if mcp_servers.is_empty() {
            return Ok(files);
        }
        let base_path = home.join("bg-agent-client.yaml");
        let Ok(base) = std::fs::read_to_string(&base_path) else {
            emit(
                events,
                SessionEventKind::Warning {
                    message: format!(
                        "{} is missing, so the collaboration tools are not mounted; run \
                         with-cli once to create it",
                        base_path.display()
                    ),
                },
            );
            return Ok(files);
        };
        let (yaml, user_mcp) = client_config(&base, home);
        let user = std::fs::read_to_string(&user_mcp).unwrap_or_default();
        let mcp_path = dir.join("mcp_config.json");
        let mcp = mcp_config(&user, mcp_servers);
        std::fs::write(
            &mcp_path,
            serde_json::to_string_pretty(&mcp).unwrap_or_default(),
        )
        .map_err(|e| failed(&mcp_path, e))?;
        let config_path = dir.join("bg-agent-client.yaml");
        let yaml = format!(
            "{yaml}\nmcp_config_path: '{}'\n",
            mcp_path.display().to_string().replace('\'', "''")
        );
        std::fs::write(&config_path, yaml).map_err(|e| failed(&config_path, e))?;
        files.config = Some(config_path);
        Ok(files)
    }

    fn remove(&self) {
        if let Err(err) = std::fs::remove_dir_all(&self.dir)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!(%err, dir = %self.dir.display(), "with session files");
        }
    }
}

/// `.bg-agent` in the user's home: with-cli's configuration.
fn with_home() -> Option<PathBuf> {
    let user = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(user).join(".bg-agent"))
}

/// The user's client configuration without its `mcp_config_path`, and the MCP file it
/// named (`mcp_config.json` beside it when it names none).
fn client_config(base: &str, home: &Path) -> (String, PathBuf) {
    let mut user_mcp = home.join("mcp_config.json");
    let mut kept = Vec::new();
    for line in base.lines() {
        if let Some(value) = line.strip_prefix("mcp_config_path:") {
            let value = value.trim().trim_matches(['\'', '"']);
            if !value.is_empty() {
                user_mcp = PathBuf::from(value);
            }
            continue;
        }
        kept.push(line);
    }
    (kept.join("\n"), user_mcp)
}

/// The user's MCP servers (`user`, the text of their file) with the session's added; the
/// session's win a name both use.
fn mcp_config(user: &str, servers: &[McpServerSpec]) -> Value {
    let mut config = serde_json::from_str::<Value>(user)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let root = config.as_object_mut().expect("object");
    if !root.get("mcpServers").is_some_and(Value::is_object) {
        root.insert("mcpServers".into(), Value::Object(Map::new()));
    }
    let list = root
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
        .expect("object");
    for server in servers {
        list.insert(
            server.name.clone(),
            json!({
                "description": "CrossAgents collaboration tools",
                "disabled": false,
                "timeout": MCP_TIMEOUT_SECS,
                "transportType": "stdio",
                "command": server.command.display().to_string(),
                "args": server.args,
                "env": server.env,
            }),
        );
    }
    config
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn server() -> McpServerSpec {
        McpServerSpec {
            name: "crossagents".into(),
            command: PathBuf::from(r"C:\bin\crossagents.exe"),
            args: vec!["mcp".into()],
            env: BTreeMap::from([("CROSSAGENTS_TOKEN".to_owned(), "t".to_owned())]),
        }
    }

    #[test]
    fn the_client_configuration_drops_its_mcp_path() {
        let home = Path::new(r"C:\Users\me\.bg-agent");
        let (yaml, mcp) = client_config("a: 1\nmcp_config_path: 'D:\\m.json'\nb: 2", home);
        assert_eq!(yaml, "a: 1\nb: 2");
        assert_eq!(mcp, PathBuf::from(r"D:\m.json"));
        let (_, mcp) = client_config("a: 1", home);
        assert_eq!(mcp, home.join("mcp_config.json"));
    }

    #[test]
    fn the_session_servers_join_the_users() {
        let user = r#"{"mcpServers":{"gongfeng":{"url":"https://x"},"crossagents":{"url":"old"}}}"#;
        let config = mcp_config(user, &[server()]);
        let servers = config["mcpServers"].as_object().unwrap();
        assert_eq!(servers["gongfeng"]["url"], "https://x");
        assert_eq!(servers["crossagents"]["transportType"], "stdio");
        assert_eq!(servers["crossagents"]["command"], r"C:\bin\crossagents.exe");
        assert_eq!(servers["crossagents"]["args"], json!(["mcp"]));
        assert_eq!(servers["crossagents"]["env"]["CROSSAGENTS_TOKEN"], "t");
        // An empty or broken file is no servers.
        assert_eq!(mcp_config("", &[]), json!({ "mcpServers": {} }));
    }

    #[test]
    fn session_files_copy_the_configuration() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("bg-agent-client.yaml"),
            "knot:\n  url: \"https://knot\"\nmcp_config_path: 'nowhere.json'\n",
        )
        .unwrap();
        let (events, _rx) = mpsc::unbounded_channel();
        let files =
            SessionFiles::write_in(home.path(), "s1", &[server()], Some("be nice"), &events)
                .unwrap();
        let config = std::fs::read_to_string(files.config.as_ref().unwrap()).unwrap();
        assert!(config.starts_with("knot:\n  url: \"https://knot\"\n"));
        assert!(!config.contains("nowhere.json"));
        assert!(config.contains("mcp_config_path: '"));
        let mcp: Value = serde_json::from_str(
            &std::fs::read_to_string(files.dir.join("mcp_config.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(mcp["mcpServers"]["crossagents"]["args"], json!(["mcp"]));
        assert_eq!(
            std::fs::read_to_string(files.rules.as_ref().unwrap()).unwrap(),
            "be nice"
        );
        files.remove();
        assert!(!files.dir.exists());
    }

    #[test]
    fn without_servers_there_is_no_configuration() {
        let home = tempfile::tempdir().unwrap();
        let (events, mut rx) = mpsc::unbounded_channel();
        let files = SessionFiles::write_in(home.path(), "s2", &[], None, &events).unwrap();
        assert!(files.config.is_none() && files.rules.is_none());
        // With servers but no configuration to copy: a warning, and no `-c`.
        let files = SessionFiles::write_in(home.path(), "s3", &[server()], None, &events).unwrap();
        assert!(files.config.is_none());
        assert!(matches!(
            rx.try_recv().map(|e| e.kind),
            Ok(SessionEventKind::Warning { .. })
        ));
    }

    #[test]
    fn the_cli_list_replaces_the_fallback_unless_pinned() {
        let driver =
            WithNativeDriver::new("with", Cli::at(vec!["with-cli".into()]), vec!["a".into()]);
        assert_eq!(driver.kind(), DriverKind::WithNative);
        assert_eq!(driver.models(), ["a"]);
        *driver.listed.lock().unwrap() = vec!["b".into()];
        assert_eq!(driver.models(), ["b"]);
        let pinned =
            WithNativeDriver::new("with", Cli::at(vec!["with-cli".into()]), vec!["a".into()])
                .pinned(true);
        *pinned.listed.lock().unwrap() = vec!["b".into()];
        assert_eq!(pinned.models(), ["a"]);
        let caps = driver.capabilities();
        assert!(caps.resume && caps.set_model && caps.mcp && !caps.permission_callback);
    }
}
