//! A hub with the mock agent registered as plugin `mock`, plus an event watcher.

#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use crossagents_api_types::{Agent, Event};
use crossagents_hub::{HubConfig, Paths, RunningHub};
use crossagents_plugins::PluginConfig;
use futures::StreamExt;
use reqwest::{Client, Response};
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

pub const MOCK_AGENT: &str = env!("CARGO_BIN_EXE_crossagents-mock-agent");

/// How long a test waits for something that does come (an event, a status, a process).
///
/// Generous by default (90 s) so that a loaded machine is not a failure; set
/// `CROSSAGENTS_TEST_WAIT_SECS` to change it.
pub fn wait_limit() -> Duration {
    static SECS: OnceLock<u64> = OnceLock::new();
    Duration::from_secs(*SECS.get_or_init(|| {
        std::env::var("CROSSAGENTS_TEST_WAIT_SECS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .filter(|&s| s > 0)
            .unwrap_or(90)
    }))
}

/// How many tests of this binary may have hubs running at once: half the machine's
/// parallelism, at least 2, or `CROSSAGENTS_TEST_HUBS`. Each hub runs agents, git and MCP
/// processes; 22 at once on Windows made single runs take longer than any wait (lesson
/// bb72e536).
fn hub_slots() -> &'static Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| {
        let n = std::env::var("CROSSAGENTS_TEST_HUBS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get() / 2)
                    .unwrap_or(2)
                    .max(2)
            });
        Arc::new(Semaphore::new(n))
    })
}

/// A slot among the hubs that may run at once (see [`hub_slots`]); released when the last
/// hub of the test holding it is dropped.
///
/// A test is one thread, and one test may hold several hubs at a time (a restart on the
/// same home starts the next hub before the last one's remains are dropped): hubs started
/// on a thread that holds a slot share it, so a test never waits for itself.
#[derive(Clone)]
pub struct HubPermit(Arc<OwnedSemaphorePermit>);

thread_local! {
    static HELD: RefCell<Weak<OwnedSemaphorePermit>> = const { RefCell::new(Weak::new()) };
}

/// Takes a hub slot for this test (waiting for one if all are taken), or shares the one
/// it already holds. Call it before starting a hub and keep it as long as the hub.
pub async fn hub_permit() -> HubPermit {
    if let Some(held) = HELD.with(|h| h.borrow().upgrade()) {
        return HubPermit(held);
    }
    let permit = Arc::new(
        hub_slots()
            .clone()
            .acquire_owned()
            .await
            .expect("the hub slots are never closed"),
    );
    HELD.with(|h| *h.borrow_mut() = Arc::downgrade(&permit));
    HubPermit(permit)
}

pub struct TestHub {
    pub hub: RunningHub,
    pub client: Client,
    pub base: String,
    pub paths: Paths,
    pub home: tempfile::TempDir,
    /// Declared last: released after the hub is dropped.
    pub permit: HubPermit,
}

pub fn mock_plugins() -> BTreeMap<String, PluginConfig> {
    let mut plugins = BTreeMap::new();
    plugins.insert(
        "mock".to_owned(),
        PluginConfig {
            command: vec![MOCK_AGENT.to_owned()],
            models: vec!["mock-opus".into(), "mock-haiku".into()],
            model_env: None,
            ..PluginConfig::default()
        },
    );
    plugins
}

/// Plugin `native`: the mock speaking Claude Code's stream-json through `claude-native`, a
/// driver that resumes a session in another directory (`move_cwd`).
pub fn native_plugin(config: &mut HubConfig) {
    native_plugin_with(&[])(config);
}

/// Like [`native_plugin`], with extra arguments for the mock (e.g. `--steer-merge`).
pub fn native_plugin_with(args: &[&str]) -> impl FnOnce(&mut HubConfig) + use<> {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    move |config: &mut HubConfig| {
        let mut command = vec![MOCK_AGENT.to_owned(), "stream-json".to_owned()];
        command.extend(args.clone());
        config.plugins.insert(
            "native".to_owned(),
            PluginConfig {
                driver: Some(crossagents_core::DriverKind::ClaudeNative),
                native_command: command,
                models: vec!["mock-native".into()],
                ..PluginConfig::default()
            },
        );
    }
}

