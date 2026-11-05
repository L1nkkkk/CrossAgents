//! Event hooks: one process per matching event, observe only (docs/EXTENSIONS.md §3).
//!
//! A hook never blocks the event and never changes it: the process is started after the
//! event has been stored and broadcast, its exit code and output are ignored, and a failure
//! is only a line in the extension's log. Nothing is retried and nothing is replayed after a
//! restart — only events published while this hub runs are seen.
//!
//! `extension.*` events are refused when the description file is read (manifest.rs), and
//! skipped here as well. That stops a hook from reacting to another extension's event, but it
//! cannot stop a loop that goes through the write API (a `write` hook on `agent.updated` that
//! changes an agent's topic). Those are bounded instead: one extension may start at most
//! [`HookRate::limit`] hooks per [`HookRate::window`], and the excess is dropped with a single
//! log line per window.
//!
//! Hook processes share the tool concurrency limits: when they are full the event is dropped
//! and the log says so, rather than queued. A disabled extension's running hook ends after
//! the same grace a tool call gets (the runner cancels it; see [`super::runner::CommandSpec`]).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use crossagents_core::{Event, EventBus};
use serde_json::json;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::runner::{CommandSpec, OutputCapture};
use super::{Registry, Runner};

/// How many hook processes one extension may start in one sliding window. It is separate
/// from the tool concurrency limits and applies to every extension on its own.
#[derive(Debug, Clone)]
pub struct HookRate {
    pub limit: usize,
    pub window: Duration,
}

impl Default for HookRate {
    fn default() -> Self {
        Self {
            limit: 60,
            window: Duration::from_secs(60),
        }
    }
}

/// One extension's starts inside the window, and when the breaker last wrote its line.
#[derive(Default)]
struct Starts {
    at: VecDeque<Instant>,
    logged_at: Option<Instant>,
}

/// Runs the enabled extensions' `[[on]]` hooks for events this hub publishes.
pub struct Hooks {
    runner: Arc<Runner>,
    registry: Arc<Registry>,
    bus: Option<Arc<EventBus>>,
    /// Ends every hook process; set from the hub's shutdown token by [`Self::start`].
    stop: CancellationToken,
    /// Hook processes still running, so shutdown can wait for them.
    tasks: Mutex<Vec<JoinHandle<()>>>,
    /// The circuit breaker's limit; a test can shrink it.
    rate: RwLock<HookRate>,
    /// Starts per extension, for the breaker.
    starts: Mutex<HashMap<String, Starts>>,
    /// Keep the breaker check and successful start accounting atomic across listeners.
    dispatch_lock: Mutex<()>,
    /// Per-extension token that ends that extension's running hooks when it is disabled.
    disabled: Mutex<HashMap<String, CancellationToken>>,
}

impl Hooks {
    pub fn new(
        runner: Arc<Runner>,
        registry: Arc<Registry>,
        bus: Option<Arc<EventBus>>,
    ) -> Arc<Self> {
        Self::with_rate(runner, registry, bus, HookRate::default())
    }

    pub fn with_rate(
        runner: Arc<Runner>,
        registry: Arc<Registry>,
        bus: Option<Arc<EventBus>>,
        rate: HookRate,
    ) -> Arc<Self> {
        Arc::new(Self {
            runner,
            registry,
            bus,
            stop: CancellationToken::new(),
            tasks: Mutex::default(),
            rate: RwLock::new(rate),
            starts: Mutex::default(),
            dispatch_lock: Mutex::default(),
            disabled: Mutex::default(),
        })
    }

    /// Changes the breaker's limit. The hub uses the default; a test needs a small one.
    pub fn set_rate(&self, rate: HookRate) {
        *self.rate.write().unwrap() = rate;
    }

    /// Listens from now on and returns the listening task. Subscribing is what makes a
    /// restart replay nothing: an event published before this call is never handed out, and
    /// the event log is not read back (docs/EXTENSIONS.md §3).
    pub fn start(self: &Arc<Self>, shutdown: CancellationToken) -> JoinHandle<()> {
        let this = self.clone();
        let events = this.bus.as_ref().map(|bus| bus.subscribe());
        tokio::spawn(async move {
            let Some(mut events) = events else {
                return;
            };
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    received = events.recv() => match received {
                        Ok(event) => this.dispatch(&event),
                        Err(broadcast::error::RecvError::Lagged(dropped)) => {
                            tracing::warn!(dropped, "event hooks missed events: the bus outran them");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                }
            }
            this.stop.cancel();
        })
    }

    /// Ends the running hook processes and waits for them to be gone.
    pub async fn shutdown(&self) {
        self.stop.cancel();
        let tasks: Vec<JoinHandle<()>> = {
            let mut tasks = self.tasks.lock().unwrap();
            std::mem::take(&mut *tasks)
        };
        let _ = tokio::time::timeout(Duration::from_secs(10), async {
            for task in tasks {
                let _ = task.await;
            }
        })
        .await;
    }

