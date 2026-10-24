//! Reminders that come due and watches that match (docs/tasks/H1.md §5.3). Both end up as a
//! line in the owner's notification digest (`Runtime::notify`); the hub says nothing else.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use crossagents_core::{
    AgentId, AgentKey, AgentStatus, CoreError, Event, EventBus, EventKind, StopReason, TaskStatus,
    WatchOn, WatchTarget,
};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

use crate::runtime::Runtime;

const TICK: Duration = Duration::from_secs(1);

pub struct Notices {
    pub bus: Arc<EventBus>,
    pub runtime: Arc<Runtime>,
    pub orchestrator: Arc<crossagents_orchestrator::Orchestrator>,
    pub budgets: Arc<crate::budget::Budgets>,
}

/// Budgets are checked every this many ticks.
const BUDGET_EVERY: u32 = 5;

impl Notices {
    /// Starts the reminder clock and the watch matcher; both end with `shutdown`.
    pub fn start(self: &Arc<Self>, shutdown: CancellationToken) {
        let clock = self.clone();
        let stop = shutdown.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(TICK);
            let mut ticks = 0u32;
            loop {
                tokio::select! {
                    _ = tick.tick() => {}
                    _ = stop.cancelled() => break,
                }
                ticks = ticks.wrapping_add(1);
                if let Err(err) = clock.fire_due().await {
                    tracing::warn!(error = %err, "cannot check reminders");
                }
                if let Err(err) = clock.orchestrator.expire_questions().await {
                    tracing::warn!(error = %err, "cannot time out questions");
                }
                if ticks.is_multiple_of(BUDGET_EVERY)
                    && let Err(err) = clock.budgets.check(clock.bus.store().as_ref()).await
                {
                    tracing::warn!(error = %err, "cannot check budgets");
                }
            }
        });
        let matcher = self.clone();
        let mut events = self.bus.subscribe();
        tokio::spawn(async move {
            loop {
                let received = tokio::select! {
                    received = events.recv() => received,
                    _ = shutdown.cancelled() => break,
                };
                match received {
                    Ok(event) => {
                        matcher.budgets.apply(&event).await;
                        // A worker refused for quota hands its task on (docs/tasks/DOGFOOD.md §5).
                        if let EventKind::AgentRateLimits { id, limits } = &event.kind
                            && let Some(why) = &limits.reached
                        {
                            let orchestrator = matcher.orchestrator.clone();
                            let (id, why) = (*id, why.clone());
                            tokio::spawn(async move {
                                if let Err(err) = orchestrator.rate_limited(id, &why).await {
                                    tracing::warn!(%id, error = %err, "cannot hand on a rate-limited task");
                                }
                            });
                        }
                        if let Err(err) = matcher.on_event(&event).await {
                            tracing::warn!(error = %err, "cannot match watches");
                        }
                    }
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                }
            }
        });
    }

    async fn fire_due(&self) -> Result<(), CoreError> {
        let store = self.bus.store();
        let now = Utc::now();
        for reminder in store.list_reminders(None).await? {
            if reminder.due_at > now {
                break; // soonest first
            }
            store.delete_reminder(&reminder.id).await?;
            if reminder.only_if_open
                && let Some(task) = reminder.task
                && let Ok(task) = store.get_task(task).await
                && ended(task.status)
            {
                continue;
            }
            self.bus
                .publish(EventKind::ReminderFired {
                    owner: reminder.owner,
                    reminder_id: reminder.id.clone(),
                    note: reminder.note.clone(),
                })
                .await?;
            let line = format!("Reminder ({}): {}", reminder.id, reminder.note);
            self.tell(reminder.owner, &format!("reminder:{}", reminder.id), &line)
                .await;
        }
        Ok(())
    }

    async fn on_event(&self, event: &Event) -> Result<(), CoreError> {
        let name = match event.kind.agent_id() {
            Some(id) => self
                .bus
                .store()
                .get_agent(&AgentKey::Id(id))
                .await
                .map(|a| a.name)
                .unwrap_or_else(|_| id.to_string()),
            None => String::new(),
        };
        let Some((target, what, line)) = describe(event, &name) else {
            return Ok(());
        };
        let store = self.bus.store();
        for watch in store.list_watches(None).await? {
            if watch.target != target || !watch.wants(what) {
                continue;
            }
            if let WatchTarget::Agent { agent_id } = &watch.target
                && *agent_id == watch.owner
            {
                continue; // nobody needs news about themselves
            }
            self.bus
                .publish(EventKind::WatchTriggered {
                    owner: watch.owner,
                    watch_id: watch.id.clone(),
                    line: line.clone(),
                })
                .await?;
            // One line per watch: newer news on it replaces older in the digest.
            self.tell(watch.owner, &format!("watch:{}", watch.id), &line)
                .await;
        }
        Ok(())
    }

    async fn tell(&self, owner: AgentId, topic: &str, line: &str) {
        if let Err(err) = self.runtime.notify(&AgentKey::Id(owner), topic, line).await {
            tracing::warn!(%owner, error = %err, "cannot deliver a notice");
        }
    }
}

fn ended(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Merged | TaskStatus::Failed | TaskStatus::Cancelled
    )
}

/// What an event means to watchers: the target, the kind of change, and one line about it.
fn describe(event: &Event, agent: &str) -> Option<(WatchTarget, WatchOn, String)> {
    match &event.kind {
        EventKind::TaskStatusChanged { id, key, to, .. } => {
            let what = match to {
                TaskStatus::Merged | TaskStatus::Passed => WatchOn::Done,
                TaskStatus::Failed | TaskStatus::Cancelled => WatchOn::Failed,
                TaskStatus::WaitingAnswer => WatchOn::Asked,
                _ => WatchOn::Any,
            };
            let to = serde_json::to_value(to).ok()?;
            Some((
                WatchTarget::Task { task_id: *id },
                what,
                format!("Task #{key} is now {}.", to.as_str().unwrap_or("?")),
            ))
        }
        EventKind::AgentTurnCompleted {
            id, stop_reason, ..
        } => {
            let what = match stop_reason {
                StopReason::EndTurn => WatchOn::Done,
                StopReason::Error => WatchOn::Failed,
                _ => WatchOn::Any,
            };
            let reason = serde_json::to_value(stop_reason).ok()?;
            Some((
                WatchTarget::Agent { agent_id: *id },
                what,
                format!(
                    "{agent} finished a turn ({}).",
                    reason.as_str().unwrap_or("?")
                ),
            ))
        }
        EventKind::AgentStatusChanged { id, to, .. } => {
            let what = match to {
                AgentStatus::Idle => WatchOn::Idle,
                AgentStatus::Error => WatchOn::Failed,
                _ => return None,
            };
            let to = serde_json::to_value(to).ok()?;
            Some((
                WatchTarget::Agent { agent_id: *id },
                what,
                format!("{agent} is now {}.", to.as_str().unwrap_or("?")),
            ))
        }
        _ => None,
    }
}
