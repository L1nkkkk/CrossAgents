//! Cost and context usage, computed from the event log (docs/tasks/M4.md §2).
//!
//! ACP reports a session's cumulative cost; the cost of a session is the last value it
//! reported, and a new session starts when the agent goes to `starting`.

use std::collections::{BTreeMap, HashMap, HashSet};

use crossagents_api_types::{UsageGroup, UsageResponse, UsageRow};
use crossagents_core::{AgentId, AgentStatus, CoreError, EventKind, Role, RunId, Store};

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
}

impl AgentUsage {
    fn close_session(&mut self) {
        self.closed_cost += self.session_cost.take().unwrap_or(0.0);
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

pub async fn compute(store: &dyn Store, by: UsageGroup) -> Result<UsageResponse, CoreError> {
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
                EventKind::AgentStatusChanged {
                    id,
                    to: AgentStatus::Starting,
                    ..
                } => {
                    let usage = agents.entry(id).or_default();
                    usage.close_session();
                    usage.sessions += 1;
                }
                EventKind::AgentUsage {
                    id,
                    size: _,
                    used,
                    cost_usd,
                    ..
                } => {
                    let usage = agents.entry(id).or_default();
                    usage.peak_context = usage.peak_context.max(used);
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
            name: name.into(),
            plugin: "mock".into(),
            driver: None,
            model: Some(model.into()),
            role,
            cwd: ".".into(),
            config: serde_json::Value::Null,
            status: AgentStatus::Stopped,
            created_at: Utc::now(),
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
        };
        push(start(planner.id)).await;
        push(cost(planner.id, 100, 0.50)).await;
        push(cost(planner.id, 300, 1.25)).await; // cumulative within the session
        push(start(worker.id)).await;
        push(cost(worker.id, 50, 0.02)).await;
        push(start(worker.id)).await; // a second session starts from zero
        push(cost(worker.id, 70, 0.03)).await;

        let by_role = compute(&store, UsageGroup::Role).await.unwrap();
        let planner_row = by_role.rows.iter().find(|r| r.key == "planner").unwrap();
        let worker_row = by_role.rows.iter().find(|r| r.key == "worker").unwrap();
        assert!((planner_row.cost_usd - 1.25).abs() < 1e-9);
        assert!((worker_row.cost_usd - 0.05).abs() < 1e-9);
        assert_eq!(worker_row.sessions, 2);
        assert_eq!(planner_row.peak_context, 300);
        assert!((by_role.total_cost_usd - 1.30).abs() < 1e-9);

        let by_model = compute(&store, UsageGroup::Model).await.unwrap();
        let keys: Vec<&str> = by_model.rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["haiku", "opus"]);
    }
}
