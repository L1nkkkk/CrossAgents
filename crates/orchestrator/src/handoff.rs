//! Handoff cards (docs/research/magpie-lancer.md §2.1): each time a task changes hands — its
//! first worker, `task_assign`, a quota reroute, an escalation — the hub records what went
//! with it on the task and in the event stream, before the new worker hears of it. What the
//! worker before left is read before it goes; a part that cannot be read stays empty and
//! never holds up the handoff.

use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AgentKey, CoreError, EventKind, Handoff, HandoffId, HandoffKind, Run, Task,
};

use crate::engine::Orchestrator;

/// What the one handing the task over leaves with it.
#[derive(Debug, Default)]
pub(crate) struct Left {
    from: Option<AgentId>,
    from_name: Option<String>,
    files: Vec<String>,
    summary: Option<String>,
    open_questions: Vec<String>,
    context_tokens: Option<u64>,
}

impl Orchestrator {
    /// A first assignment: from the issuer, with the files the spec allows.
    pub(crate) async fn left_by_issuer(&self, run: &Run, task: &Task) -> Left {
        let issuer = task.issuer_or(run.planner);
        Left {
            from: Some(issuer),
            from_name: self.name_of(issuer).await,
            files: task.spec.scope.allow.clone(),
            ..Left::default()
        }
    }

    /// What the task's worker until now (`worker`) leaves: the files it changed, its last
    /// word, the question it is still waiting on. Read before the worker is removed and
    /// before the task is changed for the next one.
    pub(crate) async fn left_by_worker(
        &self,
        run: &Run,
        task: &Task,
        worker: Option<AgentId>,
    ) -> Left {
        let files = match self.changed_files(run, task).await {
            Ok(files) => files,
            Err(err) => {
                tracing::warn!(task = %task.key, error = %err, "cannot list the files a handoff carries");
                Vec::new()
            }
        };
        // Who the person knew the worker as: the session the last handoff named, else the
        // one it was cloned from.
        let from_name = match task.handoffs.last() {
            Some(last) => Some(last.to_name.clone()),
            None => {
                self.name_of(task.worker_template.unwrap_or(run.config.worker))
                    .await
            }
        };
        // Its own last word: a handoff clears what the worker before said, and the progress
        // it leaves (why the task changed hands) is the hub's, not this worker's.
        let said_by_hub = task.handoffs.last().and_then(|h| h.reason.as_ref());
        let progress = task.progress.clone().filter(|p| Some(p) != said_by_hub);
        Left {
            from: worker,
            from_name,
            files,
            summary: task.report.summary.clone().or(progress),
            open_questions: task.report.question.iter().cloned().collect(),
            context_tokens: worker.and_then(|w| self.context_of(w)),
        }
    }

    async fn changed_files(&self, run: &Run, task: &Task) -> Result<Vec<String>, CoreError> {
        let dir = task
            .worktree
            .clone()
            .ok_or_else(|| CoreError::Conflict(format!("#{} has no worktree", task.key)))?;
        if !dir.exists() {
            return Err(CoreError::Conflict(format!(
                "the worktree of #{} is gone: {}",
                task.key,
                dir.display()
            )));
        }
        let (base, _) = self.base_of(run, task).await?;
        self.ws_for(run).changed_files(&dir, &base).await
    }

    /// Records the card on `task` and announces it. Called once `worker` has the task and
    /// before its first message is sent. `template` is the teammate `worker` was cloned
    /// from, `None` when `worker` took the task itself.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn hand_off(
        &self,
        run: &Run,
        task: &mut Task,
        kind: HandoffKind,
        left: Left,
        worker: &Agent,
        template: Option<String>,
        reason: Option<String>,
    ) -> Result<Handoff, CoreError> {
        let decisions = task
            .spec
            .interfaces
            .iter()
            .chain(&task.report.rework)
            .cloned()
            .collect();
        let card = Handoff {
            id: HandoffId::new(),
            run_id: run.id,
            task_id: task.id,
            task_key: task.key.clone(),
            kind,
            from: left.from,
            from_name: left.from_name,
            to: worker.id,
            // The name to message it by; where it came from goes alongside.
            to_name: worker.name.clone(),
            template: template.filter(|t| *t != worker.name),
            reason,
            goal: task.spec.goal.clone(),
            files: left.files,
            summary: left.summary,
            decisions,
            open_questions: left.open_questions,
            context_tokens: left.context_tokens,
            // What holds it says how it was taken: started for this, or itself.
            fresh: self
                .store
                .claim_of(worker.id)
                .await?
                .is_none_or(|c| c.fresh),
            outcome: None,
            at: Utc::now(),
        };
        task.handoffs.push(card.clone());
        // What the worker before reported is on the card now; left on the task, it would read
        // as the new worker's (and travel on the next card under its name). Until the new
        // one says something, the task's line says why it changed hands.
        task.report.summary = None;
        task.report.failure = None;
        task.report.related_lessons.clear();
        task.progress = card.reason.clone();
        task.updated_at = Utc::now();
        self.store.update_task(task).await?;
        self.publish(EventKind::TaskHandedOff {
            run_id: run.id,
            handoff: card.clone(),
        })
        .await;
        Ok(card)
    }

    pub(crate) async fn name_of(&self, agent: AgentId) -> Option<String> {
        self.store
            .get_agent(&AgentKey::Id(agent))
            .await
            .ok()
            .map(|a| a.name)
    }

    /// Context tokens `agent` last reported in use (`agent.usage`), if it ever did.
    fn context_of(&self, agent: AgentId) -> Option<u64> {
        self.contexts.lock().unwrap().get(&agent).copied()
    }

    pub(crate) fn record_context(&self, agent: AgentId, used: u64) {
        if used > 0 {
            self.contexts.lock().unwrap().insert(agent, used);
        }
    }
}
