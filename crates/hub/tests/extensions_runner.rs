//! E1 (docs/EXTENSIONS.md §8): extension tools really run. Real processes, the stand-in tool
//! in `src/extensions/test_tool.rs`, and for the HTTP parts a real hub on a random port.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use crossagents_api_types::{
    ExtensionAccess, ExtensionCallAccepted, ExtensionCallResponse, ExtensionCallStatus,
};
use crossagents_core::{
    Agent, AgentId, AgentProfile, AgentStatus, CoreError, EventKind, NameSource, Role,
};
use crossagents_hub::extensions::runner::{CommandSpec, OutputCapture};
use crossagents_hub::extensions::{
    CallError, Caller, HookRate, Limits, Registry, Runner, Services,
};
use crossagents_hub::{HubConfig, Paths};
use crossagents_orchestrator::host::ExtensionCaller;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};

/// The stand-in tool. `cargo test` builds examples next to the test binaries; asking for this
/// one test target alone (`--test extensions_runner`) does not, so build it then.
fn tool_exe() -> PathBuf {
    static EXE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    EXE.get_or_init(|| {
        let mut dir = std::env::current_exe().unwrap();
        dir.pop(); // the test binary
        if dir.ends_with("deps") {
            dir.pop();
        }
        let exe = dir.join("examples").join(format!(
            "extension_test_tool{}",
            std::env::consts::EXE_SUFFIX
        ));
        if !exe.is_file() {
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let status = std::process::Command::new(cargo)
                .args([
                    "build",
                    "-p",
                    "crossagents-hub",
                    "--example",
                    "extension_test_tool",
                ])
                .status()
                .unwrap();
            assert!(status.success(), "cannot build the stand-in tool");
        }
        assert!(exe.is_file(), "{} was not built", exe.display());
        exe
    })
    .clone()
}

fn tool(name: &str, groups: &[&str], timeout: u32) -> String {
    format!(
        r#"
[[tools]]
name = "{name}"
description = "the {name} tool"
groups = {groups}
timeout_secs = {timeout}
command = ['{exe}']
input_schema = {{ type = "object", properties = {{ mode = {{ type = "string" }}, ms = {{ type = "integer" }}, why = {{ type = "string" }} }}, required = ["mode"], additionalProperties = false }}
"#,
        groups = json!(groups),
        exe = tool_exe().display(),
    )
}

/// Writes `<home>/extensions/<name>` with the standard tools plus `extra` toml.
fn write_extension(home: &Path, name: &str, access: &str, version: &str, extra: &str) {
    let root = home.join("extensions").join(name);
    fs::create_dir_all(&root).unwrap();
    let mut toml = format!("name = \"{name}\"\napi = 1\naccess = \"{access}\"\n");
    toml += &tool("run", &["everyone", "human"], 60);
    toml += &tool("slow", &["everyone", "human"], 1);
    toml += &tool("agents_only", &["everyone"], 60);
    toml += &tool("humans_only", &["human"], 60);
    toml += &tool("lead_only", &["lead"], 60);
    toml += extra;
    fs::write(root.join("extension.toml"), toml).unwrap();
    fs::write(root.join("version.txt"), version).unwrap();
}

fn pending(registry: &Registry, name: &str) -> String {
    registry
        .get(name)
        .unwrap()
        .pending
        .expect("a snapshot is waiting for approval")
        .version
        .fingerprint
}

fn approve(registry: &Registry, name: &str) -> String {
    let fp = pending(registry, name);
    registry.enable(name, &fp).unwrap();
    fp
}

fn agent(name: &str) -> Agent {
    Agent {
        id: AgentId::new(),
        name_source: NameSource::Chosen,
        topic: None,
        archived_at: None,
        home: PathBuf::new(),
        name: name.into(),
        plugin: "claude".into(),
        driver: None,
        model: None,
        role: Role::Worker,
        cwd: "/work/here".into(),
        config: Value::Null,
        status: AgentStatus::Stopped,
        created_at: Utc::now(),
        profile: AgentProfile::default(),
    }
}

fn agent_caller(groups: &[&str]) -> Caller {
    Caller::Agent(Box::new(ExtensionCaller {
        agent: agent("alice"),
        groups: groups.iter().map(|g| g.to_string()).collect(),
        run: None,
        task: None,
    }))
}

struct Fixture {
    home: tempfile::TempDir,
    registry: Arc<Registry>,
    runner: Arc<Runner>,
}

impl Fixture {
    fn new(limits: Limits) -> Self {
        let home = tempfile::tempdir().unwrap();
        write_extension(home.path(), "demo", "write", "one", "");
        let registry = Arc::new(Registry::load(home.path()).unwrap());
        let runner = Runner::with_limits(
            registry.clone(),
            None,
            home.path().to_owned(),
            limits,
            vec![
                ("HTTPS_PROXY".into(), "http://127.0.0.1:7890".into()),
                ("NO_PROXY".into(), "localhost".into()),
            ],
        );
        runner.set_url("http://hub.test:1".into());
        Self {
            home,
            registry,
            runner,
        }
    }

    fn enabled(limits: Limits) -> Self {
        let f = Self::new(limits);
        approve(&f.registry, "demo");
        f
    }

    fn data(&self) -> PathBuf {
        self.home.path().join("extension-data/demo")
    }

    fn start(&self, tool: &str, input: Value) -> Result<ExtensionCallAccepted, CallError> {
        self.runner.start(Caller::Human, "demo", tool, input)
    }

    async fn call(&self, tool: &str, input: Value) -> ExtensionCallResponse {
        let accepted = self.start(tool, input).expect("the call starts");
        self.runner.wait(&accepted.call_id).await.unwrap()
    }
}

async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[test]
fn extensions_proxy_variables_are_picked_out_of_the_environment() {
    let vars = [
        ("PATH", "/bin"),
        ("https_proxy", "http://p:1"),
        ("HTTP_PROXY", "http://p:2"),
        ("No_Proxy", "localhost"),
        ("ALL_PROXY", ""),
        ("CROSSAGENTS_TOKEN", "secret"),
    ]
    .map(|(k, v)| (k.to_owned(), v.to_owned()));
    let picked = crossagents_hub::extensions::runner::proxy_env(vars);
    let names: Vec<&str> = picked.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(names, ["https_proxy", "HTTP_PROXY", "No_Proxy"]);
}

#[tokio::test]
async fn extensions_a_call_speaks_the_protocol_and_gets_its_environment() {
    let f = Fixture::enabled(Limits::default());
    let version = f.registry.enabled("demo").unwrap();

    let done = f.call("run", json!({ "mode": "echo" })).await;
    assert_eq!(done.status, ExtensionCallStatus::Ok, "{done:?}");
    assert_eq!(done.fingerprint, version.version.fingerprint);
    let out = done.output.unwrap();
    assert_eq!(out["request"]["tool"], "run");
    assert_eq!(out["request"]["input"], json!({ "mode": "echo" }));
    // A person's call says so, and nothing else about who.
    assert_eq!(out["request"]["caller"], json!({ "kind": "human" }));
    // It runs in the snapshot, not in the source.
    let cwd = out["cwd"].as_str().unwrap().replace('\\', "/");
    assert!(
        cwd.ends_with(&version.version.fingerprint),
        "{cwd} is not the snapshot"
    );
    let env = &out["env"];
    assert_eq!(env["url"], "http://hub.test:1");
    assert!(env["token"].as_str().unwrap().starts_with("ca-ext-"));
    assert_eq!(
        Path::new(env["data"].as_str().unwrap()),
        version.data.as_path()
    );
    // The hub's proxy goes along, an agent's own token does not.
    assert_eq!(env["https_proxy"], "http://127.0.0.1:7890");
    assert_eq!(env["no_proxy"], "localhost");
    assert!(env["agent_token"].is_null());

    // Its token died with the process.
    assert!(f.runner.token(env["token"].as_str().unwrap()).is_none());
    assert_eq!(f.runner.live_tokens(), 0);

    // An agent is described from what the hub knows, groups included; a missing input is `{}`.
    let accepted = f
        .runner
        .start(
            agent_caller(&["everyone", "conversation"]),
            "demo",
            "agents_only",
            json!({ "mode": "echo" }),
        )
        .unwrap();
    let done = f.runner.wait(&accepted.call_id).await.unwrap();
    let caller = &done.output.unwrap()["request"]["caller"];
    assert_eq!(caller["kind"], "agent");
    assert_eq!(caller["name"], "alice");
    assert_eq!(caller["groups"], json!(["everyone", "conversation"]));
    assert_eq!(caller["cwd"], "/work/here");
    assert!(caller["run"].is_null() && caller["task"].is_null());
    // No input at all is `{}`: it fails here only because this schema requires `mode`.
    let err = f.start("run", Value::Null).unwrap_err();
    assert!(
        matches!(&err, CallError::InvalidInput(m) if m.contains("mode")),
        "{err:?}"
    );
}

