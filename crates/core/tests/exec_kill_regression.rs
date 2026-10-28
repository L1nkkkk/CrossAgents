//! L1 regression: what happens to the lock when the executor is killed, and what the tree
//! means for the lock when the child closes its inherited handles
//! (`docs/tasks/LOCKS.md` §5 conclusions, §7 items 1 and 4).
//!
//! Both tests run the **real** executor (`crossagents_core::exec::run`, the same code path as
//! `crossagents exec`) in a separate process, started by `tests/support/lock_kill_helper.rs`
//! in its `exec` mode. Nothing here re-implements the lock; the competitor is the library's
//! own [`acquire`], so the predecessor check (§5 conclusion 3) is part of what is measured.
//!
//! Item 1: the executor is killed while its child writes to a file. The moment [`acquire`]
//! returns, the old child must be gone and the file must not grow. That is the regression of
//! the window L0 measured: on Windows the lock disappeared 0.14–0.32 ms after the kill while
//! the job's processes took 7–10 ms to go, so the check that waits for the recorded root
//! process is what closes it. Without that check this test would see a live writer here.
//!
//! Item 4: a child that closes the inherited handle or descriptor and leaves a grandchild
//! behind does not end the executor's hold — the lock stays taken until the whole tree ends.
//! On Unix a descendant that leaves the process group (`setsid`) is outside the tree, and
//! §5 says that is the limit of the guarantee; the `#[cfg(unix)]` test pins that behaviour
//! too, so a future change cannot quietly claim more.
//!
//! The Unix half has not been run (this machine is Windows), as §5 records.

#![allow(unsafe_code)]

#[cfg(windows)]
#[path = "support/lock_kill_win.rs"]
mod win;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crossagents_core::exec::{Access, ExecError, Request, Resource, acquire, holders};
use tempfile::TempDir;

/// Started as the executor (`lock_kill_helper exec …`) and, by it, as the writer.
const HELPER: &str = env!("CARGO_BIN_EXE_lock_kill_helper");

/// Closes the inherited handles and leaves a grandchild behind (`exec_tree_helper`).
const TREE_HELPER: &str = env!("CARGO_BIN_EXE_exec_tree_helper");

fn temp() -> TempDir {
    tempfile::tempdir().expect("a temporary directory")
}

fn millis(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

/// Rounds of the kill test: enough to see the race, `CROSSAGENTS_L1_ROUNDS` for more.
fn rounds(fallback: u32) -> u32 {
    std::env::var("CROSSAGENTS_L1_ROUNDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(fallback)
}

fn length(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(millis(2));
    }
}

/// `steady 1234 0` → `1234`: the writer's PID, as the executor's record names it.
fn ready_pid(line: &str) -> u32 {
    line.split_whitespace()
        .nth(1)
        .and_then(|pid| pid.parse().ok())
        .unwrap_or_else(|| panic!("no writer pid in {line:?}"))
}

fn request(build: &Path, timeout: Duration) -> Request {
    Request {
        resources: vec![Resource::new("build", build, Access::Exclusive)],
        command: "cargo test".to_owned(),
        agent: Some("competitor".to_owned()),
        run: Some("432bef52".to_owned()),
        timeout,
        notice_every: millis(100),
    }
}

/// Is `pid` still running? The same question the predecessor check asks of the record.
#[cfg(windows)]
fn alive(pid: u32) -> bool {
    win::alive(pid)
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 sends nothing; it only asks whether the pid exists.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(any(windows, unix)))]
fn alive(_pid: u32) -> bool {
    true
}

/// Kills the executor even when an assertion fails first, so a round leaves nothing behind.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A fresh locks directory, build directory and data file per round: two rounds must not
/// share a lock file, a record or a writer.
fn round_dir() -> (TempDir, PathBuf, PathBuf, PathBuf) {
    let dir = temp();
    let locks = dir.path().join("locks");
    let build = dir.path().join("build");
    std::fs::create_dir_all(&build).expect("create the build directory");
    let data = dir.path().join("data.txt");
    (dir, locks, build, data)
}

