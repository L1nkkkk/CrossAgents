//! Live agent sessions: start, deliver messages one turn at a time, cancel, stop, and turn
//! what happens inside a session into events.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use crossagents_core::{
    Agent, AgentId, AgentKey, AgentStatus, AttachmentSupport, Capabilities, CarriedNotice,
    CoreError, DeliveryState, DeliveryStatus, DiffSource, DriverSession, EXTERNAL_PLUGIN, Envelope,
    EventBus, EventKind, McpServerSpec, MessageId, MessageSource, NameSource, NoticeClass,
    OutputStream, PendingNotice, PermissionPolicy, Prompt, QueuedReason, SessionEvent,
    SessionEventKind, SessionSpec, StopReason, Store, validate_agent_name,
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
    /// The write timed out: it may or may not have reached the CLI. Nothing is queued and
    /// it is not sent again; the sender is told the outcome is unknown
    /// (docs/tasks/DELIVERY-AUDIT.md §3, §4).
    Unknown {
        message_id: MessageId,
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
    /// The write may have reached the agent: a repeat must not write it again.
    Unknown(MessageId),
}

/// How many finished deliveries one agent keeps by client id. A hub runs for weeks, so the
/// ids it will never see again are dropped oldest first instead of piling up; a send still
/// in flight (`Pending`) is never dropped (docs/tasks/DELIVERY-AUDIT.md §4).
const CLIENT_HISTORY: usize = 256;

/// The answer to a repeat of a message whose earlier write timed out: it may already be in,
/// so writing it again could make the model read it twice.
const UNKNOWN_REPEAT: &str = "the earlier attempt may or may not have reached the agent; \
                              it was not sent again";

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

    /// Keeps what the first send on an id turned into (dropping this agent's oldest kept one
    /// once they pass [`CLIENT_HISTORY`]), or frees the id when nothing was delivered.
    fn settle(&mut self, agent: AgentId, client: MessageId, settled: Settled<'_>) {
        match settled {
            Settled::Delivery(delivery) => {
                self.keep(agent, client, ClientEntry::Done(delivery.clone()));
            }
            Settled::Unknown(message_id) => {
                self.keep(agent, client, ClientEntry::Unknown(message_id));
            }
            Settled::Nothing => {
                if let Some(entries) = self.agents.get_mut(&agent) {
                    entries.entries.remove(&client);
                }
            }
        }
    }

    fn keep(&mut self, agent: AgentId, client: MessageId, entry: ClientEntry) {
        let entries = self.agents.entry(agent).or_default();
        entries.entries.insert(client, entry);
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
    /// This id's earlier write may have reached the agent: answer "unknown" again and do
    /// not write it twice.
    RepeatUnknown(MessageId),
    /// The first send on this id: deliver, then [`Runtime::settle_client`] it.
    First(Option<(MessageId, Arc<Notify>)>),
}

/// What a first send on a client id settled to, for repeats.
enum Settled<'a> {
    Delivery(&'a Delivery),
    /// The write may or may not have arrived; the id must not send it again.
    Unknown(MessageId),
    /// Refused before delivery: the same id may try again.
    Nothing,
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

    fn is_waiting(&self, id: MessageId) -> bool {
        self.waiting.lock().unwrap().iter().any(|(m, _)| *m == id)
    }

    /// Pops the next message and marks a turn running, under the queue's lock: a closing
    /// [`Self::close_if_idle`] sees the message still queued, or the turn running, never
    /// neither. `None` when closed or nothing is waiting.
    fn pop_turn(&self) -> Option<(MessageId, Prompt)> {
        let mut waiting = self.waiting.lock().unwrap();
        if self.closed.load(Ordering::SeqCst) {
            return None;
        }
        let message = waiting.pop_front()?;
        self.busy.store(true, Ordering::SeqCst);
        *self.running.lock().unwrap() = Some(message.0);
        Some(message)
    }

    /// Closes the queue unless a turn runs in it, decided under the queue's lock (see
    /// [`Self::pop_turn`]). What waits stays queued, for the session that takes it over.
    /// `false` when a turn runs: nothing is closed then.
    fn close_if_idle(&self) -> bool {
        let _waiting = self.waiting.lock().unwrap();
        if self.busy.load(Ordering::SeqCst) {
            return false;
        }
        self.closed.store(true, Ordering::SeqCst);
        self.notify.notify_one();
        true
    }

    async fn wait_for_message(&self) {
        self.notify.notified().await;
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }

    fn has_waiting(&self) -> bool {
        !self.waiting.lock().unwrap().is_empty()
    }

    /// Takes over what `other` holds waiting, behind what this queue already holds.
    fn take_from(&self, other: &Inbox) {
        let moved: Vec<(MessageId, Prompt)> = other.waiting.lock().unwrap().drain(..).collect();
        let people: Vec<MessageId> = other.from_person.lock().unwrap().drain().collect();
        self.waiting.lock().unwrap().extend(moved);
        self.from_person.lock().unwrap().extend(people);
        self.notify.notify_one();
    }
}

/// A message waiting for an agent outside the hub: id, sender, text.
pub type Waiting = (MessageId, MessageSource, String);

type LiveMap = Arc<Mutex<HashMap<AgentId, Arc<Live>>>>;

/// The agent config key that says its next session starts fresh ([`Runtime::renew`]). Unlike
/// [`crossagents_core::CONTEXT_LOST`] it is not a warning: starting a round is not a loss.
const NEW_SESSION: &str = "new_session";

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
    /// Pending notices: what is recorded, what is queued as a wake, and what a turn is reading.
    notice_state: Arc<NoticeState>,
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
    /// The queues of agents whose session was renewed ([`Runtime::renew`]): their waiting
    /// messages go on into the next session, which takes them over in [`Runtime::install`].
    carried: Mutex<HashMap<AgentId, Arc<Inbox>>>,
    /// Answers to teammates that were only written, to forward (crate::replies).
    replies: std::sync::Mutex<crate::replies::ReplyTracker>,
    /// Each fixed client id's first delivery, by agent: a repeat gets it instead of a new
    /// turn (docs/tasks/DELIVERY-AUDIT.md §4).
    clients: Mutex<ClientLog>,
    generation: AtomicU64,
    /// Where each session's CLI version is recorded as working or broken.
    cli_runs: Option<Arc<crate::cli_runs::CliRuns>>,
}

