//! The person reviewing work (docs/tasks/DOGFOOD.md §2): a task's diff, a run's total diff,
//! and comments that go straight to the worker as its next turn.

use crossagents_core::model::{ReviewFinding, ReviewSeverity, ReviewVerdict};
use crossagents_core::{CoreError, MessageSource, RunId, Task, TaskId, TaskStatus};

use crate::engine::Orchestrator;

/// The verdict a review submits: the one it names, or — when it names none — the one its
/// `approve` implies. Two answers that disagree are refused, so the reviewer fixes the call
/// instead of leaving the issuer with a contradiction.
///
/// The rule (docs/TODO.md §二十五 第 6 条): `approve=true` cannot carry `do_not_merge`,
/// `approve=false` cannot carry `merge`, and a P0 finding cannot carry `merge` — P0 blocks
/// the merge. Confidence must be a number from 0 to 1.
pub(crate) fn effective_verdict(
    approve: bool,
    verdict: Option<ReviewVerdict>,
    findings: &[ReviewFinding],
) -> Result<ReviewVerdict, CoreError> {
    for finding in findings {
        if !(0.0..=1.0).contains(&finding.confidence) {
            return Err(CoreError::Conflict(format!(
                "finding {:?} has confidence {}; it must be a number from 0 to 1",
                finding.title, finding.confidence
            )));
        }
    }
    let effective = verdict.unwrap_or(if approve {
        ReviewVerdict::Merge
    } else {
        ReviewVerdict::DoNotMerge
    });
    if approve && effective == ReviewVerdict::DoNotMerge {
        return Err(CoreError::Conflict(
            "approve=true but verdict=do_not_merge; approve and verdict must agree (pass approve=false, or verdict=merge)".into(),
        ));
    }
    if !approve && effective == ReviewVerdict::Merge {
        return Err(CoreError::Conflict(
            "approve=false but verdict=merge; approve and verdict must agree (pass approve=true, or verdict=do_not_merge)".into(),
        ));
    }
    if effective == ReviewVerdict::Merge
        && findings.iter().any(|f| f.severity == ReviewSeverity::P0)
    {
        return Err(CoreError::Conflict(
            "the review has a P0 finding but verdict=merge; P0 blocks the merge".into(),
        ));
    }
    Ok(effective)
}

/// The findings most severe first, blocking ones (P0/P1) before the rest. Stable, so findings
/// of one severity keep the reviewer's order.
pub(crate) fn sorted_findings(findings: &[ReviewFinding]) -> Vec<ReviewFinding> {
    let mut sorted = findings.to_vec();
    sorted.sort_by_key(|f| f.severity);
    sorted
}

fn severity_name(severity: ReviewSeverity) -> &'static str {
    match severity {
        ReviewSeverity::P0 => "P0",
        ReviewSeverity::P1 => "P1",
        ReviewSeverity::P2 => "P2",
        ReviewSeverity::P3 => "P3",
    }
}

fn verdict_name(verdict: ReviewVerdict) -> &'static str {
    match verdict {
        ReviewVerdict::Merge => "merge",
        ReviewVerdict::MergeAfterFixes => "merge_after_fixes",
        ReviewVerdict::DoNotMerge => "do_not_merge",
    }
}

/// The reviewer's notes plus its conclusion and findings, P0 first and the blocking ones
/// (P0/P1) before the rest. A call that gives neither a verdict nor a finding gets its notes
/// back unchanged, so an old `review_submit` reads exactly as it did.
pub(crate) fn review_notes(
    notes: &str,
    findings: &[ReviewFinding],
    verdict: Option<ReviewVerdict>,
) -> String {
    let notes = notes.trim_end();
    if findings.is_empty() && verdict.is_none() {
        return notes.to_owned();
    }
    let mut out = String::from(notes);
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    if let Some(verdict) = verdict {
        out.push_str(&format!("Verdict: {}\n", verdict_name(verdict)));
    }
    if !findings.is_empty() {
        out.push_str("Findings, blocking first (P0/P1 block the merge):\n");
        for finding in sorted_findings(findings) {
            let place = match (&finding.file, finding.line) {
                (Some(file), Some(line)) => format!(" — {file}:{line}"),
                (Some(file), None) => format!(" — {file}"),
                (None, _) => String::new(),
            };
            out.push_str(&format!(
                "- {} {} (confidence {:.2}){}\n",
                severity_name(finding.severity),
                finding.title.trim(),
                finding.confidence,
                place
            ));
            if let Some(detail) = finding.detail.as_deref().map(str::trim)
                && !detail.is_empty()
            {
                out.push_str(&format!("  {}\n", detail.replace('\n', "\n  ")));
            }
        }
    }
    out.trim_end().to_owned()
}

