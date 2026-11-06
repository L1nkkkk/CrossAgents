//! What a run spent, split by agent and model (docs/research/multica.md §十三).
//!
//! Every `agent.usage` event reports a session's cumulative cost and token counts, so the
//! difference from the one before is what that stretch cost; it is put down to the agent and
//! to the model it ran on at that moment. A session that starts again (`starting`) counts
//! from zero, as in [`crate::usage`].
//!
//! A conversation can work in more than one run over its life. What a stretch belongs to is
//! the run the agent was last given work in — the lead by `run.created`, a worker by the task
//! it was assigned — so a reused session's spend lands where it was spent.

use std::collections::HashMap;

use crossagents_api_types::{RunCostResponse, RunCostSlice};
use crossagents_core::{AgentId, AgentStatus, CoreError, EventKind, RunId, Store};

const PAGE: u32 = 1000;
/// What an agent that never named a model is put down as, like [`crate::usage`].
const DEFAULT_MODEL: &str = "default";

/// An agent's running figures: its name and model now, the run it works in, and the
/// cumulative cost and tokens of the session it is in.
#[derive(Default)]
struct AgentUse {
    name: String,
    model: String,
    run: Option<RunId>,
    cost: f64,
    cached: u64,
    new_input: u64,
    output: u64,
}

impl AgentUse {
    fn model(&self) -> &str {
        if self.model.is_empty() {
            DEFAULT_MODEL
        } else {
            &self.model
        }
    }

    /// A new session starts from zero (the reported counts are per session).
    fn restart(&mut self) {
        self.cost = 0.0;
        self.cached = 0;
        self.new_input = 0;
        self.output = 0;
    }
}

/// One agent on one model.
struct Slice {
    name: String,
    cached: u64,
    new_input: u64,
    output: u64,
    cost: Option<f64>,
}

impl Slice {
    fn tokens(&self) -> u64 {
        self.cached + self.new_input + self.output
    }
}

/// Inside one agent: the model that cost most first, then the one with most tokens.
fn by_spend(a: &Slice, model_a: &str, b: &Slice, model_b: &str) -> std::cmp::Ordering {
    b.cost
        .unwrap_or(0.0)
        .total_cmp(&a.cost.unwrap_or(0.0))
        .then_with(|| b.tokens().cmp(&a.tokens()))
        .then_with(|| model_a.cmp(model_b))
}

