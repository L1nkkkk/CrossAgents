use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use chrono::Utc;
use clap::Subcommand;
use crossagents_api_types::{
    Agent, AgentStatus, HealthResponse, HubInfo, ListAgentsResponse, Transcript,
};
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
    let hub_running = running.is_some();
    let runtimes = crossagents_drivers::host::records(&ctx.paths.runtimes());
    // `/v1/health` only says the hub answers. The version, its start time and the sessions
    // are behind the token, so each may come back empty on its own.
    let client = match &running {
        Some(_) => ctx.client().ok(),
        None => None,
    };
    let info = match &client {
        Some(client) => client.get::<HubInfo>("/v1/info").await.ok(),
        None => None,
    };
    let agents = match &client {
        Some(client) => client
            .get::<ListAgentsResponse>("/v1/agents")
            .await
            .map(|list| list.agents)
            .ok(),
        None => None,
    };
    // One request per session in error; there are rarely any.
    let errors = match (&client, &agents) {
        (Some(client), Some(list)) => error_reasons(client, list).await,
        _ => Vec::new(),
    };
    let uptime = info
        .as_ref()
        .map(|i| (Utc::now() - i.started_at).num_seconds().max(0) as u64);
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
        let sessions = agents.as_deref().map(|list| {
            let t = Tally::of(list);
            json!({
                "total": t.total, "archived": t.archived,
                "running": t.running, "starting": t.starting,
                "idle": t.idle, "error": t.error, "stopped": t.stopped,
            })
        });
        let errors: Vec<_> = errors
            .iter()
            .map(|e| json!({ "agent": e.name, "error": e.reason }))
            .collect();
        let value = match &running {
            Some((hub, _)) => json!({
                "running": true, "port": hub.port, "pid": hub.pid,
                "version": info.as_ref().map(|i| i.version.clone()), "uptime_secs": uptime,
                "home": ctx.paths.home, "sessions": sessions, "errors": errors,
                "runtimes": runtimes,
            }),
            None => json!({ "running": false, "home": ctx.paths.home, "runtimes": runtimes }),
        };
        return print_json(&value);
    }
    match running {
        Some((hub, _)) => {
            println!("hub:     running");
            println!("port:    {}", hub.port);
            println!("pid:     {}", hub.pid);
            match (&info, uptime) {
                (Some(i), Some(secs)) => {
                    println!("version: {}", i.version);
                    println!("uptime:  {}", duration(secs));
                }
                // The token is not the one this hub issued, so `/v1/info` refused.
                _ => println!("version: unknown (cannot read /v1/info)"),
            }
            println!("home:    {}", ctx.paths.home.display());
        }
        None => {
            println!("hub:     stopped");
            println!("home:    {}", ctx.paths.home.display());
        }
    }
    match &agents {
        Some(list) => {
            println!();
            let t = Tally::of(list);
            let archived = if t.archived > 0 {
                format!(", {} archived", t.archived)
            } else {
                String::new()
            };
            println!("sessions: {}{archived}", t.total);
            let counts: Vec<String> = [
                (t.running, "running"),
                (t.starting, "starting"),
                (t.idle, "idle"),
                (t.error, "error"),
                (t.stopped, "stopped"),
            ]
            .into_iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, label)| format!("{n} {label}"))
            .collect();
            if !counts.is_empty() {
                println!("  {}", counts.join(" · "));
            }
            let busy: Vec<&str> = list
                .iter()
                .filter(|a| matches!(a.status, AgentStatus::Running | AgentStatus::Starting))
                .map(|a| a.name.as_str())
                .collect();
            if !busy.is_empty() {
                println!("working: {}", brief(&busy));
            }
            if !errors.is_empty() {
                println!("in error:");
                for e in &errors {
                    println!("  {}: {}", e.name, e.reason);
                }
            }
        }
        None if hub_running => {
            println!();
            println!("sessions: unknown (cannot read /v1/agents)");
        }
        None => {}
    }
    if !runtimes.is_empty() {
        println!();
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

/// How many sessions are in each state, and how many are archived. Archived ones are out of
/// the lists and always stopped, so they are counted apart rather than as a state.
#[derive(Default)]
struct Tally {
    total: usize,
    archived: usize,
    running: usize,
    starting: usize,
    idle: usize,
    error: usize,
    stopped: usize,
}

impl Tally {
    fn of(agents: &[Agent]) -> Self {
        let mut tally = Self::default();
        for agent in agents {
            tally.total += 1;
            if agent.archived_at.is_some() {
                tally.archived += 1;
                continue;
            }
            match agent.status {
                AgentStatus::Running => tally.running += 1,
                AgentStatus::Starting => tally.starting += 1,
                AgentStatus::Idle => tally.idle += 1,
                AgentStatus::Error => tally.error += 1,
                AgentStatus::Stopped => tally.stopped += 1,
            }
        }
        tally
    }
}

/// Why each session in error is: its transcript's `error`, which also covers a failure before
/// the first turn (a CLI that could not be started).
struct Reason {
    name: String,
    reason: String,
}

async fn error_reasons(client: &Client, agents: &[Agent]) -> Vec<Reason> {
    /// A stuck hub should not turn one command into an endless wait.
    const MAX: usize = 20;
    let mut reasons = Vec::new();
    for agent in agents
        .iter()
        .filter(|a| a.status == AgentStatus::Error)
        .take(MAX)
    {
        let reason = client
            .get::<Transcript>(&format!("/v1/agents/{}/transcript", agent.name))
            .await
            .ok()
            .and_then(|t| t.error)
            .unwrap_or_else(|| "no reason recorded".to_owned());
        reasons.push(Reason {
            name: agent.name.clone(),
            reason: first_line(&reason),
        });
    }
    reasons
}

/// One line, cut short: status is read in a terminal.
fn first_line(text: &str) -> String {
    const WIDTH: usize = 120;
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= WIDTH {
        return line.to_owned();
    }
    let head: String = line.chars().take(WIDTH - 1).collect();
    format!("{head}…")
}

/// Names in one line, ten at most.
fn brief(names: &[&str]) -> String {
    const MAX: usize = 10;
    if names.len() <= MAX {
        return names.join(", ");
    }
    format!("{}, and {} more", names[..MAX].join(", "), names.len() - MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reason_is_one_line_and_cut_short() {
        assert_eq!(first_line("boom\nsecond line"), "boom");
        assert_eq!(first_line("  spaced  "), "spaced");
        let long = "x".repeat(200);
        let cut = first_line(&long);
        assert_eq!(cut.chars().count(), 120);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn many_names_are_summarised() {
        assert_eq!(brief(&["a", "b", "c"]), "a, b, c");
        let many = vec!["n"; 12];
        assert_eq!(
            brief(&many),
            "n, n, n, n, n, n, n, n, n, n, and 2 more"
        );
    }

    #[test]
    fn archived_sessions_are_counted_apart() {
        let agents = vec![
            agent("live", AgentStatus::Running, false),
            agent("asleep", AgentStatus::Stopped, false),
            agent("put away", AgentStatus::Stopped, true),
        ];
        let tally = Tally::of(&agents);
        assert_eq!(tally.total, 3);
        assert_eq!(tally.archived, 1);
        // The archived one is stopped too, but is not counted as a state.
        assert_eq!(tally.running, 1);
        assert_eq!(tally.stopped, 1);
    }

    fn agent(name: &str, status: AgentStatus, archived: bool) -> Agent {
        Agent {
            id: crossagents_api_types::AgentId::new(),
            name: name.to_owned(),
            name_source: Default::default(),
            plugin: "claude".into(),
            driver: None,
            model: None,
            role: crossagents_api_types::Role::Worker,
            cwd: std::path::PathBuf::from("."),
            config: serde_json::Value::Null,
            status,
            created_at: Utc::now(),
            profile: Default::default(),
            topic: None,
            archived_at: archived.then(Utc::now),
        }
    }
}
