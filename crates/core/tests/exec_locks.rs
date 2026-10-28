//! L1: the lock layer of the executor (`docs/tasks/LOCKS.md` §3, §4.1, §4.2, §4.4, §7).
//!
//! Every test name starts with `exec_`, so `cargo test -p crossagents-core exec` runs them.
//!
//! These tests talk to the library the way `exec-run` and the hub will: take locks, read the
//! holder records, clean up. They cover §7 items 2, 5 (the lock layer of it), 6, 7 and 9,
//! shared-plus-shared and shared-versus-exclusive, records and cleanup, and the lock file
//! deleted and re-created under a request (§4.1). Item 9 also runs a second process
//! (`tests/support/exec_lock_helper.rs`), because a lock that only excludes tasks would be no
//! lock at all.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crossagents_core::exec::{
    Access, EXIT_TEMPFAIL, ExecError, Holder, LockFile, Request, Resource, acquire, cleanup,
    holders, holders_dir, lock_name, lock_path,
};
use tempfile::TempDir;

fn temp() -> TempDir {
    tempfile::tempdir().expect("a temporary directory")
}

/// A resource directory that exists, the way a build directory does.
fn resource(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).expect("create the resource directory");
    path
}

fn request(
    command: &str,
    resources: &[(&str, &Path, Access)],
    timeout: Duration,
    notice: Duration,
) -> Request {
    Request {
        resources: resources
            .iter()
            .map(|(name, path, access)| Resource::new(*name, *path, *access))
            .collect(),
        command: command.to_owned(),
        agent: Some("Kai".to_owned()),
        run: Some("27bd5f0e".to_owned()),
        timeout,
        notice_every: notice,
    }
}

