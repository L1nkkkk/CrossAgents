//! What each task spent, and whether it is over its caps (`TaskBudget`): the task ledger.
//!
//! The ledger is replayed from the event log, as [`crate::run_cost`] and [`crate::usage`] are,
//! so a restart loses nothing. Every `agent.usage` report is put down to the task its agent was
//! last given work on (a worker by `task.status_changed`, a reviewer by `task.reviewer`), so a
//! task's spend keeps adding up across hand-overs, session restarts and rework: nothing resets
//! when a new agent picks the task up.
//!
//! Agents report cumulative figures per session. A session that starts again (`starting`)
//! counts from zero, as in [`crate::usage`]. Dollars are the agents' own reports (`cost_usd`)
//! and the price table's estimate; the two are kept apart, and neither is a hard cap.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use crossagents_core::{
    AgentId, AgentStatus, CoreError, Event, EventKind, RunId, Store, TaskBudget, TaskId,
    TaskStatus, TokenUsage,
};

use crate::usage::{Price, Prices};

/// What an agent is doing right now, for attribution.
#[derive(Default)]
struct AgentState {
    model: Option<String>,
    /// The task the agent was last given work on, or reviewed.
    task: Option<TaskId>,
    /// The current session's cumulative token counts and cost.
    tokens: TokenUsage,
    session_cost: f64,
    session_requests: Option<u64>,
}

/// Everything one task has spent so far.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TaskSpend {
    pub run: Option<RunId>,
    pub key: String,
    pub status: Option<TaskStatus>,
    /// Input that missed the prompt cache (cache writes not included).
    pub input: u64,
    pub cached_input: u64,
    pub cache_write_input: u64,
    pub output: u64,
    /// Total tokens as the agents report them (`tokens.total`).
    pub tokens: u64,
    /// Dollars the agents reported, summed over sessions.
    pub reported_usd: f64,
    /// Whether any agent reported a cost.
    pub cost_reported: bool,
    /// Dollars by the price table. Only meaningful while `unpriced` is empty.
    pub estimated_usd: f64,
    /// Models that spent without a price-table row.
    pub unpriced: BTreeSet<String>,
    /// Model requests, summed over the sessions that report them.
    pub requests: u64,
    /// Whether any session reported request counts.
    pub requests_seen: bool,
    /// Whether a session with usage reported no request count (the sum is then a lower bound).
    pub requests_missing: bool,
    /// Whether any usage was reported for the task.
    pub saw_usage: bool,
    /// Every agent that worked on or reviewed the task.
    pub agents: BTreeSet<AgentId>,
    /// Acceptance checks and commands run in the task's worktree, in milliseconds.
    pub check_ms: u64,
    pub started: Option<DateTime<Utc>>,
    pub ended: Option<DateTime<Utc>>,
    /// The last time anything moved on the task.
    pub last_progress: Option<DateTime<Utc>>,
}

/// One cap a task is over: what, how much it used, and the cap, in that cap's unit.
#[derive(Debug, Clone, PartialEq)]
pub struct Breach {
    pub what: &'static str,
    pub used: f64,
    pub max: f64,
}

/// The per-task ledger, replayed from events.
#[derive(Default)]
pub struct TaskLedger {
    prices: Prices,
    agents: HashMap<AgentId, AgentState>,
    tasks: BTreeMap<TaskId, TaskSpend>,
}

impl TaskLedger {
    /// An empty ledger that prices tokens with `prices` (the hub's price table).
    pub fn new(prices: Prices) -> Self {
        Self {
            prices,
            ..Self::default()
        }
    }

    fn task(&mut self, id: TaskId) -> &mut TaskSpend {
        self.tasks.entry(id).or_default()
    }

