//! Looking for the vendor CLIs again while the hub runs (docs/DESIGN.md §9.3).
//!
//! `POST /v1/plugins/refresh` answers at once and does the work in the background: the lookup
//! and its version probe are forgotten, then each CLI is asked again, in parallel. Every
//! probe that ends publishes `plugin.changed`, so a client re-reads `GET /v1/plugins` and the
//! row it was showing ("not installed", "version unknown") becomes the one the machine has
//! now -- no hub restart.

use std::collections::HashSet;
use std::sync::Arc;

use crossagents_core::{Driver, EventBus, EventKind};
use tokio::sync::Mutex;

/// The plugins being looked for right now: a second request for the same one joins the
/// probe already out instead of starting another subprocess.
#[derive(Default)]
pub struct Refresher {
    probing: Mutex<HashSet<String>>,
}

impl Refresher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets what the lookup and the version cache remembered for `driver`, then probes in
    /// the background; returns once the probe is started (never waits for it). A plugin
    /// already being probed is left alone.
    pub async fn start(self: &Arc<Self>, bus: Arc<EventBus>, driver: Arc<dyn Driver>) {
        let plugin = driver.name().to_owned();
        if !self.claim(&plugin).await {
            return;
        }
        // The file the last lookup named may be gone or replaced, and a CLI installed since
        // may be in a directory the cache never saw.
        driver.refresh_cli();
        let this = self.clone();
        tokio::spawn(async move {
            driver.probe_cli().await;
            let _ = bus
                .publish(EventKind::PluginChanged {
                    name: plugin.clone(),
                })
                .await;
            this.release(&plugin).await;
        });
    }

    /// Reserves a plugin; false when a probe for it is already out.
    async fn claim(&self, plugin: &str) -> bool {
        self.probing.lock().await.insert(plugin.to_owned())
    }

    async fn release(&self, plugin: &str) {
        self.probing.lock().await.remove(plugin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_probe_already_out_is_not_started_again() {
        let refresher = Refresher::new();
        assert!(refresher.claim("cursor").await);
        // A second click while that probe runs joins it instead of starting another.
        assert!(!refresher.claim("cursor").await);
        // Another plugin is its own probe.
        assert!(refresher.claim("claude").await);
        refresher.release("cursor").await;
        assert!(refresher.claim("cursor").await);
    }
}
