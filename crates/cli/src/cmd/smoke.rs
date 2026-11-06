//! `crossagents hub sandbox smoke`: a build of the hub checked end to end on a home of its own
//! (docs/tasks/SELF.md §C3). A build that cannot start a hub, run an agent or carry a message
//! is not one to land changes with, and this says so with a non-zero exit.
//!
//! What it does, in the order a person would: `hub sandbox start` (its own home, its own port),
//! create a mock agent, send it a message, read the events it answers with, `hub sandbox stop`.
//! The mock is `crates/mock-agent`, which needs no account and no model.
//!
//! The build under test is this very executable (`std::env::current_exe`), never a `crossagents`
//! found on `PATH`; `hub sandbox start` then runs a copy of it under the sandbox's own home.
//!
//! Nothing here touches the person's hub: the sandbox has its own home and a random port, and
//! the only use of the person's home is comparing paths, so that the sandbox is never put
//! there. On every way out - done, failed, timed out, Ctrl+C - the sandbox is stopped, and its
//! home removed unless `--keep` or the stop itself failed.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Args;
use crossagents_api_types::{Agent, AgentStatus, EventKind, Transcript};
use crossagents_hub::Paths;
use futures::StreamExt;
use serde_json::json;

use super::Context;
use crate::client::Client;
use crate::output::print_json;

/// The plugin the throwaway hub registers, and the model the agent runs on.
const PLUGIN: &str = "mock";
const MODEL: &str = "mock-haiku";
/// The agent's name, so that a failure names it.
const NAME: &str = "smoke";
/// How much of the sandbox hub's log a failed smoke shows.
const LOG_TAIL_LINES: usize = 40;
/// A smoke given less than this is not worth starting: a hub alone takes seconds.
const MIN_BUDGET: Duration = Duration::from_secs(30);
/// Stopping has its own budget: it must happen even after the smoke's own time ran out, and a
/// stop that waits for agents takes up to 30 s.
const STOP_BUDGET: Duration = Duration::from_secs(90);

#[derive(Debug, Args)]
pub struct SmokeArgs {
    /// Where the throwaway hub lives: `--dir`, else `<target>/dev-hub/smoke-<pid>`, removed
    /// again when the smoke ends.
    #[arg(long)]
    pub dir: Option<PathBuf>,
    /// The mock agent to run: `crossagents-mock-agent` next to this executable by default.
    #[arg(long)]
    pub mock_agent: Option<PathBuf>,
    /// Keep the throwaway home, whose log says what happened.
    #[arg(long)]
    pub keep: bool,
    /// How long the whole smoke may take. Under a loaded machine several hubs start at once
    /// (a run of eight tasks starts eight checks), so this is generous: 8 s is typical.
    #[arg(long, default_value_t = 240)]
    pub timeout_secs: u64,
}

