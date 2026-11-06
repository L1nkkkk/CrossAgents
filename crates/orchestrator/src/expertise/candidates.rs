//! Candidates (docs/tasks/EXPERTISE.md §6, task exp-candidates): what a rework or a first-try
//! merge suggests, the conflicts it names, and the lead's tools. Only the person accepts.
//!
//! A rework says what went wrong in the issuer's words; a task merged without rework says what
//! went right. Both become a draft for the person to accept or reject — never an entry by
//! themselves, and never decided by a lead.

use std::path::Path;

use chrono::Utc;
use crossagents_core::{
    Agent, Candidate, CandidateOrigin, CandidateStatus, CoreError, EntryKind, EntryStatus, Example,
    ExpertiseEntry, OwnerKind, PERSON_OWNER_ID, SourceRef, Task,
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::engine::{Orchestrator, first_line};
use crate::lessons::search_terms;
use crate::tools::args;

/// How long a draft's title may be, taken from the notes' first line.
const TITLE_MAX: usize = 120;
/// Entries fetched when looking for what a draft may repeat or contradict, and the most one
/// draft names (EXPERTISE §4.7).
const CONFLICT_FETCH: usize = 12;
const CONFLICT_MAX: usize = 5;
/// A draft starts in the general area: the task does not name a domain, and the person moves
/// it when they accept.
const CANDIDATE_DOMAIN: &str = "general";

/// Where a proposed entry came from: the task that proposed it.
fn task_source(task: &Task) -> SourceRef {
    SourceRef::Task {
        run_id: task.run_id,
        task_id: task.id,
    }
}

/// The area a draft applies to, from what the task was allowed to touch.
fn applies_when(task: &Task) -> String {
    task.spec.scope.allow.join(", ")
}

fn title_of(notes: &str, task: &Task) -> String {
    let line = first_line(notes, TITLE_MAX);
    if line.is_empty() {
        format!("Rework of #{}", task.key)
    } else {
        line
    }
}

/// The draft a rework note proposes: a judgement whose title is the note's first line and
/// whose body is the whole note (EXPERTISE §4.7).
fn rework_entry(owner: (OwnerKind, &str), task: &Task, notes: &str) -> ExpertiseEntry {
    let now = Utc::now();
    ExpertiseEntry {
        id: String::new(),
        owner_kind: owner.0,
        owner_id: owner.1.to_owned(),
        domain: CANDIDATE_DOMAIN.to_owned(),
        kind: EntryKind::Judgment,
        title: title_of(notes, task),
        text: notes.trim().to_owned(),
        example: None,
        applies_when: applies_when(task),
        evidence: 0,
        last_confirmed_at: None,
        status: EntryStatus::Active,
        superseded_by: None,
        stats: Default::default(),
        sources: vec![task_source(task)],
        created_at: now,
        updated_at: now,
    }
}

/// The draft a first-try merge proposes: an example whose situation is the task's goal and
/// whose chosen answer is the worker's summary (EXPERTISE §4.7).
fn first_try_entry(owner: (OwnerKind, &str), task: &Task) -> ExpertiseEntry {
    let now = Utc::now();
    ExpertiseEntry {
        id: String::new(),
        owner_kind: owner.0,
        owner_id: owner.1.to_owned(),
        domain: CANDIDATE_DOMAIN.to_owned(),
        kind: EntryKind::Example,
        title: task.title.clone(),
        text: String::new(),
        example: Some(Example {
            situation: task.spec.goal.clone(),
            rejected: String::new(),
            rejected_why: String::new(),
            chosen: task.report.summary.clone().unwrap_or_default(),
        }),
        applies_when: applies_when(task),
        evidence: 0,
        last_confirmed_at: None,
        status: EntryStatus::Active,
        superseded_by: None,
        stats: Default::default(),
        sources: vec![task_source(task)],
        created_at: now,
        updated_at: now,
    }
}

/// What a draft says, for looking up the entries it may repeat or contradict.
fn conflict_query(entry: &ExpertiseEntry) -> String {
    let mut text = format!("{} {}", entry.title.trim(), entry.text.trim());
    if let Some(example) = &entry.example {
        text.push_str(&format!(
            " {} {} {} {}",
            example.situation.trim(),
            example.rejected.trim(),
            example.rejected_why.trim(),
            example.chosen.trim()
        ));
    }
    text
}

/// The ids of the same owner's judgements and examples a draft reads most like, in the order
/// the search returned them, at most `max` and never twice (EXPERTISE §4.7).
fn conflicts(hits: &[ExpertiseEntry], max: usize) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for entry in hits {
        if ids.len() == max {
            break;
        }
        if !matches!(entry.kind, EntryKind::Judgment | EntryKind::Example) {
            continue;
        }
        if !ids.iter().any(|id| id == &entry.id) {
            ids.push(entry.id.clone());
        }
    }
    ids
}

