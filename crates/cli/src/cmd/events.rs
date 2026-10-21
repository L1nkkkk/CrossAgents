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
        EventKind::AgentUpdated { agent } => format!(
            "{} ({})",
            agent.name,
            or_dash(agent.profile.title.as_deref())
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
        EventKind::AgentInputWithdrawn { id, message_id } => {
            format!("{} withdrew {}", short(id), short(message_id))
        }
        EventKind::AgentInputUpdated { id, text, .. } => {
            format!("{} digest {}", short(id), one_line(text))
        }
        EventKind::HumanAsked { item } | EventKind::HumanAnswered { item } => format!(
            "{} {} {}{}",
            item.id,
            enum_name(&item.status),
            one_line(&item.text),
            item.answer
                .as_deref()
                .map(|a| format!(" -> {}", one_line(a)))
                .unwrap_or_default()
        ),
        EventKind::HumanDismissed { id } => id.clone(),
        EventKind::TaskProgress { key, text, .. } => format!("#{key} {}", one_line(text)),
        EventKind::TaskCommand {
            key,
            command,
            exit_code,
            duration_ms,
            ..
        } => format!(
            "#{key} $ {} -> {} ({duration_ms}ms)",
            one_line(command),
            exit_code.map_or("none".into(), |c| c.to_string())
        ),
        EventKind::RunBudget { id, what, percent } => {
            format!("{} {what} {percent}%", short_tail(id))
        }
        EventKind::ReminderFired { owner, note, .. } => {
            format!("{} {}", short(owner), one_line(note))
        }
        EventKind::WatchTriggered { owner, line, .. } => {
            format!("{} {}", short(owner), one_line(line))
        }
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
        EventKind::AgentToolOutput {
            id,
            tool_call_id,
            text,
        } => format!("{} {tool_call_id} {}", short(id), one_line(text)),
        EventKind::AgentToolDetail {
            id,
            tool_call_id,
            detail,
        } => {
            let mut parts = Vec::new();
            if let Some(code) = detail.exit_code {
                parts.push(format!("exit {code}"));
            }
            if let Some(ms) = detail.duration_ms {
                parts.push(format!("{ms}ms"));
            }
            if !detail.changes.is_empty() {
                parts.push(format!("{} file(s)", detail.changes.len()));
            }
            if let Some(error) = &detail.error {
                parts.push(one_line(error));
            }
            format!("{} {tool_call_id} {}", short(id), parts.join(", "))
        }
        EventKind::AgentTurnDiff { id, diff } => {
            format!("{} diff, {} line(s)", short(id), diff.lines().count())
        }
        EventKind::AgentPermission {
            id,
            title,
            decision,
            ..
        } => format!(
            "{} {} -> {}",
            short(id),
            one_line(title),
            enum_name(decision)
        ),
        EventKind::AgentRateLimits { id, limits } => {
            let windows: Vec<String> = limits
                .windows
                .iter()
                .map(|w| format!("{} {:.0}%", w.name, w.used_percent))
                .collect();
            format!("{} {}", short(id), windows.join(", "))
        }
        EventKind::AgentWarning { id, message } => format!("{} {}", short(id), one_line(message)),
        EventKind::AgentTurnCompleted {
            id, stop_reason, ..
        } => format!("{} {}", short(id), enum_name(stop_reason)),
        EventKind::AgentUsage { id, used, size, .. } => {
            format!("{} context {used}/{size}", short(id))
        }
        EventKind::AgentError { id, message } => format!("{} {}", short(id), one_line(message)),
        EventKind::AgentSessionStarted { id, session_id } => {
            format!("{} vendor session {session_id}", short(id))
        }
        EventKind::AccountCreated { account } => {
            format!(
                "{} ({}, {})",
                account.id,
                account.provider,
                account.home.display()
            )
        }
        EventKind::AccountRemoved { id } | EventKind::AccountQuotaUpdated { id } => id.clone(),
        EventKind::AccountLoginPending { id, url } => {
            format!(
                "{id} {}",
                url.as_deref().unwrap_or("(waiting for the browser)")
            )
        }
        EventKind::AccountLoginFinished { id, ok, message } => format!(
            "{id} {}{}",
            if *ok { "logged in" } else { "FAILED" },
            message
                .as_deref()
                .map(|m| format!(": {}", one_line(m)))
                .unwrap_or_default()
        ),
        EventKind::RunCreated { run } => {
            format!("{} {}", short_tail(&run.id), one_line(&run.title))
        }
        EventKind::RunStatusChanged { id, from, to, note } => format!(
            "{} {} -> {}{}",
            short_tail(id),
            enum_name(from),
            enum_name(to),
            note.as_deref()
                .map(|n| format!(" ({})", one_line(n)))
                .unwrap_or_default()
        ),
        EventKind::RunPlanSubmitted { id, tasks } => {
            format!(
                "{} {} task(s): {}",
                short_tail(id),
                tasks.len(),
                tasks.join(", ")
            )
        }
        EventKind::TaskStatusChanged {
            run_id,
            key,
            from,
            to,
            ..
        } => format!(
            "{}/{key} {} -> {}",
            short_tail(run_id),
            enum_name(from),
            enum_name(to)
        ),
        EventKind::TaskChecked {
            run_id,
            key,
            passed,
            checks,
            ..
        } => format!(
            "{}/{key} {} ({} check(s))",
            short_tail(run_id),
            if *passed { "passed" } else { "FAILED" },
            checks.len()
        ),
    }
}

/// The last 8 hex digits of an id, as used in branch names.
pub fn short_tail(id: &impl ToString) -> String {
    let hex = id.to_string().replace('-', "");
    hex[hex.len().saturating_sub(8)..].to_owned()
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
