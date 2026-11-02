//! The reviewer's checklist (docs/tasks/EXPERTISE.md §6, task exp-review): the relevant Active
//! judgements rendered as numbered items the reviewer answers pass/fail/na, and a `fail`
//! turned into a candidate for the person.
//!
//! The checklist is reference, never rules: it says what the person and this project consider
//! good, and the reviewer still decides.

use std::fmt::Write as _;
use std::path::Path;

use chrono::Utc;
use crossagents_core::{
    Agent, Candidate, CandidateOrigin, CandidateStatus, CoreError, EntryKind, EntryStatus,
    ExpertiseEntry, OwnerKind, PERSON_OWNER_ID, SourceRef, Task, UsageRecord, UsageSource,
    estimate_tokens,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::engine::Orchestrator;

/// The whole checklist stays within this many tokens (EXPERTISE §6.3).
pub const CHECKLIST_BUDGET: usize = 1500;

/// The short name an entry goes by in a checklist and in `checklist[].entry_id`: its id's
/// first eight characters.
fn tag(entry: &ExpertiseEntry) -> String {
    entry.id.chars().take(8).collect()
}

fn domain_of(entry: &ExpertiseEntry) -> String {
    match entry.domain.trim() {
        "" => "general".to_owned(),
        domain => domain.to_owned(),
    }
}

/// `text` as an indented block of its own lines.
fn indent(text: &str) -> String {
    let mut out = String::new();
    for line in text.trim().lines() {
        let _ = writeln!(out, "    {}", line.trim_end());
    }
    out
}

/// One item as it reads in the checklist: its number, tag, title and body.
fn render_item(entry: &ExpertiseEntry, number: usize) -> String {
    let mut out = String::new();
    let title = entry.title.trim();
    let _ = if title.is_empty() {
        writeln!(out, "{number}. [{}]", tag(entry))
    } else {
        writeln!(out, "{number}. [{}] {title}", tag(entry))
    };
    if !entry.text.trim().is_empty() {
        out.push_str(&indent(&entry.text));
    }
    if !entry.applies_when.trim().is_empty() {
        let _ = writeln!(out, "    applies when: {}", entry.applies_when.trim());
    }
    out
}

/// The selected judgements as a numbered checklist; `None` when there is nothing to ask.
/// Items are grouped by domain, in the order given.
pub fn render_checklist(entries: &[ExpertiseEntry]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let mut sorted: Vec<&ExpertiseEntry> = entries.iter().collect();
    // Group by domain; a stable sort keeps the relevance order within a group.
    sorted.sort_by_key(|entry| domain_of(entry).to_lowercase());
    let mut out = String::from(
        "Reviewer checklist — from this project's profile and the person's; reference, not rules.",
    );
    let mut group: Option<String> = None;
    for (index, entry) in sorted.iter().enumerate() {
        let domain = domain_of(entry);
        if group.as_ref() != Some(&domain) {
            let _ = write!(out, "\n\n{domain}:");
            group = Some(domain);
        }
        out.push('\n');
        out.push_str(render_item(entry, index + 1).trim_end());
    }
    out.push_str(
        "\n\nAnswer each item in `review_submit.checklist`: { entry_id: \"<tag>\", verdict: \
         \"pass\" | \"fail\" | \"na\", note }. `na` means the item does not apply here; a `fail` \
         becomes a candidate for the person.",
    );
    Some(out)
}

/// One item cut down to `budget` tokens by shortening its body; `None` when even its head is
/// too long. Cutting happens between characters, never inside one.
fn trim_body(mut entry: ExpertiseEntry, budget: usize) -> Option<ExpertiseEntry> {
    loop {
        let text = render_checklist(std::slice::from_ref(&entry))?;
        if estimate_tokens(&text) <= budget {
            return Some(entry);
        }
        entry.text.pop()?;
    }
}

/// The entries that fit in `budget` tokens: the ones at the end, least relevant, go first; a
/// single entry that is too long has its body cut.
fn within_budget(mut entries: Vec<ExpertiseEntry>, budget: usize) -> Vec<ExpertiseEntry> {
    loop {
        let Some(text) = render_checklist(&entries) else {
            return entries;
        };
        if estimate_tokens(&text) <= budget {
            return entries;
        }
        if entries.len() == 1 {
            let entry = entries.pop().expect("one entry");
            return trim_body(entry, budget).into_iter().collect();
        }
        entries.pop();
    }
}

/// Whether an entry's `applies_when` names an area the task touches (`files` are its scope
/// globs), by glob prefix.
fn applies_here(entry: &ExpertiseEntry, files: &[String]) -> bool {
    let normalize = |text: &str| {
        text.trim()
            .trim_end_matches("**")
            .trim_end_matches('*')
            .trim_end_matches('/')
            .to_ascii_lowercase()
    };
    let files: Vec<String> = files
        .iter()
        .map(|f| normalize(f))
        .filter(|f| !f.is_empty())
        .collect();
    entry
        .applies_when
        .split([',', ';'])
        .map(normalize)
        .filter(|area| !area.is_empty())
        .any(|area| {
            files.iter().any(|file| {
                file.as_str().starts_with(area.as_str()) || area.as_str().starts_with(file)
            })
        })
}

/// One verdict a reviewer gave on one item (EXPERTISE §6.3).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ChecklistItem {
    /// The entry's id, or the tag the checklist showed.
    pub entry_id: String,
    pub verdict: Verdict,
    #[serde(default)]
    pub note: String,
}

