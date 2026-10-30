//! The two processes the L0 experiment needs (`docs/tasks/LOCKS.md` §5).
//!
//! It is a `[[bin]]` target only so that `tests/lock_kill_race.rs` can start a *separate*
//! process, find it with `CARGO_BIN_EXE_lock_kill_helper`, and kill that process the way an
//! outside `taskkill /f` or `kill -9` would. The file lives under `tests/` because nothing
//! but the test uses it.
//!
//! ```text
//! lock_kill_helper executor <lock> <data> <job> <job|inherit>                 (Windows)
//! lock_kill_helper executor <lock> <data> <group> <job|inherit|close|setsid>  (Unix)
//! lock_kill_helper writer <data> <interval_ms> <handle|env> <keep|close> <lock>
//! lock_kill_helper exec <locks dir> <resource path> <command> [args…]
//! ```
//!
//! `executor` takes the lock, starts a `writer` child and then waits to be killed. The
//! writer appends a line to `<data>` — every `<interval_ms>` milliseconds, or back to back
//! when that is `0` — which is the "task still using the resource" the competitor must not
//! run beside. It reports on stdout whether it really has the parent's lock handle.
//!
//! `exec` is the real executor ([`crossagents_core::exec::run`]) over one resource, for the
//! L1 regression of §7 item 1 (`tests/exec_kill_regression.rs`). It is started only to be
//! killed from outside, like a `crossagents exec` that crashed — so the writer can pass
//! `env` as its handle argument and close whatever `CROSSAGENTS_HELD` names.

#![allow(unsafe_code)]

use std::io::Write;

#[cfg(windows)]
#[path = "lock_kill_win.rs"]
mod primitives;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("executor") => {
            #[cfg(windows)]
            win::executor(&args);
            #[cfg(unix)]
            unix::executor(&args);
            #[cfg(not(any(windows, unix)))]
            {
                let _ = &args;
                eprintln!("the experiment is Windows/Unix only");
                std::process::exit(2);
            }
        }
        Some("writer") => writer(&args),
        Some("exec") => exec(&args),
        _ => {
            eprintln!(
                "usage: lock_kill_helper executor <lock> <data> <job> <mode> | writer <data> <interval_ms> <handle> <keep|close> <lock> | exec <locks> <resource> <command> [args…]"
            );
            std::process::exit(2);
        }
    }
}

/// Runs the real executor over one resource, for `tests/exec_kill_regression.rs` (§7 item 1).
///
/// The process behaves like `crossagents exec`: it takes the lock, starts the command as a
/// strict tree, records the root process and holds the lock until the tree ends. The test
/// kills it from outside and then competes for the lock.
fn exec(args: &[String]) {
    use crossagents_core::exec::{Access, Request, Resource, run};

    let locks = std::path::PathBuf::from(&args[2]);
    let resource = std::path::PathBuf::from(&args[3]);
    let command = &args[4..];
    let Some(program) = command.first() else {
        eprintln!("usage: lock_kill_helper exec <locks> <resource> <command> [args…]");
        std::process::exit(2);
    };
    let mut request = Request::new(
        command.join(" "),
        vec![Resource::new("build", resource, Access::Exclusive)],
    );
    request.agent = Some("executor".to_owned());

    let mut cmd = tokio::process::Command::new(program);
    cmd.args(&command[1..]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    match runtime.block_on(run(&locks, &request, &mut cmd)) {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(error) => {
            eprintln!("executor error: {error}");
            std::process::exit(70);
        }
    }
}

/// Tells the parent the writer is up: `steady <pid> <inherited 0|1>`.
fn ready(pid: u32, inherited: bool) {
    println!("steady {pid} {}", u8::from(inherited));
    let _ = std::io::stdout().flush();
}

/// Appends one line every `interval_ms` until killed (or after two minutes, so a wedged
/// test leaves nothing behind).
fn writer(args: &[String]) {
    let data = &args[2];
    let interval: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
    // `env`: whatever the executor passed on in `CROSSAGENTS_HELD`, for a writer started by
    // the real executor rather than by the L0 experiment.
    let handle: usize = match args.get(4).map(String::as_str) {
        Some("env") => inherited_raw(),
        other => other.and_then(|s| s.parse().ok()).unwrap_or(0),
    };
    let close = args.get(5).map(String::as_str) == Some("close");
    let lock = args.get(6).map(String::as_str).unwrap_or("");

    let inherited = probe(handle, lock);
    if close && handle != 0 {
        drop_handle(handle);
    }
    ready(std::process::id(), inherited);

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data)
        .expect("open the data file");
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(120) {
        let _ = writeln!(file, "{}", start.elapsed().as_micros());
        let _ = file.flush();
        // `0` means "no sleep": on Windows a `Sleep(1)` really waits ~15.6 ms, and the test
        // needs a heartbeat fine enough to tell a running child from a frozen one.
        if interval > 0 {
            std::thread::sleep(std::time::Duration::from_millis(interval));
        }
    }
}