/// A candidate as the lead's tools show it: enough to tell whether it is worth flagging.
fn candidate_view(candidate: &Candidate) -> Value {
    json!({
        "id": candidate.id,
        "tag": candidate.id.chars().take(8).collect::<String>(),
        "origin": candidate.origin,
        "owner_kind": candidate.owner_kind,
        "owner_id": candidate.owner_id,
        "domain": candidate.domain,
        "kind": candidate.proposed.kind,
        "title": candidate.proposed.title,
        "text": candidate.proposed.text,
        "note": candidate.note,
        "conflicts_with": candidate.conflicts_with,
        "suggested_by_lead": candidate.suggested_by_lead,
        "created_at": candidate.created_at,
    })
}

impl Orchestrator {
    /// Which profile a task's candidates go to: its project's (EXPERTISE §6.4, first version
    /// always the project, so that nobody has to judge whether it is a person's preference).
    async fn candidate_owner(&self, task: &Task) -> Result<(OwnerKind, String), CoreError> {
        let run = self.store.get_run(task.run_id).await?;
        let project = self.project_for(Path::new(&run.repo)).await?;
        Ok((OwnerKind::Project, project.id))
    }

    /// A task sent back proposes, as a pending candidate, the judgement its notes spell out.
    pub(crate) async fn candidate_from_rework(
        &self,
        task: &Task,
        notes: &str,
    ) -> Result<Candidate, CoreError> {
        let owner = self.candidate_owner(task).await?;
        let draft = rework_entry((owner.0, owner.1.as_str()), task, notes);
        self.propose(task, draft, CandidateOrigin::Rework, notes.trim())
            .await
    }

    /// A task merged without rework proposes, as a pending candidate, the example of how it
    /// was done: the goal as the situation, the worker's summary as what was chosen.
    pub(crate) async fn candidate_from_first_try(
        &self,
        task: &Task,
    ) -> Result<Candidate, CoreError> {
        let owner = self.candidate_owner(task).await?;
        let draft = first_try_entry((owner.0, owner.1.as_str()), task);
        self.propose(task, draft, CandidateOrigin::MergedFirstTry, "")
            .await
    }

    /// A pending candidate for the person, with the entries it may conflict with filled in.
    async fn propose(
        &self,
        task: &Task,
        draft: ExpertiseEntry,
        origin: CandidateOrigin,
        note: &str,
    ) -> Result<Candidate, CoreError> {
        let mut candidate = Candidate {
            id: Uuid::new_v4().to_string(),
            owner_kind: draft.owner_kind,
            owner_id: draft.owner_id.clone(),
            domain: draft.domain.clone(),
            proposed: draft,
            conflicts_with: Vec::new(),
            origin,
            status: CandidateStatus::Pending,
            suggested_by_lead: false,
            note: note.to_owned(),
            sources: vec![task_source(task)],
            created_at: Utc::now(),
            decided_at: None,
        };
        // The candidate is worth keeping even when the profile cannot be searched: the
        // person can still see it, just without the conflicts.
        if let Err(err) = self.fill_conflicts(&mut candidate).await {
            tracing::warn!(error = %err, "cannot look up the entries a candidate may conflict with");
        }
        self.store.candidate_put(&candidate).await?;
        Ok(candidate)
    }

    /// Fills `conflicts_with` from the same owner's similar judgements and examples, and
    /// changes nothing else (EXPERTISE §4.7). Conflicts are for the person to rule on; no
    /// entry is overwritten here.
    pub(crate) async fn fill_conflicts(&self, candidate: &mut Candidate) -> Result<(), CoreError> {
        let terms = search_terms(&conflict_query(&candidate.proposed));
        if terms.is_empty() {
            candidate.conflicts_with.clear();
            return Ok(());
        }
        let owners = [(candidate.owner_kind, candidate.owner_id.clone())];
        let hits = self
            .store
            .expertise_search(&owners, &terms, CONFLICT_FETCH)
            .await?;
        candidate.conflicts_with = conflicts(&hits, CONFLICT_MAX);
        Ok(())
    }

