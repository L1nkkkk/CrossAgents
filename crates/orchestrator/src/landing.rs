//! The end of a run (docs/tasks/H1.md §5.9): landing the run branch on the base branch and
//! closing the run. The lead does both when the policy lets it; the person can always.

use chrono::Utc;
use crossagents_core::{
    Agent, CoreError, EventKind, HumanItem, HumanItemKind, HumanItemStatus, LandMode, LandPolicy,
    Landing, Run, RunId, RunStatus, TaskStatus,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::Orchestrator;
use crate::tools::args;

impl Orchestrator {
    /// The run the caller leads and is still open.
    async fn led_run(&self, caller: &Agent) -> Result<Run, CoreError> {
        match self.run_of(caller.id).await? {
            Some((run, _)) if run.planner == caller.id => Ok(run),
            _ => Err(CoreError::Conflict("you lead no run".into())),
        }
    }

    pub(crate) async fn run_land_tool(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            #[serde(default)]
            mode: Option<LandMode>,
        }
        let a: A = args(input)?;
        let run = self.led_run(caller).await?;
        let mode = match (run.config.policy.land, a.mode) {
            (LandPolicy::Manual, _) => {
                return Err(CoreError::Conflict(
                    "this run leaves landing to the person: finish it with run_finish, they land it from the run page"
                        .into(),
                ));
            }
            (LandPolicy::Pr, Some(mode)) if mode != LandMode::Pr => {
                return Err(CoreError::Conflict(
                    "this run may only open a pull request (mode pr)".into(),
                ));
            }
            (LandPolicy::Pr, _) => LandMode::Pr,
            (LandPolicy::Auto, mode) => mode.unwrap_or(LandMode::Merge),
        };
        let landing = self.land(run.id, mode, &caller.name).await?;
        Ok(json!({ "landed": landing }))
    }

    /// Lands the run branch on its base branch. Every task must have ended and at least one
    /// merged. Landing twice returns the first landing.
    pub async fn land(&self, id: RunId, mode: LandMode, by: &str) -> Result<Landing, CoreError> {
        let _guard = self.lock(id).await;
        let mut run = self.store.get_run(id).await?;
        if let Some(landing) = &run.landed {
            return Ok(landing.clone());
        }
        if !matches!(run.status, RunStatus::Running | RunStatus::Completed) {
            return Err(CoreError::Conflict(format!("the run is {:?}", run.status)));
        }
        let tasks = self.store.list_tasks(id).await?;
        let open: Vec<&str> = tasks
            .iter()
            .filter(|t| !t.status.is_terminal())
            .map(|t| t.key.as_str())
            .collect();
        if !open.is_empty() {
            return Err(CoreError::Conflict(format!(
                "tasks still open: {}; accept, drop or cancel them first",
                open.join(", ")
            )));
        }
        if !tasks.iter().any(|t| t.status == TaskStatus::Merged) {
            return Err(CoreError::Conflict(
                "nothing was merged, so there is nothing to land".into(),
            ));
        }
        let reference = self
            .workspaces
            .land(
                &run.repo,
                &run.base_branch,
                &run.branch,
                mode,
                &format!("crossagents: {}", run.title),
            )
            .await?;
        let landing = Landing {
            mode,
            by: by.to_owned(),
            reference,
            at: Utc::now(),
        };
        run.landed = Some(landing.clone());
        run.updated_at = Utc::now();
        self.store.update_run(&run).await?;
        self.publish(EventKind::RunLanded {
            id,
            landing: landing.clone(),
        })
        .await;
        Ok(landing)
    }

    pub(crate) async fn run_finish_tool(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            summary: String,
            #[serde(default)]
            cancel_open: bool,
        }
        let a: A = args(input)?;
        let run = self.led_run(caller).await?;
        let run = self.finish(run.id, Some(a.summary), a.cancel_open).await?;
        Ok(json!({ "status": run.status, "landed": run.landed }))
    }

    /// Closes the run: completed if anything merged, else failed. Open tasks are refused
    /// unless `cancel_open`. The person gets the summary, with every decision that was a
    /// default rather than theirs.
    pub async fn finish(
        &self,
        id: RunId,
        summary: Option<String>,
        cancel_open: bool,
    ) -> Result<Run, CoreError> {
        let open = self
            .store
            .list_tasks(id)
            .await?
            .into_iter()
            .filter(|t| !t.status.is_terminal())
            .count();
        if open > 0 && !cancel_open {
            return Err(CoreError::Conflict(format!(
                "{open} task(s) still open; finish them, or pass cancel_open to cancel them"
            )));
        }
        let mut run = self.store.get_run(id).await?;
        if run.status.is_terminal() {
            return Ok(run);
        }
        if open > 0 {
            for mut task in self.store.list_tasks(id).await? {
                if task.status.is_terminal() {
                    continue;
                }
                let _guard = self.lock(id).await;
                self.release_agents(&mut task).await;
                self.cleanup_worktree(&run, &task, false).await;
                task.report.failure = Some("cancelled when the run finished".into());
                self.transition(&mut task, TaskStatus::Cancelled).await?;
            }
        }
        let _guard = self.lock(id).await;
        run = self.store.get_run(id).await?;
        run.summary = summary.clone();
        let tasks = self.store.list_tasks(id).await?;
        self.finish_run(&mut run, &tasks).await?;

        // The closing note for the person: the lead's words, then what the hub knows.
        let mut text = summary.unwrap_or_else(|| format!("「{}」已结束。", run.title));
        let merged = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Merged)
            .count();
        text.push_str(&format!("\n\n— {merged}/{} 个任务已合并", tasks.len()));
        match &run.landed {
            Some(l) => text.push_str(&format!(
                "，已落地到 {}（{}）",
                run.base_branch, l.reference
            )),
            None if merged > 0 => text.push_str(&format!("，结果在 {}，等你落地", run.branch)),
            None => {}
        }
        let defaults = self.defaulted_decisions(id).await?;
        if !defaults.is_empty() {
            text.push_str("\n\n没等到你回答、按默认执行的决定：");
            for item in &defaults {
                text.push_str(&format!(
                    "\n- {} → {}",
                    item.text.lines().next().unwrap_or_default(),
                    item.answer.as_deref().unwrap_or("?")
                ));
            }
        }
        let note = HumanItem {
            id: format!("n-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]),
            kind: HumanItemKind::Notice,
            from: run.planner,
            run_id: Some(id),
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
        self.store.put_human_item(&note).await?;
        self.publish(EventKind::HumanAsked { item: note }).await;
        Ok(run)
    }
}
