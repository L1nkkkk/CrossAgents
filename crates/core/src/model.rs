//! Domain model. See docs/DESIGN.md §4 and §7.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::automation::Health;
use crate::policy::{PlanApproval, RunPolicy};

use crate::error::CoreError;
use crate::ids::{AgentId, RunId, TaskId};
use crate::state::TaskStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Planner,
    Worker,
    Reviewer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Stopped,
    Starting,
    Idle,
    Running,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Agent {
    /// Stable and opaque: everything the hub stores or routes refers to the agent by it.
    pub id: AgentId,
    /// What people and teammates call it, e.g. "Luna"; unique (ignoring case) and renamable,
    /// see [`validate_agent_name`]. Tools take it and resolve it to the id at once.
    pub name: String,
    /// Whether a person chose the name or the hub made it up (and it is there to be renamed).
    #[serde(default)]
    pub name_source: NameSource,
    /// Plugin that runs this agent, e.g. `"claude"`.
    pub plugin: String,
    /// `None` means the plugin's default driver.
    pub driver: Option<String>,
    pub model: Option<String>,
    pub role: Role,
    /// The workspace the agent belongs to (docs/DESIGN.md §4.7): where it lives between
    /// executions, and what the sidebar groups it under. Empty in records from before homes,
    /// which read it as `cwd`.
    #[serde(default)]
    pub home: PathBuf,
    /// Where it works now: its home, or the worktree of the task it is executing.
    pub cwd: PathBuf,
    /// Plugin-private settings; core never interprets them.
    pub config: serde_json::Value,
    pub status: AgentStatus,
    pub created_at: DateTime<Utc>,
    /// Who the agent is to people and to itself (docs/DESIGN.md §4.3).
    #[serde(default)]
    pub profile: AgentProfile,
    /// A conversation's title (chat mode), what lists show it by; not the agent's name.
    #[serde(default)]
    pub topic: Option<String>,
    /// Set while the conversation is archived: out of the lists and stopped, every
    /// transcript kept; restoring clears it.
    #[serde(default)]
    pub archived_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NameSource {
    /// Given by a person or the agent that recruited it.
    #[default]
    Chosen,
    /// Made up by the hub (see [`crate::names`]); clients offer to rename it.
    Generated,
}

/// `config` key naming the conversation that recruited an agent (chat mode); a run's
/// recruits carry `spawned_by_run` instead.
pub const SPAWNED_BY_AGENT: &str = "spawned_by_agent";

/// `config` key saying why the agent's next session cannot carry on its conversation (its
/// driver resumes only in the directory the session ran in, and that went with a deleted
/// run): the hub opens a new session and says so instead of resuming somewhere else
/// (docs/tasks/S1.md §7).
pub const CONTEXT_LOST: &str = "context_lost";

impl Agent {
    /// Its workspace: [`Self::home`], or its cwd in a record from before homes.
    pub fn home(&self) -> &std::path::Path {
        if self.home.as_os_str().is_empty() {
            &self.cwd
        } else {
            &self.home
        }
    }

    /// Why its next session cannot carry on its conversation ([`CONTEXT_LOST`]).
    pub fn context_lost(&self) -> Option<&str> {
        self.config.get(CONTEXT_LOST).and_then(|v| v.as_str())
    }

    /// The agent whose conversation recruited this one, if any.
    pub fn recruiter(&self) -> Option<AgentId> {
        self.config
            .get(SPAWNED_BY_AGENT)
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
    }

    /// Who the agent is, said at the top of its instructions: its name (so it does not
    /// introduce itself as the vendor's assistant), the model it runs on, then its card.
    pub fn identity(&self) -> String {
        let model = self.model.as_deref().unwrap_or("its default model");
        let mut text = format!(
            "You are {name}, an agent in CrossAgents running on {model} ({plugin}). People and teammates call you {name}; refer to teammates by their names, not by their models.",
            name = self.name,
            plugin = self.plugin,
        );
        if let Some(title) = &self.profile.title {
            text.push_str(&format!(" Your role on the team: {title}."));
        }
        if let Some(persona) = &self.profile.persona {
            text.push_str("\n\nAbout you: ");
            text.push_str(persona);
        }
        text
    }
}

/// The agent's card: how it is shown, and the persona it is given. `role` still decides its
/// tools; the profile makes it a particular colleague (a designer, a security reviewer, …).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AgentProfile {
    /// From before names were for people: what was shown instead of the name. The hub folds
    /// it into the name (or a chat's topic) when it starts; kept to read old records.
    #[serde(default)]
    pub display_name: Option<String>,
    /// What it does, e.g. "设计师" or "Security reviewer".
    #[serde(default)]
    pub title: Option<String>,
    /// An emoji or up to two letters; clients fall back to initials.
    #[serde(default)]
    pub avatar: Option<String>,
    /// A picture from the hub's avatar gallery (its content hash); shown instead of `avatar`.
    #[serde(default)]
    pub avatar_image: Option<String>,
    /// Avatar colour, one of [`AgentProfile::COLORS`].
    #[serde(default)]
    pub color: Option<String>,
    /// Expertise and working style, added to the agent's opening instructions.
    #[serde(default)]
    pub persona: Option<String>,
}

