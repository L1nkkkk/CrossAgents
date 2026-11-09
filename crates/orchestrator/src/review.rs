//! Reviews (docs/tasks/DOGFOOD.md §2, docs/TODO.md §二十五): a task's diff, a run's total diff,
//! and a reviewer's verdict on a verified report. The hub opens a review by itself when the task
//! has a reviewer, and sends a change request straight back to the worker that made the work,
//! with the findings numbered (`F1`, `F2`…) so that its answers name the same things. Only what
//! needs a decision reaches the lead: the verdict to accept, a dispute, or a limit reached.

use crossagents_core::model::{
    FindingStatus, ReviewFinding, ReviewSeverity, ReviewVerdict, TaskReport,
};
use crossagents_core::{
    Agent, AgentId, AgentKey, ClaimRole, CoreError, Handoff, HandoffOutcome, MessageSource,
    NoticeClass, Role, Run, RunId, Task, TaskBudget, TaskId, TaskStatus,
};
use serde::Deserialize;

use crate::brief;
use crate::engine::{Orchestrator, worker_name};
use crate::handoff::ReviewReturn;
use crate::verify::all_passed;

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

/// One finding as a reviewer hands it in (`review_submit.findings`). The hub numbers it, and
/// makes it blocking when the reviewer leaves that out: P0 and P1 block.
#[derive(Debug, Clone, Deserialize)]
pub struct FindingInput {
    /// The id of a finding of the previous round that this one still is.
    #[serde(default)]
    pub id: Option<String>,
    pub severity: ReviewSeverity,
    pub confidence: f64,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
    pub title: String,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub blocking: Option<bool>,
    #[serde(default)]
    pub basis: Option<String>,
    #[serde(default)]
    pub verify: Option<String>,
}

impl FindingInput {
    /// The finding before the hub numbers it; its `id` is only the one it claims to carry on.
    pub fn into_finding(self) -> ReviewFinding {
        let blocking = self.blocking.unwrap_or(matches!(
            self.severity,
            ReviewSeverity::P0 | ReviewSeverity::P1
        ));
        ReviewFinding {
            id: self.id.unwrap_or_default().trim().to_owned(),
            severity: self.severity,
            confidence: self.confidence,
            file: self.file,
            line: self.line,
            title: self.title,
            detail: self.detail,
            blocking,
            basis: self.basis,
            verify: self.verify,
        }
    }
}

/// Numbers `findings` for a task: a finding keeps the id it claims when the previous round had
/// one by that id (and no other finding took it); every other gets the next number, `F<n>`,
/// from `seq` on. Ids never repeat within a task (docs/TODO.md §二十五).
pub(crate) fn number_findings(
    findings: &mut [ReviewFinding],
    previous: &[ReviewFinding],
    seq: &mut u32,
) {
    let mut taken: Vec<String> = Vec::new();
    for finding in findings.iter_mut() {
        let carried = !finding.id.is_empty()
            && previous.iter().any(|p| p.id == finding.id)
            && !taken.contains(&finding.id);
        if carried {
            taken.push(finding.id.clone());
            continue;
        }
        *seq += 1;
        finding.id = format!("F{seq}");
        taken.push(finding.id.clone());
    }
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

fn status_name(status: FindingStatus) -> &'static str {
    match status {
        FindingStatus::Fixed => "fixed",
        FindingStatus::NotFixed => "not_fixed",
        FindingStatus::Disputed => "disputed",
    }
}

/// `text` cut to `max` characters, with an ellipsis when it was.
pub(crate) fn cut(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}…")
}

/// One finding as a line: its id, severity, whether it blocks, and its title.
fn finding_line(finding: &ReviewFinding) -> String {
    let blocking = if finding.blocking { " (blocking)" } else { "" };
    let place = match (&finding.file, finding.line) {
        (Some(file), Some(line)) => format!(" — {file}:{line}"),
        (Some(file), None) => format!(" — {file}"),
        (None, _) => String::new(),
    };
    format!(
        "{} {}{blocking} {}{place}",
        finding.id,
        severity_name(finding.severity),
        cut(&finding.title, 120)
    )
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
            out.push_str(&format!(
                "- {} (confidence {:.2})\n",
                finding_line(&finding),
                finding.confidence
            ));
            if let Some(detail) = finding.detail.as_deref().map(str::trim)
                && !detail.is_empty()
            {
                out.push_str(&format!("  {}\n", detail.replace('\n', "\n  ")));
            }
            for (label, text) in [("basis", &finding.basis), ("verify", &finding.verify)] {
                if let Some(text) = text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                    out.push_str(&format!("  {label}: {text}\n"));
                }
            }
        }
    }
    out.trim_end().to_owned()
}

