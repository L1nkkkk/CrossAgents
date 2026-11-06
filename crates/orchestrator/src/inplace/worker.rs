//! Mixed dispatch for a lead in the person's own directory (docs/tasks/LEAD.md §5, §6, §10).
//!
//! The person's directory is never a worker's working copy. At the first dispatch a
//! [`crate::snapshot`] of it is committed on a ref of the hub's, and a hub-owned integration
//! worktree (`<worktrees>/<run>/integration`, on a branch of its own) is built from that
//! snapshot. Workers get their own worktrees off that branch, so two of them run side by
//! side; accepted results merge there, exactly as they would in a run's own checkout.
//!
//! The results reach the person's directory once, when the run ends — or when the person (or
//! the lead) asks for it explicitly. [`crate::apply`] does the three-way merge, so changes the
//! person made meanwhile are theirs and a collision writes nothing. The hub commits nothing
//! there and leaves the staging area alone.
//!
//! What was dispatched and from where is kept in [`state`], beside the worktrees, so a hub
//! that restarts knows the snapshot, the integration branch, and what was already applied.

use std::path::PathBuf;

use chrono::Utc;
use crossagents_core::{CoreError, Run, RunId};
use serde_json::{Value, json};

use super::state::{self, Integration, Snapshot};
use crate::apply;
use crate::engine::{Orchestrator, short};
use crate::workspace::worktree_path;

impl Orchestrator {
    /// The hub-owned working copy this run gathers accepted results in. A path, not a
    /// promise that it exists.
    pub(crate) fn inplace_dir(&self, run: &Run) -> PathBuf {
        worktree_path(&self.config.worktrees, &short(run.id), "integration")
    }

    /// The branch of that working copy; also the prefix of each task's branch.
    pub(crate) fn inplace_branch(&self, run: &Run) -> String {
        format!("crossagents/{}", short(run.id))
    }

    /// The branch a task of `run` starts from: the run's own, or for a lead in place the
    /// hub's integration branch.
    pub(crate) fn task_base_branch(&self, run: &Run) -> String {
        if run.branch.is_empty() {
            self.inplace_branch(run)
        } else {
            run.branch.clone()
        }
    }

    fn inplace_state_path(&self, run: &Run) -> PathBuf {
        self.config
            .worktrees
            .join(short(run.id))
            .join("inplace.json")
    }

    fn inplace_apply_path(&self, run: &Run) -> PathBuf {
        self.config
            .worktrees
            .join(short(run.id))
            .join("inplace-apply.json")
    }

    /// The integration working copy and branch, when one was made and is still there.
    pub(crate) async fn inplace_base(
        &self,
        run: &Run,
    ) -> Result<Option<(String, PathBuf)>, CoreError> {
        let state = state::read(&self.inplace_state_path(run))?;
        let Some(integration) = state.integration else {
            return Ok(None);
        };
        if !integration.path.exists()
            || !self
                .ws_for(run)
                .has_branch(&run.repo, &integration.branch)
                .await
        {
            return Ok(None);
        }
        Ok(Some((integration.branch, integration.path)))
    }

    /// The directory the run's checks are about: the integration working copy when workers
    /// gathered results there, else the person's own directory (P2).
    pub(crate) async fn inplace_checks_dir(&self, run: &Run) -> Option<PathBuf> {
        if !run.leads_in_place() || !run.versioned() {
            return None;
        }
        match self.inplace_base(run).await {
            Ok(Some((_, dir))) => Some(dir),
            _ => None,
        }
    }

