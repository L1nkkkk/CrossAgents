//! Glue between the runtime and the orchestrator: per-session agent tokens, mounting the
//! `crossagents mcp` server into every session, and the [`AgentHost`] the orchestrator
//! uses to manage agents.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AgentKey, AgentStatus, CoreError, Envelope, EventBus, EventKind, McpServerSpec,
    MessageId, MessageSource, Role,
};
use crossagents_orchestrator::AgentHost;
use tokio::sync::RwLock;

use crate::runtime::{Runtime, SessionExtras, SessionSetup};

pub const URL_ENV: &str = "CROSSAGENTS_URL";
pub const TOKEN_ENV: &str = "CROSSAGENTS_AGENT_TOKEN";

/// Tokens identifying agents to the `/v1/me` API. One per live session.
#[derive(Default)]
pub struct AgentTokens {
    by_token: RwLock<HashMap<String, AgentId>>,
}

impl AgentTokens {
    /// Issues a fresh token for `agent`, revoking its previous one.
    pub async fn issue(&self, agent: AgentId) -> String {
        let token = format!(
            "ca-agent-{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let mut map = self.by_token.write().await;
        map.retain(|_, id| *id != agent);
        map.insert(token.clone(), agent);
        token
    }

    pub async fn agent(&self, token: &str) -> Option<AgentId> {
        self.by_token.read().await.get(token).copied()
    }
}

/// Mounts the collaboration MCP server into each agent session.
pub struct CollabSetup {
    pub tokens: Arc<AgentTokens>,
    pub accounts: Arc<crate::accounts::Accounts>,
    /// Command that runs the MCP server, e.g. `["/path/crossagents", "mcp"]`.
    pub mcp_command: Vec<String>,
    /// The hub's API base URL, known once the listener is bound.
    pub url: OnceLock<String>,
}

#[async_trait]
impl SessionSetup for CollabSetup {
    async fn prepare(&self, agent: &Agent) -> Result<SessionExtras, CoreError> {
        let agent_env = self.accounts.agent_env(agent).await?;
        let Some((program, args)) = self.mcp_command.split_first() else {
            return Ok(SessionExtras {
                instructions: agent.profile.instructions(&agent.name),
                env: agent_env,
                ..SessionExtras::default()
            });
        };
        let url = self
            .url
            .get()
            .cloned()
            .ok_or_else(|| CoreError::Agent("hub URL not known yet".into()))?;
        let token = self.tokens.issue(agent.id).await;
        let env = BTreeMap::from([(URL_ENV.to_owned(), url), (TOKEN_ENV.to_owned(), token)]);
        Ok(SessionExtras {
            mcp_servers: vec![McpServerSpec {
                name: "crossagents".into(),
                command: PathBuf::from(program),
                args: args.to_vec(),
                env,
            }],
            instructions: agent.profile.instructions(&agent.name),
            env: agent_env,
        })
    }
}

/// The hub as seen by the orchestrator.
pub struct HubHost {
    pub runtime: Arc<Runtime>,
    pub bus: Arc<EventBus>,
}

#[async_trait]
impl AgentHost for HubHost {
    async fn send(
        &self,
        agent: AgentId,
        from: MessageSource,
        text: String,
    ) -> Result<(), CoreError> {
        self.runtime
            .send(&AgentKey::Id(agent), text, from)
            .await
            .map(|_| ())
    }

    async fn send_envelope(
        &self,
        agent: AgentId,
        envelope: Envelope,
        now: bool,
    ) -> Result<(), CoreError> {
        let key = AgentKey::Id(agent);
        if now {
            self.runtime.send_now_envelope(&key, envelope).await
        } else {
            self.runtime.send_envelope(&key, envelope).await
        }
        .map(|_| ())
    }

    async fn withdraw(&self, agent: AgentId, message: MessageId) -> Result<(), CoreError> {
        self.runtime.withdraw(&AgentKey::Id(agent), message).await
    }

    async fn notify(&self, agent: AgentId, topic: &str, line: &str) -> Result<(), CoreError> {
        self.runtime.notify(&AgentKey::Id(agent), topic, line).await
    }

    async fn clone_agent(
        &self,
        template: AgentId,
        name: &str,
        role: Role,
        cwd: &Path,
    ) -> Result<Agent, CoreError> {
        let template = self.bus.store().get_agent(&AgentKey::Id(template)).await?;
        let agent = Agent {
            id: AgentId::new(),
            name: name.to_owned(),
            plugin: template.plugin,
            driver: template.driver,
            model: template.model,
            role,
            cwd: cwd.to_path_buf(),
            config: template.config,
            status: AgentStatus::Stopped,
            created_at: Utc::now(),
            // Every clone of "Ada the designer" is Ada the designer.
            profile: template.profile,
        };
        self.bus.store().insert_agent(&agent).await?;
        self.bus
            .publish(EventKind::AgentCreated {
                agent: agent.clone(),
            })
            .await?;
        Ok(agent)
    }

    async fn remove_agent(&self, agent: AgentId) -> Result<(), CoreError> {
        let record = self.bus.store().get_agent(&AgentKey::Id(agent)).await?;
        self.runtime.stop(&AgentKey::Id(agent)).await?;
        self.bus.store().delete_agent(agent).await?;
        self.bus
            .publish(EventKind::AgentRemoved {
                id: agent,
                name: record.name,
            })
            .await?;
        Ok(())
    }
}
