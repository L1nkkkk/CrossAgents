//! Everyone a wakeup may reach, and when it must not (docs/research/multica.md §2.1, §2.3).
//!
//! The rules here are mechanical: the hub counts and closes, and never decides what a wakeup
//! means. Four of them keep a wakeup from spinning:
//!
//! - a rule never wakes its owner about the owner's own words or changes;
//! - a chain of triggers with nobody in between, or a burst within an hour, pauses the rule;
//! - a waiting rule lasts at most seven days, then wakes its owner once and closes;
//! - a task or run ending closes the rules that follow it.
//!
//! The Planner's wake table (a member posting progress with nobody addressed wakes the lead,
//! addressing another teammate does not, the lead's own note never does) is
//! [`Orchestrator::wake_lead_on_progress`].

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use crossagents_core::{
    Agent, AgentId, Event, EventKind, HumanItem, HumanItemKind, HumanItemStatus, Run, RunId, Task,
    TaskId, TaskStatus, Watch, WatchTarget,
};

use crate::engine::Orchestrator;

/// A waiting rule lasts at most this long before it is woken once and closed ("the thing you
/// waited for did not happen").
pub const WAIT_MAX_DAYS: i64 = 7;
/// A chain that passes the same rule this many times with nobody in between pauses it.
pub const CHAIN_LIMIT: u32 = 3;
/// More triggers than this within an hour pause the rule as well.
pub const TRIGGERS_PER_HOUR: usize = 12;

/// [`WAIT_MAX_DAYS`] as a duration.
pub fn wait_max() -> chrono::Duration {
    chrono::Duration::days(WAIT_MAX_DAYS)
}

/// What a rule's trigger means now that it has been counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// The rule may wake its owner.
    Wake,
    /// The rule stopped itself with this reason, and wakes nobody.
    Pause(String),
}

/// When a rule's wait is over: its own `expires_at`, else a week after it was set.
pub fn expires_at(watch: &Watch) -> DateTime<Utc> {
    watch
        .expires_at
        .unwrap_or_else(|| watch.created_at + wait_max())
}

/// Counts one trigger and says whether the rule may wake its owner. The chain that reaches
/// [`CHAIN_LIMIT`], or more than [`TRIGGERS_PER_HOUR`] triggers in an hour, pauses the rule
/// instead: nobody took part between them, so the next wake only spins.
pub fn account_trigger(watch: &mut Watch, now: DateTime<Utc>) -> Trigger {
    if watch.health.is_paused() {
        let reason = watch
            .health
            .pause_reason
            .clone()
            .unwrap_or_else(|| "paused".into());
        return Trigger::Pause(reason);
    }
    watch.chain += 1;
    if watch.chain >= CHAIN_LIMIT {
        let reason = format!(
            "it woke its owner {} times in a row with nobody in between",
            watch.chain
        );
        watch.health.pause(now, reason.clone());
        return Trigger::Pause(reason);
    }
    let in_hour = crate::breaker::last_hour(&watch.health, now);
    if in_hour >= TRIGGERS_PER_HOUR {
        let reason = format!("it fired {in_hour} times within an hour");
        watch.health.pause(now, reason.clone());
        return Trigger::Pause(reason);
    }
    Trigger::Wake
}

/// Writes a rule back after its chain or health moved. The store keeps one row per id and has
/// no update of its own: the record is small, so it is replaced. A rule that vanished under us
/// (cancelled, or closed with its task) stays gone rather than coming back stale.
pub async fn save_rule(
    store: &dyn crossagents_core::Store,
    watch: &Watch,
) -> Result<(), crossagents_core::CoreError> {
    match store.delete_watch(&watch.id).await {
        Ok(()) => store.insert_watch(watch).await,
        Err(crossagents_core::CoreError::NotFound { .. }) => Ok(()),
        Err(err) => Err(err),
    }
}

/// A person took part: the chain starts over.
pub fn human_involved(watch: &mut Watch) {
    watch.chain = 0;
}