impl AgentProfile {
    pub const COLORS: [&'static str; 8] = [
        "indigo", "violet", "rose", "amber", "emerald", "teal", "sky", "slate",
    ];
    const MAX_TEXT: usize = 64;
    const MAX_AVATAR: usize = 8;
    const MAX_PERSONA: usize = 4000;

    /// Trims fields, turns empty ones into `None` and checks lengths and the colour.
    pub fn normalized(self) -> Result<Self, CoreError> {
        let clean = |v: Option<String>| v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
        let profile = Self {
            display_name: clean(self.display_name),
            title: clean(self.title),
            avatar: clean(self.avatar),
            avatar_image: clean(self.avatar_image),
            color: clean(self.color),
            persona: clean(self.persona),
        };
        let too_long =
            |v: &Option<String>, max: usize| v.as_ref().is_some_and(|s| s.chars().count() > max);
        if too_long(&profile.display_name, Self::MAX_TEXT)
            || too_long(&profile.title, Self::MAX_TEXT)
        {
            return Err(CoreError::Unsupported(format!(
                "display name and title are at most {} characters",
                Self::MAX_TEXT
            )));
        }
        if too_long(&profile.avatar, Self::MAX_AVATAR) {
            return Err(CoreError::Unsupported(format!(
                "avatar is at most {} characters (an emoji or initials)",
                Self::MAX_AVATAR
            )));
        }
        if let Some(image) = &profile.avatar_image
            && !(image.len() == 64 && image.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(CoreError::Unsupported(format!(
                "avatar image {image:?} is not a gallery picture"
            )));
        }
        if too_long(&profile.persona, Self::MAX_PERSONA) {
            return Err(CoreError::Unsupported(format!(
                "persona is at most {} characters",
                Self::MAX_PERSONA
            )));
        }
        if let Some(color) = &profile.color
            && !Self::COLORS.contains(&color.as_str())
        {
            return Err(CoreError::Unsupported(format!(
                "color {color:?} (known: {})",
                Self::COLORS.join(", ")
            )));
        }
        Ok(profile)
    }
}

/// Ids people type that end up in paths and environment variables (account ids, plugin
/// names): lowercase ASCII letters, digits, `_` and `-`, starting with a letter, at most 32.
pub fn validate_slug(slug: &str) -> Result<(), CoreError> {
    let mut chars = slug.chars();
    let valid = slug.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if valid {
        Ok(())
    } else {
        Err(CoreError::InvalidName(format!(
            "{slug:?}: use lowercase letters, digits, - and _, starting with a letter (at most 32)"
        )))
    }
}