/// Figures the run's slices out of the event log.
pub async fn compute(store: &dyn Store, run: RunId) -> Result<RunCostResponse, CoreError> {
    let mut agents: HashMap<AgentId, AgentUse> = HashMap::new();
    let mut slices: HashMap<(AgentId, String), Slice> = HashMap::new();

    let mut after = 0;
    loop {
        let page = store.events_since(after, PAGE).await?;
        let Some(last) = page.last() else { break };
        after = last.seq;
        for event in page {
            match event.kind {
                EventKind::AgentCreated { agent } => {
                    let use_ = agents.entry(agent.id).or_default();
                    use_.name = agent.name;
                    if use_.model.is_empty() {
                        use_.model = agent.model.unwrap_or_default();
                    }
                }
                EventKind::AgentUpdated { agent } => {
                    let use_ = agents.entry(agent.id).or_default();
                    use_.name = agent.name;
                    use_.model = agent.model.unwrap_or_default();
                }
                EventKind::AgentModelChanged { id, model, .. } => {
                    agents.entry(id).or_default().model = model.unwrap_or_default();
                }
                EventKind::AgentStatusChanged {
                    id,
                    to: AgentStatus::Starting,
                    ..
                } => {
                    agents.entry(id).or_default().restart();
                }
                EventKind::RunCreated { run: created } => {
                    agents.entry(created.planner).or_default().run = Some(created.id);
                }
                EventKind::TaskStatusChanged {
                    run_id,
                    assignee: Some(agent),
                    ..
                } => {
                    agents.entry(agent).or_default().run = Some(run_id);
                }
                EventKind::TaskHandedOff { run_id, handoff } => {
                    agents.entry(handoff.to).or_default().run = Some(run_id);
                }
                EventKind::AgentUsage {
                    id,
                    cost_usd,
                    tokens,
                    ..
                } => {
                    let Some(agent) = agents.get_mut(&id) else {
                        continue;
                    };
                    // Not this run's work, or not attributed to any run yet.
                    if agent.run != Some(run) {
                        continue;
                    }
                    let cost = cost_usd.map(|reported| {
                        let delta = (reported - agent.cost).max(0.0);
                        agent.cost = reported;
                        delta
                    });
                    let (mut cached, mut new_input, mut output) = (0, 0, 0);
                    if let Some(counts) = tokens {
                        let now_new = counts.input.saturating_sub(counts.cached_input);
                        cached = counts.cached_input.saturating_sub(agent.cached);
                        new_input = now_new.saturating_sub(agent.new_input);
                        output = counts.output.saturating_sub(agent.output);
                        agent.cached = counts.cached_input;
                        agent.new_input = now_new;
                        agent.output = counts.output;
                    }
                    if cost.is_none() && cached + new_input + output == 0 {
                        continue;
                    }
                    let slice = slices
                        .entry((id, agent.model().to_owned()))
                        .or_insert_with(|| Slice {
                            name: agent.name.clone(),
                            cached: 0,
                            new_input: 0,
                            output: 0,
                            cost: None,
                        });
                    if !agent.name.is_empty() {
                        slice.name.clone_from(&agent.name);
                    }
                    slice.cached += cached;
                    slice.new_input += new_input;
                    slice.output += output;
                    if let Some(delta) = cost {
                        slice.cost = Some(slice.cost.unwrap_or(0.0) + delta);
                    }
                }
                _ => {}
            }
        }
    }

    // What the agent spent over the run, to put the agents in order.
    let mut agent_totals: HashMap<AgentId, f64> = HashMap::new();
    for ((agent, _), slice) in &slices {
        if let Some(cost) = slice.cost {
            *agent_totals.entry(*agent).or_default() += cost;
        }
    }
    let total_of = |id: &AgentId| agent_totals.get(id).copied().unwrap_or(0.0);
    let mut rows: Vec<(AgentId, String, Slice)> = slices
        .into_iter()
        .map(|((agent, model), slice)| (agent, model, slice))
        .collect();
    rows.sort_by(|(agent_a, model_a, a), (agent_b, model_b, b)| {
        total_of(agent_b)
            .total_cmp(&total_of(agent_a))
            .then_with(|| b.tokens().cmp(&a.tokens()))
            .then_with(|| agent_a.to_string().cmp(&agent_b.to_string()))
            .then_with(|| by_spend(a, model_a, b, model_b))
    });
    // One agent's slices stand together now; order each agent's own models by spend.
    let mut sorted: Vec<(AgentId, String, Slice)> = Vec::with_capacity(rows.len());
    let mut group: Vec<(AgentId, String, Slice)> = Vec::new();
    for row in rows {
        if group.first().is_some_and(|(agent, _, _)| *agent != row.0) {
            group.sort_by(|(_, model_a, a), (_, model_b, b)| by_spend(a, model_a, b, model_b));
            sorted.append(&mut group);
        }
        group.push(row);
    }
    group.sort_by(|(_, model_a, a), (_, model_b, b)| by_spend(a, model_a, b, model_b));
    sorted.append(&mut group);

    let tokens = sorted.iter().map(|(_, _, slice)| slice.tokens()).sum();
    let mut cost_reported = false;
    let mut total = 0.0;
    let slices: Vec<RunCostSlice> = sorted
        .into_iter()
        .map(|(agent, model, slice)| {
            cost_reported |= slice.cost.is_some();
            total += slice.cost.unwrap_or(0.0);
            RunCostSlice {
                agent,
                name: slice.name,
                model,
                cached_input: slice.cached,
                new_input: slice.new_input,
                output: slice.output,
                cost_usd: slice.cost,
            }
        })
        .collect();

    let mut notes = Vec::new();
    if slices.is_empty() {
        notes.push("这个 run 还没有 agent 上报用量。".to_owned());
    } else {
        if !cost_reported {
            notes.push("没有 agent 上报金额：分段按 token 计算，悬停看 token 数。".to_owned());
        }
        notes.push(
            "金额和 token 是各 agent 自己上报的（会话内累计的差值），按上报当时的模型记账。"
                .to_owned(),
        );
    }

    Ok(RunCostResponse {
        run_id: run,
        slices,
        tokens,
        cost_usd: cost_reported.then_some(total),
        notes,
    })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use crossagents_core::{Agent, AgentStatus, Role, RunConfig, RunPolicy, RunStatus, TokenUsage};
    use crossagents_store_sqlite::SqliteStore;

    use super::*;

    fn agent(name: &str, role: Role, model: &str) -> Agent {
        Agent {
            id: AgentId::new(),
            name_source: crossagents_core::NameSource::Chosen,
            topic: None,
            archived_at: None,
            home: std::path::PathBuf::new(),
            name: name.into(),
            plugin: "mock".into(),
            driver: None,
            model: Some(model.into()),
            role,
            cwd: ".".into(),
            config: serde_json::Value::Null,
            status: AgentStatus::Stopped,
            created_at: Utc::now(),
            profile: Default::default(),
        }
    }

    fn run_of(id: RunId, planner: AgentId, worker: AgentId) -> crossagents_core::Run {
        crossagents_core::Run {
            origin_chat_id: None,
            work_id: None,
            continuation: None,
            id,
            title: "OAuth 登录".into(),
            request: "加上 OAuth".into(),
            planner,
            status: RunStatus::Running,
            config: RunConfig {
                worker,
                escalate: None,
                reviewer: None,
                auto_approve: false,
                max_parallel: 3,
                max_attempts: 2,
                team: vec![worker],
                policy: RunPolicy::default(),
            },
            repo: std::path::PathBuf::from("."),
            base_branch: "main".into(),
            branch: "run/x".into(),
            in_place: false,
            mode: None,
            note: None,
            plan: Vec::new(),
            lead_note: None,
            landed: None,
            summary: None,
            checks: Vec::new(),
            checks_state: None,
            checks_snapshot: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn tokens(input: u64, cached: u64, output: u64) -> TokenUsage {
        TokenUsage {
            input,
            cached_input: cached,
            output,
            reasoning_output: 0,
            total: input + output,
        }
    }

    #[tokio::test]
    async fn splits_a_runs_cost_by_agent_and_model() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        let planner = agent("boss", Role::Planner, "opus");
        let worker = agent("w1", Role::Worker, "haiku");
        let stranger = agent("w2", Role::Worker, "haiku");
        let run = RunId::new();
        let other = RunId::new();
        let push = |kind| {
            let store = &store;
            async move { store.append_event(kind).await.unwrap() }
        };
        for a in [&planner, &worker, &stranger] {
            push(EventKind::AgentCreated { agent: a.clone() }).await;
        }
        push(EventKind::RunCreated {
            run: Box::new(run_of(run, planner.id, worker.id)),
        })
        .await;

        let start = |id| EventKind::AgentStatusChanged {
            id,
            from: AgentStatus::Stopped,
            to: AgentStatus::Starting,
        };
        let usage = |id, cost: Option<f64>, counts: Option<TokenUsage>| EventKind::AgentUsage {
            id,
            used: 100,
            size: 1000,
            cost_usd: cost,
            tokens: counts,
        };
        // The lead: two reports, the second cumulative.
        push(start(planner.id)).await;
        push(usage(planner.id, Some(0.10), Some(tokens(100, 40, 10)))).await;
        push(usage(planner.id, Some(0.30), Some(tokens(300, 140, 30)))).await;
        // The worker takes a task in this run, then switches model mid-session.
        push(EventKind::TaskStatusChanged {
            run_id: run,
            id: crossagents_core::TaskId::new(),
            key: "a".into(),
            from: crossagents_core::TaskStatus::Pending,
            to: crossagents_core::TaskStatus::Running,
            assignee: Some(worker.id),
        })
        .await;
        push(start(worker.id)).await;
        push(usage(worker.id, Some(0.02), Some(tokens(200, 0, 20)))).await;
        push(EventKind::AgentModelChanged {
            id: worker.id,
            model: Some("sonnet".into()),
            effort: None,
            from_model: Some("haiku".into()),
            from_effort: None,
        })
        .await;
        push(usage(worker.id, Some(0.50), Some(tokens(1200, 600, 140)))).await;
        // Someone else's work belongs to another run.
        push(EventKind::TaskStatusChanged {
            run_id: other,
            id: crossagents_core::TaskId::new(),
            key: "b".into(),
            from: crossagents_core::TaskStatus::Pending,
            to: crossagents_core::TaskStatus::Running,
            assignee: Some(stranger.id),
        })
        .await;
        push(usage(stranger.id, Some(9.0), Some(tokens(100, 0, 10)))).await;

        let cost = compute(&store, run).await.unwrap();
        assert_eq!(cost.run_id, run);
        let keys: Vec<(&str, &str)> = cost
            .slices
            .iter()
            .map(|s| (s.name.as_str(), s.model.as_str()))
            .collect();
        assert_eq!(keys, [("w1", "sonnet"), ("w1", "haiku"), ("boss", "opus")]);

        let sonnet = &cost.slices[0];
        assert!((sonnet.cost_usd.unwrap() - 0.48).abs() < 1e-9);
        assert_eq!(sonnet.cached_input, 600);
        assert_eq!(sonnet.new_input, 400);
        assert_eq!(sonnet.output, 120);

        let haiku = &cost.slices[1];
        assert!((haiku.cost_usd.unwrap() - 0.02).abs() < 1e-9);
        assert_eq!(haiku.cached_input, 0);
        assert_eq!(haiku.new_input, 200);
        assert_eq!(haiku.output, 20);

        let opus = &cost.slices[2];
        assert!((opus.cost_usd.unwrap() - 0.30).abs() < 1e-9);
        assert_eq!(opus.cached_input, 140);
        assert_eq!(opus.new_input, 160);
        assert_eq!(opus.output, 30);

        assert!((cost.cost_usd.unwrap() - 0.80).abs() < 1e-9);
        assert_eq!(cost.tokens, 220 + 1120 + 330);
        assert!(cost.notes.iter().any(|n| n.contains("会话内累计")));

        // Another run's figures, though the same log holds the 9.00 report.
        let other_cost = compute(&store, other).await.unwrap();
        assert_eq!(other_cost.slices.len(), 1);
        assert_eq!(other_cost.tokens, 110);
        assert!((other_cost.cost_usd.unwrap() - 9.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn a_run_with_nothing_reported_says_so() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        let cost = compute(&store, RunId::new()).await.unwrap();
        assert!(cost.slices.is_empty());
        assert_eq!(cost.cost_usd, None);
        assert_eq!(cost.tokens, 0);
        assert!(cost.notes.iter().any(|n| n.contains("还没有")));
    }
}
