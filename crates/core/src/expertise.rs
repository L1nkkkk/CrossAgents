//! Expertise profiles (docs/tasks/EXPERTISE.md): what "good work" looks like for a person or a
//! project, kept as short entries a later agent is shown before it starts. Lessons record what
//! went wrong; this records what counts as right. Reference, never rules.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, MessageId, RunId, TaskId};

/// Who the profile belongs to: the person (`owner_id` `"me"`, kept in the hub) or a project
/// (`owner_id` is the [`crate::Project`] id).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum OwnerKind {
    Person,
    Project,
}

/// The person's own profile, as stored in the hub.
pub const PERSON_OWNER_ID: &str = "me";

/// The three shapes an entry takes (EXPERTISE §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// A judgement: one line on what counts as good here, and why.
    Judgment,
    /// A worked example: the situation, what was rejected and why, what was done.
    Example,
    /// A limit: where the judgement stops applying, or what never to do.
    Limit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum EntryStatus {
    /// Good to go by.
    Active,
    /// Not confirmed for a long time, or its results got worse: shown as a warning.
    Stale,
    /// A rework or a reviewer's `fail` put it in doubt; the person decides.
    Review,
    /// Replaced by `superseded_by`; kept for its history.
    Superseded,
}

/// What the entries did when they were used (EXPERTISE §4.5). Counted per entry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EntryStats {
    /// Usage records of any source.
    #[serde(default)]
    pub used: u32,
    /// Tasks that took the entry and were merged without rework.
    #[serde(default)]
    pub merged_first_try: u32,
    /// Tasks that took the entry and were sent back at least once.
    #[serde(default)]
    pub reworked: u32,
    /// Tasks that took the entry and were dropped.
    #[serde(default)]
    pub dropped: u32,
}

/// The worked example of an [`EntryKind::Example`] entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Example {
    /// The situation it answers.
    #[serde(default)]
    pub situation: String,
    /// What was tried first and rejected.
    #[serde(default)]
    pub rejected: String,
    /// Why it was rejected.
    #[serde(default)]
    pub rejected_why: String,
    /// What was done in the end.
    #[serde(default)]
    pub chosen: String,
}

/// Where an entry came from: a run, a task in one, or a message to the person.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceRef {
    Run { run_id: RunId },
    Task { run_id: RunId, task_id: TaskId },
    Message { message_id: MessageId },
}

/// One entry of a profile (EXPERTISE §4.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ExpertiseEntry {
    pub id: String,
    pub owner_kind: OwnerKind,
    pub owner_id: String,
    /// The area it is about (`rust`, `gui`, `windows`, `testing`, …). The judgement budget is
    /// counted per domain.
    pub domain: String,
    pub kind: EntryKind,
    /// One line: the judgement, or the case, or the limit.
    pub title: String,
    /// The body: why it holds, and how to apply it.
    #[serde(default)]
    pub text: String,
    /// Present for [`EntryKind::Example`].
    #[serde(default)]
    pub example: Option<Example>,
    /// Where it applies: repository areas, tools and versions. Empty means anywhere.
    #[serde(default)]
    pub applies_when: String,
    /// How many runs have confirmed it since it last changed.
    #[serde(default)]
    pub evidence: u32,
    #[serde(default)]
    pub last_confirmed_at: Option<DateTime<Utc>>,
    pub status: EntryStatus,
    #[serde(default)]
    pub superseded_by: Option<String>,
    #[serde(default)]
    pub stats: EntryStats,
    /// What it was proposed from (a run, a task, a message).
    #[serde(default)]
    pub sources: Vec<SourceRef>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// What proposed a candidate (EXPERTISE §4.5): only the person accepts one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CandidateOrigin {
    /// A task that was sent back for rework.
    Rework,
    /// A reviewer's `fail` on a checklist entry.
    Review,
    /// A task merged without rework: what it did right is worth keeping.
    MergedFirstTry,
    /// The person or an agent wrote it out in full.
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CandidateStatus {
    Pending,
    Accepted,
    Rejected,
}

/// An entry waiting for the person (EXPERTISE §4.5). Agents may propose and flag, never accept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Candidate {
    pub id: String,
    pub owner_kind: OwnerKind,
    pub owner_id: String,
    pub domain: String,
    /// The entry as proposed; its id is filled in when accepted.
    pub proposed: ExpertiseEntry,
    /// Entries it may contradict or repeat: the person decides, nothing is overwritten.
    #[serde(default)]
    pub conflicts_with: Vec<String>,
    pub origin: CandidateOrigin,
    pub status: CandidateStatus,
    /// The run lead thinks it is worth keeping. Only the person can accept.
    #[serde(default)]
    pub suggested_by_lead: bool,
    /// The lead's reason, or the rework/review note it came from.
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub sources: Vec<SourceRef>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub decided_at: Option<DateTime<Utc>>,
}