/// What the reviewer decided about one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
    Na,
}

impl Orchestrator {
    /// The active judgements of the task's project and the person, the ones whose
    /// `applies_when` touches its scope first, within the budget.
    async fn checklist_entries(&self, task: &Task) -> Vec<ExpertiseEntry> {
        let Ok(run) = self.store.get_run(task.run_id).await else {
            return Vec::new();
        };
        let Ok(project) = self.project_for(Path::new(&run.repo)).await else {
            return Vec::new();
        };
        let owners = [
            (OwnerKind::Project, project.id),
            (OwnerKind::Person, PERSON_OWNER_ID.to_owned()),
        ];
        let mut entries: Vec<ExpertiseEntry> = Vec::new();
        for (kind, id) in &owners {
            let Ok(found) = self.store.expertise_list(*kind, id, None).await else {
                continue;
            };
            for entry in found {
                if entry.status == EntryStatus::Active
                    && entry.kind == EntryKind::Judgment
                    && !entries.iter().any(|e| e.id == entry.id)
                {
                    entries.push(entry);
                }
            }
        }
        entries.sort_by_key(|entry| {
            (
                domain_of(entry).to_lowercase(),
                !applies_here(entry, &task.spec.scope.allow),
                entry.title.to_lowercase(),
            )
        });
        within_budget(entries, CHECKLIST_BUDGET)
    }

    /// The checklist a reviewer of `task` is shown as part of its brief; `None` when the
    /// profiles hold no judgement for it, in which case the brief says nothing at all.
    pub(crate) async fn checklist_for(&self, reviewer: &Agent, task: &Task) -> Option<String> {
        let entries = self.checklist_entries(task).await;
        let text = render_checklist(&entries)?;
        // What the reviewer was shown is a usage too (EXPERTISE §4.5).
        for entry in &entries {
            let usage = UsageRecord {
                id: Uuid::new_v4().to_string(),
                entry_id: entry.id.clone(),
                run_id: Some(task.run_id),
                task_id: Some(task.id),
                agent: Some(reviewer.id),
                source: UsageSource::Pushed,
                outcome: None,
                at: Utc::now(),
            };
            if let Err(err) = self.store.usage_record(&usage).await {
                tracing::warn!(entry = %entry.id, error = %err, "cannot record an expertise usage");
            }
        }
        Some(text)
    }

