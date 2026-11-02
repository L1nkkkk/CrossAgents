//! `cursor-native`: one `agent -p` process per turn, with Cursor persisting the chat id.
//! Session MCP configuration lives only in an ephemeral Agent Plugin, never in the repo
//! or the user's Cursor configuration. Opening instructions are prepended once.

pub mod wire;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crossagents_core::{
    Capabilities, CliStatus, CoreError, Driver, DriverKind, DriverSession, McpServerSpec,
    PermissionPolicy, Prompt, SessionEvent, SessionEventKind, SessionSpec, StopReason, TurnOutcome,
};
use serde_json::{Map, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::launch::{LaunchSpec, Launched, ProcessHandle};
use crate::locate::Cli;
use crate::process::{hide_console, resolve_program};
use wire::{ChatArgs, Line, Turn};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);

pub struct CursorNativeDriver {
    name: String,
    cli: Cli,
    models: Vec<String>,
}

impl CursorNativeDriver {
    pub fn new(name: impl Into<String>, cli: Cli, models: Vec<String>) -> Self {
        Self {
            name: name.into(),
            cli,
            models,
        }
    }

    fn command(&self, spec: &SessionSpec) -> Result<Vec<String>, CoreError> {
        if let Some(command) = spec.command.clone().filter(|c| !c.is_empty()) {
            return Ok(command);
        }
        self.cli.command().ok_or_else(|| {
            CoreError::Agent(
                self.cli
                    .missing_message(&self.name)
                    .unwrap_or_else(|| format!("the {} CLI is not installed", self.name)),
            )
        })
    }
}

