//! The end of a run (docs/tasks/H1.md §5.9): landing the run branch on the base branch and
//! closing the run. The lead does both when the policy lets it; the person can always.

use chrono::Utc;
use crossagents_core::{
    Agent, CheckResult, ChecksState, CoreError, EventKind, HumanItem, HumanItemKind,
    HumanItemStatus, LandMode, LandPolicy, Landing, Run, RunId, RunStatus, Task, TaskId,
    TaskReport, TaskSpec, TaskStatus,
};
use serde::Deserialize;
use serde::Serialize;
use serde_json::{Value, json};

use crate::engine::{Orchestrator, first_line};
use crate::tools::args;
use crate::verify::all_passed;
use crate::workspace::Synced;
use crate::{brief, project};

/// What landing did: landed, or waits for a task that resolves conflicts with the base.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LandOutcome {
    Landed(Landing),
    Deferred {
        task: String,
        conflicts: Vec<String>,
    },
}

/// Why a run did not land yet, while its sync task runs; cleared once it lands.
const SYNC_NOTE_END: &str = "land again once it is merged";

/// How a closing note says the result waits for the person; cleared once it lands.
const AWAITING_LAND: &str = "等你落地";

/// How many times the lead's landing checks again when the checkout moved during its checks.
const LAND_ATTEMPTS: usize = 3;

impl Orchestrator {
    /// The run the caller leads and is still open.
    async fn led_run(&self, caller: &Agent) -> Result<Run, CoreError> {
        match self.run_of(caller.id).await? {
            Some((run, _)) if run.planner == caller.id => Ok(run),
            _ => Err(CoreError::Conflict("you lead no run".into())),
        }
    }

    /// The project's run checks (`crossagents.toml`) in the run's checkout, before the lead
    /// lands or finishes a run that changed something (docs/tasks/DOGFOOD.md §9). The results
    /// stay on the run; a failure refuses `tool` with the output, for the lead to fix. The
    /// person is not held to them: they land or end the run from the run page.
    async fn require_run_checks(&self, run: &Run, tool: &str) -> Result<(), CoreError> {
        let mut checked_run = run.clone();
        if tool == "run_land" {
            checked_run.base_branch = self.round_base(run).await?.0;
        }
        let run = &checked_run;
        let dir = self.integration_path(run);
        if run.landed.is_some() || !dir.exists() {
            return Ok(());
        }
        let tasks = self.store.list_tasks(run.id).await?;
        if !self.has_changes(run, &tasks).await? {
            // Nothing to check, so nothing for a result to be about: an older result must not
            // stand for what is here now (docs/tasks/LEAD.md §7).
            return self.bound_checks_to(run.id, None).await;
        }
        // What the checks are about, taken before they run: for a lead in the person's
        // directory that is the content, since there is no commit to bind them to.
        let snapshot = self.checks_snapshot_of(run).await;
        let checks = project::run_checks(&dir)
            .map_err(|e| CoreError::Conflict(format!("{e}; fix it, then call {tool} again")))?;
        // Not checked is said as such, never left to read as passed.
        if checks.checks.is_empty() {
            return self
                .record_checks(run.id, ChecksState::NotConfigured, Vec::new(), snapshot)
                .await;
        }
        // What the run changes against its base, so that a check scoped to other paths is
        // skipped. Not knowing (an error) runs them all.
        let changed = self
            .ws_for(run)
            .changed_files(&dir, &run.base_branch)
            .await
            .unwrap_or_else(|err| {
                tracing::warn!(run = %run.id, error = %err, "cannot list the run's changes; running every check");
                Vec::new()
            });
        let commands = checks.commands_for(&changed);
        if commands.is_empty() {
            let declared = u32::try_from(checks.checks.len()).unwrap_or(u32::MAX);
            return self
                .record_checks(
                    run.id,
                    ChecksState::NotApplicable { declared },
                    Vec::new(),
                    snapshot,
                )
                .await;
        }
        let results = self
            .verifier
            .check_within(&dir, &commands, checks.timeout)
            .await;
        self.record_checks(run.id, ChecksState::Ran, results.clone(), snapshot)
            .await?;
        if all_passed(&results) {
            return Ok(());
        }
        let Some(bad) = results.iter().find(|r| !r.passed) else {
            return Ok(());
        };
        // The checks hold the checkout's build directory (docs/tasks/LOCKS.md §6); when it did
        // not come free in time nothing ran, and that says nothing about the change.
        if bad.exit_code == Some(crossagents_core::exec::EXIT_TEMPFAIL) {
            return Err(CoreError::Conflict(format!(
                "the project's checks ({}) did not run: {}\nThe checks wait for whatever \
                 builds in this checkout; this is not a problem in your change: call {tool} \
                 again when it is free.",
                project::FILE,
                bad.output
            )));
        }
        // A file the system says another process holds is the machine, not the change: say
        // so, or the lead goes looking for a bug that is not there.
        if crate::running::in_use_failure(&bad.output) {
            return Err(CoreError::Conflict(format!(
                "the project's checks ({}) could not run cleanly: a file they use was held by \
                 another process (a build or test running at the same time?), which is not a \
                 problem in your change: {}\nMake sure nothing else is building or testing in \
                 this checkout, then call {tool} again.",
                project::FILE,
                brief::checks(&results)
            )));
        }
        // What the toolchain's own messages suggest is only noted: they do not prove it.
        let presets = project::run_env(&dir)
            .map(|env| env.presets)
            .unwrap_or_else(|_| crate::presets::detect(&dir));
        let hints = crate::running::hints(&bad.output, &presets);
        let maybe = if hints.is_empty() {
            String::new()
        } else {
            format!(
                "\npossibly related: {} in the output, which another build running at the same \
                 time can cause.",
                hints.join(", ")
            )
        };
        Err(CoreError::Conflict(format!(
            "the project's checks ({}) failed: {}{maybe}\nFix it in your checkout and call {tool} \
             again. If a check itself is wrong, tell the person: they can still land or end the \
             run from the run page.",
            project::FILE,
            brief::checks(&results)
        )))
    }

