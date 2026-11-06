//! `crossagents work` and `crossagents run ls` against a background hub (docs/tasks/G1.md G1-5).
//! The rounds themselves need an agent to lead them; these cover the empty hub, the output
//! shapes, the errors, and the flags. The grouping of rounds is unit-tested in `cmd/work.rs`.

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

    fn text(&self, args: &[&str]) -> String {
        let out = self
            .cmd(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8(out).unwrap()
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
fn work_ls_on_an_empty_hub_says_so_and_prints_json() {
    let home = Home::new();
    home.json(&["hub", "start"]);

    let text = home.text(&["work", "ls"]);
    assert!(text.contains("no work groups"), "{text}");

    let list = home.json(&["work", "ls"]);
    assert_eq!(list, serde_json::json!([]));
}

#[test]
fn run_ls_on_an_empty_hub_still_works_and_json_lists_runs() {
    let home = Home::new();
    home.json(&["hub", "start"]);

    let text = home.text(&["run", "ls"]);
    assert!(text.contains("no runs"), "{text}");
    let list = home.json(&["run", "ls"]);
    assert_eq!(list["runs"], serde_json::json!([]));
}

#[test]
fn work_show_an_unknown_group_is_refused_with_a_pointer_to_ls() {
    let home = Home::new();
    home.json(&["hub", "start"]);

    let err = home.failure(&["work", "show", "deadbeef"]);
    assert!(err.contains("no work group"), "{err}");
    assert!(err.contains("work ls"), "{err}");
}

#[test]
fn work_show_needs_a_group() {
    let home = Home::new();
    let err = home.failure(&["work", "show"]);
    assert!(err.contains("<WORK>") || err.contains("required"), "{err}");
}

#[test]
fn work_help_lists_ls_and_show() {
    let home = Home::new();
    let help = home.text(&["work", "--help"]);
    assert!(help.contains("ls"), "{help}");
    assert!(help.contains("show"), "{help}");
}
