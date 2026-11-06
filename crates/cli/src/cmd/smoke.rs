//! `crossagents hub sandbox smoke`: a build of the hub checked end to end on a home of its own
//! (docs/tasks/SELF.md §C3). A build that cannot start a hub, run an agent or carry a message
//! is not one to land changes with, and this says so with a non-zero exit.
//!
//! It runs the same commands a person would, in the same order: `hub sandbox start`, create a
//! mock agent, send it a message, read the events it answers with, `hub sandbox stop`. The
//! mock is `crates/mock-agent`, which needs no account and no model.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
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
    /// How long the whole smoke may take. Starting a hub, an agent and a turn is seconds; a
    /// first run on a cold machine is not much more.
    #[arg(long, default_value_t = 180)]
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

    let deadline = tokio::time::Instant::now() + Duration::from_secs(args.timeout_secs.max(1));
    let mut report = Report::default();
    let outcome = exercise(&exe, &paths, deadline, &mut report).await;
    // A hub left behind would hold the port and the home, so it is stopped even when a step
    // before failed: the next smoke must be about the next build.
    let stopped = sandbox_command(&exe, "stop", &dir).await;
    let stopped_ok = stopped.is_ok();
    let result = outcome.and_then(|()| stopped.map(|_| ()));

    if ctx.json {
        print_json(&json!({
            "ok": result.is_ok(),
            "dir": dir.display().to_string(),
            "kept": args.keep || !stopped_ok,
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
    sandbox_command(exe, "start", &paths.home).await?;
    report.port = paths.read_hub_file()?.map_or(0, |hub| hub.port);
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
/// covers the same path (and the same messages) they do.
async fn sandbox_command(exe: &Path, action: &str, dir: &Path) -> Result<String> {
    let output = tokio::process::Command::new(exe)
        .args(["hub", "sandbox", action, "--dir"])
        .arg(dir)
        .stdin(Stdio::null())
        .output()
        .await
        .with_context(|| format!("cannot run {} hub sandbox {action}", exe.display()))?;
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
        let err = mock_agent(Some(dir.path().join("nope"))).unwrap_err().to_string();
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
}
