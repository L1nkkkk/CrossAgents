//! Keeping a profile in shape (docs/tasks/EXPERTISE.md §6, task exp-maintain): the per-domain
//! judgement budget, entries gone stale, conflicts waiting for the person, the card's
//! first-try merge rate, and the Markdown export. Pure functions over slices.
//!
//! Nothing here touches the store and nothing changes an entry: the budget only reports, the
//! scan only suggests a status, and conflicts are listed for the person to decide. The caller
//! (API/GUI) decides what to do with it.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crossagents_core::{
    estimate_tokens, Candidate, CandidateStatus, EntryKind, EntryStatus, ExpertiseEntry,
    UsageOutcome, UsageRecord,
};

/// The per-domain ceiling for judgements (EXPERTISE §4.3).
pub const JUDGMENT_BUDGET: usize = 1500;

/// How long an entry may go unused before [`stale_scan`] suggests `stale`.
pub const STALE_AFTER_DAYS: i64 = 60;

/// A rework this recent is fresh evidence against an entry.
pub const RECENT_FAILURE_DAYS: i64 = 30;

/// Fewer decided uses than this and a rework rate is noise.
pub const REWORK_RATE_MIN_SAMPLE: u32 = 3;

/// A share of reworked uses at or above this makes an entry worth a second look.
pub const REWORK_RATE_THRESHOLD: f64 = 0.3;

/// How far back the card's "30 days ago" figure looks (EXPERTISE §4.8).
const CARD_WINDOW_DAYS: i64 = 30;

/// One judgement over another in the same domain that reads alike: a merge candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MergeSuggestion {
    /// The two entry ids, in the order they appeared in the input.
    pub a: String,
    pub b: String,
    /// Why they could be one entry.
    pub reason: String,
}

/// The judgement budget of one domain (EXPERTISE §4.3).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DomainBudget {
    pub domain: String,
    /// Sum of [`estimate_tokens`] over the domain's non-superseded judgements.
    pub judgment_tokens: usize,
    pub budget: usize,
    pub over: bool,
    /// By how much the domain is over, `0` when it is not.
    pub over_by: usize,
    pub judgment_count: usize,
    /// Pairs worth merging; only filled when the domain is over budget.
    pub merge_suggestions: Vec<MergeSuggestion>,
}

/// A suggestion to re-flag an entry (EXPERTISE §4.4). The status is not changed here.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StaleHint {
    pub entry_id: String,
    pub domain: String,
    pub title: String,
    /// What the scan would set it to: `stale` or `review`.
    pub suggested: EntryStatus,
    pub reason: String,
    /// Days since it was last used, confirmed or created.
    pub days_unused: i64,
    /// `stats.reworked / stats.used`, `None` when it was never used.
    pub rework_rate: Option<f64>,
}

/// A pending candidate that may contradict an existing entry, waiting for a verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConflictPair {
    pub candidate_id: String,
    /// The existing entry the candidate may conflict with.
    pub entry_id: String,
    pub domain: String,
    /// The proposed entry's title, so the pair can be shown side by side.
    pub title: String,
    pub note: String,
}

/// The profile's health at a glance (EXPERTISE §4.8).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExpertiseCard {
    /// `merged_first_try / (merged_first_try + reworked + dropped)` over decided usages,
    /// `None` when there are none.
    pub first_try_rate: Option<f64>,
    /// The same figure over usages decided before `now - 30d`.
    pub first_try_rate_30d_ago: Option<f64>,
    /// `first_try_rate - first_try_rate_30d_ago`, in percentage points.
    pub delta_points: Option<f64>,
    /// Decided usages behind `first_try_rate`.
    pub sample: usize,
    /// The sample is under ten: the rate is shown, but not to be trusted yet.
    pub small_sample: bool,
    pub merged_first_try: usize,
    pub reworked: usize,
    pub dropped: usize,
    /// All entries, by kind.
    pub entries: usize,
    pub judgments: usize,
    pub examples: usize,
    pub limits: usize,
    /// Entries by status.
    pub active: usize,
    pub stale: usize,
    pub review: usize,
    pub superseded: usize,
    /// Judgement tokens over the whole profile, and the per-domain budget.
    pub judgment_tokens: usize,
    pub judgment_budget: usize,
    /// Domains whose judgements are over budget (shown red).
    pub over_budget_domains: usize,
}

