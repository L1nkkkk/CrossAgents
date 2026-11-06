//! Projects and their troubleshooting records (docs/tasks/LESSONS.md): problems agents ran
//! into during a run, kept as run notes under `issue/`, sorted at the end into lessons of the
//! project that any later agent of it can search. Lessons are reference, never rules.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, RunId};

/// Run notes whose key starts with this are problems a worker recorded (LESSONS §4.1).
pub const ISSUE_PREFIX: &str = "issue/";

/// A repository as the hub knows it across machines (LESSONS §4.2): lessons belong to it, and
/// it is recognised by its remotes and by where each node has it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Project {
    pub id: String,
    /// Shown in lists: the repository directory's name when first seen.
    pub name: String,
    /// Normalised remotes (`host/owner/repo`) that identify it.
    pub remotes: Vec<String>,
    pub paths: Vec<ProjectPath>,
    /// A remote seen at one of its paths that is not one of its own: the directory may now
    /// hold another repository (a fork, a replacement), so the person decides. Until then
    /// nothing is bound automatically to or from this project.
    #[serde(default)]
    pub unconfirmed_remote: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Where a node has the project's main checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ProjectPath {
    pub node: String,
    pub path: PathBuf,
}

/// How a recorded problem ended, as far as anyone knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum LessonOutcome {
    Resolved,
    Workaround,
    Unresolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum LessonStatus {
    /// Good to go by.
    Active,
    /// Someone found it no longer works; check before relying on it.
    Review,
    /// Replaced by `superseded_by`; kept for its history.
    Superseded,
}

/// One time the lesson was tried: when it was written and every report after.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Evidence {
    #[serde(default)]
    pub run_id: Option<RunId>,
    pub agent: AgentId,
    pub node: String,
    pub at: DateTime<Utc>,
    /// The commit checked out where it was tried, when known.
    #[serde(default)]
    pub commit: Option<String>,
    /// Whether that checkout had uncommitted changes.
    #[serde(default)]
    pub dirty: Option<bool>,
    pub outcome: LessonOutcome,
    #[serde(default)]
    pub note: String,
}

/// A recorded problem: a run note under [`ISSUE_PREFIX`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct IssueRef {
    pub run_id: RunId,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Lesson {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub symptom: String,
    pub keywords: Vec<String>,
    /// Where it applies: system, tools and versions, node, files.
    pub conditions: String,
    /// Files the record is about, repository-relative when possible: a review clue only
    /// (LESSONS §4.4). When one of them changed after the latest check, a search result says
    /// so; nothing is invalidated automatically.
    #[serde(default)]
    pub files: Vec<String>,
    pub resolution: String,
    pub outcome: LessonOutcome,
    pub status: LessonStatus,
    #[serde(default)]
    pub superseded_by: Option<String>,
    pub evidence: Vec<Evidence>,
    pub sources: Vec<IssueRef>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// What became of a recorded problem when it was sorted (LESSONS §4.3). Every one counts as
/// done, so issues worth nothing do not stay pending forever.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Triage {
    /// A new lesson was made of it.
    Lesson { lesson_id: String },
    /// It went into an existing lesson.
    Merged { lesson_id: String },
    /// Left in the run: nothing to reuse.
    Kept {
        #[serde(default)]
        why: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TriageRecord {
    pub issue: IssueRef,
    #[serde(flatten)]
    pub triage: Triage,
    pub by: AgentId,
    pub at: DateTime<Utc>,
}

/// A lesson that may bear on a failure the hub saw (LESSONS §4.5 ②): attached as
/// `related_lessons` to a failed run check, a `task_fail` and a task whose checks failed.
/// A match only means *possibly related*: the words of the error were found in the record,
/// not that its resolution was confirmed to apply here. Read it with `lesson_get` and check
/// its conditions before going by it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RelatedLesson {
    pub id: String,
    pub title: String,
    /// The words of the error output found in the record, the strong ones (error codes,
    /// exception names) first. At least one of them is strong.
    pub matched: Vec<String>,
}

/// One sorting step, applied by the store in a single transaction together with marking its
/// issues (LESSONS §4.3): issues already sorted are left as they are, and a step whose issues
/// are all sorted already writes nothing, so a retry after a crash does not duplicate.
#[derive(Debug, Clone)]
pub enum TriageStep {
    /// Store this new lesson; its `sources` are set to the issues not yet sorted.
    Create(Lesson),
    /// Replace the stored lesson with this one (same id), the issues' evidence added.
    Merge(Lesson),
    Keep {
        why: String,
    },
}
