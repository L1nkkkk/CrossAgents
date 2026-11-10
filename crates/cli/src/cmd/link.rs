//! The hub's built-in hole-punching entrance (docs/MULTI-NODE.md §14「内置打洞」): turn it on
//! or off and make one-time pairing codes, for a hub machine without the desktop GUI.
use super::Context;
use crate::output::print_json;
use anyhow::Result;
use clap::Subcommand;
use crossagents_api_types::{LinkInvite, LinkState, LinkStatus, SetLinkRequest};

#[derive(Debug, Subcommand)]
pub enum LinkCommand {
    /// Whether the entrance is open, its endpoint id and relay.
    Status,
    /// Opens the entrance (kept across hub restarts).
    On {
        /// A self-hosted relay (`https://relay.example.com`) instead of n0's public ones.
        #[arg(long)]
        relay: Option<String>,
    },
    /// Closes the entrance; pairing codes not used yet stop working.
    Off,
    /// Makes a one-time pairing code (30 minutes). Paste it into another machine's GUI:
    /// 设置 →「连接」→「用配对码连接」.
    Invite,
}

pub async fn run(ctx: &Context, cmd: LinkCommand) -> Result<()> {
    let client = ctx.client()?;
    match cmd {
        LinkCommand::Status => {
            let status: LinkStatus = client.get("/v1/link").await?;
            print_status(ctx, &status)
        }
        LinkCommand::On { relay } => {
            let status: LinkStatus = client
                .post(
                    "/v1/link",
                    &SetLinkRequest {
                        enabled: true,
                        relay,
                    },
                )
                .await?;
            print_status(ctx, &status)
        }
        LinkCommand::Off => {
            let status: LinkStatus = client
                .post(
                    "/v1/link",
                    &SetLinkRequest {
                        enabled: false,
                        relay: None,
                    },
                )
                .await?;
            print_status(ctx, &status)
        }
        LinkCommand::Invite => {
            let invite: LinkInvite = client
                .post("/v1/link/invites", &serde_json::json!({}))
                .await?;
            if ctx.json {
                return print_json(&invite);
            }
            println!("{}", invite.code);
            eprintln!(
                "One-time pairing code, valid until {} (shown only once). On the other machine: 设置 →「连接」→「用配对码连接」.",
                invite
                    .expires_at
                    .with_timezone(&chrono::Local)
                    .format("%H:%M")
            );
            Ok(())
        }
    }
}

fn print_status(ctx: &Context, status: &LinkStatus) -> Result<()> {
    if ctx.json {
        return print_json(status);
    }
    let state = match status.state {
        LinkState::Off => "off",
        LinkState::Starting => "starting (no relay connection yet)",
        LinkState::Online => "online",
        LinkState::Failed => "failed",
    };
    println!("state:    {state}");
    if let Some(endpoint) = &status.endpoint {
        println!("endpoint: {endpoint}");
    }
    if let Some(relay) = &status.relay {
        let up = if status.relay_connected {
            "connected"
        } else {
            "not connected"
        };
        println!("relay:    {relay} ({up})");
    }
    if let Some(proxy) = &status.proxy {
        println!("proxy:    {proxy}");
    }
    if let Some(error) = &status.error {
        println!("error:    {error}");
    }
    if status.pending_invites > 0 {
        println!("pending pairing codes: {}", status.pending_invites);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::Cli;
    use clap::Parser;

    #[test]
    fn link_arguments() {
        for args in [
            vec!["crossagents", "link", "status"],
            vec!["crossagents", "link", "on"],
            vec![
                "crossagents",
                "link",
                "on",
                "--relay",
                "https://relay.example.com",
            ],
            vec!["crossagents", "link", "off"],
            vec!["crossagents", "link", "invite", "--json"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
    }
}