/// Judgement tokens per domain, the budget, and what to merge when a domain is over
/// (EXPERTISE §4.3). Domains are sorted by name; example and limit entries are not counted.
pub fn budget_report(entries: &[ExpertiseEntry]) -> Vec<DomainBudget> {
    let mut by_domain: BTreeMap<&str, Vec<&ExpertiseEntry>> = BTreeMap::new();
    for entry in entries.iter().filter(|entry| {
        entry.kind == EntryKind::Judgment && entry.status != EntryStatus::Superseded
    }) {
        by_domain
            .entry(entry.domain.as_str())
            .or_default()
            .push(entry);
    }

    by_domain
        .into_iter()
        .map(|(domain, judgments)| {
            let judgment_tokens: usize = judgments
                .iter()
                .map(|entry| estimate_tokens(&judgment_copy(entry)))
                .sum();
            let over = judgment_tokens > JUDGMENT_BUDGET;
            DomainBudget {
                domain: domain.to_string(),
                judgment_tokens,
                budget: JUDGMENT_BUDGET,
                over,
                over_by: judgment_tokens.saturating_sub(JUDGMENT_BUDGET),
                judgment_count: judgments.len(),
                merge_suggestions: if over {
                    merge_suggestions(&judgments)
                } else {
                    Vec::new()
                },
            }
        })
        .collect()
}

/// Entries that have gone unused, or keep getting reworked, with the status the person may
/// want to set (EXPERTISE §4.4). Active entries only; nothing is modified.
///
/// An entry is "last seen" at its latest usage, else its `last_confirmed_at`, else when it
/// was created — a new entry is not stale just because nobody has used it yet.
pub fn stale_scan(
    entries: &[ExpertiseEntry],
    usages: &[UsageRecord],
    now: DateTime<Utc>,
) -> Vec<StaleHint> {
    let recent_failure = now - Duration::days(RECENT_FAILURE_DAYS);

    let mut last_seen: BTreeMap<&str, DateTime<Utc>> = BTreeMap::new();
    let mut recently_reworked: BTreeSet<&str> = BTreeSet::new();
    for usage in usages {
        let seen = last_seen.entry(usage.entry_id.as_str()).or_insert(usage.at);
        if usage.at > *seen {
            *seen = usage.at;
        }
        if usage.at >= recent_failure && usage.outcome == Some(UsageOutcome::Reworked) {
            recently_reworked.insert(usage.entry_id.as_str());
        }
    }

    let mut hints = Vec::new();
    for entry in entries
        .iter()
        .filter(|entry| entry.status == EntryStatus::Active)
    {
        let seen = last_seen
            .get(entry.id.as_str())
            .copied()
            .into_iter()
            .chain(entry.last_confirmed_at)
            .chain(std::iter::once(entry.created_at))
            .max()
            .unwrap_or(entry.created_at);
        let days_unused = (now - seen).num_days();

        let used = entry.stats.used;
        let reworked = entry.stats.reworked;
        let rework_rate = (used > 0).then(|| round_to(f64::from(reworked) / f64::from(used), 3));

        let (suggested, reason) = if reworked > 0 && recently_reworked.contains(entry.id.as_str())
        {
            (
                EntryStatus::Review,
                format!(
                    "最近 {RECENT_FAILURE_DAYS} 天有返工（{reworked}/{used} 次）"
                ),
            )
        } else if used >= REWORK_RATE_MIN_SAMPLE
            && rework_rate.is_some_and(|rate| rate >= REWORK_RATE_THRESHOLD)
        {
            (
                EntryStatus::Stale,
                format!("返工率偏高（{reworked}/{used} 次）"),
            )
        } else if days_unused >= STALE_AFTER_DAYS {
            (
                EntryStatus::Stale,
                format!("{days_unused} 天没被用到（超过 {STALE_AFTER_DAYS} 天）"),
            )
        } else {
            continue;
        };

        hints.push(StaleHint {
            entry_id: entry.id.clone(),
            domain: entry.domain.clone(),
            title: entry.title.clone(),
            suggested,
            reason,
            days_unused,
            rework_rate,
        });
    }
    hints
}

