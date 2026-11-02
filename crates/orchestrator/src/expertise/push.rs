//! Pushing entries into a brief (docs/tasks/EXPERTISE.md §6, task exp-push): the Active
//! judgements of the worker's project and of the person, grouped by domain within the budget,
//! plus the few examples and limits a search on the task turned up. Every entry pushed is
//! recorded as a usage.
//!
//! Reference, never rules: the section says what the person and this project consider good,
//! and the worker still decides.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use chrono::Utc;
use crossagents_core::{
    Agent, EntryKind, EntryStatus, ExpertiseEntry, OwnerKind, PERSON_OWNER_ID, Task, UsageRecord,
    UsageSource, estimate_tokens,
};
use uuid::Uuid;

use crate::engine::Orchestrator;
use crate::lessons::search_terms;

/// Each domain's judgements together stay within this many tokens (EXPERTISE §4.3).
pub const JUDGMENT_BUDGET: usize = 1500;
/// The whole section stays within this many tokens: the least relevant examples and limits go
/// first (EXPERTISE §4.3).
const BRIEF_BUDGET: usize = 2000;
/// Examples and limits pushed at most, and how many hits are fetched before filtering.
const RELATED_MAX: usize = 5;
const RELATED_FETCH: usize = 12;

/// The short name an entry goes by in a brief and in `task_complete.used`: its id's first
/// eight characters.
fn tag(entry: &ExpertiseEntry) -> String {
    entry.id.chars().take(8).collect()
}

fn label(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Judgment => "Judgment",
        EntryKind::Example => "Example",
        EntryKind::Limit => "Limit",
    }
}

fn rank(kind: EntryKind) -> u8 {
    match kind {
        EntryKind::Judgment => 0,
        EntryKind::Example => 1,
        EntryKind::Limit => 2,
    }
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

fn field(out: &mut String, label: &str, text: &str) {
    if !text.trim().is_empty() {
        let _ = writeln!(out, "    {label}: {}", text.trim());
    }
}

/// One entry as it reads in a brief: its tag, title, and for an example the four parts that
/// teach "why not that" (EXPERTISE §4.2).
fn render_entry(entry: &ExpertiseEntry) -> String {
    let mut out = String::new();
    let title = entry.title.trim();
    let _ = if title.is_empty() {
        writeln!(out, "  [{}]", tag(entry))
    } else {
        writeln!(out, "  [{}] {title}", tag(entry))
    };
    if entry.kind == EntryKind::Example
        && let Some(example) = &entry.example
    {
        field(&mut out, "when", &example.situation);
        field(&mut out, "rejected", &example.rejected);
        field(&mut out, "rejected because", &example.rejected_why);
        field(&mut out, "chosen", &example.chosen);
    }
    if !entry.text.trim().is_empty() {
        out.push_str(&indent(&entry.text));
    }
    field(&mut out, "applies when", &entry.applies_when);
    out
}

/// The selected entries as one section, numbered; `None` when there is nothing to show.
/// Groups in the order Judgment → Example → Limit, and by domain within a group.
pub fn render_brief(entries: &[ExpertiseEntry]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let mut sorted: Vec<&ExpertiseEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| {
        (
            rank(e.kind),
            domain_of(e).to_lowercase(),
            e.title.to_lowercase(),
        )
    });
    let mut out = String::from(
        "Expertise for this task (reference, from this project's profile and the person's; not \
         rules):",
    );
    let mut group: Option<(EntryKind, String)> = None;
    for entry in sorted {
        let domain = domain_of(entry);
        if group.as_ref() != Some(&(entry.kind, domain.clone())) {
            let _ = write!(out, "\n\n{} · {domain}", label(entry.kind));
            group = Some((entry.kind, domain));
        }
        out.push('\n');
        out.push_str(render_entry(entry).trim_end());
    }
    out.push_str(
        "\n\nReference, not rules. `expertise_search` finds more (this project and the person); \
         name the entries you used in `task_complete.used`.",
    );
    Some(out)
}