/// Starts the real executor running `command` under an exclusive `build` lock.
fn executor(locks: &Path, build: &Path, command: &[&str]) -> Running {
    let mut child = Command::new(HELPER);
    child
        .arg("exec")
        .arg(locks)
        .arg(build)
        .args(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        // The test itself may run under an executor; this one declares its own resources.
        .env_remove("CROSSAGENTS_HELD")
        .env_remove("CROSSAGENTS_RESOURCES");
    Running(child.spawn().expect("start the executor"))
}

// --- §7 item 1: the lock is only handed over once the old task has stopped ---------------

#[tokio::test]
async fn exec_killed_executor_leaves_no_window_for_the_next_holder() {
    let rounds = rounds(5);
    let mut slowest = Duration::ZERO;
    for round in 0..rounds {
        let (_dir, locks, build, data) = round_dir();
        let mut executor = executor(
            &locks,
            &build,
            &[
                HELPER,
                "writer",
                data.to_str().expect("a UTF-8 data path"),
                "0",
                "env",
                "close",
                "",
            ],
        );

        // The writer is up and writing; the record already names it, which is what the
        // predecessor check will read.
        let mut ready = String::new();
        BufReader::new(executor.0.stdout.take().expect("piped stdout"))
            .read_line(&mut ready)
            .expect("the writer's ready line");
        let writer = ready_pid(&ready);
        wait_until("the holder record to name the writer", || {
            holders(&locks, &build)
                .iter()
                .any(|holder| holder.root_pid == Some(writer))
        });
        wait_until("the writer's first line", || length(&data) > 0);

        // An outside kill: TerminateProcess on Windows, SIGKILL elsewhere — the case §5 is
        // about. The lock goes before the job's processes do.
        let killed = Instant::now();
        executor.0.kill().expect("kill the executor");
        executor.0.wait().expect("reap the executor");

        let held = acquire(&locks, &request(&build, Duration::from_secs(15)), |_| {})
            .await
            .expect("the lock comes free once the predecessor is gone");
        let took = killed.elapsed();

        // The moment the lock is ours: the old writer must be gone, and it must not write.
        let still_alive = alive(writer);
        let before = length(&data);
        std::thread::sleep(millis(100));
        let grew = length(&data) > before;
        assert!(
            !still_alive,
            "round {round}: the old writer was still running when the lock changed hands \
             (after {took:?})"
        );
        assert!(
            !grew,
            "round {round}: the old writer kept writing after the lock changed hands \
             (after {took:?})"
        );
        drop(held);
        slowest = slowest.max(took);
    }
    println!("killed executor: {rounds} rounds without a window, slowest hand-over {slowest:?}");
}

// --- §7 item 4: closing the inherited handles does not end the hold ------------------------

#[tokio::test]
async fn exec_executor_holds_the_lock_until_the_tree_ends_after_its_handles_are_closed() {
    let (_dir, locks, build, _data) = round_dir();
    let mut executor = executor(&locks, &build, &[TREE_HELPER, "5000", "--ready"]);

    // The root closes whatever `CROSSAGENTS_HELD` named and starts a grandchild that keeps
    // the tree alive for five seconds, then exits.
    let mut closed = String::new();
    BufReader::new(executor.0.stdout.take().expect("piped stdout"))
        .read_line(&mut closed)
        .expect("the helper's ready line");
    assert!(
        closed.trim().starts_with("closed "),
        "the child must find and close the inherited handles, got {closed:?}"
    );

    // The root is gone; only the grandchild is left. The record stops being listed once its
    // root process has exited, so an empty list means the hold is the tree's alone.
    wait_until("the root process to exit", || {
        holders(&locks, &build).is_empty()
    });
    assert!(
        executor.0.try_wait().expect("ask the executor").is_none(),
        "the executor let go of the lock when its root process exited"
    );
    let error = acquire(&locks, &request(&build, millis(500)), |_| {})
        .await
        .expect_err("the tree is still running, so the lock must stay held");
    assert!(matches!(error, ExecError::Timeout { .. }), "{error}");

    let status = executor.0.wait().expect("the executor ends");
    assert!(
        status.success(),
        "the executor only ends after the whole tree: {status:?}"
    );

    acquire(&locks, &request(&build, Duration::from_secs(15)), |_| {})
        .await
        .expect("once the tree has ended the lock comes free");
}

/// §5's Unix limit, pinned: a descendant that leaves the process group is outside the tree,
/// so the lock is released when the root exits even though the writer still runs. This is
/// the documented gap, not a guarantee — a change that "fixes" it should update §5 first.
#[cfg(unix)]
#[tokio::test]
async fn exec_executor_lets_a_detached_writer_go() {
    let (_dir, locks, build, data) = round_dir();
    let data = data.to_str().expect("a UTF-8 data path").to_owned();
    let mut executor = executor(&locks, &build, &[TREE_HELPER, "--detach", "3000", &data]);

    let mut closed = String::new();
    BufReader::new(executor.0.stdout.take().expect("piped stdout"))
        .read_line(&mut closed)
        .expect("the helper's ready line");
    assert!(
        closed.trim().starts_with("closed "),
        "the child must find and close the inherited handles, got {closed:?}"
    );
    wait_until("the detached writer's first line", || {
        length(Path::new(&data)) > 0
    });

    let status = executor.0.wait().expect("the executor ends");
    assert!(status.success(), "{status:?}");

    // Root gone, group empty: the executor considers the tree over, although the writer is
    // still going — the documented limit of the Unix guarantee (§5).
    let started = Instant::now();
    let held = acquire(&locks, &request(&build, Duration::from_secs(5)), |_| {})
        .await
        .expect("the detached writer is outside the tree");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the lock took {:?}",
        started.elapsed()
    );
    let before = length(Path::new(&data));
    std::thread::sleep(millis(100));
    assert!(
        length(Path::new(&data)) > before,
        "the detached writer should still be writing under the new holder — that is the gap \
         §5 records"
    );
    drop(held);
}
