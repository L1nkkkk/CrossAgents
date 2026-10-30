use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use chrono::Utc;
use clap::Subcommand;
use crossagents_api_types::{
    Agent, AgentStatus, HealthResponse, HubInfo, ListAgentsResponse, Transcript,
};
use crossagents_hub::{HOME_ENV, HUB_HOME_ENV, HubConfig, HubFile, Paths};
use serde_json::json;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use super::Context;
use crate::client::Client;
use crate::output::{duration, print_json};

/// Startup detects the vendors' logins (their CLIs, proxy probes) before the hub answers,
/// which takes several seconds on some machines.
const START_TIMEOUT: Duration = Duration::from_secs(30);
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
        /// Stop it even from inside one of its own agents.
        #[arg(long)]
        force: bool,
    },
    /// A throwaway dev hub with its own home and port, for checking a build of the hub
    /// without touching the one that runs you (docs/tasks/DOGFOOD.md §1).
    #[command(subcommand)]
    Sandbox(SandboxCommand),
    /// Show whether the hub is running, and the agent processes that outlive it.
    Status,
}

pub async fn run(ctx: &Context, cmd: HubCommand) -> Result<()> {
    match cmd {
        HubCommand::Run => run_foreground(&ctx.paths).await,
        HubCommand::Start => start(ctx).await,
        HubCommand::Stop { agents, force } => {
            refuse_own_hub(&ctx.paths, force)?;
            stop(ctx, agents).await
        }
        HubCommand::Sandbox(cmd) => sandbox(ctx, cmd).await,
        HubCommand::Status => status(ctx).await,
    }
}

async fn run_foreground(paths: &Paths) -> Result<()> {
    let _log_guard = init_logging(paths)?;
    let result = serve(paths).await;
    // `hub start` runs this process with stderr discarded, so a failure here would otherwise
    // leave nothing behind but "hub exited during startup": say why in the log (a database
    // from a newer version is the case that matters, docs/TODO.md §一).
    if let Err(err) = &result {
        let text = format!("{err:#}");
        tracing::error!("hub could not start: {}", text.lines().next().unwrap_or(""));
    }
    result
}

async fn serve(paths: &Paths) -> Result<()> {
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
    // Run from a copy, so the build output stays free for the next build.
    let exe = crossagents_hub::shadow::stable_exe(&ctx.paths, &exe).unwrap_or(exe);
    // Where the log ends before this attempt: a reason written after it is this attempt's.
    let log_from = log_tail(&ctx.paths);
    let mut command = std::process::Command::new(exe);
    command
        .args(["hub", "run"])
        .env(HOME_ENV, &ctx.paths.home)
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
                "hub exited during startup ({exit}){}",
                explanation(&ctx.paths, log_from.as_ref())
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
        "hub did not become ready within {}s{}",
        START_TIMEOUT.as_secs(),
        explanation(&ctx.paths, log_from.as_ref())
    )
}

/// `: <why>; see <logs>`, where `<why>` is the error the hub logged after `from`. `hub run`
/// has its stderr discarded (it is a detached process), so the log is the only place its
/// reason is written; without it the GUI could only say "cannot start the local hub".
fn explanation(paths: &Paths, from: Option<&(String, u64)>) -> String {
    let see = format!("; see {}", paths.logs().display());
    match logged_error(paths, from) {
        Some(why) => format!(": {why}{see}"),
        None => see,
    }
}

