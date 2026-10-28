//! `crossagents exec` end to end: two runners, the timeout, nesting and a forged
//! `CROSSAGENTS_HELD` (`docs/tasks/LOCKS.md` §4, §7 items 3, 4 and 5).
//!
//! Every test name starts with `exec_`, so `cargo test -p crossagents-cli exec` runs them.
//! No hub is involved: the command has to work with the hub stopped.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::Command as AssertCommand;

const BIN: &str = env!("CARGO_BIN_EXE_crossagents");

struct Home(tempfile::TempDir);

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a temporary home");
        std::fs::write(dir.path().join("config.toml"), "[hub]\nport = 0\n").expect("write config");
        Self(dir)
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    /// The `build` directory of this home, created.
    fn build(&self) -> PathBuf {
        let path = self.path().join("build");
        std::fs::create_dir_all(&path).expect("create the build directory");
        path
    }

    fn exec(&self) -> AssertCommand {
        let mut command = AssertCommand::new(BIN);
        isolate_assert(&mut command, self.path());
        command
    }
}

/// Keeps a test from reaching the real home or an enclosing `exec`.
fn isolate(command: &mut Command, home: &Path) {
    command.env("CROSSAGENTS_HOME", home);
    command.env_remove("CROSSAGENTS_HUB_HOME");
    command.env_remove("CROSSAGENTS_HELD");
    command.env_remove("CROSSAGENTS_RESOURCES");
}

fn isolate_assert(command: &mut AssertCommand, home: &Path) {
    command.env("CROSSAGENTS_HOME", home);
    command.env_remove("CROSSAGENTS_HUB_HOME");
    command.env_remove("CROSSAGENTS_HELD");
    command.env_remove("CROSSAGENTS_RESOURCES");
}

/// A background `exec`, killed when the test ends.
struct Running(Child);

impl Running {
    fn spawn(home: &Path, args: &[String]) -> Self {
        let mut command = Command::new(BIN);
        isolate(&mut command, home);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Self(command.spawn().expect("start the first exec"))
    }

