//! The CrossAgents hub: owns all state and serves the local API.

pub mod accounts;
pub mod api;
pub mod budget;
pub mod collab;
pub mod identity;
pub mod models;
pub mod notices;
pub mod paths;
pub mod runtime;
pub mod snapshot;
pub mod transcript;
pub mod usage;

use std::fs::{File, TryLockError};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use crossagents_core::{EventBus, EventKind, Store};
use crossagents_orchestrator::{GitWorkspaces, Orchestrator, OrchestratorConfig, ShellVerifier};
use crossagents_store_sqlite::SqliteStore;

use crate::accounts::Accounts;
use crate::collab::{AgentTokens, CollabSetup, HubHost};
use crossagents_plugins::claude_account::ClaudeAccounts;
use crossagents_plugins::codebuddy_account::CodebuddyAccounts;
use crossagents_plugins::codex_account::CodexAccounts;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub use api::VERSION;
use crossagents_plugins::PluginRegistry;
pub use paths::{DEFAULT_PORT, HOME_ENV, HubConfig, HubFile, Paths};
pub use runtime::{Runtime, SessionExtras, SessionSetup};

/// A hub serving in the background of the current process.
pub struct RunningHub {
    pub addr: SocketAddr,
    pub token: String,
    pub bus: Arc<EventBus>,
    pub runtime: Arc<Runtime>,
    pub orchestrator: Arc<Orchestrator>,
    shutdown: CancellationToken,
    task: JoinHandle<Result<()>>,
}

impl RunningHub {
    /// Cancelling this token stops the hub.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// Waits until the hub has stopped (after [`Self::shutdown`] or `POST /v1/shutdown`).
    pub async fn wait(self) -> Result<()> {
        self.task.await.context("hub task panicked")?
    }
}

/// Starts the hub: takes the single-instance lock, opens the store, binds the API and
/// writes `hub.json`. Returns once the hub is accepting connections.
pub async fn start(config: HubConfig) -> Result<RunningHub> {
    let paths = config.paths.clone();
    std::fs::create_dir_all(&paths.home)
        .with_context(|| format!("cannot create {}", paths.home.display()))?;

    let lock = acquire_lock(&paths)?;
    let token = ensure_token(&paths)?;
    let store = Arc::new(SqliteStore::open(&paths.db()).await?);
    let bus = Arc::new(EventBus::new(store.clone() as Arc<dyn Store>));
    // Where this hub's events begin; the previous hub's are before.
    let start_seq = store.latest_event_seq().await?;
    let hosted = config.runtime_host.is_some();
    match &config.runtime_host {
        Some(command) => crossagents_drivers::host::enable(crossagents_drivers::host::Settings {
            command: command.clone(),
            dir: paths.runtimes(),
        }),
        None => crossagents_drivers::host::disable(),
    }

    let codex_accounts = match &config.codex_usage_url {
        Some(url) => CodexAccounts::with_usage_url(url.clone()),
        None => CodexAccounts::default(),
    };
    let accounts = Arc::new(Accounts::new(
        bus.clone(),
        vec![
            Arc::new(codex_accounts),
            Arc::new(ClaudeAccounts::default()),
            Arc::new(CodebuddyAccounts::default()),
        ],
        paths.accounts(),
    ));
    accounts.watch();
    if config.detect_accounts
        && let Err(err) = accounts.detect_defaults().await
    {
        tracing::warn!(error = %err, "cannot register local logins as accounts");
    }
    accounts.warm();
    let tokens = Arc::new(AgentTokens::load(paths.agent_tokens()));
    let setup = Arc::new(CollabSetup {
        tokens: tokens.clone(),
        accounts: accounts.clone(),
        mcp_command: match &config.mcp_command {
            Some(command) => command.clone(),
            None => default_mcp_command()?,
        },
        url: OnceLock::new(),
    });
    let (plugins, plugin_warnings) = PluginRegistry::load(&config.plugins, &paths.plugins());
    for warning in &plugin_warnings {
        tracing::warn!("skipping plugin: {warning}");
    }
    let runtime = Arc::new(Runtime::new(bus.clone(), plugins).with_setup(setup.clone()));
    let model_choice = Arc::new(models::ModelChoice::load(paths.models()));
    // Agents from before names were for people get a name and (chats) a title.
    if let Err(err) = identity::fix_up(
        store.as_ref(),
        &paths.home.join("chats"),
        &identity::denied_words(runtime.plugins()),
    )
    .await
    {
        tracing::warn!(error = %err, "cannot bring agent names up to date");
    }

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", config.port))
        .await
        .with_context(|| format!("cannot bind 127.0.0.1:{}", config.port))?;
    let addr = listener.local_addr()?;
    let _ = setup.url.set(format!("http://{addr}"));

    // What the previous hub left: processes to take over, turns lost, messages waiting.
    let hub_state = HubState::load(&paths);
    let recovery = runtime.recover(hub_state.since).await?;
    tokens
        .retain(&recovery.adopted.iter().copied().collect())
        .await;
    if !recovery.adopted.is_empty() || !recovery.interrupted.is_empty() || recovery.requeued > 0 {
        tracing::info!(
            adopted = recovery.adopted.len(),
            interrupted = recovery.interrupted.len(),
            requeued = recovery.requeued,
            released = recovery.released,
            "picked up after the previous hub"
        );
    }
    // Look back from here next time, or from as far back as something is still unfinished.
    let keep_window = recovery.requeued > 0 || !recovery.adopted.is_empty();
    HubState {
        since: Some(match hub_state.since {
            Some(since) if keep_window => since,
            _ => start_seq,
        }),
    }
    .save(&paths);

    let orchestrator = Orchestrator::new(
        store.clone() as Arc<dyn Store>,
        bus.clone(),
        Arc::new(HubHost {
            runtime: runtime.clone(),
            bus: bus.clone(),
            accounts: accounts.clone(),
            models: model_choice.clone(),
        }),
        Arc::new(GitWorkspaces),
        Arc::new(ShellVerifier::default()),
        OrchestratorConfig {
            worktrees: paths.home.join("worktrees"),
            max_nudges: 2,
        },
    );
    orchestrator.watch();
    orchestrator.resume().await?;

    let started_at = Utc::now();
    let hub_file = HubFile {
        pid: std::process::id(),
        port: addr.port(),
        version: VERSION.into(),
        started_at,
    };
    std::fs::write(paths.hub_file(), serde_json::to_vec_pretty(&hub_file)?)
        .context("cannot write hub.json")?;

    let shutdown = CancellationToken::new();
    let stop_agents = Arc::new(AtomicBool::new(!hosted));
    let budgets = Arc::new(
        budget::Budgets::load(
            store.as_ref() as &dyn crossagents_core::Store,
            orchestrator.clone(),
        )
        .await?,
    );
    Arc::new(notices::Notices {
        bus: bus.clone(),
        runtime: runtime.clone(),
        orchestrator: orchestrator.clone(),
        budgets,
    })
    .start(shutdown.clone());
    let state = Arc::new(api::AppState {
        bus: bus.clone(),
        runtime: runtime.clone(),
        orchestrator: orchestrator.clone(),
        accounts: accounts.clone(),
        models: model_choice,
        tokens,
        token: token.clone(),
        home: paths.home.clone(),
        started_at,
        shutdown: shutdown.clone(),
        stop_agents: stop_agents.clone(),
        mcp_command: setup.mcp_command.clone(),
        url: format!("http://{addr}"),
    });
    bus.publish(EventKind::HubStarted {
        version: VERSION.into(),
    })
    .await?;
    tracing::info!(%addr, home = %paths.home.display(), "hub started");

    let task = tokio::spawn({
        let shutdown = shutdown.clone();
        let bus = bus.clone();
        let runtime = runtime.clone();
        let stop_agents = stop_agents.clone();
        async move {
            let served = axum::serve(listener, api::router(state))
                .with_graceful_shutdown(shutdown.clone().cancelled_owned())
                .await;
            // Make sure SSE streams end even if serve failed.
            shutdown.cancel();
            if stop_agents.load(Ordering::SeqCst) {
                runtime.stop_all().await;
            } else {
                // Agents in runtime hosts go on; the next hub takes them over.
                runtime.leave().await;
            }
            if let Err(err) = bus.publish(EventKind::HubStopping).await {
                tracing::warn!(error = %err, "cannot record hub.stopping");
            }
            store.close().await;
            if let Err(err) = std::fs::remove_file(paths.hub_file()) {
                tracing::warn!(error = %err, "cannot remove hub.json");
            }
            drop(lock);
            tracing::info!("hub stopped");
            served.context("API server failed")
        }
    });

    Ok(RunningHub {
        addr,
        token,
        bus,
        runtime,
        orchestrator,
        shutdown,
        task,
    })
}