/// Agent names are what people read and what teammates address (`agent_send(to = "Luna")`):
/// 1–32 characters of letters (any script), digits, spaces, `-`, `_` and `.`, starting with a
/// letter, no doubled or outer spaces, and never something that reads as an id.
pub fn validate_agent_name(name: &str) -> Result<(), CoreError> {
    let count = name.chars().count();
    let valid = (1..=32).contains(&count)
        && name.trim() == name
        && !name.contains("  ")
        && name.chars().next().is_some_and(char::is_alphabetic)
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
        && name.parse::<uuid::Uuid>().is_err();
    if valid {
        Ok(())
    } else {
        Err(CoreError::InvalidName(format!(
            "{name:?}: use 1–32 letters, digits, spaces, - _ or ., starting with a letter"
        )))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Planning,
    AwaitingApproval,
    Running,
    /// Stopped for a person's decision (e.g. the budget ran out); resumes or is cancelled.
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl RunStatus {
    /// Adding a terminal status here means adding it to the `runs_one_open_per_work` index
    /// too: it spells the same three out in SQL (`crates/store-sqlite/migrations/0011_works.sql`),
    /// and a migration that has been applied cannot be edited — sqlx checksums it — so the
    /// new status needs a migration of its own that rebuilds the index. Until then a work
    /// whose run ended in that status would refuse to start another round.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// How a run is carried out. See docs/DESIGN.md §8.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RunConfig {
    /// Template for worker agents: each task gets a fresh clone of it.
    pub worker: AgentId,
    /// Stronger template a task moves to after `max_attempts` failures.
    #[serde(default)]
    pub escalate: Option<AgentId>,
    /// Reviews each verified diff before it is merged.
    #[serde(default)]
    pub reviewer: Option<AgentId>,
    /// Dispatch without waiting for a human to approve the plan.
    #[serde(default)]
    pub auto_approve: bool,
    pub max_parallel: u32,
    /// Attempts per worker before escalating.
    pub max_attempts: u32,
    /// Teammates the planner may assign tasks to (workers) or have them reviewed by
    /// (reviewers), by specialty. Always includes `worker` and `reviewer`.
    #[serde(default)]
    pub team: Vec<AgentId>,
    /// How much the run may do without a person (docs/tasks/H1.md §4).
    #[serde(default)]
    pub policy: RunPolicy,
}

impl RunConfig {
    /// Whether the plan starts without a person's approval.
    pub fn auto_approves(&self) -> bool {
        self.auto_approve || self.policy.plan_approval == PlanApproval::Auto
    }

    pub const DEFAULT_MAX_PARALLEL: u32 = 3;
    pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Run {
    /// The conversation that proposed this work; its independent lead is `planner`.
    #[serde(default)]
    pub origin_chat_id: Option<AgentId>,
    #[serde(default)]
    pub work_id: Option<crate::ids::WorkId>,
    #[serde(default)]
    pub continuation: Option<Box<ContinuationState>>,
    pub id: RunId,
    pub title: String,
    /// What the human asked for.
    pub request: String,
    pub planner: AgentId,
    pub status: RunStatus,
    pub config: RunConfig,
    /// The repository the run works on (the planner's directory).
    pub repo: PathBuf,
    /// Branch the run started from.
    pub base_branch: String,
    /// Integration branch that collects every merged task.
    pub branch: String,
    /// The directory is not under version control: tasks run one at a time in it directly,
    /// with no branches, worktrees, diffs or merges (and nothing to roll back to).
    #[serde(default)]
    pub in_place: bool,
    /// Why the run failed or was cancelled, or the last plan rejection.
    #[serde(default)]
    pub note: Option<String>,
    /// The submitted plan; tasks are created from it on approval.
    #[serde(default)]
    pub plan: Vec<PlannedTask>,
    /// What to know about the lead: set when it is a copy of the agent picked, without that
    /// agent's conversation (its driver cannot resume a session elsewhere, docs/tasks/S1.md
    /// §3.2).
    #[serde(default)]
    pub lead_note: Option<String>,
    /// How the run's result reached the base branch, once it did.
    #[serde(default)]
    pub landed: Option<Landing>,
    /// The lead's closing summary (`run_finish`).
    #[serde(default)]
    pub summary: Option<String>,
    /// The project's run checks (`crossagents.toml`), as they last ran before the lead
    /// landed or finished the run.
    #[serde(default)]
    pub checks: Vec<CheckResult>,
    /// Whether those checks verified the run; none yet when nothing called for them.
    #[serde(default)]
    pub checks_state: Option<ChecksState>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A persistent workspace containing successive runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Work {
    pub id: crate::ids::WorkId,
    pub title: String,
    pub repo: PathBuf,
    pub lead: AgentId,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Whether the project's checks verified a run (docs/tasks/TOOLCHAIN.md §5): no results is
/// not a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ChecksState {
    /// The project declares none: nothing was verified.
    NotConfigured,
    /// None of the `declared` checks covers what the run changed: nothing was verified.
    NotApplicable { declared: u32 },
    /// They ran; `checks` has how they went.
    Ran,
}

/// How a run's branch is brought into its base branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum LandMode {
    /// A merge commit on the base branch.
    Merge,
    /// One commit with all of the run's changes.
    Squash,
    /// A pull request from the run branch (needs a remote and the `gh` CLI).
    Pr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Landing {
    pub mode: LandMode,
    /// Who landed it: the lead's name, or "person".
    pub by: String,
    /// The commit on the base branch (merge, squash) or the pull request's URL (pr).
    pub reference: String,
    pub at: DateTime<Utc>,
}

/// What a run left behind in its workspace, kept after the run itself was deleted: a run is
/// hard-deleted, its history is not. Written when the run ends and again when it lands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RunHistory {
    pub run_id: RunId,
    /// The work group the run was a round of.
    #[serde(default)]
    pub work_id: Option<crate::ids::WorkId>,
    /// The repository the run worked on: the history is kept per workspace.
    pub repo: PathBuf,
    pub title: String,
    #[serde(default)]
    pub request: String,
    pub status: RunStatus,
    /// The lead's closing summary, when it gave one.
    #[serde(default)]
    pub summary: Option<String>,
    /// Why the run failed or was cancelled, or the last plan rejection.
    #[serde(default)]
    pub note: Option<String>,
    pub base_branch: String,
    pub branch: String,
    /// How the run's result reached the base branch, once it did.
    #[serde(default)]
    pub landed: Option<Landing>,
    /// Its tasks as they ended: key, title, who did it, and how it ended.
    #[serde(default)]
    pub tasks: Vec<HistoryTask>,
    /// The commits the merged tasks brought into the run branch, in merge order.
    #[serde(default)]
    pub merged_commits: Vec<String>,
    pub created_at: DateTime<Utc>,
    /// When the run reached a terminal state; later writes keep the first one.
    #[serde(default)]
    pub ended_at: Option<DateTime<Utc>>,
    /// The run itself is gone (deleted); the record stays, read-only.
    #[serde(default)]
    pub deleted: bool,
}

/// One task as a workspace's history keeps it (`RunHistory.tasks`): the name and model of who
/// did it, not the id, because the agents may be gone by the time the history is read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HistoryTask {
    pub key: String,
    pub title: String,
    /// The worker's name; whoever the task was assigned to, or the teammate it was cloned from.
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub status: TaskStatus,
}

/// Files a task may or may not touch, as glob patterns relative to the workspace root.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Scope {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

/// The contract a worker is held to. See docs/DESIGN.md §7.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TaskSpec {
    pub goal: String,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub interfaces: Vec<String>,
    /// Commands that must all succeed for the task to pass.
    #[serde(default)]
    pub acceptance: Vec<String>,
    #[serde(default)]
    pub references: Vec<String>,
    /// JSON Schema for the worker's structured result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_schema: Option<serde_json::Value>,
}

/// One task in a submitted plan. `key` is the planner's own short name for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PlannedTask {
    pub key: String,
    pub title: String,
    pub spec: TaskSpec,
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Teammate (worker, by name) whose specialty fits; `None` means the run's worker.
    #[serde(default)]
    pub assignee: Option<String>,
    /// Teammate (reviewer, by name) to review this task instead of the run's reviewer.
    #[serde(default)]
    pub reviewer: Option<String>,
    /// `true` (the default): a new execution session of `assignee` does the task, starting
    /// from a clean context. `false`: `assignee` itself, with its conversation — a session of
    /// the run's workspace (docs/tasks/S1.md §3.3).
    #[serde(default = "yes")]
    pub fresh: bool,
    /// With `fresh` false: restore `assignee` first when it is archived.
    #[serde(default)]
    pub restore: bool,
    /// Like `fresh`, for `reviewer`.
    #[serde(default = "yes")]
    pub reviewer_fresh: bool,
}

