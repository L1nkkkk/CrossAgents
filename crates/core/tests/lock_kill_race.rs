//! L0: can a competitor take the lock while the dead executor's task still uses the
//! resource? (`docs/tasks/LOCKS.md` §5)
//!
//! This is not a unit test of anything in this crate. It starts two real processes
//! (`tests/support/lock_kill_helper.rs`): one takes the lock like the executor will and runs
//! a child that appends a line to a file every millisecond; the test then kills the lock
//! holder the way an outside `taskkill /f` or `kill -9` would. Right after the kill the test
//! competes for the lock in a tight loop and, the moment it gets it, records:
//!
//! * whether the old child is still running,
//! * how long it lived after the lock changed hands,
//! * how much the file grew in that time — the old task actually using the resource while
//!   the competitor holds the lock.
//!
//! A *window* is a round where the child was alive, or wrote, after the lock was ours.
//!
//! Windows runs here. The Unix half is written out with `#[cfg(unix)]` but has never run:
//! §5 says so, and its conclusion stays weak until somebody runs it on Linux.
//!
//! ```text
//! cargo test -p crossagents-core --test lock_kill_race                    # few rounds, prints the counts
//! cargo test -p crossagents-core --test lock_kill_race -- --ignored --nocapture
//! CROSSAGENTS_L0_ROUNDS=200 cargo test -p crossagents-core --test lock_kill_race -- --ignored --nocapture
//! ```
//!
//! Neither run fails because a window exists: the window is the result. It fails only when
//! the experiment itself breaks — the lock is never released, a round leaves a process
//! behind, the helper does not start.

#![allow(unsafe_code)]

#[cfg(windows)]
#[path = "support/lock_kill_win.rs"]
mod win;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The helper lives in the sibling `lock_kill_helper` binary target (see `Cargo.toml`), so
/// the test can start a process it is allowed to kill and can find it here.
const HELPER: &str = env!("CARGO_BIN_EXE_lock_kill_helper");

/// Rounds per test: low by default so the check stays quick, `CROSSAGENTS_L0_ROUNDS` to
/// repeat more, the `#[ignore]`d variants for the numbers in §5.
fn rounds(fallback: u32) -> u32 {
    std::env::var("CROSSAGENTS_L0_ROUNDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(fallback)
}

/// A fresh directory per round: two rounds must not share the lock file or the data file.
fn round_dir(round: u32) -> std::path::PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "crossagents-l0-{}-{round}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create the round directory");
    dir
}

fn length(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

/// The experiments race for one lock, kill processes and spin in tight loops, so two of them
/// running at once (cargo runs the tests of this binary in parallel by default) steal CPU and
/// lock time from each other and make the windows unreliable. Every entry point takes this
/// guard, so they run one after another inside this binary no matter how many test threads
/// there are. No flag has to be remembered: `cargo test` alone is enough.
fn exclusive() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // A panic in one experiment poisons the mutex; the next one still has to run.
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    // This cap only bounds a failure: the loop returns as soon as `done` is true. Under the
    // load of a parallel `cargo build` a helper can take well over 10 s to show up, so the
    // cap is 60 s rather than a number that passes on an idle machine only.
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// `writer=123` out of the executor's ready line.
fn writer_pid(ready: &str) -> u32 {
    let rest = ready
        .split_whitespace()
        .find_map(|word| word.strip_prefix("writer="))
        .unwrap_or_else(|| panic!("no writer pid in {ready:?}"));
    rest.parse().expect("a numeric writer pid")
}

/// `steady 123 1` out of the writer's ready line: did it really inherit the lock handle?
fn inherited(ready: &str) -> bool {
    ready.split_whitespace().last().map(str::trim) == Some("1")
}

#[cfg(windows)]
fn median(mut values: Vec<u128>) -> u128 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    values[values.len() / 2]
}

/// What the competitor does after it gets the lock, before trusting it (§5 item 4).
#[cfg(windows)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Predecessor {
    /// Take the lock and go: §5 item 1.
    Nothing,
    /// Ask the predecessor's named job whether anything of it is still running.
    Job,
    /// Ask whether the recorded root PID is still that process and still running — what
    /// the holder record (§4.4) can support even after the job is gone.
    RootPid,
}

