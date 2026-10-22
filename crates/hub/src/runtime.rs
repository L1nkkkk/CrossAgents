//! Live agent sessions: start, deliver messages one turn at a time, cancel, stop, and turn
//! what happens inside a session into events.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use crossagents_core::{
    Agent, AgentId, AgentKey, AgentStatus, CoreError, DiffSource, DriverSession, EXTERNAL_PLUGIN,
    Envelope, EventBus, EventKind, McpServerSpec, MessageId, MessageSource, NameSource,
    OutputStream, PermissionPolicy, SessionEvent, SessionEventKind, SessionSpec, StopReason, Store,
    validate_agent_name,
};
use crossagents_plugins::PluginRegistry;
use tokio::sync::{Mutex, Notify, mpsc};

const FLUSH_INTERVAL: Duration = Duration::from_millis(150);

/// Extra per-session setup supplied by higher layers (collaboration MCP server, role
/// instructions, ...).
#[async_trait]
pub trait SessionSetup: Send + Sync {
    async fn prepare(&self, agent: &Agent) -> Result<SessionExtras, CoreError>;
}

#[derive(Debug, Default, Clone)]
pub struct SessionExtras {
    pub mcp_servers: Vec<McpServerSpec>,
    pub instructions: Option<String>,
    /// Extra environment for the agent process (e.g. its account's home).
    pub env: Vec<(String, String)>,
}

struct Live {
    generation: u64,
    session: Arc<dyn DriverSession>,
    inbox: Arc<Inbox>,
    control: mpsc::UnboundedSender<Control>,
}

impl Drop for Live {
    fn drop(&mut self) {
        // Ends the session's turn loop once nobody can deliver to it any more.
        self.inbox.close();
    }
}

/// Messages waiting for their turn. Unlike a channel, a waiting message can be taken back
/// or jump the line.
#[derive(Default)]
struct Inbox {
    waiting: std::sync::Mutex<VecDeque<(MessageId, String)>>,
    /// A turn is running.
    busy: AtomicBool,
    closed: AtomicBool,
    notify: Notify,
}

impl Inbox {
    fn push(&self, message: (MessageId, String), first: bool) {
        let mut waiting = self.waiting.lock().unwrap();
        if first {
            waiting.push_front(message);
        } else {
            waiting.push_back(message);
        }
        drop(waiting);
        self.notify.notify_one();
    }

    /// Replaces a waiting message's text; `false` if it is not waiting any more.
    fn update(&self, id: MessageId, text: &str) -> bool {
        let mut waiting = self.waiting.lock().unwrap();
        match waiting.iter_mut().find(|(m, _)| *m == id) {
            Some((_, old)) => {
                text.clone_into(old);
                true
            }
            None => false,
        }
    }

    /// Takes a waiting message out; `false` if it is not waiting (started or unknown).
    fn withdraw(&self, id: MessageId) -> bool {
        let mut waiting = self.waiting.lock().unwrap();
        let Some(index) = waiting.iter().position(|(m, _)| *m == id) else {
            return false;
        };
        waiting.remove(index);
        true
    }

    /// The next message, marking a turn as running; `None` once closed.
    async fn next(&self) -> Option<(MessageId, String)> {
        loop {
            if self.closed.load(Ordering::SeqCst) {
                return None;
            }
            if let Some(message) = self.waiting.lock().unwrap().pop_front() {
                self.busy.store(true, Ordering::SeqCst);
                return Some(message);
            }
            self.notify.notified().await;
        }
    }

    fn finish_turn(&self) {
        self.busy.store(false, Ordering::SeqCst);
    }

    fn is_idle(&self) -> bool {
        let waiting = self.waiting.lock().unwrap();
        waiting.is_empty() && !self.busy.load(Ordering::SeqCst)
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }
}

/// A message waiting for an agent outside the hub: id, sender, text.
pub type Waiting = (MessageId, MessageSource, String);

type LiveMap = Arc<Mutex<HashMap<AgentId, Arc<Live>>>>;

pub struct Runtime {
    bus: Arc<EventBus>,
    plugins: PluginRegistry,
    setup: Option<Arc<dyn SessionSetup>>,
    live: LiveMap,
    /// Serializes start/stop per agent without blocking other agents.
    locks: Mutex<HashMap<AgentId, Arc<Mutex<()>>>>,
    /// Each agent's notification digest still waiting for its turn: the message and its
    /// lines by topic (a newer line on a topic replaces the older one).
    digests: Mutex<HashMap<AgentId, Digest>>,
    /// Messages for agents outside the hub, until they read them (`inbox_read`).
    external: Mutex<HashMap<AgentId, Vec<Waiting>>>,
    /// What changed about the agent that it has not been told yet (its model or effort, its
    /// name), by topic: said at the top of the next message it gets.
    notices: Mutex<HashMap<AgentId, BTreeMap<&'static str, String>>>,
    generation: AtomicU64,
}

