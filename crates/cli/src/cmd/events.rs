use anyhow::Result;
use clap::Args;
use crossagents_api_types::{Event, EventKind};
use futures::StreamExt;

use super::Context;
use super::agent::enum_name;
use crate::output::{local_time, or_dash};

#[derive(Debug, Args)]
pub struct EventsArgs {
    /// Only events after this sequence number.
    #[arg(long, default_value_t = 0)]
    pub since: u64,
    /// Keep printing new events as they happen.
    #[arg(long, short)]
    pub follow: bool,
}

pub async fn run(ctx: &Context, args: EventsArgs) -> Result<()> {
    let stream = ctx.client()?.events(args.since, args.follow).await?;
    let mut stream = std::pin::pin!(stream);
    while let Some(event) = stream.next().await {
        let event = event?;
        if ctx.json {
            println!("{}", serde_json::to_string(&event)?);
        } else {
            println!("{}", format_event(&event));
        }
    }
    Ok(())
}

fn format_event(event: &Event) -> String {
    format!(
        "#{:<5} {}  {:<24} {}",
        event.seq,
        local_time(event.ts),
        event.kind.type_name(),
        summary(&event.kind)
    )
}

/// One line describing the event, without its type.
pub fn summary(kind: &EventKind) -> String {
    match kind {
        EventKind::HubStarted { version } => format!("version {version}"),
        EventKind::HubStopping => String::new(),
        EventKind::AgentCreated { agent } => format!(
            "{} ({}, {}, {})",
            agent.name,
            agent.plugin,
            or_dash(agent.model.as_deref()),
            enum_name(&agent.role)
        ),
        EventKind::AgentRemoved { name, .. } => name.clone(),
        EventKind::AgentStatusChanged { id, from, to } => {
            format!("{} {} -> {}", short(id), enum_name(from), enum_name(to))
        }
        EventKind::AgentInput {
            id, text, queued, ..
        } => format!(
            "{}{} {}",
            short(id),
            if *queued { " (queued)" } else { "" },
            one_line(text)
        ),
        EventKind::AgentTurnStarted { id, .. } => short(id),
        EventKind::AgentOutput { id, stream, text } => {
            format!("{} [{}] {}", short(id), enum_name(stream), one_line(text))
        }
        EventKind::AgentToolCall {
            id, title, status, ..
        } => format!("{} {} ({})", short(id), title, enum_name(status)),
        EventKind::AgentToolCallUpdated {
            id,
            tool_call_id,
            status,
            ..
        } => format!(
            "{} {} {}",
            short(id),
            tool_call_id,
            status.map(|s| enum_name(&s)).unwrap_or_default()
        ),
        EventKind::AgentTurnCompleted {
            id, stop_reason, ..
        } => format!("{} {}", short(id), enum_name(stop_reason)),
        EventKind::AgentUsage { id, used, size, .. } => {
            format!("{} context {used}/{size}", short(id))
        }
        EventKind::AgentError { id, message } => format!("{} {}", short(id), one_line(message)),
    }
}

/// The first 8 characters of an id: enough to tell agents apart in a log.
fn short(id: &impl ToString) -> String {
    id.to_string().chars().take(8).collect()
}

fn one_line(text: &str) -> String {
    const MAX: usize = 100;
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > MAX {
        format!("{}…", flat.chars().take(MAX).collect::<String>())
    } else {
        flat
    }
}
