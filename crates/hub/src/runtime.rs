//! Live agent sessions: start, deliver messages one turn at a time, cancel, stop, and turn
//! what happens inside a session into events.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use crossagents_core::{
    Agent, AgentId, AgentKey, AgentStatus, AttachmentSupport, Capabilities, CoreError,
    DeliveryState, DeliveryStatus, DiffSource, DriverSession, EXTERNAL_PLUGIN, Envelope, EventBus,
    EventKind, McpServerSpec, MessageId, MessageSource, NameSource, OutputStream, PermissionPolicy,
    Prompt, QueuedReason, SessionEvent, SessionEventKind, SessionSpec, StopReason, Store,
    validate_agent_name,
};
use crossagents_plugins::PluginRegistry;
use tokio::sync::{Mutex, Notify, mpsc};

use crate::stream_rules::{self, Injection, StreamRules};

const FLUSH_INTERVAL: Duration = Duration::from_millis(150);

/// Extra per-session setup supplied by higher layers (collaboration MCP server, role
/// instructions, ...).
#[async_trait]
pub trait SessionSetup: Send + Sync {
    async fn prepare(&self, agent: &Agent) -> Result<SessionExtras, CoreError>;

    /// Only the environment of [`Self::prepare`], for taking a running session over
    /// (nothing new is issued for it).
    async fn environment(&self, agent: &Agent) -> Result<Vec<(String, String)>, CoreError> {
        Ok(self.prepare(agent).await?.env)
    }

    /// How the agent's permission requests are decided: an override of its policy and a
    /// handler (docs/tasks/DOGFOOD.md §7). Also for sessions taken over.
    async fn permissions(
        &self,
        _agent: &Agent,
    ) -> (
        Option<PermissionPolicy>,
        Option<Arc<dyn crossagents_core::PermissionHandler>>,
    ) {
        (None, None)
    }
}

#[derive(Default, Clone)]
pub struct SessionExtras {
    pub mcp_servers: Vec<McpServerSpec>,
    pub instructions: Option<String>,
    /// Extra environment for the agent process (e.g. its account's home).
    pub env: Vec<(String, String)>,
    /// Overrides the agent's own permission policy (`config.permission`).
    pub permission: Option<PermissionPolicy>,
    /// Decides the session's permission requests (docs/tasks/DOGFOOD.md §7).
    pub permissions: Option<Arc<dyn crossagents_core::PermissionHandler>>,
}

struct Live {
    generation: u64,
    session: Arc<dyn DriverSession>,
    inbox: Arc<Inbox>,
    control: mpsc::UnboundedSender<Control>,
    /// What the session's agent declared it takes as files (ACP `promptCapabilities`);
    /// `None` when only its driver's static answer is known.
    attachments: Option<AttachmentSupport>,
    /// The driver can hand a message to the running turn (`DriverSession::steer`).
    steer: bool,
}

/// What became of a message handed to an agent.
#[derive(Debug, Clone, PartialEq)]
pub struct Delivery {
    pub message_id: MessageId,
    /// It waits for its turn (or, for a steer that could not go in, for the next one).
    pub queued: bool,
    /// Why it waits, or why a steer did not go in; `None` when it started at once.
    pub queued_reason: Option<QueuedReason>,
    /// It went into the running turn (`agent.steered`); `queued` is then `false`.
    pub steered: bool,
}

impl Delivery {
    fn new(message_id: MessageId, queued: bool, queued_reason: Option<QueuedReason>) -> Self {
        Self {
            message_id,
            queued,
            queued_reason,
            steered: false,
        }
    }
}

/// Why a steer produced no delivery.
#[derive(Debug)]
pub enum SteerFailure {
    /// The message never reached the CLI: nothing was queued, and the sender decides
    /// whether to send it again (docs/tasks/DELIVERY-AUDIT.md §3).
    Refused {
        message_id: MessageId,
        reason: QueuedReason,
        error: String,
    },
    /// Nothing was delivered for another reason (the agent is archived, it stopped, ...).
    Error(CoreError),
}

impl From<CoreError> for SteerFailure {
    fn from(err: CoreError) -> Self {
        Self::Error(err)
    }
}

/// One fixed client id's first send: `Pending` while it is being delivered, `Done` after.
enum ClientEntry {
    Pending(Arc<Notify>),
    Done(Delivery),
}

/// How many finished deliveries one agent keeps by client id. A hub runs for weeks, so the
/// ids it will never see again are dropped oldest first instead of piling up; a send still
/// in flight (`Pending`) is never dropped (docs/tasks/DELIVERY-AUDIT.md §4).
const CLIENT_HISTORY: usize = 256;

/// Deliveries by fixed client id, per agent, bounded to [`CLIENT_HISTORY`] each.
#[derive(Default)]
struct ClientLog {
    agents: HashMap<AgentId, ClientEntries>,
}

#[derive(Default)]
struct ClientEntries {
    /// The latest delivery by client id.
    entries: HashMap<MessageId, ClientEntry>,
    /// The `Done` ids in the order they were kept, oldest first: what overflows drops.
    order: VecDeque<MessageId>,
}

impl ClientLog {
    fn get(&self, agent: AgentId, client: MessageId) -> Option<&ClientEntry> {
        self.agents.get(&agent)?.entries.get(&client)
    }

    /// Remembers a send that is being delivered, until it settles.
    fn pending(&mut self, agent: AgentId, client: MessageId, wait: Arc<Notify>) {
        self.agents
            .entry(agent)
            .or_default()
            .entries
            .insert(client, ClientEntry::Pending(wait));
    }

    /// Keeps the delivery for repeats (dropping this agent's oldest kept one once they pass
    /// [`CLIENT_HISTORY`]), or frees the id when nothing was delivered.
    fn settle(&mut self, agent: AgentId, client: MessageId, delivery: Option<Delivery>) {
        let Some(delivery) = delivery else {
            if let Some(entries) = self.agents.get_mut(&agent) {
                entries.entries.remove(&client);
            }
            return;
        };
        let entries = self.agents.entry(agent).or_default();
        entries.entries.insert(client, ClientEntry::Done(delivery));
        entries.order.push_back(client);
        while entries.order.len() > CLIENT_HISTORY {
            if let Some(oldest) = entries.order.pop_front() {
                entries.entries.remove(&oldest);
            }
        }
    }

    /// Forgets everything kept for an agent that stopped or was archived: a repeat to a
    /// session that is gone is a new turn, not the old delivery.
    fn forget(&mut self, agent: AgentId) {
        self.agents.remove(&agent);
    }
}

/// What a send with a client id found.
enum ClientClaim {
    /// This id was already delivered: answer with that delivery.
    Repeat(Delivery),
    /// The first send on this id: deliver, then [`Runtime::settle_client`] it.
    First(Option<(MessageId, Arc<Notify>)>),
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
    waiting: std::sync::Mutex<VecDeque<(MessageId, Prompt)>>,
    /// The messages a person sent, waiting or running: work is not put ahead of them
    /// (docs/tasks/S1.md §3.3).
    from_person: std::sync::Mutex<HashSet<MessageId>>,
    /// The message whose turn is running.
    running: std::sync::Mutex<Option<MessageId>>,
    /// A turn is running.
    busy: AtomicBool,
    closed: AtomicBool,
    notify: Notify,
    /// Held by a sender from the moment it works out whether its message waits until the
    /// moment it is in line. Without it two sends can both be told they are next and then
    /// enter the queue in a different order than they were told
    /// (docs/tasks/DELIVERY-AUDIT.md §7, item 8).
    admit: tokio::sync::Mutex<()>,
}

impl Inbox {
    fn push(&self, message: (MessageId, Prompt), first: bool) {
        let mut waiting = self.waiting.lock().unwrap();
        if first {
            waiting.push_front(message);
        } else {
            waiting.push_back(message);
        }
        drop(waiting);
        self.notify.notify_one();
    }

    /// Whether a message admitted now waits, and why. Taken under the admission lock (see
    /// [`Self::admit`]), with the push, so a concurrent sender cannot slip in between.
    fn queue_state(&self) -> (bool, Option<QueuedReason>) {
        // Same lock order as `next` and `is_idle`: waiting, then the turn flags.
        let waiting = self.waiting.lock().unwrap();
        let busy = self.busy.load(Ordering::SeqCst);
        if waiting.is_empty() && !busy {
            return (false, None);
        }
        let running = *self.running.lock().unwrap();
        let person = self.from_person.lock().unwrap();
        // A person's message that is not the running one is waiting ahead of this send.
        let ahead = person.iter().any(|id| Some(*id) != running);
        let reason = if ahead {
            QueuedReason::OwnerQueueAhead
        } else {
            QueuedReason::TurnRunning
        };
        (true, Some(reason))
    }

    /// Moves a waiting message to the front, keeping its id and its place as a person's
    /// message; `Some(queued)` when it was waiting, `None` when it already started or is
    /// unknown. `queued`: a turn must stop before it runs.
    fn promote(&self, id: MessageId) -> Option<bool> {
        let mut waiting = self.waiting.lock().unwrap();
        let index = waiting.iter().position(|(m, _)| *m == id)?;
        let message = waiting.remove(index).expect("found just above");
        let queued = self.busy.load(Ordering::SeqCst);
        waiting.push_front(message);
        drop(waiting);
        self.notify.notify_one();
        Some(queued)
    }