/// The findings a worker must answer, for its next turn: each by its id, with what the
/// reviewer wants and how to check it.
pub(crate) fn findings_for_worker(findings: &[ReviewFinding]) -> String {
    let mut out = String::from(
        "Findings to answer. Reply to each by its id in task_complete's answers: status fixed, not_fixed or disputed, with a note. A disputed finding goes to the lead to decide.\n",
    );
    for finding in sorted_findings(findings) {
        out.push_str(&format!("- {}\n", finding_line(&finding)));
        if let Some(detail) = finding
            .detail
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            out.push_str(&format!("  {}\n", detail.replace('\n', "\n  ")));
        }
        for (label, text) in [("basis", &finding.basis), ("verify", &finding.verify)] {
            if let Some(text) = text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                out.push_str(&format!("  {label}: {text}\n"));
            }
        }
    }
    out
}

/// The last `chars` characters of `text` (its end is what says how a command failed).
fn tail(text: &str, chars: usize) -> String {
    let count = text.chars().count();
    if count <= chars {
        return text.trim_end().to_owned();
    }
    let rest: String = text.chars().skip(count - chars).collect();
    format!("…{}", rest.trim_end())
}

/// What a worker is sent when its checks did not pass: each failing command, and the end of its
/// output, which says what went wrong.
pub(crate) fn checks_note(checks: &[crossagents_core::CheckResult]) -> String {
    let mut out = String::from("The checks did not pass. Fix what they show, then report again.\n");
    for check in checks.iter().filter(|c| !c.passed) {
        out.push_str(&format!("\n$ {}\n", check.command.trim()));
        let shown = tail(&check.output, 2000);
        if !shown.is_empty() {
            out.push_str(&shown);
            out.push('\n');
        }
    }
    out
}

/// The last round's findings and the worker's answers to them, for the next reviewer: a
/// finding that still stands keeps its id. Empty when there is no last round.
pub(crate) fn previous_round(report: &TaskReport) -> String {
    if report.findings.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "The last review's findings, with the worker's answers. A finding that still stands keeps its id (give it back as `id`); a fixed one you may drop.\n",
    );
    for finding in sorted_findings(&report.findings) {
        out.push_str(&format!("- {}", finding_line(&finding)));
        match report.answers.iter().find(|a| a.id == finding.id) {
            Some(answer) => {
                out.push_str(&format!(" — worker: {}", status_name(answer.status)));
                if !answer.note.trim().is_empty() {
                    out.push_str(&format!(": {}", cut(&answer.note, 200)));
                }
            }
            None => out.push_str(" — no answer"),
        }
        out.push('\n');
    }
    out
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

/// What a reviewer submitted, as the hub reads it (`review_submit`).
pub(crate) struct Submission {
    pub approve: bool,
    pub notes: String,
    /// Unnumbered: the hub numbers them.
    pub findings: Vec<ReviewFinding>,
    pub verdict: Option<ReviewVerdict>,
    /// The commit the reviewer read, when it says so.
    pub reviewed_sha: Option<String>,
}

/// Why a review that is not open is refused: it came in already, or belongs to someone else.
fn not_open(task: &Task) -> String {
    if task.status == TaskStatus::Reviewing {
        return format!(
            "#{} is being reviewed by another session (review round {}), not by you; nothing changed",
            task.key, task.report.review_attempt
        );
    }
    if task.report.review_attempt > 0 {
        return format!(
            "the review of #{} (round {}) already came in and ended ({:?}); a repeated or late review changes nothing",
            task.key, task.report.review_attempt, task.status
        );
    }
    format!("you are not reviewing #{}", task.key)
}

