use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::{Args, Subcommand, ValueEnum};
use crossagents_api_types::{Agent, AgentProfile, CreateAgentRequest, ListAgentsResponse, Role};

use super::Context;
use crate::output::{local_time, or_dash, print_json, print_table};

#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    /// Register a new agent.
    Add(Box<AddArgs>),
    /// List agents.
    Ls,
    /// Show one agent.
    Show { name: String },
    /// Remove an agent (stops it first).
    Rm { name: String },
    /// Start the agent's session.
    Start { name: String },
    /// Stop the agent's session.
    Stop { name: String },
    /// Interrupt the agent's current turn.
    Cancel { name: String },
}

#[derive(Debug, Args)]
pub struct AddArgs {
    pub name: String,
    /// Plugin that runs the agent, e.g. `claude`, `codex` or `codebuddy`.
    #[arg(long)]
    pub plugin: String,
    /// Driver to use: `codex-native`, `claude-native`, `codebuddy-native` or `acp-generic`;
    /// defaults to the plugin's `driver` (native for `claude`, `codex` and `codebuddy`,
    /// `acp-generic` otherwise).
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
    /// Account the agent runs under (see `crossagents account ls`).
    #[arg(long)]
    pub account: Option<String>,
    /// Name shown instead of `name`, e.g. "Ada".
    #[arg(long)]
    pub display_name: Option<String>,
    /// What it does, e.g. "设计师" or "Security reviewer".
    #[arg(long)]
    pub title: Option<String>,
    /// An emoji or up to two letters.
    #[arg(long)]
    pub avatar: Option<String>,
    /// Avatar colour: indigo, violet, rose, amber, emerald, teal, sky or slate.
    #[arg(long)]
    pub color: Option<String>,
    /// Expertise and working style, added to its opening instructions.
    #[arg(long)]
    pub persona: Option<String>,
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
            let args = *args;
            let cwd = match args.cwd {
                Some(cwd) => cwd,
                None => std::env::current_dir().context("cannot read current directory")?,
            };
            let req = CreateAgentRequest {
                name: Some(args.name),
                topic: None,
                plugin: args.plugin,
                driver: args.driver,
                model: args.model,
                role: args.role.into(),
                cwd: cwd.display().to_string(),
                config: match &args.account {
                    Some(account) => serde_json::json!({ "account": account }),
                    None => serde_json::Value::Null,
                },
                profile: AgentProfile {
                    display_name: args.display_name,
                    title: args.title,
                    avatar: args.avatar,
                    color: args.color,
                    persona: args.persona,
                },
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
        AgentCommand::Start { name } => {
            let agent: Agent = client
                .post(&format!("/v1/agents/{name}/start"), &serde_json::json!({}))
                .await?;
            if ctx.json {
                return print_json(&agent);
            }
            println!("{} is {}", agent.name, enum_name(&agent.status));
        }
        AgentCommand::Stop { name } => {
            client
                .post_empty(&format!("/v1/agents/{name}/stop"))
                .await?;
            if !ctx.json {
                println!("stopped {name}");
            }
        }
        AgentCommand::Cancel { name } => {
            client
                .post_empty(&format!("/v1/agents/{name}/cancel"))
                .await?;
            if !ctx.json {
                println!("cancelled the current turn of {name}");
            }
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
