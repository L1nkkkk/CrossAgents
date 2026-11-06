//! `crossagents` command line. Every command except `hub run` talks to the hub's local API.
//!
//! A program that was started under a shim's name (`cargo`, docs/tasks/LOCKS.md §4.6) is not
//! this command line at all: `main` hands it to `cmd::shim` before clap sees the arguments.

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
    /// Work groups: the rounds (runs) each one has, one after another.
    #[command(subcommand)]
    Work(cmd::work::WorkCommand),
    /// Reported cost and context use, grouped by role, model, run or agent.
    Usage(cmd::usage::UsageArgs),
    /// Expertise profiles: what good work looks like for this person and this project.
    #[command(subcommand)]
    Expertise(cmd::expertise::ExpertiseCommand),
    /// Troubleshooting records: problems earlier agents hit in a project and what they did.
    #[command(subcommand)]
    Lessons(cmd::lesson::LessonsCommand),
    /// Vendor accounts: login, plan, quota, token use.
    #[command(subcommand)]
    Account(cmd::account::AccountCommand),
    /// Guided setup of a vendor: account, login, models and agents.
    Setup(cmd::setup::SetupArgs),
    /// Run a command under the local resource locks (docs/tasks/LOCKS.md §4).
    Exec(cmd::exec::ExecArgs),
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
    // A program that was started as `cargo` (a shim, docs/tasks/LOCKS.md §4.6) has no
    // `crossagents` arguments to parse: everything after its name belongs to the real program.
    let argv0 = std::env::args_os().next().unwrap_or_default();
    if let Some(name) = crossagents_hub::shims::invoked_as(&argv0) {
        return cmd::shim::main(name);
    }
    let cli = Cli::parse();
    if let Command::Hub(cmd::hub::HubCommand::Run) = &cli.command {
        mark_hub_home();
    }
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("error: cannot start async runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    // `exec` hands back the command's own exit code (and 75 for a lock timeout), so it cannot
    // go through `cmd::run`, which only knows success or failure.
    if let Command::Exec(args) = &cli.command {
        return match runtime.block_on(cmd::exec::run_cli(args.clone())) {
            Ok(code) => ExitCode::from(code.rem_euclid(256) as u8),
            Err(err) => {
                eprintln!("error: {err:#}");
                ExitCode::FAILURE
            }
        };
    }
    match runtime.block_on(cmd::run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// `hub run` notes its home in its own environment for every process it starts (runtime
/// hosts, agents, their shells), so they can tell which hub runs them (docs/tasks/DOGFOOD.md
/// §1). Done before any other thread exists.
#[allow(unsafe_code)]
fn mark_hub_home() {
    let Ok(paths) = crossagents_hub::Paths::from_env() else {
        return;
    };
    // SAFETY: called first thing in `main`, before the runtime or any thread starts.
    unsafe { std::env::set_var(crossagents_hub::HUB_HOME_ENV, &paths.home) };
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
    fn parses_expertise_export() {
        let cli = Cli::try_parse_from([
            "crossagents",
            "expertise",
            "export",
            "--owner",
            "project:p1",
            "--domain",
            "rust",
        ])
        .unwrap();
        let Command::Expertise(cmd::expertise::ExpertiseCommand::Export(args)) = cli.command else {
            panic!("wrong command");
        };
        assert_eq!(args.owner, "project:p1");
        assert_eq!(args.domain.as_deref(), Some("rust"));

        // The person's profile by default.
        let cli = Cli::try_parse_from(["crossagents", "expertise", "export"]).unwrap();
        let Command::Expertise(cmd::expertise::ExpertiseCommand::Export(args)) = cli.command else {
            panic!("wrong command");
        };
        assert_eq!(args.owner, "person:me");
        assert_eq!(args.domain, None);
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
