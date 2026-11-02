//! Quota-aware workers (docs/tasks/DOGFOOD.md §5): a task's worker runs on an account that
//! has quota left, or on a teammate's when none of the template's vendor has; a worker that
//! hits its limit mid-task hands the task to one that has not. Each move is said in the run.

use crossagents_core::{
    Agent, AgentId, ClaimRole, CoreError, EventKind, HandoffKind, Role, Run, RunStatus, Task,
    TaskStatus,
};

use crate::brief;
use crate::engine::{Orchestrator, worker_name};
use crate::host::Route;

impl Orchestrator {
    /// A fresh worker for `task`, cloned from `template` where quota allows, else from the
    /// account or teammate the host routes it to. Returns the worker, the teammate it was
    /// cloned from, and why it did not go as planned (said in `task.rerouted`).
    pub(crate) async fn routed_worker(
        &self,
        run: &Run,
        task: &mut Task,
        template: AgentId,
        name: &str,
        limited: Option<AgentId>,
    ) -> Result<(Agent, AgentId, Option<String>), CoreError> {
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
        // An earlier execution session on the task keeps its name, archived.
        let name = self.free_name(name).await?;
        let worker = self
            .host
            .clone_agent_on(template, &name, Role::Worker, &cwd, account.as_deref())
            .await?;
        let worker = self.unmark_recruit(worker).await?;
        self.claim(&worker, run.id, Some(task.id), ClaimRole::Worker, true)
            .await?;
        if let Some(reason) = &reason {
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
            // By the session's own name: the template is only where it came from.
            let from = self.name_of(template).await.unwrap_or_default();
            self.notice(
                run.planner,
                &format!("task:{}:route", task.id),
                &format!(
                    "{}: {reason}; 执行会话 {}（来自模板 {from}）",
                    brief::task_ref(task),
                    worker.name
                ),
            )
            .await;
        }
        Ok((worker, template, reason))
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
        let name = worker_name(&run, &task, template);
        let _ = self.host.interrupt(agent).await;
        let left = self.left_by_worker(&run, &task, Some(agent)).await;
        crossagents_core::Handoff::end(
            &mut task.handoffs,
            Some(agent),
            crossagents_core::HandoffOutcome::Reassigned,
        );
        // The run lets go of it: archived (a session it started), or home (the agent itself).
        self.let_go_task(run.id, task.id, Some(ClaimRole::Worker))
            .await;
        let (worker, cloned, rerouted) = self
            .routed_worker(&run, &mut task, template, &name, Some(agent))
            .await?;
        task.assignee = Some(worker.id);
        if task.status != TaskStatus::Running {
            self.transition(&mut task, TaskStatus::Running).await?;
        } else {
            task.updated_at = chrono::Utc::now();
            self.store.update_task(&task).await?;
        }
        let reason = rerouted.unwrap_or_else(|| format!("its worker hit its quota ({why})"));
        let template_name = self.name_of(cloned).await;
        let card = self
            .hand_off(
                &run,
                &mut task,
                HandoffKind::Reroute,
                left,
                &worker,
                template_name,
                Some(reason),
            )
            .await?;
        let issuer = task.issuer_or(run.planner);
        self.notify_dispatched(issuer, &task, &card).await;
        self.relay(worker.id, issuer, brief::handover(&task, &card), None);
        Ok(())
    }
}
