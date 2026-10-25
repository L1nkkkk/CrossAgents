//! A conversation turning into a Work (docs/tasks/W1.md §6): the agent proposes it with
//! `work_start`, the person starts it or declines with the card that puts in front of them.
//! Opening a branch and a worktree is something the person sees, so the agent never does it
//! on its own.

use chrono::Utc;
use crossagents_core::{
    Agent, CoreError, EventKind, HumanAction, HumanItem, HumanItemKind, HumanItemStatus,
    MessageSource, RunConfig, RunPolicy,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::{NewRun, Orchestrator, first_line};

/// The card's answers: the first starts the Work.
const START: &str = "开始";
const DECLINE: &str = "不用";

impl Orchestrator {
    pub(crate) async fn work_start(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            request: String,
            #[serde(default)]
            title: Option<String>,
        }
        let a: A = crate::tools::args(input)?;
        if a.request.trim().is_empty() {
            return Err(CoreError::Conflict("the request is empty".into()));
        }
        if let Some(run) = self
            .store()
            .list_runs()
            .await?
            .into_iter()
            .find(|r| r.planner == caller.id)
        {
            return Err(CoreError::Conflict(format!(
                "you lead run {} already: work there (task_create, plan_submit) instead",
                run.id
            )));
        }
        if !self.workspaces.versioned(&caller.cwd).await {
            return Err(CoreError::Conflict(format!(
                "{} is not a git repository, so there is no branch to work on: do the work here",
                caller.cwd.display()
            )));
        }
        // A retried proposal (one still open) is the same card.
        if let Some(same) = self
            .store()
            .list_human_items(true)
            .await?
            .into_iter()
            .find(|i| i.from == caller.id && i.action == Some(HumanAction::Work))
        {
            return Ok(json!({ "card_id": same.id, "repeated": true }));
        }
        let title = a
            .title
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| first_line(&a.request, 60));
        let id = uuid::Uuid::new_v4().simple().to_string();
        let item = HumanItem {
            id: format!("w-{}", &id[..10]),
            detail: Some(a.request),
            kind: HumanItemKind::Question,
            from: caller.id,
            run_id: None,
            task_id: None,
            text: title,
            options: vec![START.into(), DECLINE.into()],
            default: None,
            timeout_at: None,
            status: HumanItemStatus::Open,
            answer: None,
            answered_at: None,
            action: Some(HumanAction::Work),
            created_at: Utc::now(),
        };
        self.store().put_human_item(&item).await?;
        self.publish(EventKind::HumanAsked { item: item.clone() })
            .await;
        Ok(json!({
            "card_id": item.id,
            "next": "The person sees your proposal with 开始 / 不用. End your turn; you are told which they chose."
        }))
    }

    /// The person's answer to a proposed Work; `None` when they closed the card.
    pub(crate) async fn work_decided(
        &self,
        item: &mut HumanItem,
        answer: Option<&str>,
    ) -> Result<(), CoreError> {
        if answer != Some(START) {
            self.deliver(
                item.from,
                MessageSource::Human,
                "The person chose not to open a Work for this: do it here in the conversation, or keep talking.".into(),
            );
            return Ok(());
        }
        let proposer = item.from;
        let run = self
            .create_run(NewRun {
                request: item.detail.clone().unwrap_or_else(|| item.text.clone()),
                title: Some(item.text.clone()),
                planner: proposer,
                config: RunConfig {
                    worker: proposer,
                    escalate: None,
                    reviewer: None,
                    auto_approve: false,
                    max_parallel: RunConfig::DEFAULT_MAX_PARALLEL,
                    max_attempts: RunConfig::DEFAULT_MAX_ATTEMPTS,
                    team: Vec::new(),
                    policy: RunPolicy::default(),
                },
            })
            .await?;
        item.run_id = Some(run.id);
        self.store().put_human_item(item).await?;
        let lead = self
            .store()
            .get_agent(&crossagents_core::AgentKey::Id(run.planner))
            .await?;
        self.deliver(
            proposer,
            MessageSource::Human,
            format!(
                "The person started the Work \"{}\" (run {}): {} leads it on branch {}, with your request as its brief. This conversation stays as it is; the Work goes on there.",
                run.title, run.id, lead.name, run.branch
            ),
        );
        Ok(())
    }
}