#[cfg(windows)]
impl Predecessor {
    fn label(self) -> &'static str {
        match self {
            Self::Nothing => "plain kill",
            Self::Job => "job check",
            Self::RootPid => "root pid check",
        }
    }
}

#[cfg(windows)]
#[derive(Default)]
struct WindowsRound {
    acquired: bool,
    /// From the kill to the moment the lock was ours.
    acquire_us: u128,
    /// The old child was still running when the lock was ours.
    writer_alive: bool,
    /// How long the old child lived after that (`None`: it had already exited).
    writer_died_us: Option<u128>,
    /// Bytes the old child appended after the lock was ours.
    bytes_after_lock: u64,
    /// The job's `ActiveProcesses` before the kill (`None`: no such job).
    job_while_running: Option<u32>,
    /// ... and at the moment the lock was ours.
    job_at_lock: Option<u32>,
    /// Times the predecessor check handed the lock back.
    held_back: u32,
    /// The child outlived the round and had to be killed.
    had_to_kill: bool,
}

#[cfg(windows)]
impl WindowsRound {
    /// The question §5 asks: we held the lock while the old task was still on the resource.
    fn window(&self) -> bool {
        self.writer_alive || self.bytes_after_lock > 0
    }
}

/// One round of §5 item 1, and item 4 when `check` is not `Nothing`.
///
/// The child goes in a named job with `KILL_ON_JOB_CLOSE` and the lock handle is not
/// inheritable — what `spawn_tree` gives the executor today. The inheritable-handle case is
/// [`inherited_round`], which needs no job (the child must outlive the executor).
#[cfg(windows)]
fn windows_round(round: u32, check: Predecessor) -> WindowsRound {
    use std::ffi::OsStr;

    let mut seen = WindowsRound::default();
    let dir = round_dir(round);
    let lock_path = dir.join("build.lock");
    let data_path = dir.join("data.txt");
    // `Local\`: per session. The round directory keeps two tests from sharing a job name.
    let job_name = format!(
        "Local\\crossagents-l0-{}",
        dir.file_name().expect("a directory name").to_string_lossy()
    );

    let mut executor = Command::new(HELPER)
        .arg("executor")
        .arg(&lock_path)
        .arg(&data_path)
        .arg(&job_name)
        .arg("job")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start the executor");
    let mut ready = String::new();
    BufReader::new(executor.stdout.take().expect("piped stdout"))
        .read_line(&mut ready)
        .expect("the executor's ready line");
    let pid = writer_pid(&ready);
    // The writer's first append comes right after its ready line.
    wait_until("the writer's first line", || length(&data_path) > 0);

    // Before the kill: the job exists and holds the writer. This is also the check that the
    // name is right, so "the job is gone" below means something.
    seen.job_while_running = win::open_job(OsStr::new(&job_name)).ok().map(|job| {
        let active = win::active_processes(job).unwrap_or(0);
        win::close(job);
        active
    });
    // Recorded like the holder record records it (§4.4), so a reused PID cannot fool the
    // `RootPid` check.
    let created_at = win::created(pid);

    // An outside kill, not the executor's own shutdown: this is the case §5 is about.
    executor.kill().expect("kill the executor");
    executor.wait().expect("reap the executor");

    let started = Instant::now();
    let handle = win::open_lock(lock_path.as_os_str(), false).expect("open the lock file");
    let deadline = started + Duration::from_secs(10);
    while Instant::now() < deadline {
        if win::try_lock(handle).expect("try to lock") {
            let busy = match check {
                Predecessor::Nothing => false,
                Predecessor::Job => {
                    // §5 item 4 as written: the predecessor's job still lists processes?
                    match win::open_job(OsStr::new(&job_name)) {
                        Ok(job) => {
                            let active = win::active_processes(job).unwrap_or(0);
                            win::close(job);
                            seen.job_at_lock = Some(active);
                            active > 0
                        }
                        Err(_) => {
                            seen.job_at_lock = None;
                            false
                        }
                    }
                }
                Predecessor::RootPid => win::alive_since(pid, created_at),
            };
            if busy {
                seen.held_back += 1;
                win::unlock(handle);
                std::hint::spin_loop();
                continue;
            }
            seen.acquired = true;
            break;
        }
        std::hint::spin_loop();
    }
    seen.acquire_us = started.elapsed().as_micros();

    if seen.acquired {
        seen.writer_alive = win::alive(pid);
        let before = length(&data_path);
        // Long enough for the job's kill to land; `None` means the child outlived it.
        seen.writer_died_us =
            win::wait_exit(pid, Duration::from_secs(5)).map(|took| took.as_micros());
        seen.bytes_after_lock = length(&data_path).saturating_sub(before);
        win::unlock(handle);
    }
    win::close(handle);

    // A round leaves nothing behind: the job should have taken the writer with it.
    let deadline = Instant::now() + Duration::from_secs(2);
    while win::alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    if win::alive(pid) {
        seen.had_to_kill = true;
        let _ = win::kill(pid);
    }
    let _ = std::fs::remove_dir_all(&dir);
    seen
}

