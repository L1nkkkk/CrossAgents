//! `crossagents exec`: run a command under the local resource locks of this machine
//! (`docs/tasks/LOCKS.md` §4).
//!
//! ```text
//! crossagents exec [--resource NAME[:shared|:exclusive]]… [--timeout 10m] -- <command> …
//! ```
//!
//! A named resource comes from `CROSSAGENTS_RESOURCES`, the JSON array the hub puts in every
//! agent's environment (`crates/orchestrator/src/project.rs`):
//!
//! ```json
//! [{"name":"build","path":"C:\\…\\build\\cargo","access":"exclusive"}]
//! ```
//!
//! `--resource NAME=PATH[:access]` names a path directly, for use outside a run. The command
//! is executed as given — no shell in between, so quoting and exit codes are the caller's.
//!
//! Exit codes: the command's own, `75` (`EX_TEMPFAIL`) when the lock did not come in time, and
//! `1` for a request the outer `exec` cannot serve (a resource it does not hold, an upgrade).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;
use crossagents_core::exec::{
    Access, EXIT_TEMPFAIL, ExecError, Request, Resource, run as run_under_locks,
};
use serde::Deserialize;

/// The variable the hub uses to tell agents which resources their run declares. Parsed here
/// rather than imported, so the CLI does not depend on the orchestrator.
const RESOURCES_ENV: &str = "CROSSAGENTS_RESOURCES";

/// The resource a build in a run worktree holds, and the one a shim holds for its program
/// (docs/tasks/LOCKS.md §3, §4.6).
const BUILD: &str = "build";

#[derive(Debug, Clone, Args)]
pub struct ExecArgs {
    /// A resource to hold: `NAME`, `NAME:shared` or `NAME:exclusive` for one declared in
    /// CROSSAGENTS_RESOURCES; `NAME=PATH[:access]` names a path directly.
    #[arg(long = "resource", value_name = "NAME[:ACCESS]")]
    pub resource: Vec<String>,

    /// How long to wait for the resources before giving up (exit code 75).
    #[arg(
        long,
        value_name = "TIME",
        default_value = "10m",
        value_parser = parse_timeout
    )]
    pub timeout: Duration,

    /// The command and its arguments. Everything after `--` is passed through unchanged.
    #[arg(last = true, required = true, num_args = 1.., value_name = "COMMAND")]
    pub command: Vec<String>,
}

/// Runs the subcommand and returns the exit code the CLI should end with.
pub async fn run_cli(args: ExecArgs) -> Result<i32> {
    let Some(program) = args.command.first() else {
        bail!("no command: crossagents exec [--resource NAME]… -- <command> …");
    };
    let resources = resources(&args.resource)?;
    let locks_dir = locks_dir()?;

    let mut request = Request::new(args.command.join(" "), resources);
    request.agent = env_non_empty("CROSSAGENTS_AGENT");
    request.run = env_non_empty("CROSSAGENTS_RUN");
    request.timeout = args.timeout;

    let mut cmd = tokio::process::Command::new(program);
    cmd.args(&args.command[1..]);

    match run_under_locks(&locks_dir, &request, &mut cmd).await {
        Ok(status) => Ok(status.code().unwrap_or(1)),
        Err(error @ ExecError::Timeout { .. }) => {
            eprintln!("error: {error}");
            Ok(EXIT_TEMPFAIL)
        }
        Err(error @ ExecError::Nesting(_)) => {
            eprintln!("error: {error}");
            Ok(1)
        }
        Err(error) => Err(error.into()),
    }
}

#[derive(Debug, Deserialize)]
struct Declared {
    name: String,
    path: PathBuf,
    #[serde(default)]
    access: Option<Access>,
}

/// Where the lock directory lives, in the order the hub itself uses.
fn home() -> Result<PathBuf> {
    for variable in ["CROSSAGENTS_HUB_HOME", "CROSSAGENTS_HOME"] {
        if let Some(value) = std::env::var_os(variable).filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(value));
        }
    }
    Ok(crossagents_hub::Paths::from_env()?.home)
}

/// `<home>/locks`, where the executor keeps its lock files.
pub(crate) fn locks_dir() -> Result<PathBuf> {
    Ok(home()?.join("locks"))
}

