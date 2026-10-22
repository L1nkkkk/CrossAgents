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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RunPolicy {
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
}

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
            plan_approval: PlanApproval::default(),
            land: LandPolicy::default(),
            human_ask: AskPolicy::default(),
            ask_timeout_minutes: default_ask_timeout(),
            budget: Budget::default(),
            spawn: yes(),
            max_agents: default_max_agents(),
            spawn_models: None,
            delegate: false,
        }
    }
}

impl RunPolicy {
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
    }
}