/// Messages to an agent's event forwarder, kept in order with the session's own events.
enum Control {
    TurnStarted(MessageId),
    /// The turn's outcome, and what it changed when the hub works that out itself.
    TurnFinished(MessageId, Result<StopReason, String>, Option<String>),
    Stopped,
}

impl Runtime {
    pub fn new(bus: Arc<EventBus>, plugins: PluginRegistry) -> Self {
        Self {
            bus,
            plugins,
            setup: None,
            live: LiveMap::default(),
            locks: Mutex::default(),
            digests: Mutex::default(),
            notices: Mutex::default(),
            external: Mutex::default(),
            generation: AtomicU64::new(0),
        }
    }

    pub fn with_setup(mut self, setup: Arc<dyn SessionSetup>) -> Self {
        self.setup = Some(setup);
        self
    }

    pub fn plugins(&self) -> &PluginRegistry {
        &self.plugins
    }

    fn store(&self) -> &Arc<dyn Store> {
        self.bus.store()
    }

    /// Marks every agent stopped; used at hub start, when no session can be alive.
    pub async fn reset_statuses(&self) -> Result<(), CoreError> {
        for agent in self.store().list_agents().await? {
            set_status(&self.bus, agent.id, AgentStatus::Stopped).await;
        }
        Ok(())
    }

    async fn agent_lock(&self, id: AgentId) -> Arc<Mutex<()>> {
        self.locks.lock().await.entry(id).or_default().clone()
    }

    pub async fn is_live(&self, id: AgentId) -> bool {
        self.live.lock().await.contains_key(&id)
    }

    /// Starts the agent's session if it is not running. Idempotent.
    pub async fn start(&self, key: &AgentKey) -> Result<Agent, CoreError> {
        let agent = self.store().get_agent(key).await?;
        if agent.plugin == EXTERNAL_PLUGIN {
            return Err(CoreError::Conflict(format!(
                "{} runs outside the hub; it reads its messages with inbox_read",
                agent.name
            )));
        }
        let lock = self.agent_lock(agent.id).await;
        let _guard = lock.lock().await;
        if self.is_live(agent.id).await {
            return Ok(agent);
        }
        let resolved = match self.plugins.driver(&agent.plugin, agent.driver.as_deref()) {
            Ok(resolved) => resolved,
            Err(CoreError::NotFound { .. }) => {
                return Err(CoreError::Agent(format!(
                    "unknown plugin {:?}",
                    agent.plugin
                )));
            }
            Err(err) => return Err(err),
        };

        set_status(&self.bus, agent.id, AgentStatus::Starting).await;
        if let Some(message) = resolved.warning {
            publish(
                &self.bus,
                EventKind::AgentError {
                    id: agent.id,
                    message,
                },
            )
            .await;
        }
        let extras = match &self.setup {
            Some(setup) => setup.prepare(&agent).await?,
            None => SessionExtras::default(),
        };
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);

        let spec = || SessionSpec {
            agent: agent.clone(),
            mcp_servers: extras.mcp_servers.clone(),
            permission: permission_of(&agent),
            permissions: None,
            command: command_of(&agent),
            instructions: extras.instructions.clone(),
            env: extras.env.clone(),
            events: events_tx.clone(),
        };
        // Carry on the agent's last conversation when the driver can.
        let previous = if resolved.driver.capabilities().resume {
            self.store()
                .last_session_id(agent.id)
                .await
                .unwrap_or_default()
        } else {
            None
        };
        let opened = match previous {
            Some(previous) => match resolved.driver.resume(&previous, spec()).await {
                Ok(session) => Ok(session),
                Err(err) => {
                    publish(
                        &self.bus,
                        EventKind::AgentWarning {
                            id: agent.id,
                            message: format!(
                                "cannot resume the previous session ({err}); started a new one"
                            ),
                        },
                    )
                    .await;
                    resolved.driver.open(spec()).await
                }
            },
            None => resolved.driver.open(spec()).await,
        };
        drop(events_tx);
        let session: Arc<dyn DriverSession> = match opened {
            Ok(session) => Arc::from(session),
            Err(err) => {
                publish(
                    &self.bus,
                    EventKind::AgentError {
                        id: agent.id,
                        message: err.to_string(),
                    },
                )
                .await;
                set_status(&self.bus, agent.id, AgentStatus::Error).await;
                return Err(err);
            }
        };