/// Every pending candidate that names entries it may conflict with, one pair per entry
/// (EXPERTISE §4.4). Decided candidates are left out: the verdict already happened.
pub fn conflict_list(candidates: &[Candidate]) -> Vec<ConflictPair> {
    let mut pairs = Vec::new();
    for candidate in candidates
        .iter()
        .filter(|candidate| candidate.status == CandidateStatus::Pending)
    {
        for entry_id in &candidate.conflicts_with {
            pairs.push(ConflictPair {
                candidate_id: candidate.id.clone(),
                entry_id: entry_id.clone(),
                domain: candidate.domain.clone(),
                title: candidate.proposed.title.clone(),
                note: candidate.note.clone(),
            });
        }
    }
    pairs
}

/// The card: first-try merge rate now and 30 days ago, entry counts, judgement tokens, and the
/// yellow/red counts (EXPERTISE §4.8). The rate is computed from decided usages, not from
/// entry stats, so a task that pushed three entries counts three times.
pub fn expertise_card(
    entries: &[ExpertiseEntry],
    usages: &[UsageRecord],
    now: DateTime<Utc>,
) -> ExpertiseCard {
    let cutoff = now - Duration::days(CARD_WINDOW_DAYS);

    let mut merged_first_try = 0usize;
    let mut reworked = 0usize;
    let mut dropped = 0usize;
    let mut merged_then = 0usize;
    let mut reworked_then = 0usize;
    let mut dropped_then = 0usize;

    for usage in usages {
        let Some(outcome) = usage.outcome else {
            continue;
        };
        let then = usage.at < cutoff;
        match outcome {
            UsageOutcome::MergedFirstTry => {
                merged_first_try += 1;
                if then {
                    merged_then += 1;
                }
            }
            UsageOutcome::Reworked => {
                reworked += 1;
                if then {
                    reworked_then += 1;
                }
            }
            UsageOutcome::Dropped => {
                dropped += 1;
                if then {
                    dropped_then += 1;
                }
            }
        }
    }

    let sample = merged_first_try + reworked + dropped;
    let sample_then = merged_then + reworked_then + dropped_then;
    let first_try_rate = rate(merged_first_try, sample);
    let first_try_rate_30d_ago = rate(merged_then, sample_then);
    let delta_points = match (first_try_rate, first_try_rate_30d_ago) {
        (Some(now_rate), Some(then_rate)) => {
            Some(round_to((now_rate - then_rate) * 100.0, 1))
        }
        _ => None,
    };

    let budgets = budget_report(entries);
    let judgment_tokens = budgets.iter().map(|budget| budget.judgment_tokens).sum();
    let over_budget_domains = budgets.iter().filter(|budget| budget.over).count();

    let count_kind = |kind: EntryKind| entries.iter().filter(|entry| entry.kind == kind).count();
    let count_status =
        |status: EntryStatus| entries.iter().filter(|entry| entry.status == status).count();

    ExpertiseCard {
        first_try_rate,
        first_try_rate_30d_ago,
        delta_points,
        sample,
        small_sample: sample < 10,
        merged_first_try,
        reworked,
        dropped,
        entries: entries.len(),
        judgments: count_kind(EntryKind::Judgment),
        examples: count_kind(EntryKind::Example),
        limits: count_kind(EntryKind::Limit),
        active: count_status(EntryStatus::Active),
        stale: count_status(EntryStatus::Stale),
        review: count_status(EntryStatus::Review),
        superseded: count_status(EntryStatus::Superseded),
        judgment_tokens,
        judgment_budget: JUDGMENT_BUDGET,
        over_budget_domains,
    }
}

/// The profile as Markdown: domains sorted, each with the three kinds in the order
/// judgement → example → limit (EXPERTISE §6.5). `stale` / `review` entries are marked,
/// superseded ones are left out (they are history, not the profile). Empty input gives an
/// empty string, so a caller can skip the section entirely.
pub fn export_markdown(entries: &[ExpertiseEntry]) -> String {
    let mut groups: BTreeMap<&str, Vec<&ExpertiseEntry>> = BTreeMap::new();
    for entry in entries
        .iter()
        .filter(|entry| entry.status != EntryStatus::Superseded)
    {
        groups.entry(entry.domain.as_str()).or_default().push(entry);
    }
    if groups.is_empty() {
        return String::new();
    }

    let mut out = String::from("# 专长档案\n");
    for (domain, group) in groups {
        out.push_str(&format!("\n## {domain}\n"));
        for (kind, heading) in [
            (EntryKind::Judgment, "判断"),
            (EntryKind::Example, "范例"),
            (EntryKind::Limit, "边界"),
        ] {
            let items: Vec<&ExpertiseEntry> = group
                .iter()
                .copied()
                .filter(|entry| entry.kind == kind)
                .collect();
            if items.is_empty() {
                continue;
            }
            out.push_str(&format!("\n### {heading}\n\n"));
            for entry in items {
                write_entry(&mut out, entry);
            }
        }
    }
    out
}