pub async fn run(ctx: &Context, args: SmokeArgs) -> Result<()> {
    let started = Instant::now();
    let dir = directory(args.dir)?;
    if super::hub::same_dir(&dir, &ctx.paths.home) {
        bail!(
            "{} is this hub's own home; give the smoke another --dir",
            dir.display()
        );
    }
    let mock = mock_agent(args.mock_agent)?;
    if dir.exists() {
        bail!(
            "{} is in the way; move it away or give the smoke another --dir",
            dir.display()
        );
    }
    let exe = std::env::current_exe().context("cannot locate the crossagents executable")?;
    let paths = Paths::new(&dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    std::fs::write(paths.config(), config(&mock))
        .with_context(|| format!("cannot write {}", paths.config().display()))?;

    // Every step is bounded, and so is their sum: a build that hangs must fail the check
    // rather than hold it.
    let budget = Duration::from_secs(args.timeout_secs).max(MIN_BUDGET);
    let deadline = tokio::time::Instant::now() + budget;
    let mut report = Report::default();
    let outcome = tokio::select! {
        biased;
        _ = tokio::signal::ctrl_c() => Err(anyhow!("interrupted")),
        done = tokio::time::timeout_at(deadline, exercise(&exe, &paths, deadline, &mut report)) => {
            match done {
                Ok(done) => done,
                Err(_) => Err(anyhow!("no step finished within {}s", budget.as_secs())),
            }
        }
    };

    // A hub left behind would hold its port and its home, and the next smoke would be about
    // the wrong build: it is stopped however the step before ended.
    let stopped = sandbox_command(&exe, "stop", &dir, STOP_BUDGET).await;
    let stopped_ok = stopped.is_ok();
    let result = outcome.and_then(|()| stopped.map(|_| ()));
    if result.is_err() {
        print_log_tail(&paths);
    }

    if ctx.json {
        print_json(&json!({
            "ok": result.is_ok(),
            "dir": dir.display().to_string(),
            "kept": args.keep || !stopped_ok,
            "exe": exe.display().to_string(),
            "hub_exe": report.hub_exe.as_ref().map(|p| p.display().to_string()),
            "mock_agent": mock.display().to_string(),
            "port": report.port,
            "events": report.events,
            "elapsed_secs": round(started.elapsed()),
            "steps": report
                .steps
                .iter()
                .map(|step| json!({ "step": step.name, "secs": round(step.took) }))
                .collect::<Vec<_>>(),
        }))?;
    } else {
        println!("build:   {}", exe.display());
        if let Some(ran) = &report.hub_exe {
            println!("hub:     {}", ran.display());
        }
        for step in &report.steps {
            println!("  {:<34} {:>6.2}s", step.name, step.took.as_secs_f64());
        }
        let word = if result.is_ok() { "ok" } else { "failed" };
        println!(
            "sandbox smoke {word} in {:.1}s ({})",
            started.elapsed().as_secs_f64(),
            dir.display()
        );
    }
    if args.keep || !stopped_ok {
        if !ctx.json {
            println!("kept {} (its log says what happened)", dir.display());
        }
    } else {
        remove(&dir).await;
    }
    result
}

/// The happy path, one step at a time; each step is noted with how long it took.
async fn exercise(
    exe: &Path,
    paths: &Paths,
    deadline: tokio::time::Instant,
    report: &mut Report,
) -> Result<()> {
    let mut since = Instant::now();
    sandbox_command(exe, "start", &paths.home, left(deadline)).await?;
    let hub = paths.read_hub_file()?.ok_or_else(|| {
        anyhow!(
            "`hub sandbox start` returned but {} has no hub.json",
            paths.home.display()
        )
    })?;
    report.port = hub.port;
    if let Some(ran) = hub.exe.as_ref() {
        // Normally a copy of this binary inside the sandbox (`hub sandbox start` copies it so a
        // rebuild can replace the build output); a build that already lives under a hub's
        // `bin/` or `shadow/` runs as it is, and that is still this build.
        if !inside(ran, &paths.home) && !super::hub::same_dir(ran, exe) {
            bail!(
                "the sandbox hub runs {}, which is neither this executable ({}) nor its copy \
                 inside {}: that is not the build under test",
                ran.display(),
                exe.display(),
                paths.home.display()
            );
        }
        report.hub_exe = Some(ran.clone());
    }
    report.step("hub started", &mut since);

    let client = Client::connect(paths).context("cannot talk to the hub it started")?;
    // From now on, before anything happens that these events would announce.
    let events = client
        .live_events()
        .await
        .context("cannot subscribe to the hub's events")?;
    let mut events = std::pin::pin!(events);

    let work = paths.home.join("work");
    std::fs::create_dir_all(&work).with_context(|| format!("cannot create {}", work.display()))?;
    let agent: Agent = client
        .post(
            "/v1/agents",
            &json!({
                "name": NAME,
                "plugin": PLUGIN,
                "model": MODEL,
                "role": "worker",
                "cwd": work.display().to_string(),
            }),
        )
        .await
        .context("the hub would not create an agent from the mock plugin")?;
    report.step(format!("agent {} created", agent.name), &mut since);

    let token = format!("sandbox smoke {}", std::process::id());
    let _: serde_json::Value = client
        .post(
            &format!("/v1/agents/{}/messages", agent.name),
            &json!({ "text": token }),
        )
        .await
        .context("the hub would not take a message for the agent")?;
    let mut answered = false;
    let mut completed = false;
    while !answered || !completed {
        let next = match tokio::time::timeout_at(deadline, events.next()).await {
            Ok(next) => next,
            Err(_) => bail!("the agent did not answer: {}", why(&client, NAME).await),
        };
        let Some(event) = next else {
            bail!("the hub stopped sending events: {}", why(&client, NAME).await);
        };
        report.events += 1;
        match &event?.kind {
            EventKind::AgentOutput { text, .. } if text.contains(&token) => answered = true,
            EventKind::AgentTurnCompleted { .. } => completed = true,
            EventKind::AgentStatusChanged {
                to: AgentStatus::Error,
                ..
            } => bail!("agent {NAME} went into error: {}", why(&client, NAME).await),
            _ => {}
        }
    }
    report.step("message answered", &mut since);
    Ok(())
}

/// `<exe> hub sandbox <action> --dir <dir>`, the command a person runs, so that the smoke
/// covers the same path (and the same messages) they do. Ends by `budget` at the latest.
async fn sandbox_command(
    exe: &Path,
    action: &str,
    dir: &Path,
    budget: Duration,
) -> Result<String> {
    let mut command = tokio::process::Command::new(exe);
    command
        .args(["hub", "sandbox", action, "--dir"])
        .arg(dir)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(budget, command.output()).await {
        Ok(output) => output
            .with_context(|| format!("cannot run {} hub sandbox {action}", exe.display()))?,
        Err(_) => bail!(
            "`crossagents hub sandbox {action}` did not finish within {}s",
            budget.as_secs()
        ),
    };
    let mut text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let errors = String::from_utf8_lossy(&output.stderr);
    let errors = errors.trim();
    if !errors.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(errors);
    }
    if !output.status.success() {
        bail!(
            "`crossagents hub sandbox {action}` failed ({}){}",
            output.status,
            if text.is_empty() {
                String::new()
            } else {
                format!(": {text}")
            }
        );
    }
    Ok(text)
}

