//! Talking to agents: `send`, `attach`, and looking into a session (`session diff`).

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use clap::{Args, Subcommand};
use crossagents_api_types::{
    Agent, AgentStatus, AttachmentUpload, DiffSource, Event, EventKind, MAX_ATTACHMENT_BYTES,
    MAX_ATTACHMENTS, MAX_ATTACHMENTS_TOTAL, SendMessageRequest, SendMessageResponse, Transcript,
    TurnDiffResponse, attachment_mime_ok,
};
use futures::{Stream, StreamExt};
use tokio::io::AsyncBufReadExt;

use super::Context;
use crate::client::Client;
use crate::output::print_json;

#[derive(Debug, Args)]
pub struct SendArgs {
    /// Agent name or id.
    pub agent: String,
    /// The message; several words are joined with spaces.
    #[arg(required = true, num_args = 1..)]
    pub text: Vec<String>,
    /// Print the agent's answer and wait until its turn ends.
    #[arg(long, short)]
    pub wait: bool,
    /// Send a file with the message (image, PDF, JSON or text); may be given several times.
    #[arg(long = "file", value_name = "PATH")]
    pub files: Vec<PathBuf>,
}

#[derive(Debug, Args)]
pub struct AttachArgs {
    /// Agent name or id.
    pub agent: String,
}

#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    /// Print what a turn changed, as a unified diff (the latest turn that changed files).
    Diff(DiffArgs),
}

#[derive(Debug, Args)]
pub struct DiffArgs {
    /// Agent name or id.
    pub agent: String,
    /// The turn, counting from 1 among the turns that started; negative counts from the end.
    #[arg(long, allow_hyphen_values = true)]
    pub turn: Option<i64>,
    /// Only the files and line counts.
    #[arg(long)]
    pub stat: bool,
}

pub async fn run(ctx: &Context, cmd: SessionCommand) -> Result<()> {
    match cmd {
        SessionCommand::Diff(args) => diff(ctx, args).await,
    }
}

async fn diff(ctx: &Context, args: DiffArgs) -> Result<()> {
    let client = ctx.client()?;
    let transcript: Transcript = client
        .get(&format!("/v1/agents/{}/transcript", args.agent))
        .await?;
    let started: Vec<_> = transcript
        .turns
        .iter()
        .filter(|t| t.started_at.is_some())
        .collect();
    let (number, turn) = match args.turn {
        Some(n) => {
            let index = if n < 0 {
                started.len() as i64 + n
            } else {
                n - 1
            };
            let Some(turn) = usize::try_from(index).ok().and_then(|i| started.get(i)) else {
                bail!(
                    "{} has {} turn(s); there is no turn {n}",
                    transcript.agent.name,
                    started.len()
                );
            };
            (index as usize + 1, *turn)
        }
        None => match started
            .iter()
            .enumerate()
            .rev()
            .find(|(_, t)| t.diff.is_some())
        {
            Some((i, turn)) => (i + 1, *turn),
            None => bail!("no turn of {} changed files", transcript.agent.name),
        },
    };
    if turn.diff.is_none() {
        bail!(
            "turn {number} of {} changed no files",
            transcript.agent.name
        );
    }
    let response: TurnDiffResponse = client
        .get(&format!(
            "/v1/agents/{}/turns/{}/diff",
            transcript.agent.id, turn.message_id
        ))
        .await?;
    if ctx.json {
        return print_json(&response);
    }
    if args.stat {
        let stat = &response.stat;
        for file in &stat.files {
            println!(
                "{:>5} {:>5}  {}",
                format!("+{}", file.added),
                format!("-{}", file.removed),
                file.path
            );
        }
        println!(
            "turn {number}: {} file(s), +{} -{}{}",
            stat.files.len(),
            stat.added,
            stat.removed,
            if stat.source == DiffSource::Hub {
                " (worked out by the hub)"
            } else {
                ""
            }
        );
    } else {
        print!("{}", response.diff);
    }
    Ok(())
}