/// `hub-state.json`: where the next hub looks back from for unfinished work.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct HubState {
    since: Option<u64>,
}

impl HubState {
    fn load(paths: &Paths) -> Self {
        std::fs::read(paths.hub_state())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, paths: &Paths) {
        let text = serde_json::to_string(self).unwrap_or_default();
        if let Err(err) = std::fs::write(paths.hub_state(), text) {
            tracing::warn!(error = %err, "cannot write hub-state.json");
        }
    }
}

/// `<this executable> mcp`.
fn default_mcp_command() -> Result<Vec<String>> {
    let exe = std::env::current_exe().context("cannot locate the crossagents executable")?;
    Ok(vec![exe.display().to_string(), "mcp".into()])
}

/// Runs the hub until `stop` resolves or a client calls `POST /v1/shutdown`.
pub async fn run(config: HubConfig, stop: impl Future<Output = ()>) -> Result<()> {
    let hub = start(config).await?;
    let token = hub.shutdown_token();
    tokio::select! {
        _ = stop => hub.shutdown(),
        _ = token.cancelled() => {}
    }
    hub.wait().await
}

fn acquire_lock(paths: &Paths) -> Result<File> {
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(paths.lock())
        .context("cannot open hub.lock")?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => bail!(
            "a hub is already running for {} (hub.lock is held)",
            paths.home.display()
        ),
        Err(TryLockError::Error(e)) => Err(e).context("cannot lock hub.lock"),
    }
}

/// Reads the API token, generating one on first start.
fn ensure_token(paths: &Paths) -> Result<String> {
    if let Ok(token) = paths.read_token()
        && !token.is_empty()
    {
        return Ok(token);
    }
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    write_private(&paths.token(), &token).context("cannot write token")?;
    Ok(token)
}

#[cfg(unix)]
pub(crate) fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    File::options()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?
        .write_all(contents.as_bytes())
}

#[cfg(not(unix))]
pub(crate) fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)
}