/// Plugin `pinned`: like `native`, but the mock keeps each session in `sessions` with the
/// directory it started in and resumes it nowhere else, and the plugin says so
/// (`move_cwd = false`): a CLI that resumes, but only where it ran.
pub fn pinned_plugin(sessions: &std::path::Path) -> impl FnOnce(&mut HubConfig) + use<> {
    let sessions = sessions.display().to_string();
    move |config| {
        native_plugin(config);
        config.plugins.insert(
            "pinned".to_owned(),
            PluginConfig {
                driver: Some(crossagents_core::DriverKind::ClaudeNative),
                native_command: vec![
                    MOCK_AGENT.to_owned(),
                    "stream-json".to_owned(),
                    "--mock-sessions".to_owned(),
                    sessions,
                ],
                models: vec!["mock-native".into()],
                move_cwd: Some(false),
                ..PluginConfig::default()
            },
        );
    }
}

/// The `crossagents` binary (it serves `crossagents mcp`).
pub fn crossagents_bin() -> std::path::PathBuf {
    static BUILT: std::sync::Once = std::sync::Once::new();
    let path = std::path::Path::new(MOCK_AGENT)
        .with_file_name(format!("crossagents{}", std::env::consts::EXE_SUFFIX));
    // Always build: a stale binary would serve outdated tools. Cheap when up to date.
    BUILT.call_once(|| {
        let status = std::process::Command::new(env!("CARGO"))
            .args(["build", "-q", "-p", "crossagents-cli"])
            .status()
            .unwrap();
        assert!(status.success(), "cannot build crossagents");
    });
    path
}

/// A git repository with one commit.
pub fn git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.path().join("README.md"), "test repo\n").unwrap();
    git(&["add", "-A"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "-m",
        "init",
    ]);
    dir
}

impl TestHub {
    pub async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    /// Stops this hub and gives back the home it used, so that a hub started on it finds what
    /// the last one left (`start_in`), as after a restart.
    pub async fn restart_home(self) -> tempfile::TempDir {
        self.hub.shutdown();
        let TestHub { hub, home, .. } = self;
        hub.wait().await.expect("the hub stops");
        home
    }

    /// Starts a hub after `adjust` has changed its config.
    pub async fn start_with(adjust: impl FnOnce(&mut HubConfig)) -> Self {
        Self::start_in(tempfile::tempdir().unwrap(), adjust).await
    }

    /// Like [`Self::start_with`], on a home that may hold a previous hub's state.
    pub async fn start_in(home: tempfile::TempDir, adjust: impl FnOnce(&mut HubConfig)) -> Self {
        let paths = Paths::new(home.path());
        let mut config = HubConfig::new(paths.clone(), 0);
        config.plugins = mock_plugins();
        config.mcp_command = Some(vec![
            crossagents_bin().display().to_string(),
            "mcp".to_owned(),
        ]);
        adjust(&mut config);
        let permit = hub_permit().await;
        let hub = crossagents_hub::start(config).await.unwrap();
        let base = format!("http://{}", hub.addr);
        Self {
            hub,
            client: Client::new(),
            base,
            paths,
            home,
            permit,
        }
    }

    pub async fn get(&self, path: &str) -> Response {
        self.client
            .get(format!("{}{path}", self.base))
            .bearer_auth(&self.hub.token)
            .send()
            .await
            .unwrap()
    }

    pub async fn post(&self, path: &str, body: Value) -> Response {
        self.client
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.hub.token)
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    pub async fn delete(&self, path: &str) -> Response {
        self.client
            .delete(format!("{}{path}", self.base))
            .bearer_auth(&self.hub.token)
            .send()
            .await
            .unwrap()
    }

    pub async fn add_agent(&self, name: &str, model: Option<&str>) -> Agent {
        let cwd = self.home.path().to_path_buf();
        self.add_agent_in(name, model, "worker", &cwd).await
    }