#[tokio::test]
async fn extensions_checks_come_in_the_documented_order() {
    let f = Fixture::new(Limits {
        per_extension: 1,
        ..Limits::default()
    });
    let echo = json!({ "mode": "echo" });

    // 1. Not approved yet: nothing runs, whatever else is wrong with the call.
    assert!(matches!(
        f.start("run", json!({ "mode": 5 })),
        Err(CallError::NotEnabled(_))
    ));
    assert!(matches!(
        f.runner.start(Caller::Human, "nobody", "run", echo.clone()),
        Err(CallError::Unknown(_))
    ));
    approve(&f.registry, "demo");
    assert!(matches!(
        f.start("nothing", echo.clone()),
        Err(CallError::Unknown(_))
    ));

    // 2. Permission comes before the input is looked at.
    let chat = || agent_caller(&["everyone", "conversation"]);
    let denied = f
        .runner
        .start(chat(), "demo", "lead_only", json!({ "mode": 5 }));
    assert!(matches!(denied, Err(CallError::Forbidden(_))), "{denied:?}");
    assert!(
        denied
            .unwrap_err()
            .to_string()
            .contains("your groups: everyone, conversation")
    );
    // A person needs `human` in the tool's groups; `everyone` is for agents.
    let denied = f.start("agents_only", echo.clone());
    assert!(matches!(denied, Err(CallError::Forbidden(_))), "{denied:?}");
    assert!(
        denied.unwrap_err().to_string().contains("human"),
        "says why"
    );
    // An agent in a group the tool lists gets in; `human` alone is not an agent group.
    let lead = agent_caller(&["everyone", "lead"]);
    let accepted = f
        .runner
        .start(lead, "demo", "lead_only", json!({ "mode": "version" }))
        .unwrap();
    // (It holds the one allowed slot until it ends.)
    f.runner.wait(&accepted.call_id).await.unwrap();
    assert!(
        f.runner
            .start(chat(), "demo", "humans_only", echo.clone())
            .is_err()
    );

    // 3. The input fits the schema, and that comes before the concurrency limit: the one
    // allowed slot is taken by a call that sleeps.
    let busy = f
        .start("run", json!({ "mode": "sleep", "ms": 5000 }))
        .unwrap();
    for bad in [
        json!({ "mode": 5 }),
        json!({}),
        json!({ "mode": "echo", "extra": 1 }),
        json!("text"),
    ] {
        let err = f.start("run", bad.clone()).unwrap_err();
        assert!(matches!(err, CallError::InvalidInput(_)), "{bad}: {err:?}");
        assert!(err.to_string().contains("demo__run"), "{err}");
    }
    // And permission before that, too.
    assert!(matches!(
        f.runner.start(chat(), "demo", "lead_only", echo.clone()),
        Err(CallError::Forbidden(_))
    ));

    // 4. Only now the limit: refused on the spot, not queued.
    let started = Instant::now();
    let err = f.start("run", echo.clone()).unwrap_err();
    assert!(matches!(err, CallError::Busy(_)), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(1));
    f.runner.cancel(&busy.call_id).await.unwrap();

    // Disabled: refused again, and no process starts.
    f.registry.disable("demo").unwrap();
    assert!(matches!(
        f.start("run", echo),
        Err(CallError::NotEnabled(_))
    ));
}

#[tokio::test]
async fn extensions_a_call_that_runs_too_long_is_killed_with_its_children() {
    let f = Fixture::enabled(Limits::default());
    let started = Instant::now();
    let done = f.call("slow", json!({ "mode": "tree" })).await;
    assert_eq!(done.status, ExtensionCallStatus::TimedOut, "{done:?}");
    assert!(done.error.unwrap().contains("timed out after 1s"));
    assert!(done.output.is_none());
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "killed at once"
    );

    // The child it started wrote a heartbeat until the tree ended; now it writes nothing.
    let beat = f.data().join("heartbeat");
    let before = fs::metadata(&beat).expect("the child ran").len();
    assert!(before > 0);
    tokio::time::sleep(Duration::from_millis(700)).await;
    let after = fs::metadata(&beat).unwrap().len();
    assert!(after <= before + 1, "still alive: {before} -> {after}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(fs::metadata(&beat).unwrap().len(), after, "still alive");
}

#[tokio::test]
async fn extensions_output_over_one_megabyte_is_an_error_and_the_process_is_killed() {
    let f = Fixture::enabled(Limits::default());
    let started = Instant::now();
    let done = f.call("run", json!({ "mode": "big" })).await;
    assert_eq!(done.status, ExtensionCallStatus::Failed, "{done:?}");
    assert!(done.error.unwrap().contains("more than 1048576 bytes"));
    // Never a cut-off value.
    assert!(done.output.is_none());
    // The tool would have slept for 30 s.
    assert!(started.elapsed() < Duration::from_secs(15));
}

#[tokio::test]
async fn extensions_stderr_keeps_the_last_64k_and_the_log_all_of_it_rolling_over() {
    let f = Fixture::enabled(Limits {
        log_roll: 2 * 1024 * 1024,
        ..Limits::default()
    });
    let done = f.call("run", json!({ "mode": "stderr" })).await;
    assert_eq!(done.status, ExtensionCallStatus::Ok, "{done:?}");
    let stderr = done.stderr.unwrap();
    assert!(stderr.len() <= 64 * 1024, "{} bytes", stderr.len());
    assert!(stderr.len() > 60 * 1024);
    assert!(stderr.trim_end().contains("line 019999"));
    assert!(!stderr.contains("line 000000"));

    // The log has everything (one call is about 1.1 MB).
    let log = f.home.path().join("extension-logs/demo.log");
    let first = fs::read_to_string(&log).unwrap();
    assert!(first.contains("line 000000") && first.contains("line 019999"));
    assert!(first.contains("demo__run") && first.contains("ok in"));
    // The next call would pass 2 MB: the file rolls over instead of growing.
    f.call("run", json!({ "mode": "stderr" })).await;
    f.call("run", json!({ "mode": "stderr" })).await;
    let rolled = f.home.path().join("extension-logs/demo.log.1");
    assert!(rolled.is_file(), "no rolled file");
    assert!(fs::metadata(&log).unwrap().len() <= 2 * 1024 * 1024 + 8 * 1024);
}

