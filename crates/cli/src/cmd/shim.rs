//! Shim mode: `cargo` in a run worktree's `PATH` is this executable
//! (`docs/tasks/LOCKS.md` §4.6).
//!
//! `main` decides on it from `argv[0]` alone, before clap: a program that was started as
//! `cargo.exe` has no `crossagents` subcommands to parse — its arguments belong to the real
//! `cargo`. So the work here is:
//!
//! ```text
//! find the real cargo on PATH, skipping the directory this shim is in
//!     → equivalent to `crossagents exec --resource build -- <real cargo> <argv[1..]>`
//! ```
//!
//! That is [`crossagents_core::exec::run`], the same library `exec` and the hub's checks use,
//! so an `exec` that already holds `build` runs the shim without taking the lock again
//! (§4.5), and a lock that does not come in time ends with `75` like any other `exec`.
//!
//! A checkout that declares no `build` resource, or an environment that carries none
//! (`CROSSAGENTS_RESOURCES` absent, which is every shell outside a run worktree), runs the
//! real program directly: a shim that fails where the plain program would work would be
//! worse than no shim (§4.6).

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use crossagents_core::exec::{
    self, DEFAULT_NOTICE_EVERY, DEFAULT_TIMEOUT, EXIT_TEMPFAIL, ExecError, Request,
};

use super::exec as executor;

/// What a shell's exit code would be for a program that is not there.
const EXIT_NOT_FOUND: i32 = 127;

/// Runs in shim mode and returns the exit code the process should end with.
pub fn main(name: &'static str) -> ExitCode {
    match run(name) {
        Ok(code) => ExitCode::from(code.rem_euclid(256) as u8),
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(name: &str) -> Result<i32> {
    let exe = std::env::current_exe().context("cannot locate the shim itself")?;
    let own_dir = exe.parent().unwrap_or(&exe).to_path_buf();
    let Some(program) = crossagents_hub::shims::real(name, &own_dir) else {
        eprintln!(
            "{name}: the program is not on PATH outside {} (docs/tasks/LOCKS.md §4.6)",
            own_dir.display()
        );
        return Ok(EXIT_NOT_FOUND);
    };
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match executor::declared_build() {
        Some(build) => under_the_lock(name, &program, &args, build),
        // No `build` declared: this is no run worktree, and the program runs as it would
        // without the shim.
        None => direct(&program, &args),
    }
}

/// The real program under the run's `build` resource: what
/// `crossagents exec --resource build -- <program> <args>` does.
fn under_the_lock(
    name: &str,
    program: &Path,
    args: &[OsString],
    build: exec::Resource,
) -> Result<i32> {
    let locks_dir = executor::locks_dir()?;
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args);
    let mut request = Request::new(command_line(name, args), vec![build]);
    request.agent = env("CROSSAGENTS_AGENT");
    request.run = env("CROSSAGENTS_RUN");
    request.timeout = DEFAULT_TIMEOUT;
    request.notice_every = DEFAULT_NOTICE_EVERY;

    let runtime = tokio::runtime::Runtime::new().context("cannot start the async runtime")?;
    let status = match runtime.block_on(exec::run(&locks_dir, &request, &mut cmd)) {
        Ok(status) => status,
        Err(error @ ExecError::Timeout { .. }) => {
            eprintln!("error: {error}");
            return Ok(EXIT_TEMPFAIL);
        }
        // A resource the outer exec does not hold, or a shared → exclusive upgrade: the
        // request cannot run, and waiting would never fix it (§4.5).
        Err(error @ ExecError::Nesting(_)) => {
            eprintln!("error: {error}");
            return Ok(1);
        }
        Err(error) => return Err(error.into()),
    };
    Ok(status.code().unwrap_or(1))
}

/// The program, unchanged: no locks, no process tree of the executor's.
fn direct(program: &Path, args: &[OsString]) -> Result<i32> {
    let status = std::process::Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("cannot run {}", program.display()))?;
    Ok(status.code().unwrap_or(1))
}

/// `cargo test --workspace`: the command line holder records and notices show.
fn command_line(name: &str, args: &[OsString]) -> String {
    let mut line = name.to_owned();
    for arg in args {
        line.push(' ');
        line.push_str(&arg.to_string_lossy());
    }
    line
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}
