//! How much a run may do without a person (docs/tasks/H1.md §4). Set once per run (or as
//! the hub's default); the hub enforces it, prompts do not.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum PlanApproval {
    /// A person approves the plan before work starts.
    #[default]
    Required,
    /// Work starts as soon as the plan is in.
    Auto,
}

/// Whether a run's workers ask the person before dangerous actions (docs/tasks/DOGFOOD.md §7).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    /// Pushes, publishing, CI changes and the like wait for the person's yes.
    #[default]
    Guard,
    /// Workers act without asking; the worktree is the only boundary.
    Off,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum LandPolicy {
    /// A person lands the result on the base branch.
    #[default]
    Manual,
    /// The lead may merge the run into the base branch itself.
    Auto,
    /// The lead may only open a pull request.
    Pr,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum AskPolicy {
    /// Agents may ask; questions wait for the answer.
    #[default]
    Allow,
    /// Agents may ask; after `ask_timeout_minutes` the default answer is used.
    Timeout,
    /// Agents decide for themselves.
    Deny,
}

/// Caps for a run; `None` means no cap.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Budget {
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub max_usd: Option<f64>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub max_minutes: Option<u64>,
}

impl Budget {
    pub fn is_set(&self) -> bool {
        self.max_tokens.is_some() || self.max_usd.is_some() || self.max_minutes.is_some()
    }
}

/// Caps for one task, from its `spec.budget` or the run's `task_budget` (the default for every
/// task). `None` means no cap. A task's own cap wins field by field.
///
/// The hub measures spend as agents report it. Dollars are the price table's estimate (a task
/// with an unpriced model is not held to `max_usd`, and the lead is told why); requests count
/// only when the agents report them. Usage is reported when a turn ends, so a cap can be
/// passed by one turn before the lead hears of it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(default)]
pub struct TaskBudget {
    pub max_usd: Option<f64>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub max_tokens: Option<u64>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub max_requests: Option<u64>,
    /// How many times the task may be sent back for rework before the lead decides.
    pub max_rework: Option<u32>,
    /// Minutes from the task's first start, to its end (or now).
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub max_minutes: Option<u64>,
    /// Minutes without progress (a status change, a check, a report, or usage) while the task
    /// is being worked on.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub max_idle_minutes: Option<u64>,
}

impl TaskBudget {
    pub fn is_set(&self) -> bool {
        self.max_usd.is_some()
            || self.max_tokens.is_some()
            || self.max_requests.is_some()
            || self.max_rework.is_some()
            || self.max_minutes.is_some()
            || self.max_idle_minutes.is_some()
    }