    /// Makes sure `run` has its snapshot and integration working copy, taking the snapshot
    /// at the first dispatch (docs/tasks/LEAD.md §5). Returns the branch and directory tasks
    /// and accepted results use.
    pub(crate) async fn ensure_inplace_base(
        &self,
        run: &Run,
    ) -> Result<(String, PathBuf), CoreError> {
        if let Some(base) = self.inplace_base(run).await? {
            return Ok(base);
        }
        let path = self.inplace_state_path(run);
        let mut state = state::read(&path)?;
        state.run = Some(run.id);
        // The snapshot is taken once: later workers start from the newest accepted result,
        // not from a new photograph of the person's directory (§5).
        let snapshot = match state.snapshot.clone() {
            Some(snapshot) => snapshot,
            None => {
                let captured = crate::snapshot::capture(&run.repo, &short(run.id), 1).await?;
                let snapshot = Snapshot {
                    commit: captured.commit,
                    tree: captured.tree,
                    number: captured.number,
                    head: captured.head,
                    at: Utc::now(),
                };
                state.snapshot = Some(snapshot.clone());
                // Written before the worktree is made: a hub that stops next finds the same
                // snapshot again instead of taking another photograph (§5).
                state::write(&path, &state)?;
                if !captured.secrets.is_empty() {
                    self.tell_about(
                        run,
                        format!(
                            "派工快照里有疑似密钥文件：{}。它们会交给 Worker 检出，请确认不该出现在代码里，或把它们移出工作区、写进 .gitignore。",
                            captured.secrets.join("、")
                        ),
                    )
                    .await;
                }
                if !captured.skipped.is_empty() {
                    self.tell_about(
                        run,
                        format!(
                            "派工快照跳过了超过 {} MiB 的文件：{}。Worker 看到的是版本库里的旧版本；需要它们参与修改时，请缩小文件或另行处理。",
                            crate::snapshot::MAX_FILE >> 20,
                            captured.skipped.join("、")
                        ),
                    )
                    .await;
                }
                snapshot
            }
        };
        let branch = self.inplace_branch(run);
        let dir = self.inplace_dir(run);
        let mut ready = false;
        let mut made = false;
        if self.ws_for(run).has_branch(&run.repo, &branch).await {
            if dir.exists() {
                // Made by a hub that stopped before recording it: a branch and a worktree of
                // the hub's own, at the snapshot, so it is adopted as it is.
                tracing::info!(run = %run.id, dir = %dir.display(), "adopting an integration worktree the last hub made");
                ready = true;
            } else if let Err(err) = self.ws_for(run).checkout(&run.repo, &branch, &dir).await {
                // The branch may still hold earlier accepted results: it is not thrown away
                // lightly, only when it cannot be checked out.
                tracing::warn!(run = %run.id, error = %err, "cannot check the integration branch out again; it is made from the snapshot");
                self.ws_for(run).delete_branch(&run.repo, &branch).await?;
            } else {
                ready = true;
                made = true;
            }
        }
        if !ready {
            // A directory left on no branch is the hub's own (never the person's); it goes.
            if dir.exists()
                && let Err(err) = self.ws_for(run).remove(&run.repo, &dir, None).await
            {
                tracing::warn!(run = %run.id, error = %err, "cannot remove a half-made integration worktree");
                let _ = std::fs::remove_dir_all(&dir);
            }
            if self.ws_for(run).has_branch(&run.repo, &branch).await {
                self.ws_for(run).delete_branch(&run.repo, &branch).await?;
            }
            self.ws_for(run)
                .create(&run.repo, &branch, &snapshot.commit, &dir)
                .await?;
            made = true;
        }
        if made {
            // What a fresh checkout needs before anyone works in it (TOOLCHAIN.md §7); a
            // failure is written on the run's record and told to the lead, not fatal here.
            let prepared = crate::setup::prepare(
                &dir,
                &self.config.worktrees,
                &self.config.locks,
                self.config.shims.as_deref(),
            )
            .await;
            if let Some(note) = prepared.note() {
                self.notice(
                    run.planner,
                    &format!("run:{}:setup", run.id),
                    &format!("集成工作区 {}：{note}", dir.display()),
                )
                .await;
            }
        }
        state.integration = Some(Integration {
            branch: branch.clone(),
            path: dir.clone(),
            at: Utc::now(),
        });
        state::write(&path, &state)?;
        Ok((branch, dir))
    }

    /// The workers' result as a diff against the dispatch snapshot, and whether it has been
    /// applied to the person's directory (docs/tasks/LEAD.md §6): the lead cannot read it in
    /// the person's directory before an apply, so the hub shows it here.
    pub async fn inplace_changes(&self, id: RunId) -> Result<Value, CoreError> {
        let run = self.store.get_run(id).await?;
        if !(run.leads_in_place() && run.versioned()) {
            return Ok(json!({ "outcome": "not_applicable" }));
        }
        let state = state::read(&self.inplace_state_path(&run))?;
        let (Some(integration), Some(snapshot)) = (state.integration, state.snapshot) else {
            return Ok(json!({ "outcome": "nothing" }));
        };
        if !integration.path.exists() {
            return Ok(json!({
                "outcome": "gone",
                "directory": integration.path.display().to_string(),
            }));
        }
        let head = self
            .ws_for(&run)
            .head(&integration.path)
            .await?
            .trim()
            .to_owned();
        // Read-only: the integration worktree's index is left as it is, as the person's would
        // be (docs/tasks/LEAD.md §4).
        let diff = crate::inplace::diff(&integration.path, &snapshot.commit).await?;
        let stat = crate::inplace::diff_stat(&integration.path, &snapshot.commit).await?;
        let applied = apply::State::read(&self.inplace_apply_path(&run))
            .ok()
            .and_then(|state| state.applied)
            .map(|applied| applied.result);
        Ok(json!({
            "outcome": "ok",
            "snapshot": snapshot.commit,
            "directory": integration.path.display().to_string(),
            "integration": head,
            "applied": applied,
            "stat": stat,
            "diff": diff,
        }))
    }