/// How an entry reached a run (EXPERTISE §4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    /// Shown in a worker's or reviewer's brief.
    Pushed,
    /// The agent searched for it.
    Pulled,
    /// The worker named it in `task_complete.used`.
    Declared,
}

/// One time an entry was shown, searched for or named by a task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct UsageRecord {
    pub id: String,
    /// The entry, or the candidate's proposed entry by its proposed id.
    pub entry_id: String,
    #[serde(default)]
    pub run_id: Option<RunId>,
    #[serde(default)]
    pub task_id: Option<TaskId>,
    #[serde(default)]
    pub agent: Option<AgentId>,
    pub source: UsageSource,
    /// How it ended, once the task did; `None` while undecided.
    #[serde(default)]
    pub outcome: Option<UsageOutcome>,
    pub at: DateTime<Utc>,
}

/// How a task that used an entry ended (EXPERTISE §4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum UsageOutcome {
    /// Merged without rework.
    MergedFirstTry,
    /// Sent back at least once.
    Reworked,
    /// Dropped, or the task was.
    Dropped,
}

/// Rough token count of `text`: a CJK character is one token, other text about four characters
/// a token. Good enough for the per-domain judgement budget, not for billing.
pub fn estimate_tokens(text: &str) -> usize {
    let mut cjk = 0usize;
    let mut other = 0usize;
    for ch in text.chars() {
        if is_cjk(ch) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    cjk + other.div_ceil(4)
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x3000..=0x303F   // CJK punctuation
        | 0x3400..=0x4DBF // extension A
        | 0x4E00..=0x9FFF // unified ideographs
        | 0xF900..=0xFAFF // compatibility ideographs
        | 0xFF00..=0xFFEF // fullwidth forms
        | 0x20000..=0x2FA1F // extensions B and later
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_count_chinese_by_character() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("中文"), 2);
        assert_eq!(estimate_tokens("先跑测试再提交"), 7);
        // Punctuation that belongs to the script is one token too.
        assert_eq!(estimate_tokens("好，就这样"), 5);
    }

    #[test]
    fn tokens_count_english_by_four_characters() {
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcdefgh"), 2);
        assert_eq!(estimate_tokens("a"), 1);
        assert_eq!(estimate_tokens("中文 abc"), 3);
        assert!(estimate_tokens("a longer sentence about testing") < 15);
    }

    #[test]
    fn a_source_is_tagged_by_what_it_points_at() {
        let run = RunId::new();
        let json = serde_json::to_value(SourceRef::Task {
            run_id: run,
            task_id: TaskId::new(),
        })
        .unwrap();
        assert_eq!(json["kind"], "task");
        assert_eq!(json["run_id"], run.to_string());
        let back: SourceRef = serde_json::from_value(json).unwrap();
        assert!(matches!(back, SourceRef::Task { .. }));
    }

    #[test]
    fn an_entry_round_trips_and_older_json_gains_defaults() {
        let now = Utc::now();
        let entry = ExpertiseEntry {
            id: "e1".into(),
            owner_kind: OwnerKind::Person,
            owner_id: PERSON_OWNER_ID.into(),
            domain: "rust".into(),
            kind: EntryKind::Judgment,
            title: "测试先于实现".into(),
            text: "先写一个会失败的测试".into(),
            example: None,
            applies_when: "crates/**".into(),
            evidence: 2,
            last_confirmed_at: Some(now),
            status: EntryStatus::Active,
            superseded_by: None,
            stats: EntryStats {
                used: 3,
                merged_first_try: 2,
                ..EntryStats::default()
            },
            sources: vec![SourceRef::Run {
                run_id: RunId::new(),
            }],
            created_at: now,
            updated_at: now,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert_eq!(
            serde_json::from_str::<ExpertiseEntry>(&json).unwrap(),
            entry
        );
        // Fields added later read back as empty rather than failing.
        let older = serde_json::json!({
            "id": "e2", "owner_kind": "project", "owner_id": "p1",
            "domain": "gui", "kind": "limit", "title": "t",
            "status": "active", "created_at": now, "updated_at": now,
        });
        let read: ExpertiseEntry = serde_json::from_value(older).unwrap();
        assert_eq!(read.stats, EntryStats::default());
        assert!(read.sources.is_empty() && read.example.is_none());
        assert_eq!(read.kind, EntryKind::Limit);
    }
}