fn millis(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

// --- §7 item 5: the second runner waits, sees who it waits for, then gets its turn ---------

#[tokio::test]
async fn exec_waits_reports_the_holder_and_then_takes_over() {
    let dir = temp();
    let locks = dir.path().join("locks");
    let build = resource(dir.path(), "build");
    let first = acquire(
        &locks,
        &request(
            "cargo test --workspace",
            &[("build", build.as_path(), Access::Exclusive)],
            millis(5_000),
            millis(1_000),
        ),
        |_| {},
    )
    .await
    .expect("the first acquire");

    // While it is held, its record is there and names who holds it.
    let seen = holders(&locks, &build);
    assert_eq!(seen.len(), 1, "one holder, one record");
    assert_eq!(seen[0].exec_id, first.exec_id());
    assert_eq!(seen[0].agent.as_deref(), Some("Kai"));
    assert_eq!(seen[0].run.as_deref(), Some("27bd5f0e"));
    assert_eq!(seen[0].command, "cargo test --workspace");
    assert_eq!(seen[0].access, Access::Exclusive);

    // The second runner waits and reports. The records are all it has to go on.
    let notices = Arc::new(Mutex::new(Vec::new()));
    let waiter = {
        let locks = locks.clone();
        let build = build.clone();
        let notices = Arc::clone(&notices);
        tokio::spawn(async move {
            let report = request(
                "cargo build",
                &[("build", build.as_path(), Access::Exclusive)],
                Duration::from_secs(15),
                millis(100),
            );
            acquire(&locks, &report, move |line| {
                notices.lock().expect("the notices").push(line.to_owned());
            })
            .await
        })
    };

    tokio::time::sleep(millis(300)).await;
    assert!(
        !waiter.is_finished(),
        "the second acquire took a lock that is held"
    );
    assert_eq!(
        holders(&locks, &build).len(),
        1,
        "a waiting runner writes no record"
    );
    let reported = notices.lock().expect("the notices").clone();
    assert!(
        !reported.is_empty(),
        "the waiter did not report who it was waiting for"
    );
    let line = &reported[0];
    assert!(line.starts_with("waiting for build ("), "{line}");
    assert!(line.contains("held by Kai"), "{line}");
    assert!(line.contains("cargo test --workspace"), "{line}");
    assert!(line.contains("since "), "{line}");

    drop(first);
    let second = tokio::time::timeout(Duration::from_secs(10), waiter)
        .await
        .expect("the waiter finished")
        .expect("the waiter did not panic")
        .expect("the second acquire");
    assert_eq!(second.exec_id().len(), 32);
    let after = holders(&locks, &build);
    assert_eq!(
        after.len(),
        1,
        "the first record is gone, the second is there"
    );
    assert_eq!(after[0].exec_id, second.exec_id());
}

#[tokio::test]
async fn exec_time_out_names_the_holder_and_is_tempfail() {
    let dir = temp();
    let locks = dir.path().join("locks");
    let build = resource(dir.path(), "build");
    let held = acquire(
        &locks,
        &request(
            "cargo test --workspace",
            &[("build", build.as_path(), Access::Exclusive)],
            Duration::from_secs(5),
            millis(100),
        ),
        |_| {},
    )
    .await
    .expect("the first acquire");

    let started = Instant::now();
    let error = acquire(
        &locks,
        &request(
            "cargo build",
            &[("build", build.as_path(), Access::Exclusive)],
            millis(300),
            millis(100),
        ),
        |_| {},
    )
    .await
    .expect_err("the lock is held, the second acquire cannot succeed");
    assert!(
        started.elapsed() >= millis(300),
        "it gave up after {:?}",
        started.elapsed()
    );

    match &error {
        ExecError::Timeout {
            resource, holder, ..
        } => {
            assert_eq!(resource, "build");
            assert!(holder.contains("held by Kai"), "{holder}");
            assert!(holder.contains("cargo test --workspace"), "{holder}");
        }
        other => panic!("expected a timeout, got {other:?}"),
    }
    let message = error.to_string();
    assert!(message.contains("timed out"), "{message}");
    assert!(message.contains("held by Kai"), "{message}");
    assert!(message.contains("since "), "{message}");
    assert_eq!(EXIT_TEMPFAIL, 75, "the CLI's exit code for this");

    drop(held);
}

// --- §7 item 2: the resource directory disappears, the lock does not -----------------------

#[tokio::test]
async fn exec_lock_survives_deleting_the_resource_directory() {
    let dir = temp();
    let locks = dir.path().join("locks");
    let build = resource(dir.path(), "build");
    std::fs::write(build.join("artifact.bin"), b"something").expect("put something in it");
    let held = acquire(
        &locks,
        &request(
            "cargo build",
            &[("build", build.as_path(), Access::Exclusive)],
            Duration::from_secs(5),
            millis(100),
        ),
        |_| {},
    )
    .await
    .expect("the first acquire");

    // `cargo clean` or a script removing the build directory: the lock file is elsewhere, so
    // a newcomer must not end up locking a new file of the same name.
    std::fs::remove_dir_all(&build).expect("delete the whole build directory");
    assert!(!build.exists());

    let error = acquire(
        &locks,
        &request(
            "cargo build",
            &[("build", build.as_path(), Access::Exclusive)],
            millis(300),
            millis(100),
        ),
        |_| {},
    )
    .await
    .expect_err("the lock is still held, the directory being gone changes nothing");
    assert!(matches!(error, ExecError::Timeout { .. }), "{error}");

    drop(held);
    acquire(
        &locks,
        &request(
            "cargo build",
            &[("build", build.as_path(), Access::Exclusive)],
            Duration::from_secs(5),
            millis(100),
        ),
        |_| {},
    )
    .await
    .expect("after the release the lock comes free, directory or not");
}

// --- §7 item 6: two worktrees, two build directories, no waiting ---------------------------

#[tokio::test]
async fn exec_different_directories_do_not_block_each_other() {
    let dir = temp();
    let locks = dir.path().join("locks");
    let one = resource(dir.path(), "worktree-one");
    let two = resource(dir.path(), "worktree-two");
    let first = acquire(
        &locks,
        &request(
            "cargo test",
            &[("build", one.as_path(), Access::Exclusive)],
            Duration::from_secs(5),
            millis(100),
        ),
        |_| {},
    )
    .await
    .expect("the first acquire");

    let started = Instant::now();
    let second = acquire(
        &locks,
        &request(
            "cargo test",
            &[("build", two.as_path(), Access::Exclusive)],
            Duration::from_secs(5),
            millis(100),
        ),
        |_| {},
    )
    .await
    .expect("another directory is another resource");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the other worktree waited {:?}",
        started.elapsed()
    );
    assert_ne!(
        lock_name(&one),
        lock_name(&two),
        "different directories, different locks"
    );
    drop((first, second));
}

