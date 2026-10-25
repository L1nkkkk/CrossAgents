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

/// docs/tasks/DOGFOOD.md §1: a dev hub beside the one that runs the agent, and the agent
/// cannot stop its own hub by mistake.
#[test]
fn sandbox_hub_runs_beside_the_stable_one() {
    let home = Home::new();
    let stable = home.json(&["hub", "start"]);
    let dev_dir = home.0.path().join("dev");
    let dev = dev_dir.display().to_string();
    let started = home.json(&["hub", "sandbox", "start", "--dir", &dev]);
    assert_eq!(started["running"], true);
    assert_ne!(started["port"], stable["port"]);
    assert!(dev_dir.join("config.toml").exists());
    // Launched from a copy: the build output stays replaceable.
    let hub_file: Value =
        serde_json::from_slice(&std::fs::read(dev_dir.join("hub.json")).unwrap()).unwrap();
    let exe = hub_file["exe"].as_str().unwrap();
    assert!(exe.contains("shadow"), "{exe}");

    // A shell under the stable hub may not stop it, but may stop the dev hub.
    let refused = home
        .cmd(&["hub", "stop"])
        .env("CROSSAGENTS_HUB_HOME", home.0.path())
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    assert!(String::from_utf8_lossy(&refused).contains("sandbox"));
    assert_eq!(home.json(&["hub", "status"])["running"], true);
    home.cmd(&["hub", "sandbox", "stop", "--dir", &dev])
        .env("CROSSAGENTS_HUB_HOME", home.0.path())
        .assert()
        .success();
    assert_eq!(
        home.json(&["hub", "sandbox", "status", "--dir", &dev])["running"],
        false
    );
    // A dev hub never takes the stable home.
    let own = home.0.path().display().to_string();
    assert!(
        home.failure(&["hub", "sandbox", "start", "--dir", &own])
            .contains("own home")
    );
}

/// `send --file` refuses a file the hub would refuse, before it starts or reaches a hub.
#[test]
fn send_refuses_bad_files_before_sending() {
    let home = Home::new();
    let exe = home.0.path().join("tool.exe");
    std::fs::write(&exe, b"MZ").unwrap();
    let err = home.failure(&["send", "alice", "hi", "--file", &exe.display().to_string()]);
    assert!(
        err.contains("tool.exe: files of this type cannot be sent"),
        "{err}"
    );
    assert_eq!(home.json(&["hub", "status"])["running"], false);
}
