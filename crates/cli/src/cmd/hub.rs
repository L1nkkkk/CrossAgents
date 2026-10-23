use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use crossagents_api_types::HealthResponse;
use crossagents_hub::{HubConfig, HubFile, Paths};
use serde_json::json;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use super::Context;
use crate::client::Client;
use crate::output::{duration, print_json};

const START_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(100);

#[derive(Debug, Subcommand)]
pub enum HubCommand {
    /// Run the hub in the foreground (Ctrl+C to stop).
    Run,
    /// Start the hub in the background.
    Start,
    /// Stop the background hub. Agents in runtime hosts keep working; the next hub takes
    /// them over.
    Stop {
        /// Stop every agent too.
        #[arg(long)]
        agents: bool,
    },
    /// Show whether the hub is running, and the agent processes that outlive it.
    Status,
}

pub async fn run(ctx: &Context, cmd: HubCommand) -> Result<()> {
    match cmd {
        HubCommand::Run => run_foreground(&ctx.paths).await,
        HubCommand::Start => start(ctx).await,
        HubCommand::Stop { agents } => stop(ctx, agents).await,
        HubCommand::Status => status(ctx).await,
    }
}

async fn run_foreground(paths: &Paths) -> Result<()> {
    let _log_guard = init_logging(paths)?;
    let config = HubConfig::load(paths.clone())?;
    crossagents_hub::run(config, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

fn init_logging(paths: &Paths) -> Result<tracing_appender::non_blocking::WorkerGuard> {
    std::fs::create_dir_all(paths.logs()).context("cannot create logs directory")?;
    let (file, guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::daily(paths.logs(), "hub.log"));
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(file),
        )
        .init();
    Ok(guard)
}

/// The hub described by `hub.json`, if it answers its health check.
async fn running_hub(paths: &Paths) -> Result<Option<(HubFile, HealthResponse)>> {
    let Some(hub) = paths.read_hub_file()? else {
        return Ok(None);
    };
    let Ok(client) = Client::for_hub(paths, &hub) else {
        return Ok(None);
    };
    match client.get::<HealthResponse>("/v1/health").await {
        Ok(health) => Ok(Some((hub, health))),
        Err(_) => Ok(None),
    }
}

/// Starts the hub unless it is already running.
pub async fn ensure_running(ctx: &Context) -> Result<()> {
    if running_hub(&ctx.paths).await?.is_some() {
        return Ok(());
    }
    start(ctx).await
}

async fn start(ctx: &Context) -> Result<()> {
    if let Some((hub, _)) = running_hub(&ctx.paths).await? {
        return report_started(ctx, &hub, true);
    }

    let exe = std::env::current_exe().context("cannot locate the crossagents executable")?;
    let mut command = std::process::Command::new(exe);
    command
        .args(["hub", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach(&mut command);
    let mut child = command.spawn().context("cannot start the hub process")?;
    let pid = child.id();

    let deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(exit) = child.try_wait()? {
            bail!(
                "hub exited during startup ({exit}); see {}",
                ctx.paths.logs().display()
            );
        }
        if let Some((hub, _)) = running_hub(&ctx.paths).await?
            && hub.pid == pid
        {
            return report_started(ctx, &hub, false);
        }
        tokio::time::sleep(POLL).await;
    }
    bail!(
        "hub did not become ready within {}s; see {}",
        START_TIMEOUT.as_secs(),
        ctx.paths.logs().display()
    )
}

fn report_started(ctx: &Context, hub: &HubFile, already: bool) -> Result<()> {
    if ctx.json {
        return print_json(
            &json!({ "running": true, "already_running": already, "port": hub.port, "pid": hub.pid }),
        );
    }
    if already {
        println!(
            "hub is already running (port {}, pid {})",
            hub.port, hub.pid
        );
    } else {
        println!("hub started (port {}, pid {})", hub.port, hub.pid);
    }
    Ok(())
}

#[cfg(windows)]
fn detach(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    // Else the hub would hold our stdout open (e.g. `$(crossagents hub start)`).
    crossagents_drivers::process::stop_inheriting_std_handles();
}

#[cfg(unix)]
fn detach(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // Own process group, so the terminal's Ctrl+C does not reach the hub.
    command.process_group(0);
}

async fn stop(ctx: &Context, agents: bool) -> Result<()> {
    if running_hub(&ctx.paths).await?.is_none() {
        if ctx.json {
            return print_json(&json!({ "running": false }));
        }
        println!("hub is not running");
        return Ok(());
    }
    let path = if agents {
        "/v1/shutdown?stop_agents=true"
    } else {
        "/v1/shutdown"
    };
    ctx.client()?.post_empty(path).await?;
    // Stopping agents waits for their sessions to close.
    let timeout = if agents { STOP_TIMEOUT * 6 } else { STOP_TIMEOUT };
    let deadline = Instant::now() + timeout;
    while ctx.paths.hub_file().exists() {
        if Instant::now() > deadline {
            bail!("hub did not stop within {}s", timeout.as_secs());
        }
        tokio::time::sleep(POLL).await;
    }
    let left = crossagents_drivers::host::records(&ctx.paths.runtimes()).len();
    if ctx.json {
        return print_json(&json!({ "running": false, "runtimes_left": left }));
    }
    if left > 0 {
        println!("hub stopped; {left} agent process(es) keep running for the next hub");
    } else {
        println!("hub stopped");
    }
    Ok(())
}

async fn status(ctx: &Context) -> Result<()> {
    let running = running_hub(&ctx.paths).await?;
    let runtimes = crossagents_drivers::host::records(&ctx.paths.runtimes());
    if ctx.json {
        let runtimes: Vec<_> = runtimes
            .iter()
            .map(|r| {
                json!({
                    "id": r.id, "label": r.label, "owner": r.owner, "host_pid": r.host_pid,
                    "pid": r.child_pid, "started_at": r.started_at, "isolation": r.isolation,
                    "exited": r.exited,
                })
            })
            .collect();
        let value = match &running {
            Some((hub, health)) => json!({
                "running": true, "port": hub.port, "pid": hub.pid,
                "version": health.version, "uptime_secs": health.uptime_secs,
                "home": ctx.paths.home, "runtimes": runtimes,
            }),
            None => json!({ "running": false, "home": ctx.paths.home, "runtimes": runtimes }),
        };
        return print_json(&value);
    }
    match running {
        Some((hub, health)) => {
            println!("hub:     running");
            println!("port:    {}", hub.port);
            println!("pid:     {}", hub.pid);
            println!("version: {}", health.version);
            println!("uptime:  {}", duration(health.uptime_secs));
            println!("home:    {}", ctx.paths.home.display());
        }
        None => {
            println!("hub:     stopped");
            println!("home:    {}", ctx.paths.home.display());
        }
    }
    if !runtimes.is_empty() {
        println!("agent processes ({}):", runtimes.len());
        for r in &runtimes {
            let pid = r.child_pid.map_or_else(|| "-".to_owned(), |p| p.to_string());
            let state = if r.exited { "ended, waiting for the hub" } else { "running" };
            println!(
                "  pid {pid:<7} {}  since {}  {state}  [{}]",
                r.label,
                r.started_at.format("%Y-%m-%d %H:%M"),
                r.isolation
            );
        }
    }
    Ok(())
}
