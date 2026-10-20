pub mod agent;
pub mod events;
pub mod hub;

use anyhow::Result;
use crossagents_hub::Paths;

use crate::client::Client;
use crate::{Cli, Command};

pub async fn run(cli: Cli) -> Result<()> {
    let paths = Paths::from_env()?;
    let ctx = Context {
        paths,
        json: cli.json,
    };
    match cli.command {
        Command::Hub(cmd) => hub::run(&ctx, cmd).await,
        Command::Agent(cmd) => agent::run(&ctx, cmd).await,
        Command::Events(args) => events::run(&ctx, args).await,
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