    pub fn apply(&mut self, event: &Event) {
        let at = event.ts;
        match &event.kind {
            EventKind::AgentCreated { agent } => {
                let state = self.agents.entry(agent.id).or_default();
                state.model.clone_from(&agent.model);
            }
            EventKind::AgentUpdated { agent } => {
                self.agents.entry(agent.id).or_default().model.clone_from(&agent.model);
            }
            EventKind::AgentModelChanged { id, model, .. } => {
                self.agents.entry(*id).or_default().model.clone_from(model);
            }
            EventKind::AgentStatusChanged {
                id,
                to: AgentStatus::Starting,
                ..
            } => {
                let state = self.agents.entry(*id).or_default();
                state.tokens = TokenUsage::default();
                state.session_cost = 0.0;
                state.session_requests = None;
            }
            EventKind::TaskStatusChanged {
                run_id,
                id,
                key,
                to,
                assignee,
                ..
            } => {
                let task = self.task(*id);
                task.run = Some(*run_id);
                task.key.clone_from(key);
                task.status = Some(*to);
                task.last_progress = Some(at);
                if *to == TaskStatus::Running && task.started.is_none() {
                    task.started = Some(at);
                }
                if to.is_terminal() {
                    task.ended = Some(at);
                }
                if let Some(agent) = assignee {
                    task.agents.insert(*agent);
                    self.agents.entry(*agent).or_default().task = Some(*id);
                }
            }
            EventKind::TaskReviewer {
                run_id,
                id,
                key,
                reviewer,
            } => {
                let task = self.task(*id);
                task.run = Some(*run_id);
                task.key.clone_from(key);
                task.agents.insert(*reviewer);
                task.last_progress = Some(at);
                self.agents.entry(*reviewer).or_default().task = Some(*id);
            }
            EventKind::TaskProgress { id, .. } => {
                self.task(*id).last_progress = Some(at);
            }
            EventKind::TaskChecked { id, .. } => {
                self.task(*id).last_progress = Some(at);
            }
            EventKind::TaskCheckTimed {
                run_id,
                id,
                key,
                duration_ms,
            } => {
                let task = self.task(*id);
                task.run = Some(*run_id);
                task.key.clone_from(key);
                task.check_ms += duration_ms;
                task.last_progress = Some(at);
            }
            EventKind::TaskCommand {
                id, duration_ms, ..
            } => {
                let task = self.task(*id);
                task.check_ms += duration_ms;
                task.last_progress = Some(at);
            }
            EventKind::AgentUsage {
                id,
                cost_usd,
                tokens,
                ..
            } => self.usage(*id, *cost_usd, tokens.as_ref(), at),
            _ => {}
        }
    }

    fn usage(&mut self, agent: AgentId, cost: Option<f64>, tokens: Option<&TokenUsage>, at: DateTime<Utc>) {
        let Some(state) = self.agents.get_mut(&agent) else {
            return;
        };
        let Some(task_id) = state.task else {
            return; // not on any task yet
        };
        let model = state.model.clone();
        let mut delta_cost = 0.0;
        let mut fresh = 0;
        let (mut cached, mut write, mut output, mut total) = (0, 0, 0, 0);
        let mut requests_delta = None;
        if let Some(counts) = tokens {
            let previous = std::mem::take(&mut state.tokens);
            let input = counts.input.saturating_sub(previous.input);
            cached = counts.cached_input.saturating_sub(previous.cached_input);
            write = counts.cache_write_input.saturating_sub(previous.cache_write_input);
            output = counts.output.saturating_sub(previous.output);
            total = counts.total.saturating_sub(previous.total);
            fresh = input.saturating_sub(cached).saturating_sub(write);
            requests_delta = match counts.requests {
                Some(now) => {
                    let before = state.session_requests.unwrap_or(0);
                    state.session_requests = Some(now);
                    Some(Some(now.saturating_sub(before)))
                }
                None => Some(None),
            };
            state.tokens = counts.clone();
        }
        if let Some(reported) = cost {
            delta_cost = (reported - state.session_cost).max(0.0);
            state.session_cost = reported;
        }
        let price: Option<&Price> = model.as_deref().and_then(|m| self.prices.get(m));
        let task = self.tasks.entry(task_id).or_default();
        task.last_progress = Some(at);
        if cost.is_some() {
            task.reported_usd += delta_cost;
            task.cost_reported = true;
        }
        if tokens.is_some() {
            task.saw_usage = true;
            task.input += fresh;
            task.cached_input += cached;
            task.cache_write_input += write;
            task.output += output;
            task.tokens += total;
            match requests_delta {
                Some(Some(delta)) => {
                    task.requests += delta;
                    task.requests_seen = true;
                }
                Some(None) => task.requests_missing = true,
                None => {}
            }
            match price {
                Some(price) => {
                    task.estimated_usd += (fresh as f64 * price.input
                        + cached as f64 * price.cached_input
                        + write as f64 * price.cache_write_input
                        + output as f64 * price.output)
                        / 1_000_000.0;
                }
                None if fresh + cached + write + output > 0 => {
                    task.unpriced
                        .insert(model.unwrap_or_else(|| "default".to_owned()));
                }
                None => {}
            }
        }
    }

