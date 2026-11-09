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
use crossagents_orchestrator::{AgentHost, RouteDecision, RouteMode};
use crossagents_plugins::PluginRegistry;
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
    /// The plugins the hub knows: their descriptions' CLI floors and the drivers that report
    /// a CLI's version (docs/research/multica.md §2.9).
    pub plugins: PluginRegistry,
    /// Command that runs the MCP server, e.g. `["/path/crossagents", "mcp"]`.
    pub mcp_command: Vec<String>,
    /// Further MCP servers from `[mcp.<name>]`, mounted into every session besides the
    /// collaboration one (docs/DESIGN.md §9.3).
    pub extra_mcp: Vec<McpServerSpec>,
    /// The hub's API base URL, known once the listener is bound.
    pub url: OnceLock<String>,
    /// Root of run and task worktrees: agents working there build into their own target
    /// directory (docs/tasks/DOGFOOD.md §1).
    pub worktrees: PathBuf,
    /// `<home>/shims/<version>`, the shims a run worktree's `PATH` starts with
    /// (docs/tasks/LOCKS.md §4.6); `None` leaves `PATH` alone.
    pub shims: Option<PathBuf>,
    /// Decides guarded agents' permission requests; set once the orchestrator exists.
    pub orchestrator: Arc<OnceLock<Arc<crossagents_orchestrator::Orchestrator>>>,
}

impl CollabSetup {
    /// The account's environment, plus what the project declares for an agent in a run
    /// worktree; the account's wins (docs/tasks/TOOLCHAIN.md §3.2).
    async fn agent_env(&self, agent: &Agent) -> Result<Vec<(String, String)>, CoreError> {
        let account = self.accounts.agent_env(agent).await?;
        Ok(with_project_env(
            account,
            worktree_env(&self.worktrees, &agent.cwd, self.shims.as_deref()),
        ))
    }
}

