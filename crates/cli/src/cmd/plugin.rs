use anyhow::Result;
use clap::Subcommand;
use crossagents_api_types::ListPluginsResponse;

use super::Context;
use crate::output::{print_json, print_table};

#[derive(Debug, Subcommand)]
pub enum PluginCommand {
    /// List plugins and the models they know.
    Ls,
}

pub async fn run(ctx: &Context, cmd: PluginCommand) -> Result<()> {
    match cmd {
        PluginCommand::Ls => {
            let list: ListPluginsResponse = ctx.client()?.get("/v1/plugins").await?;
            if ctx.json {
                return print_json(&list);
            }
            let rows: Vec<Vec<String>> = list
                .plugins
                .iter()
                .map(|p| {
                    let models = if p.models.is_empty() {
                        "(agent default)".to_owned()
                    } else {
                        p.models.join(", ")
                    };
                    vec![p.name.clone(), models]
                })
                .collect();
            print_table(&["PLUGIN", "MODELS"], &rows);
        }
    }
    Ok(())
}