        let inbox = Arc::new(Inbox::default());
        let live = Arc::new(Live {
            generation,
            session: session.clone(),
            inbox: inbox.clone(),
            control: control_tx.clone(),
        });
        tokio::spawn(forward_events(
            self.bus.clone(),
            agent.id,
            events_rx,
            control_rx,
            self.live.clone(),
            generation,
        ));
        // Drivers that do not report what a turn changed get it worked out by the hub.
        let diff_dir = (!resolved.driver.capabilities().structured_diff).then(|| agent.cwd.clone());
        tokio::spawn(run_turns(session, inbox, control_tx, diff_dir));
        self.live.lock().await.insert(agent.id, live);
        set_status(&self.bus, agent.id, AgentStatus::Idle).await;
        Ok(agent)
    }

    /// Delivers a message, starting the agent if needed. Returns the message id and whether
    /// it was queued behind other work.
    pub async fn send(
        &self,
        key: &AgentKey,
        text: String,
        from: MessageSource,
    ) -> Result<(MessageId, bool), CoreError> {
        self.deliver(key, Envelope::new(from, text), false).await
    }

    /// Delivers a prepared message (its id fixed by the sender, possibly a reply).
    pub async fn send_envelope(
        &self,
        key: &AgentKey,
        envelope: Envelope,
    ) -> Result<(MessageId, bool), CoreError> {
        self.deliver(key, envelope, false).await
    }

    /// Like [`Self::send_envelope`], ahead of everything waiting, interrupting the running
    /// turn.
    pub async fn send_now_envelope(
        &self,
        key: &AgentKey,
        envelope: Envelope,
    ) -> Result<(MessageId, bool), CoreError> {
        self.deliver(key, envelope, true).await
    }

    /// Adds a line to the agent's notification digest: one waiting message that gathers
    /// news while the agent is busy, so notices never pile up as separate turns. A line on
    /// a `topic` already in the digest replaces it. Starts the agent if needed.
    pub async fn notify(&self, key: &AgentKey, topic: &str, line: &str) -> Result<(), CoreError> {
        let stored = self.store().get_agent(key).await?;
        if stored.plugin == EXTERNAL_PLUGIN {
            let envelope = Envelope::new(MessageSource::Notice, line.to_owned());
            return self.deliver(key, envelope, false).await.map(|_| ());
        }
        let agent = self.start(key).await?;
        let live = self.live.lock().await.get(&agent.id).cloned();
        let mut digests = self.digests.lock().await;
        if let (Some(live), Some(digest)) = (&live, digests.get_mut(&agent.id)) {
            digest.lines.insert(topic.to_owned(), line.to_owned());
            let text = digest.text();
            if live.inbox.update(digest.message, &text) {
                publish(
                    &self.bus,
                    EventKind::AgentInputUpdated {
                        id: agent.id,
                        message_id: digest.message,
                        text,
                    },
                )
                .await;
                return Ok(());
            }
        }
        // No digest waiting (none yet, or the last one already started): begin a new one.
        let mut digest = Digest {
            message: MessageId::new(),
            lines: BTreeMap::new(),
        };
        digest.lines.insert(topic.to_owned(), line.to_owned());
        let envelope = Envelope {
            id: digest.message,
            from: MessageSource::Notice,
            text: digest.text(),
            reply_to: None,
        };
        digests.insert(agent.id, digest);
        drop(digests);
        self.deliver(&AgentKey::Id(agent.id), envelope, false)
            .await
            .map(|_| ())
    }

    /// Delivers a message ahead of everything waiting and interrupts the running turn, so it
    /// is the next thing the agent reads.
    pub async fn send_now(
        &self,
        key: &AgentKey,
        text: String,
        from: MessageSource,
    ) -> Result<(MessageId, bool), CoreError> {
        self.deliver(key, Envelope::new(from, text), true).await
    }

    async fn deliver(
        &self,
        key: &AgentKey,
        envelope: Envelope,
        first: bool,
    ) -> Result<(MessageId, bool), CoreError> {
        let Envelope {
            id: message_id,
            from,
            text,
            reply_to,
        } = envelope;
        // A reply joins the thread of the message it answers.
        let (thread, reply_excerpt) = match reply_to {
            Some(parent) => match self.store().find_message(parent).await? {
                Some((thread, text)) => (thread.or(Some(parent)), Some(excerpt(&text))),
                None => (Some(parent), None),
            },
            None => (None, None),
        };
        // Agents outside the hub get their messages when they ask for them.
        let stored = self.store().get_agent(key).await?;
        if stored.archived_at.is_some() {
            return Err(CoreError::Conflict(format!(
                "{} is archived; its conversation has to be restored before it gets messages",
                stored.name
            )));
        }
        if stored.plugin == EXTERNAL_PLUGIN {
            publish(
                &self.bus,
                EventKind::AgentInput {
                    id: stored.id,
                    message_id,
                    text: text.clone(),
                    queued: true,
                    from: Some(from),
                    reply_to,
                    thread,
                    reply_excerpt,
                },
            )
            .await;
            self.external
                .lock()
                .await
                .entry(stored.id)
                .or_default()
                .push((message_id, from, text));
            return Ok((message_id, true));
        }
        let agent = self.start(key).await?;
        let live = self
            .live
            .lock()
            .await
            .get(&agent.id)
            .cloned()
            .filter(|l| !l.inbox.is_closed())
            .ok_or_else(|| CoreError::Agent(format!("agent {} stopped", agent.name)))?;
        let queued = !live.inbox.is_idle();
        // Published before the message can start a turn, so clients see the input first.
        publish(
            &self.bus,
            EventKind::AgentInput {
                id: agent.id,
                message_id,
                text: text.clone(),
                queued,
                from: Some(from),
                reply_to,
                thread,
                reply_excerpt,
            },
        )
        .await;
        // What changed since the last message (model, effort, name) is told before it; the
        // conversation shows it as a note of its own (e.g. `agent.model_changed`).
        let text = match self.notices.lock().await.remove(&agent.id) {
            Some(notes) => {
                let notes: Vec<String> = notes.into_values().collect();
                format!("{}\n\n{text}", notes.join("\n"))
            }
            None => text,
        };
        live.inbox.push((message_id, text), first);
        if first && queued {
            let _ = live.session.interrupt().await;
        }
        Ok((message_id, queued))
    }

    /// Takes back a message that is still waiting for its turn.
    pub async fn withdraw(&self, key: &AgentKey, message_id: MessageId) -> Result<(), CoreError> {
        let agent = self.store().get_agent(key).await?;
        let withdrawn = match self.live.lock().await.get(&agent.id) {
            Some(live) => live.inbox.withdraw(message_id),
            None => false,
        };
        if !withdrawn {
            return Err(CoreError::Conflict(
                "the message is not waiting: it already started, or is unknown".into(),
            ));
        }
        publish(
            &self.bus,
            EventKind::AgentInputWithdrawn {
                id: agent.id,
                message_id,
            },
        )
        .await;
        Ok(())
    }

    pub async fn cancel(&self, key: &AgentKey) -> Result<(), CoreError> {
        let agent = self.store().get_agent(key).await?;
        let live = self.live.lock().await.get(&agent.id).cloned();
        match live {
            Some(live) => live.session.interrupt().await,
            None => Err(CoreError::Conflict(format!(
                "agent {} is not running",
                agent.name
            ))),
        }
    }

    /// Summarises the running session's history to free context.
    pub async fn compact(&self, key: &AgentKey) -> Result<(), CoreError> {
        let agent = self.store().get_agent(key).await?;
        let live = self.live.lock().await.get(&agent.id).cloned();
        match live {
            Some(live) => live.session.compact().await,
            None => Err(CoreError::Conflict(format!(
                "agent {} is not running",
                agent.name
            ))),
        }
    }

    /// Changes the agent's model and, when `effort` is given, its reasoning effort (empty:
    /// the model's default; kept in `config.effort`, which drivers read at launch). A running
    /// session switches from its next turn when the driver can; otherwise both apply when the
    /// session next starts.
    pub async fn set_model(
        &self,
        key: &AgentKey,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<Agent, CoreError> {
        let mut agent = self.store().get_agent(key).await?;
        let (from_model, from_effort) = (agent.model.clone(), effort_of(&agent));
        self.store()
            .update_agent_model(agent.id, model.as_deref())
            .await?;
        agent.model = model;
        if let Some(effort) = &effort {
            if !agent.config.is_object() {
                agent.config = serde_json::json!({});
            }
            let config = agent.config.as_object_mut().expect("config is an object");
            if effort.is_empty() {
                config.remove("effort");
            } else {
                config.insert("effort".into(), effort.clone().into());
            }
            self.store()
                .update_agent_config(agent.id, &agent.config)
                .await?;
        }
        let effort = effort.filter(|e| !e.is_empty());
        let live = self.live.lock().await.get(&agent.id).cloned();
        if let Some(live) = live
            && (agent.model.is_some() || effort.is_some())
        {
            match live
                .session
                .set_model(agent.model.as_deref(), effort.as_deref())
                .await
            {
                Ok(()) | Err(CoreError::Unsupported(_)) => {}
                Err(err) => return Err(err),
            }
        }
        publish(
            &self.bus,
            EventKind::AgentUpdated {
                agent: agent.clone(),
            },
        )
        .await;
        let to_effort = effort_of(&agent);
        if (&agent.model, &to_effort) != (&from_model, &from_effort) {
            if agent.plugin != EXTERNAL_PLUGIN {
                let note = switch_note(&from_model, &from_effort, &agent.model, &to_effort);
                self.tell_later(agent.id, "model", note).await;
            }
            publish(
                &self.bus,
                EventKind::AgentModelChanged {
                    id: agent.id,
                    model: agent.model.clone(),
                    effort: to_effort,
                    from_model,
                    from_effort,
                },
            )
            .await;
        }
        Ok(agent)
    }

    /// Hands an agent outside the hub its waiting messages; each counts as read (a turn
    /// that started and ended).
    pub async fn take_external(&self, id: AgentId) -> Vec<Waiting> {
        let messages = self.external.lock().await.remove(&id).unwrap_or_default();
        for (message_id, _, _) in &messages {
            publish(
                &self.bus,
                EventKind::AgentTurnStarted {
                    id,
                    message_id: *message_id,
                },
            )
            .await;
            publish(
                &self.bus,
                EventKind::AgentTurnCompleted {
                    id,
                    message_id: *message_id,
                    stop_reason: StopReason::EndTurn,
                },
            )
            .await;
        }
        messages
    }

    /// Keeps `note` for the agent's next message (a newer note on the same topic replaces it).
    async fn tell_later(&self, id: AgentId, topic: &'static str, note: String) {
        self.notices
            .lock()
            .await
            .entry(id)
            .or_default()
            .insert(topic, note);
    }

    /// Renames the agent (people chose the name). It hears its new name with its next message;
    /// its instructions carry it from its next session.
    pub async fn rename(&self, key: &AgentKey, name: &str) -> Result<Agent, CoreError> {
        let name = name.trim();
        validate_agent_name(name)?;
        let mut agent = self.store().get_agent(key).await?;
        if agent.name == name && agent.name_source == NameSource::Chosen {
            return Ok(agent);
        }
        self.store()
            .rename_agent(agent.id, name, NameSource::Chosen)
            .await?;
        let from = std::mem::replace(&mut agent.name, name.to_owned());
        agent.name_source = NameSource::Chosen;
        if agent.plugin != EXTERNAL_PLUGIN && from != agent.name {
            let note = format!(
                "[CrossAgents] You have been renamed: you are {} now (was {from}). People and teammates call you {}.",
                agent.name, agent.name
            );
            self.tell_later(agent.id, "name", note).await;
        }
        publish(
            &self.bus,
            EventKind::AgentUpdated {
                agent: agent.clone(),
            },
        )
        .await;
        Ok(agent)
    }

    /// Sets or clears a conversation's title.
    pub async fn set_topic(
        &self,
        key: &AgentKey,
        topic: Option<String>,
    ) -> Result<Agent, CoreError> {
        let topic = topic
            .map(|t| t.trim().chars().take(120).collect::<String>())
            .filter(|t| !t.is_empty());
        let mut agent = self.store().get_agent(key).await?;
        self.store()
            .update_agent_topic(agent.id, topic.as_deref())
            .await?;
        agent.topic = topic;
        publish(
            &self.bus,
            EventKind::AgentUpdated {
                agent: agent.clone(),
            },
        )
        .await;
        Ok(agent)
    }

    /// Archives a conversation (stopping it) or restores it, together with the teammates it
    /// recruited: archived, they are out of the lists and stopped; every transcript stays.
    pub async fn archive(&self, key: &AgentKey, archived: bool) -> Result<Agent, CoreError> {
        let root = self.store().get_agent(key).await?;
        let mut ids = vec![root.id];
        let all = self.store().list_agents().await?;
        let mut i = 0;
        while i < ids.len() {
            let parent = ids[i];
            ids.extend(
                all.iter()
                    .filter(|a| a.recruiter() == Some(parent))
                    .map(|a| a.id),
            );
            i += 1;
        }
        let at = archived.then(chrono::Utc::now);
        let mut result = root;
        for id in ids {
            if archived {
                self.stop(&AgentKey::Id(id)).await?;
            }
            self.store().set_agent_archived(id, at).await?;
            let agent = self.store().get_agent(&AgentKey::Id(id)).await?;
            publish(
                &self.bus,
                EventKind::AgentUpdated {
                    agent: agent.clone(),
                },
            )
            .await;
            if id == result.id {
                result = agent;
            }
        }
        Ok(result)
    }

    /// Stops the agent's session. Stopping a stopped agent is fine.
    pub async fn stop(&self, key: &AgentKey) -> Result<(), CoreError> {
        let agent = self.store().get_agent(key).await?;
        let lock = self.agent_lock(agent.id).await;
        let _guard = lock.lock().await;
        let Some(live) = self.live.lock().await.remove(&agent.id) else {
            set_status(&self.bus, agent.id, AgentStatus::Stopped).await;
            return Ok(());
        };
        let _ = live.session.interrupt().await;
        live.session.shutdown().await;
        let _ = live.control.send(Control::Stopped);
        set_status(&self.bus, agent.id, AgentStatus::Stopped).await;
        Ok(())
    }

    /// Stops every session; used when the hub shuts down.
    pub async fn stop_all(&self) {
        let ids: Vec<AgentId> = self.live.lock().await.keys().copied().collect();
        for id in ids {
            if let Err(err) = self.stop(&AgentKey::Id(id)).await {
                tracing::warn!(%id, error = %err, "cannot stop agent");
            }
        }
    }
}

