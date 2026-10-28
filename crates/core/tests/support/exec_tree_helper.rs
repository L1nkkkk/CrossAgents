//! A child for `tests/exec_tree.rs` (`docs/tasks/LOCKS.md` §4.3, §4.5, §7 item 4).
//!
//! ```text
//! exec_tree_helper <milliseconds>
//! ```
//!
//! As the root: closes every lock handle or descriptor inherited through `CROSSAGENTS_HELD`
//! (the thing §7 item 4 is about), starts a `--sleep` copy of itself that outlives it, and
//! exits at once. The grandchild sleeps for the given time and then exits.
//!
//! Exit codes: `0` the inherited handles were found and closed; `3` `CROSSAGENTS_HELD` was
//! missing; `4` it named no claims. The test uses them to prove the environment and the
//! inheritance really reached the child.

use std::process::Command;

use crossagents_core::exec::HELD_ENV;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) == Some("--sleep") {
        let millis: u64 = arguments
            .get(1)
            .and_then(|argument| argument.parse().ok())
            .unwrap_or(0);
        std::thread::sleep(std::time::Duration::from_millis(millis));
        return;
    }
    let millis: u64 = arguments
        .first()
        .and_then(|argument| argument.parse().ok())
        .unwrap_or(0);

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

    // The grandchild stays in the same job (Windows) or process group (Unix) and keeps the
    // tree alive after this process is gone.
    let exe = std::env::current_exe().expect("the helper's own path");
    let _ = Command::new(exe)
        .arg("--sleep")
        .arg(millis.to_string())
        .spawn();
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