fn yes() -> bool {
    true
}

/// The result of one acceptance command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CheckResult {
    pub command: String,
    pub exit_code: Option<i32>,
    pub passed: bool,
    /// Combined stdout and stderr, truncated.
    pub output: String,
}

fn deserialize_result<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<serde_json::Value>, D::Error> {
    serde_json::Value::deserialize(deserializer).map(Some)
}

/// How badly one review finding hurts: P0 blocks the merge and must be fixed; P1 should be
/// fixed before the merge; P2 can follow it; P3 is a suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ReviewSeverity {
    P0,
    P1,
    P2,
    P3,
}

/// Whether the review lets the change merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    Merge,
    MergeAfterFixes,
    DoNotMerge,
}

/// One problem a reviewer found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ReviewFinding {
    pub severity: ReviewSeverity,
    /// `0` (a guess) to `1` (certain).
    pub confidence: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// What is known about a task's outcome so far.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TaskReport {
    /// The worker's summary from `task_complete`.
    #[serde(default)]
    pub summary: Option<String>,
    /// Structured result supplied by the worker.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_result"
    )]
    pub result: Option<serde_json::Value>,
    /// Latest acceptance results.
    #[serde(default)]
    pub checks: Vec<CheckResult>,
    /// `git diff --stat` of the task against the integration branch.
    #[serde(default)]
    pub diff_stat: Option<String>,
    /// Why the task failed, if it did.
    #[serde(default)]
    pub failure: Option<String>,
    /// Open question from the worker.
    #[serde(default)]
    pub question: Option<String>,
    /// The message that carried the open question to the issuer; a reply to it answers.
    #[serde(default)]
    pub question_message: Option<crate::ids::MessageId>,
    /// The reviewer's verdict notes.
    #[serde(default)]
    pub review: Option<String>,
    /// The reviewer's findings, most severe first (P0 before P1 before P2 before P3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<ReviewFinding>,
    /// The reviewer's conclusion; absent in reports written before verdicts existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<ReviewVerdict>,
    /// What the issuer (or the person) wrote each time it sent the task back for rework.
    #[serde(default)]
    pub rework: Vec<String>,
    /// The verification this report answers: a fresh id each time the task enters
    /// `Verifying`. A report written for another one (a rework, or a later verification that
    /// started meanwhile) is dropped instead of overwriting it (docs/TODO.md §九 ①).
    #[serde(default)]
    pub verification: Option<String>,
    /// The commit the checks ran on. `task_accept` merges this one, and only this one.
    #[serde(default)]
    pub verified_sha: Option<String>,
    /// The hub restarted before the checks finished: nothing verified this report. Counted
    /// as a failure, so accepting it takes `force`.
    #[serde(default)]
    pub interrupted: bool,
    /// The task has no checks (`spec.acceptance` is empty), so nothing verified it.
    #[serde(default)]
    pub unchecked: bool,
    /// Why the issuer accepted it without a verification; also that it did.
    #[serde(default)]
    pub forced: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Task {
    pub id: TaskId,
    pub run_id: RunId,
    pub key: String,
    pub title: String,
    pub spec: TaskSpec,
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Who created the task and decides on its result (docs/tasks/H1.md §2); the run's
    /// planner for tasks from a plan. `None` in tasks recorded before issuers existed.
    #[serde(default)]
    pub issuer: Option<AgentId>,
    /// The worker agent currently on the task.
    #[serde(default)]
    pub assignee: Option<AgentId>,
    /// The reviewer agent while the task is in review.
    #[serde(default)]
    pub reviewer: Option<AgentId>,
    /// The teammate workers on this task are cloned from (the plan's `assignee`).
    #[serde(default)]
    pub worker_template: Option<AgentId>,
    /// The teammate reviewers on this task are cloned from (the plan's `reviewer`).
    #[serde(default)]
    pub reviewer_template: Option<AgentId>,
    /// Whether its worker is a new execution session of `worker_template` (`true`), or
    /// `worker_template` itself, with its conversation (docs/tasks/S1.md §3.3).
    #[serde(default = "yes")]
    pub worker_fresh: bool,
    /// With `worker_fresh` false: restore it first if it is archived.
    #[serde(default)]
    pub worker_restore: bool,
    /// Like `worker_fresh`, for `reviewer_template`.
    #[serde(default = "yes")]
    pub reviewer_fresh: bool,
    pub status: TaskStatus,
    /// Attempts by the current worker.
    pub attempts: u32,
    /// Whether the task moved to the escalation template.
    #[serde(default)]
    pub escalated: bool,
    /// Reminders sent because the worker ended a turn without reporting.
    #[serde(default)]
    pub nudges: u32,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub worktree: Option<PathBuf>,
    #[serde(default)]
    pub report: TaskReport,
    /// The task this one was split from (`delegate` policy): it branches from the parent's
    /// branch and merges back into it.
    #[serde(default)]
    pub parent: Option<TaskId>,
    /// The assignee's latest one-line progress (`task_progress`).
    #[serde(default)]
    pub progress: Option<String>,
    /// Each time the task changed hands, oldest first. Empty in tasks recorded before.
    #[serde(default)]
    pub handoffs: Vec<crate::handoff::Handoff>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Task {
    /// The agent that decides on the task: its issuer, else (older tasks) the run's planner.
    pub fn issuer_or(&self, planner: AgentId) -> AgentId {
        self.issuer.unwrap_or(planner)
    }
}

/// Persisted supervisor checkpoint. Attempts are reserved before delivery, so a crash
/// cannot replay a reserved wakeup. A lost delivery consumes an attempt conservatively.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ContinuationState {
    pub owner: AgentId,
    pub phase: String,
    pub next_step: String,
    pub attempts: u32,
    pub next_at: Option<DateTime<Utc>>,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub delivery_error: Option<String>,
    /// The person was told that the attempts ran out. Kept in the run record, so the
    /// check that repeats every few seconds, and a restarted hub reading this record,
    /// both see that the notice was already made. Cleared when the run is re-armed below
    /// the cap (attempts reset), so a later exhaustion tells them again.
    #[serde(default, skip_serializing_if = "is_false")]
    pub exhausted_notified: bool,
    /// The person was told that a wakeup could not be delivered. One notice per run of
    /// consecutive failures; cleared once a delivery succeeds, so the next failure after
    /// that is a new fact worth one notice. Persisted for the same reason as above.
    #[serde(default, skip_serializing_if = "is_false")]
    pub delivery_error_notified: bool,
    /// The opt-in stuck watch (`ContinuationPolicy::stuck_minutes`): when this run last
    /// showed durable progress while it waits on a busy agent. Absent unless that policy is
    /// set and the run is in such a phase; old records read it back as absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stuck_watch: Option<StuckWatch>,
    /// What the wakeups have done lately. The automation breaker pauses the continuation when
    /// it ran at least 50 times in 7 days and failed 90% of them
    /// (docs/research/multica.md §2.14); a paused run is not woken again.
    #[serde(default, skip_serializing_if = "Health::is_empty")]
    pub health: Health,
}