/// The last `ERROR` line written to `hub.log` after `from` (the newest file when `from` is
/// unknown, or when the log rolled over to a new day since).
fn logged_error(paths: &Paths, from: Option<&(String, u64)>) -> Option<String> {
    let (text, start) = match from {
        Some((name, len)) => match std::fs::read_to_string(paths.logs().join(name)) {
            Ok(text) => {
                let start = (*len as usize).min(text.len());
                (text, start)
            }
            Err(_) => (std::fs::read_to_string(newest_log(paths)?).ok()?, 0),
        },
        None => (std::fs::read_to_string(newest_log(paths)?).ok()?, 0),
    };
    // One line per entry: `<time> ERROR <module>: <message>`. The message is what a user can
    // act on; the module name only repeats this crate's layout.
    text.get(start..)?
        .lines()
        .filter_map(|line| line.split_once(" ERROR ").map(|(_, rest)| rest.trim()))
        .next_back()
        .map(|rest| match rest.split_once(": ") {
            Some((module, message)) if module.starts_with("crossagents") => message.to_owned(),
            _ => rest.to_owned(),
        })
}

/// The log file the hub is writing now: `hub.log.YYYY-MM-DD`, newest first.
fn newest_log(paths: &Paths) -> Option<std::path::PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(paths.logs())
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("hub.log"))
        })
        .collect();
    files.sort();
    files.pop()
}

/// Where the log ends now, so a later reason can be told apart from an earlier one.
fn log_tail(paths: &Paths) -> Option<(String, u64)> {
    let path = newest_log(paths)?;
    let name = path.file_name()?.to_str()?.to_owned();
    let len = std::fs::metadata(&path).ok()?.len();
    Some((name, len))
}