/// The reasoning effort the agent's config asks for; `None` is the model's default.
fn effort_of(agent: &Agent) -> Option<String> {
    agent
        .config
        .get("effort")
        .and_then(|v| v.as_str())
        .filter(|e| !e.is_empty())
        .map(str::to_owned)
}

/// What the agent is told about a switch, so it does not guess from replies by another model
/// earlier in the conversation.
fn switch_note(
    from_model: &Option<String>,
    from_effort: &Option<String>,
    model: &Option<String>,
    effort: &Option<String>,
) -> String {
    let name = |v: &Option<String>| v.clone().unwrap_or_else(|| "default".into());
    format!(
        "[CrossAgents] From this message on you run on model {} (was {}) with reasoning effort {} (was {}). Earlier replies in this conversation may come from the previous setting.",
        name(model),
        name(from_model),
        name(effort),
        name(from_effort),
    )
}

fn permission_of(agent: &Agent) -> PermissionPolicy {
    agent
        .config
        .get("permission")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

fn command_of(agent: &Agent) -> Option<Vec<String>> {
    agent
        .config
        .get("command")
        .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
        .filter(|c| !c.is_empty())
}

async fn publish(bus: &EventBus, kind: EventKind) {
    if let Err(err) = bus.publish(kind).await {
        tracing::error!(error = %err, "cannot publish event");
    }
}

async fn set_status(bus: &EventBus, id: AgentId, to: AgentStatus) {
    let from = match bus.store().get_agent(&AgentKey::Id(id)).await {
        Ok(agent) => agent.status,
        Err(_) => return, // deleted meanwhile
    };
    if from == to {
        return;
    }
    if let Err(err) = bus.store().update_agent_status(id, to).await {
        tracing::warn!(%id, error = %err, "cannot update agent status");
        return;
    }
    publish(bus, EventKind::AgentStatusChanged { id, from, to }).await;
}

/// Runs one prompt at a time from the inbox. With `diff_dir` (drivers without
/// `structured_diff`), snapshots that directory around each turn to tell what it changed.
async fn run_turns(
    session: Arc<dyn DriverSession>,
    inbox: Arc<Inbox>,
    control: mpsc::UnboundedSender<Control>,
    diff_dir: Option<PathBuf>,
) {
    while let Some((message_id, text)) = inbox.next().await {
        // Before the turn is announced: an interrupt that follows the announcement must find
        // the prompt already running.
        let base = match &diff_dir {
            Some(dir) => crate::snapshot::take(dir).await,
            None => None,
        };
        let _ = control.send(Control::TurnStarted(message_id));
        let result = session
            .prompt(&text)
            .await
            .map(|outcome| outcome.stop_reason)
            .map_err(|e| e.to_string());
        let diff = match (&diff_dir, base) {
            (Some(dir), Some(base)) => match crate::snapshot::take(dir).await {
                Some(head) => crate::snapshot::diff(dir, &base, &head).await,
                None => None,
            },
            _ => None,
        };
        inbox.finish_turn();
        if control
            .send(Control::TurnFinished(message_id, result, diff))
            .is_err()
        {
            return;
        }
    }
}

/// Turns session events and turn control into published events, coalescing output text.
async fn forward_events(
    bus: Arc<EventBus>,
    id: AgentId,
    mut events: mpsc::UnboundedReceiver<SessionEvent>,
    mut control: mpsc::UnboundedReceiver<Control>,
    live: LiveMap,
    generation: u64,
) {
    let mut out = Coalescer::default();
    let mut tick = tokio::time::interval(FLUSH_INTERVAL);
    let mut events_open = true;
    loop {
        tokio::select! {
            event = events.recv(), if events_open => match event {
                Some(event) => {
                    if handle_session_event(&bus, id, event, &mut out, &live, generation).await {
                        return;
                    }
                }
                None => events_open = false,
            },
            ctl = control.recv() => {
                // Everything the session emitted before this point is already queued.
                while let Ok(event) = events.try_recv() {
                    if handle_session_event(&bus, id, event, &mut out, &live, generation).await {
                        return;
                    }
                }
                out.flush(&bus, id).await;
                match ctl {
                    Some(Control::TurnStarted(message_id)) => {
                        set_status(&bus, id, AgentStatus::Running).await;
                        publish(&bus, EventKind::AgentTurnStarted { id, message_id }).await;
                    }
                    Some(Control::TurnFinished(message_id, result, diff)) => {
                        let stop_reason = match result {
                            Ok(reason) => reason,
                            Err(message) => {
                                publish(&bus, EventKind::AgentError { id, message }).await;
                                StopReason::Error
                            }
                        };
                        // Read-only turns have nothing to show.
                        if let Some(diff) = diff.filter(|d| !d.is_empty()) {
                            publish(
                                &bus,
                                EventKind::AgentTurnDiff {
                                    id,
                                    diff,
                                    message_id: Some(message_id),
                                    source: Some(DiffSource::Hub),
                                },
                            )
                            .await;
                        }
                        publish(
                            &bus,
                            EventKind::AgentTurnCompleted {
                                id,
                                message_id,
                                stop_reason,
                            },
                        )
                        .await;
                        // Only a session that is still ours and has nothing queued is idle;
                        // a stopped one keeps its status.
                        let idle = live
                            .lock()
                            .await
                            .get(&id)
                            .filter(|l| l.generation == generation)
                            .map(|l| l.inbox.is_idle());
                        if idle == Some(true) {
                            set_status(&bus, id, AgentStatus::Idle).await;
                        }
                    }
                    Some(Control::Stopped) | None => return,
                }
            },
            _ = tick.tick() => out.flush(&bus, id).await,
        }
    }
}

/// Returns `true` when the session is gone and forwarding should end.
async fn handle_session_event(
    bus: &EventBus,
    id: AgentId,
    event: SessionEvent,
    out: &mut Coalescer,
    live: &LiveMap,
    generation: u64,
) -> bool {
    let SessionEvent { kind: event, .. } = event;
    match event {
        SessionEventKind::Output { stream, text } => {
            out.push(bus, id, Stream::Output(stream), text).await;
            return false;
        }
        SessionEventKind::ToolOutput { tool_call_id, text } => {
            out.push(bus, id, Stream::Tool(tool_call_id), text).await;
            return false;
        }
        _ => {}
    }
    out.flush(bus, id).await;
    let kind = match event {
        SessionEventKind::Output { .. } | SessionEventKind::ToolOutput { .. } => unreachable!(),
        SessionEventKind::Started { session_id } => {
            EventKind::AgentSessionStarted { id, session_id }
        }
        SessionEventKind::ToolCall {
            tool_call_id,
            title,
            kind,
            status,
        } => EventKind::AgentToolCall {
            id,
            tool_call_id,
            title,
            kind,
            status,
        },
        SessionEventKind::ToolCallUpdated {
            tool_call_id,
            title,
            status,
        } => EventKind::AgentToolCallUpdated {
            id,
            tool_call_id,
            title,
            status,
        },
        SessionEventKind::ToolDetail {
            tool_call_id,
            mut detail,
        } => {
            detail.output = detail.output.map(|o| tail(o, DETAIL_OUTPUT_LIMIT));
            EventKind::AgentToolDetail {
                id,
                tool_call_id,
                detail,
            }
        }
        SessionEventKind::Usage {
            used,
            size,
            cost_usd,
            tokens,
        } => EventKind::AgentUsage {
            id,
            used,
            size,
            cost_usd,
            tokens,
        },
        SessionEventKind::TurnDiff { diff, .. } => EventKind::AgentTurnDiff {
            id,
            diff,
            message_id: None,
            source: None,
        },
        SessionEventKind::RateLimits { limits } => EventKind::AgentRateLimits { id, limits },
        SessionEventKind::PermissionRequested { request, decision } => EventKind::AgentPermission {
            id,
            kind: request.kind,
            title: request.title,
            decision,
        },
        SessionEventKind::Warning { message } => EventKind::AgentWarning { id, message },
        SessionEventKind::Error { message } => EventKind::AgentError { id, message },
        // Turn boundaries come from the runtime's own control messages.
        SessionEventKind::TurnStarted { .. } | SessionEventKind::TurnCompleted { .. } => {
            return false;
        }
        SessionEventKind::Exited { message } => {
            // The process died on its own: forget the session if it is still ours.
            let mut map = live.lock().await;
            if map.get(&id).is_some_and(|l| l.generation == generation) {
                map.remove(&id);
            }
            drop(map);
            publish(bus, EventKind::AgentError { id, message }).await;
            set_status(bus, id, AgentStatus::Error).await;
            return true;
        }
    };
    publish(bus, kind).await;
    false
}

/// A notification digest waiting for its turn.
struct Digest {
    message: MessageId,
    lines: BTreeMap<String, String>,
}

impl Digest {
    fn text(&self) -> String {
        let mut text = String::from("CrossAgents notices (things you asked to hear about):\n");
        for line in self.lines.values() {
            text.push_str("- ");
            text.push_str(line);
            text.push('\n');
        }
        text
    }
}

/// The first line of what was said (skipping the `Message from …:` header `agent_send`
/// adds), at most 80 characters.
fn excerpt(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !(l.starts_with("Message from ") && l.ends_with(':')))
        .unwrap_or_default()
        .trim();
    let mut out: String = line.chars().take(80).collect();
    if out.len() < line.len() {
        out.push('…');
    }
    out
}