impl Health {
    /// Nothing has run and nothing is paused: the field stays out of the run JSON.
    fn is_empty(&self) -> bool {
        self.runs.is_empty() && self.paused_at.is_none()
    }
}

/// Progress bookmark of the stuck watch. It lives in the run record, so a restarted hub
/// resumes the same quiet stretch instead of forgetting it. It only ever says when
/// `continuation_step` last saw progress; waking still goes through `attempts`/`next_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct StuckWatch {
    /// When durable progress was last seen: the newest `updated_at` among the run and its
    /// tasks, or the time of the newest event one of the run's own agents appended.
    pub progress_at: DateTime<Utc>,
    /// The event log was read up to this sequence number. Events after it are new; the
    /// watch keeps this fresh so the next look only reads what happened since.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub event_seq: u64,
}

/// Keeps a `false` flag out of the run JSON: old records read it back as `false`, and
/// clients see the field as optional.
fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tasks_recorded_before_handoffs_load_with_none() {
        let now = Utc::now();
        let old = serde_json::json!({
            "id": TaskId::new(), "run_id": RunId::new(), "key": "a", "title": "t",
            "spec": { "goal": "g" }, "status": "running", "attempts": 1,
            "report": { "summary": "s" }, "created_at": now, "updated_at": now,
        });
        let task: Task = serde_json::from_value(old).unwrap();
        assert!(task.handoffs.is_empty());
        assert!(task.report.rework.is_empty());
    }

    #[test]
    fn identity_names_the_agent_and_its_model() {
        let agent = Agent {
            id: AgentId::new(),
            name: "Luna".into(),
            name_source: NameSource::Generated,
            plugin: "codex".into(),
            driver: None,
            model: Some("gpt-6-luna".into()),
            role: Role::Worker,
            home: PathBuf::from("."),
            cwd: PathBuf::from("."),
            config: serde_json::Value::Null,
            status: AgentStatus::Stopped,
            created_at: Utc::now(),
            profile: AgentProfile::default(),
            topic: Some("写个冒泡排序".into()),
            archived_at: None,
        };
        let text = agent.identity();
        assert!(
            text.starts_with(
                "You are Luna, an agent in CrossAgents running on gpt-6-luna (codex)."
            ),
            "{text}"
        );
        assert!(
            !text.contains("写个冒泡排序"),
            "the title is not the name: {text}"
        );
    }

    #[test]
    fn accepts_valid_names() {
        for name in [
            "a",
            "alice",
            "Luna",
            "Nova 2",
            "worker-1",
            "plan_2",
            "émile",
            "a.b",
            "露娜",
            &"z".repeat(32),
        ] {
            assert!(validate_agent_name(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn rejects_invalid_names() {
        for name in [
            "",
            "1worker",
            "-x",
            " x",
            "x ",
            "a  b",
            "a/b",
            "01a163ee-3727-75d0-ab0f-e2fb5e88ffdf",
            // 32 hex digits read as an id too.
            &"a".repeat(32),
            &"z".repeat(33),
        ] {
            assert!(validate_agent_name(name).is_err(), "{name}");
        }
    }

    #[test]
    fn task_spec_fills_defaults() {
        let spec: TaskSpec = serde_json::from_str(r#"{"goal":"do it"}"#).unwrap();
        assert_eq!(spec.goal, "do it");
        assert!(spec.acceptance.is_empty() && spec.scope.allow.is_empty());
    }

    #[test]
    fn continuation_state_recorded_before_notices_loads_as_not_notified() {
        let now = Utc::now();
        let old = serde_json::json!({
            "owner": AgentId::new(), "phase": "exhausted", "next_step": "a person must look",
            "attempts": 3, "next_at": null, "last_attempt_at": now, "delivery_error": null,
        });
        let state: ContinuationState = serde_json::from_value(old).unwrap();
        assert!(!state.exhausted_notified);
        assert!(!state.delivery_error_notified);
        assert!(
            state.stuck_watch.is_none(),
            "a run recorded before the stuck watch has none"
        );
    }

    #[test]
    fn a_stuck_watch_round_trips_and_stays_out_of_records_without_one() {
        let now = Utc::now();
        let state = ContinuationState {
            owner: AgentId::new(),
            phase: "waiting_tasks".into(),
            next_step: "wait".into(),
            attempts: 1,
            next_at: None,
            last_attempt_at: None,
            delivery_error: None,
            exhausted_notified: false,
            delivery_error_notified: false,
            stuck_watch: Some(StuckWatch {
                progress_at: now,
                event_seq: 42,
            }),
            health: Health::default(),
        };
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(json["stuck_watch"]["event_seq"], 42);
        assert_eq!(
            serde_json::from_value::<ContinuationState>(json).unwrap(),
            state
        );

        let mut without = state.clone();
        without.stuck_watch = None;
        let json = serde_json::to_value(&without).unwrap();
        assert!(
            json.get("stuck_watch").is_none(),
            "the default record stays as it was: {json}"
        );
    }
}
