//! Event hooks: one process per matching event, observe only (docs/EXTENSIONS.md §3).
//!
//! A hook never blocks the event and never changes it: the process is started after the
//! event has been stored and broadcast, its exit code and output are ignored, and a failure
//! is only a line in the extension's log. Nothing is retried and nothing is replayed after a
//! restart — only events published while this hub runs are seen.
//!
//! `extension.*` events are refused when the description file is read (manifest.rs), and
//! skipped here as well, so two extensions cannot trigger each other in a loop.
//!
//! Hook processes share the tool concurrency limits: when they are full the event is dropped
//! and the log says so, rather than queued.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossagents_core::{Event, EventBus};
use serde_json::json;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::runner::{CommandSpec, OutputCapture};
use super::{Registry, Runner};

/// Runs the enabled extensions' `[[on]]` hooks for events this hub publishes.
pub struct Hooks {
    runner: Arc<Runner>,
    registry: Arc<Registry>,
    bus: Option<Arc<EventBus>>,
    /// Ends every hook process; set from the hub's shutdown token by [`Self::start`].
    stop: CancellationToken,
    /// Hook processes still running, so shutdown can wait for them.
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Hooks {
    pub fn new(
        runner: Arc<Runner>,
        registry: Arc<Registry>,
        bus: Option<Arc<EventBus>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            runner,
            registry,
            bus,
            stop: CancellationToken::new(),
            tasks: Mutex::default(),
        })
    }

    /// Listens from now on and returns the listening task. Subscribing is what makes a
    /// restart replay nothing: an event published before this call is never handed out, and
    /// the event log is not read back (docs/EXTENSIONS.md §3).
    pub fn start(self: &Arc<Self>, shutdown: CancellationToken) -> JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move {
            let Some(bus) = this.bus.clone() else {
                return;
            };
            let mut events = bus.subscribe();
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
            let wanted = info.enabled
                && info
                    .current
                    .as_ref()
                    .is_some_and(|version| version.hooks.iter().any(|hook| hook.event == name));
            if !wanted {
                continue;
            }
            let extension = info.name.clone();
            let Ok(enabled) = self.registry.enabled(&extension) else {
                continue;
            };
            let Some(hook) = enabled.manifest.on.iter().find(|h| h.event == name) else {
                continue;
            };
            let timeout = Duration::from_secs(hook.timeout_secs.into());
            let command = hook.command.clone();
            let stdin = serde_json::to_vec(&json!({
                "event": value,
                "caller": { "kind": "event" },
            }))
            .unwrap_or_default();
            let Ok(lease) = self.registry.acquire(&extension) else {
                continue;
            };
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
            };
            match self.runner.start_command(spec) {
                Ok(run) => {
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

    fn prune(&self) {
        let mut tasks = self.tasks.lock().unwrap();
        tasks.retain(|task| !task.is_finished());
    }
}
