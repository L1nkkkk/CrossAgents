//! The three built-in agents, as descriptions: `agents/*.toml`, compiled into the hub
//! (docs/DESIGN.md §9.3). An agent added by hand uses the same format.

use std::sync::LazyLock;

use crossagents_drivers::locate::Cli;

use crate::{PluginConfig, manifest};

/// In the order `GET /v1/plugins` lists them.
static BUILTIN: LazyLock<Vec<(String, PluginConfig)>> = LazyLock::new(|| {
    [
        ("claude", include_str!("../agents/claude.toml")),
        ("codex", include_str!("../agents/codex.toml")),
        ("codebuddy", include_str!("../agents/codebuddy.toml")),
    ]
    .into_iter()
    .map(|(name, text)| {
        manifest::parse(text, name)
            .unwrap_or_else(|err| panic!("built-in agent description {name}.toml: {err}"))
    })
    .collect()
});

/// Every built-in description, in the order the API lists them.
pub fn all() -> &'static [(String, PluginConfig)] {
    &BUILTIN
}

pub fn config(name: &str) -> Option<&'static PluginConfig> {
    BUILTIN
        .iter()
        .find(|(built_in, _)| built_in == name)
        .map(|(_, config)| config)
}

/// The CLI a built-in description names, for callers outside the registry (account probes).
pub fn cli(name: &str) -> Option<Cli> {
    config(name).map(PluginConfig::cli)
}

/// Where a built-in description looks for its CLI, for callers that find the program
/// themselves (account probes).
pub fn locator(name: &str) -> Option<crossagents_drivers::locate::Locator> {
    config(name).map(|config| config.locate.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_descriptions_load() {
        let names: Vec<&str> = all().iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["claude", "codex", "codebuddy"]);
        for (name, config) in all() {
            assert!(config.driver.is_some(), "{name} needs a driver");
            assert!(
                config.install.hint.is_some(),
                "{name} should say how to install it"
            );
            assert!(
                !config.locate.is_empty(),
                "{name} should say where its CLI is"
            );
        }
    }
}
