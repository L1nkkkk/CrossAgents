//! The CrossAgents hub: owns all state and serves the local API.

pub mod accounts;
pub mod api;
pub mod collab;
pub mod notices;
pub mod paths;
pub mod runtime;
pub mod transcript;
pub mod usage;

use std::fs::{File, TryLockError};
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use crossagents_core::{EventBus, EventKind, Store};
use crossagents_orchestrator::{GitWorkspaces, Orchestrator, OrchestratorConfig, ShellVerifier};
use crossagents_store_sqlite::SqliteStore;

use crate::accounts::Accounts;
use crate::collab::{AgentTokens, CollabSetup, HubHost};
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

    let codex_accounts = match &config.codex_usage_url {
        Some(url) => CodexAccounts::with_usage_url(url.clone()),
        None => CodexAccounts::default(),
    };
    let accounts = Arc::new(Accounts::new(
        bus.clone(),
        vec![Arc::new(codex_accounts)],
        paths.accounts(),
    ));
    accounts.watch();
    if config.detect_accounts
        && let Err(err) = accounts.detect_defaults().await
    {
        tracing::warn!(error = %err, "cannot register local logins as accounts");
    }
    let tokens = Arc::new(AgentTokens::default());
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
    runtime.reset_statuses().await?;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", config.port))
        .await
        .with_context(|| format!("cannot bind 127.0.0.1:{}", config.port))?;
    let addr = listener.local_addr()?;
    let _ = setup.url.set(format!("http://{addr}"));

    let orchestrator = Orchestrator::new(
        store.clone() as Arc<dyn Store>,
        bus.clone(),
        Arc::new(HubHost {
            runtime: runtime.clone(),
            bus: bus.clone(),
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
    Arc::new(notices::Notices {
        bus: bus.clone(),
        runtime: runtime.clone(),
    })
    .start(shutdown.clone());
    let state = Arc::new(api::AppState {
        bus: bus.clone(),
        runtime: runtime.clone(),
        orchestrator: orchestrator.clone(),
        accounts: accounts.clone(),
        tokens,
        token: token.clone(),
        home: paths.home.clone(),
        started_at,
        shutdown: shutdown.clone(),
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
        async move {
            let served = axum::serve(listener, api::router(state))
                .with_graceful_shutdown(shutdown.clone().cancelled_owned())
                .await;
            // Make sure SSE streams end even if serve failed.
            shutdown.cancel();
            runtime.stop_all().await;
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
fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
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
fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)
}
