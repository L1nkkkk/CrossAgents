//! Questions and notices for the person, and the budget (docs/tasks/H1.md §4, §5.4). The
//! run's policy decides whether agents may ask, and whether a question waits forever or
//! falls back to its default; every fallback is recorded and listed when the run ends.

use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AskPolicy, CoreError, Envelope, EventKind, HumanAction, HumanItem,
    HumanItemKind, HumanItemStatus, MessageSource, Run, RunId, RunPolicy, RunStatus,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::Orchestrator;

/// Budget use at which the lead hears about it.
const WARN_PERCENT: u32 = 80;

fn item_id(prefix: &str) -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}-{}", &id[..10])
}

impl Orchestrator {
    /// The policy that applies to `agent`: its run's, else the defaults.
    async fn policy_for(&self, agent: AgentId) -> Result<(Option<Run>, RunPolicy), CoreError> {
        Ok(match self.run_of(agent).await? {
            Some((run, _)) => {
                let policy = run.config.policy.clone();
                (Some(run), policy)
            }
            None => (None, RunPolicy::default()),
        })
    }

    pub(crate) async fn human_ask(&self, caller: &Agent, input: Value) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            text: String,
            #[serde(default)]
            options: Vec<String>,
            #[serde(default)]
            default: Option<String>,
            #[serde(default)]
            task: Option<String>,
        }
        let a: A = crate::tools::args(input)?;
        let (run, policy) = self.policy_for(caller.id).await?;
        if policy.human_ask == AskPolicy::Deny {
            return Err(CoreError::Conflict(
                "this run does not ask the person: decide yourself and say why in your report"
                    .into(),
            ));
        }
        let timeout_at = (policy.human_ask == AskPolicy::Timeout).then(|| {
            Utc::now()
                + chrono::Duration::milliseconds((policy.ask_timeout_minutes * 60_000.0) as i64)
        });
        let default = a.default.or_else(|| a.options.first().cloned());
        if timeout_at.is_some() && default.is_none() {
            return Err(CoreError::Conflict(format!(
                "questions in this run fall back to their default after {} minutes: give options or a default",
                policy.ask_timeout_minutes
            )));
        }
        let task_id = match &a.task {
            Some(key) => Some(self.find_task(caller, key).await?.id),
            None => None,
        };
        // A retried ask (same open question) is the same question.
        if let Some(same) = self
            .store()
            .list_human_items(true)
            .await?
            .into_iter()
            .find(|i| i.from == caller.id && i.text == a.text && i.kind == HumanItemKind::Question)
        {
            return Ok(json!({ "question_id": same.id, "repeated": true }));
        }
        let item = HumanItem {
            id: item_id("q"),
            detail: None,
            kind: HumanItemKind::Question,
            from: caller.id,
            run_id: run.as_ref().map(|r| r.id),
            task_id,
            text: a.text,
            options: a.options,
            default,
            timeout_at,
            status: HumanItemStatus::Open,
            answer: None,
            answered_at: None,
            action: None,
            created_at: Utc::now(),
        };
        self.store().put_human_item(&item).await?;
        self.publish(EventKind::HumanAsked { item: item.clone() })
            .await;
        let next = match timeout_at {
            Some(at) => format!(
                "End your turn. The answer arrives as a message; without one by {} the default ({}) is used and you are told so.",
                at.to_rfc3339(),
                item.default.as_deref().unwrap_or_default()
            ),
            None => "End your turn; the answer arrives as a message.".to_owned(),
        };
        Ok(json!({ "question_id": item.id, "next": next }))
    }

    pub(crate) async fn human_notify(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            text: String,
            #[serde(default)]
            task: Option<String>,
        }
        let a: A = crate::tools::args(input)?;
        let (run, _) = self.policy_for(caller.id).await?;
        let task_id = match &a.task {
            Some(key) => Some(self.find_task(caller, key).await?.id),
            None => None,
        };
        let item = HumanItem {
            id: item_id("n"),
            detail: None,
            kind: HumanItemKind::Notice,
            from: caller.id,
            run_id: run.map(|r| r.id),
            task_id,
            text: a.text,
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
        self.publish(EventKind::HumanAsked { item: item.clone() })
            .await;
        Ok(json!({ "notice_id": item.id }))
    }

    /// The person's answer. Answering a question that already fell back to its default
    /// tells the agent that the person decided otherwise.
    pub async fn answer_human(&self, id: &str, answer: String) -> Result<HumanItem, CoreError> {
        // Read under the card's lock: a second 开始 that arrived while the first was
        // starting the run finds the card answered.
        let _card = self.lock_card(id).await;
        let mut item = self.store().get_human_item(id).await?;
        if item.kind != HumanItemKind::Question {
            return Err(CoreError::Conflict(format!(
                "{id} is a notice, not a question"
            )));
        }
        let previous = item.status;
        if !matches!(previous, HumanItemStatus::Open | HumanItemStatus::Defaulted) {
            return Err(CoreError::Conflict(format!("{id} is already answered")));
        }
        // Started before the card closes: if the Work cannot start, the card stays.
        if item.action == Some(HumanAction::Work) {
            self.work_decided(&mut item, Some(&answer)).await?;
        }
        let default = item.answer.clone();
        item.status = HumanItemStatus::Answered;
        item.answer = Some(answer.clone());
        item.answered_at = Some(Utc::now());
        self.store().put_human_item(&item).await?;
        self.publish(EventKind::HumanAnswered { item: item.clone() })
            .await;

        if item.action == Some(HumanAction::Budget) {
            self.budget_decided(&item, &answer).await?;
            return Ok(item);
        }
        if item.action == Some(HumanAction::Work) {
            return Ok(item);
        }
        // The agent waits on its own request; the answer goes there, not as a message.
        if item.action == Some(HumanAction::Approval) {
            self.approval_decided(&item.id, Some(&answer));
            return Ok(item);
        }
        let text = if previous == HumanItemStatus::Defaulted {
            format!(
                "The person has now answered your question {id} (you went ahead with the default \"{}\"): {answer}",
                default.unwrap_or_default()
            )
        } else {
            format!(
                "The person answered your question {id} (\"{}\"): {answer}",
                item.text
            )
        };
        self.tell(item.from, MessageSource::Human, text).await;
        Ok(item)
    }

    pub async fn dismiss_human(&self, id: &str) -> Result<(), CoreError> {
        let _card = self.lock_card(id).await;
        let mut item = self.store().get_human_item(id).await?;
        if item.status == HumanItemStatus::Answered {
            return Err(CoreError::Conflict(format!("{id} is already answered")));
        }
        if item.status != HumanItemStatus::Open {
            return Ok(());
        }
        item.status = HumanItemStatus::Dismissed;
        // When it was decided, for the chat's record of a Work that did not start.
        item.answered_at = Some(Utc::now());
        self.store().put_human_item(&item).await?;
        if item.action == Some(HumanAction::Approval) {
            self.approval_decided(&item.id, None);
        }
        if item.action == Some(HumanAction::Work) {
            self.work_decided(&mut item, None).await?;
        }
        self.publish(EventKind::HumanDismissed { id: id.to_owned() })
            .await;
        Ok(())
    }

    /// Questions past their timeout take their default answer.
    pub async fn expire_questions(&self) -> Result<(), CoreError> {
        let now = Utc::now();
        for mut item in self.store().list_human_items(true).await? {
            let Some(at) = item.timeout_at else { continue };
            if at > now || item.kind != HumanItemKind::Question {
                continue;
            }
            item.status = HumanItemStatus::Defaulted;
            item.answer = item.default.clone();
            item.answered_at = Some(now);
            self.store().put_human_item(&item).await?;
            self.publish(EventKind::HumanAnswered { item: item.clone() })
                .await;
            if item.action == Some(HumanAction::Approval) {
                self.approval_decided(&item.id, item.default.as_deref());
                continue;
            }
            let text = format!(
                "No answer to your question {} in time; go ahead with the default: {}",
                item.id,
                item.default.as_deref().unwrap_or("(none)")
            );
            self.tell(item.from, MessageSource::Notice, text).await;
        }
        Ok(())
    }

    /// Every answer in the run that was a default rather than the person's choice.
    pub async fn defaulted_decisions(&self, run: RunId) -> Result<Vec<HumanItem>, CoreError> {
        Ok(self
            .store()
            .list_human_items(false)
            .await?
            .into_iter()
            .filter(|i| i.run_id == Some(run) && i.status == HumanItemStatus::Defaulted)
            .collect())
    }

    async fn tell(&self, agent: AgentId, from: MessageSource, text: String) {
        let host = self.host.clone();
        tokio::spawn(async move {
            if let Err(err) = host
                .send_envelope(agent, Envelope::new(from, text), false)
                .await
            {
                tracing::warn!(%agent, error = %err, "cannot deliver an answer");
            }
        });
    }

    /// The hub measured `percent` of the run's `what` cap. At 80% the lead hears about it;
    /// at 100% the run pauses and the person decides.
    pub async fn budget_reached(
        &self,
        run_id: RunId,
        what: &str,
        percent: u32,
    ) -> Result<(), CoreError> {
        let _guard = self.lock(run_id).await;
        let mut run = self.store().get_run(run_id).await?;
        if !(run.status == RunStatus::Running
            || (run.status == RunStatus::Planning && run.config.policy.continuation.is_some()))
        {
            return Ok(());
        }
        self.publish(EventKind::RunBudget {
            id: run_id,
            what: what.to_owned(),
            percent,
        })
        .await;
        if percent < 100 {
            if percent >= WARN_PERCENT {
                let line = format!("The run has used {percent}% of its {what} budget.");
                let _ = self
                    .host
                    .notify(run.planner, &format!("budget:{what}"), &line)
                    .await;
            }
            return Ok(());
        }
        self.set_run_status(
            &mut run,
            RunStatus::Paused,
            Some(format!("{what} budget used up")),
        )
        .await?;
        for agent in self.run_agents(&run).await? {
            let _ = self.host.interrupt(agent).await;
        }
        let item = HumanItem {
            id: item_id("q"),
            detail: None,
            kind: HumanItemKind::Question,
            from: run.planner,
            run_id: Some(run_id),
            task_id: None,
            text: format!("「{}」用完了 {what} 预算，已暂停。要继续吗？", run.title),
            options: vec!["继续，预算再加 50%".into(), "取消这个 run".into()],
            default: None,
            timeout_at: None,
            status: HumanItemStatus::Open,
            answer: None,
            answered_at: None,
            action: Some(HumanAction::Budget),
            created_at: Utc::now(),
        };
        self.store().put_human_item(&item).await?;
        self.publish(EventKind::HumanAsked { item }).await;
        Ok(())
    }

    async fn budget_decided(&self, item: &HumanItem, answer: &str) -> Result<(), CoreError> {
        let Some(run_id) = item.run_id else {
            return Ok(());
        };
        if answer == item.options.get(1).map(String::as_str).unwrap_or_default() {
            self.cancel(run_id).await?;
            return Ok(());
        }
        let _guard = self.lock(run_id).await;
        let mut run = self.store().get_run(run_id).await?;
        if run.status != RunStatus::Paused {
            return Ok(());
        }
        let budget = &mut run.config.policy.budget;
        budget.max_tokens = budget.max_tokens.map(|t| t + t / 2);
        budget.max_usd = budget.max_usd.map(|u| u * 1.5);
        budget.max_minutes = budget.max_minutes.map(|m| m + m / 2);
        self.set_run_status(&mut run, RunStatus::Running, None)
            .await?;
        for agent in self.run_agents(&run).await? {
            let _ = self
                .host
                .notify(
                    agent,
                    "budget:resumed",
                    "The person raised the budget; carry on where you stopped.",
                )
                .await;
        }
        self.schedule(&mut run).await
    }

    /// The lead and everyone on a task that has not ended.
    async fn run_agents(&self, run: &Run) -> Result<Vec<AgentId>, CoreError> {
        let mut agents = vec![run.planner];
        for task in self.store().list_tasks(run.id).await? {
            if task.status.is_terminal() {
                continue;
            }
            agents.extend([task.assignee, task.reviewer].into_iter().flatten());
        }
        Ok(agents)
    }
}
