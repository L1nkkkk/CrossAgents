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
        "#{:<5} {}  {:<22} {}",
        event.seq,
        local_time(event.ts),
        event.kind.type_name(),
        summary(&event.kind)
    )
}

fn summary(kind: &EventKind) -> String {
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
            format!("{id} {} -> {}", enum_name(from), enum_name(to))
        }
    }
}
