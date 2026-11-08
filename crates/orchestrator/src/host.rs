//! What the orchestrator needs from whoever runs the agents (the hub).

use std::path::Path;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crossagents_core::{
    AccountCandidate, Agent, AgentId, CoreError, Envelope, MessageId, MessageSource, Role, RunId,
};
use serde_json::Value;

/// Where a task's worker runs, by quota.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Route {
    /// As planned: the template, on its own account.
    #[default]
    Same,
    /// The template on another account of the same vendor.
    Account { account: String, reason: String },
    /// Another teammate (another vendor, or an account of its own) instead of the template.
    Teammate { agent: AgentId, reason: String },
    /// Nothing better is left; it runs as planned. Why, to say so.
    Exhausted { reason: String },
}

/// What a worker is, for the quota it may use before handing over (magpie-lancer §2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteMode {
    /// A new execution session: an account at 90% of its window already hands over, so a
    /// fresh clone never starts on one about to run out.
    Fresh,
    /// A task in rework: its worker keeps the account until 98%, because the vendor's
    /// prompt cache for the earlier attempt lives there and a switch would pay for the
    /// whole conversation again.
    Rework,
}

/// What routing decided for one dispatch, and how it got there (magpie-lancer §2.8).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RouteDecision {
    pub route: Route,
    /// The account the worker runs on, when routing knows it.
    pub chosen: Option<String>,
    /// The same vendor's accounts weighed, best first, each with why it ranks there.
    pub candidates: Vec<AccountCandidate>,
    /// Whether the template's own account can still take the work in this mode (a false
    /// here with a move below is why an agent cannot take a task itself).
    pub current_usable: bool,
    /// Accounts whose unused quota resets away soon: worth more work now (§2.5).
    pub expiring: Vec<ExpiringQuota>,
}

/// An account window that will be replaced soon with much of it unused.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpiringQuota {
    pub account: String,
    pub provider: String,
    pub window: String,
    /// Percent of the window still unused.
    pub remaining_percent: f64,
    pub resets_at: DateTime<Utc>,
}

impl ExpiringQuota {
    /// One line for the run's lead: dispatch more work while the quota lasts.
    pub fn line(&self) -> String {
        let minutes = (self.resets_at - Utc::now()).num_minutes().max(0);
        format!(
            "{} account {} has {:.0}% of its {} window left and it resets in {}h{}m: dispatch more work there while it lasts.",
            self.provider,
            self.account,
            self.remaining_percent,
            self.window,
            minutes / 60,
            minutes % 60
        )
    }
}

