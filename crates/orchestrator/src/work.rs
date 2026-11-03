//! A conversation turning into a Work (docs/tasks/W1.md §6): the agent proposes it with
//! `work_start`, the person starts it or declines with the card that puts in front of them.
//! Opening a branch and a worktree is something the person sees, so the agent never does it
//! on its own.
//! `run_next` continues an existing work group immediately from the person's new request;
//! a run that has no group yet (from before groups existed) gets the one that carries it on.

use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, AgentKey, CoreError, EventKind, HumanAction, HumanItem, HumanItemKind,
    HumanItemStatus, LandPolicy, PlanApproval, Run, RunConfig, RunPolicy, RunStatus, Work, WorkId,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::{NewRun, Orchestrator, first_line};

/// The card's answers: the first starts the Work.
const START: &str = "开始";
const DECLINE: &str = "不用";

/// Added to a Work card when the run its lead ended never landed (docs/tasks/R1.md §2.2).
const UNLANDED: &str = "上一个 run 还没落地，新 Work 从 ";

/// The person's request as the card shows it: the detail without what the card says about
/// the run before it, which is for the person, not part of the brief.
fn brief_of(item: &HumanItem) -> String {
    let detail = item.detail.clone().unwrap_or_else(|| item.text.clone());
    match detail.find(UNLANDED) {
        Some(at) => detail[..at].trim_end().to_owned(),
        None => detail,
    }
}

