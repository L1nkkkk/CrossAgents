//! "Check now" on a plugin's row: one short session on its CLI, to see whether this version
//! still talks with CrossAgents (`crate::cli_runs`).
//!
//! `POST /v1/plugins/{name}/self-check` answers at once; the session runs in the background
//! in an empty temporary directory, with every permission denied, and asks for one word. The
//! first event the driver parses from the CLI records the version as working; a run that
//! fails before any such event, in a way only a protocol change explains, records it as
//! broken. Quotas, networks and models record nothing. The end publishes `plugin.changed`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AgentStatus, Driver, EventBus, EventKind, NameSource, PermissionPolicy, Role,
    SessionSpec,
};
use tokio::sync::{Mutex, mpsc};

use crate::cli_runs::{CliRuns, Witness};

/// How long the one turn may take before the check gives up (recorded as nothing).
const TURN_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct SelfCheck {
    running: Mutex<HashSet<String>>,
}

impl SelfCheck {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a check of `plugin` is running now.
    pub async fn running(&self, plugin: &str) -> bool {
        self.running.lock().await.contains(plugin)
    }

    /// Starts a check of `driver`'s CLI in the background; false when one is already
    /// running for it or the CLI reports no version to record against.
    pub async fn start(
        self: &Arc<Self>,
        bus: Arc<EventBus>,
        runs: Arc<CliRuns>,
        driver: Arc<dyn Driver>,
        model: Option<String>,
    ) -> bool {
        let plugin = driver.name().to_owned();
        let Some(located) = driver.cli_status().await.and_then(|status| status.located) else {
            return false;
        };
        let Some(version) = located.version else {
            return false;
        };
        if !self.running.lock().await.insert(plugin.clone()) {
            return false;
        }
        let path = located.path.display().to_string();
        let this = self.clone();
        let _ = bus
            .publish(EventKind::PluginChanged {
                name: plugin.clone(),
            })
            .await;
        tokio::spawn(async move {
            let witness = Witness::new(runs, plugin.clone(), path, version);
            check(driver, model, witness).await;
            this.running.lock().await.remove(&plugin);
            let _ = bus.publish(EventKind::PluginChanged { name: plugin }).await;
        });
        true
    }
}

async fn check(driver: Arc<dyn Driver>, model: Option<String>, mut witness: Witness) {
    let plugin = driver.name().to_owned();
    let cwd = std::env::temp_dir().join(format!("crossagents-self-check-{}", uuid::Uuid::new_v4()));
    if let Err(err) = std::fs::create_dir_all(&cwd) {
        tracing::warn!(%plugin, error = %err, "cannot make a directory for the self-check");
        return;
    }
    let (events, mut rx) = mpsc::unbounded_channel();
    let spec = SessionSpec {
        agent: Agent {
            id: AgentId::new(),
            name: "self-check".into(),
            name_source: NameSource::Chosen,
            plugin: plugin.clone(),
            driver: None,
            model,
            role: Role::Worker,
            home: PathBuf::from(&cwd),
            cwd: cwd.clone(),
            config: serde_json::json!({}),
            status: AgentStatus::Stopped,
            created_at: Utc::now(),
            profile: Default::default(),
            topic: None,
            archived_at: None,
        },
        mcp_servers: Vec::new(),
        permission: PermissionPolicy::Deny,
        permissions: None,
        command: None,
        instructions: None,
        env: Vec::new(),
        events,
    };
    // The events are read while the turn runs, so the handshake is seen as it comes.
    let watcher = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            witness.observe(&event);
        }
        witness
    });
    let failure = match driver.open(spec).await {
        Ok(session) => {
            let failure =
                match tokio::time::timeout(TURN_TIMEOUT, session.prompt("Reply with OK.")).await {
                    Ok(Ok(_)) => None,
                    Ok(Err(err)) => Some(err.to_string()),
                    Err(_) => {
                        let _ = session.interrupt().await;
                        None
                    }
                };
            session.shutdown().await;
            drop(session);
            failure
        }
        Err(err) => Some(err.to_string()),
    };
    // The session's sender is gone with it; the watcher ends once the events are read.
    let witness = tokio::time::timeout(Duration::from_secs(5), watcher).await;
    match (witness, failure) {
        (Ok(Ok(mut witness)), Some(message)) => {
            if !witness.recorded() && !witness.conclude(&message) {
                tracing::info!(%plugin, message, "self-check failed for a reason that is not the CLI version");
            }
        }
        (Ok(Ok(witness)), None) if !witness.recorded() => {
            tracing::info!(%plugin, "self-check ended without an event from the CLI");
        }
        _ => {}
    }
    let _ = std::fs::remove_dir_all(&cwd);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_plugin_without_a_cli_is_not_checked() {
        struct NoCli;
        #[async_trait::async_trait]
        impl Driver for NoCli {
            fn name(&self) -> &str {
                "acp"
            }
            fn kind(&self) -> crossagents_core::DriverKind {
                crossagents_core::DriverKind::AcpGeneric
            }
            fn capabilities(&self) -> crossagents_core::Capabilities {
                Default::default()
            }
            fn models(&self) -> Vec<String> {
                Vec::new()
            }
            async fn open(
                &self,
                _spec: SessionSpec,
            ) -> Result<Box<dyn crossagents_core::DriverSession>, crossagents_core::CoreError>
            {
                unreachable!()
            }
        }
        let store = Arc::new(
            crossagents_store_sqlite::SqliteStore::open_in_memory()
                .await
                .unwrap(),
        );
        let bus = Arc::new(EventBus::new(store as Arc<dyn crossagents_core::Store>));
        let check = Arc::new(SelfCheck::new());
        assert!(
            !check
                .start(bus, Arc::new(CliRuns::in_memory()), Arc::new(NoCli), None)
                .await
        );
        assert!(!check.running("acp").await);
    }
}
