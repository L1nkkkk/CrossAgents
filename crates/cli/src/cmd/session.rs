//! Talking to agents: `send` and `attach`.

use std::io::Write;

use anyhow::{Result, bail};
use clap::Args;
use crossagents_api_types::{
    Agent, AgentStatus, Event, EventKind, SendMessageRequest, SendMessageResponse,
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
}

#[derive(Debug, Args)]
pub struct AttachArgs {
    /// Agent name or id.
    pub agent: String,
}

pub async fn send(ctx: &Context, args: SendArgs) -> Result<()> {
    let client = ctx.client()?;
    let agent: Agent = client.get(&format!("/v1/agents/{}", args.agent)).await?;
    let text = args.text.join(" ");
    // Subscribe before sending so the answer cannot be missed.
    let events = if args.wait {
        Some(client.live_events().await?)
    } else {
        None
    };
    let sent = post_message(&client, &agent, &text).await?;

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
                    if let Err(err) = post_message(&client, &agent, &line).await {
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

async fn post_message(client: &Client, agent: &Agent, text: &str) -> Result<SendMessageResponse> {
    client
        .post(
            &format!("/v1/agents/{}/messages", agent.id),
            &SendMessageRequest { text: text.into() },
        )
        .await
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
