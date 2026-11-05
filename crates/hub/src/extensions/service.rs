//! Resident processes: at most one per enabled extension, supervised
//! (docs/EXTENSIONS.md §3).
//!
//! The process runs in the approved snapshot with the extension's data directory and a
//! token issued for its lifetime. If it exits, it is started again with a growing backoff
//! (1 s, doubling, at most 60 s); running for 60 s without exiting resets the backoff and the
//! restart count. Switching the extension to another snapshot stops the old process before
//! the new one starts, disabling it keeps the process for the 5 s grace and then kills it,
//! and the hub stopping takes it down too: a service never outlives the hub that started it.
//!
//! What it does is reported through [`crossagents_api_types::ExtensionServiceStatus`], which
//! the registry carries in `ExtensionInfo.service`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossagents_api_types::{ExtensionServiceState, ExtensionServiceStatus};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::runner::{CommandResult, CommandSpec, CommandStatus, OutputCapture};
use super::{Registry, Runner};

/// The first wait after an exit, and the most it grows to.
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Running this long without exiting resets the backoff and the restart count.
const STABLE_AFTER: Duration = Duration::from_secs(60);
/// A disabled extension's process is kept this long before it is killed (§3).
const DISABLE_GRACE: Duration = Duration::from_secs(5);

/// Supervises one resident process per extension that declares a `[service]`.
pub struct Services {
    runner: Arc<Runner>,
    registry: Arc<Registry>,
    /// One supervisor per extension that declares a service.
    entries: Mutex<HashMap<String, Entry>>,
    /// Serializes stop-then-start, so one extension never runs two processes.
    reconcile: Mutex<()>,
    /// The hub is stopping.
    shutdown: CancellationToken,
}

struct Entry {
    /// The snapshot the running process (or its backoff) belongs to.
    fingerprint: String,
    /// Ends this extension's supervisor and its process.
    stop: CancellationToken,
    task: JoinHandle<()>,
}

impl Services {
    pub fn new(runner: Arc<Runner>, registry: Arc<Registry>) -> Arc<Self> {
        Arc::new(Self {
            runner,
            registry,
            entries: Mutex::default(),
            reconcile: Mutex::default(),
            shutdown: CancellationToken::new(),
        })
    }

    /// Starts what is enabled now, follows later version changes, and returns the supervising
    /// task. A version switch, an enable or a disable is the moment to stop the old process
    /// and start the new one; the registry reports all three (`on_version_changed`).
    pub fn start(self: &Arc<Self>, shutdown: CancellationToken) -> JoinHandle<()> {
        let weak = Arc::downgrade(self);
        self.registry.on_version_changed(move |name| {
            if let Some(services) = weak.upgrade() {
                services.reconcile_later(name);
            }
        });
        let this = self.clone();
        tokio::spawn(async move {
            for info in this.registry.list() {
                this.reconcile(&info.name).await;
            }
            tokio::select! {
                _ = shutdown.cancelled() => {}
                _ = this.shutdown.cancelled() => {}
            }
            this.stop_all().await;
        })
    }

