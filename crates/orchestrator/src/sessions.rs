//! The sessions of a workspace, for whoever assigns work there (docs/tasks/S1.md §3.6): who
//! is idle, who is busy and where, who is archived, and what each one last did.

use std::fmt::Write as _;
use std::path::Path;

use chrono::{DateTime, Utc};
use crossagents_core::{Agent, AgentId, CoreError, Handoff};
use serde_json::{Value, json};

use crate::engine::Orchestrator;

/// How many sessions the lead's kickoff lists.
pub(crate) const KICKOFF_SESSIONS: usize = 12;

/// One session of the workspace.
pub(crate) struct Session {
    pub agent: Agent,
    /// `idle`, `busy` or `archived`.
    pub state: &'static str,
    /// What holds it, when busy.
    pub busy_with: Option<String>,
    /// Its latest execution: the run, the task and how it went.
    pub last: Option<String>,
    used: DateTime<Utc>,
}

impl Session {
    pub fn json(&self) -> Value {
        json!({
            "name": self.agent.name,
            "model": self.agent.model,
            "state": self.state,
            "busy_with": self.busy_with,
            "last_execution": self.last,
        })
    }
}

impl Orchestrator {
    /// The sessions living in `repo`, archived ones too, most recently used first; `except`
    /// (the asking lead) is left out.
    pub(crate) async fn workspace_sessions(
        &self,
        repo: &Path,
        except: Option<AgentId>,
    ) -> Result<Vec<Session>, CoreError> {
        // Every execution in the workspace, newest last.
        let mut cards: Vec<(String, Handoff)> = Vec::new();
        for run in self.store.list_runs().await? {
            if run.repo != repo {
                continue;
            }
            for task in self.store.list_tasks(run.id).await? {
                for card in task.handoffs {
                    cards.push((run.title.clone(), card));
                }
            }
        }
        cards.sort_by_key(|(_, c)| c.at);
        let mut sessions = Vec::new();
        for agent in self.store.list_agents().await? {
            if agent.home() != repo || Some(agent.id) == except {
                continue;
            }
            let claim = self.store.claim_of(agent.id).await?;
            let (state, busy_with) = match (&claim, agent.archived_at) {
                (_, Some(_)) => ("archived", None),
                (Some(claim), None) => ("busy", Some(self.held_by(&agent, claim).await)),
                (None, None) => ("idle", None),
            };
            let last = cards.iter().rev().find(|(_, c)| c.to == agent.id);
            let used = last.map_or(agent.created_at, |(_, c)| c.at);
            let last = last.map(|(run, c)| {
                let how = c.outcome.map_or("in progress", |o| o.word());
                format!("run \"{run}\" #{} ({how})", c.task_key)
            });
            sessions.push(Session {
                agent,
                state,
                busy_with,
                last,
                used,
            });
        }
        sessions.sort_by_key(|s| std::cmp::Reverse(s.used));
        Ok(sessions)
    }

    /// The kickoff's section on the workspace's sessions; empty when there are none.
    pub(crate) async fn sessions_section(&self, repo: &Path, lead: AgentId) -> String {
        let sessions = match self.workspace_sessions(repo, Some(lead)).await {
            Ok(sessions) => sessions,
            Err(err) => {
                tracing::warn!(error = %err, "cannot list the workspace's sessions");
                return String::new();
            }
        };
        if sessions.is_empty() {
            return String::new();
        }
        let mut out = String::from(
            "\nsessions in this workspace (by default a task gets a new execution session started from its teammate's settings, fresh: true; to have one of these work on it itself, with its conversation, name it as assignee with fresh: false, and restore: true for an archived one):",
        );
        for s in sessions.iter().take(KICKOFF_SESSIONS) {
            let model = s.agent.model.as_deref().unwrap_or(&s.agent.plugin);
            let state = match (&s.busy_with, s.state) {
                (Some(what), _) => what.clone(),
                (None, state) => state.to_owned(),
            };
            let _ = write!(out, "\n- {} · {model} · {state}", s.agent.name);
            if let Some(last) = &s.last {
                let _ = write!(out, " · last: {last}");
            }
        }
        out
    }
}