/// Bytes of diff handed out at once; more is cut and flagged.
pub const DIFF_LIMIT: usize = 2 * 1024 * 1024;

/// A diff and what it compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changes {
    pub base: String,
    pub head: String,
    pub diff: String,
    pub truncated: bool,
}

/// One remark of the person's on a task's changes: a line (`line`, on the new file unless
/// `old_side`), or the whole file when `line` is `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remark {
    pub path: String,
    pub line: Option<u32>,
    pub old_side: bool,
    pub text: String,
}

impl Changes {
    pub(crate) fn new(base: String, head: String, mut diff: String) -> Self {
        let truncated = diff.len() > DIFF_LIMIT;
        if truncated {
            let mut end = DIFF_LIMIT;
            while !diff.is_char_boundary(end) {
                end -= 1;
            }
            diff.truncate(end);
        }
        Self {
            base,
            head,
            diff,
            truncated,
        }
    }
}

impl Orchestrator {
    /// Everything `task` changed against what it started from, committed or not.
    pub async fn task_changes(&self, run: RunId, task: TaskId) -> Result<Changes, CoreError> {
        let (run, task) = self.task_in(run, task).await?;
        let Some(dir) = task.worktree.clone().filter(|d| d.exists()) else {
            return Err(CoreError::Conflict(match task.status {
                TaskStatus::Merged => format!(
                    "#{} is merged and its worktree is gone; see the run's diff",
                    task.key
                ),
                _ => format!("#{} has no worktree (not started, or ended)", task.key),
            }));
        };
        let (base, _) = self.base_of(&run, &task).await?;
        let diff = self.ws_for(&run).diff(&dir, &base, &[]).await?;
        let head = task.branch.clone().unwrap_or_default();
        Ok(Changes::new(base, head, diff))
    }

    /// What landing the run would bring into its base branch — and, for a lead working in the
    /// person's own directory, what differs there now, since the run has no branch of its own
    /// (docs/tasks/LEAD.md §9). Read-only: the person's staging area is not the hub's, so the
    /// diff never stages an intent to add (`crate::inplace::diff`).
    pub async fn run_changes(&self, run: RunId) -> Result<Changes, CoreError> {
        let run = self.store.get_run(run).await?;
        let diff = if run.leads_in_place() && run.versioned() {
            crate::inplace::diff(&run.repo, &run.base_branch).await?
        } else {
            self.ws_for(&run)
                .branch_diff(&run.repo, &run.base_branch, &run.branch)
                .await?
        };
        Ok(Changes::new(
            run.base_branch.clone(),
            run.branch.clone(),
            diff,
        ))
    }

    /// The person's remarks on `task`, sent to its worker as its next turn. With `rework` the
    /// task goes back to running, as if its issuer had called `task_rework`. A reviewer's
    /// numbered checklist is answered in `review_submit.checklist` (docs/tasks/EXPERTISE.md
    /// §6.3); here the person can only say in words what they want the worker to go by.
    pub async fn review_task(
        &self,
        run: RunId,
        task: TaskId,
        remarks: Vec<Remark>,
        note: Option<String>,
        rework: bool,
    ) -> Result<Task, CoreError> {
        let note = note.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
        if remarks.iter().all(|r| r.text.trim().is_empty()) && note.is_none() && !rework {
            return Err(CoreError::Conflict("write a comment first".into()));
        }
        let _guard = self.lock(run).await;
        let (run, mut task) = self.task_in(run, task).await?;
        let Some(worker) = task.assignee else {
            return Err(CoreError::Conflict(format!(
                "#{} has no worker any more; comment on the run instead",
                task.key
            )));
        };
        let reopen = matches!(
            task.status,
            TaskStatus::Passed | TaskStatus::Stalled | TaskStatus::Reviewing
        );
        match task.status {
            TaskStatus::Running | TaskStatus::WaitingAnswer => {}
            _ if reopen => {}
            status => {
                return Err(CoreError::Conflict(format!("#{} is {status:?}", task.key)));
            }
        }
        if rework && reopen {
            if task.reviewer.take().is_some() {
                self.let_go_task(run.id, task.id, Some(crossagents_core::ClaimRole::Reviewer))
                    .await;
            }
            crossagents_core::Handoff::end(
                &mut task.handoffs,
                Some(worker),
                crossagents_core::HandoffOutcome::Reworked,
            );
            task.attempts += 1;
            task.report.failure = None;
            task.report.related_lessons.clear();
            let said: Vec<&str> = note
                .as_deref()
                .into_iter()
                .chain(remarks.iter().map(|r| r.text.trim()))
                .filter(|t| !t.is_empty())
                .collect();
            if !said.is_empty() {
                task.report.rework.push(said.join("\n"));
            }
            self.transition(&mut task, TaskStatus::Running).await?;
        }
        let text = remarks_text(&task, &remarks, note.as_deref(), rework && reopen);
        self.deliver(worker, MessageSource::Human, text);
        let issuer = task.issuer_or(run.planner);
        let line = if rework && reopen {
            format!(
                "The person sent #{} back to its worker with {} comment(s).",
                task.key,
                remarks.len()
            )
        } else {
            format!(
                "The person commented on #{} ({} comment(s)); its worker has them.",
                task.key,
                remarks.len()
            )
        };
        self.notice(issuer, "task", &line, crossagents_core::NoticeClass::Silent)
            .await;
        Ok(task)
    }

