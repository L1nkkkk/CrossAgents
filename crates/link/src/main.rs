//! `crossagents-link`: the client's side of a hole-punched connection to a hub
//! (docs/MULTI-NODE.md §14「内置打洞」). The desktop GUI runs it; secrets come on stdin, never
//! on the command line, and what happens goes to stdout as one JSON line per event.

use std::io::{BufRead, Write};
use std::net::SocketAddr;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crossagents_link::{Event, Invite, Target};

#[derive(Parser)]
#[command(
    name = "crossagents-link",
    version,
    about = "Hole-punched connection to a CrossAgents hub"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Redeems a pairing code (read from stdin) for a device token; prints `{ device, token, host }`.
    Redeem {
        /// The device name the hub records.
        #[arg(long)]
        name: String,
        /// This machine's system (`windows`, `macos`, `linux`), recorded with the device.
        #[arg(long)]
        os: Option<String>,
    },
    /// Forwards a local port to the hub, presenting the device token read from stdin.
    Connect {
        /// The hub's endpoint id.
        #[arg(long)]
        endpoint: String,
        /// The hub's relay, as a hint.
        #[arg(long)]
        relay: Option<String>,
        /// The relay is the hub's own (self-hosted): use it as this side's relay too.
        #[arg(long)]
        custom_relay: bool,
        /// A direct address of the hub to try (repeatable).
        #[arg(long = "addr")]
        addrs: Vec<SocketAddr>,
        /// Where to listen; port 0 picks a free one (reported in the `listening` event).
        #[arg(long, default_value = "127.0.0.1:0")]
        listen: SocketAddr,
    },
}

fn stdin_line() -> Result<String> {
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .context("cannot read stdin")?;
    let line = line.trim().to_owned();
    anyhow::ensure!(!line.is_empty(), "nothing on stdin");
    Ok(line)
}

fn emit(event: &Event) {
    let mut out = std::io::stdout().lock();
    let _ = serde_json::to_writer(&mut out, event);
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("CROSSAGENTS_LINK_LOG")
                .unwrap_or_else(|_| "warn".into()),
        )
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::Redeem { name, os } => {
            let invite = Invite::decode(&stdin_line()?)?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or_default();
            anyhow::ensure!(
                invite.expires > now,
                "the pairing code has expired; make a new one on the hub's machine"
            );
            let mut paired = crossagents_link::redeem(&invite, &name, os.as_deref(), None).await?;
            paired["host"] = invite.host.clone().into();
            paired["endpoint"] = invite.endpoint.clone().into();
            if let Some(relay) = &invite.relay {
                paired["relay"] = relay.clone().into();
            }
            paired["custom_relay"] = invite.custom_relay.into();
            paired["addrs"] = invite.addrs.clone().into();
            println!("{paired}");
            Ok(())
        }
        Command::Connect {
            endpoint,
            relay,
            custom_relay,
            addrs,
            listen,
        } => {
            let mut target = Target::new(&endpoint, relay.as_deref())?;
            target.custom_relay = custom_relay && target.relay.is_some();
            target.addrs = addrs;
            let token = stdin_line()?;
            crossagents_link::forward(target, token, listen, None, |event| emit(&event)).await?;
            // Only a refused token ends the forward: exit 3 says so.
            std::process::exit(3);
        }
    }
}
