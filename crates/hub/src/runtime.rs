//! Live agent sessions: start, deliver messages one turn at a time, cancel, stop, and turn
//! what happens inside a session into events.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use crossagents_core::{
    Agent, AgentId, AgentKey, AgentStatus, CoreError, DriverSession, EventBus, EventKind,
    McpServerSpec, MessageId, MessageSource, OutputStream, PermissionPolicy, SessionEvent,
    SessionEventKind, SessionSpec, StopReason, Store,
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

type LiveMap = Arc<Mutex<HashMap<AgentId, Arc<Live>>>>;

pub struct Runtime {
    bus: Arc<EventBus>,
    plugins: PluginRegistry,
    setup: Option<Arc<dyn SessionSetup>>,
    live: LiveMap,
    /// Serializes start/stop per agent without blocking other agents.
    locks: Mutex<HashMap<AgentId, Arc<Mutex<()>>>>,
    generation: AtomicU64,
}

/// Messages to an agent's event forwarder, kept in order with the session's own events.
enum Control {
    TurnStarted(MessageId),
    TurnFinished(MessageId, Result<StopReason, String>),
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

        let spec = SessionSpec {
            agent: agent.clone(),
            mcp_servers: extras.mcp_servers,
            permission: permission_of(&agent),
            permissions: None,
            command: command_of(&agent),
            instructions: extras.instructions,
            env: extras.env,
            events: events_tx,
        };
        let session: Arc<dyn DriverSession> = match resolved.driver.open(spec).await {
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
        tokio::spawn(run_turns(session, inbox, control_tx));
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
        self.deliver(key, text, from, false).await
    }

    /// Delivers a message ahead of everything waiting and interrupts the running turn, so it
    /// is the next thing the agent reads.
    pub async fn send_now(
        &self,
        key: &AgentKey,
        text: String,
        from: MessageSource,
    ) -> Result<(MessageId, bool), CoreError> {
        self.deliver(key, text, from, true).await
    }

    async fn deliver(
        &self,
        key: &AgentKey,
        text: String,
        from: MessageSource,
        first: bool,
    ) -> Result<(MessageId, bool), CoreError> {
        let agent = self.start(key).await?;
        let live = self
            .live
            .lock()
            .await
            .get(&agent.id)
            .cloned()
            .filter(|l| !l.inbox.is_closed())
            .ok_or_else(|| CoreError::Agent(format!("agent {} stopped", agent.name)))?;
        let message_id = MessageId::new();
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
            },
        )
        .await;
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

/// Runs one prompt at a time from the inbox.
async fn run_turns(
    session: Arc<dyn DriverSession>,
    inbox: Arc<Inbox>,
    control: mpsc::UnboundedSender<Control>,
) {
    while let Some((message_id, text)) = inbox.next().await {
        let _ = control.send(Control::TurnStarted(message_id));
        let result = session
            .prompt(&text)
            .await
            .map(|outcome| outcome.stop_reason)
            .map_err(|e| e.to_string());
        inbox.finish_turn();
        if control
            .send(Control::TurnFinished(message_id, result))
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
                    Some(Control::TurnFinished(message_id, result)) => {
                        let stop_reason = match result {
                            Ok(reason) => reason,
                            Err(message) => {
                                publish(&bus, EventKind::AgentError { id, message }).await;
                                StopReason::Error
                            }
                        };
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
    if let SessionEventKind::Output { stream, text } = event {
        out.push(bus, id, stream, text).await;
        return false;
    }
    out.flush(bus, id).await;
    let kind = match event {
        SessionEventKind::Output { .. } => unreachable!(),
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
        SessionEventKind::Usage {
            used,
            size,
            cost_usd,
            ..
        } => EventKind::AgentUsage {
            id,
            used,
            size,
            cost_usd,
        },
        SessionEventKind::Warning { message } | SessionEventKind::Error { message } => {
            EventKind::AgentError { id, message }
        }
        // Turn boundaries come from the runtime's own control messages. Diffs, quotas and
        // permission answers have no hub event yet (docs/tasks/N1.md §6).
        other @ (SessionEventKind::TurnStarted { .. }
        | SessionEventKind::TurnCompleted { .. }
        | SessionEventKind::TurnDiff { .. }
        | SessionEventKind::RateLimits { .. }
        | SessionEventKind::PermissionRequested { .. }) => {
            tracing::debug!(%id, event = ?other, "session event not published");
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

#[derive(Default)]
struct Coalescer {
    buffer: Option<(OutputStream, String)>,
}

impl Coalescer {
    async fn push(&mut self, bus: &EventBus, id: AgentId, stream: OutputStream, text: String) {
        match &mut self.buffer {
            Some((current, buf)) if *current == stream => buf.push_str(&text),
            _ => {
                self.flush(bus, id).await;
                self.buffer = Some((stream, text));
            }
        }
    }

    async fn flush(&mut self, bus: &EventBus, id: AgentId) {
        if let Some((stream, text)) = self.buffer.take()
            && !text.is_empty()
        {
            publish(bus, EventKind::AgentOutput { id, stream, text }).await;
        }
    }
}
