//! Opt-in, bounded run supervision. Observe durable facts, then ask the lead to decide.
//! Never replay commands, accept work, answer questions or bypass landing checks here.

use chrono::{DateTime, Duration, Utc};
use crossagents_core::{
    Agent, AgentId, AgentKey, AgentStatus, ContinuationState, CoreError, EventKind, HumanItem,
    HumanItemKind, HumanItemStatus, Run, RunId, RunStatus, TaskStatus, model::StuckWatch,
};

use crate::Orchestrator;

/// A phase that waits on a busy agent, named so a report can say who is being waited for.
struct Waiting {
    /// "the lead «Luna»" or "agent «w-3» on #a (title)": how the lead should read it.
    what: String,
}

/// One observation of a run.
struct Step {
    phase: &'static str,
    next_step: String,
    /// Whether this observation may wake the lead once the persisted interval allows.
    eligible: bool,
    /// Set for the phases that wait on a busy agent (the only ones a stuck watch applies to).
    waiting: Option<Waiting>,
}

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
            if let Err(error) = self.continue_run(run.id, true).await {
                tracing::warn!(%error, run = %run.id, "cannot continue run");
            }
        }
    }

    /// The lead's turn ended: rewrite the checkpoint from the facts as they are now.
    ///
    /// The clock wrote "working" while the lead was in the turn a wakeup bought, with nothing
    /// due; a restart derives "continue" (or another settled phase) and reserves the next
    /// wakeup afresh, so the run read differently before and after a restart. Doing the same
    /// observation here, as soon as the agent is idle, keeps the two the same without waiting
    /// for the next tick.
    ///
    /// Nothing is woken and no attempt is counted ([`Self::continue_run`] with `deliver`
    /// false): the lead has just finished the turn, and a due time that passed during it is
    /// left due for the clock to deliver.
    ///
    /// The stuck watch is not judged here either: a quiet stretch that has already run out
    /// must reach the lead as the report it is, so only a delivery pass does the judging
    /// (see the branch in [`Self::continue_run`]).
    pub async fn settle_continuation(&self, lead: AgentId) {
        let runs = match self.store.list_runs().await {
            Ok(runs) => runs,
            Err(error) => {
                tracing::warn!(%error, "cannot list runs to settle a continuation");
                return;
            }
        };
        for run in runs {
            if run.planner != lead || run.continuation.is_none() {
                continue;
            }
            if let Err(error) = self.continue_run(run.id, false).await {
                tracing::warn!(%error, run = %run.id, "cannot settle the run's continuation");
            }
        }
    }

    /// The run's durable facts, and the arm of its stuck watch, moved back by `ago`: the
    /// stand-in a test uses for elapsed time, since the hub itself never moves these
    /// backwards. Nothing outside such a test has a use for it.
    ///
    /// It holds the run's lock — the one every continuation pass takes around its whole
    /// read-modify-write — while it rewrites the record. A rewrite from outside that lock
    /// races a pass that read the run just before it and writes that copy back a few
    /// milliseconds later, silently undoing the fabricated stretch; the test then sees no
    /// overdue watch for a reason nothing in the run shows.
    #[doc(hidden)]
    pub async fn age_run_records_for_test(
        &self,
        id: RunId,
        ago: Duration,
    ) -> Result<(), CoreError> {
        let _guard = self.lock(id).await;
        let mut run = self.store.get_run(id).await?;
        run.updated_at -= ago;
        if let Some(watch) = run
            .continuation
            .as_mut()
            .and_then(|continuation| continuation.stuck_watch.as_mut())
        {
            watch.progress_at -= ago;
        }
        self.store.update_run(&run).await?;
        for mut task in self.store.list_tasks(id).await? {
            task.updated_at -= ago;
            self.store.update_task(&task).await?;
        }
        Ok(())
    }

    /// The agent, when it counts as busy: its turn is starting or running, or the host holds
    /// queued messages for it. This says a process holds the agent, nothing about whether it
    /// is getting anywhere.
    async fn continuation_busy(&self, id: AgentId) -> Result<Option<Agent>, CoreError> {
        let agent = self.store.get_agent(&AgentKey::Id(id)).await?;
        let busy = matches!(agent.status, AgentStatus::Running | AgentStatus::Starting)
            || self.host.is_busy(id).await;
        Ok(busy.then_some(agent))
    }

    async fn continuation_step(&self, run: &Run) -> Result<Step, CoreError> {
        let waiting = |phase, reason: String| Step {
            phase,
            next_step: reason,
            eligible: false,
            waiting: None,
        };
        if run.status.is_terminal() {
            return Ok(waiting("ended", "Run ended; no further wakeups.".into()));
        }
        if run.status == RunStatus::Paused {
            return Ok(waiting(
                "paused",
                run.note.clone().unwrap_or_else(|| "Run paused.".into()),
            ));
        }
        if run.status == RunStatus::AwaitingApproval {
            return Ok(waiting(
                "waiting_approval",
                "Wait for the person to approve the plan.".into(),
            ));
        }
        let lead = self.store.get_agent(&AgentKey::Id(run.planner)).await?;
        if lead.archived_at.is_some() {
            return Ok(waiting(
                "unavailable",
                "The lead is archived; restore it before continuing.".into(),
            ));
        }
        if self
            .store
            .list_human_items(true)
            .await?
            .iter()
            .any(|item| item.run_id == Some(run.id) && item.kind == HumanItemKind::Question)
        {
            return Ok(waiting(
                "waiting_human",
                "Wait for the open human question or approval.".into(),
            ));
        }
        if let Some(busy) = self.continuation_busy(run.planner).await? {
            return Ok(Step {
                phase: "working",
                next_step: "The lead has a turn or queued messages; wait for it.".into(),
                eligible: false,
                waiting: Some(Waiting {
                    what: format!("the lead «{}»", busy.name),
                }),
            });
        }
        let tasks = self.store.list_tasks(run.id).await?;
        // A restarted hub can mark an adopted worker stalled. Its actual running turn
        // still wins over that label. Never use silence or elapsed time to interrupt it.
        // (The opt-in stuck watch below may *report* a quiet busy run to the lead; it still
        // never interrupts, and a live turn stays authoritative here.)
        for task in tasks.iter().filter(|task| !task.status.is_terminal()) {
            for agent in [task.assignee, task.reviewer].into_iter().flatten() {
                if let Some(busy) = self.continuation_busy(agent).await? {
                    return Ok(Step {
                        phase: "waiting_tasks",
                        next_step: format!(
                            "Wait for #{} and its dependents; an agent is working or has queued messages.",
                            task.key
                        ),
                        eligible: false,
                        waiting: Some(Waiting {
                            what: format!(
                                "agent «{}» on #{} ({})",
                                busy.name, task.key, task.title
                            ),
                        }),
                    });
                }
            }
            if task.status == TaskStatus::Verifying {
                return Ok(Step {
                    phase: "waiting_checks",
                    next_step: format!("Wait for #{} verification.", task.key),
                    eligible: false,
                    waiting: Some(Waiting {
                        what: format!("the verification of #{} ({})", task.key, task.title),
                    }),
                });
            }
        }
        if !tasks.is_empty() && tasks.iter().all(|task| task.status.is_terminal()) {
            return Ok(Step {
                phase: "finalize",
                next_step: "All tasks ended. Check the original request, failed/cancelled tasks and integration results. Repair missing work if needed; only then run the project checks and finish/land as policy permits.".into(),
                eligible: true,
                waiting: None,
            });
        }
        if tasks.is_empty() {
            return Ok(Step {
                phase: "continue",
                next_step: "No task is executing. Inspect the run checkout and original request; continue implementation or verify and finish. Do not treat a completed turn as a completed request.".into(),
                eligible: true,
                waiting: None,
            });
        }
        Ok(Step {
            phase: "recover",
            next_step: "No task agent is executing. Inspect task reports, questions, dependencies, diffs and check results; answer worker questions, recover stalled work, review completed work or repair failures. Do not blindly replay commands.".into(),
            eligible: true,
            waiting: None,
        })
    }

    /// How long the run has shown no progress, when a stuck watch is on and that stretch is
    /// at least `policy.stuck_minutes`. Advances `watch` over the durable facts it reads.
    ///
    /// Progress is a fact the hub already persisted, never the mere absence of words:
    ///
    /// - a newer `updated_at` on the run or on any of its tasks. Every task mutation goes
    ///   through `store_task`/`transition` (`task_progress`, reports, status changes,
    ///   handoffs, reroutes), and every run mutation through `set_run_status`/landing, so a
    ///   task or run that is actually getting somewhere keeps moving these timestamps.
    /// - an event one of the run's own agents appended since the last look: a turn starting,
    ///   output, a tool call, a message. An agent that is producing work is streaming these.
    ///
    /// Both are durable, so a restarted hub reads the same quiet stretch out of the run JSON
    /// instead of forgetting it. Neither proves work is happening: a turn stuck inside one
    /// very slow command (a compile, a fetch, a model call that has not streamed a token yet)
    /// writes neither, and would look quiet after the threshold. That is why the timeout only
    /// ever tells the lead — who can look at the checkout and decide — and why it is off by
    /// default and measured in minutes, not seconds.
    async fn stuck_progress(
        &self,
        run: &Run,
        watch: &mut StuckWatch,
        now: DateTime<Utc>,
    ) -> Result<(), CoreError> {
        let mut records_at = run.updated_at;
        let mut agents = vec![run.planner];
        for task in self.store.list_tasks(run.id).await? {
            records_at = records_at.max(task.updated_at);
            agents.extend(task.assignee);
            agents.extend(task.reviewer);
        }
        let latest = self.store.latest_event_seq().await?;
        if latest > watch.event_seq {
            let mut after = watch.event_seq;
            while after < latest {
                let page = self.store.events_since(after, 500).await?;
                let Some(end) = page.last().map(|event| event.seq) else {
                    break;
                };
                for event in page {
                    if event.kind.agent_id().is_some_and(|id| agents.contains(&id)) {
                        watch.progress_at = watch.progress_at.max(event.ts);
                    }
                }
                after = end;
            }
            // Only events after this point are new; keeps the next look cheap.
            watch.event_seq = latest;
        }
        // A record timestamp is at most this observation, and the bookmark never runs
        // ahead of it either: the reported stretch must not be a negative number if the
        // hub's clock moved backwards.
        watch.progress_at = watch.progress_at.max(records_at).min(now);
        Ok(())
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

    async fn continue_run(&self, id: RunId, deliver: bool) -> Result<(), CoreError> {
        let _guard = self.lock(id).await;
        let mut run = self.store.get_run(id).await?;
        let Some(policy) = run.config.policy.continuation.clone() else {
            return Ok(());
        };
        // The automation breaker stopped these wakeups (docs/research/multica.md §2.14): only a
        // person re-arming the run (`reset_attempts`, which clears the health) starts them again.
        if run
            .continuation
            .as_deref()
            .is_some_and(|state| state.health.is_paused())
        {
            return Ok(());
        }
        let now = Utc::now();
        let step = self.continuation_step(&run).await?;
        let observed_phase = step.phase;
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
            stuck_watch: None,
            health: crossagents_core::Health::default(),
        });
        state.owner = run.planner;
        let changed_phase = state.phase != step.phase;
        state.phase = step.phase.into();
        let mut next_step = step.next_step;
        let mut eligible = step.eligible;
        // The opt-in stuck watch. A phase that waits on a busy agent is not by itself a
        // reason to act (a compile may just be slow), and a busy agent's silence is not
        // evidence either: only the run's own durable facts going quiet for `stuck_minutes`
        // is. Even then the lead is only asked to decide; nothing is interrupted or replayed.
        //
        // Only a delivery pass judges that quiet stretch. Settling (`deliver` false) runs when
        // the lead goes Idle and sends nothing, so judging there would write the stuck advice
        // and re-arm `progress_at` with nothing delivered: the clock's next pass would then
        // see a fresh stretch and send the plain advice, and the "no progress for N minutes,
        // waiting for whom" would be lost. A settle therefore leaves the watch alone, with one
        // exception it owes: `state.phase` is written further up, so a phase change it records
        // here would be invisible to the next delivery pass (`changed_phase` false there) and
        // the quiet stretch of the old phase would be charged to the new one. Re-arming the
        // existing watch for the new phase keeps that stretch from leaking and swallows
        // nothing: it is the same reset a delivery pass makes on a phase change, not a stuck
        // judgement. A watch that does not exist yet is left for the delivery pass to create.
        let mut stuck = false;
        if let (Some(minutes), Some(waiting)) = (policy.stuck_minutes, step.waiting.as_ref()) {
            if deliver {
                let mut watch = match state.stuck_watch.clone() {
                    // A new phase starts its quiet stretch now: records written before it say
                    // nothing about what this wait is doing.
                    Some(watch) if !changed_phase => watch,
                    _ => StuckWatch {
                        progress_at: now,
                        event_seq: self.store.latest_event_seq().await?,
                    },
                };
                self.stuck_progress(&run, &mut watch, now).await?;
                let quiet = now - watch.progress_at;
                if quiet >= Duration::minutes(i64::from(minutes)) {
                    stuck = true;
                    eligible = true;
                    next_step = format!(
                        "No progress for {} minutes while waiting for {}: nothing in the run changed and none of its agents reported anything. Inspect it and decide yourself whether to interrupt it, reassign it or keep waiting. Nothing was interrupted or replayed.",
                        quiet.num_minutes().max(1),
                        waiting.what
                    );
                    // The threshold was the wait: start the next stretch here, so a later report
                    // needs another full `stuck_minutes` instead of one interval.
                    watch.progress_at = now;
                }
                state.stuck_watch = Some(watch);
            } else if changed_phase && let Some(watch) = state.stuck_watch.as_mut() {
                watch.progress_at = now;
                watch.event_seq = self.store.latest_event_seq().await?;
            }
        } else if deliver {
            // Not waiting on anyone: a quiet stretch from here says nothing. A settle leaves
            // the watch for the phase it saw; nothing reads it before the delivery pass that
            // clears it here.
            state.stuck_watch = None;
        }
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
            // the run JSON, so one exhaustion tells the person exactly once either way. Stuck
            // reports share the same cap and the same flag, so they cannot notify twice.
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
            if stuck {
                // The quiet stretch already lasted the threshold; do not add an interval.
                state.next_at = Some(now);
            }
            if deliver && state.next_at.is_some_and(|at| at <= now) {
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
            let delivered = self
                .host
                .notify(
                    run.planner,
                    &format!("continuation:{}", run.id),
                    &line,
                    crossagents_core::NoticeClass::Decide,
                )
                .await;
            // Every wakeup is a run of this automation; the breaker counts it whichever way it
            // went (multica §2.14). A pause is written with the rest before anyone is told.
            let tripped = crate::breaker::record(&mut state.health, now, delivered.is_ok());
            if let Some(reason) = &tripped {
                state.phase = "automation_paused".into();
                state.next_step = format!(
                    "Automatic continuation paused itself ({reason}). A person must inspect the run and re-enable it."
                );
                state.next_at = None;
            }
            if let Err(error) = delivered {
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
                if let Some(reason) = tripped {
                    self.report_automation_pause(&run, &state, &reason).await?;
                }
                return Err(error);
            }
            let mut changed = tripped.is_some();
            if state.delivery_error_notified {
                // Delivered after all: a later failure is a new fact worth one notice.
                state.delivery_error_notified = false;
                changed = true;
            }
            if changed {
                run.continuation = Some(Box::new(state.clone()));
                self.store.update_run(&run).await?;
                self.publish(EventKind::RunContinuationChanged {
                    id,
                    state: state.clone(),
                })
                .await;
            }
            if let Some(reason) = tripped {
                self.report_automation_pause(&run, &state, &reason).await?;
            }
        }
        Ok(())
    }

    /// Tells the person that the continuation stopped itself, once: the pause is already in the
    /// run record, so a restart reads it back and never reports it again.
    async fn report_automation_pause(
        &self,
        run: &Run,
        state: &ContinuationState,
        reason: &str,
    ) -> Result<(), CoreError> {
        let (total, failed) = crate::breaker::counts(&state.health, Utc::now());
        let text = format!(
            "「{}」（run {}）的自动持续推进已自动暂停：{}（7 天内运行 {} 次，失败 {} 次，失败率 ≥90%）。多半是主导长期叫不醒或账号额度用尽；检查账号和 run 状态后，用人用 run policy 的 reset_attempts 重新放开，才会继续。",
            run.title, run.id, reason, total, failed
        );
        self.continuation_notice(run, text).await
    }
}