/// The `build` resource [`RESOURCES_ENV`] declares, if it declares one: what the shim holds
/// for the real program it runs (docs/tasks/LOCKS.md §4.6).
///
/// A value that does not parse is treated as none rather than as an error: the shim must still
/// run the program, and `exec` reports the mistake when someone asks it for a resource.
pub(crate) fn declared_build() -> Option<Resource> {
    let declared = declared().ok()?;
    let build = declared
        .into_iter()
        .find(|declared| declared.name == BUILD)?;
    Some(Resource::new(
        BUILD,
        build.path,
        build.access.unwrap_or(Access::Exclusive),
    ))
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// `--resource` entries resolved against `CROSSAGENTS_RESOURCES`.
fn resources(specs: &[String]) -> Result<Vec<Resource>> {
    let declared = declared()?;
    let mut resources = Vec::with_capacity(specs.len());
    for spec in specs {
        let resource = match parse_spec(spec)? {
            Spec::Direct { name, path, access } => {
                Resource::new(name, path, access.unwrap_or(Access::Exclusive))
            }
            Spec::Named { name, access } => {
                let Some(found) = declared.iter().find(|declared| declared.name == name) else {
                    let known: Vec<&str> = declared
                        .iter()
                        .map(|declared| declared.name.as_str())
                        .collect();
                    if known.is_empty() {
                        bail!(
                            "resource {name} is not declared: {RESOURCES_ENV} is empty \
                             (or name its path with --resource {name}=PATH)"
                        );
                    }
                    bail!(
                        "resource {name} is not declared in {RESOURCES_ENV} (declared: {})",
                        known.join(", ")
                    );
                };
                Resource::new(
                    name,
                    found.path.clone(),
                    access.or(found.access).unwrap_or(Access::Exclusive),
                )
            }
        };
        resources.push(resource);
    }
    Ok(resources)
}

fn declared() -> Result<Vec<Declared>> {
    match std::env::var(RESOURCES_ENV) {
        Ok(text) if !text.trim().is_empty() => serde_json::from_str(&text)
            .with_context(|| format!("{RESOURCES_ENV} is not a JSON array of resources")),
        _ => Ok(Vec::new()),
    }
}

#[derive(Debug)]
enum Spec {
    Named {
        name: String,
        access: Option<Access>,
    },
    Direct {
        name: String,
        path: PathBuf,
        access: Option<Access>,
    },
}

fn parse_spec(text: &str) -> Result<Spec> {
    if let Some((name, rest)) = text.split_once('=') {
        if name.is_empty() || rest.is_empty() {
            bail!("--resource {text}: expected NAME=PATH[:shared|:exclusive]");
        }
        let (path, access) = split_access(rest);
        return Ok(Spec::Direct {
            name: name.to_owned(),
            path: PathBuf::from(path),
            access,
        });
    }
    let (name, access) = split_access(text);
    if name.is_empty() {
        bail!("--resource {text}: expected NAME[:shared|:exclusive]");
    }
    Ok(Spec::Named {
        name: name.to_owned(),
        access,
    })
}

/// `build:shared` → (`build`, shared); `C:\build` keeps its colon on Windows.
fn split_access(text: &str) -> (&str, Option<Access>) {
    match text.rsplit_once(':') {
        Some((head, tail)) => match parse_access(tail) {
            Some(access) => (head, Some(access)),
            None => (text, None),
        },
        None => (text, None),
    }
}

fn parse_access(text: &str) -> Option<Access> {
    match text {
        "shared" => Some(Access::Shared),
        "exclusive" => Some(Access::Exclusive),
        _ => None,
    }
}

/// `10m`, `30s`, `500ms`, `1h`, or a bare number of seconds.
fn parse_timeout(text: &str) -> Result<Duration, String> {
    let (number, scale) = if let Some(number) = text.strip_suffix("ms") {
        (number, 0.001)
    } else {
        match text.chars().last() {
            Some('s') => (&text[..text.len() - 1], 1.0),
            Some('m') => (&text[..text.len() - 1], 60.0),
            Some('h') => (&text[..text.len() - 1], 3600.0),
            _ => (text, 1.0),
        }
    };
    let value: f64 = number
        .parse()
        .map_err(|_| format!("{text}: expected a time like 30s, 10m or 1h"))?;
    if !value.is_finite() || value < 0.0 {
        return Err(format!("{text}: expected a time like 30s, 10m or 1h"));
    }
    Ok(Duration::from_secs_f64(value * scale))
}