// --- §7 item 7: x and y in opposite orders, no deadlock -----------------------------------

/// One contender: takes both resources exclusively, in the order given, `rounds` times.
fn contender(
    locks: PathBuf,
    first: PathBuf,
    second: PathBuf,
    rounds: u32,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        for _ in 0..rounds {
            let report = Request {
                resources: vec![
                    Resource::new("first", first.clone(), Access::Exclusive),
                    Resource::new("second", second.clone(), Access::Exclusive),
                ],
                command: "cargo test".to_owned(),
                agent: Some("Kai".to_owned()),
                run: None,
                timeout: Duration::from_secs(30),
                notice_every: Duration::from_secs(1),
            };
            let held = acquire(&locks, &report, |_| {}).await.expect("acquire");
            tokio::time::sleep(millis(5)).await;
            drop(held);
        }
    })
}

#[tokio::test]
async fn exec_multi_resource_requests_never_deadlock() {
    let dir = temp();
    let locks = dir.path().join("locks");
    let x = resource(dir.path(), "x");
    let y = resource(dir.path(), "y");
    let rounds = 10;

    let one = contender(locks.clone(), x.clone(), y.clone(), rounds);
    let other = contender(locks.clone(), y.clone(), x.clone(), rounds);
    let finished = tokio::time::timeout(Duration::from_secs(60), async {
        one.await.expect("the x-then-y contender did not panic");
        other.await.expect("the y-then-x contender did not panic");
    })
    .await;
    assert!(
        finished.is_ok(),
        "the two orders took each other's first lock and waited for the second: deadlock"
    );
}

#[tokio::test]
async fn exec_takes_the_stronger_access_when_a_path_is_listed_twice() {
    let dir = temp();
    let locks = dir.path().join("locks");
    let build = resource(dir.path(), "build");
    let held = acquire(
        &locks,
        &request(
            "cargo build",
            &[
                ("build", build.as_path(), Access::Shared),
                ("build", build.as_path(), Access::Exclusive),
            ],
            Duration::from_secs(5),
            millis(100),
        ),
        |_| {},
    )
    .await
    .expect("the acquire");
    let taken = held.locks();
    assert_eq!(taken.len(), 1, "one path, one lock");
    assert_eq!(taken[0].access, Access::Exclusive);

    let error = acquire(
        &locks,
        &request(
            "cargo build",
            &[("build", build.as_path(), Access::Shared)],
            millis(200),
            millis(100),
        ),
        |_| {},
    )
    .await
    .expect_err("a shared request still waits for the exclusive one");
    assert!(matches!(error, ExecError::Timeout { .. }), "{error}");
}

// --- shared and exclusive -----------------------------------------------------------------