/// `entry` with its body cut short to fit in `budget` tokens, if a readable piece of it does.
/// Cutting happens between characters, never inside one.
fn cut_entry(entry: &ExpertiseEntry, budget: usize) -> Option<ExpertiseEntry> {
    let mut head = entry.clone();
    head.text.clear();
    let room = budget.checked_sub(estimate_tokens(&render_entry(&head)))?;
    if room == 0 {
        return None;
    }
    let mut kept = String::new();
    for ch in entry.text.chars() {
        let mut next = kept.clone();
        next.push(ch);
        if estimate_tokens(&next) > room {
            break;
        }
        kept = next;
    }
    let mut kept = kept.trim_end().to_owned();
    if kept.is_empty() {
        return None;
    }
    // The ellipsis and the labels cost a little more than the body alone; give back one
    // character at a time until the rendered entry fits.
    let mut cut = entry.clone();
    loop {
        cut.text = format!("{kept}…");
        if estimate_tokens(&render_entry(&cut)) <= budget {
            return Some(cut);
        }
        kept.pop();
        if kept.is_empty() {
            return None;
        }
    }
}

/// Judgements kept while their domain's budget lasts; one that would overflow is cut short,
/// and nothing follows it in that domain (EXPERTISE §4.3).
fn judgments_within_budget(entries: Vec<ExpertiseEntry>, budget: usize) -> Vec<ExpertiseEntry> {
    let mut spent: HashMap<String, usize> = HashMap::new();
    let mut kept = Vec::new();
    for entry in entries {
        let used = spent.get(&entry.domain).copied().unwrap_or(0);
        let cost = estimate_tokens(&render_entry(&entry));
        if used + cost <= budget {
            spent.insert(entry.domain.clone(), used + cost);
            kept.push(entry);
        } else if let Some(cut) = cut_entry(&entry, budget.saturating_sub(used)) {
            spent.insert(entry.domain.clone(), budget);
            kept.push(cut);
        }
    }
    kept
}

/// Drops the least relevant entries — the examples and limits, which come last — until the
/// whole section fits.
fn trim_to_brief_budget(entries: &mut Vec<ExpertiseEntry>) {
    while entries.len() > 1 {
        let Some(text) = render_brief(entries) else {
            return;
        };
        if estimate_tokens(&text) <= BRIEF_BUDGET {
            return;
        }
        entries.pop();
    }
}

fn active(entry: &ExpertiseEntry) -> bool {
    entry.status == EntryStatus::Active
}

