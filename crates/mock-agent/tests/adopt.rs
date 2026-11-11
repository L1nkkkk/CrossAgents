//! Agents outlive the hub (docs/PROCESS-SUPERVISION.md): a restarted hub takes over the
//! codex app-server left running in its runtime host, the turn that was running finishes,
//! and the same process serves the next turn. A host that died meanwhile is not taken
//! over: its turn is closed as `interrupted`.

mod common;

use std::time::Duration;

use common::{MOCK_AGENT, TestHub, crossagents_bin, hub_permit, mock_plugins, wait_limit};
use crossagents_api_types::{Event, EventKind, StopReason};
use crossagents_core::DriverKind;
use crossagents_hub::{HubConfig, Paths};
use crossagents_plugins::PluginConfig;
use serde_json::{Value, json};

fn config(paths: &Paths, builtin_node: bool) -> HubConfig {
    let mut config = HubConfig::new(paths.clone(), 0);
    config.plugins = mock_plugins();
    config.plugins.insert(
        "codex".into(),
        PluginConfig {
            driver: Some(DriverKind::CodexNative),
            native_command: vec![MOCK_AGENT.into(), "app-server".into()],
            ..PluginConfig::default()
        },
    );
    // Claude Code and CodeBuddy speak the same stream-json protocol; the drivers differ in
    // flags, environment and how they hand the process over.
    for (plugin, driver) in [
        ("claude", DriverKind::ClaudeNative),
        ("codebuddy", DriverKind::CodebuddyNative),
    ] {
        config.plugins.insert(
            plugin.into(),
            PluginConfig {
                driver: Some(driver),
                native_command: vec![MOCK_AGENT.into(), "stream-json".into()],
                models: vec![format!("mock-{plugin}")],
                ..PluginConfig::default()
            },
        );
    }
    // No collaboration tools: nothing here calls them.
    config.mcp_command = Some(Vec::new());
    // The hub runs its own node or drives the drivers directly (B1.2); each scenario runs both ways.
    config.builtin_node = builtin_node;
    config.runtime_host = Some(vec![
        crossagents_bin().display().to_string(),
        "runtime-host".into(),
    ]);
    config
}

async fn start(home: tempfile::TempDir, builtin_node: bool) -> TestHub {
    let paths = Paths::new(home.path());
    let permit = hub_permit().await;
    let hub = crossagents_hub::start(config(&paths, builtin_node))
        .await
        .unwrap();
    let base = format!("http://{}", hub.addr);
    TestHub {
        hub,
        client: reqwest::Client::new(),
        base,
        paths,
        home,
        permit,
    }
}

/// Stops the hub, leaving agents in runtime hosts running; returns the home.
async fn leave(t: TestHub) -> tempfile::TempDir {
    t.hub.shutdown();
    t.hub.wait().await.unwrap();
    t.home
}

/// Every event so far.
async fn history(t: &TestHub) -> Vec<Event> {
    let text = t
        .get("/v1/events?since=0&follow=false")
        .await
        .text()
        .await
        .unwrap();
    text.lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .filter_map(|d| serde_json::from_str(d.trim_start()).ok())
        .collect()
}