#[tokio::test]
async fn exec_shared_locks_share_and_exclusive_excludes() {
    let dir = temp();
    let locks = dir.path().join("locks");
    let build = resource(dir.path(), "build");
    let shared = |timeout: Duration| {
        request(
            "cargo check",
            &[("build", build.as_path(), Access::Shared)],
            timeout,
            millis(100),
        )
    };
    let exclusive = |timeout: Duration| {
        request(
            "cargo test",
            &[("build", build.as_path(), Access::Exclusive)],
            timeout,
            millis(100),
        )
    };

    let first = acquire(&locks, &shared(Duration::from_secs(5)), |_| {})
        .await
        .expect("a shared lock");
    let second = acquire(&locks, &shared(millis(500)), |_| {})
        .await
        .expect("a second shared lock runs alongside the first");
    assert_eq!(holders(&locks, &build).len(), 2, "both write a record");

    let error = acquire(&locks, &exclusive(millis(200)), |_| {})
        .await
        .expect_err("exclusive must wait for shared holders");
    assert!(matches!(error, ExecError::Timeout { .. }), "{error}");

    drop((first, second));
    let held = acquire(&locks, &exclusive(Duration::from_secs(5)), |_| {})
        .await
        .expect("exclusive once the shared holders are gone");
    let error = acquire(&locks, &shared(millis(200)), |_| {})
        .await
        .expect_err("shared must wait for an exclusive holder");
    assert!(matches!(error, ExecError::Timeout { .. }), "{error}");

    drop(held);
    acquire(&locks, &shared(Duration::from_secs(5)), |_| {})
        .await
        .expect("shared once the exclusive holder is gone");
}

// --- §7 item 9: no hub, and a real second process ------------------------------------------