fn write_entry(out: &mut String, entry: &ExpertiseEntry) {
    out.push_str("- **");
    out.push_str(&one_line(&entry.title));
    out.push_str("**");
    if !entry.text.is_empty() {
        out.push_str(" — ");
        out.push_str(&one_line(&entry.text));
    }
    match entry.status {
        EntryStatus::Stale => out.push_str("（待确认）"),
        EntryStatus::Review => out.push_str("（需复核）"),
        _ => {}
    }
    out.push('\n');

    if let Some(example) = &entry.example {
        if !example.situation.is_empty() {
            out.push_str(&format!("  - 情况：{}\n", one_line(&example.situation)));
        }
        if !example.rejected.is_empty() {
            let mut line = format!("  - 否掉的方案：{}", one_line(&example.rejected));
            if !example.rejected_why.is_empty() {
                line.push_str(&format!("（原因：{}）", one_line(&example.rejected_why)));
            }
            line.push('\n');
            out.push_str(&line);
        }
        if !example.chosen.is_empty() {
            out.push_str(&format!("  - 采用：{}\n", one_line(&example.chosen)));
        }
    }
    if !entry.applies_when.is_empty() {
        out.push_str(&format!("  - 适用：`{}`\n", one_line(&entry.applies_when)));
    }
}

/// What a judgement costs: its one-line title plus the body.
fn judgment_copy(entry: &ExpertiseEntry) -> String {
    if entry.text.is_empty() {
        entry.title.clone()
    } else {
        format!("{} {}", entry.title, entry.text)
    }
}

/// Pairs of judgements in one domain whose applicability overlaps and whose wording is close
/// enough that they may be saying the same thing.
fn merge_suggestions(judgments: &[&ExpertiseEntry]) -> Vec<MergeSuggestion> {
    let items: Vec<(&ExpertiseEntry, BTreeSet<String>)> = judgments
        .iter()
        .map(|entry| (*entry, words(&judgment_copy(entry))))
        .collect();

    let mut suggestions = Vec::new();
    for (index, (left, left_words)) in items.iter().enumerate() {
        for (right, right_words) in items.iter().skip(index + 1) {
            if !applies_overlap(&left.applies_when, &right.applies_when) {
                continue;
            }
            let shared = left_words.intersection(right_words).count();
            let smaller = left_words.len().min(right_words.len());
            // Wording is "close" when they share at least a few words and half of the shorter
            // one; that keeps common domain vocabulary from pairing everything.
            if shared < 3 || shared * 2 < smaller {
                continue;
            }
            suggestions.push(MergeSuggestion {
                a: left.id.clone(),
                b: right.id.clone(),
                reason: format!("同一 domain，适用条件有交集，{shared} 个用词相同"),
            });
        }
    }
    suggestions
}

/// Do two `applies_when` values cover anything in common? Empty means anywhere.
fn applies_overlap(a: &str, b: &str) -> bool {
    let left = scopes(a);
    let right = scopes(b);
    if left.is_empty() || right.is_empty() {
        return true;
    }
    left.iter().any(|left| {
        right
            .iter()
            .any(|right| left == right || is_under(left, right) || is_under(right, left))
    })
}

fn is_under(path: &str, parent: &str) -> bool {
    path.strip_prefix(parent)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// The areas named by an `applies_when` value, lowercased and without globs.
fn scopes(value: &str) -> Vec<String> {
    value
        .split(|ch: char| ch == ',' || ch == ';' || ch.is_whitespace())
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.trim_end_matches("**")
                .trim_end_matches('/')
                .to_lowercase()
        })
        .filter(|part| !part.is_empty())
        .collect()
}

