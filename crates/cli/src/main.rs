//! `crossagents` command line. Every command except `hub run` talks to the hub's local API.

mod client;
mod cmd;
mod output;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "crossagents",
    version,
    about = "Orchestrate coding agents across vendors"
)]
pub struct Cli {
    /// Print machine-readable JSON instead of human-readable text.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage the hub process.
    #[command(subcommand)]
    Hub(cmd::hub::HubCommand),
    /// Manage agents.
    #[command(subcommand)]
    Agent(cmd::agent::AgentCommand),
    /// Send a message to an agent (starts it if needed).
    Send(cmd::session::SendArgs),
    /// Watch an agent live and talk to it from the terminal.
    Attach(cmd::session::AttachArgs),
    /// Look into a session: what its turns changed.
    #[command(subcommand)]
    Session(cmd::session::SessionCommand),
    /// Print the event log.
    Events(cmd::events::EventsArgs),
    /// List available agent plugins.
    #[command(subcommand)]
    Plugin(cmd::plugin::PluginCommand),
    /// Plan, approve and follow orchestrated runs.
    Run(cmd::run::RunArgs),
    /// Reported cost and context use, grouped by role, model, run or agent.
    Usage(cmd::usage::UsageArgs),
    /// Vendor accounts: login, plan, quota, token use.
    #[command(subcommand)]
    Account(cmd::account::AccountCommand),
    /// Guided setup of a vendor: account, login, models and agents.
    Setup(cmd::setup::SetupArgs),
    /// Serve the collaboration tools over MCP on stdio (started by the hub for agents).
    #[command(hide = true)]
    Mcp,
    /// Run one agent process so it outlives the hub (started by the hub).
    #[command(hide = true)]
    RuntimeHost(RuntimeHostArgs),
}

#[derive(Debug, clap::Args)]
pub struct RuntimeHostArgs {
    /// Where hosts keep their records; the spec is `<dir>/<id>.spec.json`.
    #[arg(long)]
    pub dir: std::path::PathBuf,
    #[arg(long)]
    pub id: String,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("error: cannot start async runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(cmd::run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_agent_add() {
        let cli = Cli::try_parse_from([
            "crossagents",
            "agent",
            "add",
            "alice",
            "--plugin",
            "claude",
            "--model",
            "haiku",
            "--role",
            "worker",
            "--json",
        ])
        .unwrap();
        assert!(cli.json);
        let Command::Agent(cmd::agent::AgentCommand::Add(args)) = cli.command else {
            panic!("wrong command");
        };
        assert_eq!(args.name, "alice");
        assert_eq!(args.model.as_deref(), Some("haiku"));
    }

    #[test]
    fn rejects_unknown_role() {
        assert!(
            Cli::try_parse_from([
                "crossagents",
                "agent",
                "add",
                "a",
                "--plugin",
                "p",
                "--role",
                "boss"
            ])
            .is_err()
        );
    }
}
