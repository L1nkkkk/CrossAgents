//! Quota-aware workers (docs/tasks/DOGFOOD.md §5): a task's worker runs on an account that
//! has quota left, or on a teammate's when none of the template's vendor has; a worker that
//! hits its limit mid-task hands the task to one that has not. Each move is said in the run.

use crossagents_core::{
    Agent, AgentId, CoreError, EventKind, Role, Run, RunStatus, Task, TaskStatus,
};

use crate::brief;
use crate::engine::{Orchestrator, agent_name};
use crate::host::Route;

impl Orchestrator {
    /// A fresh worker for `task`, cloned from `template` where quota allows, else from the
    /// account or teammate the host routes it to.
    pub(crate) async fn routed_worker(
        &self,
        run: &Run,
        task: &mut Task,
        template: AgentId,
        name: &str,
        limited: Option<AgentId>,
    ) -> Result<Agent, CoreError> {
        let cwd = task.worktree.clone().unwrap_or_default();
        let mut team: Vec<AgentId> = run.config.team.clone();
        if !team.contains(&run.config.worker) {
            team.push(run.config.worker);
        }
        let route = self.host.route(template, &team, limited).await;
        let (template, account, reason) = match route {
            Route::Same => (template, None, None),
            Route::Exhausted { reason } => (template, None, Some(reason)),
            Route::Account { account, reason } => (template, Some(account), Some(reason)),
            Route::Teammate { agent, reason } => (agent, None, Some(reason)),
        };
        if let Ok(stale) = self
            .store
            .get_agent(&crossagents_core::AgentKey::Name(name.to_owned()))
            .await
        {
            self.host.remove_agent(stale.id).await?;
        }
        let worker = self
            .host
            .clone_agent_on(template, name, Role::Worker, &cwd, account.as_deref())
            .await?;
        if let Some(reason) = reason {
            // Stored before it is announced: whoever reads the task on the event sees why.
            task.progress = Some(reason.clone());
            task.updated_at = chrono::Utc::now();
            self.store.update_task(task).await?;
            self.publish(EventKind::TaskRerouted {
                run_id: run.id,
                id: task.id,
                key: task.key.clone(),
                to: worker.id,
                reason: reason.clone(),
            })
            .await;
            self.notice(
                run.planner,
                &format!("task:{}:route", task.id),
                &format!("{}: {reason}", brief::task_ref(task)),
            )
            .await;
        }
        Ok(worker)
    }

    /// `agent` hit its account's limit. If it works on a task, the task goes to a worker on
    /// an account (or a teammate) with quota left, in the same worktree.
    pub async fn rate_limited(&self, agent: AgentId, why: &str) -> Result<(), CoreError> {
        let Some((run, task)) = self.task_for_agent(agent).await else {
            return Ok(());
        };
        if task.assignee != Some(agent) || run.status != RunStatus::Running {
            return Ok(());
        }
        let _guard = self.lock(run.id).await;
        let mut task = self.store.get_task(task.id).await?;
        if task.assignee != Some(agent)
            || !matches!(task.status, TaskStatus::Running | TaskStatus::Stalled)
        {
            return Ok(());
        }
        let template = task.worker_template.unwrap_or(run.config.worker);
        let mut team: Vec<AgentId> = run.config.team.clone();
        team.push(run.config.worker);
        if matches!(
            self.host.route(template, &team, Some(agent)).await,
            Route::Same | Route::Exhausted { .. }
        ) {
            self.notice(
                run.planner,
                &format!("task:{}:route", task.id),
                &format!(
                    "{}: its worker hit its quota ({why}) and no other account or teammate has quota left.",
                    brief::task_ref(&task)
                ),
            )
            .await;
            return Ok(());
        }
        let name = agent_name("w", &run, &task);
        let _ = self.host.interrupt(agent).await;
        if let Err(err) = self.host.remove_agent(agent).await {
            tracing::warn!(%agent, error = %err, "cannot remove the rate-limited worker");
        }
        let worker = self
            .routed_worker(&run, &mut task, template, &name, Some(agent))
            .await?;
        task.assignee = Some(worker.id);
        if task.status != TaskStatus::Running {
            self.transition(&mut task, TaskStatus::Running).await?;
        } else {
            task.updated_at = chrono::Utc::now();
            self.store.update_task(&task).await?;
        }
        let issuer = task.issuer_or(run.planner);
        self.relay(
            worker.id,
            issuer,
            format!(
                "{}\n\nThe previous worker stopped when its account hit its quota ({why}); its changes are in your working directory.",
                brief::task(&task)
            ),
            None,
        );
        Ok(())
    }
}