    /// Applies the workers' accepted results to the person's directory, three-way and
    /// idempotently (`crate::apply`). The explicit entry and the end of a run both use it.
    pub async fn apply_in_place(&self, id: RunId) -> Result<Value, CoreError> {
        let run = self.store.get_run(id).await?;
        let _guard = self.lock(id).await;
        self.apply_in_place_locked(&run).await
    }

    async fn apply_in_place_locked(&self, run: &Run) -> Result<Value, CoreError> {
        if !(run.leads_in_place() && run.versioned()) {
            return Ok(json!({
                "outcome": "not_applicable",
                "reason": "this run's workers do not work in your directory",
            }));
        }
        let state = state::read(&self.inplace_state_path(run))?;
        let (Some(integration), Some(snapshot)) = (state.integration, state.snapshot) else {
            return Ok(json!({
                "outcome": "nothing",
                "reason": "no worker results: the hub keeps no integration workspace for this run",
            }));
        };
        if !integration.path.exists() {
            return Ok(json!({
                "outcome": "nothing",
                "reason": format!("the integration workspace {} is gone", integration.path.display()),
            }));
        }
        let head = self
            .ws_for(run)
            .head(&integration.path)
            .await?
            .trim()
            .to_owned();
        if head.is_empty() {
            return Ok(json!({
                "outcome": "nothing",
                "reason": "the integration workspace has no commit to apply",
            }));
        }
        // A new run may not take the directory while it is written in (LEAD §7); nor may an
        // apply write under a run that took it after this one ended.
        let _gate = self.dir_gate(&run.repo).await;
        if let Some(holder) = self.dir_holder(&run.repo).await?
            && holder.id != run.id
        {
            return Ok(json!({
                "outcome": "busy",
                "reason": crate::dirlock::busy(&holder, &run.repo),
            }));
        }
        let outcome = apply::apply(
            &run.repo,
            &snapshot.commit,
            &head,
            &self.inplace_apply_path(run),
        )
        .await?;
        match &outcome {
            apply::Outcome::Applied { files, .. } => {
                let note = apply_note(run, &outcome).unwrap_or_default();
                self.tell_about(
                    run,
                    format!(
                        "Worker 结果已应用到 {}：{} 个文件。修改未提交，你的暂存区和分支没有动。{note}",
                        run.repo.display(),
                        files.len()
                    ),
                )
                .await;
            }
            apply::Outcome::Conflict { files, .. } => {
                self.tell_about(
                    run,
                    format!(
                        "Worker 结果和 {} 的当前内容冲突，这次没有写入任何文件：{}。目录保持原样；处理后在 Run 页点「应用到原目录」或让 Lead 重试。",
                        run.repo.display(),
                        files.join("、")
                    ),
                )
                .await;
            }
            apply::Outcome::AlreadyApplied { .. } | apply::Outcome::UpToDate { .. } => {}
        }
        let mut value = serde_json::to_value(&outcome)
            .map_err(|err| CoreError::Storage(format!("cannot describe the apply: {err}")))?;
        if let Some(map) = value.as_object_mut() {
            map.insert("checks".into(), json!(checks_word(run)));
            if let Some(note) = apply_note(run, &outcome) {
                map.insert("note".into(), json!(note));
            }
        }
        Ok(value)
    }

    /// After a run ends: write what its workers did into the person's directory, once
    /// (docs/tasks/LEAD.md §10 item 2). Failure is said, never fatal — the run itself ended.
    pub(crate) async fn apply_in_place_after_finish(&self, run: &Run) {
        if !(run.leads_in_place() && run.versioned()) {
            return;
        }
        match self.inplace_base(run).await {
            Ok(Some(_)) => {}
            _ => return,
        }
        if let Err(err) = self.apply_in_place(run.id).await {
            tracing::warn!(run = %run.id, error = %err, "cannot apply the workers' results");
            self.tell_about(
                run,
                format!(
                    "Run 结束了，但 Worker 结果没能应用到 {}：{err}。结果留在集成工作区，问题处理后可在 Run 页再次应用。",
                    run.repo.display()
                ),
            )
            .await;
        }
    }

