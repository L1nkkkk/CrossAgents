//! The CrossAgents hub: owns all state and serves the local API.

pub mod access;
pub mod account_import;
pub mod accounts;
pub mod api;
pub mod api_expertise;
pub mod api_projects;
pub mod lessons;
pub mod api_terminal;
pub mod attachments;
pub mod avatars;
pub mod budget;
pub mod collab;
pub mod event_log;
pub mod extensions;
pub mod guard;
pub mod identity;
pub mod image;
pub mod install;
pub mod models;
pub mod notices;
pub mod paths;
pub mod prices;
pub mod refresh;
pub mod replies;
pub mod resume;
pub mod retro;
pub mod routing;
pub mod rules;
pub mod run_cost;
pub mod runtime;
pub mod shadow;
pub mod shims;
pub mod snapshot;
pub mod stream_rules;
pub mod terminal;
pub mod trace;
pub mod transcript;
pub mod usage;
pub mod usage_series;
pub mod version_gate;

use std::fs::{File, TryLockError};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use crossagents_core::{EventBus, EventKind, EventSink, Store};
use crossagents_orchestrator::{GitWorkspaces, Orchestrator, OrchestratorConfig, ShellVerifier};
use crossagents_store_sqlite::SqliteStore;

use crate::accounts::Accounts;
use crate::collab::{AgentTokens, CollabSetup, HubHost};
use crossagents_plugins::claude_account::ClaudeAccounts;
use crossagents_plugins::codebuddy_account::CodebuddyAccounts;
use crossagents_plugins::codex_account::CodexAccounts;
use crossagents_plugins::cursor_account::CursorAccounts;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub use api::VERSION;
use crossagents_plugins::PluginRegistry;
pub use paths::{DEFAULT_PORT, HOME_ENV, HUB_HOME_ENV, HubConfig, HubFile, Paths};
pub use runtime::{Runtime, SessionExtras, SessionSetup};