/// Kills the helper even when an assertion fails first.
struct Helper(Child);

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn exec_excludes_across_processes_without_a_hub() {
    let dir = temp();
    let locks = dir.path().join("locks");
    let build = resource(dir.path(), "build");

    let mut child = Command::new(env!("CARGO_BIN_EXE_exec_lock_helper"))
        .arg(&locks)
        .arg(&build)
        .arg("exclusive")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start the helper process");
    let mut ready = String::new();
    BufReader::new(child.stdout.take().expect("the helper's stdout"))
        .read_line(&mut ready)
        .expect("the helper's ready line");
    assert!(ready.starts_with("ready "), "{ready:?}");
    let mut helper = Helper(child);

    // Nothing but files is involved: no hub, no daemon, no socket.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let seen = holders(&locks, &build);
    assert_eq!(seen.len(), 1, "the other process's record is visible");
    assert_eq!(seen[0].agent.as_deref(), Some("helper"));

    let started = Instant::now();
    let error = runtime
        .block_on(acquire(
            &locks,
            &request(
                "cargo build",
                &[("build", build.as_path(), Access::Exclusive)],
                millis(500),
                millis(100),
            ),
            |_| {},
        ))
        .expect_err("the other process holds the lock");
    assert!(matches!(error, ExecError::Timeout { .. }), "{error}");
    assert!(
        started.elapsed() >= millis(500),
        "it did not even wait: {:?}",
        started.elapsed()
    );

    // Closing the helper's stdin ends it, which releases its lock.
    drop(helper.0.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(10);
    while helper.0.try_wait().expect("ask the helper").is_none() {
        assert!(Instant::now() < deadline, "the helper did not exit");
        std::thread::sleep(millis(10));
    }

    runtime
        .block_on(acquire(
            &locks,
            &request(
                "cargo build",
                &[("build", build.as_path(), Access::Exclusive)],
                Duration::from_secs(5),
                millis(100),
            ),
            |_| {},
        ))
        .expect("the lock is free once the other process is gone");
}

// --- identity ----------------------------------------------------------------------------

#[test]
fn exec_lock_names_follow_the_normalized_path() {
    let dir = temp();
    let build = resource(dir.path(), "build");
    let plain = lock_name(&build);
    assert_eq!(plain.len(), 32);
    assert_eq!(plain, lock_name(&build.join(".")));
    assert_eq!(
        plain,
        lock_name(&build.join("missing").join("..")),
        "a path that does not exist is still the same resource"
    );
    assert_eq!(
        lock_path(&dir.path().join("locks"), &build).file_name(),
        Some(std::ffi::OsStr::new(&format!("{plain}.lock")))
    );

    #[cfg(windows)]
    {
        assert_eq!(
            plain,
            lock_name(&dir.path().join("BUILD")),
            "Windows has one case"
        );
        assert_eq!(
            plain,
            lock_name(Path::new(&build.to_string_lossy().replace('\\', "/")))
        );
    }
}

// --- records and cleanup -----------------------------------------------------------------

fn record(exec_id: &str, build: &Path, root_pid: Option<u32>) -> Holder {
    Holder {
        exec_id: exec_id.to_owned(),
        resource: "build".to_owned(),
        path: build.to_path_buf(),
        access: Access::Exclusive,
        command: "cargo test".to_owned(),
        agent: Some("Kai".to_owned()),
        run: None,
        root_pid,
        root_created: root_pid.map(|_| 1),
        tree: None,
        started_at: chrono::Utc::now(),
    }
}

#[test]
fn exec_holder_records_of_gone_processes_are_not_shown() {
    let dir = temp();
    let locks = dir.path().join("locks");
    let build = resource(dir.path(), "build");
    let records = holders_dir(&locks, &build);
    std::fs::create_dir_all(&records).expect("create the holders directory");

    // PID 4 with a creation time of 1: no process is that, whatever PID 4 is now.
    record("dead", &build, Some(4))
        .write(&records.join("dead.json"))
        .expect("write the dead record");
    record("fresh", &build, None)
        .write(&records.join("fresh.json"))
        .expect("write the fresh record");

    let seen = holders(&locks, &build);
    assert_eq!(seen.len(), 1, "the dead record must not be shown");
    assert_eq!(seen[0].exec_id, "fresh");
}

#[tokio::test]
async fn exec_cleanup_removes_idle_locks_and_dead_records() {
    let dir = temp();
    let locks = dir.path().join("locks");
    let busy = resource(dir.path(), "busy");
    let idle = resource(dir.path(), "idle");
    let held = acquire(
        &locks,
        &request(
            "cargo test",
            &[("build", busy.as_path(), Access::Exclusive)],
            Duration::from_secs(5),
            millis(100),
        ),
        |_| {},
    )
    .await
    .expect("hold one lock");
    drop(
        acquire(
            &locks,
            &request(
                "cargo test",
                &[("build", idle.as_path(), Access::Exclusive)],
                Duration::from_secs(5),
                millis(100),
            ),
            |_| {},
        )
        .await
        .expect("take and release the other"),
    );

    // Both lock files look abandoned; only one of them is.
    let long_ago = SystemTime::now() - Duration::from_secs(7_200);
    for path in [lock_path(&locks, &busy), lock_path(&locks, &idle)] {
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open the lock file")
            .set_modified(long_ago)
            .expect("age the lock file");
    }
    let dead = holders_dir(&locks, &busy).join("dead.json");
    record("dead", &busy, Some(4))
        .write(&dead)
        .expect("write a dead record");

    let done = cleanup(&locks, Duration::from_secs(3_600)).expect("cleanup");
    assert_eq!(done.locks, 1, "only the idle lock file could be removed");
    assert_eq!(done.records, 1, "only the dead record could be removed");
    assert!(
        lock_path(&locks, &busy).exists(),
        "a held lock file is never removed"
    );
    assert!(!lock_path(&locks, &idle).exists(), "the idle lock is gone");
    assert!(!dead.exists(), "the dead record is gone");
    assert_eq!(
        holders(&locks, &busy).len(),
        1,
        "the live record of the held lock is untouched"
    );

    // Once nothing holds it, the old lock file goes too.
    drop(held);
    let done = cleanup(&locks, Duration::from_secs(3_600)).expect("cleanup again");
    assert_eq!(done.locks, 1);
    assert!(!lock_path(&locks, &busy).exists());
    assert!(
        !holders_dir(&locks, &busy).exists(),
        "its records went with it"
    );
}

// --- §4.1: a deleted-and-recreated lock file is not a lock --------------------------------

#[tokio::test]
async fn exec_a_replaced_lock_file_does_not_count_as_held() {
    let dir = temp();
    let locks = dir.path().join("locks");
    let build = resource(dir.path(), "build");
    std::fs::create_dir_all(&locks).expect("create the locks directory");
    let file = lock_path(&locks, &build);

    // What `acquire` does first, minus the lock: open the file.
    let opened = LockFile::open(&file).expect("open the lock file");
    assert!(opened.still_at(&file), "it is the file the path leads to");

    // Cleanup runs in the gap between the open and the lock and deletes the file.
    let done = cleanup(&locks, Duration::ZERO).expect("cleanup");
    assert_eq!(done.locks, 1, "the lock file was removable");
    assert!(!file.exists(), "the lock file is gone");

    // The old handle still locks what it holds — and that is exactly why it must not count:
    // the path no longer leads there, so a newcomer would create and lock another file.
    assert!(
        opened
            .try_lock(Access::Exclusive)
            .expect("lock the old file"),
        "the deleted file can be locked; the check is what makes this harmless"
    );
    assert!(
        !opened.still_at(&file),
        "a lock on a file that is not at the path is not a lock on the resource"
    );

    // A request made now takes a lock on a new file, as it should, and gets it.
    let held = acquire(
        &locks,
        &request(
            "cargo test",
            &[("build", build.as_path(), Access::Exclusive)],
            Duration::from_secs(5),
            millis(100),
        ),
        |_| {},
    )
    .await
    .expect("the resource is free: the stale lock belongs to nobody");
    assert!(file.exists(), "acquire created a lock file of its own");
    assert!(!opened.still_at(&file), "and it is not the one we opened");
    assert_eq!(held.locks().len(), 1);
}

// --- §4.1 under pressure: cleanup running while two execs take the same lock --------------

#[test]
fn exec_cleanup_never_lets_two_holders_in() {
    let dir = temp();
    let locks = dir.path().join("locks");
    let build = resource(dir.path(), "build");
    std::fs::create_dir_all(&locks).expect("create the locks directory");

    let holding = Arc::new(AtomicUsize::new(0));
    let most_at_once = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    // The cleaner deletes every lock file it can while the two execs take the same lock in a
    // loop. What is asserted here is the invariant — never two holders at once — and that the
    // churn never turns into an error. The microsecond-wide window in which a lock file is
    // replaced between the open and the lock is not reachable from a test here; it is covered
    // exactly by `exec_a_replaced_lock_file_does_not_count_as_held`.
    let cleaner = std::thread::spawn({
        let locks = locks.clone();
        let stop = Arc::clone(&stop);
        move || {
            while !stop.load(Ordering::Relaxed) {
                let _ = cleanup(&locks, Duration::ZERO);
                std::thread::sleep(Duration::from_micros(100));
            }
        }
    });

    let holders: Vec<_> = (0..2)
        .map(|_| {
            let locks = locks.clone();
            let build = build.clone();
            let holding = Arc::clone(&holding);
            let most_at_once = Arc::clone(&most_at_once);
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("a runtime");
                let report = request(
                    "cargo test",
                    &[("build", build.as_path(), Access::Exclusive)],
                    Duration::from_secs(60),
                    Duration::from_secs(1),
                );
                for _ in 0..150 {
                    let held = runtime
                        .block_on(acquire(&locks, &report, |_| {}))
                        .expect("acquire");
                    let now = holding.fetch_add(1, Ordering::SeqCst) + 1;
                    most_at_once.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(millis(2));
                    holding.fetch_sub(1, Ordering::SeqCst);
                    drop(held);
                }
            })
        })
        .collect();

    for holder in holders {
        holder.join().expect("a holder thread panicked");
    }
    stop.store(true, Ordering::Relaxed);
    cleaner.join().expect("the cleaner thread panicked");

    assert_eq!(
        most_at_once.load(Ordering::SeqCst),
        1,
        "two execs held the same resource at the same time"
    );
}
