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

/// Polls `run show` until the run reaches `status`.
fn wait_run(home: &Home, id: &str, status: &str) -> serde_json::Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let detail: serde_json::Value =
            serde_json::from_str(&home.stdout(&["run", "show", id, "--json"])).unwrap();
        if detail["run"]["status"] == status {
            return detail;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "run never reached {status}: {detail:#}"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

#[test]
fn full_run_with_a_directory_plugin() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home(dir);
    // No [plugins] entry: the mock comes from a plugin directory this time.
    let config = format!(
        "[hub]\nport = 0\nmcp_command = ['{}', 'mcp']\n",
        common::crossagents_bin().display()
    );
    std::fs::write(home.0.path().join("config.toml"), config).unwrap();
    let plugin_dir = home.0.path().join("plugins").join("mock");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        format!(
            "name = \"mock\"\nprotocol = \"acp\"\ncommand = ['{}']\nmodels = [\"mock-opus\", \"mock-haiku\"]\n",
            common::MOCK_AGENT
        ),
    )
    .unwrap();

    let repo = common::git_repo();
    let repo_path = repo.path().display().to_string();
    home.stdout(&["hub", "start"]);
    let plugins = home.stdout(&["plugin", "ls"]);
    assert!(
        plugins.contains("mock") && plugins.contains("mock-haiku"),
        "{plugins}"
    );
    home.stdout(&[
        "agent", "add", "boss", "--plugin", "mock", "--model", "opus", "--role", "planner",
        "--cwd", &repo_path,
    ]);
    home.stdout(&[
        "agent", "add", "coder", "--plugin", "mock", "--model", "haiku", "--role", "worker",
        "--cwd", &repo_path,
    ]);

    let plan = serde_json::json!({ "tasks": [{
        "key": "hello",
        "title": "write hello",
        "spec": {
            // The summary carries the planner's next step: accept the task.
            "goal": r#"[write hello.txt=hi] [call task_complete {"task_id":"{task_id}","summary":"hello [call task_accept {\"task\":\"hello\"}]"}]"#,
            "acceptance": ["git grep -q hi -- hello.txt"]
        }
    }]});
    let request = format!("Say hi. [call plan_submit {plan}]");
    let started: serde_json::Value = serde_json::from_str(&home.stdout(&[
        "run",
        &request,
        "--planner",
        "boss",
        "--worker",
        "coder",
        "--json",
    ]))
    .unwrap();
    let id = started["id"].as_str().unwrap().to_owned();

    let pending = wait_run(&home, &id, "awaiting_approval");
    assert_eq!(pending["run"]["plan"][0]["key"], "hello");
    let human = home.stdout(&["run", "show", &id]);
    assert!(human.contains("waiting for approval"), "{human}");

    home.stdout(&["run", "approve", &id]);
    let done = wait_run(&home, &id, "completed");
    assert_eq!(done["tasks"][0]["status"], "merged");
    let ls = home.stdout(&["run", "ls"]);
    assert!(ls.contains("completed"), "{ls}");

    let usage: serde_json::Value =
        serde_json::from_str(&home.stdout(&["usage", "--by", "role", "--json"])).unwrap();
    let cost = |role: &str| {
        usage["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == role)
            .unwrap_or_else(|| panic!("no {role} row: {usage:#}"))["cost_usd"]
            .as_f64()
            .unwrap()
    };
    assert!(cost("planner") > cost("worker"), "{usage:#}");
    assert!(cost("worker") > 0.0);
}