/// Live output of one tool call kept in the event log; the rest is dropped (the call's
/// detail still carries the tail of it).
const TOOL_OUTPUT_LIMIT: usize = 64 * 1024;
/// Output kept in a tool call's detail.
const DETAIL_OUTPUT_LIMIT: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Stream {
    Output(OutputStream),
    Tool(String),
}

/// Joins streamed text into fewer events.
#[derive(Default)]
struct Coalescer {
    buffer: Option<(Stream, String)>,
    /// Bytes of live output published per tool call.
    tool_bytes: HashMap<String, usize>,
}

impl Coalescer {
    async fn push(&mut self, bus: &EventBus, id: AgentId, stream: Stream, text: String) {
        let text = match &stream {
            Stream::Tool(call) => {
                let used = self.tool_bytes.entry(call.clone()).or_default();
                let room = TOOL_OUTPUT_LIMIT.saturating_sub(*used);
                if room == 0 {
                    return;
                }
                let text = head(text, room);
                *used += text.len();
                text
            }
            Stream::Output(_) => text,
        };
        match &mut self.buffer {
            Some((current, buf)) if *current == stream => buf.push_str(&text),
            _ => {
                self.flush(bus, id).await;
                self.buffer = Some((stream, text));
            }
        }
    }

    async fn flush(&mut self, bus: &EventBus, id: AgentId) {
        let Some((stream, text)) = self.buffer.take() else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let kind = match stream {
            Stream::Output(stream) => EventKind::AgentOutput { id, stream, text },
            Stream::Tool(tool_call_id) => EventKind::AgentToolOutput {
                id,
                tool_call_id,
                text,
            },
        };
        publish(bus, kind).await;
    }
}

/// At most `limit` bytes from the start of `text`, cut at a character boundary.
fn head(mut text: String, limit: usize) -> String {
    if text.len() > limit {
        let mut end = limit;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// At most `limit` bytes from the end of `text`, marked when something was cut.
fn tail(text: String, limit: usize) -> String {
    if text.len() <= limit {
        return text;
    }
    let mut start = text.len() - limit;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!(
        "…（前面省略 {start} 字节）
{}",
        &text[start..]
    )
}