/// Words for a rough similarity test: latin runs of three or more characters, and CJK bigrams
/// (a single CJK character when it stands alone).
fn words(text: &str) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    let mut latin = String::new();
    let mut cjk: Vec<char> = Vec::new();

    for ch in text.chars().flat_map(char::to_lowercase) {
        if is_cjk(ch) {
            flush_latin(&mut latin, &mut set);
            cjk.push(ch);
        } else if ch.is_ascii_alphanumeric() || ch == '_' {
            flush_cjk(&mut cjk, &mut set);
            latin.push(ch);
        } else {
            flush_latin(&mut latin, &mut set);
            flush_cjk(&mut cjk, &mut set);
        }
    }
    flush_latin(&mut latin, &mut set);
    flush_cjk(&mut cjk, &mut set);
    set
}

fn flush_latin(buffer: &mut String, set: &mut BTreeSet<String>) {
    if buffer.len() >= 3 {
        set.insert(buffer.clone());
    }
    buffer.clear();
}

fn flush_cjk(buffer: &mut Vec<char>, set: &mut BTreeSet<String>) {
    if buffer.len() == 1 {
        set.insert(buffer[0].to_string());
    } else {
        for pair in buffer.windows(2) {
            set.insert(pair.iter().collect());
        }
    }
    buffer.clear();
}

/// Same ranges as [`crossagents_core::estimate_tokens`]: CJK text is counted by character.
fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x3000..=0x303F
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xF900..=0xFAFF
        | 0xFF00..=0xFFEF
        | 0x20000..=0x2FA1F
    )
}

/// One line for the Markdown export: every run of whitespace (a blank line between two
/// paragraphs, a merge's `\n\n`, a wrapped body) becomes one space.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn rate(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator > 0).then(|| round_to(numerator as f64 / denominator as f64, 3))
}