/// The end of the sandbox hub's log on stderr: the hub writes its reasons there, and the
/// smoke's own message only carries what the API answered.
fn print_log_tail(paths: &Paths) {
    match tail_of(paths, LOG_TAIL_LINES) {
        Some((log, lines)) => {
            eprintln!("--- {} (last {} lines) ---", log.display(), lines.len());
            for line in lines {
                eprintln!("{line}");
            }
            eprintln!("--- end of {} ---", log.display());
        }
        None => eprintln!(
            "no hub log under {}; the hub may have failed before it could write one",
            paths.logs().display()
        ),
    }
}

/// The last `lines` lines of the newest `hub.log*` under `paths`, if there is one.
fn tail_of(paths: &Paths, lines: usize) -> Option<(PathBuf, Vec<String>)> {
    let mut logs: Vec<PathBuf> = std::fs::read_dir(paths.logs())
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("hub.log"))
        })
        .collect();
    logs.sort();
    let log = logs.pop()?;
    let text = std::fs::read_to_string(&log).ok()?;
    let tail: Vec<String> = text
        .lines()
        .rev()
        .take(lines)
        .map(str::to_owned)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Some((log, tail))
}

/// Why the agent is where it is: its transcript's error (a CLI that could not be started),
/// else the state it is in.
async fn why(client: &Client, name: &str) -> String {
    if let Ok(transcript) = client
        .get::<Transcript>(&format!("/v1/agents/{name}/transcript"))
        .await
        && let Some(error) = transcript.error
    {
        return error.lines().next().unwrap_or_default().to_owned();
    }
    match client.get::<Agent>(&format!("/v1/agents/{name}")).await {
        Ok(agent) => format!("it is {:?}", agent.status),
        Err(err) => format!("cannot read the agent: {err:#}"),
    }
}

/// A throwaway hub's `config.toml`: any port, no vendor logins (those are the person's), no
/// runtime hosts (a process that outlived the smoke would hold the home), and one `mock`
/// plugin for the agent above.
fn config(mock: &Path) -> String {
    format!(
        "# Written by `crossagents hub sandbox smoke` (docs/tasks/SELF.md §C3).\n\
         [hub]\n\
         port = 0\n\
         runtime_hosts = false\n\n\
         [accounts]\n\
         detect = false\n\n\
         [plugins.{PLUGIN}]\n\
         command = [\"{program}\"]\n\
         models = [\"{MODEL}\"]\n",
        program = path_in_toml(mock),
    )
}