/// The first handle or descriptor in `CROSSAGENTS_HELD` (`<exec-id>;<name>:<access>:<raw>,…`).
fn inherited_raw() -> usize {
    std::env::var(crossagents_core::exec::HELD_ENV)
        .ok()
        .and_then(|value| value.split(';').nth(1).map(str::to_owned))
        .and_then(|claims| claims.split(',').next().map(str::to_owned))
        .and_then(|claim| claim.rsplit(':').next().and_then(|raw| raw.parse().ok()))
        .unwrap_or(0)
}

/// Whether `handle` really is this process's handle on `lock` — same volume and file index
/// as a freshly opened copy, so a coincidental handle value cannot pass.
#[cfg(windows)]
fn probe(handle: usize, lock: &str) -> bool {
    if handle == 0 || lock.is_empty() {
        return false;
    }
    use crate::primitives::{close, file_id, open_lock};

    let Some(mine) = file_id(handle as _) else {
        return false;
    };
    let Ok(copy) = open_lock(std::ffi::OsStr::new(lock), false) else {
        return false;
    };
    let theirs = file_id(copy);
    close(copy);
    theirs == Some(mine)
}

#[cfg(windows)]
fn drop_handle(handle: usize) {
    // SAFETY: the caller passes a handle it inherited.
    unsafe {
        windows_sys::Win32::Foundation::CloseHandle(handle as _);
    }
}

#[cfg(unix)]
fn probe(fd: usize, lock: &str) -> bool {
    if fd == 0 || lock.is_empty() {
        return false;
    }
    // SAFETY: `fstat` and `stat` only fill in the structs given to them; a bad fd fails.
    unsafe {
        let mut mine: libc::stat = std::mem::zeroed();
        if libc::fstat(fd as libc::c_int, &mut mine) != 0 {
            return false;
        }
        let Ok(path) = std::ffi::CString::new(lock) else {
            return false;
        };
        let mut theirs: libc::stat = std::mem::zeroed();
        if libc::stat(path.as_ptr(), &mut theirs) != 0 {
            return false;
        }
        mine.st_dev == theirs.st_dev && mine.st_ino == theirs.st_ino
    }
}

