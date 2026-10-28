//! L1b: the executor's process tree, predecessor check and nesting
//! (`docs/tasks/LOCKS.md` §4.3, §4.4, §4.5, §5 conclusion 3, §7 items 3, 4, 8).
//!
//! Every test name starts with `exec_`, so `cargo test -p crossagents-core exec` runs them.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crossagents_core::exec::{
    Access, ExecError, Holder, LockFile, Request, Resource, acquire, holders_dir, inherited_holds,
    lock_name, lock_path, process_created, spawn, wait_tree_empty,
};
use tempfile::TempDir;

fn temp() -> TempDir {
    tempfile::tempdir().expect("a temporary directory")
}

fn resource(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).expect("create the resource directory");
    path
}

fn request(command: &str, path: &Path, access: Access, timeout: Duration) -> Request {
    Request {
        resources: vec![Resource::new("build", path, access)],
        command: command.to_owned(),
        agent: Some("Kai".to_owned()),
        run: Some("27bd5f0e".to_owned()),
        timeout,
        notice_every: Duration::from_millis(100),
    }
}

fn millis(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

// --- §4.5 and §7 item 8: nesting needs a real handle, a real file, a live outer exec -------

#[tokio::test]
async fn exec_nested_claims_are_checked_before_the_locks_are_skipped() {
    let dir = temp();
    let locks = dir.path().join("locks");
    let build = resource(dir.path(), "build");
    let held = acquire(
        &locks,
        &request("cargo test", &build, Access::Shared, millis(5_000)),
        |_| {},
    )
    .await
    .expect("take a shared lock");
    let lock = held.locks().into_iter().next().expect("one lock");
    let value = format!("{};{}:shared:{}", held.exec_id(), lock.name, lock.raw);

    let shared = vec![Resource::new("build", build.clone(), Access::Shared)];
    assert!(
        inherited_holds(&locks, &value, &shared)
            .expect("a valid claim is not an error")
            .is_some(),
        "a valid inherited handle covers the same resource at the same access"
    );

    // §7 item 3: shared -> exclusive is an upgrade, and it must fail at once, not wait.
    let exclusive = vec![Resource::new("build", build.clone(), Access::Exclusive)];
    let error = inherited_holds(&locks, &value, &exclusive)
        .expect_err("an upgrade inside an exec is an error");
    assert!(matches!(error, ExecError::Nesting(_)), "{error}");
    assert!(error.to_string().contains("upgraded"), "{error}");
    assert!(
        error.to_string().contains("declare it exclusive"),
        "the message has to say how to fix it: {error}"
    );

    // A resource the outer exec never held: also an error, not a new lock.
    let other = resource(dir.path(), "other");
    let missing = vec![Resource::new("other", other.clone(), Access::Shared)];
    let error = inherited_holds(&locks, &value, &missing).expect_err("not held by the outer");
    assert!(matches!(error, ExecError::Nesting(_)), "{error}");

    // §7 item 8: a handle that does not exist is not a hold.
    let bogus = format!("{};{}:shared:999999999", held.exec_id(), lock.name);
    assert!(
        inherited_holds(&locks, &bogus, &shared)
            .expect("a bad handle is not an error")
            .is_none(),
        "an invalid handle must fall back to taking the locks"
    );

    // §7 item 8: a real handle to *another* lock file is not this resource.
    let other_held = acquire(
        &locks,
        &request("cargo check", &other, Access::Shared, millis(5_000)),
        |_| {},
    )
    .await
    .expect("take the other lock");
    let other_lock = other_held.locks().into_iter().next().expect("one lock");
    let mismatched = format!("{};{}:shared:{}", held.exec_id(), lock.name, other_lock.raw);
    assert!(
        inherited_holds(&locks, &mismatched, &shared)
            .expect("a mismatched file is not an error")
            .is_none(),
        "a handle to another file must not count as this lock"
    );
}

#[tokio::test]
async fn exec_nested_claim_needs_the_outer_process_to_be_alive() {
    let dir = temp();
    let locks = dir.path().join("locks");
    let build = resource(dir.path(), "build");
    let held = acquire(
        &locks,
        &request("cargo test", &build, Access::Exclusive, millis(5_000)),
        |_| {},
    )
    .await
    .expect("take the lock");
    let lock = held.locks().into_iter().next().expect("one lock");
    let value = format!("{};{}:exclusive:{}", held.exec_id(), lock.name, lock.raw);
    let resources = vec![Resource::new("build", build.clone(), Access::Exclusive)];

    // Replace the record with one that names a process that is gone: PID 4 with a creation
    // time of 1 is nobody, whatever PID 4 is on this machine now.
    let record = holders_dir(&locks, &build).join(format!("{}.json", held.exec_id()));
    let mut holder = Holder::read(&record).expect("read the record");
    holder.root_pid = Some(4);
    holder.root_created = Some(1);
    holder.write(&record).expect("write the record back");

    assert!(
        inherited_holds(&locks, &value, &resources)
            .expect("a dead outer process is not an error")
            .is_none(),
        "a record whose root process is gone must fall back to taking the locks"
    );
}

// --- §4.5: a stale CROSSAGENTS_HELD value is not an outer hold -----------------------------

/// A `CROSSAGENTS_HELD` value for `path` whose holder record names a root process that is
/// gone (PID 4 with creation time 1) — what an outer exec leaves in a long-lived shell after
/// it exits. The open handle is returned so it stays valid while the value is checked: the
/// handle is real, the outer process is not.
fn stale_held(locks: &Path, path: &Path, exec_id: &str, access: Access) -> (LockFile, String) {
    std::fs::create_dir_all(locks).expect("create the locks directory");
    let file = LockFile::open(&lock_path(locks, path)).expect("open the lock file");
    let record = holders_dir(locks, path).join(format!("{exec_id}.json"));
    std::fs::create_dir_all(record.parent().expect("the holders directory"))
        .expect("create the holders directory");
    Holder {
        exec_id: exec_id.to_owned(),
        resource: "build".to_owned(),
        path: path.to_path_buf(),
        access,
        command: "cargo test".to_owned(),
        agent: Some("Kai".to_owned()),
        run: None,
        root_pid: Some(4),
        root_created: Some(1),
        tree: None,
        started_at: chrono::Utc::now(),
    }
    .write(&record)
    .expect("write the record");
    let value = format!(
        "{exec_id};{}.lock:{}:{}",
        lock_name(path),
        access.as_str(),
        file.raw()
    );
    (file, value)
}

#[tokio::test]
async fn exec_stale_held_value_does_not_report_a_missing_resource() {
    let dir = temp();
    let locks = dir.path().join("locks");
    let declared = resource(dir.path(), "declared");
    let other = resource(dir.path(), "other");
    let (_file, value) = stale_held(&locks, &declared, "gone", Access::Shared);

    // An outer exec that is gone never declared `other` — but that is not a request mistake,
    // it means the environment value is worthless, so the locks are taken normally.
    let resources = vec![Resource::new("other", other.clone(), Access::Shared)];
    assert!(
        inherited_holds(&locks, &value, &resources)
            .expect("a stale value is not an error")
            .is_none(),
        "a dead outer exec must not turn a new resource into a Nesting error"
    );
    acquire(
        &locks,
        &request("cargo check", &other, Access::Shared, millis(5_000)),
        |_| {},
    )
    .await
    .expect("the request takes the locks like any other");
}

#[tokio::test]
async fn exec_stale_shared_value_does_not_block_an_exclusive_request() {
    let dir = temp();
    let locks = dir.path().join("locks");
    let build = resource(dir.path(), "build");
    let (_file, value) = stale_held(&locks, &build, "gone", Access::Shared);

    let resources = vec![Resource::new("build", build.clone(), Access::Exclusive)];
    assert!(
        inherited_holds(&locks, &value, &resources)
            .expect("a stale value is not an error")
            .is_none(),
        "a dead outer exec's shared claim cannot be an upgrade error"
    );
    acquire(
        &locks,
        &request("cargo test", &build, Access::Exclusive, millis(5_000)),
        |_| {},
    )
    .await
    .expect("the request takes the lock normally and runs");
}

// --- §5 conclusion 3: a predecessor's root process holds the lock until it is gone ----------

#[tokio::test]
async fn exec_waits_for_a_live_predecessor_root_process() {
    let dir = temp();
    let locks = dir.path().join("locks");
    let build = resource(dir.path(), "build");

    let mut child = Command::new(env!("CARGO_BIN_EXE_exec_sleeper"))
        .arg("1500")
        .spawn()
        .expect("start the predecessor");
    let pid = child.id();
    let record = holders_dir(&locks, &build).join("previous.json");
    std::fs::create_dir_all(record.parent().expect("the holders directory"))
        .expect("create the holders directory");
    Holder {
        exec_id: "previous".to_owned(),
        resource: "build".to_owned(),
        path: build.clone(),
        access: Access::Exclusive,
        command: "cargo test".to_owned(),
        agent: Some("Kai".to_owned()),
        run: None,
        root_pid: Some(pid),
        root_created: process_created(pid),
        tree: None,
        started_at: chrono::Utc::now(),
    }
    .write(&record)
    .expect("write the predecessor's record");

    let started = Instant::now();
    let held = acquire(
        &locks,
        &request(
            "cargo build",
            &build,
            Access::Exclusive,
            Duration::from_secs(8),
        ),
        |_| {},
    )
    .await
    .expect("the lock comes free once the predecessor is gone");
    assert!(
        started.elapsed() >= Duration::from_millis(1_000),
        "the lock was taken while the predecessor's root process was still running \
         (after {:?})",
        started.elapsed()
    );
    assert!(
        !record.exists(),
        "a record whose process is gone is deleted"
    );
    drop(held);
    child.wait().expect("the predecessor exits");
}

// --- §4.3 and §7 item 4: the lock lives until the whole tree ends ---------------------------

#[tokio::test]
async fn exec_holds_the_lock_until_the_tree_ends() {
    let dir = temp();
    let locks = dir.path().join("locks");
    let build = resource(dir.path(), "build");
    let mut held = acquire(
        &locks,
        &request("helper", &build, Access::Exclusive, millis(5_000)),
        |_| {},
    )
    .await
    .expect("take the lock");

    // The helper closes the inherited lock handle, leaves a grandchild behind and exits. If
    // the executor ever tied the lock to the inherited handle, or released it when the root
    // process exited, the next acquire would get in here.
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_exec_tree_helper"));
    cmd.arg("1200");
    let (mut child, tree) = spawn(&mut held, &mut cmd).expect("start the tree");
    let status = child.wait().await.expect("wait for the root");
    assert_eq!(
        status.code(),
        Some(0),
        "the helper must find CROSSAGENTS_HELD and close its handles"
    );

    // Root gone, grandchild still running: the lock must still be held.
    assert!(
        tree.active_processes().expect("ask the tree") >= 1,
        "the grandchild should still be in the tree"
    );
    let error = acquire(
        &locks,
        &request("cargo build", &build, Access::Exclusive, millis(400)),
        |_| {},
    )
    .await
    .expect_err("the tree is still running, the lock cannot be free");
    assert!(matches!(error, ExecError::Timeout { .. }), "{error}");

    wait_tree_empty(&tree).await.expect("the tree ends");
    assert_eq!(
        tree.active_processes().expect("ask the tree"),
        0,
        "the tree is empty now"
    );
    drop(held);

    acquire(
        &locks,
        &request("cargo build", &build, Access::Exclusive, millis(5_000)),
        |_| {},
    )
    .await
    .expect("after the tree is gone the lock comes free");
}

// --- §4.4: the record names the root process and its tree ----------------------------------

#[tokio::test]
async fn exec_records_the_root_process_and_its_tree() {
    let dir = temp();
    let locks = dir.path().join("locks");
    let build = resource(dir.path(), "build");
    let mut held = acquire(
        &locks,
        &request("sleeper", &build, Access::Exclusive, millis(5_000)),
        |_| {},
    )
    .await
    .expect("take the lock");

    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_exec_sleeper"));
    cmd.arg("200");
    let (mut child, tree) = spawn(&mut held, &mut cmd).expect("start the tree");
    let pid = child.id();
    let seen = crossagents_core::exec::holders(&locks, &build);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].root_pid, pid, "the record names the root process");
    assert_eq!(
        seen[0].root_created,
        process_created(pid.expect("a pid")),
        "and its creation time, so a reused PID is not mistaken for it"
    );
    assert!(seen[0].tree.is_some(), "and the job or process group");

    let status = child.wait().await.expect("wait for the command");
    assert_eq!(status.code(), Some(0));
    wait_tree_empty(&tree).await.expect("the tree ends");
}