    /// The task's and the person's entry a reviewer named by id or by the tag it was shown,
    /// if it is one of theirs; `None` otherwise, and that verdict is ignored.
    async fn review_entry(&self, task: &Task, given: &str) -> Option<ExpertiseEntry> {
        let given = given.trim();
        if given.is_empty() {
            return None;
        }
        let Ok(run) = self.store.get_run(task.run_id).await else {
            return None;
        };
        let project = self.project_for(Path::new(&run.repo)).await.ok()?;
        let owners = [
            (OwnerKind::Project, project.id),
            (OwnerKind::Person, PERSON_OWNER_ID.to_owned()),
        ];
        let mut found: Vec<ExpertiseEntry> = Vec::new();
        for (kind, owner) in &owners {
            let Ok(entries) = self.store.expertise_list(*kind, owner, None).await else {
                continue;
            };
            for entry in entries {
                if entry.id == given
                    || (entry.id.starts_with(given) && !found.iter().any(|e| e.id == entry.id))
                {
                    found.push(entry);
                }
            }
        }
        if found.len() == 1 { found.pop() } else { None }
    }

    /// A `fail` on each named judgement proposes its replacement as a candidate for the
    /// person, quoting the reviewer's note; `pass` and `na` propose nothing. An id that is
    /// not in the task's project or the person's profile is ignored (EXPERTISE §6.3).
    pub(crate) async fn review_candidates(
        &self,
        _reviewer: &Agent,
        task: &Task,
        checklist: &[ChecklistItem],
    ) -> Result<Vec<Candidate>, CoreError> {
        if checklist.is_empty() {
            return Ok(Vec::new());
        }
        let Ok(run) = self.store.get_run(task.run_id).await else {
            return Ok(Vec::new());
        };
        let project = self.project_for(Path::new(&run.repo)).await?;
        let now = Utc::now();
        let mut candidates = Vec::new();
        for item in checklist {
            if item.verdict != Verdict::Fail {
                continue;
            }
            let Some(entry) = self.review_entry(task, &item.entry_id).await else {
                continue;
            };
            let note = item.note.trim();
            let mut proposed = entry.clone();
            proposed.id.clear();
            proposed.status = EntryStatus::Active;
            proposed.evidence = 0;
            proposed.last_confirmed_at = None;
            proposed.stats = Default::default();
            proposed.created_at = now;
            proposed.updated_at = now;
            if !note.is_empty() {
                if !proposed.text.trim().is_empty() {
                    proposed.text.push_str("\n\n");
                }
                proposed.text.push_str(note);
            }
            proposed.sources = vec![SourceRef::Task {
                run_id: task.run_id,
                task_id: task.id,
            }];
            let candidate = Candidate {
                id: Uuid::new_v4().to_string(),
                owner_kind: OwnerKind::Project,
                owner_id: project.id.clone(),
                domain: entry.domain.clone(),
                proposed,
                conflicts_with: vec![entry.id.clone()],
                origin: CandidateOrigin::Review,
                status: CandidateStatus::Pending,
                suggested_by_lead: false,
                note: note.to_owned(),
                sources: vec![SourceRef::Task {
                    run_id: task.run_id,
                    task_id: task.id,
                }],
                created_at: now,
                decided_at: None,
            };
            self.store.candidate_put(&candidate).await?;
            candidates.push(candidate);
        }
        Ok(candidates)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn entry(id: &str, domain: &str, title: &str, text: &str) -> ExpertiseEntry {
        let now = Utc::now();
        ExpertiseEntry {
            id: id.into(),
            owner_kind: OwnerKind::Project,
            owner_id: "p1".into(),
            domain: domain.into(),
            kind: EntryKind::Judgment,
            title: title.into(),
            text: text.into(),
            example: None,
            applies_when: String::new(),
            evidence: 0,
            last_confirmed_at: None,
            status: EntryStatus::Active,
            superseded_by: None,
            stats: Default::default(),
            sources: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn no_entries_means_no_checklist() {
        assert_eq!(render_checklist(&[]), None);
    }

    #[test]
    fn items_are_numbered_and_grouped_by_domain() {
        let entries = vec![
            entry("11111111-a", "rust", "先写测试", "先红后绿"),
            entry("22222222-b", "gui", "别加弹窗", ""),
            entry("33333333-c", "rust", "别抽第三个调用点", ""),
        ];
        let text = render_checklist(&entries).unwrap();
        // Domains read in order (gui before rust), their items numbered straight through.
        assert!(text.contains("gui:\n1. [22222222] 别加弹窗"), "{text}");
        assert!(text.contains("rust:\n2. [11111111] 先写测试"), "{text}");
        assert!(text.contains("3. [33333333] 别抽第三个调用点"), "{text}");
        assert!(text.contains("    先红后绿"), "{text}");
        // The heading is repeated only when the domain changes.
        assert_eq!(text.matches("rust:").count(), 1, "{text}");
        assert_eq!(text.matches("gui:").count(), 1, "{text}");
        // How to answer, and what a fail becomes.
        assert!(text.contains("review_submit.checklist"), "{text}");
        assert!(text.contains("pass"), "{text}");
    }

    #[test]
    fn a_checklist_over_budget_loses_its_last_items() {
        let entries = vec![
            entry("11111111-a", "rust", "一", "短"),
            entry("22222222-b", "rust", "二", &"测".repeat(2000)),
        ];
        let kept = within_budget(entries, CHECKLIST_BUDGET);
        assert_eq!(kept.len(), 1, "the long item had to go");
        assert_eq!(kept[0].id, "11111111-a");
    }

    #[test]
    fn a_single_long_item_has_its_body_cut() {
        let entries = vec![entry("11111111-a", "rust", "长判断", &"测".repeat(3000))];
        let kept = within_budget(entries, CHECKLIST_BUDGET);
        assert_eq!(kept.len(), 1);
        assert!(estimate_tokens(&render_checklist(&kept).unwrap()) <= CHECKLIST_BUDGET);
        assert!("测".repeat(3000).starts_with(&kept[0].text));
    }

    #[test]
    fn an_items_area_is_read_off_the_task_scope() {
        let files = vec!["crates/orchestrator/**".to_owned()];
        let mut one = entry("11111111-a", "rust", "t", "");
        one.applies_when = "crates/orchestrator".into();
        let mut other = entry("22222222-b", "gui", "t", "");
        other.applies_when = "apps/gui".into();
        assert!(applies_here(&one, &files));
        assert!(!applies_here(&other, &files));
        let mut anywhere = entry("33333333-c", "rust", "t", "");
        anywhere.applies_when.clear();
        assert!(!applies_here(&anywhere, &files));
    }

    #[test]
    fn a_checklist_is_parsed_with_its_three_verdicts() {
        let given = serde_json::json!([
            { "entry_id": "01a19b48", "verdict": "pass" },
            { "entry_id": "01a19b49", "verdict": "fail", "note": "没写测试" },
            { "entry_id": "01a19b4a", "verdict": "na", "note": "不是 GUI" }
        ]);
        let items: Vec<ChecklistItem> = serde_json::from_value(given).unwrap();
        assert_eq!(items[0].verdict, Verdict::Pass);
        assert_eq!(items[0].note, "");
        assert_eq!(items[1].verdict, Verdict::Fail);
        assert_eq!(items[1].note, "没写测试");
        assert_eq!(items[2].verdict, Verdict::Na);
        // An unknown verdict is refused rather than treated as pass.
        let bad = serde_json::json!([{ "entry_id": "x", "verdict": "maybe" }]);
        assert!(serde_json::from_value::<Vec<ChecklistItem>>(bad).is_err());
    }
}