#[cfg(unix)]
fn drop_handle(fd: usize) {
    // SAFETY: the caller passes an fd it inherited; closing it twice would be a bug, and
    // each child closes its own copy.
    unsafe {
        libc::close(fd as libc::c_int);
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::OsStr;
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    use std::time::Duration;

    use windows_sys::Win32::Foundation::HANDLE;

    use crate::primitives;

    /// Takes the lock like the executor, starts the writer and waits to be killed.
    ///
    /// `job`: the writer goes in a named job with `KILL_ON_JOB_CLOSE`, the lock handle is
    /// not inheritable — the situation §5 item 1 asks about.
    /// `inherit`: the lock handle is inheritable and there is no job, so the writer outlives
    /// the executor — §5 item 2.
    // The writer is deliberately left unreaped: this process exists to be killed from
    // outside, and whoever kills it has no interest in its exit status.
    #[allow(clippy::zombie_processes)]
    pub fn executor(args: &[String]) {
        let lock_path = &args[2];
        let data = &args[3];
        let name = &args[4];
        let mode = args.get(5).map(String::as_str).unwrap_or("job");
        let inheritable = mode == "inherit";

        let lock: HANDLE =
            primitives::open_lock(OsStr::new(lock_path), inheritable).expect("open the lock file");
        primitives::lock(lock).expect("take the lock");

        let job = if mode == "inherit" {
            None
        } else {
            Some(primitives::create_job(OsStr::new(name)).expect("create the job"))
        };

        let mut writer = Command::new(std::env::current_exe().expect("current_exe"))
            .arg("writer")
            .arg(data)
            .arg("0")
            .arg((lock as usize).to_string())
            .arg("keep")
            .arg(lock_path)
            .stdout(Stdio::piped())
            .spawn()
            .expect("start the writer");

        let pid = writer.id();
        let in_job = match job {
            Some(job) => {
                primitives::assign(job, pid).expect("put the writer in the job");
                primitives::is_in_job(job, pid)
            }
            None => false,
        };

        let mut line = String::new();
        BufReader::new(writer.stdout.take().expect("piped stdout"))
            .read_line(&mut line)
            .expect("read the writer's ready line");
        println!(
            "ready writer={pid} in_job={} {}",
            u8::from(in_job),
            line.trim()
        );
        let _ = std::io::stdout().flush();

        // The job handle must stay open, or closing it kills the writer.
        let _job = job;
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }
}

#[cfg(unix)]
mod unix {
    use std::ffi::CString;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    /// Takes the lock like the executor, starts the writer and waits to be killed.
    ///
    /// The executor makes itself a process-group leader and the writer inherits that group,
    /// which is what `spawn_tree` gives the executor (its record carries the group id).
    ///
    /// `job`: the lock fd is close-on-exec, so only the executor holds the lock and `SIGKILL`
    /// frees it while the writer keeps running (§5 item 1).
    /// `inherit`: the writer inherits the lock fd (§5 item 2).
    /// `close`: the writer inherits the lock fd and closes it (§5 item 3, first half).
    /// `setsid`: the writer leaves the process group with `setsid` (§5 item 3, second half).
    // As on Windows: the writer is left unreaped on purpose, the executor is killed from
    // outside.
    #[allow(clippy::zombie_processes)]
    pub fn executor(args: &[String]) {
        let lock_path = &args[2];
        let data = &args[3];
        // args[4] is the job name on Windows and unused here; the mode is the last one.
        let mode = args.get(5).map(String::as_str).unwrap_or("job");

        // SAFETY: our own process group; fails harmlessly if we already lead one.
        unsafe {
            libc::setpgid(0, 0);
        }

        let path = CString::new(lock_path.as_bytes()).expect("lock path without NUL");
        // SAFETY: plain syscalls; the fd is checked before use and deliberately kept
        // inheritable, which is what items 2 and 3 of §5 are about.
        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDWR | libc::O_CREAT,
                0o644 as libc::c_uint,
            )
        };
        assert!(
            fd >= 0,
            "open the lock file: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: `fd` was just opened.
        assert_eq!(
            unsafe { libc::flock(fd, libc::LOCK_EX) },
            0,
            "take the lock: {}",
            std::io::Error::last_os_error()
        );

        if mode == "job" || mode == "setsid" {
            // Only the executor keeps the lock: the child does not inherit the fd.
            // SAFETY: `fd` is ours.
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }

        let mut command = Command::new(std::env::current_exe().expect("current_exe"));
        command
            .arg("writer")
            .arg(data)
            .arg("0")
            .arg((fd as usize).to_string())
            .arg(if mode == "close" { "close" } else { "keep" })
            .arg(lock_path)
            .stdout(Stdio::piped());
        if mode == "setsid" {
            // SAFETY: `setsid` in the child between fork and exec only detaches it from the
            // group the predecessor check looks at.
            unsafe {
                command.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
        }

        let mut writer = command.spawn().expect("start the writer");
        let pid = writer.id();
        let mut line = String::new();
        BufReader::new(writer.stdout.take().expect("piped stdout"))
            .read_line(&mut line)
            .expect("read the writer's ready line");
        println!("ready writer={pid} in_job=0 {}", line.trim());
        let _ = std::io::stdout().flush();

        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }
}