    /// At hub start: an apply that a hub which stopped was in the middle of, or had not begun
    /// after the run ended, is finished. Idempotent, so one already done writes nothing.
    pub async fn recover_inplace_applies(&self) {
        let runs = match self.store.list_runs().await {
            Ok(runs) => runs,
            Err(err) => {
                tracing::warn!(error = %err, "cannot read runs to finish in-place applies");
                return;
            }
        };
        for run in runs {
            if !run.status.is_terminal() || !(run.leads_in_place() && run.versioned()) {
                continue;
            }
            let state = match state::read(&self.inplace_state_path(&run)) {
                Ok(state) => state,
                Err(err) => {
                    tracing::warn!(run = %run.id, error = %err, "cannot read an in-place record");
                    continue;
                }
            };
            let Some(integration) = state.integration else {
                continue;
            };
            let Some(_snapshot) = state.snapshot else {
                continue;
            };
            if !integration.path.exists() {
                continue;
            }
            let head = match self.ws_for(&run).head(&integration.path).await {
                Ok(head) => head.trim().to_owned(),
                Err(_) => continue,
            };
            let apply_state = match apply::State::read(&self.inplace_apply_path(&run)) {
                Ok(state) => state,
                Err(err) => {
                    tracing::warn!(run = %run.id, error = %err, "cannot read an apply record");
                    continue;
                }
            };
            let waiting = apply_state.pending.is_some()
                || apply_state
                    .applied
                    .as_ref()
                    .is_none_or(|applied| applied.result != head);
            if !waiting {
                continue;
            }
            match self.apply_in_place(run.id).await {
                Ok(value) => {
                    tracing::info!(run = %run.id, outcome = %value["outcome"], "finished an in-place apply at startup")
                }
                Err(err) => {
                    tracing::warn!(run = %run.id, error = %err, "cannot finish an in-place apply at startup")
                }
            }
        }
    }

    /// Forgets the run's in-place records after its integration workspace went: the run
    /// itself is gone, so nothing will recover from them (the snapshot ref stays).
    pub(crate) fn forget_inplace_state(&self, run: &Run) {
        let _ = std::fs::remove_file(self.inplace_state_path(run));
        let _ = std::fs::remove_file(self.inplace_apply_path(run));
    }

    /// A notice for the person about this run (docs/tasks/LEAD.md §5, §6).
    async fn tell_about(&self, run: &Run, text: String) {
        if let Err(err) = self.tell_person(run.planner, Some(run.id), text).await {
            tracing::warn!(run = %run.id, error = %err, "cannot tell the person about the apply");
        }
    }

    /// Removes the hub-owned integration worktree and its branch once a run is gone or its
    /// lead left (docs/tasks/LEAD.md §6). The person's directory and the snapshot refs are
    /// not touched. An apply that is half-written keeps the workspace: its journal is what
    /// [`Self::recover_inplace_applies`] finishes after a restart.
    pub(crate) async fn cleanup_inplace_base(
        &self,
        run: &Run,
    ) -> Result<InplaceCleanup, CoreError> {
        if !(run.leads_in_place() && run.versioned()) {
            return Ok(InplaceCleanup::Nothing);
        }
        let state_path = self.inplace_state_path(run);
        let mut state = state::read(&state_path)?;
        let Some(integration) = state.integration.clone() else {
            // A hub that stopped between making the branch and recording it leaves only the
            // branch; it is the hub's own name, never a user's.
            let branch = self.inplace_branch(run);
            if self.ws_for(run).has_branch(&run.repo, &branch).await {
                self.ws_for(run).delete_branch(&run.repo, &branch).await?;
            }
            return Ok(InplaceCleanup::Nothing);
        };
        let apply_state = apply::State::read(&self.inplace_apply_path(run))?;
        if let Some(pending) = &apply_state.pending {
            return Ok(InplaceCleanup::Kept(format!(
                "keeping the integration workspace {}: an apply ({}) is unfinished and is resumed from its record",
                integration.path.display(),
                pending.result
            )));
        }
        if integration.path.exists() {
            if !self.checkout_unused(run, &integration.path).await? {
                return Ok(InplaceCleanup::Kept(format!(
                    "keeping the integration workspace {}: it is in use",
                    integration.path.display()
                )));
            }
            self.ws_for(run)
                .remove(&run.repo, &integration.path, Some(&integration.branch))
                .await?;
        } else if self
            .ws_for(run)
            .has_branch(&run.repo, &integration.branch)
            .await
        {
            self.ws_for(run)
                .delete_branch(&run.repo, &integration.branch)
                .await?;
        }
        state.integration = None;
        state::write(&state_path, &state)?;
        Ok(InplaceCleanup::Removed(integration.path))
    }
}