/// `account` plus each of `project` it does not set itself (names compared as the platform
/// does).
fn with_project_env(
    mut account: Vec<(String, String)>,
    project: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let same = |a: &str, b: &str| {
        if cfg!(windows) {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    };
    for (name, value) in project {
        if account.iter().any(|(n, _)| same(n, &name)) {
            tracing::warn!(%name, "the account sets it; the project's value is not used");
        } else {
            account.push((name, value));
        }
    }
    account
}

#[async_trait]
impl SessionSetup for CollabSetup {
    async fn environment(&self, agent: &Agent) -> Result<Vec<(String, String)>, CoreError> {
        self.agent_env(agent).await
    }

    async fn prepare(&self, agent: &Agent) -> Result<SessionExtras, CoreError> {
        // An existing agent does not open a session on a CLI below its description's floor
        // either: the same gate `POST /v1/agents` puts on new ones, for agents made before it
        // (docs/research/multica.md §2.9). A CLI that has not answered its version starts as
        // before.
        crate::version_gate::check(agent, &self.plugins).await?;
        let agent_env = self.agent_env(agent).await?;
        let (permission, permissions) = self.permissions(agent).await;
        let (lessons, groups) = match self.orchestrator.get() {
            Some(orchestrator) => (
                orchestrator.lessons_hint(agent).await,
                orchestrator.groups(agent).await,
            ),
            None => (
                None,
                crossagents_orchestrator::messaging::groups_of(agent, None),
            ),
        };
        let mut instructions = instructions(agent, &groups, lessons.as_deref());
        // Enabled extensions' own instructions come last and only add to the hub's.
        if let Some(orchestrator) = self.orchestrator.get() {
            instructions = crossagents_orchestrator::protocol::with_extension_prompts(
                instructions,
                &orchestrator.extension_prompts(&groups),
            );
        }
        let Some((program, args)) = self.mcp_command.split_first() else {
            return Ok(SessionExtras {
                mcp_servers: self.extra_mcp.clone(),
                instructions: Some(instructions),
                env: agent_env,
                permission,
                permissions,
            });
        };
        let url = self
            .url
            .get()
            .cloned()
            .ok_or_else(|| CoreError::Agent("hub URL not known yet".into()))?;
        let token = self.tokens.issue(agent.id).await;
        let env = BTreeMap::from([(URL_ENV.to_owned(), url), (TOKEN_ENV.to_owned(), token)]);
        let mut mcp_servers = vec![McpServerSpec {
            name: "crossagents".into(),
            command: PathBuf::from(program),
            args: args.to_vec(),
            env,
        }];
        // The hub's own tools come first: an `[mcp.<name>]` entry cannot shadow them.
        mcp_servers.extend(self.extra_mcp.iter().cloned());
        Ok(SessionExtras {
            mcp_servers,
            instructions: Some(instructions),
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
/// then the project's rules when its own CLI would not read them (docs/tasks/DOGFOOD.md §6),
/// then the line saying its project has troubleshooting records (docs/tasks/LESSONS.md §4.5).
fn instructions(
    agent: &Agent,
    groups: &[crossagents_orchestrator::messaging::Group],
    lessons: Option<&str>,
) -> String {
    let guide = crossagents_orchestrator::protocol::guide_for(agent, groups);
    let mut text = format!("{}\n\n{guide}", agent.identity());
    for section in [crate::rules::for_agent(agent).as_deref(), lessons]
        .into_iter()
        .flatten()
    {
        text.push_str("\n\n");
        text.push_str(section);
    }
    text
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
    async fn tell_later(&self, agent: AgentId, topic: &str, line: &str) -> Result<(), CoreError> {
        self.bus.store().get_agent(&AgentKey::Id(agent)).await?;
        self.runtime.tell_later(agent, topic, line.to_owned()).await;
        Ok(())
    }
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

    async fn notify(
        &self,
        agent: AgentId,
        topic: &str,
        line: &str,
        class: crossagents_core::NoticeClass,
    ) -> Result<(), CoreError> {
        self.runtime
            .notify(&AgentKey::Id(agent), topic, line, class)
            .await
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

    async fn route(
        &self,
        template: AgentId,
        team: &[AgentId],
        limited: Option<AgentId>,
        mode: RouteMode,
    ) -> RouteDecision {
        crate::routing::route(self, template, team, limited, mode)
            .await
            .unwrap_or_default()
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
        let mut profile = agent.profile.normalized()?;
        crate::avatars::assign_new(&self.home, self.bus.store(), &mut profile).await?;
        let agent = Agent {
            profile,
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

    async fn archive_agent_after_turn(&self, agent: AgentId) -> Result<bool, CoreError> {
        self.runtime.archive_after_turn(&AgentKey::Id(agent)).await
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
        let by_run = crate::usage::compute(store, UsageGroup::Run, &self.home).await?;
        let by_agent = crate::usage::compute(store, UsageGroup::Agent, &self.home).await?;
        let spent = match run {
            Some(run) => {
                serde_json::to_value(by_run.rows.iter().find(|r| r.key == run.to_string()))
                    .unwrap_or_default()
            }
            None => serde_json::json!({
                "total_cost_usd": by_run.total_cost_usd,
                "estimated_cost_usd": by_run.rows.iter().try_fold(0.0, |sum, row| row.estimated_cost_usd.map(|cost| sum + cost)),
            }),
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
            "note": "Reported costs and price-table estimates are separate; estimates are unknown without prices.",
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

    async fn is_busy(&self, agent: AgentId) -> bool {
        self.runtime.is_busy(agent).await
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

    async fn renew_session(&self, agent: AgentId) -> Result<bool, CoreError> {
        self.runtime.renew(&AgentKey::Id(agent)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_worktree_of_a_run_builds_apart_and_checks_build_with_its_worker() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("worktrees");
        let task = root.join("ab12cd34").join("auth");
        let integration = root.join("ab12cd34").join("integration");
        let other = root.join("ff00ee11").join("integration");
        for d in [&task, &integration, &other] {
            std::fs::create_dir_all(d).unwrap();
            // Rust projects: the preset sets the build directory.
            std::fs::write(d.join("Cargo.toml"), "").unwrap();
        }
        let env = worktree_env(&root, &task, None);
        let target_var = |env: Vec<(String, String)>| {
            env.iter()
                .find(|(name, _)| name == "CARGO_TARGET_DIR")
                .map(|(_, value)| value.clone())
                .unwrap_or_else(|| panic!("{env:?}"))
        };
        let value = target_var(env.clone());
        let target = PathBuf::from(&value);
        assert!(target.is_absolute());
        // Its own directory, under its run and its tree, never the run's or a sibling's.
        assert!(
            target.ends_with(
                Path::new("ab12cd34")
                    .join("targets")
                    .join("auth")
                    .join("cargo")
            ),
            "{value}"
        );
        // The agent holds the same build directory the preset injects into it (LOCKS §3).
        let (name, json) = env
            .iter()
            .find(|(name, _)| name == "CROSSAGENTS_RESOURCES")
            .unwrap_or_else(|| panic!("{env:?}"));
        assert_eq!(name, "CROSSAGENTS_RESOURCES");
        let resources: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(resources[0]["name"], "build");
        assert_eq!(resources[0]["path"], value.as_str());

        assert_ne!(target_var(worktree_env(&root, &integration, None)), value);
        assert_ne!(target_var(worktree_env(&root, &other, None)), value);
        // Chats in a person's checkout, the root itself, the run's directory and the build
        // directories keep the inherited setting.
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        assert!(worktree_env(&root, dir.path(), None).is_empty());
        assert!(worktree_env(&root, &root, None).is_empty());
        assert!(worktree_env(&root, &root.join("ab12cd34"), None).is_empty());
        assert!(worktree_env(&root, &target, None).is_empty());
        // A project that is not Rust gets no cargo setting.
        std::fs::remove_file(other.join("Cargo.toml")).unwrap();
        std::fs::write(other.join("package.json"), "{}").unwrap();
        assert!(worktree_env(&root, &other, None).is_empty());
    }

    #[test]
    fn a_run_worktree_puts_the_shims_in_front_of_path_unless_it_shims_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("worktrees");
        let task = root.join("ab12cd34").join("auth");
        std::fs::create_dir_all(&task).unwrap();
        std::fs::write(task.join("Cargo.toml"), "").unwrap();
        let shims = dir.path().join("shims").join("1.0.0");
        std::fs::create_dir_all(&shims).unwrap();
        let path_of = |env: Vec<(String, String)>| {
            env.into_iter()
                .find(|(name, _)| name == "PATH")
                .map(|(_, value)| value)
        };
        let original = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .collect::<Vec<_>>();

        // Rust: `cargo` in the worktree comes from the shim directory, and the rest of PATH
        // is the original value (LOCKS §4.6).
        let path = path_of(worktree_env(&root, &task, Some(&shims))).expect("a PATH");
        let entries: Vec<PathBuf> = std::env::split_paths(&path).collect();
        assert_eq!(entries[0], shims);
        assert_eq!(entries.len(), original.len() + 1);

        // `[run] shims = []`: the shim directory is not added at all.
        std::fs::write(task.join("crossagents.toml"), "[run]\nshims = []\n").unwrap();
        assert!(path_of(worktree_env(&root, &task, Some(&shims))).is_none());

        // A hub that could not install the shims leaves PATH alone as well.
        std::fs::remove_file(task.join("crossagents.toml")).unwrap();
        assert!(path_of(worktree_env(&root, &task, None)).is_none());
    }

    #[test]
    fn the_accounts_environment_wins_over_the_projects() {
        let pair = |n: &str, v: &str| (n.to_owned(), v.to_owned());
        let env = with_project_env(
            vec![pair("HTTPS_PROXY", "http://127.0.0.1:7890")],
            vec![
                pair("HTTPS_PROXY", "http://elsewhere"),
                pair("CARGO_TARGET_DIR", "/b"),
            ],
        );
        assert_eq!(
            env,
            [
                pair("HTTPS_PROXY", "http://127.0.0.1:7890"),
                pair("CARGO_TARGET_DIR", "/b")
            ]
        );
    }
}