/// Whether a reviewer's `given` names the round's commit `current`. Spaces around it and case
/// do not count; a prefix of at least seven characters does, since it can only be the commit
/// the brief names (a reviewer copies the short form people write). Shorter, or another
/// commit's prefix, is a different commit.
fn same_commit(given: &str, current: &str) -> bool {
    let given = given.trim().to_ascii_lowercase();
    let current = current.trim().to_ascii_lowercase();
    if given.is_empty() || current.is_empty() {
        return false;
    }
    given == current || (given.len() >= 7 && current.starts_with(&given))
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
            // A task that waited to be accepted is back with its worker: the notice says so no more.
            self.settle_accept_notice(
                task.issuer_or(run.planner),
                &task,
                &format!("#{} was sent back to its worker by the person.", task.key),
            )
            .await;
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

    /// A review that says changes are needed, on a task whose spec routes it to another
    /// teammate (docs/research/multica.md §2.12): `spec.on_reject` sends it to the step or
    /// teammate it names, with the reviewer's notes as what to fix. `Some(who)` when it went
    /// there. The task's own worker is not this path: [`Self::send_back_to_worker`] takes it.
    pub(crate) async fn review_sent_back(
        &self,
        run: &crossagents_core::Run,
        task: &mut Task,
        notes: &str,
        why: &str,
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
        self.send_back(run, task, notes, why).await
    }

    /// Sends a task that did not pass (its review asked for changes, or its checks failed) back
    /// along its route, the one path both take (spec 2, and brief/common's review loop):
    /// to its own worker, to the teammate `spec.on_reject` names, or to the lead when that
    /// route, or the return limit (`TaskSpec::max_returns`), has been used up. The task is
    /// `Passed` when this is called. The lead hears one quiet line `head` when it went back,
    /// and a decision (`decide`) when it did not.
    pub(crate) async fn return_work(
        &self,
        run: &Run,
        task: &mut Task,
        notes: &str,
        findings: &[ReviewFinding],
        from: AgentId,
        head: &str,
        decide: &str,
    ) -> Result<(), CoreError> {
        let issuer = task.issuer_or(run.planner);
        let stop = task.report.budget_over.clone().map(|over| format!("its budget is used up: {over}")).or_else(|| rework_stop(run, task));
        if let Some(why) = stop {
            self.decide(
                issuer,
                task,
                format!("{decide}, and {why}. Decide: task_rework it, task_assign it, or task_accept it with a reason."),
            )
            .await;
            return Ok(());
        }
        match self.review_return(run, task).await? {
            ReviewReturn::Elsewhere => {
                // `spec.on_reject` names another teammate: it takes the work, as it always has. The
                // return is counted only when it goes: a route that refuses it (its own limit,
                // MAX_BOUNCES, or a route that names nothing) leaves the count as it was.
                task.report.review_returns += 1;
                let why = format!("sent back: {}", head.trim_end_matches('.'));
                match self.review_sent_back(run, task, notes, &why).await? {
                    Some(name) => {
                        self.notice(
                            issuer,
                            &format!("task:{}:review", task.id),
                            &format!("{head}; it went back to {name}."),
                            NoticeClass::Silent,
                        )
                        .await;
                    }
                    None => {
                        task.report.review_returns -= 1;
                        self.decide(
                            issuer,
                            task,
                            format!("{decide}. Its route could not take it. Decide: task_rework it, task_assign it, or task_accept it with a reason."),
                        )
                        .await;
                    }
                }
            }
            ReviewReturn::Stopped(target) => {
                self.decide(
                    issuer,
                    task,
                    format!("{decide}. It was sent back to {target} as often as it may; the hub stops here and it stays as it is. Decide: task_rework it, task_assign it elsewhere, or task_accept it with a reason."),
                )
                .await;
            }
            ReviewReturn::Own => {
                let limit = task.spec.max_returns();
                if task.report.review_returns >= limit || task.assignee.is_none() {
                    let why = if task.assignee.is_none() {
                        "it has no worker any more".to_owned()
                    } else {
                        format!("it has gone back to its worker {limit} times already")
                    };
                    self.decide(
                        issuer,
                        task,
                        format!("{decide}, and {why}. Decide: task_rework it, task_assign it, or task_accept it with a reason."),
                    )
                    .await;
                    return Ok(());
                }
                self.send_back_to_worker(run, task, notes, findings, from, head)
                    .await?;
            }
        }
        Ok(())
    }

    /// After a hub restart (`Orchestrator::resume`), a task that was waiting for a verdict or for
    /// acceptance is not left to wait unseen. A task in review gets its brief again, to its reviewer
    /// when that session is still there (a verdict for the same commit lands once; a repeat is
    /// refused); otherwise the lead decides. A task that waits to be accepted gets its notice again:
    /// the notice's topic replaces the old one, so nothing stacks up. A task whose notice already
    /// says what it needs (a failed check, a dispute, an open decision) is left as its notice says.
    pub(crate) async fn resume_waiting(&self, run: &Run, task: &mut Task) -> Result<(), CoreError> {
        let issuer = task.issuer_or(run.planner);
        match task.status {
            TaskStatus::Reviewing => {
                let reviewer = match task.reviewer {
                    Some(id) => self
                        .store
                        .get_agent(&AgentKey::Id(id))
                        .await
                        .ok()
                        .filter(|a| a.archived_at.is_none()),
                    None => None,
                };
                match reviewer {
                    Some(reviewer) => {
                        let text = self.review_message(run, task, &reviewer, None).await;
                        self.relay(reviewer.id, issuer, text, None);
                        self.notice(
                            issuer,
                            &format!("task:{}:review", task.id),
                            &format!(
                                "The hub restarted while review round {} of #{} was open: its brief went to {} again.",
                                task.report.review_attempt, task.key, reviewer.name
                            ),
                            NoticeClass::Silent,
                        )
                        .await;
                    }
                    None => {
                        self.decide(
                            issuer,
                            task,
                            format!(
                                "The hub restarted while review round {} of #{} was open, and its reviewer is gone. Decide: review_request it again, task_rework it, or accept it with force and a reason.",
                                task.report.review_attempt, task.key
                            ),
                        )
                        .await;
                    }
                }
            }
            TaskStatus::Passed => {
                let checks_failed =
                    !task.report.checks.is_empty() && !all_passed(&task.report.checks);
                let disputed = task
                    .report
                    .answers
                    .iter()
                    .any(|a| a.status == FindingStatus::Disputed);
                let reviewer_set = task.reviewer_template.or(run.config.reviewer).is_some();
                if task.report.failure.is_some()
                    || task.report.interrupted
                    || checks_failed
                    || disputed
                {
                    return Ok(());
                }
                if task.report.review_attempt == 0 {
                    if reviewer_set {
                        // Verified, but the hub stopped before its review was opened.
                        self.decide(
                            issuer,
                            task,
                            format!(
                                "#{} passed its checks, but its review was never opened (the hub restarted). Decide: review_request it, or accept it with force and a reason.",
                                task.key
                            ),
                        )
                        .await;
                    } else {
                        self.ready_to_accept(run, task, issuer).await;
                    }
                } else if task.report.verdict == Some(ReviewVerdict::Merge)
                    && task.report.reviewed_sha.is_some()
                    && task.report.reviewed_sha == task.report.verified_sha
                {
                    self.ready_to_accept(run, task, issuer).await;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Puts `text` on the accept notice of `task`, replacing the one it had (what the lead was
    /// asked to decide is no longer what is going on).
    pub(crate) async fn settle_accept_notice(&self, issuer: AgentId, task: &Task, text: &str) {
        self.notice(
            issuer,
            &format!("task:{}:accept", task.id),
            text,
            NoticeClass::Silent,
        )
        .await;
    }

    /// A decision the lead has to make about `task`, as a notice that wakes it at once.
    async fn decide(&self, issuer: AgentId, task: &Task, line: String) {
        self.notice(
            issuer,
            &format!("task:{}:decide", task.id),
            &line,
            NoticeClass::Decide,
        )
        .await;
    }

    /// The first line of what the worker said, for a notice.
    fn summary_line(task: &Task) -> String {
        let summary = task.report.summary.as_deref().unwrap_or_default();
        let first = summary.trim().lines().next().unwrap_or_default();
        cut(first, 160)
    }

    /// What a verified report sets going (spec 1 and 2). A failing check goes to the lead to
    /// decide, as does a dispute. With a reviewer, the hub opens the review and the lead hears
    /// one quiet line. Without one, the task waits to be accepted and the lead gets the notice
    /// that says so. Errors are told to the lead, never lost: the report already stands.
    pub(crate) async fn after_verified(&self, run: &Run, task: &mut Task, _worker: AgentId) {
        let issuer = task.issuer_or(run.planner);
        if !task.report.checks.is_empty() && !all_passed(&task.report.checks) {
            // A failed check goes back to the worker like a review's change request: the same
            // route, the same return limit, and one quiet line to the lead (brief/common, the review loop).
            let failing = task
                .report
                .checks
                .iter()
                .find(|c| !c.passed)
                .map(|c| c.command.clone())
                .unwrap_or_default();
            let head = format!(
                "#{} did not pass its checks (`{}`)",
                task.key,
                cut(&failing, 120)
            );
            let decide = format!(
                "#{} did not pass its checks ({})",
                task.key,
                brief::checks(&task.report.checks)
            );
            let notes = checks_note(&task.report.checks);
            if let Err(err) = self
                .return_work(run, task, &notes, &[], issuer, &head, &decide)
                .await
            {
                self.decide(
                    issuer,
                    task,
                    format!("{decide}, and it could not be sent back: {err}"),
                )
                .await;
            }
            return;
        }
        let disputed: Vec<String> = task
            .report
            .answers
            .iter()
            .filter(|a| a.status == FindingStatus::Disputed)
            .map(|a| {
                let title = task
                    .report
                    .findings
                    .iter()
                    .find(|f| f.id == a.id)
                    .map(|f| cut(&f.title, 80))
                    .unwrap_or_default();
                format!("{} {title} ({})", a.id, cut(&a.note, 160))
            })
            .collect();
        if !disputed.is_empty() {
            let line = format!(
                "#{}: the worker disputes {}. Decide which stands: task_rework (the finding stands), or task_accept with a reason (it does not).",
                task.key,
                disputed.join("; ")
            );
            self.decide(issuer, task, line).await;
            return;
        }
        if let Some(template) = task.reviewer_template.or(run.config.reviewer) {
            let fresh = task.reviewer_fresh;
            match self
                .open_review_locked(run, task, template, fresh, false, None, issuer)
                .await
            {
                Ok(reviewer) => {
                    self.notice(
                        issuer,
                        &format!("task:{}:review", task.id),
                        &format!(
                            "#{} passed its checks; review round {} is with {} (the hub opened it).",
                            task.key, task.report.review_attempt, reviewer.name
                        ),
                        NoticeClass::Silent,
                    )
                    .await;
                }
                Err(err) => {
                    let line = format!(
                        "#{} passed its checks, but its review could not be opened: {err}. Decide: review_request it with another reviewer, or task_accept it.",
                        task.key
                    );
                    self.decide(issuer, task, line).await;
                }
            }
            return;
        }
        self.ready_to_accept(run, task, issuer).await;
    }

    /// The notice that a task waits for its lead to accept it (spec 5): an Unblock when
    /// accepting it releases downstream work, otherwise a Batch. It is decision material: what
    /// it stands on, what accepting unlocks, the non-blocking findings registered, what is
    /// still blocking, and what the lead must decide.
    pub(crate) async fn ready_to_accept(&self, run: &Run, task: &Task, issuer: AgentId) {
        let tasks = self.store.list_tasks(run.id).await.unwrap_or_default();
        let merged = |key: &str| {
            tasks
                .iter()
                .any(|t| t.key == key && t.status == TaskStatus::Merged)
        };
        // What accepting it releases. A task that depends on nothing else still open is unlocked by
        // it; one that waits for another task says so, and is not unlocked.
        let mut unlocks: Vec<String> = Vec::new();
        let mut waits: Vec<String> = Vec::new();
        for t in tasks.iter().filter(|t| {
            t.id != task.id && !t.status.is_terminal() && t.depends_on.contains(&task.key)
        }) {
            let others: Vec<String> = t
                .depends_on
                .iter()
                .filter(|d| **d != task.key && !merged(d))
                .map(|d| format!("#{d}"))
                .collect();
            if others.is_empty() {
                unlocks.push(format!("#{}", t.key));
            } else {
                waits.push(format!("#{} still waits for {}", t.key, others.join(", ")));
            }
        }
        let next = crate::handoff::next_of(task).map(str::to_owned);
        let downstream = !unlocks.is_empty() || !waits.is_empty() || next.is_some();
        let class = if downstream {
            NoticeClass::Unblock
        } else {
            NoticeClass::Batch
        };
        let mut parts: Vec<String> = Vec::new();
        parts.push(if task.spec.acceptance.is_empty() {
            "no checks: accepting it takes force and a reason".into()
        } else if all_passed(&task.report.checks) {
            "checks passed".into()
        } else {
            "checks did not all pass: accepting it takes force and a reason".into()
        });
        let round = task.report.review_attempt;
        parts.push(if round == 0 {
            "no reviewer".into()
        } else if task.report.verdict == Some(ReviewVerdict::Merge)
            && task.report.reviewed_sha.is_some()
            && task.report.reviewed_sha == task.report.verified_sha
        {
            format!("review round {round} approved it")
        } else if task.report.verdict == Some(ReviewVerdict::Merge) {
            format!("review round {round} approved another commit than the verified one")
        } else {
            let said = match task.report.verdict {
                Some(ReviewVerdict::MergeAfterFixes) => "merge after fixes",
                Some(ReviewVerdict::DoNotMerge) => "do not merge",
                _ => "no verdict",
            };
            format!("review round {round} did not approve it ({said})")
        });
        if !unlocks.is_empty() {
            parts.push(format!("accepting it unlocks {}", unlocks.join(", ")));
        }
        parts.extend(waits);
        if let Some(next) = next {
            parts.push(format!("spec.next takes it on to {next}"));
        }
        let mut line = format!(
            "#{} waits for your acceptance: {}.",
            task.key,
            parts.join("; ")
        );
        let blocking: Vec<String> = task
            .report
            .findings
            .iter()
            .filter(|f| f.blocking)
            .map(|f| finding_line(f))
            .collect();
        if !blocking.is_empty() {
            line.push_str(&format!(
                " Blocking findings still open: {}.",
                blocking.join("; ")
            ));
        }
        let registered: Vec<String> = task
            .report
            .findings
            .iter()
            .filter(|f| !f.blocking)
            .map(|f| finding_line(f))
            .collect();
        if !registered.is_empty() {
            line.push_str(&format!(
                " Non-blocking findings registered: {}.",
                registered.join("; ")
            ));
        }
        line.push_str(&format!(
            " Worker's word: {}. Decide: task_accept it, task_rework it, or task_drop it.",
            Self::summary_line(task)
        ));
        self.notice(issuer, &format!("task:{}:accept", task.id), &line, class)
            .await;
    }

    /// The reviewer's verdict on a review of `task` (`review_submit`), under the run's lock.
    /// Refused without changing anything when the review is not open, names another commit
    /// than the one the round reads, or repeats one that came in (docs/TODO.md §九 ①).
    pub(crate) async fn review(
        &self,
        caller: &Agent,
        id: TaskId,
        submission: Submission,
    ) -> Result<(), CoreError> {
        // Pure checks first: a call that contradicts itself is refused before anything is read.
        let verdict =
            effective_verdict(submission.approve, submission.verdict, &submission.findings)?;
        let (run, _) = self.task_of(id).await?;
        let _guard = self.lock(run.id).await;
        let run = self.store.get_run(run.id).await?;
        let mut task = self.store.get_task(id).await?;
        if task.status != TaskStatus::Reviewing || task.reviewer != Some(caller.id) {
            return Err(CoreError::Conflict(not_open(&task)));
        }
        // Every verdict says which commit it read: the round's brief names it. Without that a
        // verdict could land on a later round's commit, so it is refused and nothing changes.
        let current = task.report.reviewed_sha.clone().unwrap_or_default();
        let Some(sha) = submission
            .reviewed_sha
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return Err(CoreError::Conflict(format!(
                "say which commit you reviewed: pass reviewed_sha, the one your brief names ({}). A verdict without it cannot be bound to review round {} of #{}; nothing changed.",
                current,
                task.report.review_attempt,
                task.key
            )));
        };
        if !same_commit(sha, &current) {
            return Err(CoreError::Conflict(format!(
                "your review is of commit {}, but review round {} of #{} is of {}: the task changed since, so this review is stale and nothing changed. Review the commit the brief names.",
                sha,
                task.report.review_attempt,
                task.key,
                current
            )));
        }
        let mut findings = submission.findings;
        let previous = task.report.findings.clone();
        number_findings(&mut findings, &previous, &mut task.report.finding_seq);
        let findings = sorted_findings(&findings);
        let notes = review_notes(
            &submission.notes,
            &findings,
            submission
                .verdict
                .or((!findings.is_empty()).then_some(verdict)),
        );
        let issuer = task.issuer_or(run.planner);
        task.report.findings = findings.clone();
        task.report.verdict = Some(verdict);
        task.report.review = Some(notes.clone());
        // Its review is in; the run lets go of the reviewer.
        task.reviewer = None;
        let _ = self
            .let_go_task(run.id, task.id, Some(ClaimRole::Reviewer))
            .await;
        if submission.approve {
            self.transition(&mut task, TaskStatus::Passed).await?;
            self.ready_to_accept(&run, &task, issuer).await;
            return Ok(());
        }
        // Changes are needed: the work goes back along its route, as a failed check does
        // (`return_work`). The task is Passed while the route decides.
        self.transition(&mut task, TaskStatus::Passed).await?;
        let head = format!(
            "Review of #{} (round {}): changes needed ({})",
            task.key,
            task.report.review_attempt,
            findings
                .iter()
                .map(|f| format!("{} {}", f.id, severity_name(f.severity)))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let decide = format!("#{}'s review asks for changes", task.key);
        // The task is recorded as passed with its verdict already; a return that fails must still
        // reach the lead, as a decision, rather than leave the task waiting unseen.
        if let Err(err) = self
            .return_work(
                &run, &mut task, &notes, &findings, caller.id, &head, &decide,
            )
            .await
        {
            self.decide(
                issuer,
                &task,
                format!(
                    "{decide}, but it could not be sent back: {err}. Decide: task_rework it, task_assign it, or task_accept it with a reason."
                ),
            )
            .await;
        }
        Ok(())
    }

    /// Sends a reviewed task back to the worker that made the work, the way `task_rework`
    /// does: the same execution session (a new one when that session is gone, said in the lead's
    /// line), one more attempt, the old checks cleared, and the findings with their numbers as
    /// its next turn. The lead hears one quiet line.
    async fn send_back_to_worker(
        &self,
        run: &Run,
        task: &mut Task,
        notes: &str,
        findings: &[ReviewFinding],
        from: AgentId,
        head: &str,
    ) -> Result<(), CoreError> {
        let issuer = task.issuer_or(run.planner);
        let Some(old) = task.assignee else {
            return Err(CoreError::Conflict(format!(
                "#{} has no worker to send it back to",
                task.key
            )));
        };
        let (alive, old_name) = match self.store.get_agent(&AgentKey::Id(old)).await {
            Ok(agent) => (agent.archived_at.is_none(), agent.name),
            Err(_) => (false, String::new()),
        };
        let mut fresh_note = None;
        let worker = if alive {
            old
        } else {
            // The session is gone: a new execution session of the same teammate takes it over.
            let template = task.worker_template.unwrap_or(old);
            let cwd = Self::worktree(task)?;
            let fresh = self
                .fresh_agent(
                    task,
                    template,
                    &worker_name(run, task, template),
                    Role::Worker,
                    &cwd,
                )
                .await?;
            let _ = self.let_go(run.id, old).await;
            task.assignee = Some(fresh.id);
            task.worker_template = Some(template);
            task.worker_fresh = true;
            task.worker_restore = false;
            fresh_note = Some(format!(
                "its worker session was gone, so a new execution session of {} took it over",
                fresh.name
            ));
            fresh.id
        };
        let name = if alive {
            old_name
        } else {
            self.store
                .get_agent(&AgentKey::Id(worker))
                .await
                .map(|a| a.name)
                .unwrap_or_default()
        };
        Handoff::end(&mut task.handoffs, Some(old), HandoffOutcome::Reworked);
        task.attempts += 1;
        task.report.verification = None;
        task.report.checks.clear();
        task.report.diff_stat = None;
        task.report.verified_sha = None;
        task.report.unchecked = false;
        task.report.related_lessons.clear();
        task.report.interrupted = false;
        task.report.forced = None;
        task.report.failure = None;
        task.report.review_returns += 1;
        task.report.rework.push(notes.trim().to_owned());
        self.transition(task, TaskStatus::Running).await?;
        let mut text = String::new();
        text.push_str(notes.trim());
        text.push_str("\n\n");
        if !findings.is_empty() {
            text.push_str(&findings_for_worker(findings));
            text.push('\n');
        }
        if fresh_note.is_some() {
            text.push('\n');
            text.push_str(&brief::task(task));
        }
        text.push_str(&format!(
            "\n---\nrework of task #{}\ntask_id: {}\nattempt: {}",
            task.key, task.id, task.attempts
        ));
        let envelope = self.relay_envelope(from, text);
        crate::attempts::begin(task, Some(worker), Some(envelope.id));
        self.store_task(task).await?;
        self.send_in_background(worker, envelope);
        let mut line = format!("{head}; it went back to {name}.");
        if let Some(note) = fresh_note {
            line.push(' ');
            line.push_str(&note);
            line.push('.');
        }
        self.notice(
            issuer,
            &format!("task:{}:review", task.id),
            &line,
            NoticeClass::Silent,
        )
        .await;
        Ok(())
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

    fn finding(id: &str, severity: ReviewSeverity) -> ReviewFinding {
        ReviewFinding {
            id: id.into(),
            severity,
            confidence: 0.5,
            file: None,
            line: None,
            title: "t".into(),
            detail: None,
            blocking: false,
            basis: None,
            verify: None,
        }
    }

    #[test]
    fn findings_are_numbered_and_keep_their_id_across_rounds() {
        let mut seq = 0;
        let mut first = vec![
            finding("", ReviewSeverity::P0),
            finding("", ReviewSeverity::P2),
        ];
        number_findings(&mut first, &[], &mut seq);
        assert_eq!(
            first.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
            ["F1", "F2"]
        );
        // The next round carries F2 over, invents one new finding, and claims an id that is not
        // there (it gets a new number, not the id).
        let mut second = vec![
            finding("F2", ReviewSeverity::P1),
            finding("", ReviewSeverity::P3),
            finding("F9", ReviewSeverity::P3),
        ];
        number_findings(&mut second, &first, &mut seq);
        assert_eq!(
            second.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
            ["F2", "F3", "F4"]
        );
    }

    #[test]
    fn blocking_defaults_to_p0_and_p1() {
        let input = |severity, blocking| FindingInput {
            id: None,
            severity,
            confidence: 0.9,
            file: None,
            line: None,
            title: "x".into(),
            detail: None,
            blocking,
            basis: None,
            verify: None,
        };
        assert!(input(ReviewSeverity::P0, None).into_finding().blocking);
        assert!(input(ReviewSeverity::P1, None).into_finding().blocking);
        assert!(!input(ReviewSeverity::P2, None).into_finding().blocking);
        assert!(
            !input(ReviewSeverity::P0, Some(false))
                .into_finding()
                .blocking
        );
    }
}

/// The caps a task is held to: its own `spec.budget`, field by field, else the run's `task_budget`.
pub(crate) fn task_caps(run: &Run, task: &Task) -> TaskBudget {
    task.spec
        .budget
        .clone()
        .unwrap_or_default()
        .with_default(&run.config.policy.task_budget.clone().unwrap_or_default())
}

/// Why the hub must not send `task` back for rework on its own: its budget allows no more
/// rounds (`max_rework`). The lead decides then, as it does past the review limit.
pub(crate) fn rework_stop(run: &Run, task: &Task) -> Option<String> {
    let max = task_caps(run, task).max_rework?;
    let done = task.report.rework.len() as u32;
    (done >= max).then(|| {
        format!("it has been sent back for rework {done} times, and its budget allows {max} (max_rework)")
    })
}
