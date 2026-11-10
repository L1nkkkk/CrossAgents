//! Who an agent is now, instead of its deprecated role: what it is doing (its claim), which
//! teammate an execution session was cloned from, and what it has done well (its strengths).

use std::collections::HashMap;

use crossagents_core::{Agent, AgentId, AgentNow, CoreError, RunId, SessionOf, Store};

/// How many strengths an agent shows.
pub const MAX_STRENGTHS: usize = 3;

/// The relations of every agent in a list, each fact read once for the whole list.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Relations {
    pub now: HashMap<AgentId, AgentNow>,
    pub session_of: HashMap<AgentId, SessionOf>,
    pub strengths: HashMap<AgentId, Vec<String>>,
}

impl Relations {
    /// `agents`' relations: one claims query, one usage query, and a run and task lookup per
    /// claim (there are few at a time).
    pub async fn of(store: &dyn Store, agents: &[Agent]) -> Result<Self, CoreError> {
        let mut now = HashMap::new();
        let mut titles: HashMap<RunId, String> = HashMap::new();
        for claim in store.list_claims(None).await? {
            let run_title = match titles.get(&claim.run_id) {
                Some(title) => title.clone(),
                None => {
                    let title = match store.get_run(claim.run_id).await {
                        Ok(run) => run.title,
                        Err(_) => claim.run_id.to_string(),
                    };
                    titles.insert(claim.run_id, title.clone());
                    title
                }
            };
            let task_key = match claim.task_id {
                Some(id) => store.get_task(id).await.ok().map(|t| t.key),
                None => None,
            };
            now.insert(
                claim.agent_id,
                AgentNow {
                    kind: claim.role.into(),
                    run_id: claim.run_id,
                    run_title,
                    task_key,
                },
            );
        }
        Ok(Self {
            now,
            session_of: session_of(store, agents).await,
            strengths: strengths(store, agents).await?,
        })
    }

