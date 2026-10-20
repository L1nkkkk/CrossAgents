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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Run {
    pub id: RunId,
    pub title: String,
    pub planner: Option<AgentId>,
    pub status: RunStatus,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Task {
    pub id: TaskId,
    pub run_id: RunId,
    pub parent: Option<TaskId>,
    pub title: String,
    pub spec: TaskSpec,
    pub assignee: Option<AgentId>,
    pub status: TaskStatus,
    pub attempts: u32,
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
