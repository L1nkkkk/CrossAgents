//! A child for `tests/exec_tree.rs` and `tests/exec_kill_regression.rs`
//! (`docs/tasks/LOCKS.md` §4.3, §4.5, §7 item 4).
//!
//! ```text
//! exec_tree_helper <milliseconds> [--ready]
//! exec_tree_helper --detach <milliseconds> <data>        (Unix)
//! exec_tree_helper --detached-writer <milliseconds> <data>   (Unix)
//! ```
//!
//! As the root: closes every lock handle or descriptor inherited through `CROSSAGENTS_HELD`
//! (the thing §7 item 4 is about), starts a `--sleep` copy of itself that outlives it, and
//! exits at once. The grandchild sleeps for the given time and then exits.
//!
//! `--ready` prints `closed <count>` before starting the grandchild, so a caller can wait for
//! the point at which the handles are closed and the tree is still running.
//!
//! `--detach` is the same, except the grandchild leaves the process group with `setsid` and
//! writes to `<data>` until it has run for the given time. §5's Unix conclusion says such a
//! descendant is outside the tree — the lock is released when the root exits even though it
//! is still writing — and the regression test asserts exactly that limit.
//!
//! Exit codes: `0` the inherited handles were found and closed; `3` `CROSSAGENTS_HELD` was
//! missing; `4` it named no claims. The test uses them to prove the environment and the
//! inheritance really reached the child.

use std::process::Command;

use crossagents_core::exec::HELD_ENV;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("--sleep") => {
            let millis = arguments
                .get(1)
                .and_then(|argument| argument.parse().ok())
                .unwrap_or(0);
            std::thread::sleep(std::time::Duration::from_millis(millis));
            return;
        }
        Some("--detach") => {
            #[cfg(unix)]
            {
                unix::detach(
                    millis(&arguments, 1),
                    arguments.get(2).map(String::as_str).unwrap_or(""),
                );
                return;
            }
            #[cfg(not(unix))]
            {
                eprintln!("--detach is Unix only");
                std::process::exit(2);
            }
        }
        Some("--detached-writer") => {
            #[cfg(unix)]
            {
                unix::detached_writer(
                    millis(&arguments, 1),
                    arguments.get(2).map(String::as_str).unwrap_or(""),
                );
                return;
            }
            #[cfg(not(unix))]
            {
                eprintln!("--detached-writer is Unix only");
                std::process::exit(2);
            }
        }
        _ => {}
    }
    let millis: u64 = arguments
        .first()
        .and_then(|argument| argument.parse().ok())
        .unwrap_or(0);

    let closed = close_inherited();
    if arguments.iter().any(|argument| argument == "--ready") {
        println!("closed {closed}");
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }

    // The grandchild stays in the same job (Windows) or process group (Unix) and keeps the
    // tree alive after this process is gone.
    let exe = std::env::current_exe().expect("the helper's own path");
    let _ = Command::new(exe)
        .arg("--sleep")
        .arg(millis.to_string())
        .spawn();
}

/// Closes every handle or descriptor named by `CROSSAGENTS_HELD`; exits when there are none.
fn close_inherited() -> usize {
    let Some(value) = std::env::var_os(HELD_ENV) else {
        std::process::exit(3);
    };
    let value = value.to_string_lossy().into_owned();
    let mut closed = 0usize;
    for claim in value.split(';').nth(1).unwrap_or_default().split(',') {
        let Some(raw) = claim
            .rsplit(':')
            .next()
            .and_then(|raw| raw.parse::<i64>().ok())
        else {
            continue;
        };
        close(raw);
        closed += 1;
    }
    if closed == 0 {
        std::process::exit(4);
    }
    closed
}

#[cfg(unix)]
fn millis(arguments: &[String], at: usize) -> u64 {
    arguments
        .get(at)
        .and_then(|argument| argument.parse().ok())
        .unwrap_or(0)
}

#[cfg(unix)]
#[allow(unsafe_code)]
mod unix {
    use std::io::Write;
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    use std::time::{Duration, Instant};

    /// Closes the inherited descriptors, starts a `--detached-writer` in a session of its own
    /// and exits. That writer is outside the process group the executor watches.
    pub fn detach(millis: u64, data: &str) {
        let closed = super::close_inherited();
        println!("closed {closed}");
        let _ = std::io::stdout().flush();

        let exe = std::env::current_exe().expect("the helper's own path");
        let mut command = Command::new(exe);
        command
            .arg("--detached-writer")
            .arg(millis.to_string())
            .arg(data);
        // SAFETY: `setsid` in the child between fork and exec only leaves the group the
        // executor's tree is made of.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let _ = command.spawn();
    }

    /// Appends a line to `data` until `millis` have passed.
    pub fn detached_writer(millis: u64, data: &str) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(data)
            .expect("open the data file");
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(millis) {
            let _ = writeln!(file, "{}", start.elapsed().as_micros());
            let _ = file.flush();
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn close(raw: i64) {
    use windows_sys::Win32::Foundation::CloseHandle;

    // SAFETY: the value is a handle this process inherited; closing an invalid one just fails.
    unsafe {
        CloseHandle(raw as isize as _);
    }
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn close(raw: i64) {
    // SAFETY: the descriptor was inherited into this process; closing an invalid one just
    // fails with EBADF.
    unsafe {
        libc::close(raw as i32);
    }
}

#[cfg(not(any(windows, unix)))]
fn close(_raw: i64) {}