    /// Keeps on the run how its checks went, and what content they were about (`snapshot`,
    /// for a lead in the person's own directory).
    async fn record_checks(
        &self,
        id: RunId,
        state: ChecksState,
        results: Vec<CheckResult>,
        snapshot: Option<String>,
    ) -> Result<(), CoreError> {
        let _guard = self.lock(id).await;
        let mut stored = self.store.get_run(id).await?;
        stored.checks = results;
        stored.checks_state = Some(state);
        stored.checks_snapshot = snapshot;
        stored.updated_at = Utc::now();
        self.store.update_run(&stored).await
    }

    /// Unbinds the checks on record: nothing they found is about what is there now.
    async fn bound_checks_to(&self, id: RunId, snapshot: Option<String>) -> Result<(), CoreError> {
        let _guard = self.lock(id).await;
        let mut stored = self.store.get_run(id).await?;
        if stored.checks_snapshot == snapshot {
            return Ok(());
        }
        stored.checks_snapshot = snapshot;
        stored.updated_at = Utc::now();
        self.store.update_run(&stored).await
    }

    /// What a run's checks are about: the content of the person's directory for a lead that
    /// works there, since no commit stands for it. `None` when a commit does (a checkout of
    /// the run's own), when the directory is not versioned (nothing binds a result), or when
    /// the content cannot be read (said, not turned into a pass).
    async fn checks_snapshot_of(&self, run: &Run) -> Option<String> {
        if !(run.leads_in_place() && run.versioned()) {
            return None;
        }
        let dir = self.integration_path(run);
        match crate::inplace::snapshot(&dir).await {
            Ok(snapshot) => Some(snapshot),
            Err(err) => {
                tracing::warn!(run = %run.id, error = %err, "cannot fingerprint the directory the checks ran on");
                None
            }
        }
    }

