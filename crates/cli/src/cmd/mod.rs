pub mod account;
pub mod agent;
pub mod events;
pub mod hub;
pub mod mcp;
pub mod plugin;
pub mod run;
pub mod session;
pub mod setup;
pub mod usage;

use anyhow::Result;
use crossagents_hub::Paths;

use crate::client::Client;
use crate::{Cli, Command};

pub async fn run(cli: Cli) -> Result<()> {
    if let Command::Mcp = cli.command {
        return mcp::run().await;
    }
    if let Command::RuntimeHost(args) = cli.command {
        return crossagents_drivers::host::serve(args.dir, args.id)
            .await
            .map_err(anyhow::Error::msg);
    }
    let paths = Paths::from_env()?;
    let ctx = Context {
        paths,
        json: cli.json,
    };
    match cli.command {
        Command::Hub(cmd) => hub::run(&ctx, cmd).await,
        Command::Agent(cmd) => agent::run(&ctx, cmd).await,
        Command::Events(args) => events::run(&ctx, args).await,
        Command::Send(args) => session::send(&ctx, args).await,
        Command::Attach(args) => session::attach(&ctx, args).await,
        Command::Session(cmd) => session::run(&ctx, cmd).await,
        Command::Plugin(cmd) => plugin::run(&ctx, cmd).await,
        Command::Run(args) => run::run(&ctx, args).await,
        Command::Usage(args) => usage::run(&ctx, args).await,
        Command::Account(cmd) => account::run(&ctx, cmd).await,
        Command::Setup(args) => setup::run(&ctx, args).await,
        Command::Mcp | Command::RuntimeHost(_) => unreachable!("handled above"),
    }
}

pub struct Context {
    pub paths: Paths,
    pub json: bool,
}

impl Context {
    pub fn client(&self) -> Result<Client> {
        Client::connect(&self.paths)
    }
}