/// What [`Orchestrator::cleanup_inplace_base`] did with a run's integration workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InplaceCleanup {
    /// The run never had one, or it was already gone.
    Nothing,
    /// The worktree and its branch were removed.
    Removed(PathBuf),
    /// Kept, for the reason given: an unfinished apply, or a worktree still in use.
    Kept(String),
}

/// What the run's checks were, as the apply report says it: they ran on the integration
/// result, not in the person's directory (docs/tasks/LEAD.md §6).
fn checks_word(run: &Run) -> &'static str {
    match run.checks_state {
        Some(crossagents_core::ChecksState::Ran) if run.checks.iter().all(|check| check.passed) => {
            "integration_passed"
        }
        Some(crossagents_core::ChecksState::Ran) => "integration_failed",
        Some(crossagents_core::ChecksState::NotConfigured) => "not_configured",
        Some(crossagents_core::ChecksState::NotApplicable { .. }) => "not_applicable",
        None => "not_run",
    }
}

fn rerun_note(files: &[String], rerun: Option<&apply::Rerun>) -> String {
    let prefix = format!("原目录合并了个人改动：{}。", files.join("、"));
    match rerun {
        None => format!("{prefix}原目录检查未执行。"),
        Some(report) if report.error.is_some() => format!(
            "{prefix}原目录检查未执行：{}。",
            report.error.as_deref().unwrap_or_default()
        ),
        Some(report) if report.results.is_empty() => {
            format!("{prefix}没有匹配的检查，未执行复跑。")
        }
        Some(report) if report.results.iter().all(|check| check.passed) => {
            format!("{prefix}原目录复跑检查通过。")
        }
        Some(_) => format!("{prefix}原目录复跑检查失败；详见复跑结果。"),
    }
}

#[cfg(test)]
mod apply_note_tests {
    use super::*;

    #[test]
    fn run_apply_message_reports_rerun_without_old_instruction() {
        let files = vec!["src/a.rs".to_owned()];
        for (passed, expected) in [(true, "通过"), (false, "失败")] {
            let report = apply::Rerun {
                results: vec![crossagents_core::CheckResult {
                    command: "check".into(),
                    exit_code: Some(if passed { 0 } else { 1 }),
                    passed,
                    output: String::new(),
                }],
                error: None,
            };
            let note = rerun_note(&files, Some(&report));
            assert!(note.contains(expected), "{note}");
            assert!(!note.contains("请在原目录再跑一次"));
        }
        let report = apply::Rerun {
            results: vec![],
            error: None,
        };
        assert!(rerun_note(&files, Some(&report)).contains("没有匹配的检查"));
        let report = apply::Rerun {
            results: vec![],
            error: Some("bad config".into()),
        };
        assert!(rerun_note(&files, Some(&report)).contains("bad config"));
    }
}
/// What the person should know about the checks after an apply.
fn apply_note(run: &Run, outcome: &apply::Outcome) -> Option<String> {
    match outcome {
        apply::Outcome::Applied {
            own_changes,
            checked_in_dir,
            rerun,
            ..
        } => Some(if !*checked_in_dir {
            rerun_note(own_changes, rerun.as_ref())
        } else {
            match run.checks_state {
                Some(crossagents_core::ChecksState::Ran) => {
                    "项目检查是在集成工作区对集成结果跑的；这次写入与其一致，无需复跑。".to_owned()
                }
                Some(crossagents_core::ChecksState::NotConfigured) => {
                    "项目没有在 crossagents.toml 里声明检查，无需复跑。".to_owned()
                }
                Some(crossagents_core::ChecksState::NotApplicable { .. }) => {
                    "改动不涉及声明的检查，未执行验证；按项目规则判断是否需要复跑。".to_owned()
                }
                None => "这次运行没有执行项目检查；如需验证，请在原目录跑一遍项目检查。".to_owned(),
            }
        }),
        apply::Outcome::UpToDate { .. } => Some("目录里已经是集成结果，无需复跑检查。".to_owned()),
        _ => None,
    }
}

/// The lead's `run_apply` tool: write the workers' results into the person's directory now.
impl Orchestrator {
    pub(crate) async fn run_apply_tool(
        &self,
        caller: &crossagents_core::Agent,
        _input: Value,
    ) -> Result<Value, CoreError> {
        let Some((run, _)) = self.run_of(caller.id).await? else {
            return Err(CoreError::Conflict("you lead no run".into()));
        };
        if run.planner != caller.id {
            return Err(CoreError::Conflict("you lead no run".into()));
        }
        self.apply_in_place(run.id).await
    }
}
