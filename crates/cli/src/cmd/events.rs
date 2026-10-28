use anyhow::{Context as _, Result, bail};
use chrono::{NaiveDate, Utc};
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
    /// Read the log the hub wrote instead of asking the hub: today's, or the given date
    /// (`--log 2026-10-23`). Works when the hub is not running.
    #[arg(long, value_name = "DATE", num_args = 0..=1, default_missing_value = "today")]
    pub log: Option<String>,
}

pub async fn run(ctx: &Context, args: EventsArgs) -> Result<()> {
    if let Some(date) = &args.log {
        if args.follow {
            bail!("--log reads what is already on disk; it cannot follow");
        }
        return replay(ctx, date, args.since);
    }
    let stream = ctx.client()?.events(args.since, args.follow).await?;
    let mut stream = std::pin::pin!(stream);
    while let Some(event) = stream.next().await {
        let event = event?;
        print(ctx, &event)?;
    }
    Ok(())
}

fn print(ctx: &Context, event: &Event) -> Result<()> {
    if ctx.json {
        println!("{}", serde_json::to_string(event)?);
    } else {
        println!("{}", format_event(event));
    }
    Ok(())
}

/// Prints events the hub already wrote to `logs/events-<date>.jsonl`. A hub that is still
/// writing may leave the last line half-written, so an unparsable line is skipped, not fatal.
fn replay(ctx: &Context, date: &str, since: u64) -> Result<()> {
    let date = if date.eq_ignore_ascii_case("today") {
        Utc::now().date_naive()
    } else {
        NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .with_context(|| format!("expected a date like 2026-10-23, got `{date}`"))?
    };
    let path = crossagents_hub::event_log::file(&ctx.paths.logs(), date);
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "cannot read {}; is event logging on (`[hub] event_log`)?",
            path.display()
        )
    })?;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(event) = serde_json::from_str::<Event>(line) else {
            continue;
        };
        if event.seq > since {
            print(ctx, &event)?;
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
        EventKind::AgentModelChanged {
            id, model, effort, ..
        } => format!(
            "{} model {} effort {}",
            short(id),
            model.as_deref().unwrap_or("default"),
            effort.as_deref().unwrap_or("default")
        ),
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
        EventKind::NoteWritten { key, text, .. } => format!(
            "{key} {}",
            text.as_deref()
                .map(one_line)
                .unwrap_or_else(|| "(deleted)".into())
        ),
        EventKind::TaskProgress { key, text, .. } => format!("#{key} {}", one_line(text)),
        EventKind::TaskRerouted { key, reason, .. } => format!("#{key} {}", one_line(reason)),
        EventKind::TaskHandedOff { handoff, .. } => format!(
            "#{} {} → {}",
            handoff.task_key,
            handoff.from_name.as_deref().unwrap_or("?"),
            one_line(&handoff.link())
        ),
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
        EventKind::RunLanded { id, landing } => format!(
            "{} {} by {} -> {}",
            short_tail(id),
            enum_name(&landing.mode),
            landing.by,
            landing.reference
        ),
        EventKind::RunBudget { id, what, percent } => {
            format!("{} {what} {percent}%", short_tail(id))
        }
        EventKind::BuildCache { used, limit } => format!(
            "build output {} GB of a {} GB soft budget",
            used >> 30,
            limit >> 30
        ),
        EventKind::RunDeleted { run_id } => short_tail(run_id),
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
        EventKind::AgentTurnDiff {
            id, diff, source, ..
        } => {
            let by = match source {
                Some(crossagents_api_types::DiffSource::Hub) => " (hub)",
                _ => "",
            };
            format!("{} diff, {} line(s){by}", short(id), diff.lines().count())
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
        EventKind::AgentContext { id, parts } => {
            let used: u64 = parts
                .iter()
                .filter(|p| p.kind == "used")
                .map(|p| p.tokens)
                .sum();
            format!(
                "{} context by kind: {used} used in {} parts",
                short(id),
                parts.len()
            )
        }
        EventKind::AgentError { id, message } => format!("{} {}", short(id), one_line(message)),
        EventKind::AgentExited { id, message, .. } => {
            format!("{} {}", short(id), one_line(message))
        }
        EventKind::AgentAdopted { id, message_id } => match message_id {
            Some(message_id) => format!("{} taken over, turn {} goes on", short(id), message_id),
            None => format!("{} taken over", short(id)),
        },
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
        EventKind::RunContinuationChanged { id, state } => format!(
            "{} continuation {} ({} wakeups): {}",
            short_tail(id),
            state.phase,
            state.attempts,
            state.next_step
        ),
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
        EventKind::CliInstallStarted {
            plugin,
            action,
            argv,
            ..
        } => {
            format!("{plugin} {action} $ {}", argv.join(" "))
        }
        EventKind::CliInstallOutput {
            plugin,
            line,
            dropped,
            ..
        } => format!(
            "{plugin} {}{}",
            one_line(line),
            if *dropped > 0 {
                format!(" ({dropped} line(s) dropped)")
            } else {
                String::new()
            }
        ),
        EventKind::CliInstallFinished {
            plugin,
            action,
            ok,
            code,
            duration_ms,
            reason,
            dropped,
            ..
        } => {
            let mut text = format!(
                "{plugin} {action} {} ({duration_ms}ms)",
                if *ok { "ok" } else { "FAILED" }
            );
            if let Some(code) = code {
                text.push_str(&format!(" code {code}"));
            }
            if let Some(reason) = reason {
                text.push_str(&format!(": {}", one_line(reason)));
            }
            if *dropped > 0 {
                text.push_str(&format!(" [{dropped} line(s) dropped]"));
            }
            text
        }
        EventKind::PluginChanged { name } => format!("{name} re-read"),
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

pub(crate) fn one_line(text: &str) -> String {
    const MAX: usize = 100;
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > MAX {
        format!("{}…", flat.chars().take(MAX).collect::<String>())
    } else {
        flat
    }
}