impl Orchestrator {
    pub(crate) async fn run_next(&self, caller: &Agent, input: Value) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            title: String,
            request: String,
            from: Option<String>,
        }
        let a: A = crate::tools::args(input)?;
        let run = self
            .run_of(caller.id)
            .await?
            .map(|(r, _)| r)
            .filter(|r| r.planner == caller.id)
            .ok_or_else(|| CoreError::Conflict("run_next requires a run's lead".into()))?;
        if !run.status.is_terminal() {
            return Err(CoreError::Conflict(
                "run_next requires a terminal run; this run is still in progress".into(),
            ));
        }
        // Every run belongs to a work group. One without (from before groups existed) gets
        // the group that carries it on, so the next round reuses its checkout and branch.
        let work = {
            let _guard = self.lock(run.id).await;
            let mut run = self.store().get_run(run.id).await?;
            self.work_of_run(&mut run).await?.id
        };
        let next = self
            .create_work_round(work, a.request, Some(a.title), a.from.as_deref())
            .await?;
        Ok(json!({ "run_id": next.id, "work_id": work, "status": next.status }))
    }

    /// Records a work group. Every run is in one: a group of its own when it starts, the
    /// same group as the rounds that follow it.
    pub async fn create_work_group(
        &self,
        title: &str,
        repo: std::path::PathBuf,
        lead: AgentId,
    ) -> Result<Work, CoreError> {
        let now = Utc::now();
        let work = Work {
            id: WorkId::new(),
            title: title.to_owned(),
            repo,
            lead,
            created_at: now,
            updated_at: now,
        };
        self.store().insert_work(&work).await?;
        self.publish(EventKind::WorkCreated { work: work.clone() })
            .await;
        Ok(work)
    }

    /// The group `run` is in, made for it when it has none (a run from before groups
    /// existed, or one whose lead carries on from it): the round that follows then reuses
    /// its checkout and branch instead of starting over from the base branch.
    pub async fn work_of_run(&self, run: &mut Run) -> Result<Work, CoreError> {
        if let Some(id) = run.work_id {
            return self.store().get_work(id).await;
        }
        let work = self
            .create_work_group(&run.title, run.repo.clone(), run.planner)
            .await?;
        run.work_id = Some(work.id);
        run.updated_at = Utc::now();
        self.store().update_run(run).await?;
        Ok(work)
    }

    /// Shared by the conversation tool and the work group's next-round API.
    pub async fn create_work_round(
        &self,
        id: crossagents_core::WorkId,
        request: String,
        title: Option<String>,
        from: Option<&str>,
    ) -> Result<Run, CoreError> {
        if from.is_some_and(|from| from != "base") {
            return Err(CoreError::Conflict("from only accepts base".into()));
        }
        let work = self.store().get_work(id).await?;
        let previous = self
            .store()
            .list_runs()
            .await?
            .into_iter()
            .find(|r| r.work_id == Some(id));
        let config = previous
            .as_ref()
            .map(|r| r.config.clone())
            .unwrap_or(RunConfig {
                worker: work.lead,
                escalate: None,
                reviewer: None,
                auto_approve: false,
                max_parallel: RunConfig::DEFAULT_MAX_PARALLEL,
                max_attempts: RunConfig::DEFAULT_MAX_ATTEMPTS,
                team: Vec::new(),
                policy: RunPolicy::default(),
            });
        self.create_run_for_work_from(
            NewRun {
                origin_chat_id: None,
                request,
                title,
                planner: work.lead,
                config,
                repo: Some(work.repo),
                base_branch: None,
                attachments: Vec::new(),
            },
            None,
            false,
            Some(id),
            from == Some("base"),
        )
        .await
    }

    /// The parent's next message carries this result; recording it never wakes the parent.
    pub(crate) async fn tell_origin_later(&self, run: &Run) {
        let Some(parent) = run.origin_chat_id.filter(|id| *id != run.planner) else {
            return;
        };
        let status = match run.status {
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
            _ => return,
        };
        let result = first_line(
            run.summary.as_deref().or(run.note.as_deref()).unwrap_or(""),
            240,
        );
        let landing = match &run.landed {
            Some(landed) => format!("landed on {} ({})", run.base_branch, landed.reference),
            None => format!("not landed; run branch: {}", run.branch),
        };
        let line = format!(
            "CrossAgents work result: \"{}\" (run {}) {status}. {result} {landing}.",
            first_line(&run.title, 90),
            run.id,
        );
        self.tell_quietly(parent, &format!("run:{}", run.id), &line)
            .await;
    }

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
            /// The caller saying it means a Work of its own, not the next round of the work
            /// group it already leads: the one way past the check below.
            #[serde(default)]
            separate: bool,
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
            .find(|r| r.planner == caller.id && !r.status.is_terminal())
        {
            return Err(CoreError::Conflict(format!(
                "you lead run {} already: work there (task_create, plan_submit) instead",
                run.id
            )));
        }
        // A lead whose round ended keeps `work_start`, but has to say it means a Work of its
        // own: the next round of the group it is in belongs to `run_next`. The hub does not
        // judge which one the person's words are (docs/tasks/G1.md §3.6) — it only makes the
        // choice explicit, and `separate` is how the caller states it.
        if !a.separate {
            if let Some((run, _)) = self.run_of(caller.id).await? {
                if run.planner == caller.id && run.status.is_terminal() {
                    if let Some(work) = run.work_id {
                        return Err(CoreError::Conflict(format!(
                            "you are in work group {work}: to carry on there call run_next \
                             instead; if the person really asked for a separate, independent \
                             Work, call work_start again with separate: true"
                        )));
                    }
                }
            }
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
        let mut detail = a.request;
        // The run this lead ended did not land: the new one starts from the base branch,
        // without that run's changes. Landing is the person's call, so the card says it
        // rather than merging the old branch in on its own.
        if let Some(old) = self.unlanded_home_of(caller).await? {
            detail.push_str(&format!(
                "\n\n{UNLANDED}{} 开始，不包含它的改动。",
                old.base_branch
            ));
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let item = HumanItem {
            id: format!("w-{}", &id[..10]),
            detail: Some(detail),
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

    /// The last run `caller` led in its workspace, when it ended without landing: a new Work
    /// starts from the base branch again, and does not carry that run's changes.
    async fn unlanded_home_of(
        &self,
        caller: &Agent,
    ) -> Result<Option<crossagents_core::Run>, CoreError> {
        // Newest first.
        let last = self.store().list_runs().await?.into_iter().find(|r| {
            (r.planner == caller.id || r.origin_chat_id == Some(caller.id))
                && r.repo == caller.home()
                && !r.in_place
        });
        Ok(last.filter(|r| r.status.is_terminal() && r.landed.is_none()))
    }

    /// The person's answer to a proposed Work; `None` when they closed the card.
    pub(crate) async fn work_decided(
        &self,
        item: &mut HumanItem,
        answer: Option<&str>,
    ) -> Result<(), CoreError> {
        if answer != Some(START) {
            // A fact for its next message, not an instruction: declining a branch neither
            // cancels the request nor asks for it to be done here now (docs/USAGE-TRIM.md §3.2).
            let line = format!(
                "The person did not accept your proposed Work \"{}\", so no run was created. What to do next is up to the person's next message.",
                first_line(&item.text, 90)
            );
            self.tell_quietly(item.from, &format!("work:{}", item.id), &line)
                .await;
            return Ok(());
        }
        let proposer = item.from;
        let agent = self
            .store()
            .get_agent(&crossagents_core::AgentKey::Id(proposer))
            .await?;
        // The proposer's workspace, wherever it works now; the run starts from the current
        // branch there.
        let (repo, base_branch) = (agent.home().to_path_buf(), None);
        let mut run = self
            .create_run_with_choice(
                NewRun {
                    origin_chat_id: Some(proposer),
                    request: brief_of(item),
                    title: Some(item.text.clone()),
                    planner: proposer,
                    repo: Some(repo),
                    base_branch,
                    attachments: Vec::new(),
                    config: RunConfig {
                        worker: proposer,
                        escalate: None,
                        reviewer: None,
                        auto_approve: false,
                        max_parallel: RunConfig::DEFAULT_MAX_PARALLEL,
                        max_attempts: RunConfig::DEFAULT_MAX_ATTEMPTS,
                        team: Vec::new(),
                        // The person said go on doing it, not on how: the lead writes what each
                        // session must do, and the person approves that plan and picks the model
                        // for every task before anything starts (AGENT-WORKSPACE §6.1). Landing
                        // stays automatic unless they pick 我审批 on the plan.
                        policy: RunPolicy {
                            plan_approval: PlanApproval::Required,
                            land: LandPolicy::Auto,
                            ..RunPolicy::default()
                        },
                    },
                },
                Some(true),
                false,
            )
            .await?;
        // The work group this Work starts: its next rounds call run_next from the lead.
        self.work_of_run(&mut run).await?;
        item.run_id = Some(run.id);
        self.store().put_human_item(item).await?;
        self.tell_origin_started(&run).await;
        Ok(())
    }

    /// The source chat hears that its Work started with its next message, without a turn of
    /// its own just to acknowledge it. It shares the run's topic with the closing result, so
    /// the newest state is what it reads; a run that already ended gets its result written
    /// again after this, since that one is written only once the run is saved as ended
    /// (docs/USAGE-TRIM.md §3.3).
    pub(crate) async fn tell_origin_started(&self, run: &Run) {
        let Some(parent) = run.origin_chat_id.filter(|id| *id != run.planner) else {
            return;
        };
        let lead = match self.store().get_agent(&AgentKey::Id(run.planner)).await {
            Ok(lead) => lead.name,
            Err(_) => run.planner.to_string(),
        };
        let line = format!(
            "CrossAgents work started: \"{}\" (run {}), led by {lead} on branch {}, from your brief. This conversation stays as it is; the Work goes on there.",
            first_line(&run.title, 90),
            run.id,
            run.branch
        );
        self.tell_quietly(parent, &format!("run:{}", run.id), &line)
            .await;
        match self.store().get_run(run.id).await {
            Ok(now) if now.status.is_terminal() => self.tell_origin_later(&now).await,
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(run = %run.id, error = %err, "cannot recheck the run after its start notice")
            }
        }
    }

    /// A notice kept for the agent's next message. Never falls back to waking it: the GUI
    /// shows the same fact.
    async fn tell_quietly(&self, agent: AgentId, topic: &str, line: &str) {
        if let Err(err) = self.host.tell_later(agent, topic, line).await {
            tracing::warn!(%agent, topic, error = %err, "cannot keep a notice for the agent's next message");
        }
    }
}
