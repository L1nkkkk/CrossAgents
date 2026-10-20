//! Domain model. See docs/DESIGN.md §4 and §7.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

impl RunConfig {
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
    /// The worker agent currently on the task.
    #[serde(default)]
    pub assignee: Option<AgentId>,
    /// The reviewer agent while the task is in review.
    #[serde(default)]
    pub reviewer: Option<AgentId>,
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