/// A path as TOML reads it: forward slashes, so Windows backslashes need no escaping.
fn path_in_toml(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// Where the throwaway hub lives: `--dir`, else `<target>/dev-hub/smoke-<pid>` beside the
/// build that runs it (the place `hub sandbox start` uses by default, one run apart).
fn directory(dir: Option<PathBuf>) -> Result<PathBuf> {
    match dir {
        Some(dir) => Ok(dir),
        None => Ok(super::hub::sandbox_dir(None)?.join(format!("smoke-{}", std::process::id()))),
    }
}

/// The mock agent: the one given, else the one built next to this executable.
fn mock_agent(explicit: Option<PathBuf>) -> Result<PathBuf> {
    let candidate = match explicit {
        Some(path) => path,
        None => {
            let exe = std::env::current_exe().context("cannot locate the crossagents executable")?;
            let dir = exe
                .parent()
                .context("the crossagents executable has no directory")?;
            dir.join(format!(
                "crossagents-mock-agent{}",
                std::env::consts::EXE_SUFFIX
            ))
        }
    };
    if candidate.is_file() {
        return Ok(candidate);
    }
    bail!(
        "cannot find the mock agent at {}; build it with \
         `cargo build -p crossagents-mock-agent`, or pass --mock-agent",
        candidate.display()
    )
}

/// Removes the home once its hub is gone. Best effort: the hub is a separate process and lets
/// go of its database and its copy of this executable a moment after it removes `hub.json`
/// (a few seconds on Windows), and a directory left behind is not a failed smoke.
async fn remove(dir: &Path) {
    for _ in 0..200 {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => return,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    tracing::debug!(dir = %dir.display(), "cannot remove the smoke's home; it stays");
}

/// Whether `path` is `dir` or below it (both as written, resolved where they exist).
fn inside(path: &Path, dir: &Path) -> bool {
    let canon = |p: &Path| dunce::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(path).starts_with(canon(dir))
}

/// What is left of the smoke's own budget.
fn left(deadline: tokio::time::Instant) -> Duration {
    deadline.saturating_duration_since(tokio::time::Instant::now())
}

fn round(time: Duration) -> f64 {
    (time.as_secs_f64() * 100.0).round() / 100.0
}

/// One step of the smoke and how long it took.
struct Step {
    name: String,
    took: Duration,
}

#[derive(Default)]
struct Report {
    port: u16,
    events: usize,
    /// The executable the sandbox hub actually runs (a copy of the build under test).
    hub_exe: Option<PathBuf>,
    steps: Vec<Step>,
}

impl Report {
    /// Notes a step as done now, and starts the clock for the next.
    fn step(&mut self, name: impl Into<String>, since: &mut Instant) {
        self.steps.push(Step {
            name: name.into(),
            took: since.elapsed(),
        });
        *since = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hub must read this file as the smoke means it: the plugin it declares, and none of
    /// the startup work a real hub does (vendor logins, runtime hosts).
    #[test]
    fn the_config_declares_a_mock_plugin_and_no_logins() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        let mock = dir.path().join("target").join("crossagents-mock-agent.exe");
        std::fs::write(paths.config(), config(&mock)).unwrap();
        let config = crossagents_hub::HubConfig::load(paths).unwrap();
        assert_eq!(config.port, 0);
        assert!(!config.detect_accounts);
        assert!(
            config.runtime_host.is_none(),
            "a process outliving the smoke would hold its home"
        );
        let plugin = &config.plugins[PLUGIN];
        assert_eq!(plugin.command, [path_in_toml(&mock)]);
        assert_eq!(plugin.models, [MODEL]);
    }

    #[test]
    fn paths_are_written_with_forward_slashes() {
        assert_eq!(
            path_in_toml(Path::new(r"C:\Users\a b\mock.exe")),
            "C:/Users/a b/mock.exe"
        );
    }

    #[test]
    fn a_missing_mock_agent_says_how_to_build_it() {
        let dir = tempfile::tempdir().unwrap();
        let err = mock_agent(Some(dir.path().join("nope")))
            .unwrap_err()
            .to_string();
        assert!(err.contains("crossagents-mock-agent"), "{err}");
        assert!(err.contains("cargo build -p crossagents-mock-agent"), "{err}");

        let there = dir.path().join("crossagents-mock-agent");
        std::fs::write(&there, b"not really").unwrap();
        assert_eq!(mock_agent(Some(there.clone())).unwrap(), there);
    }

    #[test]
    fn a_default_home_is_its_own_place_under_the_sandboxes() {
        let dir = directory(None).unwrap();
        assert!(
            dir.ends_with(format!("smoke-{}", std::process::id())),
            "{}",
            dir.display()
        );
        assert!(dir.starts_with(super::super::hub::sandbox_dir(None).unwrap()));
    }

    /// A failed smoke shows the end of the sandbox hub's log, from the newest file it wrote.
    #[test]
    fn the_log_tail_is_the_end_of_the_newest_log() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        std::fs::create_dir_all(paths.logs()).unwrap();
        std::fs::write(paths.logs().join("hub.log.2026-01-01"), "old\n").unwrap();
        let newest = paths.logs().join("hub.log.2026-01-02");
        let lines: Vec<String> = (1..=100).map(|n| format!("line {n}")).collect();
        std::fs::write(&newest, format!("{}\n", lines.join("\n"))).unwrap();

        let (log, tail) = tail_of(&paths, 3).unwrap();
        assert_eq!(log, newest);
        assert_eq!(tail, ["line 98", "line 99", "line 100"]);
        // Asking for more than there is gives what there is.
        assert_eq!(tail_of(&paths, 1000).unwrap().1.len(), 100);
        // No log at all is said so, not a failure.
        std::fs::remove_dir_all(paths.logs()).unwrap();
        assert!(tail_of(&paths, 3).is_none());
    }

    #[test]
    fn a_hub_outside_the_sandbox_is_not_the_build_under_test() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let shadow = home.join("shadow").join("abc").join("crossagents.exe");
        std::fs::create_dir_all(shadow.parent().unwrap()).unwrap();
        std::fs::write(&shadow, b"copy").unwrap();
        assert!(inside(&shadow, &home));
        assert!(!inside(Path::new(r"C:\somewhere\crossagents.exe"), &home));
    }
}