    pub fn spend(&self, task: TaskId) -> Option<&TaskSpend> {
        self.tasks.get(&task)
    }

    /// Every task with any figures, by id.
    pub fn tasks(&self) -> impl Iterator<Item = (TaskId, &TaskSpend)> {
        self.tasks.iter().map(|(id, spend)| (*id, spend))
    }

    /// Minutes the task has been on, from its first start to its end, or to `now`.
    pub fn minutes(&self, task: TaskId, now: DateTime<Utc>) -> Option<f64> {
        let spend = self.tasks.get(&task)?;
        let started = spend.started?;
        let end = spend.ended.unwrap_or(now);
        Some((end - started).num_milliseconds().max(0) as f64 / 60_000.0)
    }

    /// Minutes since anything moved on a task that is being worked on, or `None` if it is not.
    pub fn idle_minutes(&self, task: TaskId, now: DateTime<Utc>) -> Option<f64> {
        let spend = self.tasks.get(&task)?;
        let working = matches!(
            spend.status,
            Some(TaskStatus::Running)
                | Some(TaskStatus::Retrying)
                | Some(TaskStatus::Verifying)
                | Some(TaskStatus::Reviewing)
        );
        if !working {
            return None;
        }
        let last = spend.last_progress?;
        Some((now - last).num_milliseconds().max(0) as f64 / 60_000.0)
    }