    /// Stops every process and waits for the supervisors to end.
    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        self.stop_all().await;
    }

    async fn stop_all(&self) {
        let entries: Vec<Entry> = {
            let mut entries = self.entries.lock().await;
            std::mem::take(&mut *entries).into_values().collect()
        };
        for entry in entries {
            let Entry { stop, task, .. } = entry;
            stop.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(10), task).await;
        }
    }

    fn reconcile_later(self: &Arc<Self>, name: &str) {
        let this = self.clone();
        let name = name.to_owned();
        tokio::spawn(async move { this.reconcile(&name).await });
    }

    /// Makes the running process match what the extension declares now.
    async fn reconcile(&self, name: &str) {
        let _serial = self.reconcile.lock().await;
        let info = self.registry.get(name).ok();
        let desired = info.as_ref().and_then(|info| {
            let version = info.current.as_ref()?;
            (info.enabled && version.service).then(|| version.fingerprint.clone())
        });
        let existing = self.entries.lock().await.remove(name);
        if let Some(entry) = existing {
            if desired.as_deref() == Some(entry.fingerprint.as_str()) {
                // Same snapshot: keep it as it is, a process in backoff included.
                self.entries.lock().await.insert(name.into(), entry);
                return;
            }
            // A disable keeps the process for the grace period; a version switch and the hub
            // stopping end it now.
            if !self.shutdown.is_cancelled() && !info.as_ref().is_some_and(|info| info.enabled) {
                tokio::time::sleep(DISABLE_GRACE).await;
            }
            let Entry { stop, task, .. } = entry;
            stop.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(10), task).await;
        }
        let Some(fingerprint) = desired else {
            self.registry.set_service_status(name, None);
            return;
        };
        let stop = self.shutdown.child_token();
        self.registry
            .set_service_status(name, Some(status(ExtensionServiceState::Starting, 0, None)));
        let task = tokio::spawn(supervise(
            self.runner.clone(),
            self.registry.clone(),
            name.to_owned(),
            stop.clone(),
        ));
        self.entries.lock().await.insert(
            name.into(),
            Entry {
                fingerprint,
                stop,
                task,
            },
        );
    }
}

fn status(
    state: ExtensionServiceState,
    restarts: u32,
    last_exit: Option<String>,
) -> ExtensionServiceStatus {
    ExtensionServiceStatus {
        state,
        restarts,
        last_exit,
    }
}

/// Runs one extension's service until `stop` is cancelled: start, wait, back off, start.
async fn supervise(
    runner: Arc<Runner>,
    registry: Arc<Registry>,
    name: String,
    stop: CancellationToken,
) {
    let mut restarts: u32 = 0;
    let mut last_exit: Option<String> = None;
    let mut backoff = FIRST_BACKOFF;
    while !stop.is_cancelled() {
        // Re-acquire each time: a lease only lives as long as one process tree.
        let Ok(lease) = registry.acquire(&name) else {
            break;
        };
        let Some(service) = lease.enabled.manifest.service.as_ref() else {
            break;
        };
        let command = service.command.clone();
        registry.set_service_status(
            &name,
            Some(status(
                ExtensionServiceState::Starting,
                restarts,
                last_exit.clone(),
            )),
        );
        let started = Instant::now();
        let result = match runner.start_command(CommandSpec {
            extension: name.clone(),
            lease,
            command,
            stdin: None,
            timeout: None,
            stdout: OutputCapture::LogOnly,
            // A service is one process per extension already; it never takes a tool slot.
            limits: false,
            label: "service".into(),
            cancel: stop.clone(),
        }) {
            Ok(run) => {
                registry.set_service_status(
                    &name,
                    Some(status(
                        ExtensionServiceState::Running,
                        restarts,
                        last_exit.clone(),
                    )),
                );
                run.wait().await
            }
            Err(error) => CommandResult {
                status: CommandStatus::NotStarted,
                duration: Duration::ZERO,
                stdout: Vec::new(),
                stderr: String::new(),
                error: Some(error.to_string()),
            },
        };
        let ran = started.elapsed();
        last_exit = Some(result.describe());
        // A clean exit is a requested restart, not a crash: it waits the base delay. A crash
        // backs off, and running steadily for a minute puts both counters back to zero.
        let crashed = !result.ok() && ran < STABLE_AFTER;
        if crashed {
            restarts = restarts.saturating_add(1);
        } else {
            restarts = 0;
            backoff = FIRST_BACKOFF;
        }
        if stop.is_cancelled() {
            break;
        }
        registry.set_service_status(
            &name,
            Some(status(
                ExtensionServiceState::Backoff,
                restarts,
                last_exit.clone(),
            )),
        );
        let wait = if crashed { backoff } else { FIRST_BACKOFF };
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = tokio::time::sleep(wait) => {}
        }
        if crashed {
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }
    registry.set_service_status(
        &name,
        Some(status(ExtensionServiceState::Stopped, restarts, last_exit)),
    );
}
