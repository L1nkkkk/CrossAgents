//! Shim mode end to end (`docs/tasks/LOCKS.md` §4.6, §7 L3): a copy of this executable named
//! `cargo` finds the real program past its own directory, and holds the run's `build` like
//! `crossagents exec --resource build -- cargo …`.
//!
//! Every test name starts with `shim_`, so `cargo test -p crossagents-cli shim` runs them.
//! No hub is involved.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_crossagents");

/// The exit code the fake `cargo` ends with, so that passing it through is visible.
const FAKE_EXIT: i32 = 5;

struct Setup {
    home: tempfile::TempDir,
    /// The shim: a copy of this executable under the name `cargo`.
    shims: PathBuf,
    /// Where the real `cargo` is: a fake program that prints its arguments and exits
    /// [`FAKE_EXIT`].
    real: PathBuf,
}

impl Setup {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("a temporary home");
        let shims = home.path().join("shims");
        let real = home.path().join("real");
        std::fs::create_dir_all(&shims).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        let shim = if cfg!(windows) {
            shims.join("cargo.exe")
        } else {
            shims.join("cargo")
        };
        std::fs::copy(BIN, &shim).expect("copy the executable as a shim");
        fake_cargo(&real);
        Self { home, shims, real }
    }

    fn build(&self) -> PathBuf {
        let path = self.home.path().join("build");
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// The shim, with the shim directory first on `PATH` and the original value after the
    /// real program's directory.
    fn shim(&self) -> Command {
        let mut command = Command::new(self.shim_path());
        self.isolate(&mut command);
        command
    }

    fn isolate(&self, command: &mut Command) {
        let path = std::env::join_paths([self.shims.clone(), self.real.clone()].into_iter().chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        command
            .env("PATH", path)
            .env("CROSSAGENTS_HOME", self.home.path())
            .env_remove("CROSSAGENTS_HUB_HOME")
            .env_remove("CROSSAGENTS_HELD")
            .env_remove("CROSSAGENTS_RESOURCES");
    }

    fn shim_path(&self) -> PathBuf {
        if cfg!(windows) {
            self.shims.join("cargo.exe")
        } else {
            self.shims.join("cargo")
        }
    }

    /// `CROSSAGENTS_RESOURCES` naming `build` at `path`, as the hub puts it in a run worktree.
    fn declare_build(&self, path: &Path) -> String {
        serde_json::json!([{
            "name": "build",
            "path": path.display().to_string(),
            "access": "exclusive",
        }])
        .to_string()
    }
}

/// A `cargo` that is not the shim: prints what it was given and exits [`FAKE_EXIT`].
fn fake_cargo(dir: &Path) {
    if cfg!(windows) {
        std::fs::write(
            dir.join("cargo.bat"),
            "@echo off\r\necho real cargo: %*\r\nexit /b 5\r\n",
        )
        .unwrap();
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file = dir.join("cargo");
            std::fs::write(&file, "#!/bin/sh\necho \"real cargo: $*\"\nexit 5\n").unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A background `exec` holding `build`, killed when the test ends.
struct Holding(Child);

impl Holding {
    fn start(home: &Path, build: &Path, seconds: u64) -> Self {
        let sleeper: Vec<String> = if cfg!(windows) {
            vec![
                "cmd".to_owned(),
                "/c".to_owned(),
                format!("ping -n {} 127.0.0.1 >NUL", seconds + 1),
            ]
        } else {
            vec!["sh".to_owned(), "-c".to_owned(), format!("sleep {seconds}")]
        };
        let mut command = Command::new(BIN);
        command
            .env("CROSSAGENTS_HOME", home)
            .env_remove("CROSSAGENTS_HUB_HOME")
            .env_remove("CROSSAGENTS_HELD")
            .env_remove("CROSSAGENTS_RESOURCES")
            .arg("exec")
            .arg("--resource")
            .arg(format!("build={}", build.display()))
            .arg("--timeout")
            .arg(format!("{seconds}s"))
            .arg("--")
            .args(sleeper)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Self(command.spawn().expect("start the exec holding build"))
    }
}

impl Drop for Holding {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Waits until the lock is taken and its record names a root process.
fn wait_until_held(home: &Path) {
    let locks = home.join("locks");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(entries) = std::fs::read_dir(&locks) {
            for entry in entries.flatten() {
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
            "the exec never took the lock in {}",
            locks.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

// --- finds the real program, skipping its own directory, and passes everything through ----

#[test]
fn shim_runs_the_real_program_on_path_beyond_its_own_directory() {
    let setup = Setup::new();
    // Nothing declared: this is no run worktree, and the program runs directly.
    let output = setup
        .shim()
        .args(["test", "--workspace"])
        .output()
        .expect("run the shim");
    assert_eq!(
        output.status.code(),
        Some(FAKE_EXIT),
        "stderr: {}",
        stderr(&output)
    );
    let printed = stdout(&output);
    assert!(
        printed.contains("real cargo: test --workspace"),
        "the shim did not run the real cargo with the same arguments: {printed}"
    );
}

#[test]
fn shim_says_so_when_there_is_no_real_program() {
    let setup = Setup::new();
    let mut command = Command::new(setup.shim_path());
    command
        .env("PATH", &setup.shims)
        .env("CROSSAGENTS_HOME", setup.home.path())
        .env_remove("CROSSAGENTS_HELD")
        .env_remove("CROSSAGENTS_RESOURCES")
        .arg("--version");
    let output = command.output().expect("run the shim");
    assert_eq!(output.status.code(), Some(127), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("not on PATH"),
        "{}",
        stderr(&output)
    );
}

// --- holds `build` like `exec --resource build -- …` ---------------------------------------

#[test]
fn shim_holds_the_build_resource_of_the_run() {
    let setup = Setup::new();
    let build = setup.build();
    let output = setup
        .shim()
        .env("CROSSAGENTS_RESOURCES", setup.declare_build(&build))
        .args(["test", "-p", "crossagents-core"])
        .output()
        .expect("run the shim");
    assert_eq!(
        output.status.code(),
        Some(FAKE_EXIT),
        "stderr: {}",
        stderr(&output)
    );
    assert!(
        stdout(&output).contains("real cargo: test -p crossagents-core"),
        "{}",
        stdout(&output)
    );
    // The lock was taken and let go: nothing is left behind, and no lock is held now.
    let free = setup
        .shim()
        .env("CROSSAGENTS_RESOURCES", setup.declare_build(&build))
        .args(["--version"])
        .output()
        .expect("run the shim again");
    assert_eq!(free.status.code(), Some(FAKE_EXIT), "{}", stderr(&free));
}

// --- somebody else holds `build`: the shim waits and says so, like any exec -----------------

#[test]
fn shim_waits_for_a_lock_somebody_else_holds() {
    let setup = Setup::new();
    let build = setup.build();
    let _held = Holding::start(setup.home.path(), &build, 12);
    wait_until_held(setup.home.path());

    let log = setup.home.path().join("shim.stderr");
    let file = std::fs::File::create(&log).unwrap();
    let mut child = setup
        .shim()
        .env("CROSSAGENTS_RESOURCES", setup.declare_build(&build))
        .args(["test"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(file))
        .spawn()
        .expect("run the shim");

    // The wait reports who holds the build at once, then goes on waiting; the shim's own
    // timeout is an `exec`'s (ten minutes), so this looks at the report, not at the end.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut reported = false;
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(&log)
            && text.contains("waiting for build")
        {
            reported = true;
            break;
        }
        if child.try_wait().expect("poll the shim").is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let ran = child.try_wait().expect("poll the shim").is_some();
    let _ = child.kill();
    let _ = child.wait();
    assert!(reported, "the shim never said what it waits for");
    assert!(!ran, "the shim ran while another exec held build");
}

// --- inside an exec that already holds `build`: no second lock, no wait ---------------------

#[test]
fn a_shim_inside_an_exec_that_holds_build_takes_no_lock_again() {
    let setup = Setup::new();
    let build = setup.build();
    let shim = setup.shim_path();
    let resources = setup.declare_build(&build);

    let mut command = Command::new(BIN);
    setup.isolate(&mut command);
    // The outer exec holds `build` and runs the shim: `CROSSAGENTS_HELD` covers it (§4.5).
    command
        .arg("exec")
        .arg("--resource")
        .arg(format!("build={}", build.display()))
        .arg("--timeout")
        .arg("60s")
        .arg("--")
        .arg(&shim)
        .args(["test", "--lib"])
        .env("CROSSAGENTS_RESOURCES", resources)
        .stdin(Stdio::null());

    let started = Instant::now();
    let output = command.output().expect("run the outer exec");
    let took = started.elapsed();
    assert!(
        took < Duration::from_secs(30),
        "the shim waited for a lock its own ancestor held: {took:?}\n{}",
        stderr(&output)
    );
    assert_eq!(
        output.status.code(),
        Some(FAKE_EXIT),
        "the real cargo did not run: {}",
        stderr(&output)
    );
    assert!(
        stdout(&output).contains("real cargo: test --lib"),
        "{}",
        stdout(&output)
    );
}