/// Prints the counts §5 asks for and returns them, so a failure message can carry them.
#[cfg(windows)]
fn windows_summary(check: Predecessor, seen: &[WindowsRound]) -> String {
    let count = |what: fn(&WindowsRound) -> bool| seen.iter().filter(|round| what(round)).count();
    let job_seen = seen
        .iter()
        .filter(|round| round.job_while_running.is_some())
        .count();
    let job_one = seen
        .iter()
        .filter(|round| round.job_while_running == Some(1))
        .count();
    let job_at_lock = if check == Predecessor::Job {
        let states: Vec<String> = seen
            .iter()
            .filter(|round| round.acquired)
            .map(|round| match round.job_at_lock {
                Some(active) => active.to_string(),
                None => "gone".to_owned(),
            })
            .collect();
        format!(" | job at lock: {}", states.join(","))
    } else {
        String::new()
    };
    let had_to_kill = count(|round| round.had_to_kill);
    let summary = format!(
        "Windows, {}: {} rounds | lock taken {} | window {} (writer alive {}, wrote after \
         the lock {}) | job before kill: {} ({} with 1 process){} | held back \
         {} | median kill→lock {} µs | median writer death after lock {} µs | child left \
         behind {}",
        check.label(),
        seen.len(),
        count(|round| round.acquired),
        count(WindowsRound::window),
        count(|round| round.writer_alive),
        count(|round| round.bytes_after_lock > 0),
        job_seen,
        job_one,
        job_at_lock,
        seen.iter().map(|round| round.held_back).sum::<u32>(),
        median(seen.iter().map(|round| round.acquire_us).collect()),
        median(
            seen.iter()
                .filter_map(|round| round.writer_died_us)
                .collect()
        ),
        had_to_kill,
    );
    println!("{summary}");
    summary
}

#[cfg(windows)]
fn run_windows(check: Predecessor, count: u32) -> String {
    let _exclusive = exclusive();
    let seen: Vec<_> = (0..count)
        .map(|round| windows_round(round, check))
        .collect();
    let summary = windows_summary(check, &seen);
    assert!(
        seen.iter().all(|round| round.acquired),
        "the lock was never released after the executor was killed; {summary}"
    );
    assert!(
        seen.iter().all(|round| !round.had_to_kill),
        "a round left a child running; {summary}"
    );
    summary
}

#[cfg(windows)]
#[derive(Default)]
struct InheritedRound {
    /// The child really held the inherited lock handle (same volume and file index).
    inherited: bool,
    /// We took the lock while the child was still alive and writing.
    acquired_while_alive: bool,
    /// The file grew under us in the 30 ms after we took it.
    file_grew: bool,
    /// How long after killing the child the lock became available.
    released_after_kill_ms: Option<u128>,
}