fn report_started(ctx: &Context, hub: &HubFile, already: bool) -> Result<()> {
    if ctx.json {
        return print_json(&json!({
            "running": true, "already_running": already, "port": hub.port, "pid": hub.pid,
            "home": ctx.paths.home,
        }));
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
    let timeout = if agents {
        STOP_TIMEOUT * 6
    } else {
        STOP_TIMEOUT
    };
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

#[derive(Debug, Subcommand)]
pub enum SandboxCommand {
    /// Start a dev hub from this executable (a copy of it, so rebuilding stays possible).
    Start {
        /// Its home; defaults to `$CARGO_TARGET_DIR/dev-hub`, else `./target/dev-hub`.
        #[arg(long)]
        dir: Option<std::path::PathBuf>,
        /// Register this machine's vendor logins as accounts (off: mock and API checks only).
        #[arg(long)]
        detect: bool,
    },
    /// Stop the dev hub and its agents.
    Stop {
        #[arg(long)]
        dir: Option<std::path::PathBuf>,
    },
    /// Show whether the dev hub is running.
    Status {
        #[arg(long)]
        dir: Option<std::path::PathBuf>,
    },
}

/// Refuses to stop the hub this process runs under (an agent's shell inherits
/// `CROSSAGENTS_HUB_HOME` from it), unless `force`.
fn refuse_own_hub(paths: &Paths, force: bool) -> Result<()> {
    if force {
        return Ok(());
    }
    if let Some(own) = std::env::var_os(HUB_HOME_ENV).filter(|v| !v.is_empty())
        && same_dir(std::path::Path::new(&own), &paths.home)
    {
        bail!(
            "this process runs under the hub at {}; stopping it would stop you too. \
             Check a hub build with `crossagents hub sandbox start` instead, or pass --force",
            paths.home.display()
        );
    }
    Ok(())
}

fn same_dir(a: &std::path::Path, b: &std::path::Path) -> bool {
    let canon = |p: &std::path::Path| dunce::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

/// Where a dev hub lives unless told: next to the build that runs it.
pub fn sandbox_dir(dir: Option<std::path::PathBuf>) -> Result<std::path::PathBuf> {
    let dir = match dir {
        Some(dir) => dir,
        None => match std::env::var_os("CARGO_TARGET_DIR").filter(|v| !v.is_empty()) {
            Some(target) => std::path::PathBuf::from(target).join("dev-hub"),
            None => std::path::PathBuf::from("target").join("dev-hub"),
        },
    };
    Ok(if dir.is_absolute() {
        dir
    } else {
        std::env::current_dir()?.join(dir)
    })
}

/// A dev hub's `config.toml`: any free port, agents end with it, no vendor logins unless
/// `detect`. Written once; later edits are kept.
pub fn sandbox_config(detect: bool) -> String {
    format!(
        "# A dev hub (crossagents hub sandbox): its own port, nothing outlives it.\n\
         [hub]\nport = 0\nruntime_hosts = false\n\n[accounts]\ndetect = {detect}\n"
    )
}

async fn sandbox(ctx: &Context, cmd: SandboxCommand) -> Result<()> {
    let (dir, action) = match cmd {
        SandboxCommand::Start { dir, detect } => (dir, Some(detect)),
        SandboxCommand::Stop { dir } => (dir, None),
        SandboxCommand::Status { dir } => {
            let dev = Context {
                paths: Paths::new(sandbox_dir(dir)?),
                json: ctx.json,
            };
            return status(&dev).await;
        }
    };
    let home = sandbox_dir(dir)?;
    if same_dir(&home, &ctx.paths.home) {
        bail!(
            "{} is the hub's own home; give the dev hub another --dir",
            home.display()
        );
    }
    let dev = Context {
        paths: Paths::new(&home),
        json: ctx.json,
    };
    let Some(detect) = action else {
        return stop(&dev, true).await;
    };
    std::fs::create_dir_all(&home).with_context(|| format!("cannot create {}", home.display()))?;
    let config = dev.paths.config();
    if !config.exists() {
        std::fs::write(&config, sandbox_config(detect))
            .with_context(|| format!("cannot write {}", config.display()))?;
    }
    start(&dev).await?;
    if !ctx.json {
        println!("home:    {}", home.display());
        println!(
            "use it:  set CROSSAGENTS_HOME={} for later commands",
            home.display()
        );
        println!(
            "stop it: crossagents hub sandbox stop --dir {}",
            home.display()
        );
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
            let pid = r
                .child_pid
                .map_or_else(|| "-".to_owned(), |p| p.to_string());
            let state = if r.exited {
                "ended, waiting for the hub"
            } else {
                "running"
            };
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
    format!(
        "{}, and {} more",
        names[..MAX].join(", "),
        names.len() - MAX
    )
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
        assert_eq!(brief(&many), "n, n, n, n, n, n, n, n, n, n, and 2 more");
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

    /// docs/TODO.md §一: a hub that cannot start says why, in the message the GUI shows.
    #[test]
    fn a_failed_start_takes_its_reason_from_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        std::fs::create_dir_all(paths.logs()).unwrap();
        let log = paths.logs().join("hub.log.2026-10-29");
        std::fs::write(
            &log,
            "2026-10-29T09:00:00Z  INFO crossagents_hub: older line\n\
             2026-10-29T09:00:01Z ERROR crossagents_cli::cmd::hub: hub could not start: the \
             database comes from a newer CrossAgents (it has migration 11, this build knows up \
             to 10); upgrade CrossAgents to open it\n",
        )
        .unwrap();

        // Only what was written after the mark counts: an older reason is not this attempt's.
        let before = log_tail(&paths).unwrap();
        assert!(explanation(&paths, Some(&before)).contains("see"));
        assert!(!explanation(&paths, Some(&before)).contains("migration"));

        std::fs::write(
            &log,
            format!(
                "{}\n2026-10-29T09:00:02Z ERROR crossagents_cli::cmd::hub: hub could not start: \
                 the database comes from a newer CrossAgents (it has migration 11, this build \
                 knows up to 10); upgrade CrossAgents to open it\n",
                std::fs::read_to_string(&log).unwrap()
            ),
        )
        .unwrap();
        let reason = explanation(&paths, Some(&before));
        assert!(
            reason.contains("comes from a newer CrossAgents"),
            "{reason}"
        );
        assert!(!reason.contains("older line"), "{reason}");
        assert!(reason.contains("logs"), "{reason}");
        // The module name is dropped: only the reason is worth showing.
        assert!(!reason.contains("crossagents_cli::cmd::hub"), "{reason}");
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
            home: std::path::PathBuf::new(),
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
