//! Opt-in, bounded run supervision. Observe durable facts, then ask the lead to decide.
//! Never replay commands, accept work, answer questions or bypass landing checks here.

use chrono::{Duration, Utc};
use crossagents_core::{
    AgentId, AgentKey, AgentStatus, ContinuationState, CoreError, EventKind, HumanItem,
    HumanItemKind, HumanItemStatus, Run, RunId, RunStatus, TaskStatus,
};

use crate::Orchestrator;

impl Orchestrator {
    /// Called after budget settlement by the hub clock. Each run is isolated: an error
    /// reading or delivering one run must not prevent supervision of another.
    pub async fn continue_runs(&self) {
        let runs = match self.store.list_runs().await {
            Ok(runs) => runs,
            Err(error) => {
                tracing::warn!(%error, "cannot list runs for continuation");
                return;
            }
        };
        for run in runs {
            if run.config.policy.continuation.is_none() {
                continue;
            }
            if let Err(error) = self.continue_run(run.id).await {
                tracing::warn!(%error, run = %run.id, "cannot continue run");
            }
        }
    }

    async fn continuation_busy(&self, id: AgentId) -> Result<bool, CoreError> {
        let agent = self.store.get_agent(&AgentKey::Id(id)).await?;
        Ok(
            matches!(agent.status, AgentStatus::Running | AgentStatus::Starting)
                || self.host.is_busy(id).await,
        )
    }

    async fn continuation_step(
        &self,
        run: &Run,
    ) -> Result<(&'static str, String, bool), CoreError> {
        let waiting = |phase, reason: String| Ok((phase, reason, false));
        if run.status.is_terminal() {
            return waiting("ended", "Run ended; no further wakeups.".into());
        }
        if run.status == RunStatus::Paused {
            return waiting(
                "paused",
                run.note.clone().unwrap_or_else(|| "Run paused.".into()),
            );
        }
        if run.status == RunStatus::AwaitingApproval {
            return waiting(
                "waiting_approval",
                "Wait for the person to approve the plan.".into(),
            );
        }
        let lead = self.store.get_agent(&AgentKey::Id(run.planner)).await?;
        if lead.archived_at.is_some() {
            return waiting(
                "unavailable",
                "The lead is archived; restore it before continuing.".into(),
            );
        }
        if self
            .store
            .list_human_items(true)
            .await?
            .iter()
            .any(|item| item.run_id == Some(run.id) && item.kind == HumanItemKind::Question)
        {
            return waiting(
                "waiting_human",
                "Wait for the open human question or approval.".into(),
            );
        }
        if self.continuation_busy(run.planner).await? {
            return waiting(
                "working",
                "The lead has a turn or queued messages; wait for it.".into(),
            );
        }
        let tasks = self.store.list_tasks(run.id).await?;
        // A restarted hub can mark an adopted worker stalled. Its actual running turn
        // still wins over that label. Never use silence or elapsed time to interrupt it.
        for task in tasks.iter().filter(|task| !task.status.is_terminal()) {
            for agent in [task.assignee, task.reviewer].into_iter().flatten() {
                if self.continuation_busy(agent).await? {
                    return waiting(
                        "waiting_tasks",
                        format!(
                            "Wait for #{} and its dependents; an agent is working or has queued messages.",
                            task.key
                        ),
                    );
                }
            }
            if task.status == TaskStatus::Verifying {
                return waiting(
                    "waiting_checks",
                    format!("Wait for #{} verification.", task.key),
                );
            }
        }
        if !tasks.is_empty() && tasks.iter().all(|task| task.status.is_terminal()) {
            return Ok(("finalize", "All tasks ended. Check the original request, failed/cancelled tasks and integration results. Repair missing work if needed; only then run the project checks and finish/land as policy permits.".into(), true));
        }
        if tasks.is_empty() {
            return Ok(("continue", "No task is executing. Inspect the run checkout and original request; continue implementation or verify and finish. Do not treat a completed turn as a completed request.".into(), true));
        }
        Ok(("recover", "No task agent is executing. Inspect task reports, questions, dependencies, diffs and check results; answer worker questions, recover stalled work, review completed work or repair failures. Do not blindly replay commands.".into(), true))
    }