#[tokio::test]
async fn extensions_a_failing_tool_returns_its_stderr_as_the_error() {
    let f = Fixture::enabled(Limits::default());
    let done = f
        .call("run", json!({ "mode": "fail", "why": "disk on fire" }))
        .await;
    assert_eq!(done.status, ExtensionCallStatus::Failed);
    assert_eq!(done.error.as_deref(), Some("boom: disk on fire"));
    assert!(done.stderr.unwrap().contains("disk on fire"));
    assert!(done.output.is_none());

    for mode in ["badjson", "empty"] {
        let done = f.call("run", json!({ "mode": mode })).await;
        assert_eq!(done.status, ExtensionCallStatus::Failed, "{mode}");
        assert!(done.error.unwrap().contains("not JSON"), "{mode}");
    }
    let done = f.call("run", json!({ "mode": "nonsense" })).await;
    assert_eq!(done.status, ExtensionCallStatus::Failed);
    assert!(done.error.unwrap().contains("unknown mode"));
}

#[tokio::test]
async fn extensions_busy_is_reported_at_once_per_extension_and_globally() {
    let f = Fixture::enabled(Limits::default());
    let sleeper = || json!({ "mode": "sleep", "ms": 8000 });
    let running: Vec<_> = (0..4)
        .map(|_| f.start("run", sleeper()).expect("room for four"))
        .collect();
    let started = Instant::now();
    let err = f.start("run", sleeper()).unwrap_err();
    assert!(matches!(&err, CallError::Busy(m) if m.contains("demo") && m.contains("4")));
    assert!(started.elapsed() < Duration::from_secs(1), "not queued");
    // The slot comes back when a call ends.
    f.runner.cancel(&running[0].call_id).await.unwrap();
    let again = f.start("run", json!({ "mode": "echo" })).unwrap();
    assert_eq!(
        f.runner.wait(&again.call_id).await.unwrap().status,
        ExtensionCallStatus::Ok
    );
    for call in &running[1..] {
        f.runner.cancel(&call.call_id).await.unwrap();
    }

    // Across extensions: two fit, the third is turned away whoever it belongs to.
    let g = Fixture::new(Limits {
        global: 2,
        ..Limits::default()
    });
    write_extension(g.home.path(), "other", "read", "x", "");
    g.registry.scan().unwrap();
    approve(&g.registry, "demo");
    approve(&g.registry, "other");
    let one = g.start("run", sleeper()).unwrap();
    let two = g
        .runner
        .start(Caller::Human, "other", "run", sleeper())
        .unwrap();
    let err = g
        .runner
        .start(Caller::Human, "other", "run", sleeper())
        .unwrap_err();
    assert!(matches!(&err, CallError::Busy(m) if m.contains("2 extension tool calls")));
    for call in [one, two] {
        g.runner.cancel(&call.call_id).await.unwrap();
    }
}

#[tokio::test]
async fn extensions_changed_files_keep_the_old_version_running_until_approved() {
    let f = Fixture::enabled(Limits::default());
    let first = f.registry.enabled("demo").unwrap().version.fingerprint;
    let version =
        |r: ExtensionCallResponse| r.output.unwrap()["version"].as_str().unwrap().to_owned();

    // A call that is still running when the version changes.
    let long = f
        .start("run", json!({ "mode": "sleep", "ms": 1500 }))
        .unwrap();
    assert_eq!(long.fingerprint, first);

    // The source changes: the next call still runs the approved snapshot.
    fs::write(f.home.path().join("extensions/demo/version.txt"), "two").unwrap();
    f.registry.scan().unwrap();
    let second = pending(&f.registry, "demo");
    assert_ne!(first, second);
    let done = f.call("run", json!({ "mode": "version" })).await;
    assert_eq!(version(done.clone()), "one");
    assert_eq!(done.fingerprint, first);

    // Approved: the next call runs the new code, and the old call finishes on the old one.
    f.registry.enable("demo", &second).unwrap();
    let done = f.call("run", json!({ "mode": "version" })).await;
    assert_eq!(version(done.clone()), "two");
    assert_eq!(done.fingerprint, second);
    let old_path = f.home.path().join("extension-versions/demo").join(&first);
    assert!(old_path.is_dir(), "the running call keeps its snapshot");
    let ended = f.runner.wait(&long.call_id).await.unwrap();
    assert_eq!(ended.status, ExtensionCallStatus::Ok);
    assert_eq!(ended.fingerprint, first, "a switch does not move a call");

    // Auto-trust: the file changes and the very next call (after the poll) runs it.
    f.registry.set_auto_trust("demo", true).unwrap();
    fs::write(f.home.path().join("extensions/demo/version.txt"), "three!").unwrap();
    f.registry.scan().unwrap();
    let done = f.call("run", json!({ "mode": "version" })).await;
    assert_eq!(version(done), "three!");
}

#[tokio::test]
async fn extensions_disabling_ends_running_calls_after_the_grace_period() {
    let f = Fixture::enabled(Limits {
        disable_grace: Duration::from_millis(400),
        ..Limits::default()
    });
    let running = f
        .start("run", json!({ "mode": "sleep", "ms": 30_000 }))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    f.registry.disable("demo").unwrap();
    // New calls are refused at once ...
    assert!(matches!(
        f.start("run", json!({ "mode": "echo" })),
        Err(CallError::NotEnabled(_))
    ));
    // ... the running one is ended once the grace is over, long before it would have ended.
    let started = Instant::now();
    let done = f.runner.wait(&running.call_id).await.unwrap();
    assert_eq!(done.status, ExtensionCallStatus::Cancelled, "{done:?}");
    assert!(done.error.unwrap().contains("disabled"));
    assert!(started.elapsed() >= Duration::from_millis(100));
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn extensions_cancelling_and_stopping_the_hub_end_calls() {
    let f = Fixture::enabled(Limits::default());
    let one = f
        .start("run", json!({ "mode": "sleep", "ms": 30_000 }))
        .unwrap();
    let done = f.runner.cancel(&one.call_id).await.unwrap();
    assert_eq!(done.status, ExtensionCallStatus::Cancelled);
    // Cancelling something that ended is not an error: it answers as it ended.
    assert_eq!(
        f.runner.cancel(&one.call_id).await.unwrap().status,
        ExtensionCallStatus::Cancelled
    );
    assert!(f.runner.cancel("call-unknown").await.is_none());

    let two = f
        .start("run", json!({ "mode": "sleep", "ms": 30_000 }))
        .unwrap();
    let hold = f.start("run", json!({ "mode": "tree" })).unwrap();
    f.runner.shutdown().await;
    for call in [two, hold] {
        let done = f.runner.get(&call.call_id).unwrap();
        assert_eq!(done.status, ExtensionCallStatus::Cancelled, "{done:?}");
        assert!(done.error.unwrap().contains("hub is stopping"));
    }
    assert!(f.start("run", json!({ "mode": "echo" })).is_err());
    assert_eq!(f.runner.live_tokens(), 0);
}

#[tokio::test]
async fn extensions_results_are_kept_for_a_while_and_then_forgotten() {
    let f = Fixture::enabled(Limits {
        retention: Duration::from_millis(400),
        ..Limits::default()
    });
    let done = f.call("run", json!({ "mode": "echo" })).await;
    assert!(f.runner.get(&done.call_id).is_some());
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(f.runner.get(&done.call_id).is_none());
    // A call that is running is never forgotten.
    let running = f
        .start("run", json!({ "mode": "sleep", "ms": 1200 }))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        f.runner.get(&running.call_id).unwrap().status,
        ExtensionCallStatus::Running
    );
    f.runner.cancel(&running.call_id).await;
}