    /// Replaces a waiting message's text; `false` if it is not waiting any more.
    fn update(&self, id: MessageId, text: &str) -> bool {
        let mut waiting = self.waiting.lock().unwrap();
        match waiting.iter_mut().find(|(m, _)| *m == id) {
            Some((_, old)) => {
                text.clone_into(&mut old.text);
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
        self.from_person.lock().unwrap().remove(&id);
        true
    }

    /// The next message, marking a turn as running; `None` once closed.
    async fn next(&self) -> Option<(MessageId, Prompt)> {
        loop {
            if self.closed.load(Ordering::SeqCst) {
                return None;
            }
            if let Some(message) = self.waiting.lock().unwrap().pop_front() {
                self.busy.store(true, Ordering::SeqCst);
                *self.running.lock().unwrap() = Some(message.0);
                return Some(message);
            }
            self.notify.notified().await;
        }
    }

    fn finish_turn(&self) {
        if let Some(id) = self.running.lock().unwrap().take() {
            self.from_person.lock().unwrap().remove(&id);
        }
        self.busy.store(false, Ordering::SeqCst);
    }

    /// A person's message is being answered or waits in line.
    fn serving_person(&self) -> bool {
        !self.from_person.lock().unwrap().is_empty()
    }

    /// Why the agent cannot be moved now, if it cannot: what it is doing.
    fn busy_with(&self, name: &str) -> Option<String> {
        if self.is_idle() {
            return None;
        }
        let running = *self.running.lock().unwrap();
        let person = self.from_person.lock().unwrap();
        Some(match running {
            Some(id) if person.contains(&id) => format!("{name} is answering a person's message"),
            _ if !person.is_empty() => format!("a person's message is waiting for {name}"),
            Some(_) => format!("{name} is in the middle of a turn"),
            None => format!("messages are waiting for {name}"),
        })
    }

    fn is_idle(&self) -> bool {
        let waiting = self.waiting.lock().unwrap();
        waiting.is_empty() && !self.busy.load(Ordering::SeqCst)
    }

    /// What a client should see: the same facts `is_idle` and `busy_with` read, in one
    /// answer (docs/tasks/DELIVERY-AUDIT.md §8 P1-4).
    fn delivery(&self) -> DeliveryStatus {
        // Same lock order as `next` and `is_idle`: waiting, then the turn flags.
        let waiting = self.waiting.lock().unwrap();
        let queued = waiting.len();
        let state = if self.busy.load(Ordering::SeqCst) {
            DeliveryState::Turn
        } else if queued > 0 {
            // A message is in line but its turn has not begun: the session is starting or
            // the previous turn just ended.
            DeliveryState::Starting
        } else {
            DeliveryState::Idle
        };
        DeliveryStatus { state, queued }
    }

    /// A turn runs: a message can be steered into it.
    fn is_running(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
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
    /// The CrossAgents home, for what a session reads from it as it starts (an extension's
    /// stream rules). `None` when the runtime was built without one: then only the
    /// project's own rules are read.
    home: Option<PathBuf>,
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
    notices: Mutex<HashMap<AgentId, BTreeMap<String, String>>>,
    /// Agents to send back to their workspace once their turn ends ([`Self::send_home`]
    /// while they were busy).
    going_home: Mutex<HashSet<AgentId>>,
    /// Agents to archive once their turn ends ([`Self::archive_after_turn`] while they were
    /// busy).
    archiving: Mutex<HashSet<AgentId>>,
    /// Answers to teammates that were only written, to forward (crate::replies).
    replies: std::sync::Mutex<crate::replies::ReplyTracker>,
    /// Each fixed client id's first delivery, by agent: a repeat gets it instead of a new
    /// turn (docs/tasks/DELIVERY-AUDIT.md §4).
    clients: Mutex<ClientLog>,
    generation: AtomicU64,
}

/// Messages to an agent's event forwarder, kept in order with the session's own events.
enum Control {
    TurnStarted(MessageId),
    /// The turn's outcome, and what it changed when the hub works that out itself.
    TurnFinished(MessageId, Result<StopReason, String>, Option<String>),
    /// The session's context was compacted (`Runtime::compact`): stream rules may repeat.
    Compacted,
    Stopped,
    Detached,
}

/// A session's event and control channels.
struct Channels {
    events: mpsc::UnboundedReceiver<SessionEvent>,
    control_tx: mpsc::UnboundedSender<Control>,
    control_rx: mpsc::UnboundedReceiver<Control>,
    generation: u64,
}

/// What a hub start found of the previous hub's agents (see [`Runtime::recover`]).
#[derive(Debug, Default)]
pub struct Recovery {
    /// Taken over with their processes.
    pub adopted: Vec<AgentId>,
    /// Their running turn was lost and closed as `interrupted`.
    pub interrupted: Vec<AgentId>,
    /// Messages that had not started and were queued again.
    pub requeued: usize,
    /// Runtime hosts nobody claimed, stopped.
    pub released: usize,
}

/// One agent's unfinished business in the event log.
#[derive(Debug, Default)]
struct Unfinished {
    /// The turn that started and did not end.
    running: Option<MessageId>,
    /// Messages delivered that never started, in order.
    waiting: Vec<(MessageId, Prompt)>,
}

impl Runtime {
    pub fn new(bus: Arc<EventBus>, plugins: PluginRegistry) -> Self {
        Self {
            bus,
            plugins,
            setup: None,
            home: None,
            live: LiveMap::default(),
            locks: Mutex::default(),
            digests: Mutex::default(),
            notices: Mutex::default(),
            external: Mutex::default(),
            going_home: Mutex::default(),
            archiving: Mutex::default(),
            replies: std::sync::Mutex::default(),
            clients: Mutex::default(),
            generation: AtomicU64::new(0),
        }
    }

    pub fn with_setup(mut self, setup: Arc<dyn SessionSetup>) -> Self {
        self.setup = Some(setup);
        self
    }

    /// The home whose enabled extensions contribute stream rules to a new session.
    pub fn with_home(mut self, home: PathBuf) -> Self {
        self.home = Some(home);
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

    /// Picks up where the previous hub left off, at hub start before anything else runs
    /// (docs/PROCESS-SUPERVISION.md): takes over the agent processes it left running in
    /// runtime hosts, closes the turns lost with it as `interrupted`, queues again the
    /// messages that never started, and marks every other agent stopped. `since`: the
    /// event seq to look back from (`None`: nothing to pick up but processes).
    pub async fn recover(self: &Arc<Self>, since: Option<u64>) -> Result<Recovery, CoreError> {
        let discovery = crossagents_drivers::host::discover().await;
        let unfinished = match since {
            Some(since) => self.unfinished(since).await?,
            None => HashMap::new(),
        };
        let mut recovery = Recovery::default();
        for agent in self.store().list_agents().await? {
            let running = unfinished.get(&agent.id).and_then(|u| u.running);
            let was_live = matches!(
                agent.status,
                AgentStatus::Idle | AgentStatus::Running | AgentStatus::Starting
            );
            let adopt = was_live
                && !discovery.live.is_empty()
                && agent.plugin != EXTERNAL_PLUGIN
                && agent.archived_at.is_none();
            if adopt {
                match self.adopt(&agent, running).await {
                    Ok(true) => {
                        recovery.adopted.push(agent.id);
                        continue;
                    }
                    Ok(false) => {}
                    Err(err) => {
                        tracing::warn!(agent = %agent.name, error = %err, "cannot take the agent over");
                    }
                }
            }
            set_status(&self.bus, agent.id, AgentStatus::Stopped).await;
            if let Some(message_id) = running {
                publish(
                    &self.bus,
                    EventKind::AgentWarning {
                        id: agent.id,
                        message: "the turn was interrupted: its process ended with the previous hub; it is not run again".into(),
                    },
                )
                .await;
                publish(
                    &self.bus,
                    EventKind::AgentTurnCompleted {
                        id: agent.id,
                        message_id,
                        stop_reason: StopReason::Interrupted,
                    },
                )
                .await;
                recovery.interrupted.push(agent.id);
            }
        }
        recovery.released = crossagents_drivers::host::release_unclaimed().await.len();
        // Messages that never started go back in line, in the background: their agents
        // start (or resume) as for any message.
        for (id, work) in unfinished {
            if work.waiting.is_empty() {
                continue;
            }
            let Ok(agent) = self.store().get_agent(&AgentKey::Id(id)).await else {
                continue;
            };
            if agent.plugin == EXTERNAL_PLUGIN || agent.archived_at.is_some() {
                continue;
            }
            recovery.requeued += work.waiting.len();
            let this = self.clone();
            tokio::spawn(async move {
                if let Err(err) = this.requeue(id, work.waiting).await {
                    tracing::warn!(agent = %agent.name, error = %err, "cannot queue the agent's waiting messages again");
                }
            });
        }
        Ok(recovery)
    }

    /// What every agent left unfinished in the events after `since`: a turn that started
    /// and did not end, messages that never started.
    async fn unfinished(&self, since: u64) -> Result<HashMap<AgentId, Unfinished>, CoreError> {
        let mut out: HashMap<AgentId, Unfinished> = HashMap::new();
        let mut after = since;
        loop {
            let page = self.store().events_since(after, 500).await?;
            let Some(last) = page.last().map(|e| e.seq) else {
                return Ok(out);
            };
            after = last;
            for event in page {
                match event.kind {
                    EventKind::AgentInput {
                        id,
                        message_id,
                        text,
                        attachments,
                        ..
                    } => out
                        .entry(id)
                        .or_default()
                        .waiting
                        .push((message_id, Prompt { text, attachments })),
                    EventKind::AgentInputUpdated {
                        id,
                        message_id,
                        text,
                    } => {
                        if let Some(waiting) = out
                            .get_mut(&id)
                            .and_then(|u| u.waiting.iter_mut().find(|(m, _)| *m == message_id))
                        {
                            waiting.1.text = text;
                        }
                    }
                    EventKind::AgentInputWithdrawn { id, message_id } => {
                        if let Some(u) = out.get_mut(&id) {
                            u.waiting.retain(|(m, _)| *m != message_id);
                        }
                    }
                    EventKind::AgentTurnStarted { id, message_id } => {
                        let u = out.entry(id).or_default();
                        u.waiting.retain(|(m, _)| *m != message_id);
                        u.running = Some(message_id);
                    }
                    EventKind::AgentTurnCompleted { id, message_id, .. } => {
                        if let Some(u) = out.get_mut(&id)
                            && u.running == Some(message_id)
                        {
                            u.running = None;
                        }
                    }
                    EventKind::AgentRemoved { id, .. } => {
                        out.remove(&id);
                    }
                    _ => {}
                }
            }
        }
    }

    /// Takes over the agent's process left running by the previous hub; `false` when there
    /// is none to take over.
    async fn adopt(&self, agent: &Agent, running: Option<MessageId>) -> Result<bool, CoreError> {
        let Ok(resolved) = self.plugins.driver(&agent.plugin, agent.driver.as_deref()) else {
            return Ok(false);
        };
        let Some(session_id) = self.store().last_session_id(agent.id).await? else {
            return Ok(false);
        };
        // The same environment as at its start: it names the process to take over.
        let env = match &self.setup {
            Some(setup) => setup.environment(agent).await?,
            None => Vec::new(),
        };
        let (permission, permissions) = match &self.setup {
            Some(setup) => setup.permissions(agent).await,
            None => (None, None),
        };
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);
        let spec = SessionSpec {
            agent: agent.clone(),
            mcp_servers: Vec::new(),
            permission: permission.unwrap_or_else(|| permission_of(agent)),
            permissions,
            command: command_of(agent),
            instructions: None,
            env,
            events: events_tx,
        };
        let Some(session) = resolved
            .driver
            .adopt(&session_id, running.is_some(), spec)
            .await?
        else {
            return Ok(false);
        };
        let diff_dir = (!resolved.driver.capabilities().structured_diff).then(|| agent.cwd.clone());
        self.install(
            agent.id,
            Arc::from(session),
            Channels {
                events: events_rx,
                control_tx,
                control_rx,
                generation,
            },
            diff_dir,
            running,
            resolved.driver.capabilities().steer,
        )
        .await;
        let status = if running.is_some() {
            AgentStatus::Running
        } else {
            AgentStatus::Idle
        };
        set_status(&self.bus, agent.id, status).await;
        publish(
            &self.bus,
            EventKind::AgentAdopted {
                id: agent.id,
                message_id: running,
            },
        )
        .await;
        Ok(true)
    }

    /// Puts messages delivered before the hub restarted back in the agent's line (they
    /// were announced then; they are not announced again).
    async fn requeue(
        &self,
        id: AgentId,
        waiting: Vec<(MessageId, Prompt)>,
    ) -> Result<(), CoreError> {
        let agent = self.start(&AgentKey::Id(id)).await?;
        let live = self
            .live
            .lock()
            .await
            .get(&agent.id)
            .cloned()
            .filter(|l| !l.inbox.is_closed())
            .ok_or_else(|| CoreError::Agent(format!("agent {} stopped", agent.name)))?;
        for message in waiting {
            live.inbox.push(message, false);
        }
        self.announce_delivery(agent.id).await;
        Ok(())
    }

    /// The hub is going away: agents running in runtime hosts are left running for the
    /// next hub to take over (their waiting messages with them); the others are stopped.
    pub async fn leave(&self) {
        crossagents_drivers::host::leave();
        let lives: Vec<(AgentId, Arc<Live>)> = self.live.lock().await.drain().collect();
        for (id, live) in lives {
            if live.session.outlives_hub() {
                let _ = live.control.send(Control::Detached);
                continue;
            }
            self.withdraw_waiting(id, &live).await;
            let _ = live.session.interrupt().await;
            live.session.shutdown().await;
            let _ = live.control.send(Control::Stopped);
            set_status(&self.bus, id, AgentStatus::Stopped).await;
        }
    }

    /// Takes back every message still waiting in a session that goes away.
    async fn withdraw_waiting(&self, id: AgentId, live: &Live) {
        let waiting: Vec<MessageId> = live
            .inbox
            .waiting
            .lock()
            .unwrap()
            .drain(..)
            .map(|(m, _)| m)
            .collect();
        for message_id in waiting {
            publish(&self.bus, EventKind::AgentInputWithdrawn { id, message_id }).await;
        }
    }

    async fn agent_lock(&self, id: AgentId) -> Arc<Mutex<()>> {
        self.locks.lock().await.entry(id).or_default().clone()
    }

    pub async fn is_live(&self, id: AgentId) -> bool {
        self.live.lock().await.contains_key(&id)
    }

    /// What the agent's driver can do now (docs/TODO.md §7): a live session's own
    /// declaration of the files it takes (ACP `promptCapabilities`) overrides the
    /// driver's static answer.
    pub async fn capabilities(&self, agent: &Agent) -> Option<Capabilities> {
        let resolved = self
            .plugins
            .driver(&agent.plugin, agent.driver.as_deref())
            .ok()?;
        let mut capabilities = resolved.driver.capabilities();
        if let Some(move_cwd) = self.plugins.config(&agent.plugin).and_then(|c| c.move_cwd) {
            capabilities.move_cwd = move_cwd;
        }
        let declared = self
            .live
            .lock()
            .await
            .get(&agent.id)
            .and_then(|live| live.attachments);
        if let Some(declared) = declared {
            capabilities.attachments = Some(declared);
        }
        Some(capabilities)
    }

    /// What the agent takes as files now: its live session's declaration, else its
    /// driver's static answer. `None`: not known until the agent starts. Upstreams refuse
    /// the kinds it does not take instead of passing a reference the agent cannot use.
    /// Starting an agent whose answer is still unknown (an ACP agent) is what a message
    /// with files would do anyway.
    pub async fn attachment_support(
        &self,
        agent: &Agent,
    ) -> Result<Option<AttachmentSupport>, CoreError> {
        // Agents outside the hub read the files themselves, by path.
        if agent.plugin == EXTERNAL_PLUGIN {
            return Ok(Some(AttachmentSupport::ALL));
        }
        let declared = self
            .live
            .lock()
            .await
            .get(&agent.id)
            .and_then(|live| live.attachments);
        if declared.is_some() {
            return Ok(declared);
        }
        let Ok(resolved) = self.plugins.driver(&agent.plugin, agent.driver.as_deref()) else {
            return Ok(None);
        };
        if let Some(support) = resolved.driver.capabilities().attachments {
            return Ok(Some(support));
        }
        // An ACP agent declares its kinds at `initialize`, once its session starts.
        self.start(&AgentKey::Id(agent.id)).await?;
        Ok(self
            .live
            .lock()
            .await
            .get(&agent.id)
            .and_then(|live| live.attachments))
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
        // Read again under the lock: `place` may have moved it meanwhile.
        let agent = self.store().get_agent(&AgentKey::Id(agent.id)).await?;
        self.open(&agent, false).await?;
        Ok(agent)
    }

    /// Opens (or resumes) the agent's session; the caller holds its lock. With `carry` a
    /// session that cannot be resumed is an error instead of a new one: the agent was moved
    /// to work with its conversation (docs/tasks/S1.md §3.1).
    async fn open(&self, agent: &Agent, carry: bool) -> Result<(), CoreError> {
        let agent = agent.clone();
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
        let (mut events_tx, mut events_rx) = mpsc::unbounded_channel();
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);

        let spec = |events: &mpsc::UnboundedSender<SessionEvent>| SessionSpec {
            agent: agent.clone(),
            mcp_servers: extras.mcp_servers.clone(),
            permission: extras.permission.unwrap_or_else(|| permission_of(&agent)),
            permissions: extras.permissions.clone(),
            command: command_of(&agent),
            instructions: extras.instructions.clone(),
            env: extras.env.clone(),
            events: events.clone(),
        };
        // Carry on the agent's last conversation when the driver can, unless it is known
        // not to carry on from where the agent is now.
        let lost = agent.context_lost().map(str::to_owned);
        let previous = if resolved.driver.capabilities().resume && lost.is_none() {
            self.store()
                .last_session_id(agent.id)
                .await
                .unwrap_or_default()
        } else {
            None
        };
        let opened = match previous {
            Some(previous) => match resolved.driver.resume(&previous, spec(&events_tx)).await {
                Ok(session) => Ok(session),
                Err(err) if carry => Err(CoreError::Agent(format!(
                    "cannot resume {}'s session in {} ({err}): its context was not carried over",
                    agent.name,
                    agent.cwd.display()
                ))),
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
                    // A channel of its own: what the failed attempt still reports (its
                    // process exiting) must not end the new session.
                    (events_tx, events_rx) = mpsc::unbounded_channel();
                    resolved.driver.open(spec(&events_tx)).await
                }
            },
            None => resolved.driver.open(spec(&events_tx)).await,
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

        // Only now that the new session is up: a start that failed says it again next time.
        if let Some(why) = lost {
            self.forget_lost_context(agent.id).await;
            publish(
                &self.bus,
                EventKind::AgentWarning {
                    id: agent.id,
                    message: format!("这是新会话，没有带上之前的上下文：{why}"),
                },
            )
            .await;
        }

        // Drivers that do not report what a turn changed get it worked out by the hub.
        let diff_dir = (!resolved.driver.capabilities().structured_diff).then(|| agent.cwd.clone());
        self.install(
            agent.id,
            session,
            Channels {
                events: events_rx,
                control_tx,
                control_rx,
                generation,
            },
            diff_dir,
            None,
            resolved.driver.capabilities().steer,
        )
        .await;
        set_status(&self.bus, agent.id, AgentStatus::Idle).await;
        Ok(())
    }

    /// Moves the agent to work in `dir` (docs/tasks/S1.md §3.1): an idle session's process
    /// stops, and the session resumes there, with its conversation. Busy while a turn runs
    /// or a message waits: what was asked of it is not cut off, nor put behind the move.
    /// With `carry` the session resumes right away and failing to is an error (it is not
    /// replaced by a new one); without, it resumes with its next message, as any start does.
    pub async fn place(&self, key: &AgentKey, dir: &Path, carry: bool) -> Result<Agent, CoreError> {
        let agent = self.store().get_agent(key).await?;
        if agent.plugin == EXTERNAL_PLUGIN {
            return Err(CoreError::Conflict(format!(
                "{} runs outside the hub and cannot be moved",
                agent.name
            )));
        }
        let lock = self.agent_lock(agent.id).await;
        let _guard = lock.lock().await;
        let live = {
            let mut live = self.live.lock().await;
            match live.get(&agent.id) {
                Some(l) => match l.inbox.busy_with(&agent.name) {
                    Some(why) => return Err(CoreError::Busy(why)),
                    None => live.remove(&agent.id),
                },
                None => None,
            }
        };
        if let Some(live) = live {
            live.session.shutdown().await;
            let _ = live.control.send(Control::Stopped);
            set_status(&self.bus, agent.id, AgentStatus::Stopped).await;
        }
        self.going_home.lock().await.remove(&agent.id);
        if agent.cwd != dir {
            self.store().update_agent_cwd(agent.id, dir).await?;
        }
        let agent = self.store().get_agent(&AgentKey::Id(agent.id)).await?;
        publish(
            &self.bus,
            EventKind::AgentUpdated {
                agent: agent.clone(),
            },
        )
        .await;
        if carry && self.store().last_session_id(agent.id).await?.is_some() {
            self.open(&agent, true).await?;
        }
        Ok(agent)
    }

    /// Sends the agent back to its workspace: now when it is idle, else once its turn ends
    /// and nothing waits for it. `true` when it moved now.
    pub async fn send_home(&self, key: &AgentKey) -> Result<bool, CoreError> {
        let agent = self.store().get_agent(key).await?;
        match self.place(key, agent.home(), false).await {
            Ok(_) => Ok(true),
            Err(CoreError::Busy(_)) => {
                self.going_home.lock().await.insert(agent.id);
                // Its turn may have ended in between, with nobody left to notice.
                self.settle_home(agent.id).await;
                Ok(false)
            }
            Err(err) => Err(err),
        }
    }

    /// Moves an agent waiting to go home there, if it is idle now.
    async fn settle_home(&self, id: AgentId) {
        if !self.going_home.lock().await.contains(&id) {
            return;
        }
        let agent = match self.store().get_agent(&AgentKey::Id(id)).await {
            Ok(agent) => agent,
            Err(_) => {
                self.going_home.lock().await.remove(&id);
                return;
            }
        };
        match self.place(&AgentKey::Id(id), agent.home(), false).await {
            Ok(_) | Err(CoreError::Busy(_)) => {}
            Err(err) => {
                self.going_home.lock().await.remove(&id);
                tracing::warn!(%id, error = %err, "cannot send the agent home");
            }
        }
    }

    /// Archives the agent once its turn ends and nothing waits for it: now when it is idle.
    /// `true` when it was archived now.
    pub async fn archive_after_turn(&self, key: &AgentKey) -> Result<bool, CoreError> {
        let agent = self.store().get_agent(key).await?;
        if !self.is_busy(agent.id).await {
            self.archive(&AgentKey::Id(agent.id), true).await?;
            return Ok(true);
        }
        self.archiving.lock().await.insert(agent.id);
        // Its turn may have ended in between, with nobody left to notice.
        self.settle_archive(agent.id).await;
        Ok(false)
    }

    /// Archives an agent waiting to be, if it is idle now.
    async fn settle_archive(&self, id: AgentId) {
        if !self.archiving.lock().await.contains(&id) || self.is_busy(id).await {
            return;
        }
        // Taken out first, so that two turns ending together archive it once.
        if !self.archiving.lock().await.remove(&id) {
            return;
        }
        if let Err(err) = self.archive(&AgentKey::Id(id), true).await {
            tracing::warn!(%id, error = %err, "cannot archive the agent after its turn");
        }
    }

    /// Follows turns ending, to send home (or archive) the agents that were busy when asked.
    pub fn watch_moves(self: &Arc<Self>) {
        let this = self.clone();
        let mut events = self.bus.subscribe();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        let id = match event.kind {
                            EventKind::AgentTurnCompleted { id, .. }
                            | EventKind::AgentStatusChanged { id, .. } => id,
                            _ => continue,
                        };
                        if this.going_home.lock().await.contains(&id) {
                            let this = this.clone();
                            tokio::spawn(async move { this.settle_home(id).await });
                        }
                        if this.archiving.lock().await.contains(&id) {
                            let this = this.clone();
                            tokio::spawn(async move { this.settle_archive(id).await });
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let ids: Vec<AgentId> =
                            this.going_home.lock().await.iter().copied().collect();
                        for id in ids {
                            this.settle_home(id).await;
                        }
                        let ids: Vec<AgentId> =
                            this.archiving.lock().await.iter().copied().collect();
                        for id in ids {
                            this.settle_archive(id).await;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }

    /// Follows turns that answer a teammate's message, and forwards the answer when the agent
    /// only wrote it (crate::replies). Agents on a run's task are left out: they report
    /// through the task tools, and their turn texts are not meant for the lead.
    pub fn watch_replies(self: &Arc<Self>) {
        let this = self.clone();
        let mut events = self.bus.subscribe();
        tokio::spawn(async move {
            loop {
                let event = match events.recv().await {
                    Ok(event) => event,
                    // A forward or two may be missed; nothing else depends on it.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                };
                let forward = this
                    .replies
                    .lock()
                    .unwrap()
                    .on_event(&event.kind, std::time::Instant::now());
                if let Some(forward) = forward {
                    let this = this.clone();
                    tokio::spawn(async move { this.forward_reply(forward).await });
                }
            }
        });
    }

    async fn forward_reply(&self, forward: crate::replies::Forward) {
        for id in [forward.from, forward.to] {
            if matches!(self.store().claim_of(id).await, Ok(Some(_))) {
                return;
            }
        }
        let Ok(from) = self.store().get_agent(&AgentKey::Id(forward.from)).await else {
            return;
        };
        let id = MessageId::new();
        let text = format!(
            "Message from {} (id {id}; its answer to your message, forwarded by the hub because it did not send one):\n{}",
            from.name, forward.text
        );
        let envelope = Envelope {
            id,
            from: MessageSource::Agent {
                agent_id: forward.from,
            },
            text,
            reply_to: Some(forward.answers),
            attachments: Vec::new(),
        };
        self.replies.lock().unwrap().mark_forwarded(id);
        if let Err(err) = self
            .deliver(&AgentKey::Id(forward.to), envelope, false, None, None)
            .await
        {
            tracing::warn!(from = %forward.from, to = %forward.to, error = %err, "cannot forward an answer");
        }
    }

    /// A teammate's message, as the reply tracker needs to know it was sent.
    async fn note_send(&self, key: &AgentKey, envelope: &Envelope) {
        let MessageSource::Agent { agent_id } = envelope.from else {
            return;
        };
        let to = match key {
            AgentKey::Id(id) => *id,
            other => match self.store().get_agent(other).await {
                Ok(agent) => agent.id,
                Err(_) => return,
            },
        };
        self.replies
            .lock()
            .unwrap()
            .note_send(agent_id, to, std::time::Instant::now());
    }

    /// Whether a person waits on the agent: its running turn answers a person's message,
    /// or one is in line. Work is not given to it ahead of them (docs/tasks/S1.md §3.3).
    pub async fn serving_person(&self, id: AgentId) -> bool {
        self.live
            .lock()
            .await
            .get(&id)
            .is_some_and(|live| live.inbox.serving_person())
    }

    /// Whether a turn runs or a message waits.
    pub async fn is_busy(&self, id: AgentId) -> bool {
        self.live
            .lock()
            .await
            .get(&id)
            .is_some_and(|live| !live.inbox.is_idle())
    }

    /// What the agent's session does with messages now, from the same inbox the hub admits
    /// them into (docs/tasks/DELIVERY-AUDIT.md §8 P1-4). An external agent's messages wait
    /// for it to fetch them (`held`); an agent with no session is idle, or still `starting`
    /// while its first session opens.
    pub async fn delivery_state(&self, agent: &Agent) -> DeliveryStatus {
        if agent.plugin == EXTERNAL_PLUGIN {
            let queued = self
                .external
                .lock()
                .await
                .get(&agent.id)
                .map_or(0, Vec::len);
            let state = if queued > 0 {
                DeliveryState::Held
            } else {
                DeliveryState::Idle
            };
            return DeliveryStatus { state, queued };
        }
        let inbox = self
            .live
            .lock()
            .await
            .get(&agent.id)
            .filter(|live| !live.inbox.is_closed())
            .map(|live| live.inbox.clone());
        match inbox {
            Some(inbox) => inbox.delivery(),
            None => DeliveryStatus {
                state: if agent.status == AgentStatus::Starting {
                    DeliveryState::Starting
                } else {
                    DeliveryState::Idle
                },
                queued: 0,
            },
        }
    }

    /// Tells clients what the agent's session does with messages now, when that just
    /// changed (docs/tasks/DELIVERY-AUDIT.md §8 P1-4). The agent is read again, so a status
    /// changed just before this call (`stop`) is the one the state is worked out from.
    async fn announce_delivery(&self, id: AgentId) {
        let Ok(agent) = self.store().get_agent(&AgentKey::Id(id)).await else {
            return;
        };
        let delivery = self.delivery_state(&agent).await;
        publish(&self.bus, EventKind::AgentDeliveryChanged { id, delivery }).await;
    }

    /// Makes `session` the agent's live one: its events are published, its inbox served.
    /// `adopted`: the message whose turn was running when the session was taken over.
    async fn install(
        &self,
        id: AgentId,
        session: Arc<dyn DriverSession>,
        channels: Channels,
        diff_dir: Option<PathBuf>,
        adopted: Option<MessageId>,
        steer: bool,
    ) -> Arc<Inbox> {
        let Channels {
            events,
            control_tx,
            control_rx,
            generation,
        } = channels;
        let inbox = Arc::new(Inbox::default());
        let live = Arc::new(Live {
            generation,
            session: session.clone(),
            inbox: inbox.clone(),
            control: control_tx.clone(),
            attachments: session.attachment_support(),
            steer,
        });
        // The stream rules of the agent's workspace, read as its session starts.
        let rules = match self.store().get_agent(&AgentKey::Id(id)).await {
            Ok(agent) => StreamRules::new(stream_rules::load(&agent.cwd, self.home.as_deref())),
            Err(_) => StreamRules::default(),
        };
        tokio::spawn(forward_events(
            self.bus.clone(),
            id,
            events,
            control_rx,
            self.live.clone(),
            generation,
            rules,
        ));
        if adopted.is_some() {
            inbox.busy.store(true, Ordering::SeqCst);
        }
        tokio::spawn(run_turns(
            session,
            inbox.clone(),
            control_tx,
            diff_dir,
            adopted,
        ));
        self.live.lock().await.insert(id, live);
        inbox
    }

    /// Delivers a message, starting the agent if needed.
    pub async fn send(
        &self,
        key: &AgentKey,
        text: String,
        from: MessageSource,
    ) -> Result<Delivery, CoreError> {
        self.deliver(key, Envelope::new(from, text), false, None, None)
            .await
    }

    /// Delivers a prepared message (its id fixed by the sender, possibly a reply).
    pub async fn send_envelope(
        &self,
        key: &AgentKey,
        envelope: Envelope,
    ) -> Result<Delivery, CoreError> {
        self.note_send(key, &envelope).await;
        self.deliver(key, envelope, false, None, None).await
    }

    /// Like [`Self::send_envelope`], with the sender's own id for the message: a repeat of
    /// that id gets the first delivery instead of another turn.
    pub async fn send_envelope_from(
        &self,
        key: &AgentKey,
        envelope: Envelope,
        client_message_id: Option<MessageId>,
    ) -> Result<Delivery, CoreError> {
        self.note_send(key, &envelope).await;
        self.deliver(key, envelope, false, client_message_id, None)
            .await
    }

    /// Like [`Self::send_envelope`], ahead of everything waiting, interrupting the running
    /// turn.
    pub async fn send_now_envelope(
        &self,
        key: &AgentKey,
        envelope: Envelope,
    ) -> Result<Delivery, CoreError> {
        self.note_send(key, &envelope).await;
        self.deliver(key, envelope, true, None, None).await
    }

    /// [`Self::send_now_envelope`], with the sender's own id: a repeat gets the first
    /// delivery instead of being put ahead a second time.
    pub async fn send_now_envelope_from(
        &self,
        key: &AgentKey,
        envelope: Envelope,
        client_message_id: Option<MessageId>,
    ) -> Result<Delivery, CoreError> {
        self.note_send(key, &envelope).await;
        self.deliver(key, envelope, true, client_message_id, None)
            .await
    }

    /// Hands a message to the agent's running turn when its driver can steer, else queues
    /// it for the next one. A steer that cannot go in because the driver has no steer, or
    /// because no turn runs, is queued with that reason; a write that failed is a visible
    /// refusal and is not queued (docs/tasks/DELIVERY-AUDIT.md §3).
    pub async fn steer_envelope(
        &self,
        key: &AgentKey,
        envelope: Envelope,
    ) -> Result<Delivery, SteerFailure> {
        self.steer_envelope_from(key, envelope, None).await
    }

    /// [`Self::steer_envelope`], with the sender's own id for the message: a repeat of
    /// that id gets the first delivery instead of steering again.
    pub async fn steer_envelope_from(
        &self,
        key: &AgentKey,
        envelope: Envelope,
        client_message_id: Option<MessageId>,
    ) -> Result<Delivery, SteerFailure> {
        self.note_send(key, &envelope).await;
        let stored = self.store().get_agent(key).await?;
        let claim = self.claim_client(stored.id, client_message_id).await;
        let claimed = match claim {
            ClientClaim::Repeat(delivery) => return Ok(delivery),
            ClientClaim::First(claimed) => claimed,
        };
        let outcome = self.steer_once(key, envelope).await;
        // A steer that went in or was queued is this client id's answer from now on; a
        // refused one is not, so the same id can try again.
        self.settle_client(stored.id, claimed, outcome.as_ref().ok())
            .await;
        outcome
    }

    async fn steer_once(
        &self,
        key: &AgentKey,
        envelope: Envelope,
    ) -> Result<Delivery, SteerFailure> {
        let message_id = envelope.id;
        let stored = self.store().get_agent(key).await?;
        let live = if stored.archived_at.is_none() && stored.plugin != EXTERNAL_PLUGIN {
            self.live.lock().await.get(&stored.id).cloned()
        } else {
            None
        };
        let live = live.filter(|live| !live.inbox.is_closed());
        let Some(live) = live else {
            // Archived, or an agent outside the hub: `deliver` says what happens to it.
            return self
                .deliver(key, envelope, false, None, None)
                .await
                .map_err(Into::into);
        };
        let running = live.inbox.is_running();
        if !running || !live.steer {
            // Nothing to steer into: the message is the next turn. The reason says which
            // of the two it was.
            let reason = if running {
                QueuedReason::RuntimeNoSteer
            } else {
                QueuedReason::NoActiveTurn
            };
            return self
                .deliver(key, envelope, false, None, Some(reason))
                .await
                .map_err(Into::into);
        }
        // What changed since the last message (model, effort, name) goes with the steered
        // text, but the bubble shows what the person wrote, not the notes. The notes are
        // only consumed if the message really goes in: a queued copy below gets them from
        // `deliver`.
        let (display, sent, consumed) = {
            let notices = self.notices.lock().await;
            match notices.get(&stored.id).filter(|notes| !notes.is_empty()) {
                Some(notes) => {
                    let lines: Vec<&str> = notes.values().map(String::as_str).collect();
                    (
                        envelope.text.clone(),
                        format!("{}\n\n{}", lines.join("\n"), envelope.text),
                        notes.clone(),
                    )
                }
                None => (
                    envelope.text.clone(),
                    envelope.text.clone(),
                    BTreeMap::new(),
                ),
            }
        };
        let prompt = Prompt {
            text: sent,
            attachments: envelope.attachments.clone(),
        };
        match live.session.steer(message_id, &prompt).await {
            Ok(()) => {
                if !consumed.is_empty() {
                    let mut notices = self.notices.lock().await;
                    if let Some(notes) = notices.get_mut(&stored.id) {
                        for topic in consumed.keys() {
                            notes.remove(topic);
                        }
                        if notes.is_empty() {
                            notices.remove(&stored.id);
                        }
                    }
                }
                publish(
                    &self.bus,
                    EventKind::AgentSteered {
                        id: stored.id,
                        message_id,
                        text: display,
                        attachments: envelope.attachments,
                    },
                )
                .await;
                Ok(Delivery {
                    message_id,
                    queued: false,
                    queued_reason: None,
                    steered: true,
                })
            }
            Err(err) => {
                match &err {
                    // The driver cannot steer at all, or the turn ended before the write:
                    // the message is clearly not in yet, so it goes to the next turn with
                    // the reason said out loud.
                    CoreError::Unsupported(_) | CoreError::Conflict(_) => {
                        let reason = if matches!(err, CoreError::Unsupported(_)) {
                            QueuedReason::RuntimeNoSteer
                        } else {
                            QueuedReason::NoActiveTurn
                        };
                        tracing::debug!(%message_id, %err, ?reason, "cannot steer; queueing the message");
                        self.deliver(key, envelope, false, None, Some(reason))
                            .await
                            .map_err(Into::into)
                    }
                    // The write failed: nobody can tell whether the CLI saw it, so it is
                    // neither steered nor queued. The sender decides whether to send it
                    // again, and sees the failure.
                    _ => {
                        tracing::warn!(%message_id, %err, "cannot write a steered message");
                        publish(
                            &self.bus,
                            EventKind::AgentInputRefused {
                                id: stored.id,
                                message_id,
                                text: envelope.text,
                                reason: QueuedReason::SteerWriteFailed,
                            },
                        )
                        .await;
                        Err(SteerFailure::Refused {
                            message_id,
                            reason: QueuedReason::SteerWriteFailed,
                            error: err.to_string(),
                        })
                    }
                }
            }
        }
    }

    /// The delivery a repeat of `client` already got, or the first claim on that id.
    async fn claim_client(&self, agent: AgentId, client: Option<MessageId>) -> ClientClaim {
        let Some(client) = client else {
            return ClientClaim::First(None);
        };
        loop {
            let mut clients = self.clients.lock().await;
            match clients.get(agent, client) {
                Some(ClientEntry::Done(delivery)) => return ClientClaim::Repeat(delivery.clone()),
                Some(ClientEntry::Pending(wait)) => {
                    let wait = wait.clone();
                    drop(clients);
                    // The first send is still in flight: answer with what it found, not
                    // with a second delivery.
                    wait.notified().await;
                }
                None => {
                    let wait = Arc::new(Notify::new());
                    clients.pending(agent, client, wait.clone());
                    return ClientClaim::First(Some((client, wait)));
                }
            }
        }
    }

    /// Keeps a client id's delivery for repeats, or frees the id when nothing was delivered.
    async fn settle_client(
        &self,
        agent: AgentId,
        claimed: Option<(MessageId, Arc<Notify>)>,
        delivery: Option<&Delivery>,
    ) {
        let Some((client, wait)) = claimed else {
            return;
        };
        let mut clients = self.clients.lock().await;
        clients.settle(agent, client, delivery.cloned());
        drop(clients);
        wait.notify_one();
    }

    /// Adds a line to the agent's notification digest: one waiting message that gathers
    /// news while the agent is busy, so notices never pile up as separate turns. A line on
    /// a `topic` already in the digest replaces it. Starts the agent if needed.
    pub async fn notify(&self, key: &AgentKey, topic: &str, line: &str) -> Result<(), CoreError> {
        let stored = self.store().get_agent(key).await?;
        if stored.plugin == EXTERNAL_PLUGIN {
            let envelope = Envelope::new(MessageSource::Notice, line.to_owned());
            return self
                .deliver(key, envelope, false, None, None)
                .await
                .map(|_| ());
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
            attachments: Vec::new(),
        };
        digests.insert(agent.id, digest);
        drop(digests);
        self.deliver(&AgentKey::Id(agent.id), envelope, false, None, None)
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
    ) -> Result<Delivery, CoreError> {
        self.deliver(key, Envelope::new(from, text), true, None, None)
            .await
    }

    /// Delivers `envelope`; with a `client_message_id`, a repeat of the same id gets the
    /// first delivery instead of another turn.
    async fn deliver(
        &self,
        key: &AgentKey,
        envelope: Envelope,
        first: bool,
        client_message_id: Option<MessageId>,
        queued_reason: Option<QueuedReason>,
    ) -> Result<Delivery, CoreError> {
        let stored = self.store().get_agent(key).await?;
        let claimed = match self.claim_client(stored.id, client_message_id).await {
            ClientClaim::Repeat(delivery) => return Ok(delivery),
            ClientClaim::First(claimed) => claimed,
        };
        let outcome = self.deliver_once(key, envelope, first, queued_reason).await;
        self.settle_client(stored.id, claimed, outcome.as_ref().ok())
            .await;
        outcome
    }

    async fn deliver_once(
        &self,
        key: &AgentKey,
        envelope: Envelope,
        first: bool,
        queued_reason: Option<QueuedReason>,
    ) -> Result<Delivery, CoreError> {
        let Envelope {
            id: message_id,
            from,
            text,
            reply_to,
            attachments,
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
                    attachments: attachments.clone(),
                    notices: Vec::new(),
                    queued_reason: Some(QueuedReason::ExternalAgent),
                },
            )
            .await;
            let text = self.with_notices(stored.id, text).await;
            self.external
                .lock()
                .await
                .entry(stored.id)
                .or_default()
                .push((
                    message_id,
                    from,
                    Prompt { text, attachments }.with_references(),
                ));
            self.announce_delivery(stored.id).await;
            return Ok(Delivery::new(
                message_id,
                true,
                Some(QueuedReason::ExternalAgent),
            ));
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
        // Deciding whether it waits and putting it in line are one step: two concurrent
        // sends cannot both be told they are next (docs/tasks/DELIVERY-AUDIT.md §7, item 8).
        let admission = live.inbox.admit.lock().await;
        let (queued, queue_reason) = live.inbox.queue_state();
        let queued_reason = queued_reason.or(queue_reason);
        // A person's message takes a notice digest that is still waiting with it: the agent
        // reads the notices right after the person's words, in the same turn, instead of
        // spending a turn of its own on them (docs/TODO.md §二十五 第 3 条).
        let carried = match from {
            MessageSource::Human => self.take_waiting_digest(agent.id, &live.inbox).await,
            _ => None,
        };
        if let Some(digest) = &carried {
            publish(
                &self.bus,
                EventKind::AgentInputWithdrawn {
                    id: agent.id,
                    message_id: digest.message,
                },
            )
            .await;
        }
        let notices: Vec<String> = carried
            .as_ref()
            .map(|digest| digest.lines.values().cloned().collect())
            .unwrap_or_default();
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
                attachments: attachments.clone(),
                notices,
                queued_reason,
            },
        )
        .await;
        // What changed since the last message (model, effort, name) is told before it; the
        // conversation shows it as a note of its own (e.g. `agent.model_changed`).
        let mut text = self.with_notices(agent.id, text).await;
        // The notices go after the person's words, as the agent reads them.
        if let Some(digest) = carried {
            text = format!("{text}\n\n{}", digest.text());
        }
        if from == MessageSource::Human {
            live.inbox.from_person.lock().unwrap().insert(message_id);
        }
        live.inbox
            .push((message_id, Prompt { text, attachments }), first);
        drop(admission);
        // The announcement is what clients show as "busy"/"next"; it reads the inbox, so it
        // says `turn` with the messages behind the running turn, or `starting` for the first.
        self.announce_delivery(agent.id).await;
        if first && queued {
            let _ = live.session.interrupt().await;
        }
        Ok(Delivery::new(message_id, queued, queued_reason))
    }

    /// Moves a message that is still waiting to the front of the line and interrupts the
    /// running turn, so it is the next thing the agent reads. It keeps its id: nothing is
    /// withdrawn and sent again (docs/tasks/DELIVERY-AUDIT.md §2). A message that already
    /// started, or one nobody knows, is a conflict and leaves the queue as it was.
    pub async fn promote(
        &self,
        key: &AgentKey,
        message_id: MessageId,
    ) -> Result<Delivery, CoreError> {
        let agent = self.store().get_agent(key).await?;
        let live = self
            .live
            .lock()
            .await
            .get(&agent.id)
            .cloned()
            .filter(|l| !l.inbox.is_closed());
        let Some(live) = live else {
            return Err(CoreError::Conflict(
                "the message already started, or is unknown".into(),
            ));
        };
        let admission = live.inbox.admit.lock().await;
        let Some(queued) = live.inbox.promote(message_id) else {
            drop(admission);
            return Err(CoreError::Conflict(
                "the message already started, or is unknown".into(),
            ));
        };
        drop(admission);
        if queued {
            let _ = live.session.interrupt().await;
        }
        let reason = queued.then_some(QueuedReason::TurnRunning);
        Ok(Delivery::new(message_id, queued, reason))
    }

    /// Takes the agent's notification digest if it is still waiting for its turn, so a
    /// person's message can carry it; `None` once it started or when there is none.
    ///
    /// The check, the withdraw and the removal happen under one `digests` lock: a concurrent
    /// `notify` takes the same lock first, so it cannot add a line to a digest this call is
    /// taking out (which `inbox.update` would then drop as already withdrawn). Locks go
    /// `digests` first, then `inbox` — the order `notify` uses too (`digests` → `inbox.update`).
    async fn take_waiting_digest(&self, id: AgentId, inbox: &Inbox) -> Option<Digest> {
        let mut digests = self.digests.lock().await;
        let message = digests.get(&id)?.message;
        if !inbox.withdraw(message) {
            return None;
        }
        digests.remove(&id)
    }

    async fn with_notices(&self, agent: AgentId, text: String) -> String {
        match self.notices.lock().await.remove(&agent) {
            Some(notes) => {
                let notes: Vec<String> = notes.into_values().collect();
                format!("{}\n\n{text}", notes.join("\n"))
            }
            None => text,
        }
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
        self.announce_delivery(agent.id).await;
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

    /// The slash commands and skills the agent's own CLI reports for its live session
    /// (Claude Code's `system/init`). `known` is false when nothing has started, or the
    /// driver does not report them, and `note` says which.
    pub async fn commands(
        &self,
        key: &AgentKey,
    ) -> Result<crossagents_api_types::AgentCommandsResponse, CoreError> {
        let agent = self.store().get_agent(key).await?;
        let live = self.live.lock().await.get(&agent.id).cloned();
        let (commands, note) = match live {
            Some(live) => match live.session.commands() {
                Some(commands) => (commands, None),
                None => (Vec::new(), Some("这个驱动不报告自带指令".to_owned())),
            },
            None => (Vec::new(), Some("还没启动过会话".to_owned())),
        };
        Ok(crossagents_api_types::AgentCommandsResponse {
            known: note.is_none(),
            commands,
            note,
        })
    }

    /// Summarises the running session's history to free context.
    pub async fn compact(&self, key: &AgentKey) -> Result<(), CoreError> {
        let agent = self.store().get_agent(key).await?;
        let live = self.live.lock().await.get(&agent.id).cloned();
        let Some(live) = live else {
            return Err(CoreError::Conflict(format!(
                "agent {} is not running",
                agent.name
            )));
        };
        // A long history takes minutes to summarise; a caller that stops waiting (the GUI's
        // request timeout) must not cancel it halfway and leave the session mid-compaction.
        let session = live.session.clone();
        let control = live.control.clone();
        tokio::spawn(async move {
            let result = session.compact().await;
            if result.is_ok() {
                // The stream rules sent before are no longer in the context.
                let _ = control.send(Control::Compacted);
            }
            result
        })
        .await
        .map_err(|err| CoreError::Agent(format!("compaction stopped: {err}")))?
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
        // What it took has started, so nothing waits on the hub any more; a message that
        // arrived meanwhile still counts.
        self.announce_delivery(id).await;
        messages
    }

    /// Keeps `note` for the agent's next message (a newer note on the same topic replaces it).
    pub async fn tell_later(&self, id: AgentId, topic: &str, note: String) {
        self.notices
            .lock()
            .await
            .entry(id)
            .or_default()
            .insert(topic.to_owned(), note);
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
        // Archived or restored now: nothing is left to do once its turn ends.
        self.archiving.lock().await.remove(&root.id);
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
        if !archived && let Some(why) = self.cannot_carry_on(&result).await {
            publish(
                &self.bus,
                EventKind::AgentWarning {
                    id: result.id,
                    message: format!(
                        "对话记录恢复了，但{why}：下一条消息会开新会话，不带原来的上下文"
                    ),
                },
            )
            .await;
        }
        Ok(result)
    }

    /// Why the agent's next session will not carry on its last conversation, when it had
    /// one: its driver resumes no session, or the place it could resume in is gone.
    async fn cannot_carry_on(&self, agent: &Agent) -> Option<String> {
        self.store()
            .last_session_id(agent.id)
            .await
            .ok()
            .flatten()?;
        if let Some(why) = agent.context_lost() {
            return Some(why.to_owned());
        }
        let resumes = self.capabilities(agent).await.is_some_and(|c| c.resume);
        (!resumes).then(|| format!("{} 的驱动不能延续会话", agent.name))
    }

    /// The command that carries the session on in its own CLI, in a terminal
    /// (`GET /v1/agents/{key}/resume-command`, [`crate::resume`]). Runs nothing.
    pub async fn resume_command(
        &self,
        key: &AgentKey,
    ) -> Result<crossagents_api_types::ResumeCommandResponse, CoreError> {
        use crate::resume;
        let agent = self.store().get_agent(key).await?;
        let external = agent.plugin == EXTERNAL_PLUGIN;
        let resolved = if external {
            None
        } else {
            self.plugins
                .driver(&agent.plugin, agent.driver.as_deref())
                .ok()
        };
        let kind = resolved.as_ref().map(|r| r.driver.kind());
        let resumes = self.capabilities(&agent).await.is_some_and(|c| c.resume);
        let session_id = self.store().last_session_id(agent.id).await?;
        let program = match &resolved {
            Some(r) => r
                .driver
                .cli_status()
                .await
                .and_then(|s| s.located)
                .map(|l| l.argv),
            None => None,
        };
        // The account's environment, as the session gets it; `resume::build` keeps only the
        // config directories and proxies of it.
        let mut env = match &self.setup {
            Some(setup) if !external => setup.environment(&agent).await.unwrap_or_default(),
            _ => Vec::new(),
        };
        env.extend(resume::proxy_env(kind));
        let session_file = match (kind, &session_id) {
            (Some(kind), Some(id)) => resume::claude_config_dir(kind, &env)
                .and_then(|dir| resume::find_session_file(&dir, id)),
            _ => None,
        };
        let facts = resume::Facts {
            name: agent.name.clone(),
            external,
            kind,
            resumes,
            session_id,
            context_lost: agent.context_lost().map(str::to_owned),
            cwd_exists: agent.cwd.is_dir(),
            moved_away: agent.archived_at.is_some() && agent.cwd != agent.home(),
            cwd: agent.cwd.clone(),
            program,
            env,
            live: self.is_live(agent.id).await,
            session_file,
        };
        Ok(resume::build(&facts, resume::Shell::here()))
    }

    /// Drops the [`crossagents_core::CONTEXT_LOST`] mark once a new session has opened.
    async fn forget_lost_context(&self, agent: AgentId) {
        let Ok(mut record) = self.store().get_agent(&AgentKey::Id(agent)).await else {
            return;
        };
        if let Some(config) = record.config.as_object_mut() {
            config.remove(crossagents_core::CONTEXT_LOST);
        }
        if let Err(err) = self
            .store()
            .update_agent_config(agent, &record.config)
            .await
        {
            tracing::warn!(%agent, error = %err, "cannot clear the lost-context mark");
        }
    }

    /// Stops the agent's session. Stopping a stopped agent is fine.
    pub async fn stop(&self, key: &AgentKey) -> Result<(), CoreError> {
        let agent = self.store().get_agent(key).await?;
        let lock = self.agent_lock(agent.id).await;
        let _guard = lock.lock().await;
        // Nothing can repeat a send to a session that is gone.
        self.clients.lock().await.forget(agent.id);
        let Some(live) = self.live.lock().await.remove(&agent.id) else {
            // No session: what it does with messages did not change, so nothing is announced.
            set_status(&self.bus, agent.id, AgentStatus::Stopped).await;
            return Ok(());
        };
        self.withdraw_waiting(agent.id, &live).await;
        let _ = live.session.interrupt().await;
        live.session.shutdown().await;
        let _ = live.control.send(Control::Stopped);
        set_status(&self.bus, agent.id, AgentStatus::Stopped).await;
        self.announce_delivery(agent.id).await;
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

/// Publishes what the session's inbox says now, for a session this forwarder owns: what
/// clients show as "busy"/"next" (docs/tasks/DELIVERY-AUDIT.md §8 P1-4).
async fn announce_inbox(bus: &EventBus, id: AgentId, live: &LiveMap, generation: u64) {
    let delivery = live
        .lock()
        .await
        .get(&id)
        .filter(|live| live.generation == generation)
        .map(|live| live.inbox.delivery());
    if let Some(delivery) = delivery {
        publish(bus, EventKind::AgentDeliveryChanged { id, delivery }).await;
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
    adopted: Option<MessageId>,
) {
    // The turn a previous hub started goes on in the process taken over; it ends first.
    if let Some(message_id) = adopted {
        let result = match session.adopted_turn().await {
            Some(result) => result
                .map(|outcome| outcome.stop_reason)
                .map_err(|e| e.to_string()),
            None => Ok(StopReason::Interrupted),
        };
        inbox.finish_turn();
        if control
            .send(Control::TurnFinished(message_id, result, None))
            .is_err()
        {
            return;
        }
    }
    while let Some((message_id, prompt)) = inbox.next().await {
        // Before the turn is announced: an interrupt that follows the announcement must find
        // the prompt already running.
        let base = match &diff_dir {
            Some(dir) => crate::snapshot::take(dir).await,
            None => None,
        };
        let _ = control.send(Control::TurnStarted(message_id));
        let result = session
            .prompt_with(&prompt)
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
    mut rules: StreamRules,
) {
    let mut out = Coalescer::default();
    let mut tick = tokio::time::interval(FLUSH_INTERVAL);
    let mut events_open = true;
    let mut running = None;
    loop {
        tokio::select! {
            event = events.recv(), if events_open => match event {
                Some(event) => {
                    let reason = if matches!(&event.kind, SessionEventKind::Exited { cause: Some(crossagents_core::ExitCause::Killed), .. }) { StopReason::Cancelled } else { StopReason::Error };
                    if handle_session_event(&bus, id, event, &mut out, &live, generation, &mut rules).await {
                        if let Some(message_id) = running.take() {
                            publish(&bus, EventKind::AgentTurnCompleted { id, message_id, stop_reason: reason }).await;
                        }
                        return;
                    }
                }
                None => events_open = false,
            },
            ctl = control.recv() => {
                // Everything the session emitted before this point is already queued.
                while let Ok(event) = events.try_recv() {
                    let reason = if matches!(&event.kind, SessionEventKind::Exited { cause: Some(crossagents_core::ExitCause::Killed), .. }) { StopReason::Cancelled } else { StopReason::Error };
                    if handle_session_event(&bus, id, event, &mut out, &live, generation, &mut rules).await {
                        if let Some(message_id) = running.take() {
                            publish(&bus, EventKind::AgentTurnCompleted { id, message_id, stop_reason: reason }).await;
                        }
                        return;
                    }
                }
                out.flush(&bus, id).await;
                match ctl {
                    Some(Control::TurnStarted(message_id)) => {
                        running = Some(message_id);
                        rules.turn_started();
                        set_status(&bus, id, AgentStatus::Running).await;
                        publish(&bus, EventKind::AgentTurnStarted { id, message_id }).await;
                        announce_inbox(&bus, id, &live, generation).await;
                    }
                    Some(Control::Compacted) => rules.compacted(),
                    Some(Control::TurnFinished(message_id, result, diff)) => {
                        if running == Some(message_id) {
                            running = None;
                        }
                        rules.turn_finished();
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
                        announce_inbox(&bus, id, &live, generation).await;
                    }
                    Some(Control::Stopped) | None => {
                        if let Some(message_id) = running.take() {
                            publish(&bus, EventKind::AgentTurnCompleted { id, message_id, stop_reason: StopReason::Cancelled }).await;
                        }
                        return;
                    }
                    Some(Control::Detached) => return,
                }
            },
            _ = tick.tick() => out.flush(&bus, id).await,
        }
    }
}

/// Returns `true` when the session is gone and forwarding should end.
async fn handle_session_event(
    bus: &Arc<EventBus>,
    id: AgentId,
    event: SessionEvent,
    out: &mut Coalescer,
    live: &LiveMap,
    generation: u64,
    rules: &mut StreamRules,
) -> bool {
    let SessionEvent { kind: event, .. } = event;
    match event {
        SessionEventKind::Output { stream, text } => {
            let injections = rules.on_output(&text);
            out.push(bus, id, Stream::Output(stream), text).await;
            if !injections.is_empty() {
                // The output that matched goes out before the rule that answers it.
                out.flush(bus, id).await;
                // Handing the rule in may wait on the driver (a steer is a request, and an
                // interrupt is not always instant), so it runs beside the pump: output keeps
                // flowing while it does (docs/TODO.md §二十五「4 的尾巴」).
                tokio::spawn(inject(bus.clone(), id, live.clone(), generation, injections));
            }
            return false;
        }
        SessionEventKind::ToolOutput { tool_call_id, text } => {
            out.push(bus, id, Stream::Tool(tool_call_id), text).await;
            return false;
        }
        _ => {}
    }
    out.flush(bus, id).await;
    // What a tool call says, for the stream rules: its name and kind, then its arguments.
    let tool_text = match &event {
        SessionEventKind::ToolCall { title, kind, .. } => {
            Some(format!("{} {title}", kind.as_deref().unwrap_or_default()))
        }
        SessionEventKind::ToolDetail { detail, .. } => Some(format!(
            "{} {}",
            detail.command.as_deref().unwrap_or_default(),
            detail.arguments.as_deref().unwrap_or_default()
        )),
        _ => None,
    };
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
        SessionEventKind::ContextBreakdown { parts } => EventKind::AgentContext { id, parts },
        SessionEventKind::PermissionRequested { request, decision } => EventKind::AgentPermission {
            id,
            kind: request.kind,
            title: request.title,
            decision,
        },
        SessionEventKind::Warning { message } => EventKind::AgentWarning { id, message },
        SessionEventKind::Error { message } => EventKind::AgentError { id, message },
        SessionEventKind::SteerRead { message_id } => EventKind::AgentSteerRead { id, message_id },
        // The agent compacted its own context: what the rules said is no longer in it, so
        // they may be sent again (docs/tasks/STREAM-RULES.md).
        SessionEventKind::Compacted => {
            rules.compacted();
            return false;
        }
        // Turn boundaries come from the runtime's own control messages.
        SessionEventKind::TurnStarted { .. } | SessionEventKind::TurnCompleted { .. } => {
            return false;
        }
        SessionEventKind::Exited {
            message,
            cause,
            code,
        } => {
            // The process died on its own: forget the session if it is still ours, and take
            // back what was waiting for it (it would never run).
            let mut map = live.lock().await;
            let gone = match map.get(&id) {
                Some(l) if l.generation == generation => map.remove(&id),
                _ => None,
            };
            drop(map);
            if let Some(gone) = gone {
                let waiting: Vec<MessageId> = gone
                    .inbox
                    .waiting
                    .lock()
                    .unwrap()
                    .drain(..)
                    .map(|(m, _)| m)
                    .collect();
                for message_id in waiting {
                    publish(bus, EventKind::AgentInputWithdrawn { id, message_id }).await;
                }
            }
            publish(
                bus,
                EventKind::AgentExited {
                    id,
                    cause,
                    code,
                    message: message.clone(),
                },
            )
            .await;
            publish(bus, EventKind::AgentError { id, message }).await;
            set_status(bus, id, AgentStatus::Error).await;
            return true;
        }
    };
    publish(bus, kind).await;
    if let Some(text) = tool_text {
        let injections = rules.on_tool(&text);
        if !injections.is_empty() {
            tokio::spawn(inject(bus.clone(), id, live.clone(), generation, injections));
        }
    }
    false
}

/// Sends stream rules into the session's running turn: steered when the driver can take
/// them in; otherwise the turn is interrupted and the rules run next, ahead of what waits.
/// Only the session that matched is served; a session already replaced gets nothing.
///
/// Runs as its own task: the driver's steer and interrupt are calls that can take seconds
/// (an ACP interrupt may wait on the agent's process), and the event pump must not wait for
/// them.
async fn inject(
    bus: Arc<EventBus>,
    id: AgentId,
    live: LiveMap,
    generation: u64,
    injections: Vec<Injection>,
) {
    if injections.is_empty() {
        return;
    }
    let Some(live) = live
        .lock()
        .await
        .get(&id)
        .filter(|l| l.generation == generation)
        .cloned()
    else {
        return;
    };
    if live.inbox.is_closed() {
        return;
    }
    let mut queued = Vec::new();
    for injection in injections {
        let message_id = MessageId::new();
        if live.steer && live.inbox.is_running() {
            let prompt = Prompt {
                text: injection.text.clone(),
                attachments: Vec::new(),
            };
            if live.session.steer(message_id, &prompt).await.is_ok() {
                tracing::debug!(rule = %injection.rule, "stream rule steered into the turn");
                publish(
                    &bus,
                    EventKind::AgentSteered {
                        id,
                        message_id,
                        text: injection.text,
                        attachments: Vec::new(),
                    },
                )
                .await;
                continue;
            }
        }
        queued.push(injection);
    }
    if queued.is_empty() {
        return;
    }
    // The driver cannot take them into the turn: interrupt it, and run the rules next.
    let text = queued
        .iter()
        .map(|injection| injection.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let message_id = MessageId::new();
    if live.inbox.is_running() {
        let _ = live.session.interrupt().await;
    }
    live.inbox.push(
        (
            message_id,
            Prompt {
                text: text.clone(),
                attachments: Vec::new(),
            },
        ),
        true,
    );
    tracing::debug!(
        rules = queued.len(),
        "stream rules interrupt the turn and run next"
    );
    publish(
        &bus,
        EventKind::AgentInput {
            id,
            message_id,
            text,
            queued: true,
            from: Some(MessageSource::Notice),
            reply_to: None,
            thread: None,
            reply_excerpt: None,
            attachments: Vec::new(),
            notices: Vec::new(),
            queued_reason: Some(QueuedReason::TurnRunning),
        },
    )
    .await;
    // What waits changed: say the new state with it.
    publish(
        &bus,
        EventKind::AgentDeliveryChanged {
            id,
            delivery: live.inbox.delivery(),
        },
    )
    .await;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn delivery(id: MessageId) -> Delivery {
        Delivery::new(id, false, None)
    }

    /// Past the cap the oldest kept delivery goes, the newest stays, and a repeat of an id
    /// still inside the window is still answered with its delivery.
    #[test]
    fn client_history_drops_the_oldest_and_keeps_repeats() {
        let mut log = ClientLog::default();
        let agent = AgentId::new();
        let ids: Vec<MessageId> = (0..CLIENT_HISTORY + 8).map(|_| MessageId::new()).collect();
        for id in &ids {
            log.settle(agent, *id, Some(delivery(*id)));
        }
        assert!(log.get(agent, ids[0]).is_none(), "the oldest went");
        assert!(log.get(agent, ids[7]).is_none(), "up to the excess went");
        assert!(log.get(agent, ids[8]).is_some(), "the window starts here");
        assert!(
            matches!(log.get(agent, ids[CLIENT_HISTORY + 7]), Some(ClientEntry::Done(d)) if d.message_id == ids[CLIENT_HISTORY + 7]),
            "the newest is kept"
        );
        // A pending send is not dropped, even while finished ones overflow.
        let pending = MessageId::new();
        log.pending(agent, pending, Arc::new(Notify::new()));
        for _ in 0..8 {
            let id = MessageId::new();
            log.settle(agent, id, Some(delivery(id)));
        }
        assert!(matches!(
            log.get(agent, pending),
            Some(ClientEntry::Pending(_))
        ));
        // Another agent's history is untouched.
        let other = AgentId::new();
        log.settle(other, ids[0], Some(delivery(ids[0])));
        assert!(log.get(other, ids[0]).is_some());
        // Forgetting an agent clears just its records.
        log.forget(agent);
        assert!(log.get(agent, pending).is_none());
        assert!(log.get(other, ids[0]).is_some());
    }
}