    /// The caps a task gets: its own, field by field, else the run's default.
    pub fn with_default(&self, default: &TaskBudget) -> TaskBudget {
        TaskBudget {
            max_usd: self.max_usd.or(default.max_usd),
            max_tokens: self.max_tokens.or(default.max_tokens),
            max_requests: self.max_requests.or(default.max_requests),
            max_rework: self.max_rework.or(default.max_rework),
            max_minutes: self.max_minutes.or(default.max_minutes),
            max_idle_minutes: self.max_idle_minutes.or(default.max_idle_minutes),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RunPolicy {
    /// None preserves H1: notifications only, no periodic recovery.
    #[serde(default)]
    pub continuation: Option<ContinuationPolicy>,
    #[serde(default)]
    pub plan_approval: PlanApproval,
    #[serde(default)]
    pub land: LandPolicy,
    #[serde(default)]
    pub human_ask: AskPolicy,
    /// For [`AskPolicy::Timeout`].
    #[serde(default = "default_ask_timeout")]
    pub ask_timeout_minutes: f64,
    #[serde(default)]
    pub budget: Budget,
    /// Caps every task gets unless its plan sets its own (`spec.budget`); absent: no caps.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub task_budget: Option<TaskBudget>,
    /// Whether the lead may recruit teammates (H1-5), and how many at most.
    #[serde(default = "yes")]
    pub spawn: bool,
    #[serde(default = "default_max_agents")]
    pub max_agents: u32,
    /// Models a recruit may run (exact ids); absent: any model a logged-in account can run.
    #[serde(default)]
    pub spawn_models: Option<Vec<String>>,
    /// Whether assignees may split their tasks and hand parts on (H1-5).
    #[serde(default)]
    pub delegate: bool,
    /// Whether workers ask before dangerous actions.
    #[serde(default)]
    pub approvals: ApprovalPolicy,
    /// Context tokens over which work sent back to its own worker (a failed check or review,
    /// `task_rework`, the person's send-back) goes to a new model session of that same agent,
    /// opened with a handover from the hub's records (docs/USAGE-TRIM.md §8). Absent:
    /// [`DEFAULT_REWORK_NEW_SESSION_TOKENS`]; `0`: never.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub rework_new_session_tokens: Option<u64>,
}

/// The default of [`RunPolicy::rework_new_session_tokens`]. A starting point read off four
/// tasks (docs/USAGE-TRIM.md §8), not a tuned value.
pub const DEFAULT_REWORK_NEW_SESSION_TOKENS: u64 = 150_000;

fn default_ask_timeout() -> f64 {
    30.0
}
fn yes() -> bool {
    true
}
fn default_max_agents() -> u32 {
    8
}

impl Default for RunPolicy {
    fn default() -> Self {
        Self {
            continuation: None,
            plan_approval: PlanApproval::default(),
            land: LandPolicy::default(),
            human_ask: AskPolicy::default(),
            ask_timeout_minutes: default_ask_timeout(),
            budget: Budget::default(),
            task_budget: None,
            spawn: yes(),
            max_agents: default_max_agents(),
            spawn_models: None,
            delegate: false,
            approvals: ApprovalPolicy::default(),
            rework_new_session_tokens: None,
        }
    }
}

impl RunPolicy {
    /// The context size over which rework opens a new session; `None` when it never does.
    pub fn rework_new_session_at(&self) -> Option<u64> {
        match self
            .rework_new_session_tokens
            .unwrap_or(DEFAULT_REWORK_NEW_SESSION_TOKENS)
        {
            0 => None,
            n => Some(n),
        }
    }

    /// No person needed: plans start at once, the lead lands, questions fall back to
    /// their defaults after 30 minutes.
    pub fn hands_off() -> Self {
        Self {
            plan_approval: PlanApproval::Auto,
            land: LandPolicy::Auto,
            human_ask: AskPolicy::Timeout,
            ..Self::default()
        }
    }
}

/// Optional, bounded supervisor wakeups. The cap is per run, not per task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(default)]
pub struct ContinuationPolicy {
    pub max_attempts: u32,
    pub interval_seconds: u32,
    /// Report a run that waits on a busy agent to the lead when the run's own durable
    /// facts show no progress for this many minutes. `None` (the default) keeps waiting
    /// in silence forever, exactly as before; the report never interrupts a turn, it only
    /// asks the lead to decide. Shares `max_attempts` with the other wakeups.
    #[serde(default)]
    pub stuck_minutes: Option<u32>,
}

impl Default for ContinuationPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            interval_seconds: 60,
            stuck_minutes: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_take_defaults() {
        let policy: RunPolicy = serde_json::from_str("{}").unwrap();
        assert_eq!(policy, RunPolicy::default());
        assert_eq!(policy.ask_timeout_minutes, 30.0);
        let auto: RunPolicy =
            serde_json::from_str(r#"{"plan_approval":"auto","land":"pr"}"#).unwrap();
        assert_eq!(auto.plan_approval, PlanApproval::Auto);
        assert_eq!(auto.land, LandPolicy::Pr);
        assert!(auto.continuation.is_none());
        let enabled: RunPolicy = serde_json::from_str(r#"{"continuation":{}}"#).unwrap();
        assert_eq!(enabled.continuation, Some(ContinuationPolicy::default()));
        assert_eq!(
            enabled.continuation.as_ref().unwrap().stuck_minutes,
            None,
            "no stuck timeout unless asked for"
        );
        assert_eq!(
            serde_json::from_value::<RunPolicy>(serde_json::to_value(&enabled).unwrap()).unwrap(),
            enabled
        );
        let stuck: RunPolicy =
            serde_json::from_str(r#"{"continuation":{"stuck_minutes":15}}"#).unwrap();
        assert_eq!(stuck.continuation.unwrap().stuck_minutes, Some(15));
    }

    #[test]
    fn a_tasks_own_caps_win_field_by_field_over_the_runs_default() {
        let policy: RunPolicy = serde_json::from_str(
            r#"{"task_budget":{"max_usd":2.5,"max_rework":3,"max_idle_minutes":20}}"#,
        )
        .unwrap();
        let run_default = policy.task_budget.clone().unwrap_or_default();
        assert!(run_default.is_set());
        assert_eq!(run_default.max_tokens, None, "absent means no cap");
        let own = TaskBudget {
            max_usd: Some(1.0),
            max_minutes: Some(45),
            ..TaskBudget::default()
        };
        let effective = own.with_default(&run_default);
        assert_eq!(effective.max_usd, Some(1.0));
        assert_eq!(effective.max_rework, Some(3));
        assert_eq!(effective.max_minutes, Some(45));
        assert_eq!(effective.max_idle_minutes, Some(20));
        assert!(!TaskBudget::default().is_set());
    }
}