    /// The profiles a caller may see candidates of: its project's, and the person's.
    async fn candidate_owners(
        &self,
        caller: &Agent,
    ) -> Result<Vec<(OwnerKind, String)>, CoreError> {
        let project = self.project_for(&caller.cwd).await?;
        Ok(vec![
            (OwnerKind::Project, project.id),
            (OwnerKind::Person, PERSON_OWNER_ID.to_owned()),
        ])
    }

    /// One candidate of the caller's project or the person, by its id or the tag a tool shows;
    /// `None` outside those profiles, or when the tag names none or several.
    async fn own_candidate(
        &self,
        caller: &Agent,
        given: &str,
    ) -> Result<Option<Candidate>, CoreError> {
        let given = given.trim();
        if given.is_empty() {
            return Ok(None);
        }
        let mut found: Vec<Candidate> = Vec::new();
        for (kind, id) in self.candidate_owners(caller).await? {
            for candidate in self.store.candidate_list(Some((kind, id)), None).await? {
                if (candidate.id == given || candidate.id.starts_with(given))
                    && !found.iter().any(|c| c.id == candidate.id)
                {
                    found.push(candidate);
                }
            }
        }
        Ok(if found.len() == 1 { found.pop() } else { None })
    }

    /// The candidates waiting for the person in this project and in the person's profile,
    /// oldest first. A lead reads them to decide which are worth flagging; it cannot decide.
    pub(crate) async fn expertise_pending(&self, caller: &Agent) -> Result<Value, CoreError> {
        let mut candidates: Vec<Candidate> = Vec::new();
        for (kind, id) in self.candidate_owners(caller).await? {
            candidates.extend(
                self.store
                    .candidate_list(Some((kind, id)), Some(CandidateStatus::Pending))
                    .await?,
            );
        }
        candidates.sort_by_key(|c| c.created_at);
        Ok(json!({
            "candidates": candidates.iter().map(candidate_view).collect::<Vec<_>>(),
            "note": "Only the person accepts or rejects a candidate. You may flag one for them \
                     with expertise_triage: suggested_by_lead and a reason.",
        }))
    }

    /// Flags one pending candidate for the person. Nothing else: accept and reject are the
    /// person's alone, and asking for them here is refused (EXPERTISE §4.7).
    pub(crate) async fn expertise_triage(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            #[serde(alias = "id")]
            candidate: String,
            #[serde(default)]
            reason: String,
            /// Present only when someone tries to decide the candidate.
            #[serde(default)]
            accept: Option<Value>,
            #[serde(default)]
            reject: Option<Value>,
        }
        let a: A = args(input)?;
        if a.accept.is_some() || a.reject.is_some() {
            return Err(CoreError::Conflict(
                "only the person can accept or reject an expertise candidate; a lead can only \
                 flag it for them (suggested_by_lead and a reason)"
                    .into(),
            ));
        }
        let reason = a.reason.trim();
        if reason.is_empty() {
            return Err(CoreError::Conflict(
                "give a reason the person can weigh: expertise_triage { candidate, reason }".into(),
            ));
        }
        let mut candidate = self
            .own_candidate(caller, &a.candidate)
            .await?
            .ok_or_else(|| CoreError::NotFound {
                kind: "expertise candidate",
                id: a.candidate.trim().to_owned(),
            })?;
        if candidate.status != CandidateStatus::Pending {
            return Err(CoreError::Conflict(format!(
                "candidate {} was already decided",
                candidate.id
            )));
        }
        candidate.suggested_by_lead = true;
        if !candidate.note.trim().is_empty() {
            candidate.note.push_str("\n\n");
        }
        candidate.note.push_str(&format!("lead: {reason}"));
        self.store.candidate_put(&candidate).await?;
        Ok(json!({
            "candidate": candidate_view(&candidate),
            "suggested_by_lead": true,
            "next": "Only the person decides: they accept or reject it in the Expertise page.",
        }))
    }

    /// How many candidates wait for the person here, for `run_finish` to report: the
    /// project's and the person's, never one blocking the close (EXPERTISE §6.4).
    pub(crate) async fn pending_candidates(&self, project_id: &str) -> Result<usize, CoreError> {
        let owners = [
            (OwnerKind::Project, project_id.to_owned()),
            (OwnerKind::Person, PERSON_OWNER_ID.to_owned()),
        ];
        let mut pending = 0usize;
        for (kind, id) in owners {
            pending += self
                .store
                .candidate_list(Some((kind, id)), Some(CandidateStatus::Pending))
                .await?
                .len();
        }
        Ok(pending)
    }
}