    /// Refuses to close a run whose lead works in the person's directory when the content
    /// moved since its checks ran: the result on record is about other content, so it counts
    /// neither as passed nor as failed — the checks run again (docs/tasks/LEAD.md §7).
    async fn require_checks_current(&self, run: &Run) -> Result<(), CoreError> {
        if !(run.leads_in_place() && run.versioned()) || run.checks_state != Some(ChecksState::Ran)
        {
            return Ok(());
        }
        let Some(recorded) = run.checks_snapshot.as_deref() else {
            return Ok(());
        };
        let dir = self.integration_path(run);
        let now = crate::inplace::snapshot(&dir).await?;
        if now == recorded {
            return Ok(());
        }
        Err(CoreError::Conflict(format!(
            "the content of {} changed after the checks ran, so their result is about other \
             content and counts neither way. Run them again (run_finish does) and then close the \
             run",
            dir.display()
        )))
    }

    pub(crate) async fn run_land_tool(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        let id = self.led_run(caller).await?.id;
        let out = self.land_as_lead(caller, input).await?;
        // Out of the run's locks: the cleanup takes them. The lead is mid-turn here, so a
        // finished run's checkout goes once that turn ends.
        if !out["landed"].is_null() {
            self.cleanup_after_land(id).await;
        }
        Ok(out)
    }

