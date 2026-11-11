//! Whether a session may run hooks (TODO §五, SELF B5).
//!
//! A hook is a command the CLI runs on its own: on a prompt, before or after a tool, when it
//! stops. Claude Code collects them from the settings files it reads, and nothing about them
//! is asked of the hub — they are not the tool calls that go through `can_use_tool`, and an
//! agent whose person never looked at the project's `.claude/settings.json` would run them
//! anyway.
//!
//! Where hooks come from, and what CrossAgents does with each file:
//!
//! - **user** — `~/.claude/settings.json`. Not read by default: sessions launch with
//!   `--setting-sources project,local` ([`crate::claude_native::wire::DEFAULT_SETTING_SOURCES`]),
//!   so the person's own setup (plugins, connectors, hooks) stays out of the agent. An agent
//!   whose settings were widened to `user,project,local` ("全部") does read it.
//! - **project** — `<cwd>/.claude/settings.json`, and **local** — the same directory's
//!   `settings.local.json`. Both are read by default, and both may carry `hooks`. This is
//!   the half that the default setting sources do not cover.
//!
//! With `allow_hooks` off (the default) the driver adds `--settings '{"disableAllHooks":true}'`:
//! Claude Code merges a `--settings` object over the files it read, and `disableAllHooks`
//! switches off every hook from every source. Only hooks are affected — the project's
//! `CLAUDE.md`, its permission rules and the rest of the settings still load — and it holds
//! whatever the setting sources are, so a widened "全部" does not quietly bring the user's
//! hooks back. With it on, no extra settings are passed and the hooks of whichever sources
//! the agent loads run as the CLI would run them.
//!
//! The switch lives on the agent (`config.allow_hooks`) and is read at launch, like the
//! setting sources: changing it restarts an idle session.
//!
//! Off is the default for new agents. Agents that existed before the switch are the
//! exception: the store's one-off migration writes `allow_hooks: true` for the claude ones
//! that never chose, so an upgrade does not silently stop their hooks
//! (`crates/store-sqlite/src/lib.rs`, `HOOKS_ON`).

use serde_json::Value;

/// The key on an agent's config: `config.allow_hooks`.
pub const KEY: &str = "allow_hooks";

/// Passed as `--settings` while hooks are off. Top-level settings merge, so naming only
/// `disableAllHooks` replaces nothing else the agent configured.
pub const DISABLED_SETTINGS: &str = r#"{"disableAllHooks":true}"#;

/// Whether this agent may run hooks. Absent, `null`, or anything that is not `true` means
/// off: the switch is turned on deliberately, never by a typo or an old config.
pub fn allowed(config: &Value) -> bool {
    config.get(KEY).and_then(Value::as_bool).unwrap_or(false)
}

/// The launch arguments that keep hooks off; empty when they are allowed.
pub fn args(allow: bool) -> Vec<String> {
    if allow {
        Vec::new()
    } else {
        vec!["--settings".to_owned(), DISABLED_SETTINGS.to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Absent config is off here. Agents from before the switch are turned on by the store's
    /// upgrade migration; one made since is off unless it turns them on itself.
    #[test]
    fn hooks_are_off_unless_the_agent_turns_them_on() {
        assert!(!allowed(&json!({})));
        assert!(!allowed(&json!({ "allow_hooks": null })));
        assert!(!allowed(&json!({ "allow_hooks": false })));
        // A string or a number is not a yes: only the checkbox's `true` counts.
        assert!(!allowed(&json!({ "allow_hooks": "true" })));
        assert!(!allowed(&json!({ "allow_hooks": 1 })));
        assert!(allowed(&json!({ "allow_hooks": true })));
        // Other config keys are not read as this one.
        assert!(!allowed(
            &json!({ "setting_sources": "user,project,local" })
        ));
    }

    #[test]
    fn off_state_disables_hooks_in_the_settings_it_passes() {
        assert_eq!(
            args(false),
            vec!["--settings".to_owned(), DISABLED_SETTINGS.to_owned()]
        );
        let settings: Value = serde_json::from_str(DISABLED_SETTINGS).unwrap();
        assert_eq!(settings, json!({ "disableAllHooks": true }));
    }

    #[test]
    fn on_state_passes_nothing() {
        assert!(args(true).is_empty());
    }
}