#[tokio::test]
async fn extensions_token_works_while_the_process_runs_and_only_that_long() {
    let f = Fixture::enabled(Limits::default());
    let running = f.start("run", json!({ "mode": "hold" })).unwrap();
    let file = f.data().join("token.txt");
    until("the tool to write its token", || file.is_file()).await;
    let token = fs::read_to_string(&file).unwrap();
    assert_eq!(
        f.runner.token(&token),
        Some(("demo".to_owned(), ExtensionAccess::Write))
    );
    fs::write(f.data().join("release"), "").unwrap();
    let done = f.runner.wait(&running.call_id).await.unwrap();
    assert_eq!(done.status, ExtensionCallStatus::Ok, "{done:?}");
    assert_eq!(f.runner.token(&token), None);
    // Every call gets its own.
    let again = f.call("run", json!({ "mode": "echo" })).await;
    assert_ne!(again.output.unwrap()["env"]["token"], token);
}

#[test]
fn extensions_a_manifest_cannot_be_named_calls() {
    let home = tempfile::tempdir().unwrap();
    write_extension(home.path(), "calls", "read", "x", "");
    let registry = Registry::load(home.path()).unwrap();
    let info = registry.get("calls").unwrap();
    assert!(info.pending.is_none());
    assert!(info.error.unwrap().contains("reserved"));
}

// ---------------------------------------------------------------------------------------
// With a hub

struct TestHub {
    hub: crossagents_hub::RunningHub,
    home: tempfile::TempDir,
    client: Client,
    base: String,
}

impl TestHub {
    /// A hub whose home already holds `demo` (write) and `reader` (read), both approved.
    async fn start() -> Self {
        let home = tempfile::tempdir().unwrap();
        write_extension(home.path(), "demo", "write", "one", "");
        write_extension(home.path(), "reader", "read", "one", "");
        let hub = crossagents_hub::start(HubConfig::new(Paths::new(home.path()), 0))
            .await
            .unwrap();
        for name in ["demo", "reader"] {
            hub.extensions.scan().unwrap();
            let fp = pending(&hub.extensions, name);
            hub.extensions.enable(name, &fp).unwrap();
        }
        let base = format!("http://{}", hub.addr);
        Self {
            hub,
            home,
            client: Client::new(),
            base,
        }
    }

    fn req(&self, method: reqwest::Method, path: &str, token: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(token)
    }

    async fn status(
        &self,
        method: reqwest::Method,
        path: &str,
        token: &str,
    ) -> (StatusCode, Value) {
        let resp = self
            .req(method, path, token)
            .json(&json!({ "input": { "mode": "echo" } }))
            .send()
            .await
            .unwrap();
        let status = resp.status();
        (status, resp.json().await.unwrap_or(Value::Null))
    }

    async fn call(&self, tool: &str, input: Value) -> (StatusCode, Value) {
        let resp = self
            .req(
                reqwest::Method::POST,
                &format!("/v1/extensions/demo/tools/{tool}"),
                &self.hub.token,
            )
            .json(&json!({ "input": input }))
            .send()
            .await
            .unwrap();
        let status = resp.status();
        (status, resp.json().await.unwrap_or(Value::Null))
    }