    async fn land_as_lead(&self, caller: &Agent, input: Value) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            #[serde(default)]
            mode: Option<LandMode>,
            #[serde(default)]
            message: Option<String>,
        }
        let a: A = args(input)?;
        let run = self.led_run(caller).await?;
        let _work_guard = if let Some(id) = run.work_id {
            Some(self.lock_card(&format!("work:{id}")).await)
        } else {
            None
        };
        let mode = match (run.config.policy.land, a.mode) {
            (LandPolicy::Manual, _) => {
                return Err(CoreError::Conflict(
                    "this run leaves landing to the person: finish it with run_finish, they land it from the run page; do not merge it into the base branch yourself"
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
        let message = a
            .message
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty());
        let json = |outcome| match outcome {
            LandOutcome::Landed(landing) => json!({ "landed": landing }),
            LandOutcome::Deferred { task, conflicts } => json!({
                "landed": null,
                "deferred": { "task": task, "conflicts": conflicts },
            }),
        };
        // What lands is what was checked: the base comes into the run branch first, then
        // the checks run on the result, then it merges. The checks can take minutes, so they
        // run outside the run's lock; should the checkout move meanwhile (the base gained
        // more, the lead committed), it is brought in and checked again.
        for _ in 0..LAND_ATTEMPTS {
            let (run, checked) = {
                let _guard = self.lock(run.id).await;
                let mut run = self.store.get_run(run.id).await?;
                if let Some(outcome) = self.ready_to_land(&mut run).await? {
                    return Ok(json(outcome));
                }
                let head = self.head_of(&run).await?;
                (run, head)
            };
            self.require_run_checks(&run, "run_land").await?;
            let _guard = self.lock(run.id).await;
            let mut run = self.store.get_run(run.id).await?;
            if let Some(outcome) = self.ready_to_land(&mut run).await? {
                return Ok(json(outcome));
            }
            if self.head_of(&run).await? != checked {
                continue;
            }
            let verified = brief::verification(&run);
            let mut out = json(self.merge_run(run, mode, &caller.name, message).await?);
            out["checks"] = verified.into();
            return Ok(out);
        }
        Err(CoreError::Conflict(format!(
            "{} kept changing while the project's checks ran, so what would land was never \
             checked; call run_land again",
            run.base_branch
        )))
    }

    /// The commit the run's checkout has, to tell whether it moved.
    async fn head_of(&self, run: &Run) -> Result<String, CoreError> {
        self.ws_for(run).head(&self.integration_path(run)).await
    }

    /// Lands the run branch on its base branch. Every task must have ended and at least one
    /// merged. Landing twice returns the first landing. What the base gained meanwhile is
    /// merged into the run branch first; when that conflicts, a task to resolve it goes to
    /// the run's worker and landing waits for it (docs/tasks/DOGFOOD.md §4). `message` is the
    /// landing commit's, as the lead wrote it to the project's rules; none gets the hub's.
    /// The project's checks are not run: the person lands from the run page regardless.
    pub async fn land(
        &self,
        id: RunId,
        mode: LandMode,
        by: &str,
        message: Option<&str>,
    ) -> Result<LandOutcome, CoreError> {
        let outcome = self.land_locked(id, mode, by, message).await?;
        // What landed is on the base now: a finished run's checkout goes (cleanup_after_land),
        // out of the run's locks, which the cleanup takes itself.
        if matches!(outcome, LandOutcome::Landed(_)) {
            self.cleanup_after_land(id).await;
        }
        Ok(outcome)
    }

    async fn land_locked(
        &self,
        id: RunId,
        mode: LandMode,
        by: &str,
        message: Option<&str>,
    ) -> Result<LandOutcome, CoreError> {
        let work_id = self.store.get_run(id).await?.work_id;
        let _work_guard = if let Some(id) = work_id {
            Some(self.lock_card(&format!("work:{id}")).await)
        } else {
            None
        };
        let _guard = self.lock(id).await;
        let mut run = self.store.get_run(id).await?;
        if let Some(outcome) = self.ready_to_land(&mut run).await? {
            return Ok(outcome);
        }
        self.merge_run(run, mode, by, message).await
    }

    /// Everything before merging, under the run's lock: refuses a run that cannot land,
    /// commits the lead's edits and brings in what the base gained. `Some` when that settles
    /// it: landed already, or waiting for a task that resolves conflicts with the base.
    async fn ready_to_land(&self, run: &mut Run) -> Result<Option<LandOutcome>, CoreError> {
        let id = run.id;
        if let Some(landing) = &run.landed {
            return Ok(Some(LandOutcome::Landed(landing.clone())));
        }
        // A lead in the person's directory has no branch of the hub's: its changes are
        // already where the person works, and there is nothing to merge anywhere
        // (docs/tasks/LEAD.md §9).
        if run.leads_in_place() && run.versioned() {
            return Err(CoreError::Conflict(
                "this run's lead works in your own directory and the hub keeps no branch for \
                 it: the changes are already there, so there is nothing to land. Close the run \
                 with run_finish"
                    .into(),
            ));
        }
        // Another run whose lead works in this directory holds it: a merge into it would
        // write under that lead (docs/tasks/LEAD.md §7).
        if let Some(holder) = self.dir_holder(&run.repo).await?
            && holder.id != id
        {
            return Err(CoreError::Conflict(crate::dirlock::busy(
                &holder, &run.repo,
            )));
        }
        // A solo lead stays in Planning until it finishes; it can land its own work first.
        // A failed run may still carry work worth keeping. Empty branches are refused below.
        if !matches!(
            run.status,
            RunStatus::Planning | RunStatus::Running | RunStatus::Completed | RunStatus::Failed
        ) {
            return Err(CoreError::Conflict(format!("the run is {:?}", run.status)));
        }
        let tasks = self.store.list_tasks(id).await?;
        let baseline = self.round_base(run).await?.0;
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
        self.commit_lead(run, &format!("crossagents: {}", run.title))
            .await?;
        let mut compared = run.clone();
        compared.base_branch = baseline.clone();
        if !self.has_changes(&compared, &tasks).await? {
            return Err(CoreError::Conflict(
                "the run branch changes nothing, so there is nothing to land".into(),
            ));
        }
        if !run.in_place {
            // A finished run has no integration worktree any more; check the branch out again.
            let shared = self.integration_path(run);
            // An earlier round can still be landed independently while the retained lead
            // is on a later branch. Never commit or switch that lead's checkout for it.
            let historical =
                shared.exists() && self.workspaces.current_branch(&shared).await? != run.branch;
            let integration = if historical {
                crate::workspace::worktree_path(
                    &self.config.worktrees,
                    &crate::engine::short(run.id),
                    "landing",
                )
            } else {
                shared
            };
            if !integration.exists() {
                self.workspaces
                    .checkout(&run.repo, &run.branch, &integration)
                    .await?;
            }
            if let Synced::Conflicts { files, detail } =
                self.workspaces.sync_base(&integration, &baseline).await?
            {
                if historical {
                    return Err(CoreError::Conflict(format!(
                        "the earlier round conflicts with {baseline}; resolve {} on its branch {} before landing: {detail}",
                        integration.display(),
                        run.branch
                    )));
                }
                let key = self
                    .sync_task(run, &tasks, &files, &detail, &baseline)
                    .await?;
                return Ok(Some(LandOutcome::Deferred {
                    task: key,
                    conflicts: files,
                }));
            }
            if historical {
                self.workspaces
                    .remove(&run.repo, &integration, None)
                    .await?;
            }
        }
        Ok(None)
    }

    /// Merges the run branch, made [`Self::ready_to_land`], into its base, under the run's lock.
    async fn merge_run(
        &self,
        mut run: Run,
        mode: LandMode,
        by: &str,
        message: Option<&str>,
    ) -> Result<LandOutcome, CoreError> {
        let id = run.id;
        let stored = self.store.get_run(id).await?;
        let (baseline, parents) = self.round_base(&stored).await?;
        let rounds = self.store.list_runs().await?;
        let mut group: Vec<_> = rounds
            .iter()
            .filter(|r| run.work_id.is_some() && r.work_id == run.work_id)
            .collect();
        group.sort_by_key(|r| (r.created_at, r.id));
        let number = |id| {
            group
                .iter()
                .position(|r| r.id == id)
                .map(|n| n + 1)
                .unwrap_or(1)
        };
        let carried: Vec<_> = parents.into_iter().filter(|r| r.landed.is_none()).collect();
        let landed_ids: Vec<_> = std::iter::once(id)
            .chain(carried.iter().map(|r| r.id))
            .collect();
        let mut message = message
            .map(str::to_owned)
            .unwrap_or_else(|| format!("crossagents: {}", run.title));
        if !carried.is_empty() {
            message.push_str(&format!(
                "\n\n连带落地：{}",
                carried
                    .iter()
                    .map(|r| format!("第 {} 轮（{}）", number(r.id), r.title))
                    .collect::<Vec<_>>()
                    .join("、")
            ));
        }
        run.base_branch = baseline;
        // Read again here, right before the merge: a run may have taken the directory since
        // the checks above (docs/tasks/LEAD.md §7). Landing refuses rather than merge there.
        let holder = self.dir_holder(&run.repo).await?;
        let holder = holder.filter(|h| h.id != id);
        let reference = self
            .ws_for(&run)
            .land_held(
                &run.repo,
                &run.base_branch,
                &run.branch,
                mode,
                &message,
                holder.as_ref(),
            )
            .await?;
        let landing = Landing {
            mode,
            by: by.to_owned(),
            reference,
            at: Utc::now(),
        };
        run.landed = Some(landing.clone());
        if run
            .note
            .as_deref()
            .is_some_and(|n| n.ends_with(SYNC_NOTE_END))
        {
            run.note = None;
        }
        run.updated_at = Utc::now();
        self.store.update_run(&run).await?;
        // The landing is part of what the run did: its history says so too.
        self.record_history(&run).await;
        if run.status.is_terminal() {
            self.tell_origin_later(&run).await;
        }
        self.publish(EventKind::RunLanded {
            id,
            landing: landing.clone(),
        })
        .await;
        for mut parent in carried {
            let _guard = self.lock(parent.id).await;
            parent = self.store.get_run(parent.id).await?;
            if parent.landed.is_some() {
                continue;
            }
            parent.landed = Some(Landing {
                by: format!("随第 {} 轮落地", number(id)),
                ..landing.clone()
            });
            if parent
                .note
                .as_deref()
                .is_some_and(|n| n.ends_with(SYNC_NOTE_END))
            {
                parent.note = None;
            }
            parent.updated_at = Utc::now();
            self.store.update_run(&parent).await?;
            self.record_history(&parent).await;
            self.publish(EventKind::RunLanded {
                id: parent.id,
                landing: parent.landed.clone().unwrap(),
            })
            .await;
            self.tell_origin_later(&parent).await;
        }
        // The closing note asked the person to land; that is done now.
        for item in self.store.list_human_items(true).await? {
            if item.run_id.is_some_and(|id| landed_ids.contains(&id))
                && item.kind == HumanItemKind::Notice
                && item.text.contains(AWAITING_LAND)
            {
                self.dismiss_human(&item.id).await?;
            }
        }
        Ok(LandOutcome::Landed(landing))
    }

    /// `task` was merged: if it was the one resolving the base branch's conflict, the run's
    /// "land again once it is merged" note is done with (it would read as still blocked).
    pub(crate) async fn sync_resolved(&self, run: &mut Run, task: &Task) -> Result<(), CoreError> {
        let resolved = run.note.as_deref().is_some_and(|n| {
            n.ends_with(SYNC_NOTE_END) && n.contains(&format!("task #{} ", task.key))
        });
        if resolved {
            run.note = None;
            run.updated_at = Utc::now();
            self.store.update_run(run).await?;
        }
        Ok(())
    }

    /// A task for the run's worker: merge the base branch into the run branch and resolve
    /// `files`. Reopens a finished run so it gets done. Returns the task's key.
    async fn sync_task(
        &self,
        run: &mut Run,
        tasks: &[Task],
        files: &[String],
        detail: &str,
        baseline: &str,
    ) -> Result<String, CoreError> {
        let stem: String = format!("sync-{baseline}")
            .to_lowercase()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .take(28)
            .collect();
        let key = (1..)
            .map(|n| {
                if n == 1 {
                    stem.clone()
                } else {
                    format!("{stem}-{n}")
                }
            })
            .find(|k| !tasks.iter().any(|t| &t.key == k))
            .expect("some key is free");
        let base = baseline;
        let goal = format!(
            "{base} gained commits after this run started, and they conflict with the run's \
             changes, so the run cannot land. In your worktree, run `git merge {base}`, resolve \
             the conflicts so that both sides' changes survive, run the tests, commit, and \
             task_complete.\n\nConflicting files:\n{}",
            files
                .iter()
                .map(|f| format!("- {f}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let now = Utc::now();
        let task = Task {
            id: TaskId::new(),
            run_id: run.id,
            key: key.clone(),
            title: format!("Resolve conflicts with {base}"),
            spec: TaskSpec {
                goal,
                scope: Default::default(),
                interfaces: Vec::new(),
                acceptance: Vec::new(),
                references: vec![format!("conflict hunks when merging {base}:\n{detail}")],
                ..TaskSpec::default()
            },
            depends_on: Vec::new(),
            issuer: Some(run.planner),
            parent: None,
            assignee: None,
            reviewer: None,
            worker_template: Some(run.config.worker),
            reviewer_template: None,
            worker_fresh: true,
            worker_restore: false,
            reviewer_fresh: true,
            status: TaskStatus::Pending,
            attempts: 0,
            runs: Vec::new(),
            escalated: false,
            nudges: 0,
            branch: None,
            worktree: None,
            report: TaskReport::default(),
            progress: None,
            handoffs: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        self.store.insert_task(&task).await?;
        self.publish(EventKind::TaskStatusChanged {
            run_id: run.id,
            id: task.id,
            key: key.clone(),
            from: TaskStatus::Pending,
            to: TaskStatus::Pending,
            assignee: None,
        })
        .await;
        let note = format!(
            "{base} moved and conflicts with the run in {}; task #{key} resolves it, {SYNC_NOTE_END}",
            files.join(", ")
        );
        if run.status == RunStatus::Running {
            run.note = Some(note.clone());
            run.updated_at = now;
            self.store.update_run(run).await?;
        } else {
            self.set_run_status(run, RunStatus::Running, Some(note.clone()))
                .await?;
        }
        self.notice(
            run.planner,
            &format!("run:{}:sync", run.id),
            &format!("Not landed: {note}."),
        )
        .await;
        self.schedule(run).await?;
        Ok(key)
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
            #[serde(default)]
            message: Option<String>,
        }
        let a: A = args(input)?;
        let run = self.led_run(caller).await?;
        // What the lead changed itself and did not commit, under its message.
        let message = a
            .message
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| first_line(&a.summary, 72));
        self.commit_lead(&run, &message).await?;
        self.require_run_checks(&run, "run_finish").await?;
        let run = self.finish(run.id, Some(a.summary), a.cancel_open).await?;
        // Sorting is its own state: it never holds up the end, it is only counted.
        let unsorted = self.unsorted_issues(run.id).await;
        // So do the expertise candidates waiting for the person (EXPERTISE §6.4).
        let pending_expertise = match self.project_for(&run.repo).await {
            Ok(project) => self.pending_candidates(&project.id).await.unwrap_or(0),
            Err(err) => {
                tracing::warn!(run = %run.id, error = %err, "cannot count expertise candidates");
                0
            }
        };
        Ok(json!({
            "status": run.status,
            "landed": run.landed,
            "checks": brief::verification(&run),
            "unsorted_issues": unsorted,
            "pending_expertise": pending_expertise,
        }))
    }

    /// Closes the run: completed if anything merged, else failed. Open tasks are refused
    /// unless `cancel_open`. The person gets the summary, with every decision that was a
    /// default rather than theirs.
    ///
    /// Once it ended, what it built goes (`remove_finished_targets`): every build directory
    /// of the run but those of tasks still being verified. A run that has landed already loses
    /// its checkout too (`cleanup_after_land`); one that has not keeps it for the person.
    pub async fn finish(
        &self,
        id: RunId,
        summary: Option<String>,
        cancel_open: bool,
    ) -> Result<Run, CoreError> {
        let was_open = !self.store.get_run(id).await?.status.is_terminal();
        let run = self.finish_locked(id, summary, cancel_open).await?;
        if was_open && run.status.is_terminal() {
            self.remove_finished_targets(&run).await;
            if run.landed.is_some() {
                self.cleanup_after_land(id).await;
            }
        }
        Ok(run)
    }

    async fn finish_locked(
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
                crossagents_core::Handoff::end(
                    &mut task.handoffs,
                    task.assignee,
                    crossagents_core::HandoffOutcome::Cancelled,
                );
                let released = self.release_agents(&mut task).await;
                self.cleanup_worktree(&run, &task, false, released.is_empty())
                    .await;
                task.report.failure = Some("cancelled when the run finished".into());
                self.transition(&mut task, TaskStatus::Cancelled).await?;
            }
        }
        let _guard = self.lock(id).await;
        run = self.store.get_run(id).await?;
        run.summary = summary.clone();
        // Ended by the person (or with no message from the lead): keep the lead's edits.
        self.commit_lead(&run, &format!("crossagents: {}", run.title))
            .await?;
        let tasks = self.store.list_tasks(id).await?;
        let changed = self.has_changes(&run, &tasks).await?;
        // Closing is the person's usual way out, and it does not run the checks itself: a
        // result about other content cannot close the run (docs/tasks/LEAD.md §7).
        self.require_checks_current(&run).await?;
        self.finish_run(&mut run, &tasks, changed).await?;

        // The closing note for the person: the lead's words, then what the hub knows.
        let mut text = summary.unwrap_or_else(|| format!("「{}」已结束。", run.title));
        let merged = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Merged)
            .count();
        // A lead that did the work itself has no tasks to count.
        text.push_str("\n\n— ");
        if !tasks.is_empty() {
            text.push_str(&format!("{merged}/{} 个任务已合并，", tasks.len()));
        }
        match &run.landed {
            _ if run.leads_in_place() && changed => {
                text.push_str(&format!("改动已直接写在 {}", run.repo.display()))
            }
            Some(l) => text.push_str(&format!("已落地到 {}（{}）", run.base_branch, l.reference)),
            None if changed => text.push_str(&format!("结果在 {}，{AWAITING_LAND}", run.branch)),
            None => text.push_str("没有改动"),
        }
        match run.checks_state {
            Some(ChecksState::NotConfigured) => text.push_str(&format!(
                "\n验收：{} 没有声明检查，未执行验证",
                project::FILE
            )),
            Some(ChecksState::NotApplicable { declared }) => text.push_str(&format!(
                "\n验收：这次的改动不涉及 {} 里声明的 {declared} 项检查，未执行验证",
                project::FILE
            )),
            _ => {}
        }
        if !run.checks.is_empty() {
            let passed = run.checks.iter().filter(|c| c.passed).count();
            text.push_str(&format!(
                "\n验收（{}）：{passed}/{} 通过",
                project::FILE,
                run.checks.len()
            ));
            if let Some(bad) = run.checks.iter().find(|c| !c.passed) {
                text.push_str(&format!("，没通过的是 `{}`", bad.command));
            }
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
            detail: None,
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

    /// Ended runs whose checkout `lead` lives in.
    async fn runs_lived_in(&self, lead: &Agent) -> Result<Vec<Run>, CoreError> {
        let mut seen = std::collections::HashSet::new();
        Ok(self
            .store
            .list_runs()
            .await?
            .into_iter()
            .filter(|r| {
                r.planner == lead.id
                    && r.status.is_terminal()
                    && self.owns_checkout(r, &self.integration_path(r))
                    && !lead.home().starts_with(self.integration_path(r))
                    && lead.cwd == self.integration_path(r)
            })
            .filter(|r| seen.insert(self.integration_path(r)))
            .collect())
    }

    /// `lead` was archived or is being removed (docs/tasks/W1.md §11): the checkout it kept
    /// after its run ended goes. What it changed since is committed first; the run branch
    /// goes too when nothing on it waits for the person (landed, or nothing changed).
    ///
    /// Its build directories follow the rule the rest of the hub uses (docs/TODO.md §九 ②):
    /// only the ones whose checkout is gone go, and a task still being verified keeps its own,
    /// so a run that other work still uses is not torn down from here.
    pub async fn lead_left(&self, lead: &Agent) -> Result<(), CoreError> {
        for run in self.runs_lived_in(lead).await? {
            let _guard = self.lock(run.id).await;
            let dir = self.integration_path(&run);
            if !dir.exists() {
                continue;
            }
            if self.store.claim_of(lead.id).await?.is_some()
                || self.store.list_agents().await?.iter().any(|a| {
                    a.id != lead.id
                        && (a.home().starts_with(&dir)
                            || (a.cwd.starts_with(&dir) && a.archived_at.is_none()))
                })
            {
                continue;
            }
            let after = self
                .commit_lead(&run, &format!("crossagents: {} (after the run)", run.title))
                .await?;
            let tasks = self.store.list_tasks(run.id).await?;
            let waiting = after || (run.landed.is_none() && self.has_changes(&run, &tasks).await?);
            let branch = (!waiting).then_some(run.branch.as_str());
            self.ws_for(&run).remove(&run.repo, &dir, branch).await?;
            self.prune_run_build(&run).await;
        }
        Ok(())
    }

    /// An archived lead was restored: it gets a checkout of its run branch back, or of a
    /// fresh one from the base when that branch was removed. Then the conversation is back
    /// but what the run did is not, and the person is told: a landing does not say the base
    /// has it now (a pull request may never have merged; a merge may have been reverted).
    pub async fn lead_returned(&self, lead: &Agent) -> Result<(), CoreError> {
        for run in self.runs_lived_in(lead).await? {
            let _guard = self.lock(run.id).await;
            let dir = self.integration_path(&run);
            if dir.exists() {
                continue;
            }
            let ws = self.ws_for(&run);
            if ws.checkout(&run.repo, &run.branch, &dir).await.is_err() {
                ws.create(&run.repo, &run.branch, &run.base_branch, &dir)
                    .await?;
                let landed = match &run.landed {
                    None => String::new(),
                    Some(l) if l.mode == crossagents_core::LandMode::Pr => format!(
                        "；run 曾开过 PR（{}），不一定已经合入 {}",
                        l.reference, run.base_branch
                    ),
                    Some(l) => format!(
                        "；run 曾落地到 {}（{}），没有核对那些改动现在是否还在",
                        run.base_branch, l.reference
                    ),
                };
                self.publish(EventKind::AgentWarning {
                    id: lead.id,
                    message: format!(
                        "对话保留了，但原执行代码没有恢复：run 分支 {} 已经删除，检出按 {} 重新建立{landed}",
                        run.branch, run.base_branch
                    ),
                })
                .await;
            }
        }
        Ok(())
    }
}
