use anyhow::Result;
use clap::{Args, ValueEnum};
use crossagents_api_types::UsageResponse;

use super::Context;
use crate::output::{print_json, print_table};

#[derive(Debug, Args)]
pub struct UsageArgs {
    /// How to group agents.
    #[arg(long, value_enum, default_value_t = GroupArg::Role)]
    pub by: GroupArg,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum GroupArg {
    Role,
    Model,
    Run,
    Agent,
}

impl GroupArg {
    fn as_query(self) -> &'static str {
        match self {
            Self::Role => "role",
            Self::Model => "model",
            Self::Run => "run",
            Self::Agent => "agent",
        }
    }
}

pub async fn run(ctx: &Context, args: UsageArgs) -> Result<()> {
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
                r.peak_context.to_string(),
            ]
        })
        .collect();
    let header = args.by.as_query().to_uppercase();
    print_table(
        &[&header, "AGENTS", "SESSIONS", "COST", "PEAK CONTEXT"],
        &rows,
    );
    println!("\ntotal reported cost: ${:.4}", usage.total_cost_usd);
    if usage.rows.iter().any(|r| !r.cost_reported) {
        println!("(\"-\": the agent did not report cost)");
    }
    Ok(())
}