/// Whether a task has ended: its rules close with it.
pub fn task_ended(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Merged | TaskStatus::Failed | TaskStatus::Cancelled
    )
}

/// The first non-empty line of `text`, at most `max` characters.
fn one_line(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim();
    let mut out: String = line.chars().take(max).collect();
    if out.chars().count() < line.chars().count() {
        out.push('…');
    }
    out
}

/// Whether `text` addresses a member of the run other than its lead. That is a handoff
/// already made on purpose, so the lead is not woken as well (multica §2.3).
pub fn mentions_another_member(text: &str, lead: AgentId, members: &[(AgentId, String)]) -> bool {
    // Longest name first: "Ada 2" must not be found as "Ada".
    let mut names: Vec<&String> = members
        .iter()
        .filter(|(id, _)| *id != lead)
        .map(|(_, name)| name)
        .collect();
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    names
        .iter()
        .any(|name| !name.is_empty() && text.contains(&format!("@{name}")))
}

/// The Planner's wake table as a decision (multica §2.3): the lead's own word never wakes it,
/// and neither does anything about a run or task that has already ended. A note that addresses
/// another teammate is a handoff already made, so the lead is not woken as well; anything else
/// is progress it should evaluate.
pub fn lead_should_be_woken(
    lead: AgentId,
    caller: AgentId,
    run_ended: bool,
    task_ended: bool,
    text: &str,
    members: &[(AgentId, String)],
) -> bool {
    if lead == caller || run_ended || task_ended {
        return false;
    }
    !mentions_another_member(text, lead, members)
}

/// Whether a watch has outlived its wait: the owner is woken once and the watch closes.
pub fn watch_expired(watch: &Watch, now: DateTime<Utc>) -> bool {
    now >= expires_at(watch)
}

/// "task #a (title)" or "agent Ada (id)", for a notice about what a rule followed.
pub fn describe_target(
    target: &WatchTarget,
    task: Option<(&str, &str)>,
    agent: Option<&str>,
) -> String {
    match target {
        WatchTarget::Task { task_id } => match task {
            Some((key, title)) => format!("task #{key} ({title})"),
            None => format!("task {task_id}"),
        },
        WatchTarget::Agent { agent_id } => match agent {
            Some(name) => format!("agent {name}"),
            None => format!("agent {agent_id}"),
        },
    }
}