/// Messages to an agent's event forwarder, kept in order with the session's own events.
enum Control {
    TurnStarted(MessageId),
    /// The turn's outcome, and what it changed when the hub works that out itself; the
    /// forwarder has published all of it once it answers.
    ///
    /// The next turn waits for that answer: its prompt may already be on its way to the
    /// session, and the forwarder must not still be reading the last turn's output then
    /// (it would file it under the new turn).
    TurnFinished(
        MessageId,
        Result<StopReason, String>,
        Option<String>,
        tokio::sync::oneshot::Sender<()>,
    ),
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
            notice_state: Arc::new(NoticeState::new()),
            notices: Mutex::default(),
            external: Mutex::default(),
            going_home: Mutex::default(),
            archiving: Mutex::default(),
            carried: Mutex::default(),
            replies: std::sync::Mutex::default(),
            clients: Mutex::default(),
            generation: AtomicU64::new(0),
            cli_runs: None,
        }
    }

    /// Records what each session's CLI version did (`crate::cli_runs`).
    pub fn with_cli_runs(mut self, runs: Arc<crate::cli_runs::CliRuns>) -> Self {
        self.cli_runs = Some(runs);
        self
    }

    /// A witness for a session of `agent`, when the runtime records CLI runs and the CLI
    /// it launches reports where it is and its version.
    async fn witness(&self, agent: &Agent) -> Option<crate::cli_runs::Witness> {
        let runs = self.cli_runs.clone()?;
        let resolved = self
            .plugins
            .driver(&agent.plugin, agent.driver.as_deref())
            .ok()?;
        let located = resolved.driver.cli_status().await?.located?;
        let version = located.version?;
        Some(crate::cli_runs::Witness::new(
            runs,
            agent.plugin.clone(),
            located.path.display().to_string(),
            version,
        ))
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

    /// How long a batch of notices waits: `quiet` after the latest, and at most `max` after
    /// the earliest. Defaults are 60s and 180s. Zero wakes on the next check.
    pub fn with_notice_windows(self, quiet: Duration, max: Duration) -> Self {
        self.notice_state
            .quiet_ms
            .store(quiet.as_millis() as u64, Ordering::Relaxed);
        self.notice_state
            .max_ms
            .store(max.as_millis() as u64, Ordering::Relaxed);
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
        // A wake that never started is rebuilt from the table. A turn this hub takes over
        // keeps its notices, so poll does not open a second one.
        let keep: Vec<MessageId> = unfinished
            .values()
            .filter_map(|work| work.running)
            .collect();
        if let Err(err) = self.store().release_stranded_notices(&keep).await {
            tracing::warn!(error = %err, "cannot release notices the previous hub left");
        }
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
                if let Err(err) = self.store().settle_carried_notices(message_id, false).await {
                    tracing::warn!(%message_id, error = %err, "cannot release notices from an interrupted turn");
                }
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
                        from,
                        ..
                    } => {
                        // A notice wake is rebuilt from the pending table, which survives
                        // restart. Requeueing the message as well would deliver it twice.
                        if from == Some(MessageSource::Notice) {
                            continue;
                        }
                        out.entry(id)
                            .or_default()
                            .waiting
                            .push((message_id, Prompt { text, attachments }));
                    }
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
        crate::builtin_node::check_node(agent)?;
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
        self.withdraw_queue(id, &live.inbox).await;
    }

    /// Takes back what waits in `queue`, the way [`Self::withdraw_waiting`] does.
    async fn withdraw_queue(&self, id: AgentId, queue: &Inbox) {
        let waiting: Vec<MessageId> = queue
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

    /// Takes back the queue carried to a renewed session that never opened: a stopped,
    /// archived or deleted agent does not get its messages through a session it lost.
    async fn withdraw_carried(&self, id: AgentId) {
        let Some(queue) = self.carried.lock().await.remove(&id) else {
            return;
        };
        queue.close();
        self.withdraw_queue(id, &queue).await;
    }

    /// Whether messages wait in the queue carried to a renewed session that has not opened.
    async fn carried_waiting(&self, id: AgentId) -> bool {
        self.carried
            .lock()
            .await
            .get(&id)
            .is_some_and(|queue| queue.has_waiting())
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
        crate::builtin_node::check_node(agent)?;
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
        // A renewed session (a new round's lead) starts fresh without a warning: it is the
        // normal way a round starts, and the handoff in its kickoff carries the facts.
        let renewed = agent.config.get(NEW_SESSION).is_some();
        let previous = if resolved.driver.capabilities().resume && lost.is_none() && !renewed {
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
        // Both marks are cleared once the new session is up. A renewed session (a round the lead
        // chose to start) is not a loss of context: its handoff is in the kickoff, so no warning.
        if renewed {
            self.forget_mark(agent.id, NEW_SESSION).await;
        }
        if let Some(why) = lost {
            self.forget_lost_context(agent.id).await;
            if !renewed {
                publish(
                    &self.bus,
                    EventKind::AgentWarning {
                        id: agent.id,
                        message: format!("这是新会话，没有带上之前的上下文：{why}"),
                    },
                )
                .await;
            }
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
        // Messages waiting for a renewed session that has not opened: they go on there first.
        if self.carried_waiting(agent.id).await {
            return Err(CoreError::Busy(format!(
                "messages are waiting for {}",
                agent.name
            )));
        }
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
        let live = self
            .live
            .lock()
            .await
            .get(&id)
            .map(|live| !live.inbox.is_idle());
        match live {
            Some(busy) => busy,
            // Messages carried to a session that has not opened yet are waiting all the same.
            None => self.carried_waiting(id).await,
        }
    }

    /// Whether a message is in line for the agent, asked as one of its turns ends: one
    /// waiting in its inbox (or carried to a session not open yet), or one whose turn has
    /// begun already. The inbox lets the ended turn go before that end is published, so a
    /// running turn here is the next one, started from the line; the turn that ended never
    /// counts.
    pub async fn has_queued(&self, id: AgentId) -> bool {
        let live =
            self.live.lock().await.get(&id).map(|live| {
                live.inbox.has_waiting() || live.inbox.running.lock().unwrap().is_some()
            });
        match live {
            Some(queued) => queued,
            None => self.carried_waiting(id).await,
        }
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
        let live = self
            .live
            .lock()
            .await
            .get(&agent.id)
            .filter(|live| !live.inbox.is_closed())
            .map(|live| live.inbox.clone());
        // A renewed session that has not opened yet still holds what was sent to the agent.
        let inbox = match live {
            Some(inbox) => Some(inbox),
            None => self.carried.lock().await.get(&agent.id).cloned(),
        };
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
        // A renewed session takes over the queue its predecessor closed, with what waits in it.
        let carried = self.carried.lock().await.remove(&id);
        let inbox = match carried {
            Some(inbox) => {
                inbox.closed.store(false, Ordering::SeqCst);
                inbox
            }
            None => Arc::new(Inbox::default()),
        };
        let live = Arc::new(Live {
            generation,
            session: session.clone(),
            inbox: inbox.clone(),
            control: control_tx.clone(),
            attachments: session.attachment_support(),
            steer,
        });
        // The stream rules of the agent's workspace, read as its session starts.
        let (rules, witness) = match self.store().get_agent(&AgentKey::Id(id)).await {
            Ok(agent) => (
                StreamRules::new(stream_rules::load(&agent.cwd, self.home.as_deref())),
                self.witness(&agent).await,
            ),
            Err(_) => (StreamRules::default(), None),
        };
        tokio::spawn(forward_events(
            self.bus.clone(),
            id,
            events,
            control_rx,
            self.live.clone(),
            generation,
            rules,
            witness,
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
            self.notice_state.clone(),
            self.store().clone(),
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
            ClientClaim::RepeatUnknown(message_id) => {
                return Err(SteerFailure::Unknown {
                    message_id,
                    error: UNKNOWN_REPEAT.into(),
                });
            }
            ClientClaim::First(claimed) => claimed,
        };
        let outcome = self.steer_once(key, envelope).await;
        // A steer that went in or was queued is this client id's answer from now on; one
        // whose write timed out is answered "unknown" again, never written twice; a refused
        // one frees the id, so the same id can try again.
        let settled = match &outcome {
            Ok(delivery) => Settled::Delivery(delivery),
            Err(SteerFailure::Unknown { message_id, .. }) => Settled::Unknown(*message_id),
            Err(_) => Settled::Nothing,
        };
        self.settle_client(stored.id, claimed, settled).await;
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
                        from: Some(envelope.from.clone()),
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
                    // No answer came before the timeout: the CLI may or may not have the
                    // message. It is neither queued nor sent again, and the sender is told
                    // the outcome is unknown, not that it failed
                    // (docs/tasks/DELIVERY-AUDIT.md §3, §4).
                    CoreError::OutcomeUnknown(_) => {
                        tracing::warn!(%message_id, %err, "a steered message may not have arrived");
                        publish(
                            &self.bus,
                            EventKind::AgentInputRefused {
                                id: stored.id,
                                message_id,
                                text: envelope.text,
                                reason: QueuedReason::OutcomeUnknown,
                            },
                        )
                        .await;
                        Err(SteerFailure::Unknown {
                            message_id,
                            error: err.to_string(),
                        })
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
                Some(ClientEntry::Unknown(message_id)) => {
                    return ClientClaim::RepeatUnknown(*message_id);
                }
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

    /// Keeps what a client id's first send settled to, or frees the id when nothing was
    /// delivered.
    async fn settle_client(
        &self,
        agent: AgentId,
        claimed: Option<(MessageId, Arc<Notify>)>,
        settled: Settled<'_>,
    ) {
        let Some((client, wait)) = claimed else {
            return;
        };
        let mut clients = self.clients.lock().await;
        clients.settle(agent, client, settled);
        drop(clients);
        wait.notify_one();
    }

    /// Records a notice, then wakes only when its class says to.
    ///
    /// Silent notices wait for the next wake. Batch notices wake together once the quiet
    /// interval after the latest has passed, or the maximum wait after the earliest,
    /// whichever comes first. Unblock, Decide and RunDone wake at once. A wake is one
    /// message: actions first, then silent changes, each line marked with its class. The
    /// same topic replaces the previous line and keeps `first_at`. Rows are deleted only
    /// when the turn that carried them ends successfully. An archived agent keeps the row
    /// and is not delivered to.
    pub async fn notify(
        &self,
        key: &AgentKey,
        topic: &str,
        line: &str,
        class: NoticeClass,
    ) -> Result<(), CoreError> {
        let stored = self.store().get_agent(key).await?;
        let _gate = self.notice_state.gate.lock().await;
        let now = Utc::now();
        self.store()
            .upsert_pending_notice(&PendingNotice {
                agent: stored.id,
                topic: topic.to_owned(),
                line: line.to_owned(),
                class,
                first_at: now,
                updated_at: now,
                carried_by: None,
                wake_tries: 0,
            })
            .await?;
        publish(
            &self.bus,
            EventKind::NoticeRecorded {
                id: stored.id,
                topic: topic.to_owned(),
                line: line.to_owned(),
                class,
            },
        )
        .await;
        let rows = self.store().list_pending_notices(Some(stored.id)).await?;
        if stored.archived_at.is_some() {
            return Ok(());
        }
        if self.has_waiting_digest(stored.id).await || self.should_wake(stored.id, &rows) {
            self.sync_digest(&stored, &rows).await?;
        }
        Ok(())
    }

    /// Wakes agents whose pending batch window has elapsed, or who still have an immediate
    /// notice. Called on the reminder tick and after a restart.
    pub async fn poll_notice_wakes(&self) -> Result<(), CoreError> {
        let _gate = self.notice_state.gate.lock().await;
        let rows = self.store().list_pending_notices(None).await?;
        let mut by_agent: HashMap<AgentId, Vec<PendingNotice>> = HashMap::new();
        for row in rows {
            by_agent.entry(row.agent).or_default().push(row);
        }
        for (id, rows) in by_agent {
            let Ok(agent) = self.store().get_agent(&AgentKey::Id(id)).await else {
                continue;
            };
            // Archived agents are not delivered to. The rows stay on this agent id, so a
            // later session of the same agent still has them.
            if agent.archived_at.is_some() {
                continue;
            }
            let stale = self.notice_state.digest_message(id).is_some()
                && !self.has_waiting_digest(id).await;
            if !stale && !self.should_wake(id, &rows) {
                continue;
            }
            if let Err(err) = self.sync_digest(&agent, &rows).await {
                tracing::warn!(agent = %agent.name, error = %err, "cannot deliver notices");
            }
        }
        Ok(())
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
            ClientClaim::RepeatUnknown(_) => {
                return Err(CoreError::OutcomeUnknown(UNKNOWN_REPEAT.into()));
            }
            ClientClaim::First(claimed) => claimed,
        };
        let outcome = self.deliver_once(key, envelope, first, queued_reason).await;
        let settled = match &outcome {
            Ok(delivery) => Settled::Delivery(delivery),
            Err(_) => Settled::Nothing,
        };
        self.settle_client(stored.id, claimed, settled).await;
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
            let _human_gate = if from == MessageSource::Human {
                Some(self.notice_state.gate.lock().await)
            } else {
                None
            };
            let carried = if _human_gate.is_some() {
                self.absorb_pending_locked(stored.id, None).await
            } else {
                None
            };
            if let Some(message_id) = carried.as_ref().and_then(|c| c.withdrawn) {
                publish(
                    &self.bus,
                    EventKind::AgentInputWithdrawn {
                        id: stored.id,
                        message_id,
                    },
                )
                .await;
            }
            let notices = carried
                .as_ref()
                .map(|c| c.lines.clone())
                .unwrap_or_default();
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
                    notices,
                    queued_reason: Some(QueuedReason::ExternalAgent),
                },
            )
            .await;
            let mut text = self.with_notices(stored.id, text).await;
            if let Some(carried) = &carried {
                text = format!("{text}\n\n{}", carried.text);
            }
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
            if let Some(carried) = &carried {
                self.mark_carried(stored.id, message_id, &carried.rows)
                    .await;
            }
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
        // Gate before the admission lock: recording a notice takes the gate and then this
        // lock, and the other order deadlocks. A person's message carries pending notices.
        let _human_gate = if from == MessageSource::Human {
            Some(self.notice_state.gate.lock().await)
        } else {
            None
        };
        // Deciding whether it waits and putting it in line are one step: two concurrent
        // sends cannot both be told they are next (docs/tasks/DELIVERY-AUDIT.md §7, item 8).
        let admission = live.inbox.admit.lock().await;
        let carried = if _human_gate.is_some() {
            self.absorb_pending_locked(agent.id, Some(&live.inbox))
                .await
        } else {
            None
        };
        let (queued, queue_reason) = live.inbox.queue_state();
        let queued_reason = queued_reason.or(queue_reason);
        if let Some(message_id) = carried.as_ref().and_then(|c| c.withdrawn) {
            publish(
                &self.bus,
                EventKind::AgentInputWithdrawn {
                    id: agent.id,
                    message_id,
                },
            )
            .await;
        }
        let notices: Vec<String> = carried
            .as_ref()
            .map(|c| c.lines.clone())
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
        if let Some(carried) = &carried {
            text = format!("{text}\n\n{}", carried.text);
        }
        if from == MessageSource::Human {
            live.inbox.from_person.lock().unwrap().insert(message_id);
        }
        live.inbox
            .push((message_id, Prompt { text, attachments }), first);
        if let Some(carried) = &carried {
            self.mark_carried(agent.id, message_id, &carried.rows).await;
        }
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

    /// Pending notices a person's message should carry. Caller holds [`NoticeState::gate`].
    /// Versions already being read in a turn stay there. `None` when there is nothing new.
    async fn absorb_pending_locked(&self, id: AgentId, inbox: Option<&Inbox>) -> Option<Carried> {
        let rows = self
            .store()
            .list_pending_notices(Some(id))
            .await
            .unwrap_or_default();
        let inflight = self.notice_state.inflight_versions(id);
        let withdrawn = self.withdraw_waiting_digest(id, inbox).await;
        // A version already being read stays with that turn. A waiting wake was just
        // withdrawn, so its rows move onto this message.
        let active: Vec<PendingNotice> = rows
            .into_iter()
            .filter(|row| {
                if version_inflight(row, &inflight) {
                    return false;
                }
                match row.carried_by {
                    Some(by) => Some(by) == withdrawn,
                    None => true,
                }
            })
            .collect();
        if active.is_empty() {
            return None;
        }
        let (text, lines) = compose_notices(&active);
        Some(Carried {
            withdrawn,
            text,
            lines,
            rows: active,
        })
    }

    async fn mark_carried(&self, id: AgentId, message: MessageId, rows: &[PendingNotice]) {
        if rows.is_empty() {
            return;
        }
        if let Err(err) = self
            .store()
            .mark_notices_carried(id, &versions_of(rows), message)
            .await
        {
            tracing::warn!(%id, error = %err, "cannot mark notices carried by a message");
            return;
        }
        self.publish_carried(id, message, rows).await;
    }

    /// Tells the event log which notices a message carries from now on (`notice.carried`, read
    /// by round accounting). Published once the store has marked them.
    async fn publish_carried(&self, id: AgentId, message: MessageId, rows: &[PendingNotice]) {
        let notices = rows
            .iter()
            .map(|row| CarriedNotice {
                line: row.line.clone(),
                class: row.class,
                topic: row.topic.clone(),
            })
            .collect();
        publish(
            &self.bus,
            EventKind::NoticeCarried {
                id,
                message_id: message,
                notices,
            },
        )
        .await;
    }

    /// Automatic wakes of one version that failed. After this many, it only rides along.
    const fn notice_auto_limit() -> u32 {
        2
    }

    fn can_start_wake(&self, id: AgentId, row: &PendingNotice) -> bool {
        let inflight = self.notice_state.inflight_versions(id);
        row.carried_by.is_none()
            && row.wake_tries < Self::notice_auto_limit()
            && !version_inflight(row, &inflight)
    }

    fn free_rows(&self, id: AgentId, rows: &[PendingNotice]) -> Vec<PendingNotice> {
        let inflight = self.notice_state.inflight_versions(id);
        rows.iter()
            .filter(|row| row.carried_by.is_none() && !version_inflight(row, &inflight))
            .cloned()
            .collect()
    }

    fn should_wake(&self, id: AgentId, rows: &[PendingNotice]) -> bool {
        let mut immediate = false;
        let mut first: Option<DateTime<Utc>> = None;
        let mut last: Option<DateTime<Utc>> = None;
        for row in rows.iter().filter(|row| self.can_start_wake(id, row)) {
            if row.class.wakes_immediately() {
                immediate = true;
            }
            if row.class == NoticeClass::Batch {
                first = Some(first.map_or(row.first_at, |at| at.min(row.first_at)));
                last = Some(last.map_or(row.updated_at, |at| at.max(row.updated_at)));
            }
        }
        if immediate {
            return true;
        }
        match (first, last) {
            (Some(first), Some(last)) => {
                Utc::now()
                    >= batch_deadline(
                        first,
                        last,
                        self.notice_state.quiet(),
                        self.notice_state.max_wait(),
                    )
            }
            _ => false,
        }
    }

    async fn has_waiting_digest(&self, id: AgentId) -> bool {
        let Some(message) = self.notice_state.digest_message(id) else {
            return false;
        };
        if let Some(live) = self.live.lock().await.get(&id)
            && live.inbox.is_waiting(message)
        {
            return true;
        }
        self.external_has(id, message).await
    }

    /// Rows a waiting wake should show: ones it already carries, plus ones nobody has.
    fn rows_for_digest(
        &self,
        id: AgentId,
        rows: &[PendingNotice],
        message: MessageId,
    ) -> Vec<PendingNotice> {
        let inflight = self.notice_state.inflight_versions(id);
        rows.iter()
            .filter(|row| {
                if version_inflight(row, &inflight) {
                    return false;
                }
                match row.carried_by {
                    Some(by) => by == message,
                    None => true,
                }
            })
            .cloned()
            .collect()
    }

    async fn message_is_waiting(&self, agent: &Agent, message: MessageId) -> bool {
        if agent.plugin == EXTERNAL_PLUGIN {
            return self.external_has(agent.id, message).await;
        }
        self.live
            .lock()
            .await
            .get(&agent.id)
            .is_some_and(|live| live.inbox.is_waiting(message))
    }

    /// The wake left the queue without being read. Its rows stay, and this counts as one
    /// failed automatic wake so a dead session cannot be woken every second.
    async fn release_withdrawn(&self, agent: &Agent, message: MessageId) {
        self.notice_state.clear_digest_if(agent.id, message);
        if let Err(err) = self.store().settle_carried_notices(message, false).await {
            tracing::warn!(agent = %agent.name, error = %err, "cannot release notices from a withdrawn wake");
        }
    }

    /// Writes the pending notices into the waiting wake, or queues a new one when it is time.
    /// Caller holds [`NoticeState::gate`].
    async fn sync_digest(&self, agent: &Agent, rows: &[PendingNotice]) -> Result<(), CoreError> {
        let mut rows = rows.to_vec();
        if let Some(message) = self.notice_state.digest_message(agent.id) {
            let active = self.rows_for_digest(agent.id, &rows, message);
            if active.is_empty() {
                let held = self.notice_state.is_inflight(message)
                    || self.message_is_waiting(agent, message).await;
                if held {
                    return Ok(());
                }
                self.release_withdrawn(agent, message).await;
            } else if self.update_waiting(agent, message, &active).await? {
                return Ok(());
            }
            rows = self.store().list_pending_notices(Some(agent.id)).await?;
        }
        let active = self.free_rows(agent.id, &rows);
        if active.is_empty() || !self.should_wake(agent.id, &active) {
            return Ok(());
        }
        self.queue_wake(agent, &active).await
    }

    async fn update_waiting(
        &self,
        agent: &Agent,
        message: MessageId,
        rows: &[PendingNotice],
    ) -> Result<bool, CoreError> {
        if !self.message_is_waiting(agent, message).await {
            // The turn already has this message. New rows stay free so the caller can
            // queue them on their own; this one is not a withdrawn wake.
            if self.notice_state.is_inflight(message) {
                return Ok(false);
            }
            self.release_withdrawn(agent, message).await;
            return Ok(false);
        }
        let versions: Vec<(String, DateTime<Utc>)> = rows
            .iter()
            .map(|row| (row.topic.clone(), row.updated_at))
            .collect();
        if self.notice_state.digest_versions(agent.id) == versions {
            self.store()
                .mark_notices_carried(agent.id, &versions, message)
                .await?;
            self.publish_carried(agent.id, message, rows).await;
            return Ok(true);
        }
        let (text, _) = compose_notices(rows);
        if agent.plugin == EXTERNAL_PLUGIN {
            let mut queues = self.external.lock().await;
            let Some(queue) = queues.get_mut(&agent.id) else {
                drop(queues);
                self.release_withdrawn(agent, message).await;
                return Ok(false);
            };
            let Some((_, _, body)) = queue.iter_mut().find(|(id, _, _)| *id == message) else {
                drop(queues);
                self.release_withdrawn(agent, message).await;
                return Ok(false);
            };
            *body = text.clone();
            self.notice_state
                .set_digest(agent.id, message, versions.clone());
            drop(queues);
            self.store()
                .mark_notices_carried(agent.id, &versions, message)
                .await?;
            self.publish_carried(agent.id, message, rows).await;
            publish(
                &self.bus,
                EventKind::AgentInputUpdated {
                    id: agent.id,
                    message_id: message,
                    text,
                },
            )
            .await;
            return Ok(true);
        }
        let live = self.live.lock().await.get(&agent.id).cloned();
        let Some(live) = live else {
            self.release_withdrawn(agent, message).await;
            return Ok(false);
        };
        let updated = {
            let _turn = self.notice_state.turn.lock().unwrap();
            if !live.inbox.is_waiting(message) {
                false
            } else {
                live.inbox.update(message, &text);
                self.notice_state
                    .set_digest(agent.id, message, versions.clone());
                true
            }
        };
        if !updated {
            if self.notice_state.is_inflight(message) {
                return Ok(false);
            }
            self.release_withdrawn(agent, message).await;
            return Ok(false);
        }
        self.store()
            .mark_notices_carried(agent.id, &versions, message)
            .await?;
        self.publish_carried(agent.id, message, rows).await;
        publish(
            &self.bus,
            EventKind::AgentInputUpdated {
                id: agent.id,
                message_id: message,
                text,
            },
        )
        .await;
        Ok(true)
    }

    async fn queue_wake(&self, agent: &Agent, rows: &[PendingNotice]) -> Result<(), CoreError> {
        let (text, _) = compose_notices(rows);
        let versions: Vec<(String, DateTime<Utc>)> = rows
            .iter()
            .map(|row| (row.topic.clone(), row.updated_at))
            .collect();
        let message = MessageId::new();
        // Record the digest before the message can be popped, so the turn marks these
        // versions in flight instead of waking them again.
        self.notice_state
            .set_digest(agent.id, message, versions.clone());
        if let Err(err) = self
            .store()
            .mark_notices_carried(agent.id, &versions, message)
            .await
        {
            self.notice_state.clear_digest_if(agent.id, message);
            return Err(err);
        }
        let envelope = Envelope {
            id: message,
            from: MessageSource::Notice,
            text,
            reply_to: None,
            attachments: Vec::new(),
        };
        if let Err(err) = self
            .deliver(&AgentKey::Id(agent.id), envelope, false, None, None)
            .await
        {
            if let Err(release) = self.store().settle_carried_notices(message, false).await {
                tracing::warn!(agent = %agent.name, error = %release, "cannot release notices after a failed wake");
            }
            self.notice_state.clear_digest_if(agent.id, message);
            return Err(err);
        }
        // Published after the delivery, so a wake that failed to deliver leaves no record for
        // round accounting (its notices were released above).
        self.publish_carried(agent.id, message, rows).await;
        Ok(())
    }

    async fn withdraw_waiting_digest(
        &self,
        id: AgentId,
        inbox: Option<&Inbox>,
    ) -> Option<MessageId> {
        let Some(message) = self.notice_state.digest_message(id) else {
            return None;
        };
        if let Some(inbox) = inbox {
            let _turn = self.notice_state.turn.lock().unwrap();
            if inbox.is_waiting(message) && inbox.withdraw(message) {
                self.notice_state.clear_digest_if(id, message);
                return Some(message);
            }
            return None;
        }
        let mut queues = self.external.lock().await;
        let Some(queue) = queues.get_mut(&id) else {
            return None;
        };
        let Some(index) = queue.iter().position(|(m, _, _)| *m == message) else {
            return None;
        };
        queue.remove(index);
        drop(queues);
        self.notice_state.clear_digest_if(id, message);
        Some(message)
    }

    async fn external_has(&self, id: AgentId, message: MessageId) -> bool {
        self.external
            .lock()
            .await
            .get(&id)
            .is_some_and(|queue| queue.iter().any(|(m, _, _)| *m == message))
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
        if let Err(err) = self.store().settle_carried_notices(message_id, false).await {
            tracing::warn!(%message_id, error = %err, "cannot release notices a withdrawn message carried");
        }
        self.notice_state.clear_digest_if(agent.id, message_id);
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
    /// that started and ended). A notice wake included here is consumed, so a restart
    /// does not deliver it again.
    pub async fn take_external(&self, id: AgentId) -> Vec<Waiting> {
        let _gate = self.notice_state.gate.lock().await;
        let messages = self.external.lock().await.remove(&id).unwrap_or_default();
        for (message_id, _, _) in &messages {
            self.notice_state.clear_digest_if(id, *message_id);
            if let Err(err) = self.store().settle_carried_notices(*message_id, true).await {
                tracing::warn!(%id, error = %err, "cannot drop notices an external agent took");
            }
        }
        drop(_gate);
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
        self.forget_mark(agent, crossagents_core::CONTEXT_LOST)
            .await;
    }

    /// Drops one of the agent's session marks (`key` in its config) once a session has opened.
    async fn forget_mark(&self, agent: AgentId, key: &str) {
        let Ok(mut record) = self.store().get_agent(&AgentKey::Id(agent)).await else {
            return;
        };
        if let Some(config) = record.config.as_object_mut() {
            config.remove(key);
        }
        if let Err(err) = self
            .store()
            .update_agent_config(agent, &record.config)
            .await
        {
            tracing::warn!(%agent, key, error = %err, "cannot clear a session mark");
        }
    }

    /// Stops the agent's session. Stopping a stopped agent is fine.
    pub async fn stop(&self, key: &AgentKey) -> Result<(), CoreError> {
        let agent = self.store().get_agent(key).await?;
        let lock = self.agent_lock(agent.id).await;
        let _guard = lock.lock().await;
        // Nothing can repeat a send to a session that is gone.
        self.clients.lock().await.forget(agent.id);
        // A renewed session that never opened goes with it, its waiting messages withdrawn.
        self.withdraw_carried(agent.id).await;
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

    /// Ends the agent's session so that its next message opens a new one: how a new round's
    /// lead starts. The conversation stays in the transcript and the event log, under the old
    /// session id. Busy only while a turn runs: messages waiting in the queue are not left
    /// behind, the queue moves to the new session ([`Self::carried`]), ahead of what is sent
    /// after this call. `false` for an agent that runs outside the hub.
    pub async fn renew(&self, key: &AgentKey) -> Result<bool, CoreError> {
        let agent = self.store().get_agent(key).await?;
        // An agent outside the hub keeps its session. An archived one gets no mark either: its
        // next open would start fresh without a handoff, dropping its context on restore.
        if agent.plugin == EXTERNAL_PLUGIN || agent.archived_at.is_some() {
            return Ok(false);
        }
        let lock = self.agent_lock(agent.id).await;
        let _guard = lock.lock().await;
        let live = self.live.lock().await.get(&agent.id).cloned();
        if let Some(live) = &live {
            // Decided under the queue's lock, before any await: a turn that just began keeps
            // its session, and a turn that just ended cannot take the next message any more.
            if !live.inbox.close_if_idle() {
                return Err(CoreError::Busy(format!(
                    "{} is in the middle of a turn",
                    agent.name
                )));
            }
            // What waits is carried before anything else is written, so that a failing mark
            // below cannot lose it. Nothing is withdrawn: nothing is lost.
            let mut carried = self.carried.lock().await;
            match carried.get(&agent.id) {
                Some(queue) => queue.take_from(&live.inbox),
                None => {
                    carried.insert(agent.id, live.inbox.clone());
                }
            }
        }
        // Written before the old session ends: whatever opens the session next starts it fresh.
        let marked = self.mark_new_session(agent.id).await;
        if let Some(live) = live {
            self.live.lock().await.remove(&agent.id);
            let _ = live.session.interrupt().await;
            live.session.shutdown().await;
            let _ = live.control.send(Control::Stopped);
        }
        set_status(&self.bus, agent.id, AgentStatus::Stopped).await;
        self.announce_delivery(agent.id).await;
        marked?;
        Ok(true)
    }

    /// Records that the agent's next session starts fresh ([`NEW_SESSION`]); the open drops
    /// the mark once it has started.
    async fn mark_new_session(&self, id: AgentId) -> Result<(), CoreError> {
        let mut record = self.store().get_agent(&AgentKey::Id(id)).await?;
        if !record.config.is_object() {
            record.config = serde_json::json!({});
        }
        if let Some(config) = record.config.as_object_mut() {
            config.insert(NEW_SESSION.into(), serde_json::json!(true));
        }
        self.store().update_agent_config(id, &record.config).await
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
    notices: Arc<NoticeState>,
    store: Arc<dyn Store>,
) {
    // The turn a previous hub started goes on in the process taken over; it ends first.
    if let Some(message_id) = adopted {
        let result = match session.adopted_turn().await {
            Some(result) => result
                .map(|outcome| outcome.stop_reason)
                .map_err(|e| e.to_string()),
            None => Ok(StopReason::Interrupted),
        };
        let ended = matches!(&result, Ok(StopReason::EndTurn));
        notices.settle(store.as_ref(), message_id, ended).await;
        inbox.finish_turn();
        if !report_turn(&control, message_id, result, None).await {
            return;
        }
    }
    while let Some((message_id, prompt)) = next_turn(&inbox, &notices).await {
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
        // Deleted only when the turn ends on its own. A failure or interrupt keeps the
        // rows and counts one failed automatic wake.
        let ended = matches!(&result, Ok(StopReason::EndTurn));
        notices.settle(store.as_ref(), message_id, ended).await;
        inbox.finish_turn();
        if !report_turn(&control, message_id, result, diff).await {
            return;
        }
    }
}

/// Hands a finished turn to the forwarder and waits until it has published it; `false`
/// when the forwarder is gone (so is the session).
///
/// The next prompt must not reach the session before this: the forwarder would still be
/// reading the last turn's output, and the transcript would file it under the new turn.
async fn report_turn(
    control: &mpsc::UnboundedSender<Control>,
    message_id: MessageId,
    result: Result<StopReason, String>,
    diff: Option<String>,
) -> bool {
    let (done, published) = tokio::sync::oneshot::channel();
    if control
        .send(Control::TurnFinished(message_id, result, diff, done))
        .is_err()
    {
        return false;
    }
    published.await.is_ok()
}

/// The next inbox message, marking its notice versions in flight in the same critical
/// section as the pop so a flush cannot queue them a second time.
async fn next_turn(inbox: &Inbox, notices: &NoticeState) -> Option<(MessageId, Prompt)> {
    loop {
        enum Step {
            Closed,
            Wait,
            Ready((MessageId, Prompt)),
        }
        let step = {
            let _turn = notices.turn.lock().unwrap();
            if let Some(message) = inbox.pop_turn() {
                notices.mark_inflight(message.0);
                Step::Ready(message)
            } else if inbox.is_closed() {
                Step::Closed
            } else {
                Step::Wait
            }
        };
        match step {
            Step::Closed => return None,
            Step::Ready(message) => return Some(message),
            Step::Wait => inbox.wait_for_message().await,
        }
    }
}

/// Error phrases that specifically indicate a model is unavailable, not a temporary failure.
fn unavailable_model_error(error: &str) -> bool {
    const PHRASES: &[&str] = &[
        "service info not found",
        "model not found",
        "model does not exist",
        "model is not available",
        "model is unavailable",
        "invalid model",
        "unknown model",
        "unsupported model",
    ];
    let lower = error.to_ascii_lowercase();
    if lower
        .split(|c: char| !c.is_ascii_digit())
        .any(|n| n == "429")
        || lower.contains("rate limit")
        || lower.contains("too many requests")
    {
        return false;
    }
    PHRASES.iter().any(|phrase| lower.contains(phrase))
}

async fn learn_model_result(
    bus: &EventBus,
    agent: Option<&Agent>,
    reason: StopReason,
    error: Option<&str>,
) {
    let Some(agent) = agent else {
        return;
    };
    let Some(model) = agent.model.as_deref().filter(|m| !m.is_empty()) else {
        return;
    };
    let account = agent
        .config
        .get("account")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let result = if reason == StopReason::EndTurn {
        bus.store()
            .clear_unavailable_model(&agent.plugin, account, model)
            .await
    } else if reason == StopReason::Error && error.is_some_and(unavailable_model_error) {
        bus.store()
            .record_unavailable_model(&agent.plugin, account, model, error.unwrap())
            .await
    } else {
        return;
    };
    if let Err(err) = result {
        tracing::warn!(error = %err, "cannot update model availability");
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
    mut witness: Option<crate::cli_runs::Witness>,
) {
    let mut out = Coalescer::default();
    let mut tick = tokio::time::interval(FLUSH_INTERVAL);
    let mut events_open = true;
    let mut running = None;
    let mut turn_agent = bus.store().get_agent(&AgentKey::Id(id)).await.ok();
    let mut turn_error: Option<String> = None;
    loop {
        tokio::select! {
            // Not while a control waits: the prompt of a turn being announced may already
            // be out, and its output must be read after the announcement, not before.
            event = events.recv(), if events_open && control.is_empty() => match event {
                Some(event) => {
                    if let Some(witness) = witness.as_mut() { witness.observe(&event); }
                    if let SessionEventKind::Error { message } | SessionEventKind::Exited { message, .. } = &event.kind && unavailable_model_error(message) { turn_error = Some(message.clone()); }
                    let reason = if matches!(&event.kind, SessionEventKind::Exited { cause: Some(crossagents_core::ExitCause::Killed), .. }) { StopReason::Cancelled } else { StopReason::Error };
                    if handle_session_event(&bus, id, event, &mut out, &live, generation, &mut rules).await {
                        if let Some(message_id) = running.take() {
                            learn_model_result(&bus, turn_agent.as_ref(), reason, turn_error.as_deref()).await;
                            publish(&bus, EventKind::AgentTurnCompleted { id, message_id, stop_reason: reason }).await;
                        }
                        return;
                    }
                }
                None => events_open = false,
            },
            ctl = control.recv() => {
                // Everything the session emitted before this point is already queued. Not
                // so for a turn being announced: its prompt is out already, so what waits
                // here may be its own output, and reading it now would file it under the
                // turn before.
                if !matches!(ctl, Some(Control::TurnStarted(..))) {
                    while let Ok(event) = events.try_recv() {
                        if let Some(witness) = witness.as_mut() { witness.observe(&event); }
                        if let SessionEventKind::Error { message } | SessionEventKind::Exited { message, .. } = &event.kind && unavailable_model_error(message) { turn_error = Some(message.clone()); }
                        let reason = if matches!(&event.kind, SessionEventKind::Exited { cause: Some(crossagents_core::ExitCause::Killed), .. }) { StopReason::Cancelled } else { StopReason::Error };
                        if handle_session_event(&bus, id, event, &mut out, &live, generation, &mut rules).await {
                            if let Some(message_id) = running.take() {
                                learn_model_result(&bus, turn_agent.as_ref(), reason, turn_error.as_deref()).await;
                                publish(&bus, EventKind::AgentTurnCompleted { id, message_id, stop_reason: reason }).await;
                            }
                            return;
                        }
                    }
                }
                out.flush(&bus, id).await;
                match ctl {
                    Some(Control::TurnStarted(message_id)) => {
                        running = Some(message_id);
                        turn_agent = bus.store().get_agent(&AgentKey::Id(id)).await.ok();
                        rules.turn_started();
                        set_status(&bus, id, AgentStatus::Running).await;
                        publish(&bus, EventKind::AgentTurnStarted { id, message_id }).await;
                        announce_inbox(&bus, id, &live, generation).await;
                    }
                    Some(Control::Compacted) => rules.compacted(),
                    Some(Control::TurnFinished(message_id, result, diff, done)) => {
                        if running == Some(message_id) {
                            running = None;
                        }
                        rules.turn_finished();
                        let stop_reason = match result {
                            Ok(reason) => reason,
                            Err(message) => {
                                if unavailable_model_error(&message) { turn_error = Some(message.clone()); }
                                publish(&bus, EventKind::AgentError { id, message }).await;
                                StopReason::Error
                            }
                        };
                        learn_model_result(&bus, turn_agent.as_ref(), stop_reason, turn_error.as_deref()).await;
                        turn_error = None;
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
                        // The next turn may start now (see `Control`).
                        let _ = done.send(());
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
                tokio::spawn(inject(
                    bus.clone(),
                    id,
                    live.clone(),
                    generation,
                    injections,
                ));
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
        SessionEventKind::PromptViaFile => EventKind::AgentPromptViaFile { id },
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
                    // A person's message carries notices without being the digest. The
                    // process is gone, so the turn will not finish and release them.
                    if let Err(err) = bus.store().settle_carried_notices(message_id, false).await {
                        tracing::warn!(%message_id, error = %err, "cannot release notices a waiting message carried");
                    }
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
            tokio::spawn(inject(
                bus.clone(),
                id,
                live.clone(),
                generation,
                injections,
            ));
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
                        // A stream rule is the hub's own text; it is counted as a notice.
                        from: Some(MessageSource::Notice),
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

/// The notice versions the store keeps carried: topic and last update, per row.
fn versions_of(rows: &[PendingNotice]) -> Vec<(String, DateTime<Utc>)> {
    rows.iter()
        .map(|row| (row.topic.clone(), row.updated_at))
        .collect()
}

/// Notices carried with a person's message.
struct Carried {
    /// A wake message that was still waiting and was taken out of the queue.
    withdrawn: Option<MessageId>,
    text: String,
    /// The lines themselves, without the class mark, for the input event.
    lines: Vec<String>,
    /// The rows the message carries, for the store (marked carried) and `notice.carried`.
    rows: Vec<PendingNotice>,
}

/// The wake message queued for an agent, and which notice versions it contains.
struct Digest {
    message: MessageId,
    versions: Vec<(String, DateTime<Utc>)>,
}

/// Quiet interval and maximum wait for batch notices. Defaults are 60s and 180s;
/// `[hub] notice_batch_quiet_secs` and `notice_batch_max_secs` override them.
struct NoticeState {
    gate: Mutex<()>,
    /// Held across popping a turn and updating a wake that is still waiting.
    turn: std::sync::Mutex<()>,
    digests: std::sync::Mutex<HashMap<AgentId, Digest>>,
    inflight: std::sync::Mutex<HashMap<MessageId, (AgentId, Vec<(String, DateTime<Utc>)>)>>,
    quiet_ms: AtomicU64,
    max_ms: AtomicU64,
}

impl NoticeState {
    fn new() -> Self {
        Self {
            gate: Mutex::new(()),
            turn: std::sync::Mutex::new(()),
            digests: std::sync::Mutex::new(HashMap::new()),
            inflight: std::sync::Mutex::new(HashMap::new()),
            quiet_ms: AtomicU64::new(60_000),
            max_ms: AtomicU64::new(180_000),
        }
    }

    fn quiet(&self) -> Duration {
        Duration::from_millis(self.quiet_ms.load(Ordering::Relaxed))
    }

    fn max_wait(&self) -> Duration {
        Duration::from_millis(self.max_ms.load(Ordering::Relaxed))
    }

    fn digest_message(&self, agent: AgentId) -> Option<MessageId> {
        self.digests.lock().unwrap().get(&agent).map(|d| d.message)
    }

    fn digest_versions(&self, agent: AgentId) -> Vec<(String, DateTime<Utc>)> {
        self.digests
            .lock()
            .unwrap()
            .get(&agent)
            .map(|d| d.versions.clone())
            .unwrap_or_default()
    }

    fn set_digest(
        &self,
        agent: AgentId,
        message: MessageId,
        versions: Vec<(String, DateTime<Utc>)>,
    ) {
        self.digests
            .lock()
            .unwrap()
            .insert(agent, Digest { message, versions });
    }

    fn clear_digest_if(&self, agent: AgentId, message: MessageId) {
        let mut digests = self.digests.lock().unwrap();
        if digests.get(&agent).is_some_and(|d| d.message == message) {
            digests.remove(&agent);
        }
    }

    fn inflight_versions(&self, agent: AgentId) -> Vec<(String, DateTime<Utc>)> {
        self.inflight
            .lock()
            .unwrap()
            .values()
            .filter(|(id, _)| *id == agent)
            .flat_map(|(_, versions)| versions.clone())
            .collect()
    }

    fn is_inflight(&self, message: MessageId) -> bool {
        self.inflight.lock().unwrap().contains_key(&message)
    }

    /// The wake `message` has started. Its versions are excluded from the next wake until
    /// [`Self::settle`] drops or releases them.
    fn mark_inflight(&self, message: MessageId) {
        let found = self
            .digests
            .lock()
            .unwrap()
            .iter()
            .find(|(_, digest)| digest.message == message)
            .map(|(agent, digest)| (*agent, digest.versions.clone()));
        if let Some((agent, versions)) = found {
            self.inflight
                .lock()
                .unwrap()
                .insert(message, (agent, versions));
        }
    }

    /// `success`: the turn ended with [`StopReason::EndTurn`], so the versions it carried
    /// are deleted. Otherwise they stay, the carry is cleared, and one failed automatic
    /// wake is counted.
    async fn settle(&self, store: &dyn Store, message: MessageId, success: bool) {
        let _gate = self.gate.lock().await;
        let removed = self.inflight.lock().unwrap().remove(&message);
        if let Some((agent, _)) = removed {
            self.clear_digest_if(agent, message);
        } else {
            let agent = self
                .digests
                .lock()
                .unwrap()
                .iter()
                .find(|(_, digest)| digest.message == message)
                .map(|(agent, _)| *agent);
            if let Some(agent) = agent {
                self.clear_digest_if(agent, message);
            }
        }
        if let Err(err) = store.settle_carried_notices(message, success).await {
            tracing::warn!(error = %err, "cannot settle notices a turn finished");
        }
    }
}

fn version_inflight(row: &PendingNotice, inflight: &[(String, DateTime<Utc>)]) -> bool {
    inflight
        .iter()
        .any(|(topic, at)| topic == &row.topic && *at == row.updated_at)
}

fn batch_deadline(
    first_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    quiet: Duration,
    max_wait: Duration,
) -> DateTime<Utc> {
    let quiet_at = updated_at + ChronoDuration::from_std(quiet).unwrap_or(ChronoDuration::MAX);
    let max_at = first_at + ChronoDuration::from_std(max_wait).unwrap_or(ChronoDuration::MAX);
    quiet_at.min(max_at)
}

/// Actions first (unblock, decide, run done, batch), then silent changes. Each line is
/// marked with its class.
fn compose_notices(rows: &[PendingNotice]) -> (String, Vec<String>) {
    let mut action: Vec<&PendingNotice> = rows.iter().filter(|row| row.class.is_action()).collect();
    let mut silent: Vec<&PendingNotice> =
        rows.iter().filter(|row| !row.class.is_action()).collect();
    let by_time = |a: &&PendingNotice, b: &&PendingNotice| {
        a.updated_at.cmp(&b.updated_at).then(a.topic.cmp(&b.topic))
    };
    action.sort_by(|a, b| {
        a.class
            .action_rank()
            .cmp(&b.class.action_rank())
            .then(by_time(a, b))
    });
    silent.sort_by(by_time);
    let mut text = String::from("CrossAgents notices (things you asked to hear about):\n");
    let mut lines = Vec::new();
    if !action.is_empty() {
        text.push_str("To handle:\n");
        for row in action {
            push_notice_line(&mut text, &mut lines, row);
        }
    }
    if !silent.is_empty() {
        text.push_str("Since last time:\n");
        for row in silent {
            push_notice_line(&mut text, &mut lines, row);
        }
    }
    (text, lines)
}

fn push_notice_line(text: &mut String, lines: &mut Vec<String>, row: &PendingNotice) {
    text.push_str("- [");
    text.push_str(row.class.as_str());
    text.push_str("] ");
    text.push_str(&row.line);
    text.push('\n');
    lines.push(row.line.clone());
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

    fn queued(inbox: &Inbox) -> usize {
        inbox.waiting.lock().unwrap().len()
    }

    #[test]
    fn a_running_turn_keeps_the_queue_open_and_a_closed_one_hands_out_nothing() {
        let inbox = Inbox::default();
        inbox.push((MessageId::new(), Prompt::text("one")), false);
        assert!(
            inbox.pop_turn().is_some(),
            "the first message starts a turn"
        );
        assert!(
            !inbox.close_if_idle(),
            "a running turn keeps the queue open"
        );
        assert!(!inbox.is_closed());
        inbox.finish_turn();
        inbox.push((MessageId::new(), Prompt::text("two")), false);
        assert!(inbox.close_if_idle(), "idle again: the queue closes");
        assert!(inbox.is_closed());
        assert!(
            inbox.pop_turn().is_none(),
            "a closed queue starts no turn, so the message is not taken by the old session"
        );
        assert_eq!(
            queued(&inbox),
            1,
            "what waits stays for the session that takes it over"
        );
    }

    #[test]
    fn a_queue_taken_over_is_reopened_with_its_messages() {
        let old = Inbox::default();
        let new = Inbox::default();
        old.push((MessageId::new(), Prompt::text("waiting")), false);
        assert!(old.close_if_idle());
        new.take_from(&old);
        assert_eq!(queued(&new), 1);
        assert_eq!(queued(&old), 0);
        new.closed.store(false, Ordering::SeqCst);
        assert!(
            new.pop_turn().is_some(),
            "the new session starts with the carried message"
        );
    }

    const INCIDENT: &str = "400 model [deepseek-v4.1-flash-sg] service info not found (01a1b1fcef287ba683c3c502e76f1bb3/29fac42c-1b53-4461-8bab-4f8b3c92723f)\nCurrently supported models for your account:\n  - hy4free\n  - hy4std\n  - hy3\n  - hy3-x\n  - deepseek-v4.1-flash\n  - glm-5.3\n  - glm-5.3-flash\n  - glm-5.2\n  - space-bunny\n  - minimax-m3.1-flash\n  - minimax-m2.7\n  - kimi-k3.1-pro\n  - kimi-k3-1\n  - kimi-k2.8-preview\n  - deepseek-v4-pro\n  - step-5-preview\n  - mimo-v2.6-pro\nPlease use --model <model_id> to specify a valid model.";

    #[test]
    fn unavailable_errors_are_distinct_from_rate_limits() {
        for error in [
            INCIDENT,
            "MODEL NOT FOUND",
            "invalid model",
            "unknown model",
            "unsupported model",
        ] {
            assert!(unavailable_model_error(error), "{error}");
        }
        for error in [
            "429 rate limit: model not found",
            "too many requests",
            "500 internal error",
        ] {
            assert!(!unavailable_model_error(error), "{error}");
        }
    }

    #[tokio::test]
    async fn learned_failure_refuses_until_a_successful_turn() {
        let store = Arc::new(
            crossagents_store_sqlite::SqliteStore::open_in_memory()
                .await
                .unwrap(),
        );
        let bus = EventBus::new(store.clone());
        let agent: Agent = serde_json::from_value(serde_json::json!({
            "id": AgentId::new(), "name": "Hazel", "plugin": "codebuddy", "driver": null,
            "model": "deepseek-v4.1-flash-sg", "role": "worker", "cwd": ".", "config": {"account": "site"},
            "status": "idle", "created_at": chrono::Utc::now()
        })).unwrap();
        learn_model_result(&bus, Some(&agent), StopReason::Error, Some(INCIDENT)).await;
        let reason = crossagents_orchestrator::approval::unusable_for(store.as_ref(), &agent, None)
            .await
            .unwrap()
            .unwrap();
        assert!(reason.contains("service info not found"));
        learn_model_result(&bus, Some(&agent), StopReason::EndTurn, None).await;
        assert!(
            crossagents_orchestrator::approval::unusable_for(store.as_ref(), &agent, None)
                .await
                .unwrap()
                .is_none()
        );
    }

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
            log.settle(agent, *id, Settled::Delivery(&delivery(*id)));
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
            log.settle(agent, id, Settled::Delivery(&delivery(id)));
        }
        assert!(matches!(
            log.get(agent, pending),
            Some(ClientEntry::Pending(_))
        ));
        // Another agent's history is untouched.
        let other = AgentId::new();
        log.settle(other, ids[0], Settled::Delivery(&delivery(ids[0])));
        assert!(log.get(other, ids[0]).is_some());
        // Forgetting an agent clears just its records.
        log.forget(agent);
        assert!(log.get(agent, pending).is_none());
        assert!(log.get(other, ids[0]).is_some());
    }

    /// A send whose write timed out is remembered as "may have arrived": a repeat must answer
    /// with that, not start over (docs/TODO.md §二十五「2 的尾巴」).
    #[test]
    fn a_timed_out_send_is_answered_unknown_on_a_repeat() {
        let mut log = ClientLog::default();
        let agent = AgentId::new();
        let client = MessageId::new();
        let sent = MessageId::new();
        log.settle(agent, client, Settled::Unknown(sent));
        assert!(
            matches!(log.get(agent, client), Some(ClientEntry::Unknown(id)) if *id == sent),
            "a repeat must not write the message again"
        );
        // A refused send frees the id, so the same one can try again.
        let retried = MessageId::new();
        log.settle(agent, retried, Settled::Nothing);
        assert!(log.get(agent, retried).is_none());
    }

    fn sample(class: NoticeClass, topic: &str, line: &str, secs: i64) -> PendingNotice {
        let at = Utc::now() + ChronoDuration::seconds(secs);
        PendingNotice {
            agent: AgentId::new(),
            topic: topic.into(),
            line: line.into(),
            class,
            first_at: at,
            updated_at: at,
            carried_by: None,
            wake_tries: 0,
        }
    }

    #[test]
    fn a_wake_lists_actions_before_silent_changes() {
        let rows = vec![
            sample(NoticeClass::Silent, "s", "quiet", 1),
            sample(NoticeClass::Batch, "b", "batched", 2),
            sample(NoticeClass::RunDone, "r", "done", 3),
            sample(NoticeClass::Decide, "d", "choose", 4),
            sample(NoticeClass::Unblock, "u", "unblock", 5),
        ];
        let (text, lines) = compose_notices(&rows);
        let handle = text.find("To handle").unwrap();
        let since = text.find("Since last time").unwrap();
        assert!(handle < since, "{text}");
        assert!(text.find("[unblock]").unwrap() < text.find("[decide]").unwrap());
        assert!(text.find("[decide]").unwrap() < text.find("[run_done]").unwrap());
        assert!(text.find("[run_done]").unwrap() < text.find("[batch]").unwrap());
        assert!(text.find("[batch]").unwrap() < text.find("[silent]").unwrap());
        assert_eq!(
            lines,
            vec![
                "unblock".to_owned(),
                "choose".to_owned(),
                "done".to_owned(),
                "batched".to_owned(),
                "quiet".to_owned(),
            ]
        );
    }

    fn external(name: &str) -> Agent {
        Agent {
            node: None,
            id: AgentId::new(),
            name: name.into(),
            name_source: NameSource::Chosen,
            plugin: EXTERNAL_PLUGIN.into(),
            driver: None,
            model: None,
            role: crossagents_core::Role::Worker,
            home: PathBuf::from("."),
            cwd: PathBuf::from("."),
            config: serde_json::json!({}),
            status: AgentStatus::Stopped,
            created_at: Utc::now(),
            profile: Default::default(),
            topic: None,
            archived_at: None,
        }
    }

    async fn harness(name: &str) -> (Arc<crossagents_store_sqlite::SqliteStore>, Runtime, Agent) {
        let store = Arc::new(
            crossagents_store_sqlite::SqliteStore::open_in_memory()
                .await
                .unwrap(),
        );
        let runtime = Runtime::new(
            Arc::new(EventBus::new(store.clone())),
            PluginRegistry::default(),
        );
        let agent = external(name);
        store.insert_agent(&agent).await.unwrap();
        (store, runtime, agent)
    }

    fn notice_texts(events: &[crossagents_core::Event]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::AgentInput {
                    from: Some(MessageSource::Notice),
                    text,
                    ..
                } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// The text of the latest notice wake, including a revision of a message still waiting.
    fn latest_wake(events: &[crossagents_core::Event]) -> Option<String> {
        let mut current: Option<(MessageId, String)> = None;
        for event in events {
            match &event.kind {
                EventKind::AgentInput {
                    from: Some(MessageSource::Notice),
                    message_id,
                    text,
                    ..
                } => current = Some((*message_id, text.clone())),
                EventKind::AgentInputUpdated {
                    message_id, text, ..
                } if current.as_ref().is_some_and(|(id, _)| id == message_id) => {
                    current = Some((*message_id, text.clone()));
                }
                _ => {}
            }
        }
        current.map(|(_, text)| text)
    }

    async fn stamp(
        store: &crossagents_store_sqlite::SqliteStore,
        agent: AgentId,
        topic: &str,
        first_ago: i64,
        updated_ago: i64,
    ) {
        let now = Utc::now();
        crossagents_store_sqlite::sqlx::query(
            "UPDATE pending_notices SET first_at = ?1, updated_at = ?2 WHERE agent = ?3 AND topic = ?4",
        )
        .bind((now - ChronoDuration::seconds(first_ago)).to_rfc3339())
        .bind((now - ChronoDuration::seconds(updated_ago)).to_rfc3339())
        .bind(agent.to_string())
        .bind(topic)
        .execute(store.pool())
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn silent_is_kept_and_unblock_or_decide_wakes_at_once() {
        let (store, runtime, agent) = harness("ada").await;
        let key = AgentKey::Id(agent.id);
        runtime
            .notify(&key, "progress", "worker moved", NoticeClass::Silent)
            .await
            .unwrap();
        let events = store.events_since(0, 50).await.unwrap();
        assert!(notice_texts(&events).is_empty(), "silent started a turn");
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            EventKind::NoticeRecorded { class: NoticeClass::Silent, line, .. } if line == "worker moved"
        )));
        assert_eq!(
            store
                .list_pending_notices(Some(agent.id))
                .await
                .unwrap()
                .len(),
            1
        );

        runtime
            .notify(&key, "need", "choose a route", NoticeClass::Decide)
            .await
            .unwrap();
        let events = store.events_since(0, 50).await.unwrap();
        let wakes = notice_texts(&events);
        assert_eq!(wakes.len(), 1, "{wakes:?}");
        let wake = &wakes[0];
        let handle = wake.find("To handle").unwrap();
        let since = wake.find("Since last time").unwrap();
        assert!(handle < since, "{wake}");
        assert!(wake.contains("[decide] choose a route"), "{wake}");
        assert!(wake.contains("[silent] worker moved"), "{wake}");

        let (store, runtime, agent) = harness("bea").await;
        runtime
            .notify(
                &AgentKey::Id(agent.id),
                "down",
                "accepting opens the next task",
                NoticeClass::Unblock,
            )
            .await
            .unwrap();
        runtime
            .notify(
                &AgentKey::Id(agent.id),
                "end",
                "the run ended",
                NoticeClass::RunDone,
            )
            .await
            .unwrap();
        let events = store.events_since(0, 50).await.unwrap();
        assert_eq!(notice_texts(&events).len(), 1, "a second wake was queued");
        let wake = latest_wake(&events).unwrap();
        assert!(wake.contains("[unblock]"), "{wake}");
        assert!(wake.contains("[run_done]"), "{wake}");
        assert!(wake.find("[unblock]").unwrap() < wake.find("[run_done]").unwrap());
    }

    #[tokio::test]
    async fn a_persons_message_carries_pending_notices() {
        let (store, runtime, agent) = harness("cara").await;
        let key = AgentKey::Id(agent.id);
        runtime
            .notify(&key, "progress", "still going", NoticeClass::Silent)
            .await
            .unwrap();
        runtime
            .send(&key, "please stop".into(), MessageSource::Human)
            .await
            .unwrap();
        let events = store.events_since(0, 50).await.unwrap();
        assert!(
            notice_texts(&events).is_empty(),
            "the silent notice woke on its own"
        );
        let human = events.iter().find_map(|event| match &event.kind {
            EventKind::AgentInput {
                from: Some(MessageSource::Human),
                text,
                notices,
                ..
            } => Some((text.clone(), notices.clone())),
            _ => None,
        });
        let (text, notices) = human.expect("the person's message");
        assert_eq!(text, "please stop");
        assert!(
            notices.iter().any(|line| line.contains("still going")),
            "{notices:?}"
        );
        assert_eq!(
            store
                .list_pending_notices(Some(agent.id))
                .await
                .unwrap()
                .len(),
            1,
            "a person's message deleted the notice before the turn ended"
        );
        let taken = runtime.take_external(agent.id).await;
        assert_eq!(taken.len(), 1);
        assert!(taken[0].2.contains("please stop"));
        assert!(
            taken[0].2.contains("[silent] still going"),
            "{}",
            taken[0].2
        );
        assert!(
            store
                .list_pending_notices(Some(agent.id))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn batch_merges_until_quiet_or_the_max_wait() {
        let (store, runtime, agent) = harness("dora").await;
        let key = AgentKey::Id(agent.id);
        runtime
            .notify(&key, "a", "first batch", NoticeClass::Batch)
            .await
            .unwrap();
        runtime
            .notify(&key, "b", "second batch", NoticeClass::Batch)
            .await
            .unwrap();
        runtime
            .notify(&key, "s", "aside", NoticeClass::Silent)
            .await
            .unwrap();
        runtime.poll_notice_wakes().await.unwrap();
        assert!(
            notice_texts(&store.events_since(0, 50).await.unwrap()).is_empty(),
            "a fresh batch woke immediately"
        );

        // Latest activity is recent, so the quiet interval has not elapsed, but the batch
        // started long enough ago that the maximum wait has.
        stamp(&store, agent.id, "a", 200, 10).await;
        stamp(&store, agent.id, "b", 190, 10).await;
        runtime.poll_notice_wakes().await.unwrap();
        let wakes = notice_texts(&store.events_since(0, 50).await.unwrap());
        assert_eq!(wakes.len(), 1, "{wakes:?}");
        assert!(wakes[0].contains("[batch] first batch"), "{}", wakes[0]);
        assert!(wakes[0].contains("[batch] second batch"), "{}", wakes[0]);
        assert!(wakes[0].contains("[silent] aside"), "{}", wakes[0]);
        assert!(wakes[0].find("To handle").unwrap() < wakes[0].find("Since last time").unwrap());
    }

    #[tokio::test]
    async fn a_batch_waits_out_the_quiet_interval() {
        let (store, runtime, agent) = harness("eden").await;
        let key = AgentKey::Id(agent.id);
        runtime
            .notify(&key, "a", "only batch", NoticeClass::Batch)
            .await
            .unwrap();
        // Old enough that quiet would have elapsed, then a fresh update pushes it out,
        // while the original first_at is still inside the maximum wait.
        stamp(&store, agent.id, "a", 100, 10).await;
        runtime.poll_notice_wakes().await.unwrap();
        assert!(notice_texts(&store.events_since(0, 20).await.unwrap()).is_empty());
        stamp(&store, agent.id, "a", 100, 70).await;
        runtime.poll_notice_wakes().await.unwrap();
        let wakes = notice_texts(&store.events_since(0, 20).await.unwrap());
        assert_eq!(wakes.len(), 1, "{wakes:?}");
        assert!(wakes[0].contains("only batch"));
    }

    #[tokio::test]
    async fn pending_notices_survive_a_restart() {
        let (store, runtime, agent) = harness("fay").await;
        let key = AgentKey::Id(agent.id);
        runtime
            .notify(&key, "s", "kept", NoticeClass::Silent)
            .await
            .unwrap();
        runtime
            .notify(&key, "b", "held", NoticeClass::Batch)
            .await
            .unwrap();
        drop(runtime);
        assert_eq!(
            store
                .list_pending_notices(Some(agent.id))
                .await
                .unwrap()
                .len(),
            2
        );
        stamp(&store, agent.id, "b", 80, 70).await;
        let runtime = Runtime::new(
            Arc::new(EventBus::new(store.clone())),
            PluginRegistry::default(),
        );
        runtime.poll_notice_wakes().await.unwrap();
        let wakes = notice_texts(&store.events_since(0, 50).await.unwrap());
        assert_eq!(wakes.len(), 1, "{wakes:?}");
        assert!(wakes[0].contains("[batch] held"), "{}", wakes[0]);
        assert!(wakes[0].contains("[silent] kept"), "{}", wakes[0]);
    }

    #[tokio::test]
    async fn the_same_topic_replaces_the_line_and_keeps_first_at() {
        let (store, runtime, agent) = harness("gia").await;
        let key = AgentKey::Id(agent.id);
        runtime
            .notify(&key, "task", "older", NoticeClass::Batch)
            .await
            .unwrap();
        let first = store.list_pending_notices(Some(agent.id)).await.unwrap()[0].first_at;
        runtime
            .notify(&key, "task", "newer", NoticeClass::Silent)
            .await
            .unwrap();
        let rows = store.list_pending_notices(Some(agent.id)).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].line, "newer");
        assert_eq!(rows[0].class, NoticeClass::Silent);
        assert_eq!(rows[0].first_at, first);
        runtime.poll_notice_wakes().await.unwrap();
        assert!(
            notice_texts(&store.events_since(0, 20).await.unwrap()).is_empty(),
            "replacing a batch with silent woke"
        );
    }

    #[tokio::test]
    async fn one_agents_failed_delivery_does_not_block_another() {
        let (store, runtime, broken) = harness("broken").await;
        crossagents_store_sqlite::sqlx::query("UPDATE agents SET plugin = 'missing' WHERE id = ?1")
            .bind(broken.id.to_string())
            .execute(store.pool())
            .await
            .unwrap();
        let other = external("other");
        store.insert_agent(&other).await.unwrap();
        let now = Utc::now();
        for (agent, line) in [(broken.id, "broken notice"), (other.id, "other notice")] {
            store
                .upsert_pending_notice(&PendingNotice {
                    agent,
                    topic: "need".into(),
                    line: line.into(),
                    class: NoticeClass::Decide,
                    first_at: now,
                    updated_at: now,
                    carried_by: None,
                    wake_tries: 0,
                })
                .await
                .unwrap();
        }
        runtime.poll_notice_wakes().await.unwrap();
        let wakes = notice_texts(&store.events_since(0, 50).await.unwrap());
        assert_eq!(wakes.len(), 1, "{wakes:?}");
        assert!(wakes[0].contains("other notice"), "{}", wakes[0]);
        assert!(!wakes.iter().any(|wake| wake.contains("broken notice")));
        let broken_rows = store.list_pending_notices(Some(broken.id)).await.unwrap();
        assert_eq!(broken_rows.len(), 1);
        assert_eq!(broken_rows[0].wake_tries, 1);
        assert!(broken_rows[0].carried_by.is_none());
    }

    #[tokio::test]
    async fn an_archived_agent_is_not_delivered_and_keeps_the_row() {
        let (store, runtime, agent) = harness("lea").await;
        crossagents_store_sqlite::sqlx::query("UPDATE agents SET archived_at = ?1 WHERE id = ?2")
            .bind(Utc::now().to_rfc3339())
            .bind(agent.id.to_string())
            .execute(store.pool())
            .await
            .unwrap();
        runtime
            .notify(&AgentKey::Id(agent.id), "need", "kept", NoticeClass::Decide)
            .await
            .unwrap();
        runtime.poll_notice_wakes().await.unwrap();
        assert!(
            notice_texts(&store.events_since(0, 20).await.unwrap()).is_empty(),
            "an archived agent was woken"
        );
        let rows = store.list_pending_notices(Some(agent.id)).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].line, "kept");
        assert!(rows[0].carried_by.is_none());
        assert_eq!(rows[0].wake_tries, 0);
    }

    #[tokio::test]
    async fn a_failed_or_interrupted_wake_is_retried_once() {
        let (store, runtime, agent) = harness("ida").await;
        let key = AgentKey::Id(agent.id);
        runtime
            .notify(&key, "need", "choose", NoticeClass::Decide)
            .await
            .unwrap();
        let first = runtime.notice_state.digest_message(agent.id).unwrap();
        {
            let mut queues = runtime.external.lock().await;
            queues
                .get_mut(&agent.id)
                .unwrap()
                .retain(|(id, _, _)| *id != first);
        }
        runtime
            .notice_state
            .settle(store.as_ref(), first, false)
            .await;
        let rows = store.list_pending_notices(Some(agent.id)).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].wake_tries, 1);
        assert!(rows[0].carried_by.is_none());
        runtime.poll_notice_wakes().await.unwrap();
        assert_eq!(
            notice_texts(&store.events_since(0, 20).await.unwrap()).len(),
            2
        );
        let second = runtime.notice_state.digest_message(agent.id).unwrap();
        {
            let mut queues = runtime.external.lock().await;
            queues
                .get_mut(&agent.id)
                .unwrap()
                .retain(|(id, _, _)| *id != second);
        }
        runtime
            .notice_state
            .settle(store.as_ref(), second, false)
            .await;
        runtime.poll_notice_wakes().await.unwrap();
        assert_eq!(
            notice_texts(&store.events_since(0, 20).await.unwrap()).len(),
            2,
            "a third automatic wake was queued"
        );
        let rows = store.list_pending_notices(Some(agent.id)).await.unwrap();
        assert_eq!(rows[0].wake_tries, 2);
        runtime
            .notify(&key, "other", "fresh", NoticeClass::Decide)
            .await
            .unwrap();
        let wake = latest_wake(&store.events_since(0, 30).await.unwrap()).unwrap();
        assert!(wake.contains("[decide] choose"), "{wake}");
        assert!(wake.contains("[decide] fresh"), "{wake}");
    }

    #[tokio::test]
    async fn a_withdrawn_wake_is_queued_again() {
        let (store, runtime, agent) = harness("jules").await;
        runtime
            .notify(
                &AgentKey::Id(agent.id),
                "need",
                "still here",
                NoticeClass::Decide,
            )
            .await
            .unwrap();
        let first = runtime.notice_state.digest_message(agent.id).unwrap();
        {
            let mut queues = runtime.external.lock().await;
            queues.get_mut(&agent.id).unwrap().clear();
        }
        runtime.poll_notice_wakes().await.unwrap();
        let second = runtime.notice_state.digest_message(agent.id).unwrap();
        assert_ne!(first, second);
        let wakes = notice_texts(&store.events_since(0, 20).await.unwrap());
        assert_eq!(wakes.len(), 2, "{wakes:?}");
        assert!(wakes[1].contains("still here"), "{}", wakes[1]);
        let rows = store.list_pending_notices(Some(agent.id)).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].carried_by, Some(second));
        assert_eq!(rows[0].wake_tries, 1);
    }

    #[tokio::test]
    async fn notices_are_deleted_only_when_the_turn_is_handed_off() {
        let (store, runtime, agent) = harness("kea").await;
        let key = AgentKey::Id(agent.id);
        runtime
            .notify(&key, "need", "first", NoticeClass::Decide)
            .await
            .unwrap();
        let queued = store.list_pending_notices(Some(agent.id)).await.unwrap();
        assert_eq!(queued.len(), 1);
        assert!(queued[0].carried_by.is_some());
        runtime.take_external(agent.id).await;
        assert!(
            store
                .list_pending_notices(Some(agent.id))
                .await
                .unwrap()
                .is_empty()
        );

        runtime
            .notify(&key, "need", "old", NoticeClass::Decide)
            .await
            .unwrap();
        let kept = store.list_pending_notices(Some(agent.id)).await.unwrap()[0].clone();
        store
            .upsert_pending_notice(&PendingNotice {
                line: "replaced".into(),
                updated_at: Utc::now(),
                carried_by: None,
                wake_tries: 0,
                ..kept
            })
            .await
            .unwrap();
        runtime.take_external(agent.id).await;
        let rows = store.list_pending_notices(Some(agent.id)).await.unwrap();
        assert_eq!(
            rows.len(),
            1,
            "the replacement was deleted with the old wake"
        );
        assert_eq!(rows[0].line, "replaced");
        assert!(rows[0].carried_by.is_none());
    }

    #[tokio::test]
    async fn deleting_an_agent_drops_its_pending_notices() {
        let (store, runtime, agent) = harness("moe").await;
        runtime
            .notify(
                &AgentKey::Id(agent.id),
                "progress",
                "kept until delete",
                NoticeClass::Silent,
            )
            .await
            .unwrap();
        store.delete_agent(agent.id).await.unwrap();
        assert!(
            store
                .list_pending_notices(Some(agent.id))
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// A session whose first prompt does not return until `hold` is notified.
    struct HoldSession {
        entered: std::sync::atomic::AtomicBool,
        hold: tokio::sync::Notify,
    }

    #[async_trait]
    impl DriverSession for HoldSession {
        fn session_id(&self) -> &str {
            "held"
        }

        async fn prompt(&self, _text: &str) -> Result<crossagents_core::TurnOutcome, CoreError> {
            self.entered.store(true, Ordering::SeqCst);
            self.hold.notified().await;
            Ok(crossagents_core::TurnOutcome {
                stop_reason: StopReason::EndTurn,
                turn_id: None,
            })
        }

        async fn interrupt(&self) -> Result<(), CoreError> {
            Ok(())
        }

        async fn shutdown(&self) {}
    }

    /// An in-hub agent whose turn is blocked, so a later message stays waiting.
    async fn busy_agent(
        store: &crossagents_store_sqlite::SqliteStore,
        runtime: &Runtime,
        agent: &Agent,
    ) -> (mpsc::UnboundedSender<SessionEvent>, Arc<HoldSession>) {
        crossagents_store_sqlite::sqlx::query("UPDATE agents SET plugin = 'local' WHERE id = ?1")
            .bind(agent.id.to_string())
            .execute(store.pool())
            .await
            .unwrap();
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let session = Arc::new(HoldSession {
            entered: std::sync::atomic::AtomicBool::new(false),
            hold: tokio::sync::Notify::new(),
        });
        let generation = runtime.generation.fetch_add(1, Ordering::Relaxed);
        let inbox = runtime
            .install(
                agent.id,
                session.clone(),
                Channels {
                    events: events_rx,
                    control_tx,
                    control_rx,
                    generation,
                },
                None,
                None,
                false,
            )
            .await;
        inbox.push(
            (
                MessageId::new(),
                Prompt {
                    text: "busy".into(),
                    attachments: Vec::new(),
                },
            ),
            false,
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while !session.entered.load(Ordering::SeqCst) {
            if tokio::time::Instant::now() > deadline {
                panic!("the turn did not start");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        (events_tx, session)
    }

    async fn person_carries_a_notice(
        store: &crossagents_store_sqlite::SqliteStore,
        runtime: &Runtime,
        agent: &Agent,
    ) -> MessageId {
        let now = Utc::now();
        store
            .upsert_pending_notice(&PendingNotice {
                agent: agent.id,
                topic: "need".into(),
                line: "choose".into(),
                class: NoticeClass::Decide,
                first_at: now,
                updated_at: now,
                carried_by: None,
                wake_tries: 0,
            })
            .await
            .unwrap();
        let delivery = runtime
            .send(
                &AgentKey::Id(agent.id),
                "please look".into(),
                MessageSource::Human,
            )
            .await
            .unwrap();
        assert!(delivery.queued, "the person's message started at once");
        let rows = store.list_pending_notices(Some(agent.id)).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].carried_by, Some(delivery.message_id));
        delivery.message_id
    }

    fn carried_is_free(rows: &[PendingNotice]) {
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].line, "choose");
        assert!(rows[0].carried_by.is_none());
        assert_eq!(rows[0].wake_tries, 1);
    }

    #[tokio::test]
    async fn withdrawing_a_persons_message_releases_the_notices_it_carried() {
        let (store, runtime, agent) = harness("niel").await;
        let (_events, _session) = busy_agent(&store, &runtime, &agent).await;
        let message = person_carries_a_notice(&store, &runtime, &agent).await;
        runtime
            .withdraw(&AgentKey::Id(agent.id), message)
            .await
            .unwrap();
        let rows = store.list_pending_notices(Some(agent.id)).await.unwrap();
        carried_is_free(&rows);
        runtime.poll_notice_wakes().await.unwrap();
        let wakes = notice_texts(&store.events_since(0, 20).await.unwrap());
        assert_eq!(wakes.len(), 1, "{wakes:?}");
        assert!(wakes[0].contains("[decide] choose"), "{}", wakes[0]);
    }

    #[tokio::test]
    async fn a_session_exit_releases_notices_a_waiting_message_carried() {
        let (store, runtime, agent) = harness("orin").await;
        let (events, _session) = busy_agent(&store, &runtime, &agent).await;
        let message = person_carries_a_notice(&store, &runtime, &agent).await;
        events
            .send(
                SessionEventKind::Exited {
                    message: "gone".into(),
                    cause: None,
                    code: None,
                }
                .into(),
            )
            .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let withdrawn = store
                .events_since(0, 40)
                .await
                .unwrap()
                .iter()
                .any(|event| {
                    matches!(
                        &event.kind,
                        EventKind::AgentInputWithdrawn { message_id, .. } if *message_id == message
                    )
                });
            if withdrawn {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("the exit did not take the waiting message back");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let rows = store.list_pending_notices(Some(agent.id)).await.unwrap();
        carried_is_free(&rows);
        // The process is gone. A new session is what the next start would open, so the
        // poll can hand the wake over.
        let (_events, _again) = busy_agent(&store, &runtime, &agent).await;
        runtime.poll_notice_wakes().await.unwrap();
        let wakes = notice_texts(&store.events_since(0, 40).await.unwrap());
        assert_eq!(wakes.len(), 1, "{wakes:?}");
        assert!(wakes[0].contains("[decide] choose"), "{}", wakes[0]);
    }

    #[tokio::test]
    async fn a_restart_retries_an_interrupted_turn_once_and_rebuilds_an_unstarted_wake() {
        let (store, runtime, queued) = harness("queued").await;
        runtime
            .notify(
                &AgentKey::Id(queued.id),
                "need",
                "once",
                NoticeClass::Decide,
            )
            .await
            .unwrap();
        let interrupted = external("interrupted");
        store.insert_agent(&interrupted).await.unwrap();
        let now = Utc::now();
        let stored = store
            .upsert_pending_notice(&PendingNotice {
                agent: interrupted.id,
                topic: "hand".into(),
                line: "in hand".into(),
                class: NoticeClass::Decide,
                first_at: now,
                updated_at: now,
                carried_by: None,
                wake_tries: 0,
            })
            .await
            .unwrap();
        let message = MessageId::new();
        store
            .mark_notices_carried(
                interrupted.id,
                &[("hand".into(), stored.updated_at)],
                message,
            )
            .await
            .unwrap();
        publish(
            &runtime.bus,
            EventKind::AgentTurnStarted {
                id: interrupted.id,
                message_id: message,
            },
        )
        .await;
        drop(runtime);
        let runtime = Arc::new(Runtime::new(
            Arc::new(EventBus::new(store.clone())),
            PluginRegistry::default(),
        ));
        runtime.recover(Some(0)).await.unwrap();
        let queued_rows = store.list_pending_notices(Some(queued.id)).await.unwrap();
        assert_eq!(queued_rows.len(), 1);
        assert!(queued_rows[0].carried_by.is_none());
        assert_eq!(queued_rows[0].wake_tries, 0);
        let interrupted_rows = store
            .list_pending_notices(Some(interrupted.id))
            .await
            .unwrap();
        assert_eq!(interrupted_rows.len(), 1);
        assert_eq!(interrupted_rows[0].line, "in hand");
        assert!(interrupted_rows[0].carried_by.is_none());
        assert_eq!(interrupted_rows[0].wake_tries, 1);
        runtime.poll_notice_wakes().await.unwrap();
        runtime.poll_notice_wakes().await.unwrap();
        let wakes = notice_texts(&store.events_since(0, 40).await.unwrap());
        let once = wakes.iter().filter(|wake| wake.contains("once")).count();
        let handed = wakes.iter().filter(|wake| wake.contains("in hand")).count();
        assert_eq!(once, 2, "{wakes:?}");
        assert_eq!(handed, 1, "{wakes:?}");
    }

    #[tokio::test]
    async fn release_stranded_keeps_a_turn_this_hub_takes_over() {
        let (store, _runtime, agent) = harness("keep").await;
        let now = Utc::now();
        let kept = store
            .upsert_pending_notice(&PendingNotice {
                agent: agent.id,
                topic: "kept".into(),
                line: "still running".into(),
                class: NoticeClass::Decide,
                first_at: now,
                updated_at: now,
                carried_by: None,
                wake_tries: 0,
            })
            .await
            .unwrap();
        let dropped = store
            .upsert_pending_notice(&PendingNotice {
                agent: agent.id,
                topic: "dropped".into(),
                line: "left behind".into(),
                class: NoticeClass::Decide,
                first_at: now,
                updated_at: now,
                carried_by: None,
                wake_tries: 0,
            })
            .await
            .unwrap();
        let running = MessageId::new();
        store
            .mark_notices_carried(agent.id, &[("kept".into(), kept.updated_at)], running)
            .await
            .unwrap();
        store
            .mark_notices_carried(
                agent.id,
                &[("dropped".into(), dropped.updated_at)],
                MessageId::new(),
            )
            .await
            .unwrap();
        store.release_stranded_notices(&[running]).await.unwrap();
        let rows = store.list_pending_notices(Some(agent.id)).await.unwrap();
        let kept_row = rows.iter().find(|row| row.topic == "kept").unwrap();
        let dropped_row = rows.iter().find(|row| row.topic == "dropped").unwrap();
        assert_eq!(kept_row.line, "still running");
        assert_eq!(kept_row.carried_by, Some(running));
        assert_eq!(kept_row.wake_tries, 0);
        assert_eq!(dropped_row.line, "left behind");
        assert!(dropped_row.carried_by.is_none());
        assert_eq!(dropped_row.wake_tries, 0);
    }
}
