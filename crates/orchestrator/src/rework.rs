//! Work that comes back to its own worker (a failed check or review, `task_rework`, the
//! person's send-back) after that worker's session has grown large (docs/USAGE-TRIM.md §8).
//!
//! Every request of a rework turn sends the whole session again, and a worker that delivered
//! often has 200k–300k tokens of it. Over the run's threshold
//! ([`RunPolicy::rework_new_session_at`]) the same agent — same id, name, assignee and task
//! ledger — gets a new model session, the way `run_next` renews a lead, and the rework opens
//! it with the hub's record of the work so far ([`brief::rework_opening`]): no summary a
//! model wrote. Below it, without a reported figure, or when the session cannot be renewed in
//! time, the rework goes to the session the worker has, exactly as before.
//!
//! [`RunPolicy::rework_new_session_at`]: crossagents_core::RunPolicy::rework_new_session_at

use crossagents_core::{AgentId, Envelope, EventKind, Run, Task};

use crate::brief;
use crate::engine::{Orchestrator, renew_for_round};

impl Orchestrator {
    /// `(context, threshold)` when `worker`'s session last reported more context in use than
    /// the run allows a rework to carry on in; `None` below it, with the threshold off, or
    /// when the hub has no figure for it (none reported since the hub started).
    pub(crate) fn rework_over(&self, run: &Run, worker: AgentId) -> Option<(u64, u64)> {
        let threshold = run.config.policy.rework_new_session_at()?;
        let context = self.context_of(worker)?;
        (context > threshold).then_some((context, threshold))
    }

    /// Sends `envelope` — the rework as the worker's session would read it — to `worker`.
    /// Over the threshold ([`Self::rework_over`]) its session is renewed first and the
    /// message opens the new one with the work so far; `task.rework_session` records it.
    /// `noted`: this return added its notes to `task.report.rework` (they are in `envelope`
    /// already, so they are not repeated among the decisions).
    pub(crate) async fn send_rework(
        &self,
        run: &Run,
        task: &Task,
        worker: AgentId,
        envelope: Envelope,
        noted: bool,
    ) {
        let Some((context, threshold)) = self.rework_over(run, worker) else {
            self.send_in_background(worker, envelope);
            return;
        };
        let mut earlier = task.report.rework.clone();
        if noted {
            earlier.pop();
        }
        let decisions = task
            .spec
            .interfaces
            .iter()
            .cloned()
            .chain(earlier)
            .collect();
        let facts = self
            .left_by_worker(run, task, Some(worker))
            .await
            .into_rework_facts(decisions);
        let opening = Envelope {
            text: brief::rework_opening(task, &facts, context, &envelope.text),
            ..envelope.clone()
        };
        let record = EventKind::TaskReworkSession {
            run_id: run.id,
            id: task.id,
            key: task.key.clone(),
            agent: worker,
            context_tokens: context,
            threshold,
        };
        let host = self.host.clone();
        let bus = self.bus.clone();
        // Subscribed before the spawn, so a turn that ends in between is not missed.
        let events = self.bus.subscribe();
        let limit = self.round_wait_limit();
        let handle = tokio::spawn(async move {
            // The worker is usually idle by now (it reported, then the checks or the review
            // ran); a turn still running is waited for, as a new round waits for its lead.
            let renewed = renew_for_round(host.as_ref(), events, worker, limit).await;
            let envelope = if renewed {
                if let Err(err) = bus.publish(record).await {
                    tracing::error!(error = %err, "cannot publish event");
                }
                opening
            } else {
                envelope
            };
            if let Err(err) = host.send_envelope(worker, envelope, false).await {
                tracing::warn!(agent = %worker, error = %err, "cannot deliver the rework");
            }
        });
        self.track_relay(handle);
    }
}