    /// The caps of `caps` the task is over now. Rework is not among them: the orchestrator
    /// counts it when it would send the work back. A cap whose figure is unknown is not held
    /// against the task (see [`Self::hints`]).
    pub fn breaches(&self, task: TaskId, caps: &TaskBudget, now: DateTime<Utc>) -> Vec<Breach> {
        let Some(spend) = self.tasks.get(&task) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut check = |what: &'static str, used: Option<f64>, max: Option<f64>| {
            if let (Some(used), Some(max)) = (used, max)
                && used >= max
            {
                out.push(Breach { what, used, max });
            }
        };
        let usd = (spend.saw_usage && spend.unpriced.is_empty()).then_some(spend.estimated_usd);
        check("usd", usd, caps.max_usd);
        check("tokens", spend.saw_usage.then_some(spend.tokens as f64), caps.max_tokens.map(|m| m as f64));
        let requests = (spend.requests_seen && !spend.requests_missing)
            .then_some(spend.requests as f64);
        check("requests", requests, caps.max_requests.map(|m| m as f64));
        check(
            "minutes",
            self.minutes(task, now),
            caps.max_minutes.map(|m| m as f64),
        );
        check(
            "idle",
            self.idle_minutes(task, now),
            caps.max_idle_minutes.map(|m| m as f64),
        );
        out
    }

    /// What a cap cannot check yet, for the lead to hear once: a dollar cap with an unpriced
    /// model, a request cap without request counts. Keyed like the breaches (`what`).
    pub fn hints(&self, task: TaskId, caps: &TaskBudget) -> Vec<(&'static str, String)> {
        let Some(spend) = self.tasks.get(&task) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if caps.max_usd.is_some() && !spend.unpriced.is_empty() {
            let models: Vec<&str> = spend.unpriced.iter().map(String::as_str).collect();
            out.push((
                "usd_unpriced",
                format!(
                    "max_usd 没有生效：{} 没有价格表里的价格，无法估算金额。上限只按估算金额判断，请补价格或改用 max_tokens。",
                    models.join("、")
                ),
            ));
        }
        if caps.max_requests.is_some() && !(spend.requests_seen && !spend.requests_missing) {
            out.push((
                "requests_unknown",
                "max_requests 没有生效：agent 没有报告请求次数。".to_owned(),
            ));
        }
        out
    }

    /// The whole run's tasks, replayed from the store's event log.
    pub async fn replay(store: &dyn Store, prices: Prices) -> Result<Self, CoreError> {
        let mut ledger = Self::new(prices);
        let mut after = 0;
        loop {
            let page = store.events_since(after, 1000).await?;
            let Some(last) = page.last() else { break };
            after = last.seq;
            for event in &page {
                ledger.apply(event);
            }
        }
        Ok(ledger)
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};
    use crossagents_core::{Agent, AgentStatus, EVENT_SCHEMA_VERSION, Role, TaskStatus};

    use super::*;

    fn ev(seq: u64, at: DateTime<Utc>, kind: EventKind) -> Event {
        Event {
            seq,
            id: uuid::Uuid::now_v7(),
            ts: at,
            schema: EVENT_SCHEMA_VERSION,
            kind,
        }
    }

    fn at(minute: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 9, 10, 0, 0).unwrap() + Duration::minutes(minute)
    }

    fn agent(model: &str) -> Agent {
        Agent {
            id: AgentId::new(),
            name_source: crossagents_core::NameSource::Chosen,
            topic: None,
            archived_at: None,
            home: std::path::PathBuf::new(),
            name: "w".into(),
            plugin: "mock".into(),
            driver: None,
            model: Some(model.into()),
            role: Role::Worker,
            cwd: ".".into(),
            config: serde_json::Value::Null,
            status: AgentStatus::Stopped,
            created_at: Utc::now(),
            profile: Default::default(),
        }
    }

    fn counts(total: u64, requests: Option<u64>) -> TokenUsage {
        TokenUsage {
            input: total / 2,
            cached_input: total / 4,
            cache_write_input: 10,
            output: total / 4,
            total,
            requests,
            ..TokenUsage::default()
        }
    }

    fn prices() -> Prices {
        let mut prices = Prices::new();
        prices.insert(
            "priced".into(),
            Price {
                input: 1_000_000.0,
                cached_input: 0.0,
                cache_write_input: 0.0,
                output: 0.0,
            },
        );
        prices
    }

    struct Log {
        seq: u64,
        events: Vec<Event>,
    }

    impl Log {
        fn new() -> Self {
            Self { seq: 0, events: Vec::new() }
        }
        fn push(&mut self, minute: i64, kind: EventKind) {
            self.seq += 1;
            self.events.push(ev(self.seq, at(minute), kind));
        }
    }

    fn assign(run: RunId, task: TaskId, to: AgentId) -> EventKind {
        EventKind::TaskStatusChanged {
            run_id: run,
            id: task,
            key: "a".into(),
            from: TaskStatus::Pending,
            to: TaskStatus::Running,
            assignee: Some(to),
        }
    }

    fn usage(id: AgentId, cost: Option<f64>, tokens: Option<TokenUsage>) -> EventKind {
        EventKind::AgentUsage {
            id,
            used: 0,
            size: 0,
            cost_usd: cost,
            tokens,
        }
    }

    #[test]
    fn spend_follows_the_task_across_agents_and_sessions_without_resetting() {
        let (run, task) = (RunId::new(), TaskId::new());
        let (first, second) = (agent("priced"), agent("priced"));
        let mut log = Log::new();
        log.push(0, EventKind::AgentCreated { agent: first.clone() });
        log.push(0, EventKind::AgentCreated { agent: second.clone() });
        log.push(1, assign(run, task, first.id));
        log.push(2, EventKind::AgentStatusChanged { id: first.id, from: AgentStatus::Stopped, to: AgentStatus::Starting });
        log.push(3, usage(first.id, Some(0.10), Some(counts(400, Some(2)))));
        log.push(4, usage(first.id, Some(0.25), Some(counts(1000, Some(5)))));
        // The first worker is replaced: the task's spend keeps adding up.
        log.push(5, assign(run, task, second.id));
        log.push(6, EventKind::AgentStatusChanged { id: second.id, from: AgentStatus::Stopped, to: AgentStatus::Starting });
        log.push(7, usage(second.id, Some(0.05), Some(counts(200, Some(1)))));
        // The first worker's session restarts: its counts start from zero again.
        log.push(8, EventKind::AgentStatusChanged { id: first.id, from: AgentStatus::Stopped, to: AgentStatus::Starting });
        log.push(9, usage(first.id, Some(0.03), Some(counts(100, None))));

        let ledger = {
            let mut ledger = TaskLedger::new(prices());
            for event in &log.events {
                ledger.apply(event);
            }
            ledger
        };
        let spend = ledger.spend(task).unwrap();
        // 1000 tokens from the first worker's first session, 200 from the second, 100 more after
        // the restart: nothing is lost or reset.
        assert_eq!(spend.tokens, 1000 + 200 + 100);
        assert!((spend.reported_usd - (0.25 + 0.05 + 0.03)).abs() < 1e-9);
        assert_eq!(spend.requests, 5 + 1, "the restarted session reported none");
        assert!(spend.requests_missing, "a session without counts makes the sum a lower bound");
        assert_eq!(spend.agents.len(), 2);
        assert!(spend.unpriced.is_empty());
        assert!(spend.estimated_usd > 0.0, "priced tokens are estimated");
    }

    #[test]
    fn unpriced_models_do_not_make_a_usd_cap_fire() {
        let (run, task) = (RunId::new(), TaskId::new());
        let worker = agent("unlisted");
        let mut log = Log::new();
        log.push(0, EventKind::AgentCreated { agent: worker.clone() });
        log.push(1, assign(run, task, worker.id));
        log.push(2, usage(worker.id, Some(99.0), Some(counts(5_000_000, Some(9)))));
        let mut ledger = TaskLedger::new(prices());
        for event in &log.events {
            ledger.apply(event);
        }
        let caps = TaskBudget {
            max_usd: Some(1.0),
            ..TaskBudget::default()
        };
        // The agent's own 99.00 is not what the cap measures: no price, no check, and a hint.
        assert!(ledger.breaches(task, &caps, at(3)).is_empty());
        let hints = ledger.hints(task, &caps);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].0, "usd_unpriced");
        assert!(hints[0].1.contains("unlisted"), "{:?}", hints);
    }

    #[test]
    fn unknown_request_counts_do_not_fire_the_request_cap() {
        let (run, task) = (RunId::new(), TaskId::new());
        let worker = agent("priced");
        let mut log = Log::new();
        log.push(0, EventKind::AgentCreated { agent: worker.clone() });
        log.push(1, assign(run, task, worker.id));
        log.push(2, usage(worker.id, None, Some(counts(100, None))));
        let mut ledger = TaskLedger::new(prices());
        for event in &log.events {
            ledger.apply(event);
        }
        let caps = TaskBudget {
            max_requests: Some(1),
            ..TaskBudget::default()
        };
        assert!(ledger.breaches(task, &caps, at(3)).is_empty());
        assert_eq!(ledger.hints(task, &caps)[0].0, "requests_unknown");
    }

    #[test]
    fn caps_on_tokens_minutes_and_idle_time_are_reported_with_their_figures() {
        let (run, task) = (RunId::new(), TaskId::new());
        let worker = agent("priced");
        let mut log = Log::new();
        log.push(0, EventKind::AgentCreated { agent: worker.clone() });
        log.push(1, assign(run, task, worker.id));
        log.push(2, EventKind::AgentStatusChanged { id: worker.id, from: AgentStatus::Stopped, to: AgentStatus::Starting });
        log.push(4, usage(worker.id, Some(0.5), Some(counts(600, Some(3)))));
        log.push(5, EventKind::TaskCheckTimed { run_id: run, id: task, key: "a".into(), duration_ms: 90_000 });
        let mut ledger = TaskLedger::new(prices());
        for event in &log.events {
            ledger.apply(event);
        }
        let caps = TaskBudget {
            max_tokens: Some(500),
            max_minutes: Some(10),
            max_idle_minutes: Some(20),
            max_requests: Some(3),
            ..TaskBudget::default()
        };
        // At minute 40 the task is 39 minutes old, 35 minutes since its last report.
        let breaches = ledger.breaches(task, &caps, at(40));
        let names: Vec<&str> = breaches.iter().map(|b| b.what).collect();
        assert_eq!(names, ["tokens", "requests", "minutes", "idle"]);
        assert_eq!(breaches[0].used, 600.0);
        assert!(breaches[2].used >= 39.0, "{breaches:?}");
        assert_eq!(ledger.spend(task).unwrap().check_ms, 90_000);
        // Under every cap, nothing fires.
        let loose = TaskBudget {
            max_tokens: Some(10_000),
            ..TaskBudget::default()
        };
        assert!(ledger.breaches(task, &loose, at(5)).is_empty());
    }

    #[test]
    fn a_finished_task_is_not_idle_and_its_minutes_stop_at_its_end() {
        let (run, task) = (RunId::new(), TaskId::new());
        let worker = agent("priced");
        let mut log = Log::new();
        log.push(0, EventKind::AgentCreated { agent: worker.clone() });
        log.push(1, assign(run, task, worker.id));
        log.push(
            6,
            EventKind::TaskStatusChanged {
                run_id: run,
                id: task,
                key: "a".into(),
                from: TaskStatus::Running,
                to: TaskStatus::Merged,
                assignee: Some(worker.id),
            },
        );
        let mut ledger = TaskLedger::new(prices());
        for event in &log.events {
            ledger.apply(event);
        }
        assert_eq!(ledger.idle_minutes(task, at(600)), None);
        assert_eq!(ledger.minutes(task, at(600)), Some(5.0));
    }
}