    pub async fn add_agent_in(
        &self,
        name: &str,
        model: Option<&str>,
        role: &str,
        cwd: &std::path::Path,
    ) -> Agent {
        let resp = self
            .post(
                "/v1/agents",
                json!({
                    "name": name, "plugin": "mock", "model": model, "role": role,
                    "cwd": cwd.display().to_string(),
                }),
            )
            .await;
        assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
        resp.json().await.unwrap()
    }

    /// A `native` agent (see [`native_plugin`]) living in `cwd`.
    pub async fn add_native(&self, name: &str, role: &str, cwd: &std::path::Path) -> Agent {
        self.add_on("native", name, role, cwd).await
    }

    /// An agent of `plugin` (one of the stream-json plugins here, model `mock-native`).
    pub async fn add_on(
        &self,
        plugin: &str,
        name: &str,
        role: &str,
        cwd: &std::path::Path,
    ) -> Agent {
        let resp = self
            .post(
                "/v1/agents",
                json!({
                    "name": name, "plugin": plugin, "model": "mock-native", "role": role,
                    "cwd": cwd.display().to_string(),
                }),
            )
            .await;
        assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
        resp.json().await.unwrap()
    }

    pub async fn agent(&self, key: &str) -> Agent {
        let resp = self.get(&format!("/v1/agents/{key}")).await;
        assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
        resp.json().await.unwrap()
    }

    pub async fn send(&self, agent: &str, text: &str) -> Value {
        let resp = self
            .post(
                &format!("/v1/agents/{agent}/messages"),
                json!({ "text": text }),
            )
            .await;
        assert_eq!(resp.status(), 202, "{}", resp.text().await.unwrap());
        resp.json().await.unwrap()
    }

    /// Starts collecting events published from now on.
    pub async fn watch(&self) -> Watcher {
        let resp = self
            .client
            .get(format!("{}/v1/events?live=true", self.base))
            .bearer_auth(&self.hub.token)
            .send()
            .await
            .unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut bytes = resp.bytes_stream();
            let mut buf = String::new();
            while let Some(Ok(chunk)) = bytes.next().await {
                buf.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(end) = buf.find("\n\n") {
                    let block: String = buf.drain(..end + 2).collect();
                    for line in block.lines() {
                        if let Some(data) = line.strip_prefix("data:")
                            && let Ok(event) = serde_json::from_str::<Event>(data.trim_start())
                        {
                            let _ = tx.send(event);
                        }
                    }
                }
            }
        });
        Watcher {
            rx,
            seen: Vec::new(),
        }
    }
}

pub struct Watcher {
    rx: mpsc::UnboundedReceiver<Event>,
    pub seen: Vec<Event>,
}

impl Watcher {
    /// Waits for the first event matching `pred`, keeping everything seen.
    ///
    /// The wait is for a fact that lands, not a reason to fail early: it lasts
    /// [`wait_limit`] (docs/TODO.md §九, the pipeline tests), and hubs running at once are
    /// limited ([`hub_permit`]) so that a loaded machine stays within it.
    pub async fn until(&mut self, what: &str, pred: impl Fn(&Event) -> bool) -> Event {
        let deadline = tokio::time::Instant::now() + wait_limit();
        loop {
            let event = tokio::time::timeout_at(deadline, self.rx.recv())
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "timed out waiting for {what}; saw {:#?}\noutput:\n{}",
                        types(&self.seen),
                        self.output()
                    )
                })
                .expect("event stream ended");
            self.seen.push(event.clone());
            if pred(&event) {
                return event;
            }
        }
    }

    /// Like [`Self::until`], but an event already seen counts too.
    pub async fn find(&mut self, what: &str, pred: impl Fn(&Event) -> bool) -> Event {
        if let Some(event) = self.seen.iter().find(|e| pred(e)) {
            return event.clone();
        }
        self.until(what, pred).await
    }

    /// Concatenated `agent.output` text seen so far.
    pub fn output(&self) -> String {
        self.seen
            .iter()
            .filter_map(|e| match &e.kind {
                crossagents_api_types::EventKind::AgentOutput { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
}

pub fn types(events: &[Event]) -> Vec<&'static str> {
    events.iter().map(|e| e.kind.type_name()).collect()
}