impl Orchestrator {
    /// A notice for the person, through the same channel `continuation` uses. Nothing about it
    /// wakes an agent.
    pub(crate) async fn tell_person(
        &self,
        from: AgentId,
        run: Option<RunId>,
        text: String,
    ) -> Result<(), crossagents_core::CoreError> {
        let item = HumanItem {
            id: format!("n-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]),
            detail: None,
            kind: HumanItemKind::Notice,
            from,
            run_id: run,
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
        self.store().put_human_item(&item).await?;
        self.publish(EventKind::HumanAsked { item }).await;
        Ok(())
    }

    /// Tells the person that a rule stopped itself, with what it was following. Once per pause:
    /// the caller saves the paused rule first, so a restart cannot send a second one.
    pub async fn warn_rule_paused(
        &self,
        watch: &Watch,
        reason: &str,
    ) -> Result<(), crossagents_core::CoreError> {
        let what = self.describe_watch_target(&watch.target).await;
        let owner = self
            .store()
            .get_agent(&crossagents_core::AgentKey::Id(watch.owner))
            .await
            .map(|agent| agent.name)
            .unwrap_or_else(|_| watch.owner.to_string());
        let text = format!(
            "唤醒规则 {}（主人 {owner}，等 {what}）已自动暂停：{reason}。\n连环唤醒和 1 小时内的密集触发不会自己停下，先查有没有成环；要重新开始就 watch_cancel 删掉，再让主人 watch 一次。",
            watch.id
        );
        self.tell_person(watch.owner, None, text).await
    }

    /// "task #a (title)" / "agent Ada" for a rule's target.
    pub async fn describe_watch_target(&self, target: &WatchTarget) -> String {
        match target {
            WatchTarget::Task { task_id } => {
                let task = self.store().get_task(*task_id).await.ok();
                describe_target(
                    target,
                    task.as_ref().map(|t| (t.key.as_str(), t.title.as_str())),
                    None,
                )
            }
            WatchTarget::Agent { agent_id } => {
                let name = self
                    .store()
                    .get_agent(&crossagents_core::AgentKey::Id(*agent_id))
                    .await
                    .ok()
                    .map(|a| a.name);
                describe_target(target, None, name.as_deref())
            }
        }
    }

    /// Wakes the owner of every rule whose wait ran out, then closes the rule (multica §2.1:
    /// "the thing you waited for did not happen"). Nothing is left behind that could fire again.
    pub async fn expire_watches(&self) -> Result<(), crossagents_core::CoreError> {
        let now = Utc::now();
        for watch in self.store().list_watches(None).await? {
            // A paused rule already stopped; its wait is not what is running out.
            if watch.health.is_paused() || !watch_expired(&watch, now) {
                continue;
            }
            let what = self.describe_watch_target(&watch.target).await;
            self.store().delete_watch(&watch.id).await?;
            let line = format!(
                "Watch {} expired: the thing it waited for ({what}) did not happen in {} days. Ask again with watch if you still need it.",
                watch.id,
                WAIT_MAX_DAYS
            );
            if let Err(err) = self
                .host
                .notify(watch.owner, &format!("watch:{}", watch.id), &line)
                .await
            {
                tracing::warn!(owner = %watch.owner, error = %err, "cannot deliver an expired watch");
            }
        }
        Ok(())
    }

    /// A person spoke to `owner`: any chain of wakeups that had nobody in between starts over.
    pub async fn reset_rule_chains(
        &self,
        owner: AgentId,
    ) -> Result<(), crossagents_core::CoreError> {
        for mut watch in self.store().list_watches(Some(owner)).await? {
            if watch.chain == 0 {
                continue;
            }
            human_involved(&mut watch);
            save_rule(self.store().as_ref(), &watch).await?;
        }
        Ok(())
    }

    /// Closes the rules a task or run ending makes pointless (multica §2.1). Called for every
    /// event, so a task that ends by any path closes its own rules.
    pub async fn close_rules_for_event(
        &self,
        event: &Event,
    ) -> Result<(), crossagents_core::CoreError> {
        match &event.kind {
            EventKind::TaskStatusChanged { id, to, .. } if task_ended(*to) => {
                self.close_task_rules(*id).await
            }
            EventKind::RunStatusChanged { id, to, .. } if to.is_terminal() => {
                self.close_run_rules(*id).await
            }
            _ => Ok(()),
        }
    }

    async fn close_task_rules(&self, task: TaskId) -> Result<(), crossagents_core::CoreError> {
        for watch in self.store().list_watches(None).await? {
            if watch.target == (WatchTarget::Task { task_id: task }) {
                self.store().delete_watch(&watch.id).await?;
            }
        }
        for reminder in self.store().list_reminders(None).await? {
            if reminder.task == Some(task) {
                self.store().delete_reminder(&reminder.id).await?;
            }
        }
        Ok(())
    }

    async fn close_run_rules(&self, run: RunId) -> Result<(), crossagents_core::CoreError> {
        let Ok(run_record) = self.store().get_run(run).await else {
            return Ok(());
        };
        let tasks = self.store().list_tasks(run).await?;
        let task_ids: HashSet<TaskId> = tasks.iter().map(|task| task.id).collect();
        // Everyone who took part: the lead, whoever is on a task now, whoever handled one
        // before (a dropped task releases its assignee, but its handoffs still name them), and
        // the teammates the run was given.
        let mut agents: HashSet<AgentId> = HashSet::from([run_record.planner]);
        agents.extend(run_record.config.team.iter().copied());
        for task in &tasks {
            agents.extend([task.assignee, task.reviewer].into_iter().flatten());
            for card in &task.handoffs {
                agents.extend([card.from, Some(card.to)].into_iter().flatten());
            }
        }
        for watch in self.store().list_watches(None).await? {
            if !agents.contains(&watch.owner) {
                continue;
            }
            let in_run = match &watch.target {
                WatchTarget::Task { task_id } => task_ids.contains(task_id),
                WatchTarget::Agent { agent_id } => agents.contains(agent_id),
            };
            if in_run {
                self.store().delete_watch(&watch.id).await?;
            }
        }
        for reminder in self.store().list_reminders(None).await? {
            if agents.contains(&reminder.owner)
                && reminder.task.is_some_and(|task| task_ids.contains(&task))
            {
                self.store().delete_reminder(&reminder.id).await?;
            }
        }
        Ok(())
    }

    /// The Planner's wake table (multica §2.3): a member's progress note that addresses nobody
    /// wakes the lead to evaluate it; one that addresses another teammate is a handoff already
    /// made, and the lead's own note never wakes it. The line goes into the lead's digest under
    /// one topic per run, so a burst of notes is one queued wake, not one turn each.
    pub async fn wake_lead_on_progress(&self, run: &Run, task: &Task, caller: &Agent, text: &str) {
        if run.planner == caller.id {
            // The lead's own word never wakes it.
            return;
        }
        let mut members = Vec::new();
        let mut ids = vec![run.planner];
        for other in self.store().list_tasks(run.id).await.unwrap_or_default() {
            ids.extend([other.assignee, other.reviewer].into_iter().flatten());
        }
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            if let Ok(agent) = self
                .store()
                .get_agent(&crossagents_core::AgentKey::Id(id))
                .await
            {
                members.push((agent.id, agent.name));
            }
        }
        if !lead_should_be_woken(
            run.planner,
            caller.id,
            run.status.is_terminal(),
            task_ended(task.status),
            text,
            &members,
        ) {
            return;
        }
        let line = format!(
            "Progress from {} on #{} ({}): {}",
            caller.name,
            task.key,
            task.title,
            one_line(text, 200)
        );
        if let Err(err) = self
            .host
            .notify(run.planner, &format!("lead-progress:{}", run.id), &line)
            .await
        {
            tracing::warn!(lead = %run.planner, error = %err, "cannot wake the lead about progress");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossagents_core::{Health, WatchOn};

    fn watch() -> Watch {
        Watch {
            id: "w-1".into(),
            owner: AgentId::new(),
            target: WatchTarget::Agent {
                agent_id: AgentId::new(),
            },
            on: vec![WatchOn::Any],
            created_at: Utc::now(),
            expires_at: None,
            chain: 0,
            health: Health::default(),
        }
    }

    #[test]
    fn the_planners_wake_table() {
        let lead = AgentId::new();
        let worker = AgentId::new();
        let rex = AgentId::new();
        let members = vec![
            (lead, "Pia".to_owned()),
            (worker, "Ada".to_owned()),
            (rex, "Rex".to_owned()),
        ];
        // A member's progress note, nobody addressed: the lead evaluates it.
        assert!(lead_should_be_woken(
            lead,
            worker,
            false,
            false,
            "halfway",
            &members
        ));
        // The lead's own note never wakes it.
        assert!(!lead_should_be_woken(
            lead, lead, false, false, "halfway", &members
        ));
        // Addressing another teammate is a handoff, not progress for the lead.
        assert!(!lead_should_be_woken(
            lead,
            worker,
            false,
            false,
            "@Rex run it",
            &members
        ));
        // Nothing to evaluate once the run or the task is over.
        assert!(!lead_should_be_woken(
            lead, worker, true, false, "done", &members
        ));
        assert!(!lead_should_be_woken(
            lead, worker, false, true, "done", &members
        ));
    }

    #[test]
    fn addressing_another_member_is_a_handoff_not_a_progress_note() {
        let lead = AgentId::new();
        let rex = AgentId::new();
        let mia = AgentId::new();
        let members = vec![
            (lead, "Pia".to_owned()),
            (rex, "Rex".to_owned()),
            (mia, "Ada 2".to_owned()),
        ];
        // Nobody addressed: the lead is woken.
        assert!(!mentions_another_member(
            "halfway there, tests pass",
            lead,
            &members
        ));
        // Addressing the lead itself is still a progress note to evaluate.
        assert!(!mentions_another_member(
            "@Pia the schema is settled",
            lead,
            &members
        ));
        // Addressing a teammate: already handed over, so the lead is not woken.
        assert!(mentions_another_member(
            "@Rex please run the migration",
            lead,
            &members
        ));
        // A name that is a prefix of another is matched as itself.
        assert!(mentions_another_member("@Ada 2 took it", lead, &members));
    }

    #[test]
    fn a_watch_expires_after_its_wait() {
        let now = Utc::now();
        let mut watch = watch();
        watch.created_at = now;
        assert!(!watch_expired(&watch, now + chrono::Duration::days(6)));
        assert!(watch_expired(&watch, now + wait_max()));
        // A shorter wait is what the record says.
        watch.expires_at = Some(now + chrono::Duration::minutes(5));
        assert!(watch_expired(&watch, now + chrono::Duration::minutes(6)));
    }

    #[test]
    fn the_third_trigger_in_a_row_pauses_the_rule() {
        let now = Utc::now();
        let mut rule = watch();
        for _ in 1..CHAIN_LIMIT {
            assert_eq!(account_trigger(&mut rule, now), Trigger::Wake);
        }
        let trigger = account_trigger(&mut rule, now);
        assert!(
            matches!(&trigger, Trigger::Pause(reason) if reason.contains("in a row")),
            "{trigger:?}"
        );
        assert!(rule.health.is_paused());
        // It stays paused, with its reason, and never wakes anyone again.
        assert!(matches!(
            account_trigger(&mut rule, now),
            Trigger::Pause(_)
        ));
        // A person taking part clears the chain, so the rule may wake again.
        let mut again = watch();
        account_trigger(&mut again, now);
        account_trigger(&mut again, now);
        human_involved(&mut again);
        assert_eq!(again.chain, 0);
        assert_eq!(account_trigger(&mut again, now), Trigger::Wake);
    }

    #[test]
    fn a_burst_within_an_hour_pauses_the_rule() {
        let now = Utc::now();
        let mut watch = watch();
        // Twelve runs in the hour, each with a person in between so the chain never trips.
        for _ in 0..TRIGGERS_PER_HOUR {
            assert_eq!(account_trigger(&mut watch, now), Trigger::Wake);
            crate::breaker::record(&mut watch.health, now, true);
            human_involved(&mut watch);
        }
        assert!(
            matches!(account_trigger(&mut watch, now), Trigger::Pause(reason) if reason.contains("within an hour")),
            "{:?}",
            watch.health
        );
    }

    #[test]
    fn fifty_runs_at_ninety_percent_failures_pause_the_automation() {
        let now = Utc::now();
        let mut health = Health::default();
        // Five good runs, then forty-five bad ones: the fiftieth run is 90% failed.
        for _ in 0..5 {
            crate::breaker::record(&mut health, now, true);
        }
        let mut tripped = None;
        for _ in 0..45 {
            tripped = crate::breaker::record(&mut health, now, false);
        }
        let reason = tripped.expect("the breaker trips on the fiftieth run");
        assert!(health.is_paused(), "{health:?}");
        assert!(reason.contains("45 of 50"), "{reason}");
        assert_eq!(crate::breaker::counts(&health, now), (50, 45));
        // The record is what a restarted hub reads back.
        assert_eq!(health.runs.len(), 50);
        assert_eq!(health.runs.iter().filter(|run| !run.ok).count(), 45);
    }
}