fn pid_in(output: &str) -> String {
    output
        .split("pid=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("no pid in {output}"))
        .to_owned()
}

async fn add_codex(t: &TestHub) {
    add(t, "cx", "codex", "mock-codex").await;
}

async fn add(t: &TestHub, name: &str, plugin: &str, model: &str) {
    let resp = t
        .post(
            "/v1/agents",
            json!({ "name": name, "plugin": plugin, "model": model, "role": "worker",
                    "cwd": t.home.path().display().to_string() }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
}

/// Runtime host settings are process-wide: these tests take turns.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// Each scenario runs with the hub's built-in node on (the default) and off (B1.2): the
// same takeover must work either way.

#[tokio::test]
async fn a_restarted_hub_takes_over_running_agents() {
    takes_over(
        "codex",
        "mock-codex",
        "echo(model=mock-codex,effort=medium): [sleep 6000] long",
        true,
    )
    .await;
}

#[tokio::test]
async fn a_restarted_hub_takes_over_running_agents_without_the_built_in_node() {
    takes_over(
        "codex",
        "mock-codex",
        "echo(model=mock-codex,effort=medium): [sleep 6000] long",
        false,
    )
    .await;
}

#[tokio::test]
async fn a_restarted_hub_takes_over_claude_code() {
    takes_over(
        "claude",
        "mock-claude",
        "echo(model=mock-claude): [sleep 6000] long",
        true,
    )
    .await;
}

#[tokio::test]
async fn a_restarted_hub_takes_over_claude_code_without_the_built_in_node() {
    takes_over(
        "claude",
        "mock-claude",
        "echo(model=mock-claude): [sleep 6000] long",
        false,
    )
    .await;
}

#[tokio::test]
async fn a_restarted_hub_takes_over_codebuddy() {
    takes_over(
        "codebuddy",
        "mock-codebuddy",
        "echo(model=mock-codebuddy): [sleep 6000] long",
        true,
    )
    .await;
}

#[tokio::test]
async fn a_restarted_hub_takes_over_codebuddy_without_the_built_in_node() {
    takes_over(
        "codebuddy",
        "mock-codebuddy",
        "echo(model=mock-codebuddy): [sleep 6000] long",
        false,
    )
    .await;
}

#[tokio::test]
async fn a_turn_whose_host_died_is_interrupted() {
    host_died_is_interrupted(true).await;
}

#[tokio::test]
async fn a_turn_whose_host_died_is_interrupted_without_the_built_in_node() {
    host_died_is_interrupted(false).await;
}

/// A hub stops while the agent's long turn runs; the next hub takes the process over, the
/// turn ends there with the answer `echo`, and the same process serves the next turn.
async fn takes_over(plugin: &str, model: &str, echo: &str, builtin_node: bool) {
    let _serial = SERIAL.lock().await;
    let t = start(tempfile::tempdir().unwrap(), builtin_node).await;
    let name = "worker";
    add(&t, name, plugin, model).await;

    let mut w = t.watch().await;
    t.send(name, "[pid]").await;
    w.until("first turn", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;
    let pid = pid_in(&w.output());

    // A long turn is running when the hub goes.
    let sent = t.send(name, "[sleep 6000] long").await;
    let long: crossagents_api_types::MessageId =
        serde_json::from_value(sent["message_id"].clone()).unwrap();
    w.until("long turn started", |e| {
        matches!(&e.kind, EventKind::AgentTurnStarted { message_id, .. } if *message_id == long)
    })
    .await;
    let runtimes = t.paths.runtimes();
    let home = leave(t).await;
    assert_eq!(
        crossagents_drivers::host::records(&runtimes).len(),
        1,
        "the {plugin} process's host keeps running"
    );

    // The next hub takes it over; the long turn ends there.
    let t = start(home, builtin_node).await;
    let agent: Value = t
        .get(&format!("/v1/agents/{name}"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(agent["status"], "running", "{agent}");
    let deadline = std::time::Instant::now() + wait_limit();
    let (events, completed) = loop {
        let events = history(&t).await;
        let completed = events.iter().find_map(|e| match &e.kind {
            EventKind::AgentTurnCompleted {
                message_id,
                stop_reason,
                ..
            } if *message_id == long => Some(*stop_reason),
            _ => None,
        });
        if completed.is_some() {
            break (events, completed);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the long turn did not end"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(completed, Some(StopReason::EndTurn));
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.kind, EventKind::AgentAdopted { message_id, .. } if *message_id == Some(long)))
    );
    let output: String = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::AgentOutput { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(output.contains(echo), "{output}");

    // Same process for the next turn.
    let mut w = t.watch().await;
    t.send(name, "[pid]").await;
    w.until("turn after takeover", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;
    assert_eq!(pid_in(&w.output()), pid);

    // Stopping with the agents ends the host too.
    let resp = t.post("/v1/shutdown?stop_agents=true", json!({})).await;
    assert_eq!(resp.status(), 202);
    let runtimes = t.paths.runtimes();
    t.hub.wait().await.unwrap();
    let deadline = std::time::Instant::now() + wait_limit();
    while !crossagents_drivers::host::records(&runtimes).is_empty() {
        assert!(std::time::Instant::now() < deadline, "host still running");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn host_died_is_interrupted(builtin_node: bool) {
    let _serial = SERIAL.lock().await;
    let t = start(tempfile::tempdir().unwrap(), builtin_node).await;
    add_codex(&t).await;
    let mut w = t.watch().await;
    let sent = t.send("cx", "[slow]").await;
    let slow: crossagents_api_types::MessageId =
        serde_json::from_value(sent["message_id"].clone()).unwrap();
    w.until("slow turn started", |e| {
        matches!(&e.kind, EventKind::AgentTurnStarted { message_id, .. } if *message_id == slow)
    })
    .await;
    let runtimes = t.paths.runtimes();
    let home = leave(t).await;

    // The host is killed from outside while no hub is there.
    let records = crossagents_drivers::host::records(&runtimes);
    assert_eq!(records.len(), 1);
    kill(records[0].host_pid);
    if let Some(pid) = records[0].child_pid {
        kill(pid);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    let t = start(home, builtin_node).await;
    let events = history(&t).await;
    let completed = events.iter().find_map(|e| match &e.kind {
        EventKind::AgentTurnCompleted {
            message_id,
            stop_reason,
            ..
        } if *message_id == slow => Some(*stop_reason),
        _ => None,
    });
    assert_eq!(completed, Some(StopReason::Interrupted));
    let agent: Value = t.get("/v1/agents/cx").await.json().await.unwrap();
    assert_eq!(agent["status"], "stopped", "{agent}");
    assert!(crossagents_drivers::host::records(&runtimes).is_empty());

    // The agent works again: it resumes its thread in a new process.
    let mut w = t.watch().await;
    t.send("cx", "again").await;
    let done = w
        .until("turn after the loss", |e| {
            matches!(e.kind, EventKind::AgentTurnCompleted { .. })
        })
        .await;
    assert!(matches!(
        done.kind,
        EventKind::AgentTurnCompleted {
            stop_reason: StopReason::EndTurn,
            ..
        }
    ));
    let resp = t.post("/v1/shutdown?stop_agents=true", json!({})).await;
    assert_eq!(resp.status(), 202);
    t.hub.wait().await.unwrap();
    let deadline = std::time::Instant::now() + wait_limit();
    while !crossagents_drivers::host::records(&runtimes).is_empty() {
        assert!(std::time::Instant::now() < deadline, "host still running");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn kill(pid: u32) {
    let status = if cfg!(windows) {
        std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
    } else {
        std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
    };
    let _ = status;
}