fn round_to(value: f64, digits: i32) -> f64 {
    let factor = 10f64.powi(digits);
    (value * factor).round() / factor
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossagents_core::{
        CandidateOrigin, EntryStats, Example, OwnerKind, PERSON_OWNER_ID, UsageSource,
    };

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-02T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn at(days_ago: i64) -> DateTime<Utc> {
        now() - Duration::days(days_ago)
    }

    fn entry(id: &str, domain: &str, kind: EntryKind, title: &str, text: &str) -> ExpertiseEntry {
        ExpertiseEntry {
            id: id.into(),
            owner_kind: OwnerKind::Person,
            owner_id: PERSON_OWNER_ID.into(),
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
            stats: EntryStats::default(),
            sources: Vec::new(),
            created_at: at(0),
            updated_at: at(0),
        }
    }

    fn usage(id: &str, entry_id: &str, days_ago: i64, outcome: Option<UsageOutcome>) -> UsageRecord {
        UsageRecord {
            id: id.into(),
            entry_id: entry_id.into(),
            run_id: None,
            task_id: None,
            agent: None,
            source: UsageSource::Declared,
            outcome,
            at: at(days_ago),
        }
    }

    fn candidate(id: &str, conflicts: &[&str], status: CandidateStatus) -> Candidate {
        let proposed = entry("e-new", "rust", EntryKind::Judgment, "新判断", "");
        Candidate {
            id: id.into(),
            owner_kind: OwnerKind::Project,
            owner_id: "p1".into(),
            domain: "rust".into(),
            proposed,
            conflicts_with: conflicts.iter().map(|id| (*id).to_string()).collect(),
            origin: CandidateOrigin::Rework,
            status,
            suggested_by_lead: false,
            note: "返工说明".into(),
            sources: Vec::new(),
            created_at: at(1),
            decided_at: None,
        }
    }

    #[test]
    fn budget_counts_judgements_per_domain_and_ignores_other_kinds() {
        let short = entry("a", "rust", EntryKind::Judgment, "t", "");
        let other = entry("b", "rust", EntryKind::Judgment, "t2", "");
        let example = entry("c", "rust", EntryKind::Example, "例子", &"x".repeat(4000));
        let limit = entry("d", "gui", EntryKind::Limit, "边界", &"x".repeat(4000));
        let report = budget_report(&[short, other, example, limit]);

        assert_eq!(report.len(), 1);
        assert_eq!(report[0].domain, "rust");
        assert_eq!(report[0].judgment_count, 2);
        assert_eq!(report[0].judgment_tokens, 2);
        assert_eq!(report[0].budget, JUDGMENT_BUDGET);
        assert!(!report[0].over);
        assert_eq!(report[0].over_by, 0);
        assert!(report[0].merge_suggestions.is_empty());
    }

    #[test]
    fn budget_ignores_superseded_judgements() {
        let mut old = entry("a", "rust", EntryKind::Judgment, "旧判断", &"x".repeat(8000));
        old.status = EntryStatus::Superseded;
        let live = entry("b", "rust", EntryKind::Judgment, "t", "");
        let report = budget_report(&[old, live]);

        assert_eq!(report[0].judgment_tokens, 1);
        assert!(!report[0].over);
    }

    #[test]
    fn an_over_budget_domain_suggests_similar_pairs_to_merge() {
        let mut left = entry(
            "a",
            "rust",
            EntryKind::Judgment,
            "always write the failing test first",
            &"x".repeat(3200),
        );
        left.applies_when = "crates/orchestrator/**".into();
        let mut right = entry(
            "b",
            "rust",
            EntryKind::Judgment,
            "always write a failing test before the code",
            &"y".repeat(3200),
        );
        right.applies_when = "crates/orchestrator/push.rs".into();

        let report = budget_report(&[left, right]);
        assert!(report[0].over);
        assert!(report[0].over_by > 0);
        assert_eq!(report[0].merge_suggestions.len(), 1);
        let pair = &report[0].merge_suggestions[0];
        assert_eq!((pair.a.as_str(), pair.b.as_str()), ("a", "b"));
        assert!(pair.reason.contains("适用条件"));
    }

    #[test]
    fn overlapping_wording_does_not_pair_entries_with_disjoint_areas() {
        let mut left = entry(
            "a",
            "rust",
            EntryKind::Judgment,
            "always write the failing test first",
            &"x".repeat(3200),
        );
        left.applies_when = "crates/core/**".into();
        let mut right = entry(
            "b",
            "rust",
            EntryKind::Judgment,
            "always write a failing test before the code",
            &"y".repeat(3200),
        );
        right.applies_when = "apps/gui/**".into();

        let report = budget_report(&[left, right]);
        assert!(report[0].over);
        assert!(report[0].merge_suggestions.is_empty());
    }

    #[test]
    fn a_domain_under_budget_makes_no_merge_suggestions_even_when_similar() {
        let left = entry(
            "a",
            "rust",
            EntryKind::Judgment,
            "always write the failing test first",
            "",
        );
        let right = entry(
            "b",
            "rust",
            EntryKind::Judgment,
            "always write a failing test before the code",
            "",
        );
        let report = budget_report(&[left, right]);
        assert!(!report[0].over);
        assert!(report[0].merge_suggestions.is_empty());
    }

    #[test]
    fn stale_scan_flags_unused_and_rework_heavy_entries() {
        let mut unused = entry("unused", "rust", EntryKind::Judgment, "长期没用的", "");
        unused.created_at = at(100);
        unused.last_confirmed_at = Some(at(100));

        let mut fresh = entry("fresh", "rust", EntryKind::Judgment, "刚用过的", "");
        fresh.created_at = at(10);

        let mut reworked = entry("reworked", "rust", EntryKind::Judgment, "老被返工", "");
        reworked.created_at = at(40);
        reworked.stats = EntryStats {
            used: 10,
            reworked: 4,
            ..EntryStats::default()
        };

        let mut reviewed = entry("reviewed", "rust", EntryKind::Judgment, "最近返工", "");
        reviewed.created_at = at(5);
        reviewed.stats = EntryStats {
            used: 4,
            reworked: 2,
            ..EntryStats::default()
        };

        let mut new_entry = entry("new", "rust", EntryKind::Judgment, "刚建的", "");
        new_entry.created_at = at(3);

        let mut already_stale = entry("stale", "rust", EntryKind::Judgment, "已经标黄", "");
        already_stale.created_at = at(200);
        already_stale.status = EntryStatus::Stale;

        let usages = vec![
            usage("u1", "fresh", 5, Some(UsageOutcome::MergedFirstTry)),
            usage("u2", "reworked", 40, Some(UsageOutcome::Reworked)),
            usage("u3", "reviewed", 2, Some(UsageOutcome::Reworked)),
        ];

        let hints = stale_scan(
            &[
                unused,
                fresh,
                reworked,
                reviewed,
                new_entry,
                already_stale,
            ],
            &usages,
            now(),
        );
        let ids: Vec<&str> = hints.iter().map(|hint| hint.entry_id.as_str()).collect();
        assert_eq!(ids, vec!["unused", "reworked", "reviewed"]);

        assert_eq!(hints[0].suggested, EntryStatus::Stale);
        assert!(hints[0].reason.contains("100 天"));
        assert_eq!(hints[0].days_unused, 100);

        assert_eq!(hints[1].suggested, EntryStatus::Stale);
        assert!(hints[1].reason.contains("返工率"));
        assert_eq!(hints[1].rework_rate, Some(0.4));

        assert_eq!(hints[2].suggested, EntryStatus::Review);
        assert!(hints[2].reason.contains("返工"));
        assert_eq!(hints[2].rework_rate, Some(0.5));
    }

    #[test]
    fn a_rework_rate_is_ignored_on_a_tiny_sample() {
        let mut entry = entry("a", "rust", EntryKind::Judgment, "小样本", "");
        entry.created_at = at(5);
        entry.stats = EntryStats {
            used: 2,
            reworked: 2,
            ..EntryStats::default()
        };
        let hints = stale_scan(&[entry], &[], now());
        assert!(hints.is_empty());
    }

    #[test]
    fn conflict_list_flattens_pending_candidates_only() {
        let pending = candidate("c1", &["e1", "e2"], CandidateStatus::Pending);
        let clean = candidate("c2", &[], CandidateStatus::Pending);
        let accepted = candidate("c3", &["e3"], CandidateStatus::Accepted);
        let pairs = conflict_list(&[pending, clean, accepted]);

        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].candidate_id, "c1");
        assert_eq!(pairs[0].entry_id, "e1");
        assert_eq!(pairs[0].domain, "rust");
        assert_eq!(pairs[0].title, "新判断");
        assert_eq!(pairs[0].note, "返工说明");
        assert_eq!(pairs[1].entry_id, "e2");
    }

    #[test]
    fn card_compares_the_first_try_rate_with_thirty_days_ago() {
        let usages = vec![
            usage("u1", "e", 2, Some(UsageOutcome::MergedFirstTry)),
            usage("u2", "e", 3, Some(UsageOutcome::MergedFirstTry)),
            usage("u3", "e", 4, Some(UsageOutcome::MergedFirstTry)),
            usage("u4", "e", 5, Some(UsageOutcome::Reworked)),
            usage("u5", "e", 40, Some(UsageOutcome::MergedFirstTry)),
            usage("u6", "e", 41, Some(UsageOutcome::MergedFirstTry)),
            usage("u7", "e", 42, Some(UsageOutcome::Reworked)),
            usage("u8", "e", 43, Some(UsageOutcome::Reworked)),
            // No outcome yet: not part of the rate.
            usage("u9", "e", 1, None),
        ];
        let mut stale = entry("s", "rust", EntryKind::Judgment, "待确认", "");
        stale.status = EntryStatus::Stale;
        let mut review = entry("r", "rust", EntryKind::Judgment, "需复核", "");
        review.status = EntryStatus::Review;
        let mut gone = entry("g", "rust", EntryKind::Judgment, "被替代", "");
        gone.status = EntryStatus::Superseded;
        let over = entry("o", "rust", EntryKind::Judgment, "长篇判断", &"x".repeat(6400));
        let example = entry("x", "gui", EntryKind::Example, "例子", "");
        let limit = entry("l", "gui", EntryKind::Limit, "边界", "");

        let card = expertise_card(
            &[stale, review, gone, over, example, limit],
            &usages,
            now(),
        );

        assert_eq!(card.first_try_rate, Some(0.625));
        assert_eq!(card.first_try_rate_30d_ago, Some(0.5));
        assert_eq!(card.delta_points, Some(12.5));
        assert_eq!(card.sample, 8);
        assert!(card.small_sample);
        assert_eq!(card.merged_first_try, 5);
        assert_eq!(card.reworked, 3);
        assert_eq!(card.dropped, 0);
        assert_eq!(card.entries, 6);
        assert_eq!(card.judgments, 4);
        assert_eq!(card.examples, 1);
        assert_eq!(card.limits, 1);
        assert_eq!(card.stale, 1);
        assert_eq!(card.review, 1);
        assert_eq!(card.superseded, 1);
        assert_eq!(card.active, 3);
        assert!(card.judgment_tokens > JUDGMENT_BUDGET);
        assert_eq!(card.judgment_budget, JUDGMENT_BUDGET);
        assert_eq!(card.over_budget_domains, 1);
    }

    #[test]
    fn card_gives_no_rate_without_decided_usages() {
        let usages = vec![usage("u1", "e", 2, None)];
        let card = expertise_card(&[], &usages, now());
        assert_eq!(card.first_try_rate, None);
        assert_eq!(card.first_try_rate_30d_ago, None);
        assert_eq!(card.delta_points, None);
        assert_eq!(card.sample, 0);
        assert!(card.small_sample);

        let json = serde_json::to_value(&card).unwrap();
        assert!(json["first_try_rate"].is_null());
        assert_eq!(json["entries"], 0);
    }

    #[test]
    fn card_has_no_delta_when_only_one_window_has_samples() {
        let usages = vec![usage("u1", "e", 2, Some(UsageOutcome::MergedFirstTry))];
        let card = expertise_card(&[], &usages, now());
        assert_eq!(card.first_try_rate, Some(1.0));
        assert_eq!(card.first_try_rate_30d_ago, None);
        assert_eq!(card.delta_points, None);
    }

    #[test]
    fn export_groups_by_domain_then_kind_and_marks_warnings() {
        let mut example = entry("x", "rust", EntryKind::Example, "一次取舍", "正文");
        example.example = Some(Example {
            situation: "要复用".into(),
            rejected: "抽抽象".into(),
            rejected_why: "只有两个调用点".into(),
            chosen: "先复制".into(),
        });
        let mut stale = entry("s", "rust", EntryKind::Judgment, "老判断", "因为这样");
        stale.status = EntryStatus::Stale;
        stale.applies_when = "crates/**".into();
        let mut review = entry("r", "rust", EntryKind::Limit, "别这么做", "");
        review.status = EntryStatus::Review;
        let mut gone = entry("g", "rust", EntryKind::Limit, "已替代", "");
        gone.status = EntryStatus::Superseded;
        let gui = entry("u", "gui", EntryKind::Judgment, "界面判断", "");

        let text = export_markdown(&[example, stale, review, gone, gui]);

        assert!(text.starts_with("# 专长档案"));
        let gui_at = text.find("## gui").unwrap();
        let rust_at = text.find("## rust").unwrap();
        assert!(gui_at < rust_at, "domains are sorted");

        let judgment_at = text.find("### 判断").unwrap();
        let example_at = text.find("### 范例").unwrap();
        let limit_at = text.find("### 边界").unwrap();
        assert!(judgment_at < example_at && example_at < limit_at);

        assert!(text.contains("**老判断** — 因为这样（待确认）"));
        assert!(text.contains("适用：`crates/**`"));
        assert!(text.contains("**别这么做**（需复核）"));
        assert!(text.contains("- 情况：要复用"));
        assert!(text.contains("- 否掉的方案：抽抽象（原因：只有两个调用点）"));
        assert!(text.contains("- 采用：先复制"));
        assert!(!text.contains("已替代"), "superseded entries are history");

        assert_eq!(export_markdown(&[]), "");
    }

    #[test]
    fn export_puts_a_multi_line_body_on_one_line() {
        let text = export_markdown(&[entry(
            "m",
            "rust",
            EntryKind::Judgment,
            "标题",
            "第一段\n\n第二段",
        )]);
        assert!(text.contains("- **标题** — 第一段 第二段\n"), "{text}");
    }

    #[test]
    fn export_keeps_each_domain_together() {
        let gui = entry("u", "gui", EntryKind::Judgment, "界面判断", "");
        let rust = entry("r", "rust", EntryKind::Judgment, "测试判断", "");
        let text = export_markdown(&[gui, rust]);

        let gui_at = text.find("## gui").unwrap();
        let rust_at = text.find("## rust").unwrap();
        assert!(gui_at < rust_at);
        assert!(text.contains("## gui\n\n### 判断\n\n- **界面判断**"));
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn applies_overlap_reads_scopes_and_globs() {
        assert!(applies_overlap("", "crates/core/**"));
        assert!(applies_overlap("crates/**", "crates/orchestrator/push.rs"));
        assert!(applies_overlap("crates/core, apps/gui", "apps/gui"));
        assert!(!applies_overlap("crates/core/**", "apps/gui/**"));
        assert!(!applies_overlap("crates/core", "crates/corelib"));
    }
}
