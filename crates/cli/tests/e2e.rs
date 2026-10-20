//! End-to-end: the real `crossagents` binary against a background hub.

use assert_cmd::Command;
use serde_json::Value;

struct Home(tempfile::TempDir);

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[hub]\nport = 0\n").unwrap();
        Self(dir)
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::cargo_bin("crossagents").unwrap();
        cmd.env("CROSSAGENTS_HOME", self.0.path()).args(args);
        cmd
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut full = args.to_vec();
        full.push("--json");
        let out = self
            .cmd(&full)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&out).unwrap()
    }

    /// Runs a command that must fail and returns its stderr.
    fn failure(&self, args: &[&str]) -> String {
        let out = self
            .cmd(args)
            .assert()
            .failure()
            .get_output()
            .stderr
            .clone();
        String::from_utf8(out).unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // Never leave a background hub behind, even if the test failed.
        let _ = self.cmd(&["hub", "stop"]).output();
    }
}

#[test]
fn hub_and_agent_round_trip() {
    let home = Home::new();
    assert_eq!(home.json(&["hub", "status"])["running"], false);
    assert_eq!(home.json(&["hub", "start"])["running"], true);
    assert_eq!(home.json(&["hub", "start"])["already_running"], true);

    let cwd = home.0.path().display().to_string();
    let agent = home.json(&[
        "agent", "add", "alice", "--plugin", "claude", "--model", "haiku", "--role", "worker",
        "--cwd", &cwd,
    ]);
    assert_eq!(agent["name"], "alice");

    let dup = home.failure(&[
        "agent", "add", "alice", "--plugin", "claude", "--role", "worker",
    ]);
    assert!(dup.contains("already taken"), "{dup}");

    let list = home.json(&["agent", "ls"]);
    assert_eq!(list["agents"].as_array().unwrap().len(), 1);

    let events = home
        .cmd(&["events"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let events = String::from_utf8(events).unwrap();
    assert!(
        events.contains("hub.started") && events.contains("agent.created"),
        "{events}"
    );

    assert_eq!(home.json(&["hub", "stop"])["running"], false);
    let stopped = home.failure(&["agent", "ls"]);
    assert!(stopped.contains("not running"), "{stopped}");

    // State survives a restart.
    home.json(&["hub", "start"]);
    let list = home.json(&["agent", "ls"]);
    assert_eq!(list["agents"][0]["name"], "alice");
}
