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
    MessageId, MessageSource, Role, RunId,
};
use crossagents_orchestrator::workspace::worktree_env;
use crossagents_orchestrator::{AgentHost, Route};
use tokio::sync::RwLock;

use crate::runtime::{Runtime, SessionExtras, SessionSetup};

pub const URL_ENV: &str = "CROSSAGENTS_URL";
pub const TOKEN_ENV: &str = "CROSSAGENTS_AGENT_TOKEN";

/// Tokens identifying agents to the `/v1/me` API. One per live session. Kept in a file
/// when given one: the tools of an agent taken over by the next hub keep their token.
#[derive(Default)]
pub struct AgentTokens {
    by_token: RwLock<HashMap<String, AgentId>>,
    file: Option<PathBuf>,
}

impl AgentTokens {
    /// Tokens kept in `file`, starting with the ones a previous hub left there.
    pub fn load(file: PathBuf) -> Self {
        let by_token = std::fs::read(&file)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            by_token: RwLock::new(by_token),
            file: Some(file),
        }
    }

    fn save(&self, map: &HashMap<String, AgentId>) {
        if let Some(file) = &self.file
            && let Err(err) =
                crate::write_private(file, &serde_json::to_string(map).unwrap_or_default())
        {
            tracing::warn!(error = %err, "cannot save agent tokens");
        }
    }

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
        self.save(&map);
        token
    }

    /// Keeps only the tokens of `agents` (the ones still running).
    pub async fn retain(&self, agents: &std::collections::HashSet<AgentId>) {
        let mut map = self.by_token.write().await;
        map.retain(|_, id| agents.contains(id));
        self.save(&map);
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
    /// Root of run and task worktrees: agents working there build into their own target
    /// directory (docs/tasks/DOGFOOD.md §1).
    pub worktrees: PathBuf,
    /// Decides guarded agents' permission requests; set once the orchestrator exists.
    pub orchestrator: Arc<OnceLock<Arc<crossagents_orchestrator::Orchestrator>>>,
}

impl CollabSetup {
    /// The account's environment, plus `CARGO_TARGET_DIR` for an agent in a worktree.
    async fn agent_env(&self, agent: &Agent) -> Result<Vec<(String, String)>, CoreError> {
        let mut env = self.accounts.agent_env(agent).await?;
        env.extend(worktree_env(&self.worktrees, &agent.cwd));
        Ok(env)
    }
}

#[async_trait]
impl SessionSetup for CollabSetup {
    async fn environment(&self, agent: &Agent) -> Result<Vec<(String, String)>, CoreError> {
        self.agent_env(agent).await
    }

    async fn prepare(&self, agent: &Agent) -> Result<SessionExtras, CoreError> {
        let agent_env = self.agent_env(agent).await?;
        let (permission, permissions) = self.permissions(agent).await;
        let Some((program, args)) = self.mcp_command.split_first() else {
            return Ok(SessionExtras {
                instructions: Some(instructions(agent)),
                env: agent_env,
                permission,
                permissions,
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
            instructions: Some(instructions(agent)),
            env: agent_env,
            permission,
            permissions,
        })
    }

    async fn permissions(
        &self,
        agent: &Agent,
    ) -> (
        Option<crossagents_core::PermissionPolicy>,
        Option<Arc<dyn crossagents_core::PermissionHandler>>,
    ) {
        let guard = Arc::new(crate::guard::Guard {
            orchestrator: self.orchestrator.clone(),
            agent: agent.clone(),
        });
        // Before the orchestrator exists (sessions taken over at startup) the policy cannot
        // be told yet; the guard itself checks once it asks.
        let Some(orchestrator) = self.orchestrator.get() else {
            return (None, Some(guard));
        };
        if !orchestrator.guards(agent).await {
            return (None, None);
        }
        (Some(crossagents_core::PermissionPolicy::Guard), Some(guard))
    }
}

/// Who the agent is (name, model, card), then how it works with the hub (its role's guide),
/// then the project's rules when its own CLI would not read them (docs/tasks/DOGFOOD.md §6).
fn instructions(agent: &Agent) -> String {
    let guide = crossagents_orchestrator::protocol::guide_for(agent);
    match crate::rules::for_agent(agent) {
        Some(rules) => format!("{}\n\n{guide}\n\n{rules}", agent.identity()),
        None => format!("{}\n\n{guide}", agent.identity()),
    }
}

/// The hub as seen by the orchestrator.
pub struct HubHost {
    pub runtime: Arc<Runtime>,
    pub bus: Arc<EventBus>,
    pub accounts: Arc<crate::accounts::Accounts>,
    pub models: Arc<crate::models::ModelChoice>,
    /// The hub's home, for files kept per agent (attachments).
    pub home: PathBuf,
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