#[async_trait]
impl Driver for CursorNativeDriver {
    fn name(&self) -> &str {
        &self.name
    }
    fn kind(&self) -> DriverKind {
        DriverKind::CursorNative
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            interrupt: true,
            mcp: true,
            resume: true,
            set_model: true,
            ..Capabilities::default()
        }
    }
    fn models(&self) -> Vec<String> {
        self.models.clone()
    }
    fn installed(&self) -> bool {
        self.cli.installed()
    }
    async fn cli_status(&self) -> Option<CliStatus> {
        Some(self.cli.status().await)
    }
    async fn probe_cli(&self) {
        let _ = self.cli.version().await;
    }
    fn refresh_cli(&self) {
        self.cli.forget();
    }
    fn cli_places(&self) -> Vec<String> {
        self.cli.searched()
    }

    async fn open(&self, spec: SessionSpec) -> Result<Box<dyn DriverSession>, CoreError> {
        let command = self.command(&spec)?;
        let (program, extra) = command
            .split_first()
            .ok_or_else(|| CoreError::Agent("empty Cursor launch command".into()))?;
        let mut create = Command::new(resolve_program(program));
        create
            .args(extra)
            .arg("create-chat")
            .current_dir(&spec.agent.cwd)
            .envs(spec.env.iter().cloned())
            .stdin(Stdio::null())
            .kill_on_drop(true);
        hide_console(&mut create);
        let output = tokio::time::timeout(PROCESS_TIMEOUT, create.output())
            .await
            .map_err(|_| CoreError::Agent("Cursor create-chat timed out".into()))?
            .map_err(|e| CoreError::Agent(format!("Cursor create-chat failed: {e}")))?;
        if !output.status.success() {
            return Err(CoreError::Agent(format!(
                "Cursor create-chat {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let id = String::from_utf8(output.stdout).map_err(|e| {
            CoreError::Agent(format!("Cursor create-chat returned invalid UTF-8: {e}"))
        })?;
        let id = id.trim();
        if id.is_empty() || id.split_whitespace().count() != 1 {
            return Err(CoreError::Agent(
                "Cursor create-chat did not return a chat id".into(),
            ));
        }
        Ok(Box::new(CursorSession::new(id.into(), command, spec)?))
    }

    async fn resume(
        &self,
        session_id: &str,
        spec: SessionSpec,
    ) -> Result<Box<dyn DriverSession>, CoreError> {
        let command = self.command(&spec)?;
        Ok(Box::new(CursorSession::new(
            session_id.into(),
            command,
            spec,
        )?))
    }
}

pub struct CursorSession {
    id: String,
    command: Vec<String>,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    files: SessionFiles,
    model: Mutex<Option<String>>,
    instructions: Mutex<Option<String>>,
    bypass: bool,
    events: mpsc::UnboundedSender<SessionEvent>,
    turn: tokio::sync::Mutex<()>,
    running: Mutex<Option<Arc<ProcessHandle>>>,
    cancelled: AtomicBool,
    closed: AtomicBool,
}

impl CursorSession {
    fn new(id: String, command: Vec<String>, spec: SessionSpec) -> Result<Self, CoreError> {
        let files = SessionFiles::new(&spec.mcp_servers)?;
        if spec.permission != PermissionPolicy::Bypass && spec.permissions.is_some() {
            let _ = spec.events.send(SessionEventKind::Warning {
                message: "Cursor CLI does not support per-action permission callbacks; --sandbox=enabled limits it instead (--force is used only for bypass sessions).".into(),
            }.into());
        }
        let _ = spec.events.send(
            SessionEventKind::Started {
                session_id: id.clone(),
            }
            .into(),
        );
        Ok(Self {
            id,
            command,
            cwd: spec.agent.cwd,
            env: spec.env,
            files,
            model: Mutex::new(spec.agent.model.filter(|m| !m.is_empty())),
            instructions: Mutex::new(spec.instructions),
            bypass: spec.permission == PermissionPolicy::Bypass,
            events: spec.events,
            turn: tokio::sync::Mutex::new(()),
            running: Mutex::new(None),
            cancelled: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        })
    }

    fn emit(&self, kind: SessionEventKind) {
        let _ = self.events.send(kind.into());
    }

    fn prompt_text(&self, prompt: &Prompt) -> Result<String, CoreError> {
        let mut text = match self.instructions.lock().expect("instructions").take() {
            Some(instructions) => format!("{instructions}\n\n{}", prompt.text),
            None => prompt.text.clone(),
        };
        for attachment in &prompt.attachments {
            text.push_str("\n\n");
            text.push_str(&attachment.reference());
        }
        let through_batch = self.command.first().is_some_and(|program| {
            let lower = program.to_ascii_lowercase();
            lower.ends_with(".cmd") || lower.ends_with(".bat")
        }) && text.contains(['\r', '\n']);
        if through_batch || text.encode_utf16().count() > crate::with_native::wire::PROMPT_ARG_LIMIT
        {
            text = self.files.message(&text)?;
        }
        Ok(text)
    }

    async fn run(&self, prompt: &Prompt) -> Result<TurnOutcome, CoreError> {
        let _guard = self.turn.lock().await;
        if self.closed.load(Ordering::SeqCst) {
            return Err(CoreError::Agent("the session has ended".into()));
        }
        self.cancelled.store(false, Ordering::SeqCst);
        let text = self.prompt_text(prompt)?;
        let model = self.model.lock().expect("model").clone();
        let args = wire::chat_args(&ChatArgs {
            session_id: &self.id,
            cwd: &self.cwd,
            prompt: &text,
            model: model.as_deref(),
            bypass: self.bypass,
            plugin: self.files.plugin(),
        });
        let (program, extra) = self
            .command
            .split_first()
            .ok_or_else(|| CoreError::Agent("empty Cursor launch command".into()))?;
        let mut all_args = extra.to_vec();
        all_args.extend(args);
        self.emit(SessionEventKind::TurnStarted { turn_id: None });
        let launched = crate::launch::launch(LaunchSpec {
            program: resolve_program(program),
            args: all_args,
            cwd: self.cwd.clone(),
            env: self.env.clone(),
            label: "Cursor agent".into(),
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
        drop(stdin);
        let process = Arc::new(process);
        *self.running.lock().expect("running") = Some(process.clone());
        // An interrupt may have arrived while launch was awaiting process creation.
        if self.cancelled.load(Ordering::SeqCst) || self.closed.load(Ordering::SeqCst) {
            process.start_kill();
        }
        let mut turn = Turn::default();
        let mut text_lines = Vec::new();
        let mut lines = BufReader::new(stdout).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => match turn.line(&line) {
                    Line::Events(events) => {
                        for event in events {
                            let _ = self.events.send(event);
                        }
                    }
                    Line::Text(text) if text_lines.len() < 20 => text_lines.push(text),
                    _ => {}
                },
                Ok(None) => break,
                Err(err) => {
                    text_lines.push(format!("Cursor output: {err}"));
                    break;
                }
            }
        }
        let exit = process.exit_within(PROCESS_TIMEOUT).await;
        if exit.is_none() {
            process.start_kill();
        }
        *self.running.lock().expect("running") = None;
        let stop_reason = if self.cancelled.load(Ordering::SeqCst) {
            StopReason::Cancelled
        } else if let Some(reason) = turn.stop_reason {
            reason
        } else {
            let what = exit
                .map(|e| e.describe("Cursor agent"))
                .unwrap_or_else(|| "Cursor agent stopped answering".into());
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
        let turn_id = turn.request_id;
        self.emit(SessionEventKind::TurnCompleted {
            turn_id: turn_id.clone(),
            stop_reason,
        });
        Ok(TurnOutcome {
            stop_reason,
            turn_id,
        })
    }

    fn stop(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(process) = self.running.lock().expect("running").as_ref() {
            process.start_kill();
        }
    }
}

#[async_trait]
impl DriverSession for CursorSession {
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
        self.stop();
        Ok(())
    }
    async fn suspend(&self) -> Result<(), CoreError> {
        if self.turn.try_lock().is_err() {
            return Err(CoreError::Agent("a turn is running".into()));
        }
        Ok(())
    }
    async fn set_model(&self, model: Option<&str>, _effort: Option<&str>) -> Result<(), CoreError> {
        if let Some(model) = model {
            *self.model.lock().expect("model") =
                Some(model.into()).filter(|m: &String| !m.is_empty());
        }
        Ok(())
    }
    async fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.stop();
        let _guard = self.turn.lock().await;
        self.files.remove();
    }
}

