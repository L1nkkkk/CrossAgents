//! The CLI version gate (docs/research/multica.md §2.9).
//!
//! A description file names the oldest CLI it runs on (`min_version`). `POST /v1/agents`
//! refuses to register an agent for a CLI below that floor (api.rs `create_agent`); this is
//! the same gate for an agent that already exists, whose session is about to open: it was
//! made while the CLI was new enough, or while the description had no floor yet, and the
//! person has since moved to an older CLI.
//!
//! Unknown is never too old: a CLI that has not answered its version yet (the probe runs in
//! the background, docs/DESIGN.md §9.3) and an agent whose driver reports no version at all
//! start as before.

use crossagents_core::{Agent, CoreError};
use crossagents_plugins::{PluginConfig, PluginRegistry};

/// The error that stops `agent`'s session: its CLI is older than the description's floor.
/// `Ok(())` when the session may open.
pub async fn check(agent: &Agent, plugins: &PluginRegistry) -> Result<(), CoreError> {
    // An agent that runs outside the hub launches no CLI here.
    if agent.plugin == crossagents_core::EXTERNAL_PLUGIN {
        return Ok(());
    }
    let Some(config) = plugins.config(&agent.plugin) else {
        return Ok(());
    };
    // The driver this session would launch: the description's default unless the agent names
    // another, whose own `cli_status` says what that CLI is (an ACP agent reports none).
    let Ok(resolved) = plugins.driver(&agent.plugin, agent.driver.as_deref()) else {
        return Ok(());
    };
    let version = match resolved.driver.cli_status().await {
        Some(status) => status.located.and_then(|located| located.version),
        None => None,
    };
    match refusal(&agent.plugin, config, version.as_deref()) {
        Some(why) => Err(CoreError::Agent(why)),
        None => Ok(()),
    }
}

/// What to say when the CLI that reported `version` is below the description's floor;
/// `None` when it may run.
pub fn refusal(plugin: &str, config: &PluginConfig, version: Option<&str>) -> Option<String> {
    config
        .too_old(version)
        .then(|| message(plugin, version, config.min_version.as_deref()))
}

/// The sentence both gates say: which CLI, what it is, what it needs, where to upgrade it.
fn message(plugin: &str, version: Option<&str>, minimum: Option<&str>) -> String {
    format!(
        "the {plugin} CLI on this machine is too old (version {}, needs {} or newer): \
         upgrade it on the Agents page",
        version.unwrap_or("unknown"),
        minimum.unwrap_or(""),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossagents_plugins::{PluginConfig, builtin};

    #[test]
    fn an_old_cli_is_refused_with_the_sentence_create_agent_uses() {
        let claude = builtin::config("claude").unwrap();
        assert_eq!(
            refusal("claude", claude, Some("1.9.0")),
            Some(
                "the claude CLI on this machine is too old (version 1.9.0, needs 2.0.0 or \
                 newer): upgrade it on the Agents page"
                    .to_owned()
            )
        );
        // New enough, and a version in the series the description names: started.
        assert_eq!(refusal("claude", claude, Some("2.1.317 (Claude Code)")), None);
        assert_eq!(refusal("codex", builtin::config("codex").unwrap(), Some("0.166.0")), None);
        // The floor may be raised after the agent was made: an old one is then refused too.
        let raised: PluginConfig = toml::from_str("min_version = \"3.0.0\"").unwrap();
        assert!(refusal("claude", &raised, Some("2.1.317 (Claude Code)")).is_some());
    }

    #[test]
    fn a_cli_that_has_not_answered_its_version_is_started() {
        // The probe had not answered yet (or the file answered nothing): unknown, not old.
        assert_eq!(refusal("claude", builtin::config("claude").unwrap(), None), None);
        // A description with no floor refuses nothing, whatever the CLI reports.
        let codebuddy = builtin::config("codebuddy").unwrap();
        assert_eq!(refusal("codebuddy", codebuddy, Some("1.0.0")), None);
    }
}