pub async fn send(ctx: &Context, args: SendArgs) -> Result<()> {
    // Files are checked before anything reaches the hub, so a bad one sends nothing.
    let attachments = read_attachments(&args.files)?;
    let client = ctx.client()?;
    let agent: Agent = client.get(&format!("/v1/agents/{}", args.agent)).await?;
    let text = args.text.join(" ");
    // Subscribe before sending so the answer cannot be missed.
    let events = if args.wait {
        Some(client.live_events().await?)
    } else {
        None
    };
    let sent = post_message(&client, &agent, &text, attachments).await?;

    let Some(events) = events else {
        if ctx.json {
            return print_json(&sent);
        }
        let how = if sent.queued { "queued for" } else { "sent to" };
        println!("message {how} {}", agent.name);
        return Ok(());
    };

    let mut events = std::pin::pin!(events);
    let mut renderer = Renderer::default();
    let mut in_turn = false;
    while let Some(event) = events.next().await {
        let event = event?;
        if event.kind.agent_id() != Some(agent.id) {
            continue;
        }
        if ctx.json {
            println!("{}", serde_json::to_string(&event)?);
        }
        match &event.kind {
            EventKind::AgentTurnStarted { message_id, .. } => {
                in_turn = *message_id == sent.message_id;
            }
            EventKind::AgentTurnCompleted {
                message_id,
                stop_reason,
                ..
            } if *message_id == sent.message_id => {
                if !ctx.json {
                    renderer.end_turn(stop_reason);
                }
                return Ok(());
            }
            EventKind::AgentStatusChanged {
                to: AgentStatus::Error | AgentStatus::Stopped,
                ..
            } => bail!("agent {} stopped before answering", agent.name),
            _ if in_turn && !ctx.json => renderer.render(&event),
            _ => {}
        }
    }
    bail!("event stream ended before the agent answered")
}

pub async fn attach(ctx: &Context, args: AttachArgs) -> Result<()> {
    let client = ctx.client()?;
    let agent: Agent = client.get(&format!("/v1/agents/{}", args.agent)).await?;
    let events = client.live_events().await?;
    eprintln!(
        "attached to {} ({}). Type a message and press Enter; Ctrl+C to detach.",
        agent.name,
        crate::cmd::agent::enum_name(&agent.status)
    );

    let printer = tokio::spawn(print_agent_events(events, agent.clone()));
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => match line? {
                Some(line) if line.trim().is_empty() => {}
                Some(line) => {
                    if let Err(err) = post_message(&client, &agent, &line, Vec::new()).await {
                        eprintln!("error: {err:#}");
                    }
                }
                None => break,
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    printer.abort();
    Ok(())
}

async fn post_message(
    client: &Client,
    agent: &Agent,
    text: &str,
    attachments: Vec<AttachmentUpload>,
) -> Result<SendMessageResponse> {
    client
        .post(
            &format!("/v1/agents/{}/messages", agent.id),
            &SendMessageRequest {
                text: text.into(),
                now: None,
                steer: None,
                attachments: (!attachments.is_empty()).then_some(attachments),
            },
        )
        .await
}

/// Reads `--file`s for upload, refusing them by the hub's rules (count, size, type).
fn read_attachments(paths: &[PathBuf]) -> Result<Vec<AttachmentUpload>> {
    if paths.len() > MAX_ATTACHMENTS {
        bail!(
            "at most {MAX_ATTACHMENTS} files per message (got {})",
            paths.len()
        );
    }
    // Types and sizes first, so nothing large is read only to be refused.
    let mut files = Vec::with_capacity(paths.len());
    let mut total = 0;
    for path in paths {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let size = std::fs::metadata(path)
            .with_context(|| format!("cannot read {}", path.display()))?
            .len() as usize;
        let Some(mime) = mime_of(path).filter(|m| attachment_mime_ok(m)) else {
            bail!("{name}: files of this type cannot be sent (images, PDF, JSON and text can)");
        };
        if size > MAX_ATTACHMENT_BYTES {
            bail!(
                "{name} is {} MB; one file may be at most {} MB",
                size / (1024 * 1024),
                MAX_ATTACHMENT_BYTES / (1024 * 1024)
            );
        }
        total += size;
        files.push((path, name, mime));
    }
    if total > MAX_ATTACHMENTS_TOTAL {
        bail!(
            "the files add up to {} MB; a message may carry at most {} MB",
            total / (1024 * 1024),
            MAX_ATTACHMENTS_TOTAL / (1024 * 1024)
        );
    }
    files
        .into_iter()
        .map(|(path, name, mime)| {
            let bytes =
                std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
            Ok(AttachmentUpload {
                name,
                mime: mime.into(),
                data: STANDARD.encode(bytes),
            })
        })
        .collect()
}

/// A file's type by its extension, as the GUI guesses it (apps/gui/src/lib/attachments.ts);
/// code and logs count as plain text.
fn mime_of(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "html" => "text/html",
        "css" => "text/css",
        "txt" | "log" | "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "py" | "go"
        | "java" | "c" | "h" | "cpp" | "toml" | "yaml" | "yml" | "sh" | "ps1" | "sql" | "xml"
        | "ini" | "cfg" => "text/plain",
        _ => return None,
    })
}

