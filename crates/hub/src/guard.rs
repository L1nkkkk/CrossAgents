//! The hub's answer to a guarded agent's permission requests (docs/tasks/DOGFOOD.md §7):
//! what is safe goes ahead at once, what is dangerous waits for the person.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use crossagents_core::{Agent, PermissionDecision, PermissionHandler, PermissionRequest};
use crossagents_orchestrator::Orchestrator;
use crossagents_orchestrator::approval::danger;

pub struct Guard {
    /// Known once the hub has started: sessions taken over at startup get their guard
    /// before the orchestrator exists.
    pub orchestrator: Arc<OnceLock<Arc<Orchestrator>>>,
    pub agent: Agent,
}

#[async_trait]
impl PermissionHandler for Guard {
    async fn decide(&self, request: &PermissionRequest) -> PermissionDecision {
        let Some(why) = danger(&request.kind, &request.title) else {
            return PermissionDecision::Allow;
        };
        let Some(orchestrator) = self.orchestrator.get() else {
            return PermissionDecision::Deny;
        };
        if !orchestrator.guards(&self.agent).await {
            return PermissionDecision::Allow;
        }
        if orchestrator
            .ask_approval(&self.agent, &request.title, why)
            .await
        {
            PermissionDecision::Allow
        } else {
            PermissionDecision::Deny
        }
    }
}
