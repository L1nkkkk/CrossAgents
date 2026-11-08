//! Cost and context usage, computed from the event log (docs/tasks/M4.md §2).
//!
//! ACP reports a session's cumulative cost; the cost of a session is the last value it
//! reported, and a new session starts when the agent goes to `starting`.

use std::collections::{BTreeMap, HashMap, HashSet};

use crossagents_api_types::{UsageGroup, UsageResponse, UsageRow};
use crossagents_core::{
    AgentId, AgentStatus, CoreError, EventKind, Role, RunId, Store, TokenUsage,
};
use std::path::Path;

#[derive(serde::Deserialize)]
struct Price {
    input: f64,
    cached_input: f64,
    cache_write_input: f64,
    output: f64,
}
type Prices = HashMap<String, Price>;

fn prices(home: &Path) -> Prices {
    std::fs::read_to_string(home.join("prices.toml"))
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

const PAGE: u32 = 1000;

#[derive(Default)]
struct AgentUsage {
    name: String,
    role: Option<Role>,
    model: Option<String>,
    run: Option<RunId>,
    sessions: u32,
    closed_cost: f64,
    session_cost: Option<f64>,
    reported: bool,
    peak_context: u64,
    tokens: TokenUsage,
    cache_write: u64,
    requests: Option<u64>,
    requests_known: bool,
    estimate: f64,
    estimate_known: bool,
    saw_tokens: bool,
    session_has_tokens: bool,
    missing_session: bool,
}

impl AgentUsage {
    fn close_session(&mut self) {
        self.closed_cost += self.session_cost.take().unwrap_or(0.0);
        if self.sessions > 0 && !self.session_has_tokens {
            self.missing_session = true;
        }
        self.session_has_tokens = false;
        self.tokens = TokenUsage::default();
    }
}

fn role_name(role: Option<Role>) -> String {
    match role {
        Some(Role::Planner) => "planner",
        Some(Role::Worker) => "worker",
        Some(Role::Reviewer) => "reviewer",
        None => "unknown",
    }
    .to_owned()
}

pub async fn compute(
    store: &dyn Store,
    by: UsageGroup,
    home: &Path,
) -> Result<UsageResponse, CoreError> {
    let prices = prices(home);
    let mut agents: HashMap<AgentId, AgentUsage> = HashMap::new();
    let mut after = 0;
    loop {
        let page = store.events_since(after, PAGE).await?;
        let Some(last) = page.last() else { break };
        after = last.seq;
        for event in page {
            match event.kind {
                EventKind::AgentCreated { agent } => {
                    let usage = agents.entry(agent.id).or_default();
                    usage.name = agent.name;
                    usage.role = Some(agent.role);
                    usage.model = agent.model;
                }
                EventKind::AgentUpdated { agent } => {
                    let usage = agents.entry(agent.id).or_default();
                    usage.name = agent.name;
                    usage.model = agent.model;
                }
                EventKind::AgentStatusChanged {
                    id,
                    to: AgentStatus::Starting,
                    ..
                } => {
                    let usage = agents.entry(id).or_default();
                    usage.close_session();
                    usage.sessions += 1;
                }
                EventKind::AgentModelChanged { id, model, .. } => {
                    agents.entry(id).or_default().model = model;
                }
                EventKind::AgentUsage {
                    id,
                    size: _,
                    used,
                    cost_usd,
                    tokens,
                    ..
                } => {
                    let usage = agents.entry(id).or_default();
                    usage.peak_context = usage.peak_context.max(used);
                    if let Some(tokens) = tokens {
                        let previous = &usage.tokens;
                        let input = tokens.input.saturating_sub(previous.input);
                        let cached = tokens.cached_input.saturating_sub(previous.cached_input);
                        let write = tokens
                            .cache_write_input
                            .saturating_sub(previous.cache_write_input);
                        let output = tokens.output.saturating_sub(previous.output);
                        usage.cache_write += write;
                        let first = !usage.saw_tokens;
                        usage.saw_tokens = true;
                        usage.session_has_tokens = true;
                        let delta_requests = tokens
                            .requests
                            .map(|n| n.saturating_sub(previous.requests.unwrap_or(0)));
                        if first {
                            usage.requests_known = true;
                            usage.estimate_known = true;
                        }
                        if let Some(n) = delta_requests {
                            usage.requests = Some(usage.requests.unwrap_or(0) + n);
                        } else {
                            usage.requests_known = false;
                        }
                        if let Some(price) = usage.model.as_ref().and_then(|m| prices.get(m)) {
                            usage.estimate += ((input.saturating_sub(cached).saturating_sub(write)
                                as f64
                                * price.input)
                                + cached as f64 * price.cached_input
                                + write as f64 * price.cache_write_input
                                + output as f64 * price.output)
                                / 1_000_000.0;
                        } else {
                            usage.estimate_known = false;
                        }
                        usage.tokens = tokens;
                    }
                    if let Some(cost) = cost_usd {
                        usage.session_cost = Some(cost);
                        usage.reported = true;
                    }
                }
                EventKind::RunCreated { run } => {
                    agents.entry(run.planner).or_default().run = Some(run.id);
                }
                EventKind::TaskStatusChanged {
                    run_id,
                    assignee: Some(agent),
                    ..
                } => {
                    agents.entry(agent).or_default().run = Some(run_id);
                }
                _ => {}
            }
        }
    }

    let mut rows: BTreeMap<String, (UsageRow, HashSet<AgentId>)> = BTreeMap::new();
    let mut total = 0.0;
    for (id, mut usage) in agents {
        usage.close_session();
        if usage.sessions == 0 && !usage.reported {
            continue; // never ran
        }
        let key = match by {
            UsageGroup::Role => role_name(usage.role),
            UsageGroup::Model => usage.model.clone().unwrap_or_else(|| "default".into()),
            UsageGroup::Run => usage
                .run
                .map_or_else(|| "(no run)".into(), |r| r.to_string()),
            UsageGroup::Agent => {
                if usage.name.is_empty() {
                    id.to_string()
                } else {
                    usage.name.clone()
                }
            }
        };
        let (row, ids) = rows.entry(key.clone()).or_insert_with(|| {
            (
                UsageRow {
                    key,
                    agents: 0,
                    sessions: 0,
                    cost_usd: 0.0,
                    cost_reported: false,
                    estimated_cost_usd: Some(0.0),
                    cache_write_input: 0,
                    requests: Some(0),
                    peak_context: 0,
                },
                HashSet::new(),
            )
        });
        if ids.insert(id) {
            row.agents += 1;
        }
        row.sessions += usage.sessions;
        row.cost_usd += usage.closed_cost;
        row.cost_reported |= usage.reported;
        row.cache_write_input += usage.cache_write;
        row.requests = row
            .requests
            .zip(if usage.requests_known && !usage.missing_session {
                usage.requests
            } else {
                None
            })
            .map(|(a, b)| a + b);
        row.estimated_cost_usd = row
            .estimated_cost_usd
            .zip(
                if usage.saw_tokens && usage.estimate_known && !usage.missing_session {
                    Some(usage.estimate)
                } else {
                    None
                },
            )
            .map(|(a, b)| a + b);
        row.peak_context = row.peak_context.max(usage.peak_context);
        total += usage.closed_cost;
    }
    Ok(UsageResponse {
        by,
        rows: rows.into_values().map(|(row, _)| row).collect(),
        total_cost_usd: total,
    })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use crossagents_core::{Agent, AgentStatus};
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

    #[tokio::test]
    async fn sums_last_cost_per_session() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        let planner = agent("boss", Role::Planner, "opus");
        let worker = agent("w1", Role::Worker, "haiku");
        let push = |kind| {
            let store = &store;
            async move { store.append_event(kind).await.unwrap() }
        };
        for a in [&planner, &worker] {
            push(EventKind::AgentCreated { agent: a.clone() }).await;
        }
        let start = |id| EventKind::AgentStatusChanged {
            id,
            from: AgentStatus::Stopped,
            to: AgentStatus::Starting,
        };
        let cost = |id, used, c| EventKind::AgentUsage {
            id,
            used,
            size: 1000,
            cost_usd: Some(c),
            tokens: None,
        };
        push(start(planner.id)).await;
        push(cost(planner.id, 100, 0.50)).await;
        push(cost(planner.id, 300, 1.25)).await; // cumulative within the session
        push(start(worker.id)).await;
        push(cost(worker.id, 50, 0.02)).await;
        push(start(worker.id)).await; // a second session starts from zero
        push(cost(worker.id, 70, 0.03)).await;

        let dir = tempfile::tempdir().unwrap();
        let by_role = compute(&store, UsageGroup::Role, dir.path()).await.unwrap();
        let planner_row = by_role.rows.iter().find(|r| r.key == "planner").unwrap();
        let worker_row = by_role.rows.iter().find(|r| r.key == "worker").unwrap();
        assert!((planner_row.cost_usd - 1.25).abs() < 1e-9);
        assert!((worker_row.cost_usd - 0.05).abs() < 1e-9);
        assert_eq!(worker_row.sessions, 2);
        assert_eq!(planner_row.peak_context, 300);
        assert!((by_role.total_cost_usd - 1.30).abs() < 1e-9);

        let by_model = compute(&store, UsageGroup::Model, dir.path())
            .await
            .unwrap();
        let keys: Vec<&str> = by_model.rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["haiku", "opus"]);
    }

    #[tokio::test]
    async fn estimates_only_with_model_prices_and_counts_session_deltas() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        let a = agent("priced", Role::Worker, "example/model");
        store
            .append_event(EventKind::AgentCreated { agent: a.clone() })
            .await
            .unwrap();
        store
            .append_event(EventKind::AgentStatusChanged {
                id: a.id,
                from: AgentStatus::Stopped,
                to: AgentStatus::Starting,
            })
            .await
            .unwrap();
        let counts = TokenUsage {
            input: 1_000_000,
            cached_input: 200_000,
            cache_write_input: 100_000,
            output: 100_000,
            total: 1_100_000,
            requests: Some(2),
            ..Default::default()
        };
        for _ in 0..2 {
            store
                .append_event(EventKind::AgentUsage {
                    id: a.id,
                    used: 0,
                    size: 0,
                    cost_usd: Some(9.0),
                    tokens: Some(counts.clone()),
                })
                .await
                .unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let unknown = compute(&store, UsageGroup::Agent, dir.path())
            .await
            .unwrap();
        assert_eq!(unknown.rows[0].estimated_cost_usd, None);
        std::fs::write(dir.path().join("prices.toml"), "[\"example/model\"]\ninput = 2.0\ncached_input = 1.0\ncache_write_input = 3.0\noutput = 4.0\n").unwrap();
        let priced = compute(&store, UsageGroup::Agent, dir.path())
            .await
            .unwrap();
        let row = &priced.rows[0];
        assert!((row.estimated_cost_usd.unwrap() - 2.3).abs() < 1e-9);
        assert_eq!(row.cache_write_input, 100_000);
        assert_eq!(row.requests, Some(2));
        assert_eq!(row.cost_usd, 9.0);
    }
}
