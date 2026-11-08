use std::collections::HashMap;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use clap::{Args, ValueEnum};
use crossagents_api_types::UsageResponse;
use crossagents_core::{AgentId, EventKind, RoundSource, TokenUsage};
use futures::StreamExt;
use serde::Serialize;

use super::Context;
use crate::output::{local_time, print_json, print_table};

#[derive(Debug, Args)]
pub struct UsageArgs {
    /// How to group agents. `round` lists the agent's turns instead (`agent.round` events).
    #[arg(long, value_enum, default_value_t = GroupArg::Role)]
    pub by: GroupArg,
    /// With `--by round`: only this agent's turns (its name, or the start of its id).
    #[arg(long, value_name = "NAME|ID")]
    pub agent: Option<String>,
    /// With `--by round`: only turns whose messages came from this run (the start of its id).
    #[arg(long, value_name = "ID")]
    pub run: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum GroupArg {
    Role,
    Model,
    Run,
    Agent,
    Round,
}

impl GroupArg {
    fn as_query(self) -> &'static str {
        match self {
            Self::Role => "role",
            Self::Model => "model",
            Self::Run => "run",
            Self::Agent => "agent",
            Self::Round => "round",
        }
    }
}

pub async fn run(ctx: &Context, args: UsageArgs) -> Result<()> {
    if matches!(args.by, GroupArg::Round) {
        return rounds(ctx, &args).await;
    }
    if args.agent.is_some() || args.run.is_some() {
        bail!("--agent and --run only narrow `--by round`");
    }
    let usage: UsageResponse = ctx
        .client()?
        .get(&format!("/v1/usage?by={}", args.by.as_query()))
        .await?;
    if ctx.json {
        return print_json(&usage);
    }
    if usage.rows.is_empty() {
        println!("no agent has run yet");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = usage
        .rows
        .iter()
        .map(|r| {
            vec![
                r.key.clone(),
                r.agents.to_string(),
                r.sessions.to_string(),
                if r.cost_reported {
                    format!("${:.4}", r.cost_usd)
                } else {
                    "-".into()
                },
                r.estimated_cost_usd
                    .map_or_else(|| "-".into(), |v| format!("${v:.4}")),
                r.cache_write_input.to_string(),
                r.requests.map_or_else(|| "-".into(), |v| v.to_string()),
                r.peak_context.to_string(),
            ]
        })
        .collect();
    let header = args.by.as_query().to_uppercase();
    print_table(
        &[
            &header,
            "AGENTS",
            "SESSIONS",
            "REPORTED",
            "ESTIMATED",
            "CACHE WRITE",
            "REQUESTS",
            "PEAK CONTEXT",
        ],
        &rows,
    );
    println!("\ntotal reported cost: ${:.4}", usage.total_cost_usd);
    if usage.rows.iter().any(|r| !r.cost_reported) {
        println!("(\"-\": the agent did not report cost)");
    }
    Ok(())
}

/// `usage --by round`: every turn the hub has ended, read from the event log the hub keeps.
/// Old logs have no `agent.round`, and then there is nothing to list.
async fn rounds(ctx: &Context, args: &UsageArgs) -> Result<()> {
    let mut stream = std::pin::pin!(ctx.client()?.events(0, false).await?);
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        let event = event?;
        events.push((event.ts, event.kind));
    }
    let report = report(&events, args.agent.as_deref(), args.run.as_deref());
    if ctx.json {
        return print_json(&report);
    }
    if report.rows.is_empty() {
        println!("no turn has ended yet (or the log predates rounds)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = report
        .rows
        .iter()
        .map(|r| {
            vec![
                local_time(r.at),
                r.agent.clone(),
                r.sources.join(", "),
                r.messages.to_string(),
                r.tools.join(", "),
                yes_no(r.decided),
                yes_no(r.prompt_via_file),
                count(r.tokens.as_ref().map(|t| t.input)),
                count(r.tokens.as_ref().map(|t| t.cached_input)),
                count(r.tokens.as_ref().map(|t| t.output)),
                count(r.tokens.as_ref().and_then(|t| t.requests)),
            ]
        })
        .collect();
    print_table(
        &[
            "WHEN",
            "AGENT",
            "FROM",
            "MESSAGES",
            "TOOLS",
            "DECIDED",
            "FILE",
            "INPUT",
            "CACHE READ",
            "OUTPUT",
            "REQUESTS",
        ],
        &rows,
    );
    println!();
    println!("turns: {}", report.summary.turns);
    println!(
        "without a decision: {}",
        tally_line(&report.summary.undecided)
    );
    println!(
        "read a message file (an extra request): {}",
        tally_line(&report.summary.read_file)
    );
    println!("(\"?\": not known; the agent did not report it)");
    Ok(())
}

fn yes_no(value: bool) -> String {
    if value { "yes" } else { "no" }.into()
}

fn count(value: Option<u64>) -> String {
    value.map_or_else(|| "?".into(), |v| v.to_string())
}

fn tally_line(tally: &Tally) -> String {
    let mut line = format!(
        "{} turn(s): input {}, cache read {}, cache write {}, output {}, requests {}",
        tally.turns, tally.input, tally.cache_read, tally.cache_write, tally.output, tally.requests,
    );
    if tally.unknown_tokens > 0 {
        line.push_str(&format!("; {} without usage", tally.unknown_tokens));
    }
    if tally.unknown_requests > 0 {
        line.push_str(&format!(
            "; {} without a request count",
            tally.unknown_requests
        ));
    }
    line
}

/// One turn, as the listing shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RoundRow {
    pub at: DateTime<Utc>,
    pub agent: String,
    pub agent_id: AgentId,
    pub messages: usize,
    pub sources: Vec<String>,
    pub notices: usize,
    pub tokens: Option<TokenUsage>,
    pub tools: Vec<String>,
    pub decided: bool,
    pub prompt_via_file: bool,
}

/// Turns that made no decision, and turns that read their message from a file. Token sums
/// count only what was reported; the rest is counted as unknown.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct Tally {
    pub turns: u64,
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    pub requests: u64,
    pub unknown_tokens: u64,
    pub unknown_requests: u64,
}