    /// Starts one process per extension that declares a hook for this event.
    fn dispatch(self: &Arc<Self>, event: &Event) {
        let _dispatch = self.dispatch_lock.lock().unwrap();
        self.prune();
        let name = event.kind.type_name();
        if name.starts_with("extension.") {
            return;
        }
        let Ok(value) = serde_json::to_value(event) else {
            tracing::warn!(event = name, "cannot serialize an event for its hooks");
            return;
        };
        for info in self.registry.list() {
            if !info.enabled {
                continue;
            }
            let extension = info.name.clone();
            let Ok(lease) = self.registry.acquire(&extension) else {
                continue;
            };
            let Some(hook) = lease.enabled.manifest.on.iter().find(|h| h.event == name) else {
                continue;
            };
            // Before anything is acquired: a breaker-dropped event must leave no trace but
            // its one log line. A start only counts once it really started, so an event the
            // concurrency limits turn away does not use up the allowance.
            if !self.allows(&extension) {
                self.refuse(&extension, name);
                continue;
            }
            let timeout = Duration::from_secs(hook.timeout_secs.into());
            let command = hook.command.clone();
            let stdin = serde_json::to_vec(&json!({
                "event": value,
                "caller": { "kind": "event" },
            }))
            .unwrap_or_default();
            let spec = CommandSpec {
                extension: extension.clone(),
                lease,
                command,
                stdin: Some(stdin),
                timeout: Some(timeout),
                stdout: OutputCapture::LogOnly,
                // The same slots as tool calls: a burst of events is dropped, not queued.
                limits: true,
                label: format!("hook {name}"),
                cancel: self.stop.clone(),
                // A disable ends a running hook after the tool grace, not at its timeout.
                disabled: Some(self.disable_token(&extension)),
            };
            match self.runner.start_command(spec) {
                Ok(run) => {
                    // Counted only now: a command the concurrency limits refused never ran,
                    // and must not eat into the breaker's allowance.
                    self.record(&extension);
                    let task = tokio::spawn(async move {
                        let _ = run.wait().await;
                    });
                    self.tasks.lock().unwrap().push(task);
                }
                Err(error) => self
                    .runner
                    .note(&extension, &format!("hook {name} dropped: {error}")),
            }
        }
    }

    /// Whether another start fits in the window. It does not count anything: the start is
    /// recorded only after it actually happened (see [`Self::record`]).
    fn allows(&self, extension: &str) -> bool {
        let (limit, window) = self.rate();
        let now = Instant::now();
        let mut starts = self.starts.lock().unwrap();
        let entry = starts.entry(extension.to_owned()).or_default();
        entry.at.retain(|at| now.duration_since(*at) < window);
        entry.at.len() < limit
    }

    /// Counts a start that happened.
    fn record(&self, extension: &str) {
        let (_, window) = self.rate();
        let now = Instant::now();
        let mut starts = self.starts.lock().unwrap();
        let entry = starts.entry(extension.to_owned()).or_default();
        entry.at.retain(|at| now.duration_since(*at) < window);
        entry.at.push_back(now);
    }

    /// Drops an event because the window is full; one log line per window.
    fn refuse(&self, extension: &str, event: &str) {
        let (limit, window) = self.rate();
        let now = Instant::now();
        let mut starts = self.starts.lock().unwrap();
        let entry = starts.entry(extension.to_owned()).or_default();
        let log = entry
            .logged_at
            .is_none_or(|at| now.duration_since(at) >= window);
        if log {
            entry.logged_at = Some(now);
        }
        drop(starts);
        if log {
            self.runner.note(
                extension,
                &format!(
                    "hook {event} dropped: {} starts in {}s is the limit (the circuit breaker); this line is written once per window",
                    limit,
                    window.as_secs()
                ),
            );
        }
    }

    fn rate(&self) -> (usize, Duration) {
        let rate = self.rate.read().unwrap();
        (rate.limit, rate.window)
    }

    /// The token that ends this extension's running hooks when it is disabled. One token per
    /// extension; after it was cancelled (the extension was disabled) a fresh one is made.
    fn disable_token(&self, extension: &str) -> CancellationToken {
        let mut tokens = self.disabled.lock().unwrap();
        match tokens.get(extension) {
            Some(token) if !token.is_cancelled() => token.clone(),
            _ => {
                let token = self.stop.child_token();
                tokens.insert(extension.to_owned(), token.clone());
                token
            }
        }
    }

    fn prune(&self) {
        let mut tasks = self.tasks.lock().unwrap();
        tasks.retain(|task| !task.is_finished());
    }
}