    fn wait(&mut self) -> std::process::ExitStatus {
        self.0.wait().expect("the first exec ends")
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn exec_args(resources: &[String], timeout: &str, command: &[String]) -> Vec<String> {
    let mut args = vec!["exec".to_owned()];
    for resource in resources {
        args.push("--resource".to_owned());
        args.push(resource.clone());
    }
    args.push("--timeout".to_owned());
    args.push(timeout.to_owned());
    args.push("--".to_owned());
    args.extend(command.iter().cloned());
    args
}

/// A command that uses the machine for a while without touching anything.
fn sleeper(seconds: u64) -> Vec<String> {
    #[cfg(windows)]
    {
        vec![
            "cmd".to_owned(),
            "/c".to_owned(),
            format!("ping -n {} 127.0.0.1 >NUL", seconds + 1),
        ]
    }
    #[cfg(not(windows))]
    {
        vec!["sh".to_owned(), "-c".to_owned(), format!("sleep {seconds}")]
    }
}

/// A command that ends with `code`.
fn exiting(code: i32) -> Vec<String> {
    #[cfg(windows)]
    {
        vec!["cmd".to_owned(), "/c".to_owned(), format!("exit {code}")]
    }
    #[cfg(not(windows))]
    {
        vec!["sh".to_owned(), "-c".to_owned(), format!("exit {code}")]
    }
}

/// Waits until a lock is taken *and* its record names a root process: the point at which the
/// second runner has something to wait for.
fn wait_until_held(home: &Path) {
    let locks = home.join("locks");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(entries) = std::fs::read_dir(&locks) {
            for entry in entries.flatten() {
                if !entry.file_name().to_string_lossy().ends_with(".holders") {
                    continue;
                }
                let Ok(records) = std::fs::read_dir(entry.path()) else {
                    continue;
                };
                for record in records.flatten() {
                    if let Ok(text) = std::fs::read_to_string(record.path())
                        && text.contains("\"root_pid\"")
                    {
                        return;
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "the first exec never took the lock in {}",
            locks.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The only lock file's name, `<hash>.lock`.
fn lock_file_name(home: &Path) -> String {
    let entries = std::fs::read_dir(home.join("locks")).expect("list the locks");
    entries
        .flatten()
        .find_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.ends_with(".lock").then_some(name)
        })
        .expect("a lock file")
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

// --- §7 item 5: the second runner waits, says so, and then runs -----------------------------

#[test]
fn exec_second_runner_waits_its_turn() {
    let home = Home::new();
    let build = home.build();
    let resource = format!("build={}", build.display());
    let mut first = Running::spawn(
        home.path(),
        &exec_args(std::slice::from_ref(&resource), "60s", &sleeper(4)),
    );
    wait_until_held(home.path());

    let started = Instant::now();
    let output = home
        .exec()
        .args(exec_args(&[resource], "60s", &exiting(0)))
        .assert()
        .success()
        .get_output()
        .clone();
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_secs(1),
        "the second exec ran while the first held the resource: {waited:?}"
    );
    let reported = stderr(&output);
    assert!(
        reported.contains("waiting for build"),
        "the second exec did not say what it was waiting for: {reported}"
    );
    assert!(
        first.wait().success(),
        "the first exec should have finished normally"
    );
}

// --- §7 item 5: a short timeout is 75, not the command's failure ----------------------------

#[test]
fn exec_timeout_exits_with_75() {
    let home = Home::new();
    let build = home.build();
    let resource = format!("build={}", build.display());
    let _first = Running::spawn(
        home.path(),
        &exec_args(std::slice::from_ref(&resource), "60s", &sleeper(6)),
    );
    wait_until_held(home.path());

    let started = Instant::now();
    let output = home
        .exec()
        .args(exec_args(&[resource], "400ms", &exiting(0)))
        .assert()
        .code(75)
        .get_output()
        .clone();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the timeout did not end the wait: {:?}",
        started.elapsed()
    );
    let reported = stderr(&output);
    assert!(reported.contains("timed out"), "{reported}");
    assert!(reported.contains("waiting for build"), "{reported}");
}

// --- §7 item 3: shared outside, exclusive inside: an error, not a wait ----------------------

#[test]
fn exec_nested_exclusive_after_shared_fails_at_once() {
    let home = Home::new();
    let build = home.build();
    let inner = exec_args(&[format!("build={}", build.display())], "10s", &sleeper(10));
    let mut nested = vec![BIN.to_owned()];
    nested.extend(inner);
    let outer = exec_args(
        &[format!("build={}:shared", build.display())],
        "60s",
        &nested,
    );

    let started = Instant::now();
    let output = home
        .exec()
        .args(outer)
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the inner exec waited instead of reporting the upgrade: {:?}",
        started.elapsed()
    );
    let reported = stderr(&output);
    assert!(
        reported.contains("cannot be upgraded"),
        "the stderr should explain the upgrade: {reported}"
    );
}

// --- §7 item 8: a forged CROSSAGENTS_HELD does not skip the lock -----------------------------

#[test]
fn exec_forged_held_environment_does_not_skip_locking() {
    let home = Home::new();
    let build = home.build();
    let resource = format!("build={}", build.display());
    let _first = Running::spawn(
        home.path(),
        &exec_args(std::slice::from_ref(&resource), "60s", &sleeper(6)),
    );
    wait_until_held(home.path());

    // The right lock name, an outer exec that does not exist, and a handle that is not one.
    let forged = format!("forged;{}:exclusive:999999999", lock_file_name(home.path()));
    let output = home
        .exec()
        .env("CROSSAGENTS_HELD", forged)
        .args(exec_args(&[resource], "400ms", &exiting(0)))
        .assert()
        .code(75)
        .get_output()
        .clone();
    let reported = stderr(&output);
    assert!(
        reported.contains("timed out"),
        "a forged claim must still wait for the real lock: {reported}"
    );
}

// --- resources: CROSSAGENTS_RESOURCES and --resource NAME=PATH -------------------------------

#[test]
fn exec_reads_resources_from_the_environment() {
    let home = Home::new();
    let build = home.build();
    let declared = serde_json::json!([{
        "name": "build",
        "path": build.display().to_string(),
        "access": "shared",
    }])
    .to_string();

    home.exec()
        .env("CROSSAGENTS_RESOURCES", declared)
        .args(exec_args(&["build".to_owned()], "10s", &exiting(0)))
        .assert()
        .success();
}

#[test]
fn exec_reports_a_resource_it_cannot_find() {
    let home = Home::new();
    let output = home
        .exec()
        .args(exec_args(&["nope".to_owned()], "10s", &exiting(0)))
        .assert()
        .failure()
        .get_output()
        .clone();
    let reported = stderr(&output);
    assert!(reported.contains("not declared"), "{reported}");
}

// --- the command's own exit code passes through ---------------------------------------------

#[test]
fn exec_passes_the_command_exit_code() {
    let home = Home::new();
    home.exec()
        .args(exec_args(&[], "10s", &exiting(7)))
        .assert()
        .code(7);
}