    async fn interrupt(&self, agent: AgentId) -> Result<(), CoreError> {
        match self.runtime.cancel(&AgentKey::Id(agent)).await {
            Err(CoreError::Conflict(_)) => Ok(()), // not running: nothing to stop
            other => other,
        }
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
        self.clone_agent_on(template, name, role, cwd, None).await
    }

    async fn clone_agent_on(
        &self,
        template: AgentId,
        name: &str,
        role: Role,
        cwd: &Path,
        account: Option<&str>,
    ) -> Result<Agent, CoreError> {
        let mut template = self.bus.store().get_agent(&AgentKey::Id(template)).await?;
        if let Some(account) = account {
            if !template.config.is_object() {
                template.config = serde_json::json!({});
            }
            template.config["account"] = account.into();
        }
        let agent = Agent {
            id: AgentId::new(),
            name_source: crossagents_core::NameSource::Chosen,
            topic: None,
            archived_at: None,
            home: template.home().to_path_buf(),
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

    async fn route(&self, template: AgentId, team: &[AgentId], limited: Option<AgentId>) -> Route {
        crate::routing::route(self, template, team, limited)
            .await
            .unwrap_or(Route::Same)
    }

    async fn spawn_agent(&self, agent: Agent) -> Result<Agent, CoreError> {
        if !self.runtime.plugins().contains(&agent.plugin) {
            return Err(CoreError::Conflict(format!(
                "unknown plugin {:?} (available: {})",
                agent.plugin,
                self.runtime.plugins().names().join(", ")
            )));
        }
        let (name, name_source) = crate::identity::free_name(
            self.bus.store().as_ref(),
            Some(agent.name.as_str()),
            true,
            &self.denied_name_words(),
        )
        .await?;
        let agent = Agent {
            profile: agent.profile.normalized()?,
            name,
            name_source,
            ..agent
        };
        self.bus.store().insert_agent(&agent).await?;
        self.bus
            .publish(EventKind::AgentCreated {
                agent: agent.clone(),
            })
            .await?;
        Ok(agent)
    }

    async fn available_models(&self, plugin: &str) -> Result<Vec<String>, CoreError> {
        let Some(driver) = self
            .runtime
            .plugins()
            .defaults()
            .into_iter()
            .find(|d| d.name() == plugin)
        else {
            return Ok(Vec::new());
        };
        if !driver.installed() {
            return Ok(Vec::new());
        }
        // A plugin without accounts (an ACP agent) runs whatever it lists.
        if !self.accounts.manages(plugin) {
            return Ok(self.models.filter(plugin, driver.models()));
        }
        let accounts: Vec<_> = self
            .bus
            .store()
            .list_accounts()
            .await?
            .into_iter()
            .filter(|a| a.provider == plugin)
            .collect();
        if accounts.is_empty() {
            return Ok(Vec::new());
        }
        let mut models = Vec::new();
        for account in &accounts {
            if let Ok(list) = self.accounts.models(&account.id).await {
                models.extend(list.into_iter().map(|m| m.id));
            }
        }
        // An account that has not reported its models yet: the plugin's own list.
        if models.is_empty() {
            models = driver.models();
        }
        models.sort();
        models.dedup();
        Ok(self.models.filter(plugin, models))
    }

    async fn archive_agent(&self, agent: AgentId) -> Result<(), CoreError> {
        self.runtime
            .archive(&AgentKey::Id(agent), true)
            .await
            .map(|_| ())
    }

    fn plugin_names(&self) -> Vec<String> {
        self.runtime
            .plugins()
            .defaults()
            .into_iter()
            .map(|d| d.name().to_owned())
            .collect()
    }

    fn denied_name_words(&self) -> Vec<String> {
        crate::identity::denied_words(self.runtime.plugins())
    }

    async fn set_model(&self, agent: AgentId, model: Option<String>) -> Result<Agent, CoreError> {
        self.runtime
            .set_model(&AgentKey::Id(agent), model, None)
            .await
    }

    async fn usage_report(&self, run: Option<RunId>) -> Result<serde_json::Value, CoreError> {
        use crossagents_api_types::UsageGroup;
        let store = self.bus.store().as_ref();
        let by_run = crate::usage::compute(store, UsageGroup::Run).await?;
        let by_agent = crate::usage::compute(store, UsageGroup::Agent).await?;
        let spent = match run {
            Some(run) => {
                serde_json::to_value(by_run.rows.iter().find(|r| r.key == run.to_string()))
                    .unwrap_or_default()
            }
            None => serde_json::json!({ "total_cost_usd": by_run.total_cost_usd }),
        };
        let mut quota = Vec::new();
        for account in store.list_accounts().await? {
            if let Ok(status) = self.accounts.status(&account.id).await {
                quota.push(serde_json::json!({
                    "account": account.id,
                    "plan": status.plan,
                    "windows": status.rate_limits,
                    "limit_reached": status.limit_reached,
                    "credits": status.credits,
                }));
            }
        }
        Ok(serde_json::json!({
            "run": run,
            "spent": spent,
            "by_agent": by_agent.rows,
            "accounts": quota,
            "note": "Costs are what agents report; Codex reports quota windows rather than dollars.",
        }))
    }

    async fn take_inbox(&self, agent: AgentId) -> Result<Vec<serde_json::Value>, CoreError> {
        Ok(self
            .runtime
            .take_external(agent)
            .await
            .into_iter()
            .map(|(id, from, text)| serde_json::json!({ "message_id": id, "from": from, "text": text }))
            .collect())
    }

    async fn stop_agent(&self, agent: AgentId) -> Result<(), CoreError> {
        self.runtime.stop(&AgentKey::Id(agent)).await
    }

    async fn place(&self, agent: AgentId, dir: &Path, carry: bool) -> Result<(), CoreError> {
        self.runtime
            .place(&AgentKey::Id(agent), dir, carry)
            .await
            .map(|_| ())
    }

    async fn send_home(&self, agent: AgentId) -> Result<bool, CoreError> {
        self.runtime.send_home(&AgentKey::Id(agent)).await
    }

    async fn serving_person(&self, agent: AgentId) -> bool {
        self.runtime.serving_person(agent).await
    }

    async fn restore_agent(&self, agent: AgentId) -> Result<(), CoreError> {
        self.runtime
            .archive(&AgentKey::Id(agent), false)
            .await
            .map(|_| ())
    }

    async fn can_move(&self, agent: &Agent) -> bool {
        self.runtime
            .capabilities(agent)
            .await
            .is_some_and(|c| c.move_cwd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runs_worktrees_share_one_target_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("worktrees");
        let task = root.join("ab12cd34").join("auth");
        let integration = root.join("ab12cd34").join("integration");
        let other = root.join("ff00ee11").join("integration");
        for d in [&task, &integration, &other] {
            std::fs::create_dir_all(d).unwrap();
        }
        let (key, value) = worktree_env(&root, &task).unwrap();
        assert_eq!(key, "CARGO_TARGET_DIR");
        let target = PathBuf::from(&value);
        assert!(target.is_absolute());
        assert!(
            target.ends_with(Path::new("ab12cd34").join("target")),
            "{value}"
        );
        // The run's checkout builds into the same place; another run does not.
        assert_eq!(worktree_env(&root, &integration).unwrap().1, value);
        assert_ne!(worktree_env(&root, &other).unwrap().1, value);
        // Chats in a person's checkout, and the root itself, keep the inherited setting.
        assert_eq!(worktree_env(&root, dir.path()), None);
        assert_eq!(worktree_env(&root, &root), None);
    }
}
