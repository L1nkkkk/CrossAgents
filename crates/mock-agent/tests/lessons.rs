//! Troubleshooting records (docs/tasks/LESSONS.md, L1): a problem recorded in a run that was
//! then cancelled is sorted from another run, once however often it is repeated, and found by
//! a third agent of the project, whose instructions say the records exist. Another project
//! can neither read the run nor find the record.

mod common;

use common::{MOCK_AGENT, TestHub, git_repo};
use crossagents_api_types::EventKind;
use crossagents_core::{AgentId, DriverKind};
use crossagents_plugins::PluginConfig;
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
}

/// Sends `text` to `agent` and returns what it printed in that turn.
async fn turn(t: &TestHub, agent: &str, id: AgentId, text: &str) -> String {
    let mut w = t.watch().await;
    t.send(agent, text).await;
    w.until(
        "turn completed",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id: done, .. } if *done == id),
    )
    .await;
    w.output()
}

/// Starts a run led by `lead` and waits for its kickoff turn to end.
async fn run_led_by(t: &TestHub, lead: &str, lead_id: AgentId) -> String {
    let mut w = t.watch().await;
    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": "Tidy the README.", "planner": lead }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let run: Value = resp.json().await.unwrap();
    // A lead that had no session is moved into the run, so it keeps its id.
    assert_eq!(run["planner"], json!(lead_id.to_string()), "{run}");
    w.until(
        "kickoff answered",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == lead_id),
    )
    .await;
    run["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn a_problem_from_a_cancelled_run_is_sorted_elsewhere_and_found_later() {
    let repo = git_repo();
    git(
        repo.path(),
        &["remote", "add", "origin", "git@github.com:o/r.git"],
    );
    let other = git_repo();
    git(
        other.path(),
        &["remote", "add", "origin", "https://github.com/me/elsewhere"],
    );
    // A Codex-like mock says its instructions back when asked (`[instructions]`).
    let t = TestHub::start_with(|config| {
        config.plugins.insert(
            "codex".into(),
            PluginConfig {
                driver: Some(DriverKind::CodexNative),
                native_command: vec![MOCK_AGENT.into(), "app-server".into()],
                ..PluginConfig::default()
            },
        );
    })
    .await;
    let instructions_in = |dir: &std::path::Path, name: &'static str| {
        let dir = dir.to_path_buf();
        let t = &t;
        async move {
            let resp = t
                .post(
                    "/v1/agents",
                    json!({ "name": name, "plugin": "codex", "model": "mock-codex", "role": "worker",
                            "cwd": dir.display().to_string() }),
                )
                .await;
            assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
            let agent: Value = resp.json().await.unwrap();
            let id: AgentId = agent["id"].as_str().unwrap().parse().unwrap();
            turn(t, name, id, "[instructions]").await
        }
    };

    // Run 1: its lead records a problem, then the run is cancelled.
    let pia = t
        .add_agent_in("pia", Some("opus"), "planner", repo.path())
        .await;
    let first = run_led_by(&t, "pia", pia.id).await;
    let issue = json!({
        "key": "issue/lead/0b8c",
        "text": "问题：python exited with code 9009\n环境：Windows 11\n处理：改用 node\n结果：临时绕过",
    });
    let out = turn(&t, "pia", pia.id, &call("note_write", issue)).await;
    assert!(out.contains("\"written\": true"), "{out}");
    let resp = t.post(&format!("/v1/runs/{first}/cancel"), json!({})).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());

    // Run 2's lead reads the ended run's problem and sorts it, twice as after a crash.
    let quinn = t
        .add_agent_in("quinn", Some("opus"), "planner", repo.path())
        .await;
    run_led_by(&t, "quinn", quinn.id).await;
    let out = turn(
        &t,
        "quinn",
        quinn.id,
        &call("issue_pending", json!({ "run": first })),
    )
    .await;
    assert!(
        out.contains("issue/lead/0b8c") && out.contains("9009"),
        "{out}"
    );
    let create = call(
        "lesson_create",
        json!({
            "run": first,
            "issues": ["issue/lead/0b8c"],
            "title": "python 是 Windows Store 的占位程序",
            "symptom": "python exited with code 9009",
            "keywords": ["9009", "WindowsApps"],
            "conditions": "Windows",
            "resolution": "改用 node 写脚本",
            "outcome": "workaround",
        }),
    );
    let once = turn(&t, "quinn", quinn.id, &create).await;
    let twice = turn(&t, "quinn", quinn.id, &create).await;
    let lesson_id = |out: &str| {
        let at = out
            .find("\"lesson\": \"")
            .unwrap_or_else(|| panic!("{out}"))
            + 11;
        out[at..at + 36].to_owned()
    };
    assert_eq!(lesson_id(&once), lesson_id(&twice));
    let out = turn(
        &t,
        "quinn",
        quinn.id,
        &call("issue_pending", json!({ "run": first })),
    )
    .await;
    assert!(out.contains("\"pending\": []"), "{out}");

    // A third agent of the project is told the records exist, and finds this one.
    let rex = t
        .add_agent_in("rex", Some("opus"), "worker", repo.path())
        .await;
    let out = instructions_in(repo.path(), "cx").await;
    assert!(out.contains("1 troubleshooting record"), "{out}");
    assert!(out.contains("lesson_search"), "{out}");
    assert!(
        !out.contains("改用 node"),
        "only that they exist, never their text: {out}"
    );
    let out = turn(
        &t,
        "rex",
        rex.id,
        &call("lesson_search", json!({ "query": "exit code 9009" })),
    )
    .await;
    assert!(out.contains("python 是 Windows Store 的占位程序"), "{out}");
    assert!(out.contains(&lesson_id(&once)), "{out}");
    let out = turn(
        &t,
        "rex",
        rex.id,
        &call(
            "lesson_report",
            json!({ "id": lesson_id(&once), "worked": false, "note": "python 是真的" }),
        ),
    )
    .await;
    assert!(out.contains("\"status\": \"review\""), "{out}");
    let out = turn(
        &t,
        "rex",
        rex.id,
        &call("lesson_get", json!({ "id": lesson_id(&once) })),
    )
    .await;
    assert!(out.contains("no longer works"), "{out}");
    assert!(
        out.contains("\"last_failure\"") && out.contains("python 是真的"),
        "{out}"
    );
    assert!(out.contains("\"total\": 2"), "{out}");

    // Another project: the run is not its to read, and the record is not there.
    let olga = t
        .add_agent_in("olga", Some("opus"), "worker", other.path())
        .await;
    let out = turn(
        &t,
        "olga",
        olga.id,
        &call("issue_pending", json!({ "run": first })),
    )
    .await;
    assert!(out.contains("another project"), "{out}");
    let out = turn(
        &t,
        "olga",
        olga.id,
        &call("lesson_search", json!({ "query": "9009" })),
    )
    .await;
    assert!(out.contains("\"lessons\": []"), "{out}");
    let out = instructions_in(other.path(), "cy").await;
    assert!(!out.contains("troubleshooting record"), "{out}");
}