struct SessionFiles {
    dir: PathBuf,
    has_plugin: bool,
}

impl SessionFiles {
    fn new(servers: &[McpServerSpec]) -> Result<Self, CoreError> {
        let files = Self {
            dir: std::env::temp_dir().join(format!("crossagents-cursor-{}", uuid::Uuid::new_v4())),
            has_plugin: !servers.is_empty(),
        };
        if files.has_plugin {
            files.create()?;
            let manifest = json!({"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json", "name":"crossagents"});
            let mut entries = Map::new();
            for server in servers {
                entries.insert(
                    server.name.clone(),
                    json!({"command":server.command,"args":server.args,"env":server.env}),
                );
            }
            for (name, content) in [
                ("plugin.json", manifest),
                ("mcp.json", json!({"mcpServers":entries})),
            ] {
                std::fs::write(files.dir.join(name), content.to_string())
                    .map_err(|e| CoreError::Agent(format!("cannot write Cursor {name}: {e}")))?;
            }
        }
        Ok(files)
    }
    fn create(&self) -> Result<(), CoreError> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| CoreError::Agent(format!("cannot create {}: {e}", self.dir.display())))
    }
    fn plugin(&self) -> Option<&Path> {
        self.has_plugin.then_some(self.dir.as_path())
    }
    fn message(&self, text: &str) -> Result<String, CoreError> {
        self.create()?;
        let path = self
            .dir
            .join(format!("message-{}.md", uuid::Uuid::new_v4()));
        std::fs::write(&path, text)
            .map_err(|e| CoreError::Agent(format!("cannot write {}: {e}", path.display())))?;
        Ok(format!(
            "The message is in the file {}. Read all of it and treat it as the message itself.",
            path.display()
        ))
    }
    fn remove(&self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Drop for SessionFiles {
    fn drop(&mut self) {
        self.remove();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locate::{Install, Locator};
    use crossagents_core::{Agent, AgentId, AgentStatus, Role};
    use serde_json::Value;
    use std::collections::BTreeMap;

    fn spec(cwd: &Path) -> (SessionSpec, mpsc::UnboundedReceiver<SessionEvent>) {
        let (events, rx) = mpsc::unbounded_channel();
        (
            SessionSpec {
                agent: Agent {
                    id: AgentId::new(),
                    name: "Cursor".into(),
                    name_source: Default::default(),
                    plugin: "cursor".into(),
                    driver: None,
                    model: Some("gpt-5".into()),
                    role: Role::Worker,
                    home: cwd.into(),
                    cwd: cwd.into(),
                    config: json!({}),
                    status: AgentStatus::Stopped,
                    created_at: chrono::Utc::now(),
                    profile: Default::default(),
                    topic: None,
                    archived_at: None,
                },
                mcp_servers: vec![],
                permission: PermissionPolicy::Bypass,
                permissions: None,
                command: None,
                instructions: None,
                env: vec![],
                events,
            },
            rx,
        )
    }

    fn server() -> McpServerSpec {
        McpServerSpec {
            name: "crossagents".into(),
            command: "hub executable".into(),
            args: vec!["mcp".into()],
            env: BTreeMap::from([("TOKEN".into(), "literal ${HOME} %USERPROFILE% 中文".into())]),
        }
    }

    // Exercise the real launcher without requiring Cursor or a signed-in account.
    fn mock_cli(root: &Path, windows: &str, unix: &str) -> Vec<String> {
        let path = root.join(if cfg!(windows) { "mock.ps1" } else { "mock.sh" });
        std::fs::write(&path, if cfg!(windows) { windows } else { unix }).unwrap();
        if cfg!(windows) {
            vec![
                "powershell".into(),
                "-NoProfile".into(),
                "-File".into(),
                path.display().to_string(),
            ]
        } else {
            vec!["sh".into(), path.display().to_string()]
        }
    }

    #[tokio::test]
    async fn open_creates_chat_and_each_turn_resumes_it() {
        let root = tempfile::tempdir().unwrap();
        let command = mock_cli(
            root.path(),
            r#"
if ($args[0] -eq 'create-chat') { Write-Output 'created-chat'; exit 0 }
if (-not ($args -contains '--resume=created-chat')) { exit 2 }
$args | Add-Content -LiteralPath $env:CROSSAGENTS_CURSOR_TEST_LOG
Write-Output '{"type":"assistant","timestamp_ms":1,"message":{"content":[{"text":"hello"}]}}'
Write-Output '{"type":"result","subtype":"success","is_error":false,"result":"hello","session_id":"created-chat"}'
"#,
            r#"
if [ "$1" = create-chat ]; then printf 'created-chat\n'; exit 0; fi
case " $* " in *' --resume=created-chat '*) ;; *) exit 2;; esac
printf '%s\n' "$@" >> "$CROSSAGENTS_CURSOR_TEST_LOG"
printf '%s\n' '{"type":"assistant","timestamp_ms":1,"message":{"content":[{"text":"hello"}]}}' '{"type":"result","subtype":"success","is_error":false,"result":"hello","session_id":"created-chat"}'
"#,
        );
        let driver = CursorNativeDriver::new("cursor", Cli::at(command), vec![]);
        let (mut spec, mut rx) = spec(root.path());
        let log = root.path().join("argv.txt");
        spec.env.push((
            "CROSSAGENTS_CURSOR_TEST_LOG".into(),
            log.display().to_string(),
        ));
        spec.instructions = Some("opening instructions".into());
        let session = driver.open(spec).await.unwrap();
        assert_eq!(session.session_id(), "created-chat");
        assert_eq!(
            session
                .prompt("--prompt starts with dash")
                .await
                .unwrap()
                .stop_reason,
            StopReason::EndTurn
        );
        session.set_model(Some("next-model"), None).await.unwrap();
        assert_eq!(
            session.prompt("second").await.unwrap().stop_reason,
            StopReason::EndTurn
        );
        let argv = std::fs::read_to_string(log).unwrap();
        assert_eq!(argv.matches("--resume=created-chat").count(), 2);
        assert_eq!(argv.matches("opening instructions").count(), 1);
        assert_eq!(argv.matches("--model=gpt-5").count(), 1);
        assert_eq!(argv.matches("--model=next-model").count(), 1);
        let mut text = String::new();
        while let Ok(event) = rx.try_recv() {
            match event.kind {
                SessionEventKind::Output { text: part, .. } => text.push_str(&part),
                SessionEventKind::Usage { .. } => panic!("no usage"),
                _ => {}
            }
        }
        assert_eq!(text, "hellohello");
        session.shutdown().await;
    }

    #[tokio::test]
    async fn failed_or_empty_create_chat_returns_agent_error() {
        for (windows, unix) in [
            (
                "[Console]::Error.WriteLine('create failed'); exit 7",
                "echo 'create failed' >&2; exit 7",
            ),
            ("exit 0", "exit 0"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let command = mock_cli(root.path(), windows, unix);
            let driver = CursorNativeDriver::new("cursor", Cli::at(command), vec![]);
            assert!(matches!(
                driver.open(spec(root.path()).0).await,
                Err(CoreError::Agent(_))
            ));
        }
    }

    #[tokio::test]
    async fn interrupt_kills_the_turn_and_reports_cancelled() {
        let root = tempfile::tempdir().unwrap();
        let command = mock_cli(root.path(), "Start-Sleep -Seconds 60", "exec sleep 60");
        let (spec, _rx) = spec(root.path());
        let session = CursorSession::new("chat".into(), command, spec).unwrap();
        let prompt = session.prompt("wait");
        let interrupt = async {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if session.running.lock().unwrap().is_some() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(session.suspend().await.is_err());
            session.interrupt().await.unwrap();
        };
        let (outcome, ()) = tokio::time::timeout(Duration::from_secs(15), async {
            tokio::join!(prompt, interrupt)
        })
        .await
        .unwrap();
        assert_eq!(outcome.unwrap().stop_reason, StopReason::Cancelled);
        session.shutdown().await;
    }

    #[test]
    fn argv_preserves_values_and_a_prompt_starting_with_dash() {
        let args = wire::chat_args(&ChatArgs {
            session_id: "chat",
            cwd: Path::new("workspace with spaces"),
            prompt: "--model=evil\nmessage",
            model: Some("gpt-5"),
            bypass: true,
            plugin: Some(Path::new("plugin dir")),
        });
        assert_eq!(
            args,
            [
                "-p",
                "--output-format=stream-json",
                "--stream-partial-output",
                "--resume=chat",
                "--model=gpt-5",
                "--force",
                "--workspace=workspace with spaces",
                "--plugin-dir=plugin dir",
                "--approve-mcps",
                "--",
                "--model=evil\nmessage"
            ]
        );
        let args = wire::chat_args(&ChatArgs {
            session_id: "chat",
            cwd: Path::new("w"),
            prompt: "hi",
            model: None,
            bypass: false,
            plugin: None,
        });
        assert!(!args.iter().any(|a| a == "--force"
            || a.starts_with("--model")
            || a.starts_with("--plugin-dir")
            || a == "--approve-mcps"
            || a.starts_with("--files")));
        assert!(args.contains(&"--sandbox=enabled".into()));
    }

    #[tokio::test]
    async fn plugin_is_private_and_shutdown_removes_it() {
        let root = tempfile::tempdir().unwrap();
        let (mut spec, _rx) = spec(root.path());
        spec.mcp_servers = vec![server()];
        let session = CursorSession::new("chat".into(), vec!["unused-cli".into()], spec).unwrap();
        let dir = session.files.dir.clone();
        assert!(dir.starts_with(std::env::temp_dir()));
        assert!(
            dir.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("crossagents-cursor-")
        );
        let manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("plugin.json")).unwrap())
                .unwrap();
        assert_eq!(
            manifest,
            json!({"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json", "name":"crossagents"})
        );
        let mcp: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("mcp.json")).unwrap()).unwrap();
        let server = server();
        assert_eq!(
            mcp,
            json!({"mcpServers":{"crossagents":{"command":server.command,"args":server.args,"env":server.env}}})
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        session.shutdown().await;
        assert!(!dir.exists());
    }

    #[test]
    fn no_servers_make_no_directory_and_long_messages_use_utf16_length() {
        let files = SessionFiles::new(&[]).unwrap();
        assert!(!files.dir.exists());
        assert!(files.plugin().is_none());
        let root = tempfile::tempdir().unwrap();
        let (mut spec, _rx) = spec(root.path());
        spec.instructions = Some("instructions".into());
        let session = CursorSession::new("chat".into(), vec!["unused-cli".into()], spec).unwrap();
        assert_eq!(
            session.prompt_text(&Prompt::text("first")).unwrap(),
            "instructions\n\nfirst"
        );
        assert_eq!(
            session.prompt_text(&Prompt::text("second")).unwrap(),
            "second"
        );
        let long = "😀".repeat(12_001);
        assert!(long.chars().count() < crate::with_native::wire::PROMPT_ARG_LIMIT);
        let text = session.prompt_text(&Prompt::text(&long)).unwrap();
        assert!(text.contains("treat it as the message itself"));
        let paths: Vec<_> = std::fs::read_dir(&session.files.dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(paths.len(), 1);
        assert!(
            paths[0]
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("message-")
        );
        assert_eq!(std::fs::read_to_string(&paths[0]).unwrap(), long);
        assert!(session.files.plugin().is_none());
        let dir = session.files.dir.clone();
        drop(session);
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn absent_cli_is_not_installed_and_open_returns_agent_error() {
        let locator: Locator = toml::from_str(r#"
            commands = ["crossagents-definitely-no-such-cli"]
            paths = ["~/crossagents-definitely-no-such-directory/crossagents-definitely-no-such-cli"]
        "#).unwrap();
        let driver = CursorNativeDriver::new(
            "cursor",
            Cli::new(vec![], locator, Install::default()),
            vec![],
        );
        assert!(!driver.installed());
        let root = tempfile::tempdir().unwrap();
        assert!(matches!(
            driver.open(spec(root.path()).0).await,
            Err(CoreError::Agent(_))
        ));
    }

    #[tokio::test]
    async fn resume_uses_supplied_id_and_model_changes_apply_to_future_turns() {
        let driver = CursorNativeDriver::new(
            "cursor",
            Cli::at(vec!["unused-cli".into()]),
            vec!["a".into()],
        );
        assert_eq!(driver.kind(), DriverKind::CursorNative);
        assert_eq!(driver.models(), ["a"]);
        assert_eq!(
            driver.capabilities(),
            Capabilities {
                streaming: true,
                interrupt: true,
                mcp: true,
                resume: true,
                set_model: true,
                ..Capabilities::default()
            }
        );
        let root = tempfile::tempdir().unwrap();
        let (spec, _rx) = spec(root.path());
        let session = driver.resume("existing-chat", spec).await.unwrap();
        assert_eq!(session.session_id(), "existing-chat");
        session.shutdown().await;
        let (spec, _rx) = self::spec(root.path());
        let session = CursorSession::new("chat".into(), vec!["unused-cli".into()], spec).unwrap();
        session.set_model(Some("new-model"), None).await.unwrap();
        assert_eq!(session.model.lock().unwrap().as_deref(), Some("new-model"));
        session.set_model(None, None).await.unwrap();
        assert_eq!(session.model.lock().unwrap().as_deref(), Some("new-model"));
    }
}
