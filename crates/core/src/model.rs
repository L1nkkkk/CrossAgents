//! Domain model. See docs/DESIGN.md §4 and §7.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
    pub id: AgentId,
    /// Unique, see [`validate_agent_name`].
    pub name: String,
    /// Plugin that runs this agent, e.g. `"claude"`.
    pub plugin: String,
    /// `None` means the plugin's default driver.
    pub driver: Option<String>,
    pub model: Option<String>,
    pub role: Role,
    pub cwd: PathBuf,
    /// Plugin-private settings; core never interprets them.
    pub config: serde_json::Value,
    pub status: AgentStatus,
    pub created_at: DateTime<Utc>,
    /// Who the agent is to people and to itself (docs/DESIGN.md §4.3).
    #[serde(default)]
    pub profile: AgentProfile,
}

/// The agent's card: how it is shown, and the persona it is given. `role` still decides its
/// tools; the profile makes it a particular colleague (a designer, a security reviewer, …).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AgentProfile {
    /// Shown instead of the name, e.g. "Ada". The name stays the address.
    #[serde(default)]
    pub display_name: Option<String>,
    /// What it does, e.g. "设计师" or "Security reviewer".
    #[serde(default)]
    pub title: Option<String>,
    /// An emoji or up to two letters; clients fall back to initials.
    #[serde(default)]
    pub avatar: Option<String>,
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

    /// The opening instructions this profile adds, if it says anything about the agent.
    pub fn instructions(&self, name: &str) -> Option<String> {
        if self.title.is_none() && self.persona.is_none() && self.display_name.is_none() {
            return None;
        }
        let who = self.display_name.as_deref().unwrap_or(name);
        let mut text = match &self.title {
            Some(title) => format!("You are {who}, the team's {title}."),
            None => format!("You are {who}."),
        };
        if let Some(persona) = &self.persona {
            text.push_str("\n\n");
            text.push_str(persona);
        }
        Some(text)
    }
}

/// Agent names are used as addresses (`agent_send(to = "alice")`), so keep them simple.
pub fn validate_agent_name(name: &str) -> Result<(), CoreError> {
    let mut chars = name.chars();
    let valid = name.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if valid {
        Ok(())
    } else {
        Err(CoreError::InvalidName(name.to_owned()))
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
    /// Why the run failed or was cancelled, or the last plan rejection.
    #[serde(default)]
    pub note: Option<String>,
    /// The submitted plan; tasks are created from it on approval.
    #[serde(default)]
    pub plan: Vec<PlannedTask>,
    /// How the run's result reached the base branch, once it did.
    #[serde(default)]
    pub landed: Option<Landing>,
    /// The lead's closing summary (`run_finish`).
    #[serde(default)]
    pub summary: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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

/// What is known about a task's outcome so far.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TaskReport {
    /// The worker's summary from `task_complete`.
    #[serde(default)]
    pub summary: Option<String>,
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
    /// The assignee's latest one-line progress (`task_progress`).
    #[serde(default)]
    pub progress: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Task {
    /// The agent that decides on the task: its issuer, else (older tasks) the run's planner.
    pub fn issuer_or(&self, planner: AgentId) -> AgentId {
        self.issuer.unwrap_or(planner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_names() {
        for name in ["a", "alice", "worker-1", "plan_2", &"a".repeat(32)] {
            assert!(validate_agent_name(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn rejects_invalid_names() {
        for name in [
            "",
            "Alice",
            "1worker",
            "-x",
            "has space",
            "émile",
            "a.b",
            &"a".repeat(33),
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
}