#[cfg(test)]
mod tests {
    use crossagents_core::{RunId, TaskId, TaskSpec, TaskStatus};

    use super::*;

    fn task(key: &str) -> Task {
        let now = Utc::now();
        Task {
            id: TaskId::new(),
            run_id: RunId::new(),
            key: key.into(),
            title: format!("task {key}"),
            spec: TaskSpec {
                goal: "write a.txt".into(),
                ..TaskSpec::default()
            },
            depends_on: Vec::new(),
            issuer: None,
            assignee: None,
            reviewer: None,
            worker_template: None,
            reviewer_template: None,
            worker_fresh: true,
            worker_restore: false,
            reviewer_fresh: true,
            status: TaskStatus::Passed,
            attempts: 1,
            runs: Vec::new(),
            escalated: false,
            nudges: 0,
            branch: None,
            worktree: None,
            report: Default::default(),
            progress: None,
            parent: None,
            handoffs: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    fn entry(id: &str, kind: EntryKind) -> ExpertiseEntry {
        ExpertiseEntry {
            id: id.into(),
            owner_kind: OwnerKind::Project,
            owner_id: "p1".into(),
            domain: "rust".into(),
            kind,
            title: "t".into(),
            text: String::new(),
            example: None,
            applies_when: String::new(),
            evidence: 0,
            last_confirmed_at: None,
            status: EntryStatus::Active,
            superseded_by: None,
            stats: Default::default(),
            sources: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn a_rework_draft_takes_its_title_from_the_first_line_and_keeps_the_whole_note() {
        let task = task("a");
        let notes = "先写一个会失败的测试\n\n这次直接改了实现，问题出在没先红后绿";
        let draft = rework_entry((OwnerKind::Project, "p1"), &task, notes);
        assert_eq!(draft.kind, EntryKind::Judgment);
        assert_eq!(draft.title, "先写一个会失败的测试");
        assert_eq!(draft.text, notes);
        assert!(draft.id.is_empty(), "the id is filled in when accepted");
        assert_eq!(draft.owner_kind, OwnerKind::Project);
        assert_eq!(draft.domain, CANDIDATE_DOMAIN);
        assert!(matches!(draft.sources.as_slice(), [SourceRef::Task { .. }]));
    }

    #[test]
    fn a_rework_with_blank_notes_still_names_the_task() {
        let task = task("a");
        let draft = rework_entry((OwnerKind::Project, "p1"), &task, "   ");
        assert_eq!(draft.title, "Rework of #a");
        assert!(draft.text.is_empty());
    }

    #[test]
    fn a_first_try_draft_is_an_example_of_the_goal_and_the_summary() {
        let mut task = task("b");
        task.spec.goal = "把 a.txt 改成 beta".into();
        task.report.summary = Some("把 a.txt 改成了 beta，并加了用例".into());
        let draft = first_try_entry((OwnerKind::Project, "p1"), &task);
        assert_eq!(draft.kind, EntryKind::Example);
        assert_eq!(draft.title, "task b");
        let example = draft.example.expect("an example draft");
        assert_eq!(example.situation, "把 a.txt 改成 beta");
        assert_eq!(example.chosen, "把 a.txt 改成了 beta，并加了用例");
        assert!(example.rejected.is_empty() && example.rejected_why.is_empty());
    }

    #[test]
    fn conflicts_take_only_judgements_and_examples_once() {
        let hits = vec![
            entry("j1", EntryKind::Judgment),
            entry("l1", EntryKind::Limit),
            entry("e1", EntryKind::Example),
            entry("j1", EntryKind::Judgment),
            entry("j2", EntryKind::Judgment),
        ];
        assert_eq!(conflicts(&hits, 2), vec!["j1", "e1"]);
        assert_eq!(conflicts(&hits, 10), vec!["j1", "e1", "j2"]);
    }

    #[test]
    fn a_drafts_query_reads_the_example_fields_too() {
        let mut example = entry("e1", EntryKind::Example);
        example.title = "改 beta".into();
        example.example = Some(Example {
            situation: "要改 a.txt".into(),
            rejected: "直接改字符串".into(),
            rejected_why: "会漏掉测试".into(),
            chosen: "先补用例".into(),
        });
        let query = conflict_query(&example);
        for part in [
            "改 beta",
            "要改 a.txt",
            "直接改字符串",
            "会漏掉测试",
            "先补用例",
        ] {
            assert!(query.contains(part), "{part} missing: {query}");
        }
    }
}