    async fn task_in(
        &self,
        run: RunId,
        task: TaskId,
    ) -> Result<(crossagents_core::Run, Task), CoreError> {
        let (found, task) = self.task_of(task).await?;
        if found.id != run {
            return Err(CoreError::NotFound {
                kind: "task",
                id: task.id.to_string(),
            });
        }
        Ok((found, task))
    }

    /// A review that says changes are needed, on a task whose spec routes it back
    /// (docs/research/multica.md §2.12): `spec.on_reject` sends it to the step or teammate
    /// it names, with the reviewer's notes as what to fix. `Some(who)` when it went there;
    /// `None` when the spec sets no route, names the task's own worker, or has already taken
    /// the work back as often as it may — then the review stands as it did before, and the
    /// issuer decides.
    pub(crate) async fn review_sent_back(
        &self,
        run: &crossagents_core::Run,
        task: &mut Task,
        notes: &str,
        reviewer: &crossagents_core::Agent,
    ) -> Result<Option<String>, CoreError> {
        // Checked before anything changes: at the limit the work stays where it is.
        if self.reject_stopped(run, task).await? || !self.rejects_elsewhere(run, task).await? {
            return Ok(None);
        }
        // A new round: an older verification's report must not land on it.
        task.report.verification = None;
        task.attempts += 1;
        task.report.failure = None;
        task.report.rework.push(notes.trim().to_owned());
        let why = format!("a review by {} said changes are needed", reviewer.name);
        self.send_back(run, task, notes, &why).await
    }
}

/// The person's words, each placed at its file and line, then the task's facts.
fn remarks_text(task: &Task, remarks: &[Remark], note: Option<&str>, rework: bool) -> String {
    let mut out = String::new();
    if let Some(note) = note {
        out.push_str(note);
        out.push_str("\n\n");
    }
    let remarks: Vec<&Remark> = remarks
        .iter()
        .filter(|r| !r.text.trim().is_empty())
        .collect();
    if !remarks.is_empty() {
        out.push_str(&format!("Review comments on #{}:\n", task.key));
        for r in remarks {
            let place = match r.line {
                Some(line) if r.old_side => format!("{} (old line {line})", r.path),
                Some(line) => format!("{}:{line}", r.path),
                None => r.path.clone(),
            };
            out.push_str(&format!("- {place} — {}\n", r.text.trim()));
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "---\nfrom: the person, reviewing task #{}\ntask_id: {}",
        task.key, task.id
    ));
    if rework {
        out.push_str(&format!(
            "\nrework of task #{}\nattempt: {}",
            task.key, task.attempts
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_diffs_are_cut_on_a_char_boundary() {
        let diff = "é".repeat(DIFF_LIMIT);
        let c = Changes::new("main".into(), "run".into(), diff);
        assert!(c.truncated);
        assert!(c.diff.len() <= DIFF_LIMIT);
        let short = Changes::new("main".into(), "run".into(), "+a".into());
        assert!(!short.truncated);
    }
}
