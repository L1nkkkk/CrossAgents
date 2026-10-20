//! The `crossagents` binary talking to the mock agent through a background hub.

mod common;

use assert_cmd::Command;

struct Home(tempfile::TempDir);

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // TOML literal strings ('...') keep Windows backslashes as they are.
        let config = format!(
            "[hub]\nport = 0\n\n[plugins.mock]\ncommand = ['{}']\n",
            common::MOCK_AGENT
        );
        std::fs::write(dir.path().join("config.toml"), config).unwrap();
        Self(dir)
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::cargo_bin("crossagents").expect("build the workspace first");
        cmd.env("CROSSAGENTS_HOME", self.0.path()).args(args);
        cmd
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self
            .cmd(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8(out).unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.cmd(&["hub", "stop"]).output();
    }
}

#[test]
fn send_wait_prints_the_answer() {
    let home = Home::new();
    home.stdout(&["hub", "start"]);
    let cwd = home.0.path().display().to_string();
    home.stdout(&[
        "agent", "add", "alice", "--plugin", "mock", "--model", "haiku", "--role", "worker",
        "--cwd", &cwd,
    ]);

    let answer = home.stdout(&["send", "alice", "hello", "world", "--wait"]);
    assert_eq!(
        answer.trim(),
        "echo(model=mock-haiku,mode=bypassPermissions): hello world"
    );

    let ls = home.stdout(&["agent", "ls"]);
    assert!(ls.contains("idle"), "{ls}");
    let plugins = home.stdout(&["plugin", "ls"]);
    assert!(plugins.contains("mock"), "{plugins}");

    home.stdout(&["agent", "stop", "alice"]);
    let ls = home.stdout(&["agent", "ls"]);
    assert!(ls.contains("stopped"), "{ls}");
}
