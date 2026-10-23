//! `crossagents hub run` with its output piped (`hub run | tee log`, a GUI or script reading
//! it): once the hub exits, the reader sees the end of its output at once, even though the
//! agents it started keep running in their runtime hosts. On Windows those hosts would
//! otherwise inherit the hub's own stdout and stderr and hold the pipes open.

mod common;

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::{MOCK_AGENT, crossagents_bin};

fn cli(home: &Path, args: &[&str]) -> String {
    let out = Command::new(crossagents_bin())
        .env("CROSSAGENTS_HOME", home)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "crossagents {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Reads `pipe` to its end in the background; says on `done` when it got there.
fn drain(
    mut pipe: impl Read + Send + 'static,
    done: mpsc::Sender<&'static str>,
    what: &'static str,
) {
    std::thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = pipe.read_to_end(&mut sink);
        let _ = done.send(what);
    });
}

#[test]
fn piped_output_ends_with_the_hub_while_agents_keep_running() {
    let home = tempfile::tempdir().unwrap();
    // TOML literal strings ('...') keep Windows backslashes as they are.
    let config = format!(
        "[hub]\nport = 0\nmcp_command = []\n\n[accounts]\ndetect = false\n\n\
         [plugins.codex]\ndriver = \"codex-native\"\nnative_command = ['{MOCK_AGENT}', 'app-server']\n"
    );
    std::fs::write(home.path().join("config.toml"), config).unwrap();

    let mut hub = Command::new(crossagents_bin())
        .env("CROSSAGENTS_HOME", home.path())
        .args(["hub", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (done_tx, done) = mpsc::channel();
    drain(hub.stdout.take().unwrap(), done_tx.clone(), "stdout");
    drain(hub.stderr.take().unwrap(), done_tx, "stderr");

    let deadline = Instant::now() + Duration::from_secs(20);
    while !home.path().join("hub.json").exists() {
        assert!(Instant::now() < deadline, "the hub did not start");
        std::thread::sleep(Duration::from_millis(100));
    }
    let cwd = home.path().display().to_string();
    cli(
        home.path(),
        &[
            "agent",
            "add",
            "cx",
            "--plugin",
            "codex",
            "--model",
            "mock-codex",
            "--role",
            "worker",
            "--cwd",
            &cwd,
        ],
    );
    cli(home.path(), &["send", "cx", "hello", "--wait"]);
    let runtimes = home.path().join("runtimes");
    assert_eq!(crossagents_drivers::host::records(&runtimes).len(), 1);

    cli(home.path(), &["hub", "stop"]);
    let deadline = Instant::now() + Duration::from_secs(20);
    while hub.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the hub did not exit");
        std::thread::sleep(Duration::from_millis(100));
    }

    // The agent is still running, and the pipes have ended anyway.
    let records = crossagents_drivers::host::records(&runtimes);
    assert_eq!(records.len(), 1, "the agent's host keeps running");
    let ended: Vec<_> = (0..2)
        .map_while(|_| done.recv_timeout(Duration::from_secs(10)).ok())
        .collect();

    for record in &records {
        kill(record.host_pid);
        if let Some(pid) = record.child_pid {
            kill(pid);
        }
    }
    assert_eq!(
        ended.len(),
        2,
        "the hub's pipes stay open after it exited (ended: {ended:?}): its agents hold them"
    );
}

fn kill(pid: u32) {
    let _ = if cfg!(windows) {
        Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    } else {
        Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
    };
}
