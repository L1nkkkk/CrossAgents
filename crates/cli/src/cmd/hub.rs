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
    /// Stop the background hub.
    Stop,
    /// Show whether the hub is running.
    Status,
}

pub async fn run(ctx: &Context, cmd: HubCommand) -> Result<()> {
    match cmd {
        HubCommand::Run => run_foreground(&ctx.paths).await,
        HubCommand::Start => start(ctx).await,
        HubCommand::Stop => stop(ctx).await,
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
    stop_inheriting_std_handles();
}

/// Windows children inherit every inheritable handle, not just the ones passed as their
/// stdio. If our stdout is a pipe (e.g. `$(crossagents hub start)`), the long-lived hub
/// would keep it open and the reader would never see EOF. We exit right after spawning,
/// so it is fine to clear the inherit flag on our own std handles.
#[cfg(windows)]
#[allow(unsafe_code)]
fn stop_inheriting_std_handles() {
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: plain Win32 calls on handles owned by this process; failures (e.g. no
        // such handle) are harmless and ignored.
        unsafe {
            let handle = GetStdHandle(which);
            if !handle.is_null() {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

#[cfg(unix)]
fn detach(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // Own process group, so the terminal's Ctrl+C does not reach the hub.
    command.process_group(0);
}

async fn stop(ctx: &Context) -> Result<()> {
    if running_hub(&ctx.paths).await?.is_none() {
        if ctx.json {
            return print_json(&json!({ "running": false }));
        }
        println!("hub is not running");
        return Ok(());
    }
    ctx.client()?.post_empty("/v1/shutdown").await?;
    let deadline = Instant::now() + STOP_TIMEOUT;
    while ctx.paths.hub_file().exists() {
        if Instant::now() > deadline {
            bail!("hub did not stop within {}s", STOP_TIMEOUT.as_secs());
        }
        tokio::time::sleep(POLL).await;
    }
    if ctx.json {
        return print_json(&json!({ "running": false }));
    }
    println!("hub stopped");
    Ok(())
}

async fn status(ctx: &Context) -> Result<()> {
    let running = running_hub(&ctx.paths).await?;
    if ctx.json {
        let value = match &running {
            Some((hub, health)) => json!({
                "running": true, "port": hub.port, "pid": hub.pid,
                "version": health.version, "uptime_secs": health.uptime_secs,
                "home": ctx.paths.home,
            }),
            None => json!({ "running": false, "home": ctx.paths.home }),
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
    Ok(())
}