async fn print_agent_events(events: impl Stream<Item = Result<Event>>, agent: Agent) {
    let mut events = std::pin::pin!(events);
    let mut renderer = Renderer::default();
    while let Some(Ok(event)) = events.next().await {
        if event.kind.agent_id() != Some(agent.id) {
            continue;
        }
        match &event.kind {
            EventKind::AgentInput { text, queued, .. } => {
                renderer.line(&format!(
                    "> {text}{}",
                    if *queued { "  (queued)" } else { "" }
                ));
            }
            EventKind::AgentTurnCompleted { stop_reason, .. } => renderer.end_turn(stop_reason),
            EventKind::AgentStatusChanged { to, .. } => {
                renderer.line(&format!("[{}]", crate::cmd::agent::enum_name(to)));
            }
            _ => renderer.render(&event),
        }
    }
}

/// Prints a turn's output as a continuous transcript.
#[derive(Default)]
struct Renderer {
    /// The cursor is mid-line after streamed text.
    mid_line: bool,
}

impl Renderer {
    fn render(&mut self, event: &Event) {
        match &event.kind {
            EventKind::AgentOutput { stream, text, .. } => {
                if *stream == crossagents_api_types::OutputStream::Thought {
                    return;
                }
                print!("{text}");
                let _ = std::io::stdout().flush();
                self.mid_line = !text.ends_with('\n');
            }
            EventKind::AgentToolCall { title, .. } => self.line(&format!("· {title}")),
            EventKind::AgentError { message, .. } => {
                self.newline();
                eprintln!("! {message}");
            }
            _ => {}
        }
    }

    fn line(&mut self, text: &str) {
        self.newline();
        println!("{text}");
    }

    fn newline(&mut self) {
        if self.mid_line {
            println!();
            self.mid_line = false;
        }
    }

    fn end_turn(&mut self, stop_reason: &crossagents_api_types::StopReason) {
        self.newline();
        if *stop_reason != crossagents_api_types::StopReason::EndTurn {
            eprintln!(
                "[turn ended: {}]",
                crate::cmd::agent::enum_name(stop_reason)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &Path, name: &str, len: usize) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, vec![b'a'; len]).unwrap();
        path
    }

    #[test]
    fn reads_files_as_the_hub_takes_them() {
        let dir = tempfile::tempdir().unwrap();
        let shot = file(dir.path(), "Shot.PNG", 3);
        let code = file(dir.path(), "main.rs", 1);
        let uploads = read_attachments(&[shot, code]).unwrap();
        assert_eq!(uploads[0].name, "Shot.PNG");
        assert_eq!(uploads[0].mime, "image/png");
        assert_eq!(uploads[0].data, "YWFh");
        assert_eq!(uploads[1].mime, "text/plain");
        assert!(read_attachments(&[]).unwrap().is_empty());
    }

    #[test]
    fn refuses_what_the_hub_would() {
        let dir = tempfile::tempdir().unwrap();
        let err = |paths: &[PathBuf]| read_attachments(paths).unwrap_err().to_string();

        let exe = file(dir.path(), "tool.exe", 1);
        assert!(err(&[exe]).contains("tool.exe: files of this type"));
        let bare = file(dir.path(), "README", 1);
        assert!(err(&[bare]).contains("README: files of this type"));

        let many: Vec<_> = (0..=MAX_ATTACHMENTS)
            .map(|i| file(dir.path(), &format!("{i}.txt"), 1))
            .collect();
        assert!(err(&many).contains("at most 8 files per message (got 9)"));

        let big = file(dir.path(), "big.log", MAX_ATTACHMENT_BYTES + 1);
        assert!(err(&[big]).contains("big.log is 10 MB; one file may be at most 10 MB"));

        let parts: Vec<_> = (0..3)
            .map(|i| file(dir.path(), &format!("part{i}.json"), 8 * 1024 * 1024))
            .collect();
        assert!(err(&parts).contains("add up to 24 MB; a message may carry at most 20 MB"));

        assert!(err(&[dir.path().join("missing.txt")]).contains("cannot read"));
    }
}