#[async_trait]
pub trait AgentHost: Send + Sync {
    /// Keeps a contextual notice for the next message without starting an agent or a turn.
    /// Replaces an earlier notice on the same topic.
    async fn tell_later(&self, agent: AgentId, topic: &str, line: &str) -> Result<(), CoreError> {
        let _ = (agent, topic, line);
        Err(CoreError::Unsupported("passive notices".into()))
    }
    /// Delivers a message to an agent, starting it if needed.
    async fn send(
        &self,
        agent: AgentId,
        from: MessageSource,
        text: String,
    ) -> Result<(), CoreError>;
    /// Delivers a prepared message (id fixed by the sender, possibly a reply); `now` puts it
    /// first and interrupts the running turn.
    async fn send_envelope(
        &self,
        agent: AgentId,
        envelope: Envelope,
        now: bool,
    ) -> Result<(), CoreError>;
    /// Stops the agent's running turn, if any.
    async fn interrupt(&self, agent: AgentId) -> Result<(), CoreError>;
    /// Takes back a message that has not started yet.
    async fn withdraw(&self, agent: AgentId, message: MessageId) -> Result<(), CoreError>;
    /// Records a notice for the agent. `class` decides whether it wakes them now
    /// (see `Runtime::notify`).
    async fn notify(
        &self,
        agent: AgentId,
        topic: &str,
        line: &str,
        class: crossagents_core::NoticeClass,
    ) -> Result<(), CoreError>;
    /// Creates a new agent with `template`'s plugin, model and settings, working in `cwd`.
    async fn clone_agent(
        &self,
        template: AgentId,
        name: &str,
        role: Role,
        cwd: &Path,
    ) -> Result<Agent, CoreError>;
    /// Like [`Self::clone_agent`], on `account` instead of the template's when given.
    async fn clone_agent_on(
        &self,
        template: AgentId,
        name: &str,
        role: Role,
        cwd: &Path,
        account: Option<&str>,
    ) -> Result<Agent, CoreError> {
        let _ = account;
        self.clone_agent(template, name, role, cwd).await
    }
    /// Where a clone of `template` should run, given what the accounts have left
    /// (docs/tasks/DOGFOOD.md §5) and what it is (`mode`). `team` are the run's teammates to
    /// fall back on; `limited` is an agent whose account just hit its limit. The decision
    /// also carries the accounts weighed and why, for the run's trace (magpie-lancer §2.8).
    async fn route(
        &self,
        template: AgentId,
        team: &[AgentId],
        limited: Option<AgentId>,
        mode: RouteMode,
    ) -> RouteDecision {
        let _ = (template, team, limited, mode);
        RouteDecision::default()
    }
    /// Registers a new agent (`agent_spawn`); fails if its plugin is unknown. An empty name
    /// gets a made-up one, a taken one an ordinal ("Nova 2").
    async fn spawn_agent(&self, agent: Agent) -> Result<Agent, CoreError>;
    /// The models an account that is logged in can run for `plugin` (empty: no such account).
    async fn available_models(&self, plugin: &str) -> Result<Vec<String>, CoreError>;
    /// The plugins agents can run on.
    fn plugin_names(&self) -> Vec<String> {
        Vec::new()
    }
    /// Words no agent name may contain: vendors' and every known model's.
    fn denied_name_words(&self) -> Vec<String>;
    /// Changes an agent's model, from its next turn when it is running.
    async fn set_model(&self, agent: AgentId, model: Option<String>) -> Result<Agent, CoreError>;
    /// Spend of a run (or of everything) and the accounts' quota, for `usage_get`.
    async fn usage_report(&self, run: Option<RunId>) -> Result<Value, CoreError>;
    /// Takes the messages waiting for an agent that runs outside the hub.
    async fn take_inbox(&self, agent: AgentId) -> Result<Vec<Value>, CoreError>;
    /// Archives an agent (and the ones its conversation recruited): stopped and out of the
    /// lists, its transcript kept. Nothing the orchestrator does deletes an agent; only a
    /// person does (docs/tasks/S1.md §3.4).
    async fn archive_agent(&self, agent: AgentId) -> Result<(), CoreError>;
    /// Archives the agent as [`AgentHost::archive_agent`] does, but only once its turn has
    /// ended and nothing waits for it (now when it is idle): a lead closing its own run is
    /// still in the turn that called `run_finish`, and stopping it there cuts off its reply
    /// to the person. `true` when it was archived now.
    async fn archive_agent_after_turn(&self, agent: AgentId) -> Result<bool, CoreError> {
        self.archive_agent(agent).await.map(|_| true)
    }
    /// Stops the agent's session and process, keeping the agent; its next message starts it
    /// again.
    async fn stop_agent(&self, agent: AgentId) -> Result<(), CoreError>;
    /// Moves the agent to work in `dir` (docs/tasks/S1.md §3.1); [`CoreError::Busy`] while
    /// a turn runs or messages wait for it. With `carry` its session resumes there at once,
    /// and failing to is an error: it never quietly becomes a new session.
    async fn place(&self, agent: AgentId, dir: &Path, carry: bool) -> Result<(), CoreError> {
        let _ = (agent, dir, carry);
        Err(CoreError::Unsupported("moving agents".into()))
    }
    /// Sends the agent back to its workspace: now when it is idle, else once its turn ends.
    /// `true` when it moved now.
    async fn send_home(&self, agent: AgentId) -> Result<bool, CoreError> {
        let _ = agent;
        Err(CoreError::Unsupported("moving agents".into()))
    }
    /// Whether a turn runs or any message is queued. Unknown hosts fail closed.
    async fn is_busy(&self, agent: AgentId) -> bool {
        let _ = agent;
        true
    }
    /// Whether a person waits on the agent: its turn answers their message, or one waits.
    async fn serving_person(&self, agent: AgentId) -> bool {
        let _ = agent;
        false
    }
    /// Brings an archived agent back to the lists (and its recruits with it).
    async fn restore_agent(&self, agent: AgentId) -> Result<(), CoreError> {
        let _ = agent;
        Err(CoreError::Unsupported("restoring agents".into()))
    }
    /// Whether the agent's driver resumes its session in another directory (`move_cwd`).
    async fn can_move(&self, agent: &Agent) -> bool {
        let _ = agent;
        false
    }
}

/// One tool of an enabled extension, as the orchestrator lists it for an agent
/// (docs/EXTENSIONS.md §3).
#[derive(Debug, Clone)]
pub struct ExtensionToolSpec {
    /// `<extension>__<tool>`.
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    /// The groups that may call it, by name (`lead`, `everyone`, `human`, ...).
    pub groups: Vec<String>,
}

/// Who calls an extension tool, as the orchestrator knows them at the moment of the call.
#[derive(Debug, Clone)]
pub struct ExtensionCaller {
    pub agent: Agent,
    /// The agent's permission groups right now, by name.
    pub groups: Vec<String>,
    pub run: Option<RunId>,
    pub task: Option<String>,
}

/// What the orchestrator needs from whoever runs extension tools (the hub). It is optional:
/// without one there are no extension tools. The orchestrator does not know how a tool runs.
#[async_trait]
pub trait ExtensionHost: Send + Sync {
    /// The tools of every enabled extension, at their current version.
    fn tools(&self) -> Vec<ExtensionToolSpec>;
    /// Runs `tool` (`<extension>__<tool>`). The host checks, in this order: the extension is
    /// enabled (and which version this call uses), the caller's groups allow the tool, the
    /// input fits the schema, and the concurrency limits hold.
    async fn call(
        &self,
        caller: ExtensionCaller,
        tool: &str,
        input: Value,
    ) -> Result<Value, CoreError>;
    /// The extra instructions the enabled extensions declare for `groups`, in order. Read
    /// when a session starts; they are appended to the hub's own and never replace them.
    fn prompts(&self, groups: &[&str]) -> Vec<String> {
        let _ = groups;
        Vec::new()
    }
}
