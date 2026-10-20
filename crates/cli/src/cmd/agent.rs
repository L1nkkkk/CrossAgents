use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::{Args, Subcommand, ValueEnum};
use crossagents_api_types::{Agent, CreateAgentRequest, ListAgentsResponse, Role};

use super::Context;
use crate::output::{local_time, or_dash, print_json, print_table};

#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    /// Register a new agent.
    Add(AddArgs),
    /// List agents.
    Ls,
    /// Show one agent.
    Show { name: String },
    /// Remove an agent.
    Rm { name: String },
}

#[derive(Debug, Args)]
pub struct AddArgs {
    pub name: String,
    /// Plugin that runs the agent, e.g. `claude` or `codex`.
    #[arg(long)]
    pub plugin: String,
    /// Driver to use; defaults to the plugin's default.
    #[arg(long)]
    pub driver: Option<String>,
    /// Model, e.g. `opus` or `haiku`.
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long, value_enum)]
    pub role: RoleArg,
    /// Working directory; defaults to the current directory.
    #[arg(long)]
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RoleArg {
    Planner,
    Worker,
    Reviewer,
}

impl From<RoleArg> for Role {
    fn from(role: RoleArg) -> Self {
        match role {
            RoleArg::Planner => Role::Planner,
            RoleArg::Worker => Role::Worker,
            RoleArg::Reviewer => Role::Reviewer,
        }
    }
}

pub async fn run(ctx: &Context, cmd: AgentCommand) -> Result<()> {
    let client = ctx.client()?;
    match cmd {
        AgentCommand::Add(args) => {
            let cwd = match args.cwd {
                Some(cwd) => cwd,
                None => std::env::current_dir().context("cannot read current directory")?,
            };
            let req = CreateAgentRequest {
                name: args.name,
                plugin: args.plugin,
                driver: args.driver,
                model: args.model,
                role: args.role.into(),
                cwd: cwd.display().to_string(),
                config: serde_json::Value::Null,
            };
            let agent: Agent = client.post("/v1/agents", &req).await?;
            if ctx.json {
                return print_json(&agent);
            }
            println!("added agent {} ({})", agent.name, agent.id);
        }
        AgentCommand::Ls => {
            let list: ListAgentsResponse = client.get("/v1/agents").await?;
            if ctx.json {
                return print_json(&list);
            }
            if list.agents.is_empty() {
                println!("no agents; add one with `crossagents agent add`");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = list
                .agents
                .iter()
                .map(|a| {
                    vec![
                        a.name.clone(),
                        a.plugin.clone(),
                        or_dash(a.model.as_deref()),
                        enum_name(&a.role),
                        enum_name(&a.status),
                        a.cwd.display().to_string(),
                    ]
                })
                .collect();
            print_table(&["NAME", "PLUGIN", "MODEL", "ROLE", "STATUS", "CWD"], &rows);
        }
        AgentCommand::Show { name } => {
            let agent: Agent = client.get(&format!("/v1/agents/{name}")).await?;
            if ctx.json {
                return print_json(&agent);
            }
            println!("name:    {}", agent.name);
            println!("id:      {}", agent.id);
            println!("plugin:  {}", agent.plugin);
            println!("driver:  {}", or_dash(agent.driver.as_deref()));
            println!("model:   {}", or_dash(agent.model.as_deref()));
            println!("role:    {}", enum_name(&agent.role));
            println!("status:  {}", enum_name(&agent.status));
            println!("cwd:     {}", agent.cwd.display());
            println!("created: {}", local_time(agent.created_at));
        }
        AgentCommand::Rm { name } => {
            client.delete(&format!("/v1/agents/{name}")).await?;
            if ctx.json {
                return print_json(&serde_json::json!({ "removed": name }));
            }
            println!("removed agent {name}");
        }
    }
    Ok(())
}

/// The serde name of a unit enum, e.g. `Role::Worker` -> `worker`.
pub fn enum_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}