    pub fn strengths_of(&self, agent: AgentId) -> &[String] {
        self.strengths.get(&agent).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The three facts of one agent, as the MCP tools show them.
    pub fn json(&self, agent: AgentId) -> serde_json::Value {
        serde_json::json!({
            "now": self.now.get(&agent),
            "session_of": self.session_of.get(&agent),
            "strengths": self.strengths_of(agent),
        })
    }
}

/// Which teammate each execution session in `agents` was cloned from. Sessions from before
/// the hub recorded it have none.
pub async fn session_of(store: &dyn Store, agents: &[Agent]) -> HashMap<AgentId, SessionOf> {
    let by_id: HashMap<AgentId, &Agent> = agents.iter().map(|a| (a.id, a)).collect();
    let mut out = HashMap::new();
    for agent in agents {
        let Some(template) = agent.cloned_from() else {
            continue;
        };
        let name = match by_id.get(&template) {
            Some(t) => t.name.clone(),
            None => match store
                .get_agent(&crossagents_core::AgentKey::Id(template))
                .await
            {
                Ok(t) => t.name,
                Err(_) => continue,
            },
        };
        out.insert(
            agent.id,
            SessionOf {
                id: template,
                name,
                task_key: agent.cloned_for().map(str::to_owned),
            },
        );
    }
    out
}

/// Up to [`MAX_STRENGTHS`] expertise domains per agent, best first: the domains of entries
/// used on tasks that merged first try, by the agent or one of its execution sessions.
pub async fn strengths(
    store: &dyn Store,
    agents: &[Agent],
) -> Result<HashMap<AgentId, Vec<String>>, CoreError> {
    let root: HashMap<AgentId, AgentId> = agents
        .iter()
        .filter_map(|a| Some((a.id, a.cloned_from()?)))
        .collect();
    let mut counts: HashMap<AgentId, HashMap<String, u32>> = HashMap::new();
    for (agent, domain, n) in store.usage_merged_domains().await? {
        *counts
            .entry(agent)
            .or_default()
            .entry(domain.clone())
            .or_default() += n;
        if let Some(template) = root.get(&agent).filter(|t| **t != agent) {
            *counts
                .entry(*template)
                .or_default()
                .entry(domain)
                .or_default() += n;
        }
    }
    Ok(counts
        .into_iter()
        .map(|(agent, domains)| {
            let mut domains: Vec<(String, u32)> = domains.into_iter().collect();
            domains.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let best = domains
                .into_iter()
                .take(MAX_STRENGTHS)
                .map(|(d, _)| d)
                .collect();
            (agent, best)
        })
        .collect())
}

/// `config` for a new execution session of `template` started for `task_key`: it names the
/// root template (a clone of a clone points past the session in between) and the task, and
/// drops what the template's own session marks would otherwise carry over.
pub fn mark_session(config: &mut serde_json::Value, template: &Agent, task_key: Option<&str>) {
    use crossagents_core::model::{CLONED_FOR, CLONED_FROM};
    if !config.is_object() {
        *config = serde_json::json!({});
    }
    let root = template.cloned_from().unwrap_or(template.id);
    let map = config.as_object_mut().expect("an object");
    map.insert(CLONED_FROM.into(), root.to_string().into());
    match task_key {
        Some(key) => {
            map.insert(CLONED_FOR.into(), key.into());
        }
        None => {
            map.remove(CLONED_FOR);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use crossagents_core::{
        AgentClaim, ClaimRole, EntryKind, EntryStats, EntryStatus, ExpertiseEntry, NowKind,
        OwnerKind, TaskId, UsageOutcome, UsageRecord, UsageSource,
    };
    use crossagents_store_sqlite::SqliteStore;

    fn agent(name: &str) -> Agent {
        Agent {
            id: AgentId::new(),
            name_source: Default::default(),
            topic: None,
            archived_at: None,
            home: Default::default(),
            name: name.into(),
            plugin: "claude".into(),
            driver: None,
            model: None,
            role: Default::default(),
            cwd: ".".into(),
            config: serde_json::json!({}),
            status: crossagents_core::AgentStatus::Idle,
            created_at: Utc::now(),
            profile: Default::default(),
        }
    }

    fn entry(id: &str, domain: &str) -> ExpertiseEntry {
        let now = Utc::now();
        ExpertiseEntry {
            id: id.into(),
            owner_kind: OwnerKind::Person,
            owner_id: "me".into(),
            domain: domain.into(),
            kind: EntryKind::Judgment,
            title: id.into(),
            text: String::new(),
            example: None,
            applies_when: String::new(),
            evidence: 1,
            last_confirmed_at: None,
            status: EntryStatus::Active,
            superseded_by: None,
            stats: EntryStats::default(),
            sources: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn relations_say_what_each_agent_does_where_it_came_from_and_is_good_at() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        let ada = agent("Ada");
        let mut first = agent("w-1");
        mark_session(&mut first.config, &ada, Some("auth"));
        // A clone of the session still points at Ada.
        let mut second = agent("w-2");
        mark_session(&mut second.config, &first, None);
        let bob = agent("Bob");
        let agents = vec![ada.clone(), first.clone(), second.clone(), bob.clone()];
        for a in &agents {
            store.insert_agent(a).await.unwrap();
        }

        for (id, domain) in [
            ("e1", "rust"),
            ("e2", "gui"),
            ("e3", "sqlite"),
            ("e4", "docs"),
        ] {
            store.expertise_put(&entry(id, domain)).await.unwrap();
        }
        let mut n = 0;
        let mut used = async |by: &Agent, entry: &str, outcome: UsageOutcome| {
            let task = TaskId::new();
            n += 1;
            store
                .usage_record(&UsageRecord {
                    id: format!("u{n}"),
                    entry_id: entry.into(),
                    run_id: None,
                    task_id: Some(task),
                    agent: Some(by.id),
                    source: UsageSource::Declared,
                    outcome: None,
                    at: Utc::now(),
                })
                .await
                .unwrap();
            store.usage_set_outcome(task, outcome).await.unwrap();
        };
        // Ada did rust well twice (once herself, once through a session), gui once through
        // a session; reworked and dropped work counts for nothing.
        used(&ada, "e1", UsageOutcome::MergedFirstTry).await;
        used(&first, "e1", UsageOutcome::MergedFirstTry).await;
        used(&second, "e2", UsageOutcome::MergedFirstTry).await;
        used(&ada, "e3", UsageOutcome::Reworked).await;
        used(&ada, "e4", UsageOutcome::Dropped).await;

        let run = crossagents_core::RunId::new();
        store
            .claim_agent(&AgentClaim::new(
                first.id,
                run,
                None,
                ClaimRole::Reviewer,
                true,
            ))
            .await
            .unwrap();

        let rel = Relations::of(&store, &agents).await.unwrap();
        assert_eq!(rel.strengths_of(ada.id), ["rust", "gui"]);
        assert_eq!(rel.strengths_of(first.id), ["rust"]);
        assert_eq!(rel.strengths_of(second.id), ["gui"]);
        assert!(rel.strengths_of(bob.id).is_empty());

        let of = &rel.session_of[&first.id];
        assert_eq!((of.id, of.name.as_str()), (ada.id, "Ada"));
        assert_eq!(of.task_key.as_deref(), Some("auth"));
        let of = &rel.session_of[&second.id];
        assert_eq!((of.id, of.task_key.as_deref()), (ada.id, None));
        assert!(!rel.session_of.contains_key(&ada.id));

        let now = &rel.now[&first.id];
        assert_eq!(now.kind, NowKind::Reviewing);
        assert_eq!(now.run_id, run);
        assert!(!rel.now.contains_key(&ada.id));
        let json = rel.json(bob.id);
        assert!(json["now"].is_null() && json["session_of"].is_null());
        assert_eq!(json["strengths"], serde_json::json!([]));
    }
}