    /// A notice for the person about the run, through the existing human-item channel.
    /// Callers persist the flag that makes this the only time a fact is reported before
    /// calling, so a duplicate is never made even if the hub stops right after.
    async fn continuation_notice(&self, run: &Run, text: String) -> Result<(), CoreError> {
        let item = HumanItem {
            id: format!("n-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]),
            detail: None,
            kind: HumanItemKind::Notice,
            from: run.planner,
            run_id: Some(run.id),
            task_id: None,
            text,
            options: Vec::new(),
            default: None,
            timeout_at: None,
            status: HumanItemStatus::Open,
            answer: None,
            answered_at: None,
            action: None,
            created_at: Utc::now(),
        };
        self.store.put_human_item(&item).await?;
        self.publish(EventKind::HumanAsked { item }).await;
        Ok(())
    }

    async fn continue_run(&self, id: RunId) -> Result<(), CoreError> {
        let _guard = self.lock(id).await;
        let mut run = self.store.get_run(id).await?;
        let Some(policy) = run.config.policy.continuation.clone() else {
            return Ok(());
        };
        let now = Utc::now();
        let (phase, next_step, eligible) = self.continuation_step(&run).await?;
        // What the run actually waits on, before the cap rewrites the phase to "exhausted".
        let observed_phase = phase;
        let old = run.continuation.as_deref().cloned();
        let mut state = old.clone().unwrap_or(ContinuationState {
            owner: run.planner,
            phase: String::new(),
            next_step: String::new(),
            attempts: 0,
            next_at: None,
            last_attempt_at: None,
            delivery_error: None,
            exhausted_notified: false,
            delivery_error_notified: false,
        });
        state.owner = run.planner;
        let changed_phase = state.phase != phase;
        state.phase = phase.into();
        state.next_step = next_step;
        let interval = Duration::seconds(i64::from(policy.interval_seconds.max(1)));
        let mut wake = false;
        let mut tell_exhausted = false;
        if !eligible {
            state.next_at = None;
        } else if state.attempts >= policy.max_attempts {
            state.phase = "exhausted".into();
            state.next_step = format!(
                "Automatic continuation limit ({}) reached. A person must inspect the run or send the lead further instructions. {}",
                policy.max_attempts, state.next_step
            );
            state.next_at = None;
            // The flag lives in the run record, not in this process: the clock revisits this
            // branch every few seconds and a restarted hub reads the flag straight back from
            // the run JSON, so one exhaustion tells the person exactly once either way.
            if !state.exhausted_notified {
                state.exhausted_notified = true;
                tell_exhausted = true;
            }
        } else {
            // Below the cap again: a person re-armed the run (attempts back to zero, or a
            // larger cap). The next exhaustion is a new fact and will notify again; the
            // policy-update path only has to reset `attempts` and this flag clears itself.
            state.exhausted_notified = false;
            if changed_phase || state.next_at.is_none() {
                state.next_at = Some(now + interval);
            }
            if state.next_at.is_some_and(|at| at <= now) {
                state.attempts += 1;
                state.last_attempt_at = Some(now);
                state.next_at = Some(now + interval);
                state.delivery_error = None;
                wake = true;
            }
        }
        if old.as_ref() != Some(&state) {
            run.continuation = Some(Box::new(state.clone()));
            self.store.update_run(&run).await?;
            self.publish(EventKind::RunContinuationChanged {
                id,
                state: state.clone(),
            })
            .await;
        }
        if tell_exhausted {
            // The flag was persisted just above, before anything reached the person: a crash
            // here can lose this notice, but no restart can produce a second one.
            let text = format!(
                "「{}」（run {}）的自动持续推进已用尽：已唤醒 {}/{} 次，停在 {} 阶段，需要人处理。\n下一步建议：{}",
                run.title,
                run.id,
                state.attempts,
                policy.max_attempts,
                observed_phase,
                state.next_step
            );
            self.continuation_notice(&run, text).await?;
        }
        if wake {
            // Reserve durably before delivery, under the run lock. A crash can lose one
            // attempt but cannot reset the cap or immediately replay it on restart.
            let line = format!(
                "Run continuation {}/{}: {} First re-read run/task state and the checkout. Respect pending human decisions, budget, project checks and landing permissions. Report a concrete next step or complete the run only when verified.",
                state.attempts, policy.max_attempts, state.next_step
            );
            if let Err(error) = self
                .host
                .notify(run.planner, &format!("continuation:{}", run.id), &line)
                .await
            {
                let reason = error.to_string();
                state.delivery_error = Some(reason.clone());
                let first_failure = !state.delivery_error_notified;
                state.delivery_error_notified = true;
                run.continuation = Some(Box::new(state.clone()));
                self.store.update_run(&run).await?;
                self.publish(EventKind::RunContinuationChanged {
                    id,
                    state: state.clone(),
                })
                .await;
                if first_failure {
                    // One notice per streak of failures, on the same persisted flag rule; the
                    // next successful delivery clears it and lets a later failure tell again.
                    let text = format!(
                        "「{}」（run {}）的持续推进唤醒没有送达主导：{}。这次额度已扣除，之后会按间隔重试；连续失败只通知这一次。",
                        run.title, run.id, reason
                    );
                    if let Err(notice) = self.continuation_notice(&run, text).await {
                        tracing::warn!(%notice, run = %id, "cannot report a failed continuation delivery");
                    }
                }
                return Err(error);
            } else if state.delivery_error_notified {
                // Delivered after all: a later failure is a new fact worth one notice.
                state.delivery_error_notified = false;
                run.continuation = Some(Box::new(state.clone()));
                self.store.update_run(&run).await?;
                self.publish(EventKind::RunContinuationChanged { id, state })
                    .await;
            }
        }
        Ok(())
    }
}
