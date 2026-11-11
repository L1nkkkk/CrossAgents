//! `cursor-native`: one `agent -p` process per turn, with Cursor persisting the chat id.
//! Session MCP configuration lives only in an ephemeral Agent Plugin, never in the repo
//! or the user's Cursor configuration. Opening instructions are prepended once.
//!
//! On Windows `agent` is `agent.cmd`, and that runs `cursor-agent.ps1` to choose
//! `node.exe` and `index.js`. A newline cannot be an argument of the batch file, so
//! when that pair is found the turn starts them directly and the prompt stays an
//! argument. A prompt past the command-line limit still goes in a file, and so does
//! a newline when the pair cannot be found. The CLI does not read the prompt from stdin.

pub mod wire;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crossagents_core::{
    Capabilities, CliStatus, CoreError, Driver, DriverKind, DriverSession, McpServerSpec,
    PermissionPolicy, Prompt, SessionEvent, SessionEventKind, SessionSpec, StopReason, TokenUsage,
    TurnOutcome,
};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::launch::{LaunchSpec, Launched, ProcessHandle};
use crate::locate::Cli;
use wire::{ChatArgs, Line, Turn};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);

fn validate_platform_permission(
    windows: bool,
    permission: PermissionPolicy,
) -> Result<(), CoreError> {
    if windows && permission != PermissionPolicy::Bypass {
        return Err(CoreError::Agent("Cursor sandbox is supported only on macOS and Linux. To use Cursor on Windows, enable bypass for this agent: it runs without a sandbox or per-action confirmation, and commands run as your user.".into()));
    }
    Ok(())
}

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
            usage: true,
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
        validate_platform_permission(cfg!(windows), spec.permission)?;
        let command = self.command(&spec)?;
        let (program, args) = crate::argv::launch_command(&command, vec!["create-chat".into()])
            .ok_or_else(|| CoreError::Agent("empty Cursor launch command".into()))?;
        let mut create = Command::new(program);
        create
            .args(&args)
            .current_dir(&spec.agent.cwd)
            .envs(spec.env.iter().cloned())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // spawn_tree also hides the console window on Windows.
        let (mut child, mut tree) = crossagents_core::process::spawn_tree(&mut create)
            .map_err(|e| CoreError::Agent(format!("Cursor create-chat failed: {e}")))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stderr = tokio::spawn(async move {
            let mut text = Vec::new();
            if let Some(mut stderr) = stderr {
                let _ = stderr.read_to_end(&mut text).await;
            }
            String::from_utf8_lossy(&text).trim().to_owned()
        });
        // The Cursor CLI prints the chat id but does not exit while its stdout is a pipe (it
        // only exits promptly on a TTY), so take the first non-empty line as the id and then
        // end the whole cmd/powershell/node tree instead of waiting for it to exit.
        let created = tokio::time::timeout(PROCESS_TIMEOUT, async {
            let mut first = None;
            if let Some(stdout) = stdout {
                let mut lines = BufReader::new(stdout).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) if line.trim().is_empty() => continue,
                        Ok(Some(line)) => {
                            first = Some(line);
                            break;
                        }
                        Ok(None) => break,
                        Err(e) => {
                            return Err(CoreError::Agent(format!(
                                "Cursor create-chat returned unreadable output: {e}"
                            )));
                        }
                    }
                }
            }
            if let Some(line) = first {
                // A CLI that printed an error and already exited still reports failure.
                if let Ok(Some(status)) = child.try_wait()
                    && !status.success()
                {
                    let stderr = stderr.await.unwrap_or_default();
                    return Err(CoreError::Agent(format!(
                        "Cursor create-chat {status}: {stderr}"
                    )));
                }
                return Ok(line);
            }
            let status = child
                .wait()
                .await
                .map_err(|e| CoreError::Agent(format!("Cursor create-chat failed: {e}")))?;
            if !status.success() {
                let stderr = stderr.await.unwrap_or_default();
                return Err(CoreError::Agent(format!(
                    "Cursor create-chat {status}: {stderr}"
                )));
            }
            Ok(String::new())
        })
        .await;
        tree.kill();
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        let id = created.map_err(|_| CoreError::Agent("Cursor create-chat timed out".into()))??;
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
        validate_platform_permission(cfg!(windows), spec.permission)?;
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
    tokens: Mutex<TokenUsage>,
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
                message: "Cursor CLI does not support per-action permission callbacks; on macOS and Linux, --sandbox=enabled limits it instead (--force is used only for bypass sessions). Cursor sandbox is unavailable on Windows.".into(),
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
            tokens: Mutex::new(TokenUsage::default()),
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
        if crate::argv::prompt_via_file(self.command.first().map(String::as_str), &text) {
            text = self.files.message(&text)?;
            self.emit(SessionEventKind::PromptViaFile);
        }
        Ok(text)
    }

    /// Emit one cumulative `Usage` event from the terminal `result` event's
    /// `usage` object (present in both installed CLI versions). When the CLI
    /// omitted `usage`, emit nothing — no fabrication, no waiting, no polling.
    fn emit_usage(&self, result_usage: Option<&Value>) {
        let Some(usage) = result_usage else {
            return;
        };
        let parsed = ResultUsage::from_value(usage);
        let input = parsed.input_tokens + parsed.cache_read_tokens + parsed.cache_write_tokens;
        let output = parsed.output_tokens;
        let used = input + output;
        // `used` is this turn's input+output; `tokens` is the session cumulative
        // (the hub computes deltas from `tokens.total`). `cached_input` is a
        // subset of `input`, `reasoning_output` a subset of `output`, and `total`
        // is `input + output` — reasoning is not added to `total` again.
        let tokens = {
            let mut totals = self.tokens.lock().expect("tokens");
            totals.input += input;
            totals.cached_input += parsed.cache_read_tokens;
            totals.cache_write_input += parsed.cache_write_tokens;
            totals.output += output;
            totals.reasoning_output += parsed.reasoning_tokens;
            totals.total += used;
            totals.clone()
        };
        self.emit(SessionEventKind::Usage {
            used,
            size: 0,
            cost_usd: None,
            tokens: Some(tokens),
        });
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
        let (program, all_args) = crate::argv::launch_command(&self.command, args)
            .ok_or_else(|| CoreError::Agent("empty Cursor launch command".into()))?;
        self.emit(SessionEventKind::TurnStarted { turn_id: None });
        let launched = crate::launch::launch(LaunchSpec {
            program,
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
        // Emit one cumulative Usage event from `result.usage` before signalling
        // the turn is complete.
        self.emit_usage(turn.usage.as_ref());
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
            // Measured 2026-11-09 on CLI 2026.11.05-86047df: a `--plugin-dir` server
            // is named `plugin-${displayName}-${mcpServers key}`. The directory was
            // `ns-dirleaf`, the manifest name `ns-manifest`, displayName `ns-display`,
            // and the server key `ns-server`; the tool server was
            // `plugin-ns-display-ns-server`. The parent path was not used. Without
            // displayName the directory name is used instead, which is why a fresh
            // `crossagents-cursor-<uuid>` directory changed the namespace every
            // session. displayName stays fixed. The directory stays per session,
            // because `mcp.json` is this session's and must not be shared.
            let manifest = json!({
                "$schema": "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json",
                "name": "crossagents",
                "displayName": "crossagents"
            });
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

/// Token counts parsed from the terminal `result` event's `usage` object. The
/// installed CLIs emit camelCase field names; snake_case spellings are accepted
/// for compatibility. Missing/renamed, non-numeric, and negative values become 0.
#[derive(Default)]
struct ResultUsage {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    reasoning_tokens: u64,
}

impl ResultUsage {
    fn from_value(raw: &Value) -> Self {
        let num = |keys: &[&str]| {
            keys.iter()
                .find_map(|key| raw.get(*key))
                .and_then(parse_token)
                .unwrap_or(0)
        };
        Self {
            input_tokens: num(&["inputTokens", "input_tokens"]),
            output_tokens: num(&["outputTokens", "output_tokens"]),
            cache_read_tokens: num(&["cacheReadTokens", "cache_read_tokens"]),
            cache_write_tokens: num(&["cacheWriteTokens", "cache_write_tokens"]),
            // Observed as `reasoningTokens`; `reasoning_output_tokens` is the
            // snake_case name of the `TokenUsage` field it feeds.
            reasoning_tokens: num(&["reasoningTokens", "reasoning_output_tokens"]),
        }
    }
}

fn parse_token(value: &Value) -> Option<u64> {
    let n = value.as_f64()?;
    (n.is_finite() && n >= 0.0 && n.fract() == 0.0).then_some(n as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locate::{Install, Locator};
    use crossagents_core::{Agent, AgentId, AgentStatus, Role};
    use serde_json::Value;
    use std::collections::BTreeMap;

    #[test]
    fn platform_permission_validation() {
        for permission in [PermissionPolicy::Guard, PermissionPolicy::Deny] {
            let error = validate_platform_permission(true, permission)
                .unwrap_err()
                .to_string();
            assert!(error.contains("only on macOS and Linux"));
            assert!(error.contains("enable bypass"));
            assert!(error.contains("without a sandbox or per-action confirmation"));
            assert!(error.contains("commands run as your user"));
            assert!(validate_platform_permission(false, permission).is_ok());
        }
        assert!(validate_platform_permission(true, PermissionPolicy::Bypass).is_ok());
        assert!(validate_platform_permission(false, PermissionPolicy::Bypass).is_ok());
    }

    fn spec(cwd: &Path) -> (SessionSpec, mpsc::UnboundedReceiver<SessionEvent>) {
        let (events, rx) = mpsc::unbounded_channel();
        (
            SessionSpec {
                agent: Agent {
                    node: None,
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
                "-ExecutionPolicy".into(),
                "Bypass".into(),
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
            if let SessionEventKind::Output { text: part, .. } = event.kind {
                text.push_str(&part);
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

    fn alive(pid: &str) -> bool {
        if cfg!(windows) {
            let out = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
        } else {
            std::process::Command::new("kill")
                .args(["-0", pid])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        }
    }

    // The real CLI prints the id but keeps running while stdout is a pipe.
    #[tokio::test]
    async fn create_chat_that_keeps_running_is_killed_after_the_id() {
        let root = tempfile::tempdir().unwrap();
        let pid_file = root.path().join("pid.txt");
        let command = mock_cli(
            root.path(),
            r#"
Set-Content -LiteralPath $env:CROSSAGENTS_CURSOR_TEST_PID -Value $PID
Write-Output 'lingering-chat'
Start-Sleep -Seconds 60
"#,
            r#"
echo $$ > "$CROSSAGENTS_CURSOR_TEST_PID"
printf 'lingering-chat\n'
sleep 60
"#,
        );
        let driver = CursorNativeDriver::new("cursor", Cli::at(command), vec![]);
        let (mut spec, _rx) = spec(root.path());
        spec.env.push((
            "CROSSAGENTS_CURSOR_TEST_PID".into(),
            pid_file.display().to_string(),
        ));
        let started = std::time::Instant::now();
        let session = tokio::time::timeout(Duration::from_secs(20), driver.open(spec))
            .await
            .expect("open must not wait for create-chat to exit")
            .unwrap();
        assert!(started.elapsed() < PROCESS_TIMEOUT / 2);
        assert_eq!(session.session_id(), "lingering-chat");
        let pid = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .to_owned();
        assert!(!pid.is_empty());
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while alive(&pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "create-chat process {pid} is still alive"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        session.shutdown().await;
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
        assert_eq!(
            args,
            [
                "-p",
                "--output-format=stream-json",
                "--stream-partial-output",
                "--resume=chat",
                "--trust",
                "--sandbox=enabled",
                "--workspace=w",
                "--",
                "hi"
            ]
        );
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
            json!({
                "$schema": "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json",
                "name": "crossagents",
                "displayName": "crossagents"
            })
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

    /// A fake Cursor install: `agent.cmd` plus the `versions\<date>\` layout
    /// `cursor-agent.ps1` selects. The files are empty; nothing is executed.
    fn cursor_install(root: &Path, version: &str) -> PathBuf {
        let cmd = root.join("agent.cmd");
        std::fs::write(&cmd, b"@echo off\r\n").unwrap();
        let dir = root.join("versions").join(version);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("node.exe"), b"").unwrap();
        std::fs::write(dir.join("index.js"), b"").unwrap();
        cmd
    }

    #[test]
    fn a_resolved_shim_keeps_a_newline_and_a_long_prompt_still_goes_in_a_file() {
        let root = tempfile::tempdir().unwrap();
        let cmd = cursor_install(root.path(), "2026.11.05-86047df");
        let (spec, _rx) = spec(root.path());
        let session =
            CursorSession::new("chat".into(), vec![cmd.display().to_string()], spec).unwrap();
        assert_eq!(
            session.prompt_text(&Prompt::text("one\ntwo")).unwrap(),
            "one\ntwo"
        );
        assert!(
            std::fs::read_dir(&session.files.dir).is_err(),
            "a newline must not be written out to a file"
        );
        let long = "😀".repeat(12_001);
        let text = session.prompt_text(&Prompt::text(&long)).unwrap();
        assert!(text.contains("treat it as the message itself"));
        let written = std::fs::read_dir(&session.files.dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(written.len(), 1);
        assert_eq!(std::fs::read_to_string(&written[0]).unwrap(), long);
    }

    #[test]
    fn an_unresolved_batch_file_still_puts_a_newline_in_a_file() {
        let root = tempfile::tempdir().unwrap();
        let cmd = root.path().join("agent.cmd");
        std::fs::write(&cmd, b"@echo off\r\n").unwrap();
        let (spec, _rx) = spec(root.path());
        let session =
            CursorSession::new("chat".into(), vec![cmd.display().to_string()], spec).unwrap();
        let text = session.prompt_text(&Prompt::text("one\r\ntwo")).unwrap();
        assert!(text.contains("treat it as the message itself"));
        let written = std::fs::read_dir(&session.files.dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(written.path()).unwrap(),
            "one\r\ntwo"
        );
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
                usage: true,
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

    // A mock CLI that just emits one assistant delta and a successful result, so
    // run() completes normally without touching a real Cursor install.
    fn ndjson_cli(root: &Path) -> Vec<String> {
        mock_cli(
            root,
            r#"
Write-Output '{"type":"assistant","timestamp_ms":1,"message":{"content":[{"text":"ok"}]}}'
Write-Output '{"type":"result","subtype":"success","is_error":false,"result":"ok","session_id":"chat"}'
"#,
            r#"
printf '%s\n' '{"type":"assistant","timestamp_ms":1,"message":{"content":[{"text":"ok"}]}}' '{"type":"result","subtype":"success","is_error":false,"result":"ok","session_id":"chat"}'
"#,
        )
    }

    // What a real signed-in `agent -p ... --output-format stream-json` turn
    // emits: the terminal `result` event carries a `usage` object. This is the
    // cheap, local proof that the token channel works without a real CLI.
    fn ndjson_cli_with_result_usage(
        root: &Path,
        input: u64,
        output: u64,
        reasoning: u64,
    ) -> Vec<String> {
        let assistant =
            r#"{"type":"assistant","timestamp_ms":1,"message":{"content":[{"text":"ok"}]}}"#;
        let result = json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": "ok",
            "session_id": "chat",
            "usage": {
                "inputTokens": input,
                "outputTokens": output,
                "cacheReadTokens": 0,
                "cacheWriteTokens": 0,
                "reasoningTokens": reasoning
            }
        })
        .to_string();
        let windows = format!("\nWrite-Output '{assistant}'\nWrite-Output '{result}'\n");
        let unix = format!("\nprintf '%s\\n' '{assistant}' '{result}'\n");
        mock_cli(root, &windows, &unix)
    }

    fn drain_usage(
        rx: &mut mpsc::UnboundedReceiver<SessionEvent>,
    ) -> Vec<(u64, u64, Option<f64>, Option<TokenUsage>)> {
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let SessionEventKind::Usage {
                used,
                size,
                cost_usd,
                tokens,
            } = event.kind
            {
                out.push((used, size, cost_usd, tokens));
            }
        }
        out
    }

    #[test]
    fn result_usage_parsing_is_tolerant_and_accepts_both_spellings() {
        // snake_case spellings (and the snake_case reasoning field).
        let snake = ResultUsage::from_value(&json!({
            "input_tokens": 100, "output_tokens": 20,
            "cache_read_tokens": 30, "cache_write_tokens": 5,
            "reasoning_output_tokens": 7
        }));
        assert_eq!(
            (
                snake.input_tokens,
                snake.output_tokens,
                snake.cache_read_tokens,
                snake.cache_write_tokens,
                snake.reasoning_tokens
            ),
            (100, 20, 30, 5, 7)
        );
        // camelCase, as emitted by both installed CLI versions.
        let camel = ResultUsage::from_value(&json!({
            "inputTokens": 7, "outputTokens": 8,
            "cacheReadTokens": 9, "cacheWriteTokens": 10,
            "reasoningTokens": 11
        }));
        assert_eq!(
            (
                camel.input_tokens,
                camel.output_tokens,
                camel.cache_read_tokens,
                camel.cache_write_tokens,
                camel.reasoning_tokens
            ),
            (7, 8, 9, 10, 11)
        );
        // missing / renamed / non-numeric / negative / fractional -> 0
        let bad = ResultUsage::from_value(&json!({
            "input_tokens": "nope", "output_tokens": -5,
            "cache_read_tokens": true, "cache_write_tokens": 12.5,
            "reasoningTokens": "not a number",
            "unrelated": 999
        }));
        assert_eq!(
            (
                bad.input_tokens,
                bad.output_tokens,
                bad.cache_read_tokens,
                bad.cache_write_tokens,
                bad.reasoning_tokens
            ),
            (0, 0, 0, 0, 0)
        );
        let empty = ResultUsage::from_value(&json!({}));
        assert_eq!(
            (
                empty.input_tokens,
                empty.output_tokens,
                empty.cache_read_tokens,
                empty.cache_write_tokens,
                empty.reasoning_tokens
            ),
            (0, 0, 0, 0, 0)
        );
    }

    #[tokio::test]
    async fn no_result_usage_emits_no_usage_event() {
        let root = tempfile::tempdir().unwrap();
        let (spec, mut rx) = spec(root.path());
        let session = CursorSession::new("chat".into(), ndjson_cli(root.path()), spec).unwrap();
        let outcome = session.prompt("hi").await.unwrap();
        assert_eq!(outcome.stop_reason, StopReason::EndTurn);
        assert!(drain_usage(&mut rx).is_empty(), "no fabricated usage");
        session.shutdown().await;
    }

    #[test]
    fn result_usage_counts_cache_write_without_guessing_requests() {
        let root = tempfile::tempdir().unwrap();
        let (spec, mut rx) = spec(root.path());
        let session = CursorSession::new("chat".into(), ndjson_cli(root.path()), spec).unwrap();
        session.emit_usage(Some(&serde_json::json!({"inputTokens": 10, "cacheReadTokens": 3, "cacheWriteTokens": 5, "outputTokens": 2})));
        let usages = drain_usage(&mut rx);
        let tokens = usages[0].3.as_ref().unwrap();
        assert_eq!(
            (
                tokens.input,
                tokens.cached_input,
                tokens.cache_write_input,
                tokens.requests
            ),
            (18, 3, 5, None)
        );
    }

    // Two turns of `result.usage` produce one cumulative Usage event each, with
    // tokens.total > 0 and reasoning_output accumulated as a subset of output —
    // no extra files or configuration involved.
    #[tokio::test]
    async fn result_usage_emits_cumulative_usage_per_turn() {
        let root = tempfile::tempdir().unwrap();
        let (spec, mut rx) = spec(root.path());
        let session = CursorSession::new(
            "chat".into(),
            ndjson_cli_with_result_usage(root.path(), 100, 20, 7),
            spec,
        )
        .unwrap();
        assert_eq!(
            session.prompt("one").await.unwrap().stop_reason,
            StopReason::EndTurn
        );
        let usages = drain_usage(&mut rx);
        assert_eq!(usages.len(), 1, "one Usage event per turn");
        let (used, size, cost_usd, tokens) = &usages[0];
        assert_eq!(*used, 120); // input + output
        assert_eq!(*size, 0);
        assert!(cost_usd.is_none());
        assert_eq!(
            tokens.as_ref().unwrap(),
            &TokenUsage {
                input: 100,
                cached_input: 0,
                output: 20,
                reasoning_output: 7, // subset of output, not added to total
                total: 120,
                ..TokenUsage::default()
            }
        );
        assert!(tokens.as_ref().unwrap().total > 0);

        // A second turn on the same session keeps the cumulative counter.
        assert_eq!(
            session.prompt("two").await.unwrap().stop_reason,
            StopReason::EndTurn
        );
        let usages = drain_usage(&mut rx);
        assert_eq!(usages.len(), 1);
        assert_eq!(usages[0].0, 120);
        let tokens = usages[0].3.as_ref().unwrap();
        assert_eq!(tokens.total, 240);
        assert_eq!(tokens.reasoning_output, 14);
        assert_eq!(tokens.output, 40);
        session.shutdown().await;
    }
}