/// §5 item 2: the lock handle is inheritable and the child inherits it. There is no job this
/// time, so the child outlives the executor and keeps the inherited handle open: is the lock
/// still held?
#[cfg(windows)]
fn inherited_round(round: u32) -> InheritedRound {
    let mut seen = InheritedRound::default();
    let dir = round_dir(1_000 + round);
    let lock_path = dir.join("build.lock");
    let data_path = dir.join("data.txt");
    let job_name = format!(
        "Local\\crossagents-l0-inherit-{}",
        dir.file_name().expect("a directory name").to_string_lossy()
    );

    let mut executor = Command::new(HELPER)
        .arg("executor")
        .arg(&lock_path)
        .arg(&data_path)
        .arg(&job_name)
        .arg("inherit")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start the executor");
    let mut ready = String::new();
    BufReader::new(executor.stdout.take().expect("piped stdout"))
        .read_line(&mut ready)
        .expect("the executor's ready line");
    let pid = writer_pid(&ready);
    seen.inherited = inherited(&ready);
    wait_until("the writer's first line", || length(&data_path) > 0);

    executor.kill().expect("kill the executor");
    executor.wait().expect("reap the executor");

    let handle = win::open_lock(lock_path.as_os_str(), false).expect("open the lock file");
    let started = Instant::now();
    // Long enough for the executor's own handle to be gone; if the child's inherited handle
    // counted, the lock would stay away for all of it.
    let while_alive = Duration::from_millis(500);
    while Instant::now() < started + while_alive {
        if win::try_lock(handle).expect("try to lock") {
            seen.acquired_while_alive = win::alive(pid);
            let before = length(&data_path);
            std::thread::sleep(Duration::from_millis(30));
            seen.file_grew = length(&data_path) > before;
            win::unlock(handle);
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    if seen.released_after_kill_ms.is_none() && !seen.acquired_while_alive {
        // The child is the only other holder left: kill it and see the lock go.
        let killed_at = Instant::now();
        let _ = win::kill(pid);
        let deadline = killed_at + Duration::from_secs(5);
        while Instant::now() < deadline {
            if win::try_lock(handle).expect("try to lock") {
                seen.released_after_kill_ms = Some(killed_at.elapsed().as_millis());
                win::unlock(handle);
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    win::close(handle);

    let deadline = Instant::now() + Duration::from_secs(2);
    while win::alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    if win::alive(pid) {
        let _ = win::kill(pid);
    }
    let _ = std::fs::remove_dir_all(&dir);
    seen
}

#[cfg(windows)]
fn run_inherited_rounds(count: u32) -> String {
    let _exclusive = exclusive();
    let seen: Vec<_> = (0..count).map(inherited_round).collect();
    let count_where =
        |what: fn(&InheritedRound) -> bool| seen.iter().filter(|round| what(round)).count();
    let summary = format!(
        "Windows, inherited lock handle: {} rounds | child inherited the handle {} | lock \
         taken while the child was alive {} (it wrote under the lock {}) | lock held past the \
         executor's death {} (released {:?} ms after the child was killed)",
        seen.len(),
        count_where(|round| round.inherited),
        count_where(|round| round.acquired_while_alive),
        count_where(|round| round.file_grew),
        count_where(|round| round.released_after_kill_ms.is_some()),
        seen.iter()
            .filter_map(|round| round.released_after_kill_ms)
            .min(),
    );
    println!("{summary}");
    summary
}

// --- Windows entry points -------------------------------------------------------------

#[cfg(windows)]
#[test]
fn kill_does_not_wait_for_the_writers_job() {
    run_windows(Predecessor::Nothing, rounds(5));
}

#[cfg(windows)]
#[test]
#[ignore = "100 rounds; for the numbers in docs/tasks/LOCKS.md §5"]
fn kill_does_not_wait_for_the_writers_job_100() {
    run_windows(Predecessor::Nothing, rounds(100));
}

#[cfg(windows)]
#[test]
fn predecessor_job_check_cannot_close_the_window() {
    run_windows(Predecessor::Job, rounds(5));
}

#[cfg(windows)]
#[test]
#[ignore = "100 rounds; for the numbers in docs/tasks/LOCKS.md §5"]
fn predecessor_job_check_cannot_close_the_window_100() {
    run_windows(Predecessor::Job, rounds(100));
}

#[cfg(windows)]
#[test]
fn predecessor_root_pid_check_closes_the_window() {
    run_windows(Predecessor::RootPid, rounds(5));
}

#[cfg(windows)]
#[test]
#[ignore = "100 rounds; for the numbers in docs/tasks/LOCKS.md §5"]
fn predecessor_root_pid_check_closes_the_window_100() {
    run_windows(Predecessor::RootPid, rounds(100));
}

#[cfg(windows)]
#[test]
fn inherited_lock_handle_does_not_outlive_the_executor() {
    run_inherited_rounds(rounds(3));
}

#[cfg(windows)]
#[test]
#[ignore = "50 rounds; for the numbers in docs/tasks/LOCKS.md §5"]
fn inherited_lock_handle_does_not_outlive_the_executor_50() {
    run_inherited_rounds(rounds(50));
}

// --- Unix: written from the same plan, never run (this machine is Windows) ------------

#[cfg(unix)]
mod unix_experiment {
    //! §5's Unix half: `flock` in a real process, `SIGKILL`, a child that closes the
    //! inherited fd, a child that leaves the process group with `setsid`, and the
    //! predecessor check by process group (`kill(-pgid, 0)`).
    //!
    //! **Never run.** It is here so whoever runs L0 on Linux has the experiment ready; §5
    //! keeps the weak wording until those numbers exist.

    use std::os::unix::io::AsRawFd;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::{HELPER, length, round_dir, rounds, wait_until, writer_pid};

    fn flock_try(file: &std::fs::File) -> bool {
        // SAFETY: `flock` only looks at the fd; a failed non-blocking lock returns -1.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
    }

    fn flock_unlock(file: &std::fs::File) {
        // SAFETY: the same fd that took the lock.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    }

    fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 sends nothing, it only asks whether the pid exists.
        unsafe {
            if libc::kill(pid as libc::pid_t, 0) == 0 {
                return true;
            }
            std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
        }
    }

    /// The predecessor check of §5 item 4: is there still a process in group `pgid`?
    fn group_alive(pgid: u32) -> bool {
        // SAFETY: as for `alive`, with a negated pid for a whole group.
        unsafe { libc::kill(-(pgid as libc::pid_t), 0) == 0 }
    }

    fn sigkill(pid: u32) {
        // SAFETY: SIGKILL to a pid this test started.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    }

    #[derive(Default)]
    struct Round {
        acquired: bool,
        acquire_us: u128,
        writer_alive: bool,
        bytes_after_lock: u64,
        group_alive: bool,
        held_back: u32,
        /// When the lock only came free after the child was killed.
        released_after_child_ms: Option<u128>,
        had_to_kill: bool,
    }

    impl Round {
        fn window(&self) -> bool {
            self.writer_alive || self.bytes_after_lock > 0
        }
    }

    struct Killed {
        /// The writer: the old task still using the resource.
        pid: u32,
        /// The executor's process group — what the holder record would carry.
        group: u32,
        lock: PathBuf,
        data: PathBuf,
        dir: PathBuf,
    }

    /// Starts the executor in `mode` and `SIGKILL`s it once its child is writing.
    ///
    /// The executor makes itself a process-group leader, so its whole tree (the writer
    /// included, unless the writer leaves with `setsid`) is one group — the same arrangement
    /// `spawn_tree` gives the executor.
    fn start_and_kill(round: u32, mode: &str) -> Killed {
        use std::io::BufRead;

        let dir = round_dir(2_000 + round);
        let lock = dir.join("build.lock");
        let data = dir.join("data.txt");
        let mut child = Command::new(HELPER)
            .arg("executor")
            .arg(&lock)
            .arg(&data)
            .arg("group")
            .arg(mode)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start the executor");
        let group = child.id();
        let mut ready = String::new();
        std::io::BufReader::new(child.stdout.take().expect("piped stdout"))
            .read_line(&mut ready)
            .expect("the executor's ready line");
        let pid = writer_pid(&ready);
        wait_until("the writer's first line", || length(&data) > 0);
        child.kill().expect("SIGKILL the executor");
        child.wait().expect("reap the executor");
        Killed {
            pid,
            group,
            lock,
            data,
            dir,
        }
    }

    /// §5 items 1 and 3; `mode` is `job`, `inherit`, `close` or `setsid`.
    fn round(round: u32, mode: &str, predecessor_check: bool) -> Round {
        let mut seen = Round::default();
        let killed = start_and_kill(round, mode);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&killed.lock)
            .expect("open the lock file");

        let started = Instant::now();
        let deadline = started + Duration::from_secs(10);
        let mut locked = false;
        while Instant::now() < deadline {
            if flock_try(&file) {
                if predecessor_check && group_alive(killed.group) {
                    seen.held_back += 1;
                    flock_unlock(&file);
                    std::thread::yield_now();
                    continue;
                }
                locked = true;
                break;
            }
            std::thread::yield_now();
        }
        seen.acquire_us = started.elapsed().as_micros();
        seen.acquired = locked;

        if locked {
            seen.writer_alive = alive(killed.pid);
            seen.group_alive = group_alive(killed.group);
            let before = length(&killed.data);
            // The writer appends continuously; 30 ms is long enough to see a running one.
            std::thread::sleep(Duration::from_millis(30));
            seen.bytes_after_lock = length(&killed.data).saturating_sub(before);
            flock_unlock(&file);
        } else {
            // The lock never came free: whatever the child's fd is doing keeps it held.
            // Kill the child, and record how long the lock took to go then.
            seen.writer_alive = alive(killed.pid);
            let killed_at = Instant::now();
            sigkill(killed.pid);
            let deadline = killed_at + Duration::from_secs(5);
            while Instant::now() < deadline {
                if flock_try(&file) {
                    seen.released_after_child_ms = Some(killed_at.elapsed().as_millis());
                    flock_unlock(&file);
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        drop(file);

        let deadline = Instant::now() + Duration::from_secs(2);
        while alive(killed.pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if alive(killed.pid) {
            seen.had_to_kill = true;
            sigkill(killed.pid);
        }
        let _ = std::fs::remove_dir_all(&killed.dir);
        seen
    }

    fn summarize(label: &str, seen: &[Round]) -> String {
        let count = |what: fn(&Round) -> bool| seen.iter().filter(|round| what(round)).count();
        let summary = format!(
            "{label}: {} rounds | lock taken {} | window {} (writer alive {}, wrote after the \
             lock {}) | group still had processes when locked {} | held back {} | best \
             kill→lock {} µs | lock released {} ms after the child was killed | child left \
             behind {}",
            seen.len(),
            count(|round| round.acquired),
            count(Round::window),
            count(|round| round.writer_alive),
            count(|round| round.bytes_after_lock > 0),
            count(|round| round.group_alive),
            seen.iter().map(|round| round.held_back).sum::<u32>(),
            seen.iter().map(|round| round.acquire_us).min().unwrap_or(0),
            seen.iter()
                .filter_map(|round| round.released_after_child_ms)
                .min()
                .map_or("-".to_owned(), |ms| ms.to_string()),
            count(|round| round.had_to_kill),
        );
        println!("{summary}");
        summary
    }

    fn run(mode: &str, predecessor_check: bool, count: u32) -> String {
        let _exclusive = super::exclusive();
        let seen: Vec<_> = (0..count)
            .map(|round| self::round(round, mode, predecessor_check))
            .collect();
        let summary = summarize(mode, &seen);
        // Only the plain case must end with the lock free: when the child inherits the fd
        // (`inherit`, `close`) or leaves the group (`setsid`), "still held" *is* the answer.
        if mode == "job" {
            assert!(
                seen.iter().all(|round| round.acquired),
                "the lock was never released after SIGKILL; {summary}"
            );
        }
        summary
    }

    #[test]
    fn sigkill_releases_the_flock() {
        run("job", false, rounds(5));
    }

    #[test]
    #[ignore = "100 rounds; for the numbers in docs/tasks/LOCKS.md §5"]
    fn sigkill_releases_the_flock_100() {
        run("job", false, rounds(100));
    }

    #[test]
    fn inherited_fd_keeps_the_flock() {
        run("inherit", false, rounds(3));
    }

    #[test]
    fn child_closing_the_inherited_fd_drops_the_lock() {
        run("close", false, rounds(3));
    }

    #[test]
    fn setsid_writer_outlives_the_group() {
        run("setsid", false, rounds(3));
    }

    #[test]
    fn predecessor_group_check_closes_the_window() {
        run("job", true, rounds(5));
    }
}