/// A hub serving in the background of the current process.
pub struct RunningHub {
    pub addr: SocketAddr,
    pub token: String,
    /// The read-only token (docs/API-AUTH.md §3).
    pub read_token: String,
    pub bus: Arc<EventBus>,
    pub runtime: Arc<Runtime>,
    pub orchestrator: Arc<Orchestrator>,
    /// Extension discovery and approval.
    pub extensions: Arc<extensions::Registry>,
    /// Runs extension tools.
    pub extension_runner: Arc<extensions::Runner>,
    /// Event hooks (`[[on]]`) running beside the hub.
    pub extension_hooks: Arc<extensions::Hooks>,
    /// The agents' session tokens (`/v1/me`).
    pub agent_tokens: Arc<AgentTokens>,
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
    // `cargo` in a run worktree is this executable (docs/tasks/LOCKS.md §4.6). Failing to
    // place the shims is not fatal: the worktrees then run the real programs directly, which
    // is what happened before them.
    let shims = match shims::ensure(&paths) {
        Ok(dir) => Some(dir),
        Err(err) => {
            tracing::warn!(error = %err, "cannot install the shims; run worktrees will not hold their resources for plain commands");
            None
        }
    };
    let token = ensure_token(&paths)?;
    let read_token = ensure_token_at(&paths.token_read())?;
    let store = Arc::new(SqliteStore::open(&paths.db()).await?);
    // Every event goes to a JSON-lines file per day as well: that history can be read with no
    // window open (docs/TODO.md §四). The sink holds the writer, so it lives as long as the bus.
    let mut bus = EventBus::new(store.clone() as Arc<dyn Store>);
    if config.event_log {
        bus = bus.with_sink(event_log::EventLog::spawn(paths.logs()) as Arc<dyn EventSink>);
    }
    let bus = Arc::new(bus);
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
            Arc::new(CursorAccounts::default()),
        ],
        paths.accounts(),
    ));
    accounts.watch();
    let tokens = Arc::new(AgentTokens::load(paths.agent_tokens()));
    let (plugins, plugin_warnings) = PluginRegistry::load(&config.plugins, &paths.plugins());
    for warning in &plugin_warnings {
        tracing::warn!("skipping plugin: {warning}");
    }
    let setup = Arc::new(CollabSetup {
        tokens: tokens.clone(),
        accounts: accounts.clone(),
        // Cheap to clone: clones share the descriptions and the drivers, so a session's
        // version gate reads the same CLI status `/v1/plugins` reports.
        plugins: plugins.clone(),
        mcp_command: match &config.mcp_command {
            Some(command) => command.clone(),
            None => default_mcp_command()?,
        },
        extra_mcp: config.mcp_servers.clone(),
        url: OnceLock::new(),
        worktrees: paths.home.join("worktrees"),
        shims: shims.clone(),
        orchestrator: Arc::new(OnceLock::new()),
    });
    let runtime = Arc::new(Runtime::new(bus.clone(), plugins).with_setup(setup.clone()));
    runtime.watch_moves();
    runtime.watch_replies();
    // Each CLI is asked its version in the background, in parallel: `GET /v1/plugins`
    // reports it once it answers and never waits for a subprocess (docs/DESIGN.md §9.3).
    // Asking again later (an install, `POST /v1/plugins/refresh`) goes through `Refresher`,
    // which also says `plugin.changed` when a probe ends.
    for driver in runtime.plugins().defaults() {
        tokio::spawn(async move { driver.probe_cli().await });
    }
    let refresher = Arc::new(refresh::Refresher::new());
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

    // Extensions come before the orchestrator starts sessions, so a new session's
    // instructions already carry the enabled extensions' prompts.
    let extensions = Arc::new(extensions::Registry::load_with_bus(
        paths.home.clone(),
        Some(bus.clone()),
    )?);
    let extension_runner =
        extensions::Runner::new(extensions.clone(), Some(bus.clone()), paths.home.clone());
    extension_runner.set_url(format!("http://{addr}"));
    let orchestrator = Orchestrator::new(
        store.clone() as Arc<dyn Store>,
        bus.clone(),
        Arc::new(HubHost {
            runtime: runtime.clone(),
            bus: bus.clone(),
            accounts: accounts.clone(),
            models: model_choice.clone(),
            home: paths.home.clone(),
        }),
        Arc::new(GitWorkspaces),
        Arc::new(ShellVerifier {
            worktrees: Some(paths.home.join("worktrees")),
            locks: Some(paths.home.join("locks")),
            shims: shims.clone(),
            ..ShellVerifier::default()
        }),
        OrchestratorConfig {
            worktrees: paths.home.join("worktrees"),
            locks: paths.home.join("locks"),
            shims,
            max_nudges: 2,
            build_cache_limit: config.build_cache_limit,
        },
    );
    orchestrator.set_extensions(extension_runner.clone());
    let _ = setup.orchestrator.set(orchestrator.clone());
    orchestrator.watch();
    orchestrator.prune_build_cache();
    // Retry what an earlier hub could not delete (a Windows lock on a build directory, a
    // checkout whose lead was mid-turn when the hub stopped).
    {
        let orchestrator = orchestrator.clone();
        tokio::spawn(async move {
            orchestrator.sweep_worktrees().await;
            // An apply the last hub was in the middle of, or owed when it stopped, is
            // finished (docs/tasks/LEAD.md §6).
            orchestrator.recover_inplace_applies().await;
        });
    }
    // Settle what runs spent while no hub was there (agents' replayed usage included) before
    // anything is dispatched: an over-budget run pauses first (docs/tasks/DOGFOOD.md §5).
    let budgets = Arc::new(
        budget::Budgets::load(
            store.as_ref() as &dyn crossagents_core::Store,
            orchestrator.clone(),
        )
        .await?,
    );
    if let Err(err) = budgets
        .check(store.as_ref() as &dyn crossagents_core::Store)
        .await
    {
        tracing::warn!(error = %err, "cannot settle budgets at startup");
    }
    // Agents from before homes get the workspace they belong to; claims of runs that ended
    // while no hub was there go (docs/tasks/S1.md §3.1).
    match orchestrator.migrate_homes().await {
        Ok(moved) => {
            for (name, home) in &moved {
                tracing::info!(agent = %name, home = %home.display(), "agent home set");
            }
        }
        Err(err) => tracing::warn!(error = %err, "cannot set agent homes"),
    }
    if let Err(err) = orchestrator.settle_claims().await {
        tracing::warn!(error = %err, "cannot drop stale claims");
    }
    orchestrator.resume().await?;

    let started_at = Utc::now();
    let hub_file = HubFile {
        pid: std::process::id(),
        port: addr.port(),
        version: VERSION.into(),
        started_at,
        exe: std::env::current_exe().ok(),
    };
    // Atomic: `hub start` polls this file and must never read it half-written.
    crate::paths::write_atomic(&paths.hub_file(), &serde_json::to_vec_pretty(&hub_file)?)
        .context("cannot write hub.json")?;

    let shutdown = CancellationToken::new();
    // Vendor status/model probes can take tens of seconds. Serve first; account.created
    // events refresh clients as local logins are discovered, without delaying hub readiness.
    let account_start = tokio::spawn({
        let accounts = accounts.clone();
        let shutdown = shutdown.clone();
        let detect = config.detect_accounts;
        async move {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => {}
                _ = async {
                    if detect && let Err(err) = accounts.detect_defaults().await {
                        tracing::warn!(error = %err, "cannot register local logins as accounts");
                    }
                    accounts.warm();
                } => {}
            }
        }
    });
    let stop_agents = Arc::new(AtomicBool::new(!hosted));
    Arc::new(notices::Notices {
        bus: bus.clone(),
        runtime: runtime.clone(),
        orchestrator: orchestrator.clone(),
        budgets,
    })
    .start(shutdown.clone());
    let terminals = terminal::Terminals::new(config.terminal_shell.clone(), config.terminal_idle);
    terminals.start_reclaiming(shutdown.clone());
    let extension_poll = extensions.start(shutdown.clone());
    // Event hooks and resident services run beside the hub and end with it
    // (docs/EXTENSIONS.md §3). A service starts here, for what the enabled snapshots declare.
    let extension_hooks = extensions::Hooks::new(
        extension_runner.clone(),
        extensions.clone(),
        Some(bus.clone()),
    );
    let extension_services =
        extensions::Services::new(extension_runner.clone(), extensions.clone());
    let hooks_task = extension_hooks.start(shutdown.clone());
    let services_task = extension_services.start(shutdown.clone());
    let agent_tokens = tokens.clone();
    let state = Arc::new(api::AppState {
        extensions: extensions.clone(),
        runner: extension_runner.clone(),
        bus: bus.clone(),
        runtime: runtime.clone(),
        orchestrator: orchestrator.clone(),
        accounts: accounts.clone(),
        models: model_choice,
        tokens,
        token: token.clone(),
        read_token: read_token.clone(),
        home: paths.home.clone(),
        started_at,
        shutdown: shutdown.clone(),
        stop_agents: stop_agents.clone(),
        mcp_command: setup.mcp_command.clone(),
        url: format!("http://{addr}"),
        installer: Arc::new(install::Installer::new()),
        refresher,
        terminals: terminals.clone(),
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
        let stopping_runner = extension_runner.clone();
        let stopping_hooks = extension_hooks.clone();
        // Tool calls end the moment the hub is told to stop, not after the HTTP server has
        // drained: a request waiting for a tool's result would hold the drain up.
        tokio::spawn({
            let (shutdown, runner) = (shutdown.clone(), extension_runner.clone());
            async move {
                shutdown.cancelled().await;
                runner.shutdown().await;
            }
        });
        async move {
            let served = axum::serve(listener, api::router(state))
                .with_graceful_shutdown(shutdown.clone().cancelled_owned())
                .await;
            // Make sure SSE streams end even if serve failed.
            shutdown.cancel();
            let _ = account_start.await;
            let _ = extension_poll.await;
            // Hooks and services end before the runner closes its door, so their processes
            // are gone with the hub and none of them is restarted.
            let _ = hooks_task.await;
            // The listener has cancelled the hooks' token; wait for their trees to be gone.
            stopping_hooks.shutdown().await;
            let _ = services_task.await;
            // Running tool calls end with the hub; their results are lost (docs/EXTENSIONS.md §3).
            stopping_runner.shutdown().await;
            terminals.stop_all();
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
        read_token,
        bus,
        runtime,
        orchestrator,
        extensions,
        extension_runner,
        extension_hooks,
        agent_tokens,
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
    ensure_token_at(&paths.token())
}

/// Reads the token in `path`, generating one when there is none.
fn ensure_token_at(path: &std::path::Path) -> Result<String> {
    if let Ok(token) = std::fs::read_to_string(path)
        && !token.trim().is_empty()
    {
        return Ok(token.trim().to_owned());
    }
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    write_private(path, &token).with_context(|| format!("cannot write {}", path.display()))?;
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