impl Tally {
    fn add(&mut self, tokens: Option<&TokenUsage>) {
        self.turns += 1;
        let Some(tokens) = tokens else {
            self.unknown_tokens += 1;
            return;
        };
        self.input += tokens.input;
        self.cache_read += tokens.cached_input;
        self.cache_write += tokens.cache_write_input;
        self.output += tokens.output;
        match tokens.requests {
            Some(requests) => self.requests += requests,
            None => self.unknown_requests += 1,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct RoundSummary {
    pub turns: u64,
    pub undecided: Tally,
    pub read_file: Tally,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RoundReport {
    pub rows: Vec<RoundRow>,
    pub summary: RoundSummary,
}

/// The turns in `events` that the filters keep, in order, with the summary. `agent` matches a
/// name (any case) or the start of an id; `run` the start of a run id, for turns whose
/// messages came from that run.
pub fn report(
    events: &[(DateTime<Utc>, EventKind)],
    agent: Option<&str>,
    run: Option<&str>,
) -> RoundReport {
    report_with(&agent_names(events), events, agent, run)
}

/// The latest name each agent had in `events`.
fn agent_names(events: &[(DateTime<Utc>, EventKind)]) -> HashMap<AgentId, String> {
    let mut names = HashMap::new();
    for (_, kind) in events {
        match kind {
            EventKind::AgentCreated { agent } | EventKind::AgentUpdated { agent } => {
                names.insert(agent.id, agent.name.clone());
            }
            _ => {}
        }
    }
    names
}

/// [`report`] with the agents names known up front.
pub fn report_with(
    names: &HashMap<AgentId, String>,
    events: &[(DateTime<Utc>, EventKind)],
    agent: Option<&str>,
    run: Option<&str>,
) -> RoundReport {
    let matches_agent = |id: AgentId| {
        let Some(wanted) = agent else {
            return true;
        };
        let name = names.get(&id).map(String::as_str).unwrap_or("");
        name.eq_ignore_ascii_case(wanted) || id.to_string().starts_with(wanted)
    };
    let matches_run = |sources: &[RoundSource]| {
        let Some(wanted) = run else {
            return true;
        };
        sources.iter().any(|source| {
            matches!(source, RoundSource::Orchestrator { run_id, .. } if run_id.to_string().starts_with(wanted))
        })
    };

    let mut report = RoundReport {
        rows: Vec::new(),
        summary: RoundSummary::default(),
    };
    for (at, kind) in events {
        let EventKind::AgentRound {
            id,
            message_ids,
            sources,
            notices,
            tokens,
            tools,
            decided,
            prompt_via_file,
        } = kind
        else {
            continue;
        };
        if !matches_agent(*id) || !matches_run(sources) {
            continue;
        }
        let tokens = tokens.clone();
        report.summary.turns += 1;
        if !decided {
            report.summary.undecided.add(tokens.as_ref());
        }
        if *prompt_via_file {
            report.summary.read_file.add(tokens.as_ref());
        }
        report.rows.push(RoundRow {
            at: *at,
            agent: names.get(id).cloned().unwrap_or_else(|| id.to_string()),
            agent_id: *id,
            messages: message_ids.len(),
            sources: sources.iter().map(source_label).collect(),
            notices: notices.len(),
            tokens,
            tools: tools.clone(),
            decided: *decided,
            prompt_via_file: *prompt_via_file,
        });
    }
    report
}

fn source_label(source: &RoundSource) -> String {
    match source {
        RoundSource::Human => "human".into(),
        RoundSource::Notice => "notice".into(),
        RoundSource::Reminder => "reminder".into(),
        RoundSource::Teammate { agent_id } => format!("teammate {agent_id}"),
        RoundSource::Orchestrator { run_id, .. } => format!("run {run_id}"),
    }
}

#[cfg(test)]
mod tests {
    use crossagents_core::{MessageId, RoundNotice, RunId};

    use super::*;

    fn tokens(input: u64, output: u64, requests: Option<u64>) -> TokenUsage {
        TokenUsage {
            input,
            cached_input: input / 2,
            cache_write_input: 0,
            output,
            reasoning_output: 0,
            total: input + output,
            requests,
        }
    }

    fn round(
        id: AgentId,
        sources: Vec<RoundSource>,
        tokens: Option<TokenUsage>,
        decided: bool,
        prompt_via_file: bool,
    ) -> (DateTime<Utc>, EventKind) {
        (
            Utc::now(),
            EventKind::AgentRound {
                id,
                message_ids: vec![MessageId::new()],
                sources,
                notices: vec![RoundNotice {
                    line: "x".into(),
                    class: None,
                }],
                tokens,
                tools: vec!["task_get".into()],
                decided,
                prompt_via_file,
            },
        )
    }

    #[test]
    fn an_old_log_without_rounds_gives_an_empty_report() {
        let report = report(&[], None, None);
        assert!(report.rows.is_empty());
        assert_eq!(report.summary, RoundSummary::default());
    }

    #[test]
    fn the_summary_counts_undecided_turns_and_file_reads_with_unknowns() {
        let kai = AgentId::new();
        let names = HashMap::from([(kai, "kai".to_owned())]);
        let events = vec![
            round(
                kai,
                vec![RoundSource::Human],
                Some(tokens(100, 10, Some(2))),
                true,
                false,
            ),
            round(
                kai,
                vec![RoundSource::Notice],
                Some(tokens(40, 4, Some(1))),
                false,
                true,
            ),
            round(kai, vec![RoundSource::Notice], None, false, true),
            round(
                kai,
                vec![RoundSource::Reminder],
                Some(tokens(7, 1, None)),
                false,
                false,
            ),
        ];
        let report = report_with(&names, &events, None, None);
        assert_eq!(report.rows.len(), 4);
        assert_eq!(report.rows[0].agent, "kai");
        assert_eq!(report.summary.turns, 4);
        let undecided = &report.summary.undecided;
        assert_eq!(undecided.turns, 3);
        assert_eq!((undecided.input, undecided.output), (47, 5));
        assert_eq!(undecided.requests, 1);
        assert_eq!(undecided.unknown_tokens, 1);
        assert_eq!(undecided.unknown_requests, 1);
        let read = &report.summary.read_file;
        assert_eq!(read.turns, 2);
        assert_eq!(read.requests, 1);
        assert_eq!(read.unknown_tokens, 1);
    }

    #[test]
    fn filters_keep_one_agent_or_the_turns_of_one_run() {
        let kai = AgentId::new();
        let lee = AgentId::new();
        let run = RunId::new();
        let other = RunId::new();
        let names = HashMap::from([(kai, "kai".to_owned()), (lee, "lee".to_owned())]);
        let events = vec![
            round(
                kai,
                vec![RoundSource::Orchestrator {
                    run_id: run,
                    task_id: None,
                }],
                None,
                true,
                false,
            ),
            round(lee, vec![RoundSource::Human], None, true, false),
            round(
                lee,
                vec![RoundSource::Orchestrator {
                    run_id: other,
                    task_id: None,
                }],
                None,
                true,
                false,
            ),
        ];
        assert_eq!(
            report_with(&names, &events, Some("KAI"), None).rows.len(),
            1
        );
        assert_eq!(
            report_with(&names, &events, Some(&lee.to_string()), None)
                .rows
                .len(),
            2
        );
        let by_run = report_with(&names, &events, None, Some(&run.to_string()));
        assert_eq!(by_run.rows.len(), 1);
        assert_eq!(by_run.rows[0].agent, "kai");
        assert_eq!(
            report_with(&names, &events, Some("nobody"), None)
                .rows
                .len(),
            0
        );
    }

    #[test]
    fn a_source_reads_as_a_word() {
        assert_eq!(source_label(&RoundSource::Reminder), "reminder");
        let lee = AgentId::new();
        assert_eq!(
            source_label(&RoundSource::Teammate { agent_id: lee }),
            format!("teammate {lee}")
        );
    }
}