    async fn result(&self, id: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let body: Value = self
                .req(
                    reqwest::Method::GET,
                    &format!("/v1/extensions/calls/{id}"),
                    &self.hub.read_token,
                )
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if body["status"] != "running" {
                return body;
            }
            assert!(Instant::now() < deadline, "call never ended: {body}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[tokio::test]
async fn extensions_http_a_person_calls_asynchronously_and_cannot_choose_who_they_are() {
    let t = TestHub::start().await;
    let mut events = t.hub.bus.subscribe();

    // Only the body's `input` counts: a `caller` in it is ignored.
    let resp = t
        .req(
            reqwest::Method::POST,
            "/v1/extensions/demo/tools/run",
            &t.hub.token,
        )
        .json(&json!({
            "input": { "mode": "echo" },
            "caller": { "kind": "agent", "name": "someone else" },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let accepted: ExtensionCallAccepted = resp.json().await.unwrap();
    assert_eq!(accepted.extension, "demo");
    assert_eq!(accepted.tool, "run");
    assert_eq!(accepted.timeout_secs, 60);
    let done = t.result(&accepted.call_id).await;
    assert_eq!(done["status"], "ok", "{done}");
    assert_eq!(done["fingerprint"], accepted.fingerprint);
    assert_eq!(
        done["output"]["request"]["caller"],
        json!({ "kind": "human" })
    );

    // The event says it ended, and which snapshot it ran on.
    let finished = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let EventKind::ExtensionCallFinished {
                call_id,
                status,
                fingerprint,
                ..
            } = events.recv().await.unwrap().kind
                && call_id == accepted.call_id
            {
                return (status, fingerprint);
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(finished, ("ok".to_owned(), accepted.fingerprint.clone()));

    // A tool open only to agents is not the GUI's; a bad input is a 400; unknown things 404.
    let (status, body) = t.call("agents_only", json!({ "mode": "echo" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, _) = t.call("run", json!({ "mode": 3 })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = t.call("missing", json!({ "mode": "echo" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = t
        .status(
            reqwest::Method::GET,
            "/v1/extensions/calls/call-nope",
            &t.hub.token,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Reading the listing is read; calling is write.
    let (status, _) = t
        .status(
            reqwest::Method::POST,
            "/v1/extensions/demo/tools/run",
            &t.hub.read_token,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A slow one can be cancelled by its id.
    let (status, body) = t
        .call("run", json!({ "mode": "sleep", "ms": 30_000 }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = body["call_id"].as_str().unwrap().to_owned();
    assert_eq!(t.result_status_now(&id).await, "running");
    let cancelled: Value = t
        .req(
            reqwest::Method::DELETE,
            &format!("/v1/extensions/calls/{id}"),
            &t.hub.token,
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(t.result(&id).await["status"], "cancelled");

    // And a timeout is reported as one.
    let (_, body) = t
        .call("slow", json!({ "mode": "sleep", "ms": 30_000 }))
        .await;
    let done = t.result(body["call_id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "timed_out", "{done}");
}

impl TestHub {
    async fn result_status_now(&self, id: &str) -> String {
        let body: Value = self
            .req(
                reqwest::Method::GET,
                &format!("/v1/extensions/calls/{id}"),
                &self.hub.read_token,
            )
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        body["status"].as_str().unwrap().to_owned()
    }
}

#[tokio::test]
async fn extensions_http_a_tool_token_cannot_reach_admin_me_or_the_person_route() {
    let t = TestHub::start().await;
    let hold = |name: &str| {
        let data = t.home.path().join("extension-data").join(name);
        (data.join("token.txt"), data.join("release"))
    };
    // `demo` is write, `reader` is read; both are held open so their tokens are live.
    let (_, demo) = t.call("run", json!({ "mode": "hold" })).await;
    let reader = {
        let resp = t
            .req(
                reqwest::Method::POST,
                "/v1/extensions/reader/tools/run",
                &t.hub.token,
            )
            .json(&json!({ "input": { "mode": "hold" } }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        resp.json::<Value>().await.unwrap()
    };
    let (demo_token, demo_release) = hold("demo");
    let (reader_token, reader_release) = hold("reader");
    until("both tokens", || {
        demo_token.is_file() && reader_token.is_file()
    })
    .await;
    let write = fs::read_to_string(&demo_token).unwrap();
    let read = fs::read_to_string(&reader_token).unwrap();
    assert!(write.starts_with("ca-ext-") && read.starts_with("ca-ext-"));

    use reqwest::Method as M;
    // The level it declared works ...
    assert_eq!(
        t.status(M::GET, "/v1/agents", &write).await.0,
        StatusCode::OK
    );
    assert_eq!(
        t.status(M::GET, "/v1/agents", &read).await.0,
        StatusCode::OK
    );
    // ... read cannot write, and the refusal says whose token it was.
    let (status, body) = t.status(M::POST, "/v1/agents", &read).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let why = body["error"]["message"].as_str().unwrap();
    assert!(
        why.contains("reader") && why.contains("needs write"),
        "{why}"
    );
    // Neither reaches admin: shutdown, trust, terminals, accounts.
    for (method, path) in [
        (M::POST, "/v1/shutdown"),
        (M::POST, "/v1/extensions/demo/enable"),
        (M::POST, "/v1/extensions/demo/auto-trust"),
        (M::GET, "/v1/terminals"),
        (M::POST, "/v1/accounts"),
    ] {
        for token in [&write, &read] {
            let (status, body) = t.status(method.clone(), path, token).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {body}");
        }
    }
    // /v1/me is for agents' session tokens: an extension's is simply not one.
    for (method, path) in [
        (M::GET, "/v1/me"),
        (M::GET, "/v1/me/tools"),
        (M::POST, "/v1/me/tools/whoami"),
        (M::POST, "/v1/me/tools/extension_call"),
    ] {
        for token in [&write, &read] {
            let (status, _) = t.status(method.clone(), path, token).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
        }
    }
    // The route that calls a tool as a person is shut to it, even at write level: otherwise
    // it could call a tool only open to `human` as a person.
    let (status, body) = t
        .status(M::POST, "/v1/extensions/demo/tools/humans_only", &write)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body["error"]["message"].as_str().unwrap().contains("demo"));
    let (status, _) = t
        .status(M::POST, "/v1/extensions/demo/tools/run", &write)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // And the person's own credential does reach it.
    let (status, _) = t
        .status(
            M::POST,
            "/v1/extensions/demo/tools/humans_only",
            &t.hub.token,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "reaches the tool");

    // When the process ends the token is worth nothing.
    fs::write(demo_release, "").unwrap();
    fs::write(reader_release, "").unwrap();
    assert_eq!(
        t.result(demo["call_id"].as_str().unwrap()).await["status"],
        "ok"
    );
    assert_eq!(
        t.result(reader["call_id"].as_str().unwrap()).await["status"],
        "ok"
    );
    for token in [&write, &read] {
        let (status, _) = t.status(M::GET, "/v1/agents", token).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "after the process ended");
    }
}

fn names(tools: &[crossagents_core::ToolInfo]) -> Vec<&str> {
    tools.iter().map(|t| t.name.as_str()).collect()
}

#[tokio::test]
async fn extensions_http_a_running_session_calls_a_tool_enabled_after_it_started() {
    // The hub starts with no extension at all.
    let home = tempfile::tempdir().unwrap();
    let hub = crossagents_hub::start(HubConfig::new(Paths::new(home.path()), 0))
        .await
        .unwrap();
    let base = format!("http://{}", hub.addr);
    let client = Client::new();
    let resp = client
        .post(format!("{base}/v1/agents"))
        .bearer_auth(&hub.token)
        .json(&json!({
            "name": "alice", "plugin": "claude", "model": "haiku", "role": "worker",
            "cwd": home.path().display().to_string(),
        }))
        .send()
        .await
        .unwrap();
    let alice: Agent = resp.json().await.unwrap();
    // The session's MCP server asks the hub with this token; that was set when it started.
    let session = hub.agent_tokens.issue(alice.id).await;
    let list = |client: &Client| {
        client
            .get(format!("{base}/v1/me/tools"))
            .bearer_auth(&session)
            .send()
    };
    let before: Value = list(&client).await.unwrap().json().await.unwrap();
    let before: Vec<&str> = before["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(before.contains(&"extension_tools") && before.contains(&"extension_call"));
    assert!(!before.iter().any(|n| n.contains("__")));

    // A tool is written and approved now, without restarting the hub or the session.
    write_extension(home.path(), "demo", "write", "one", "");
    hub.extensions.scan().unwrap();
    let fp = pending(&hub.extensions, "demo");
    // Not yet approved: it is not offered, and cannot be called.
    let call = |tool: &'static str, input: Value| {
        let (client, base, session) = (client.clone(), base.clone(), session.clone());
        async move {
            let resp = client
                .post(format!("{base}/v1/me/tools/extension_call"))
                .bearer_auth(&session)
                .json(&json!({ "tool": tool, "input": input }))
                .send()
                .await
                .unwrap();
            (
                resp.status(),
                resp.json::<Value>().await.unwrap_or(Value::Null),
            )
        }
    };
    let (status, body) = call("demo__run", json!({ "mode": "echo" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not enabled")
    );
    hub.extensions.enable("demo", &fp).unwrap();

    // The running session lists what it may call, then calls it through `extension_call`.
    let (_, listed) = {
        let resp = client
            .post(format!("{base}/v1/me/tools/extension_tools"))
            .bearer_auth(&session)
            .send()
            .await
            .unwrap();
        (resp.status(), resp.json::<Value>().await.unwrap())
    };
    let listed: Vec<&str> = listed["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    // A conversation is in `everyone` and `conversation`: not the lead's or the person's.
    assert_eq!(listed, ["demo__run", "demo__slow", "demo__agents_only"]);
    let (status, body) = call("demo__run", json!({ "mode": "echo" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let caller = &body["request"]["caller"];
    assert_eq!(caller["kind"], "agent");
    assert_eq!(caller["name"], "alice");
    assert_eq!(caller["id"], alice.id.to_string());
    assert!(
        caller["groups"]
            .as_array()
            .unwrap()
            .contains(&json!("conversation"))
    );
    // A new tools/list shows it directly, and a direct call is the same thing.
    let after: Value = list(&client).await.unwrap().json().await.unwrap();
    assert!(
        after["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "demo__run")
    );
    let direct = client
        .post(format!("{base}/v1/me/tools/demo__run"))
        .bearer_auth(&session)
        .json(&json!({ "mode": "version" }))
        .send()
        .await
        .unwrap();
    assert_eq!(direct.status(), StatusCode::OK);
    assert_eq!(direct.json::<Value>().await.unwrap()["version"], "one");

    // Every route checks again, whatever the list showed.
    let (status, body) = call("demo__lead_only", json!({ "mode": "echo" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not available to you")
    );
    let (status, _) = call("demo__humans_only", json!({ "mode": "echo" })).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, body) = call("demo__run", json!({ "mode": 1 })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("schema")
    );
    let (status, _) = call("nope__run", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call("run", json!({})).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "not an extension tool name"
    );
    // A tool that fails tells the model why.
    let (status, body) = call("demo__run", json!({ "mode": "fail", "why": "no luck" })).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no luck")
    );

    // Straight through the orchestrator too: the list the model gets is the same.
    let tools = hub.orchestrator.tools_for(&alice).await;
    let tools = names(&tools);
    assert!(tools.contains(&"demo__run") && !tools.contains(&"demo__lead_only"));
    assert!(matches!(
        hub.orchestrator
            .call_tool(&alice, "demo__lead_only", json!({ "mode": "echo" }))
            .await,
        Err(CoreError::Conflict(_))
    ));

    // After disabling, the session cannot call it any more, and nothing is listed.
    hub.extensions.disable("demo").unwrap();
    let (status, _) = call("demo__run", json!({ "mode": "echo" })).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let tools = hub.orchestrator.tools_for(&alice).await;
    assert!(!names(&tools).iter().any(|n| n.contains("__")));
}

#[tokio::test]
async fn extensions_prompts_are_added_to_new_sessions_by_group_and_only_added() {
    let home = tempfile::tempdir().unwrap();
    write_extension(
        home.path(),
        "demo",
        "read",
        "one",
        r#"
[[prompts]]
groups = ["lead"]
file = "lead.md"

[[prompts]]
groups = ["conversation"]
file = "chat.md"

[[prompts]]
groups = ["everyone"]
file = "all.md"
"#,
    );
    let root = home.path().join("extensions/demo");
    fs::write(root.join("lead.md"), "LEAD-ONLY-RULE").unwrap();
    fs::write(root.join("chat.md"), "CHAT-RULE").unwrap();
    fs::write(root.join("all.md"), "ALL-RULE").unwrap();
    let hub = crossagents_hub::start(HubConfig::new(Paths::new(home.path()), 0))
        .await
        .unwrap();
    use crossagents_orchestrator::messaging::Group;
    let chat = [Group::Everyone, Group::Conversation];
    let lead = [Group::Everyone, Group::Lead];

    // Not approved: nothing is added.
    assert_eq!(hub.orchestrator.extension_prompts(&chat), "");
    let fp = pending(&hub.extensions, "demo");
    hub.extensions.enable("demo", &fp).unwrap();
    // In the order the description lists them, for the groups the session holds.
    assert_eq!(
        hub.orchestrator.extension_prompts(&chat),
        "CHAT-RULE\n\nALL-RULE"
    );
    assert_eq!(
        hub.orchestrator.extension_prompts(&lead),
        "LEAD-ONLY-RULE\n\nALL-RULE"
    );
    // They follow the hub's own text and leave it alone.
    let hub_text = crossagents_orchestrator::protocol::guide(Role::Worker);
    let both = crossagents_orchestrator::protocol::with_extension_prompts(
        hub_text.clone(),
        &hub.orchestrator.extension_prompts(&chat),
    );
    assert!(both.starts_with(&hub_text));
    assert!(both.ends_with("CHAT-RULE\n\nALL-RULE"));

    // A changed file is not read until the new version is approved: the snapshot is.
    fs::write(root.join("chat.md"), "CHAT-RULE-2").unwrap();
    hub.extensions.scan().unwrap();
    assert!(
        hub.orchestrator
            .extension_prompts(&chat)
            .contains("CHAT-RULE\n")
    );
    let fp = pending(&hub.extensions, "demo");
    hub.extensions.enable("demo", &fp).unwrap();
    assert!(
        hub.orchestrator
            .extension_prompts(&chat)
            .contains("CHAT-RULE-2")
    );
    hub.extensions.disable("demo").unwrap();
    assert_eq!(hub.orchestrator.extension_prompts(&chat), "");
}

#[tokio::test]
async fn extensions_a_child_holding_only_stderr_open_does_not_lose_stdout_or_hang_the_call() {
    let f = Fixture::enabled(Limits::default());
    let started = Instant::now();
    let accepted = f.start("run", json!({ "mode": "stderr_child" })).unwrap();
    let done = tokio::time::timeout(Duration::from_secs(15), f.runner.wait(&accepted.call_id))
        .await
        .expect("the call must end")
        .unwrap();
    assert_eq!(done.status, ExtensionCallStatus::Ok, "{done:?}");
    assert_eq!(done.output, Some(json!({ "answer": 42 })));
    assert_eq!(
        f.runner.get(&accepted.call_id).unwrap().status,
        ExtensionCallStatus::Ok
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(f.runner.live_tokens(), 0);
    // The leftover child went with the process tree.
    let beat = f.data().join("heartbeat");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let size = fs::metadata(&beat).map(|m| m.len()).unwrap_or(0);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        fs::metadata(&beat).map(|m| m.len()).unwrap_or(0),
        size,
        "still alive"
    );
}

#[tokio::test]
async fn extensions_http_stopping_the_hub_ends_a_tool_a_request_is_waiting_for() {
    let t = TestHub::start().await;
    let TestHub {
        hub,
        home,
        client,
        base,
    } = t;
    let resp = client
        .post(format!("{base}/v1/agents"))
        .bearer_auth(&hub.token)
        .json(&json!({
            "name": "alice", "plugin": "claude", "model": "haiku", "role": "worker",
            "cwd": home.path().display().to_string(),
        }))
        .send()
        .await
        .unwrap();
    let alice: Agent = resp.json().await.unwrap();
    let session = hub.agent_tokens.issue(alice.id).await;
    let runner = hub.extension_runner.clone();
    // The agent's request waits for the tool, which runs (with a child) for two minutes.
    let waiting = tokio::spawn({
        let (client, base) = (client.clone(), base.clone());
        async move {
            client
                .post(format!("{base}/v1/me/tools/demo__run"))
                .bearer_auth(&session)
                .json(&json!({ "mode": "tree" }))
                .send()
                .await
        }
    });
    let beat = home.path().join("extension-data/demo/heartbeat");
    until("the tool's child to start", || {
        beat.is_file() && runner.live_tokens() == 1
    })
    .await;

    let stopping = Instant::now();
    hub.shutdown();
    tokio::time::timeout(Duration::from_secs(20), hub.wait())
        .await
        .expect("the hub must stop without waiting for the tool")
        .unwrap();
    assert!(stopping.elapsed() < Duration::from_secs(20));
    assert_eq!(runner.live_tokens(), 0);
    // The request got an answer (an error), and the tool's tree is gone.
    if let Ok(Ok(resp)) = tokio::time::timeout(Duration::from_secs(5), waiting).await {
        if let Ok(resp) = resp {
            assert!(!resp.status().is_success());
        }
    }
    let size = fs::metadata(&beat).unwrap().len();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        fs::metadata(&beat).unwrap().len(),
        size,
        "the tool is still running"
    );
}

/// Hooks and services run through the same runner: the snapshot, the tree, the token and —
/// when asked — the tool limits, without the tool's group and schema checks and without the
/// stdin/stdout protocol mattering (docs/EXTENSIONS.md §3).
#[tokio::test]
async fn extensions_start_command_shares_the_limits_and_ends_its_token() {
    let f = Fixture::new(Limits {
        per_extension: 1,
        ..Limits::default()
    });
    approve(&f.registry, "demo");
    let cancel = tokio_util::sync::CancellationToken::new();
    let start = |mode: &str, limits: bool| {
        let lease = f.registry.acquire("demo").unwrap();
        f.runner.start_command(CommandSpec {
            extension: "demo".into(),
            lease,
            command: vec![tool_exe().display().to_string()],
            stdin: Some(format!(r#"{{"input":{{"mode":"{mode}"}}}}"#).into_bytes()),
            timeout: Some(Duration::from_secs(10)),
            stdout: OutputCapture::Collect,
            limits,
            label: "hook test".into(),
            cancel: cancel.clone(),
            disabled: None,
        })
    };

    let run = start("echo", true).unwrap();
    let done = run.wait().await;
    assert!(done.ok(), "{done:?}");
    let out: Value = serde_json::from_slice(&done.stdout).unwrap();
    assert_eq!(out["request"]["input"]["mode"], "echo");
    // The token died with the process, before the result was visible.
    assert_eq!(f.runner.live_tokens(), 0);

    // A hook takes a tool slot: with one per extension, the next one is dropped as busy.
    let held = start("hold", true).unwrap();
    let busy = start("echo", true).unwrap_err();
    assert!(matches!(busy, CallError::Busy(_)), "{busy:?}");
    // The token lives exactly while the process does.
    until("the held process", || f.data().join("token.txt").is_file()).await;
    let token = fs::read_to_string(f.data().join("token.txt")).unwrap();
    assert_eq!(f.runner.token(&token).unwrap().0, "demo");
    fs::write(f.data().join("release"), b"x").unwrap();
    assert!(held.wait().await.ok());
    assert!(f.runner.token(&token).is_none());
}

// ---------------------------------------------------------------------------------------
// Hooks and services (docs/EXTENSIONS.md §3)

/// A `[service]` that never exits, so a test can see whether it is still alive.
fn service_extension(home: &Path, name: &str) {
    write_extension(
        home,
        name,
        "write",
        "one",
        &format!(
            "\n[service]\ncommand = ['{}', 'heartbeat']\n",
            tool_exe().display()
        ),
    );
}

fn lines(path: &Path) -> usize {
    fs::read_to_string(path)
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

/// Disabling an extension must end a hook that is still running, not leave it (and its
/// token, lease and concurrency slot) until its 60 s timeout.
#[tokio::test]
async fn extensions_disabling_ends_a_running_hook_and_its_token() {
    let home = tempfile::tempdir().unwrap();
    write_extension(
        home.path(),
        "demo",
        "write",
        "one",
        &format!(
            "\n[[on]]\nevent = \"hub.started\"\ncommand = ['{}', 'heartbeat']\ntimeout_secs = 60\n",
            tool_exe().display()
        ),
    );
    let hub = crossagents_hub::start(HubConfig::new(Paths::new(home.path()), 0))
        .await
        .unwrap();
    hub.extensions.scan().unwrap();
    let fp = pending(&hub.extensions, "demo");
    hub.extensions.enable("demo", &fp).unwrap();
    // The hub is already running, so this is a new event, not the startup one.
    hub.bus
        .publish(EventKind::HubStarted {
            version: "test".into(),
        })
        .await
        .unwrap();

    let runner = hub.extension_runner.clone();
    until("the hook to start", || runner.live_tokens() == 1).await;
    let beat = home.path().join("extension-data/demo/heartbeat");
    until("the hook's heartbeat", || beat.is_file()).await;

    hub.extensions.disable("demo").unwrap();
    // The same grace a tool call gets, not the hook's 60 s timeout.
    tokio::time::timeout(
        Duration::from_secs(9),
        until("the hook to end", || runner.live_tokens() == 0),
    )
    .await
    .expect("disabling must end the hook");
    let size = fs::metadata(&beat).unwrap().len();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        fs::metadata(&beat).unwrap().len(),
        size,
        "the hook is still running"
    );
}

/// A hub that stops in the middle of a disable's grace period still takes the service down.
#[tokio::test]
async fn extensions_shutdown_during_the_disable_grace_still_stops_a_service() {
    let home = tempfile::tempdir().unwrap();
    service_extension(home.path(), "demo");
    let registry = Arc::new(Registry::load(home.path()).unwrap());
    let runner = Runner::new(registry.clone(), None, home.path().to_owned());
    approve(&registry, "demo");
    let services = Services::new(runner.clone(), registry.clone());
    let shutdown = tokio_util::sync::CancellationToken::new();
    let task = services.start(shutdown.clone());
    until("the service to start", || runner.live_tokens() == 1).await;
    let beat = home.path().join("extension-data/demo/heartbeat");
    until("the service's heartbeat", || beat.is_file()).await;

    registry.disable("demo").unwrap();
    // The 5 s grace has just begun; the hub goes away in the middle of it.
    tokio::time::sleep(Duration::from_millis(150)).await;
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(8), task)
        .await
        .expect("services must stop with the hub")
        .unwrap();
    assert_eq!(runner.live_tokens(), 0, "the service outlived the hub");
    let size = fs::metadata(&beat).unwrap().len();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        fs::metadata(&beat).unwrap().len(),
        size,
        "the service is still running"
    );
}

/// Two extensions disabled one after the other wait out their graces side by side: one
/// extension's grace must not add to another's.
#[tokio::test]
async fn extensions_disabling_two_services_waits_out_their_graces_in_parallel() {
    let home = tempfile::tempdir().unwrap();
    service_extension(home.path(), "demo");
    service_extension(home.path(), "beta");
    let registry = Arc::new(Registry::load(home.path()).unwrap());
    let runner = Runner::new(registry.clone(), None, home.path().to_owned());
    for name in ["demo", "beta"] {
        approve(&registry, name);
    }
    let services = Services::new(runner.clone(), registry.clone());
    let shutdown = tokio_util::sync::CancellationToken::new();
    let task = services.start(shutdown.clone());
    until("both services", || runner.live_tokens() == 2).await;

    let started = Instant::now();
    registry.disable("demo").unwrap();
    registry.disable("beta").unwrap();
    // A 5 s grace each, at the same time; in series the second would end at about 10 s.
    tokio::time::timeout(
        Duration::from_secs(6),
        until("both services to end", || runner.live_tokens() == 0),
    )
    .await
    .expect("the two graces must run at the same time");
    assert!(started.elapsed() < Duration::from_secs(6));
    shutdown.cancel();
    let _ = task.await;
}

/// A hook that goes through the write API can feed itself (`agent.updated` -> change the
/// topic -> `agent.updated`). `extension.*` filtering does not catch that, so the circuit
/// breaker bounds it: at most the configured number of starts per window, one log line, and
/// nothing more once the extension is disabled.
#[tokio::test]
async fn extensions_a_hook_loop_through_the_write_api_is_capped_by_the_breaker() {
    #[cfg(windows)]
    let (command, script, text) = (
        r#"["powershell", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", "hook.ps1"]"#,
        "hook.ps1",
        r#"$raw = [Console]::In.ReadToEnd()
$e = $raw | ConvertFrom-Json
$dir = $env:CROSSAGENTS_EXTENSION_DATA
Add-Content -LiteralPath (Join-Path $dir 'starts') -Value 'x'
$id = $e.event.data.agent.id
$body = @{ topic = [guid]::NewGuid().ToString() } | ConvertTo-Json -Compress
try {
  Invoke-RestMethod -Method Post -Uri "$env:CROSSAGENTS_URL/v1/agents/$id/topic" -Headers @{ Authorization = "Bearer $env:CROSSAGENTS_TOKEN" } -ContentType 'application/json' -Body $body | Out-Null
} catch { }
"#,
    );
    #[cfg(not(windows))]
    let (command, script, text) = (
        r#"["sh", "hook.sh"]"#,
        "hook.sh",
        r#"raw=$(cat)
dir="$CROSSAGENTS_EXTENSION_DATA"
echo x >> "$dir/starts"
id=$(printf '%s' "$raw" | grep -o '"agent":{"id":"[^"]*"' | head -n 1 | cut -d'"' -f6)
curl -s -o /dev/null -X POST -H "Authorization: Bearer $CROSSAGENTS_TOKEN" -H 'Content-Type: application/json' --data "{\"topic\":\"loop-$$-$(date +%s%N)\"}" "$CROSSAGENTS_URL/v1/agents/$id/topic"
"#,
    );
    #[cfg(not(windows))]
    if std::process::Command::new("curl")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: curl is not available");
        return;
    }

    let home = tempfile::tempdir().unwrap();
    write_extension(
        home.path(),
        "demo",
        "write",
        "one",
        &format!("\n[[on]]\nevent = \"agent.updated\"\ncommand = {command}\ntimeout_secs = 60\n"),
    );
    fs::write(home.path().join("extensions/demo").join(script), text).unwrap();
    let hub = crossagents_hub::start(HubConfig::new(Paths::new(home.path()), 0))
        .await
        .unwrap();
    // A small limit so the loop settles quickly; the rule is the same at 60 per minute.
    hub.extension_hooks.set_rate(HookRate {
        limit: 5,
        window: Duration::from_secs(60),
    });
    hub.extensions.scan().unwrap();
    let fp = pending(&hub.extensions, "demo");
    hub.extensions.enable("demo", &fp).unwrap();

    // The hook changes this agent's topic, and that is what feeds it again.
    let alice: Agent = Client::new()
        .post(format!("http://{}/v1/agents", hub.addr))
        .bearer_auth(&hub.token)
        .json(&json!({
            "name": "alice", "plugin": "claude", "model": "haiku", "role": "worker",
            "cwd": home.path().display().to_string(),
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    hub.bus
        .publish(EventKind::AgentUpdated { agent: alice })
        .await
        .unwrap();

    let starts = home.path().join("extension-data/demo/starts");
    until("the hook to start", || lines(&starts) >= 1).await;
    // Wait for the breaker itself, rather than assuming a fixed time for five
    // PowerShell processes and HTTP requests under parallel test load.
    let log_path = home.path().join("extension-logs/demo.log");
    tokio::time::timeout(Duration::from_secs(45), until("the breaker to trip", || {
        fs::read_to_string(&log_path)
            .is_ok_and(|log| log.contains("circuit breaker"))
    }))
    .await
    .expect("the loop must reach the breaker");
    until("all admitted hooks to finish", || hub.extension_runner.live_tokens() == 0).await;
    let capped = lines(&starts);
    assert!(capped >= 2, "the loop never ran: {capped} starts");
    assert!(capped <= 5, "the breaker let {capped} hooks start");
    // It is a breaker, not a delay: still no more starts later in the same window.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(lines(&starts), capped, "the breaker is not holding");

    let log = fs::read_to_string(log_path).unwrap();
    assert!(log.contains("circuit breaker"), "{log}");
    assert_eq!(
        log.matches("circuit breaker").count(),
        1,
        "one line per window"
    );

    hub.extensions.disable("demo").unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        lines(&starts),
        capped,
        "a disabled extension still ran a hook"
    );
}

/// A hook that only leaves a marker, for tests that count starts.
#[cfg(windows)]
fn marker_hook() -> (&'static str, &'static str, &'static str) {
    (
        r#"["powershell", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", "hook.ps1"]"#,
        "hook.ps1",
        "Add-Content -LiteralPath (Join-Path $env:CROSSAGENTS_EXTENSION_DATA 'starts') -Value 'x'\n",
    )
}
#[cfg(not(windows))]
fn marker_hook() -> (&'static str, &'static str, &'static str) {
    (
        r#"["sh", "hook.sh"]"#,
        "hook.sh",
        "echo x >> \"$CROSSAGENTS_EXTENSION_DATA/starts\"\n",
    )
}

/// The hook's order is acquire, then register with the runner. A disable in between cannot
/// be seen by the disable notification (the command is not registered yet), so the runner
/// checks the lease's generation after registering: the process must not start.
#[tokio::test]
async fn extensions_a_disable_between_the_lease_and_the_start_does_not_let_a_hook_run() {
    let f = Fixture::new(Limits {
        disable_grace: Duration::from_millis(100),
        ..Limits::default()
    });
    let fp = approve(&f.registry, "demo");
    let lease = f.registry.acquire("demo").unwrap();
    // Exactly the window: the lease is taken, then the extension is disabled, and only then
    // does the hook reach start_command with its own disabled token.
    f.registry.disable("demo").unwrap();
    // Even an immediate re-enable of the same snapshot cannot revive the old lease.
    f.registry.enable("demo", &fp).unwrap();
    let run = f.runner.start_command(CommandSpec {
        extension: "demo".into(),
        lease,
        command: vec![tool_exe().display().to_string(), "heartbeat".into()],
        stdin: None,
        timeout: Some(Duration::from_secs(60)),
        stdout: OutputCapture::LogOnly,
        limits: true,
        label: "hook hub.started".into(),
        cancel: tokio_util::sync::CancellationToken::new(),
        disabled: Some(tokio_util::sync::CancellationToken::new()),
    });
    assert!(
        run.is_err(),
        "a hook must not start on a lease from before the disable"
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(f.runner.live_tokens(), 0, "a hook outlived the disable");
}

/// Disabling ends a hook that is running; re-enabling does not revive it, but the next event
/// starts a new one.
#[tokio::test]
async fn extensions_a_hook_ends_on_disable_and_a_new_event_starts_after_reenable() {
    let home = tempfile::tempdir().unwrap();
    write_extension(
        home.path(),
        "demo",
        "write",
        "one",
        &format!(
            "\n[[on]]\nevent = \"hub.started\"\ncommand = ['{}', 'heartbeat']\ntimeout_secs = 60\n",
            tool_exe().display()
        ),
    );
    let hub = crossagents_hub::start(HubConfig::new(Paths::new(home.path()), 0))
        .await
        .unwrap();
    hub.extensions.scan().unwrap();
    let fp = pending(&hub.extensions, "demo");
    hub.extensions.enable("demo", &fp).unwrap();
    hub.bus
        .publish(EventKind::HubStarted {
            version: "test".into(),
        })
        .await
        .unwrap();
    let runner = hub.extension_runner.clone();
    until("the hook to start", || runner.live_tokens() == 1).await;
    let beat = home.path().join("extension-data/demo/heartbeat");
    until("the hook's heartbeat", || beat.is_file()).await;

    // Disable and enable again at once: the old hook still ends after its grace.
    hub.extensions.disable("demo").unwrap();
    hub.extensions.enable("demo", &fp).unwrap();
    tokio::time::timeout(
        Duration::from_secs(20),
        until("the old hook to end", || runner.live_tokens() == 0),
    )
    .await
    .expect("the old hook must end");

    // A new event is not charged against the old one and starts a fresh hook.
    hub.bus
        .publish(EventKind::HubStarted {
            version: "test".into(),
        })
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(9),
        until("a new hook to start", || runner.live_tokens() == 1),
    )
    .await
    .expect("a new event must start a hook after the re-enable");
}

/// Events the concurrency limits turn away never started, so they must not use up the
/// breaker's allowance.
#[tokio::test]
async fn extensions_busy_hook_events_do_not_use_up_the_breaker_allowance() {
    let (command, script, text) = marker_hook();
    let home = tempfile::tempdir().unwrap();
    write_extension(
        home.path(),
        "demo",
        "write",
        "one",
        &format!("\n[[on]]\nevent = \"hub.started\"\ncommand = {command}\ntimeout_secs = 60\n"),
    );
    fs::write(home.path().join("extensions/demo").join(script), text).unwrap();
    let hub = crossagents_hub::start(HubConfig::new(Paths::new(home.path()), 0))
        .await
        .unwrap();
    hub.extension_hooks.set_rate(HookRate {
        limit: 2,
        window: Duration::from_secs(60),
    });
    hub.extensions.scan().unwrap();
    let fp = pending(&hub.extensions, "demo");
    hub.extensions.enable("demo", &fp).unwrap();

    // Four holds fill demo's per-extension limit, so the next hook is Busy.
    for _ in 0..4 {
        hub.extension_runner
            .start(Caller::Human, "demo", "run", json!({ "mode": "hold" }))
            .unwrap();
    }
    until("the holds to start", || {
        hub.extension_runner.live_tokens() == 4
    })
    .await;
    for _ in 0..2 {
        hub.bus
            .publish(EventKind::HubStarted {
                version: "test".into(),
            })
            .await
            .unwrap();
    }
    let log = home.path().join("extension-logs/demo.log");
    until("both Busy drops", || {
        fs::read_to_string(&log)
            .map(|text| text.matches("dropped: busy").count() >= 2)
            .unwrap_or(false)
    })
    .await;

    // Free the slots; the breaker must still allow a hook that can really start.
    fs::write(home.path().join("extension-data/demo/release"), b"x").unwrap();
    until("the holds to end", || {
        hub.extension_runner.live_tokens() == 0
    })
    .await;
    hub.bus
        .publish(EventKind::HubStarted {
            version: "test".into(),
        })
        .await
        .unwrap();
    let starts = home.path().join("extension-data/demo/starts");
    until("the hook to start", || lines(&starts) == 1).await;
    assert_eq!(
        lines(&starts),
        1,
        "the breaker charged events that never started"
    );
    let log = fs::read_to_string(&log).unwrap();
    assert!(!log.contains("2 starts"), "{log}");
}