impl Orchestrator {
    /// The entries a worker taking on `task` is shown, as one section; `None` when the
    /// profiles hold nothing for it, in which case the brief says nothing at all.
    pub(crate) async fn expertise_brief(&self, worker: &Agent, task: &Task) -> Option<String> {
        let run = self.store.get_run(task.run_id).await.ok()?;
        let project = self.project_for(Path::new(&run.repo)).await.ok()?;
        let owners = [
            (OwnerKind::Project, project.id),
            (OwnerKind::Person, PERSON_OWNER_ID.to_owned()),
        ];

        let mut judgments = Vec::new();
        for (kind, id) in &owners {
            let Ok(entries) = self.store.expertise_list(*kind, id, None).await else {
                continue;
            };
            judgments.extend(
                entries
                    .into_iter()
                    .filter(active)
                    .filter(|e| e.kind == EntryKind::Judgment),
            );
        }
        let mut selected = judgments_within_budget(judgments, JUDGMENT_BUDGET);

        // The examples and limits closest to what this task is about.
        let terms = search_terms(&format!("{} {}", task.title, task.spec.goal));
        if !terms.is_empty()
            && let Ok(hits) = self
                .store
                .expertise_search(&owners, &terms, RELATED_FETCH)
                .await
        {
            let mut related = 0;
            for hit in hits {
                if related == RELATED_MAX {
                    break;
                }
                if !active(&hit)
                    || !matches!(hit.kind, EntryKind::Example | EntryKind::Limit)
                    || selected.iter().any(|e| e.id == hit.id)
                {
                    continue;
                }
                selected.push(hit);
                related += 1;
            }
        }
        if selected.is_empty() {
            return None;
        }
        trim_to_brief_budget(&mut selected);
        let text = render_brief(&selected)?;

        // What was shown is a usage; the outcome comes when the task ends (EXPERTISE §4.6).
        for entry in &selected {
            let usage = UsageRecord {
                id: Uuid::new_v4().to_string(),
                entry_id: entry.id.clone(),
                run_id: Some(task.run_id),
                task_id: Some(task.id),
                agent: Some(worker.id),
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
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn entry(id: &str, kind: EntryKind, domain: &str, title: &str, text: &str) -> ExpertiseEntry {
        let now = Utc::now();
        ExpertiseEntry {
            id: id.into(),
            owner_kind: OwnerKind::Project,
            owner_id: "p1".into(),
            domain: domain.into(),
            kind,
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
    fn no_entries_means_no_section() {
        assert_eq!(render_brief(&[]), None);
    }

    #[test]
    fn entries_are_numbered_and_grouped_judgment_example_limit() {
        let entries = vec![
            entry("33333333-a", EntryKind::Limit, "windows", "不要用软链", ""),
            entry(
                "11111111-a",
                EntryKind::Judgment,
                "rust",
                "先写测试",
                "先红后绿",
            ),
            entry("22222222-a", EntryKind::Example, "rust", "抽不抽抽象", ""),
        ];
        let text = render_brief(&entries).unwrap();
        for tag in ["[33333333]", "[11111111]", "[22222222]"] {
            assert!(text.contains(tag), "{tag} missing:\n{text}");
        }
        // The domain heading is a line of its own, above its entries.
        assert!(
            text.contains("Judgment · rust\n  [11111111] 先写测试"),
            "{text}"
        );
        let judgment = text.find("Judgment · rust").unwrap();
        let example = text.find("Example · rust").unwrap();
        let limit = text.find("Limit · windows").unwrap();
        assert!(judgment < example && example < limit, "{text}");
        // How to search on, and how to report having used an entry.
        assert!(text.contains("expertise_search"), "{text}");
        assert!(text.contains("task_complete.used"), "{text}");
    }

    #[test]
    fn a_judgment_over_budget_is_cut_between_characters() {
        let long = "测".repeat(3000);
        let entries = vec![entry(
            "aaaaaaaa-1",
            EntryKind::Judgment,
            "rust",
            "长判断",
            &long,
        )];
        let kept = judgments_within_budget(entries, JUDGMENT_BUDGET);
        assert_eq!(kept.len(), 1, "the head of the entry is kept");
        let text = &kept[0].text;
        assert!(text.ends_with('…'), "{text}");
        let body = text.trim_end_matches('…');
        assert!(long.starts_with(body), "cut in the middle of a character");
        assert!(estimate_tokens(&render_entry(&kept[0])) <= JUDGMENT_BUDGET);
    }

    #[test]
    fn each_domain_has_its_own_budget() {
        let entries = vec![
            entry(
                "aaaaaaaa-1",
                EntryKind::Judgment,
                "rust",
                "一",
                &"测".repeat(1000),
            ),
            entry(
                "bbbbbbbb-1",
                EntryKind::Judgment,
                "gui",
                "二",
                &"测".repeat(1000),
            ),
        ];
        let kept = judgments_within_budget(entries, JUDGMENT_BUDGET);
        assert_eq!(kept.len(), 2, "neither domain is charged for the other");
    }

    #[test]
    fn examples_and_limits_go_first_when_the_section_is_too_long() {
        let mut selected = vec![
            entry("aaaaaaaa-1", EntryKind::Judgment, "rust", "判断", "短"),
            entry(
                "bbbbbbbb-1",
                EntryKind::Example,
                "rust",
                "旧例子",
                &"测".repeat(1500),
            ),
            entry(
                "cccccccc-1",
                EntryKind::Limit,
                "rust",
                "旧边界",
                &"测".repeat(1500),
            ),
        ];
        trim_to_brief_budget(&mut selected);
        assert_eq!(selected[0].kind, EntryKind::Judgment);
        assert!(
            selected.len() < 3,
            "something had to go:\n{}",
            render_brief(&selected).unwrap()
        );
        assert!(estimate_tokens(&render_brief(&selected).unwrap()) <= BRIEF_BUDGET);
    }
}
