//! Accounts behind agents: login, plan, quota and token usage (docs/tasks/M5.md).
//!
//! An account is one login with a vendor (e.g. a ChatGPT account for Codex). Agents bind
//! to an account; the account's provider knows how to inspect it. Credentials never leave
//! the provider: nothing here carries a token.

use std::collections::BTreeMap;
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::CoreError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Account {
    /// Unique, same rules as agent names (e.g. `codex-main`).
    pub id: String,
    /// The provider that manages it, e.g. `codex`.
    pub provider: String,
    pub label: String,
    /// The provider's home for this account (for Codex: its `CODEX_HOME`).
    pub home: PathBuf,
    pub created_at: DateTime<Utc>,
}

/// Where a status figure came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum DataSource {
    /// Asked the vendor just now.
    Live,
    /// The last value recorded in local session logs.
    Snapshot,
}

/// One rate-limit window, e.g. "5% of the 30-day allowance used".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RateWindow {
    pub name: String,
    pub used_percent: f64,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub window_seconds: Option<u64>,
    pub resets_at: Option<DateTime<Utc>>,
}

/// One of an account's separate limits, e.g. the general Codex limit and one for a
/// particular model, each with its own windows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct LimitGroup {
    /// The vendor's id, e.g. `codex`.
    pub id: String,
    /// The vendor's name for it, when it has one.
    pub name: Option<String>,
    /// The model it applies to, when it is model-specific.
    pub model: Option<String>,
    pub windows: Vec<RateWindow>,
    /// The vendor says this limit is used up.
    pub reached: bool,
}

/// A credit that resets the limits once (Codex grants these, e.g. monthly).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ResetCredit {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    /// `available`, or the vendor's word for used / expired.
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Credits {
    pub has_credits: bool,
    pub unlimited: bool,
    /// As reported by the vendor (a decimal string).
    pub balance: Option<String>,
}

/// Everything known about an account right now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AccountStatus {
    pub logged_in: bool,
    /// e.g. `chatgpt` or `api_key`.
    pub auth_mode: Option<String>,
    /// Masked, e.g. `z***@gmail.com`.
    pub email: Option<String>,
    /// e.g. `free`, `plus`, `pro`.
    pub plan: Option<String>,
    pub subscription_until: Option<DateTime<Utc>>,
    pub rate_limits: Vec<RateWindow>,
    pub limit_reached: bool,
    pub credits: Option<Credits>,
    /// Every separate limit with its windows (when the vendor has several, e.g. one per
    /// model); `rate_limits` is the main one.
    #[serde(default)]
    pub limits: Vec<LimitGroup>,
    /// Credits that reset the limits once.
    #[serde(default)]
    pub reset_credits: Vec<ResetCredit>,
    /// Where the quota figures came from, and when.
    pub source: Option<DataSource>,
    pub as_of: Option<DateTime<Utc>>,
    /// Why live data is missing or degraded, if it is.
    pub note: Option<String>,
}

/// Token counts of one group of sessions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TokenTotals {
    pub sessions: u32,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub input_tokens: u64,
    /// Part of `input_tokens` served from the prompt cache.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub cached_input_tokens: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub output_tokens: u64,
    /// Part of `output_tokens` spent on reasoning.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub reasoning_output_tokens: u64,
}

impl TokenTotals {
    pub fn add(&mut self, other: &TokenTotals) {
        self.sessions += other.sessions;
        self.input_tokens += other.input_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_output_tokens += other.reasoning_output_tokens;
    }

    /// Share of input served from cache, 0..=1.
    pub fn cache_hit_rate(&self) -> f64 {
        if self.input_tokens == 0 {
            0.0
        } else {
            self.cached_input_tokens as f64 / self.input_tokens as f64
        }
    }
}

/// Token use of one vendor session, as recorded by the vendor's own logs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionUsage {
    /// The vendor's session id (for ACP agents, the ACP session id).
    pub session_id: String,
    pub started_at: DateTime<Utc>,
    pub model: Option<String>,
    /// Who started it, e.g. `crossagents` or the vendor's own CLI.
    pub originator: Option<String>,
    pub totals: TokenTotals,
}

/// A model the account may use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub context_window: Option<u64>,
}

/// How a login is carried out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginCommand {
    pub command: Vec<String>,
    pub env: BTreeMap<String, String>,
}

/// One vendor's accounts (docs/tasks/M5.md §3).
#[async_trait]
pub trait AccountProvider: Send + Sync {
    /// The provider name, matching the plugin it serves (e.g. `codex`).
    fn name(&self) -> &str;
    /// Where a new account's files go if none is given.
    fn default_home(&self) -> Option<PathBuf>;
    /// Login state, plan, quota and credits. Never fails: problems go into `note`.
    async fn status(&self, account: &Account) -> AccountStatus;
    /// Per-session token use from the vendor's local logs, newest first.
    async fn sessions(&self, account: &Account) -> Result<Vec<SessionUsage>, CoreError>;
    /// Models this account may use, best first. Empty if unknown.
    async fn models(&self, account: &Account) -> Vec<ModelInfo>;
    /// The command that logs this account in (interactive, usually opens a browser).
    fn login_command(&self, account: &Account) -> LoginCommand;
    /// Environment for an agent process that uses this account.
    fn agent_env(&self, account: &Account) -> Vec<(String, String)>;
}

/// `zhang@gmail.com` -> `z***@gmail.com`.
pub fn mask_email(email: &str) -> String {
    match email.split_once('@') {
        Some((user, domain)) => {
            let first: String = user.chars().take(1).collect();
            format!("{first}***@{domain}")
        }
        None => "***".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_email() {
        assert_eq!(mask_email("zhang@gmail.com"), "z***@gmail.com");
        assert_eq!(mask_email("nope"), "***");
    }

    #[test]
    fn totals_add_and_hit_rate() {
        let mut a = TokenTotals {
            sessions: 1,
            input_tokens: 100,
            cached_input_tokens: 40,
            ..Default::default()
        };
        a.add(&TokenTotals {
            sessions: 1,
            input_tokens: 100,
            cached_input_tokens: 60,
            ..Default::default()
        });
        assert_eq!(a.sessions, 2);
        assert!((a.cache_hit_rate() - 0.5).abs() < 1e-9);
        assert_eq!(TokenTotals::default().cache_hit_rate(), 0.0);
    }
}
