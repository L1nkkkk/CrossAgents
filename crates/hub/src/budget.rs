//! What each run has spent, kept up to date from the event stream, and the budget caps of
//! its policy (docs/tasks/H1.md §4). Agents report cumulative per-session figures; a new
//! session starts when the agent goes to `starting`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::Utc;
use crossagents_core::{
    AgentId, AgentStatus, CoreError, Event, EventKind, RunId, RunStatus, Store,
};
use crossagents_orchestrator::Orchestrator;
use tokio::sync::Mutex;

/// Levels at which the orchestrator hears about a cap.
const LEVELS: [u32; 2] = [80, 100];

#[derive(Default)]
struct Spend {
    run: Option<RunId>,
    closed_tokens: u64,
    session_tokens: u64,
    closed_usd: f64,
    session_usd: f64,
}

/// Tokens and dollars per run.
#[derive(Default)]
pub struct Ledger {
    agents: HashMap<AgentId, Spend>,
    /// (run, cap, level) already reported.
    reported: HashSet<(RunId, &'static str, u32)>,
}

impl Ledger {
    pub fn apply(&mut self, event: &Event) {
        match &event.kind {
            EventKind::AgentStatusChanged {
                id,
                to: AgentStatus::Starting,
                ..
            } => {
                let spend = self.agents.entry(*id).or_default();
                spend.closed_tokens += std::mem::take(&mut spend.session_tokens);
                spend.closed_usd += std::mem::take(&mut spend.session_usd);
            }
            EventKind::AgentUsage {
                id,
                cost_usd,
                tokens,
                ..
            } => {
                let spend = self.agents.entry(*id).or_default();
                if let Some(tokens) = tokens {
                    spend.session_tokens = tokens.total;
                }
                if let Some(cost) = cost_usd {
                    spend.session_usd = *cost;
                }
            }
            EventKind::RunCreated { run } => {
                self.agents.entry(run.planner).or_default().run = Some(run.id);
            }
            EventKind::TaskStatusChanged {
                run_id,
                assignee: Some(agent),
                ..
            } => {
                self.agents.entry(*agent).or_default().run = Some(*run_id);
            }
            _ => {}
        }
    }

    /// (tokens, dollars) spent by the run's agents.
    pub fn spent(&self, run: RunId) -> (u64, f64) {
        self.agents
            .values()
            .filter(|s| s.run == Some(run))
            .fold((0, 0.0), |(t, u), s| {
                (
                    t + s.closed_tokens + s.session_tokens,
                    u + s.closed_usd + s.session_usd,
                )
            })
    }
}

/// Keeps the ledger and tells the orchestrator when a running run crosses 80% or 100% of a
/// cap.
pub struct Budgets {
    pub ledger: Mutex<Ledger>,
    pub orchestrator: Arc<Orchestrator>,
}

impl Budgets {
    /// Reads the event log so far into the ledger.
    pub async fn load(
        store: &dyn Store,
        orchestrator: Arc<Orchestrator>,
    ) -> Result<Self, CoreError> {
        let mut ledger = Ledger::default();
        let mut after = 0;
        loop {
            let page = store.events_since(after, 1000).await?;
            let Some(last) = page.last() else { break };
            after = last.seq;
            for event in &page {
                ledger.apply(event);
            }
        }
        Ok(Self {
            ledger: Mutex::new(ledger),
            orchestrator,
        })
    }

    pub async fn apply(&self, event: &Event) {
        self.ledger.lock().await.apply(event);
    }

    pub async fn check(&self, store: &dyn Store) -> Result<(), CoreError> {
        for run in store.list_runs().await? {
            let budget = &run.config.policy.budget;
            if !(run.status == RunStatus::Running
                || (run.status == RunStatus::Planning && run.config.policy.continuation.is_some()))
                || !budget.is_set()
            {
                continue;
            }
            let (tokens, usd) = self.ledger.lock().await.spent(run.id);
            let minutes = (Utc::now() - run.created_at).num_minutes().max(0) as u64;
            let percents = [
                (
                    "tokens",
                    budget.max_tokens.map(|max| pct(tokens as f64, max as f64)),
                ),
                ("usd", budget.max_usd.map(|max| pct(usd, max))),
                (
                    "minutes",
                    budget
                        .max_minutes
                        .map(|max| pct(minutes as f64, max as f64)),
                ),
            ];
            for (what, percent) in percents {
                let Some(percent) = percent else { continue };
                let Some(level) = LEVELS.iter().rev().find(|l| percent >= **l).copied() else {
                    continue;
                };
                // A raised cap starts over: report again once it is crossed anew.
                let key = (run.id, what, level);
                let mut ledger = self.ledger.lock().await;
                if level == 100 {
                    ledger.reported.retain(|k| !(k.0 == run.id && k.1 == what));
                } else if !ledger.reported.insert(key) {
                    continue;
                }
                drop(ledger);
                self.orchestrator
                    .budget_reached(run.id, what, percent)
                    .await?;
            }
        }
        Ok(())
    }
}

fn pct(used: f64, max: f64) -> u32 {
    if max <= 0.0 {
        return 100;
    }
    ((used / max) * 100.0).floor().min(999.0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossagents_core::{EVENT_SCHEMA_VERSION, TokenUsage};

    fn ev(kind: EventKind) -> Event {
        Event {
            seq: 1,
            id: uuid::Uuid::now_v7(),
            ts: Utc::now(),
            schema: EVENT_SCHEMA_VERSION,
            kind,
        }
    }

    #[test]
    fn sums_sessions_per_run() {
        let (agent, run) = (AgentId::new(), RunId::new());
        let mut ledger = Ledger::default();
        let usage = |total| EventKind::AgentUsage {
            id: agent,
            used: 0,
            size: 0,
            cost_usd: Some(total as f64 / 1000.0),
            tokens: Some(TokenUsage {
                total,
                ..TokenUsage::default()
            }),
        };
        ledger.apply(&ev(EventKind::TaskStatusChanged {
            run_id: run,
            id: crossagents_core::TaskId::new(),
            key: "a".into(),
            from: crossagents_core::TaskStatus::Pending,
            to: crossagents_core::TaskStatus::Running,
            assignee: Some(agent),
        }));
        ledger.apply(&ev(usage(100)));
        ledger.apply(&ev(usage(300))); // cumulative within the session
        ledger.apply(&ev(EventKind::AgentStatusChanged {
            id: agent,
            from: AgentStatus::Stopped,
            to: AgentStatus::Starting,
        }));
        ledger.apply(&ev(usage(50)));
        let (tokens, usd) = ledger.spent(run);
        assert_eq!(tokens, 350);
        assert!((usd - 0.35).abs() < 1e-9);
        assert_eq!(ledger.spent(RunId::new()), (0, 0.0));
        assert_eq!(pct(80.0, 100.0), 80);
    }
}
