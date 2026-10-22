//! Profiles (still called plugins in the API and config) and the drivers that run them
//! (docs/DESIGN.md §9).
//!
//! A plugin entry names an agent and how to reach it. Without a `driver` it is an ACP
//! agent (`acp-generic`), which is what every configuration written before drivers existed
//! means. Any ACP agent can be registered from `config.toml` without code:
//!
//! ```toml
//! [plugins.gemini]
//! command = ["gemini", "--experimental-acp"]
//!
//! [plugins.codex]
//! driver = "acp-generic"    # use the codex-acp adapter instead of `codex app-server`
//! ```
//!
//! or dropped in as a directory with a manifest, see [`manifest`]. An agent can pick
//! another driver than its plugin's default through its own `driver` field.

pub mod claude_account;
pub mod codex_account;
pub mod manifest;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crossagents_core::{CoreError, Driver, DriverKind};
use crossagents_drivers::acp::AcpDriver;
use crossagents_drivers::claude_native::ClaudeNativeDriver;
use crossagents_drivers::codex_native::CodexNativeDriver;
use serde::Deserialize;

/// A plugin entry in `config.toml`. For a built-in plugin it overrides the launch commands
/// and the default driver; otherwise it defines a new plugin.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct PluginConfig {
    /// ACP launch command (`acp-generic`).
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub models: Vec<String>,
    /// Environment variable that receives the agent's model, e.g. `ANTHROPIC_MODEL`.
    #[serde(default)]
    pub model_env: Option<String>,
    /// Default driver for this plugin's agents; `acp-generic` when absent.
    #[serde(default)]
    pub driver: Option<DriverKind>,
    /// Launch command for the native driver: `codex app-server` for `codex-native`,
    /// `claude` for `claude-native`.
    #[serde(default)]
    pub native_command: Vec<String>,
}

impl PluginConfig {
    pub fn default_driver(&self) -> DriverKind {
        self.driver.unwrap_or_default()
    }

    /// Claude Code: its stream-json protocol by default; Anthropic's ACP adapter (needs
    /// Node.js) with `driver = "acp-generic"`.
    pub fn claude() -> Self {
        Self {
            command: npx("@agentclientprotocol/claude-agent-acp"),
            driver: Some(DriverKind::ClaudeNative),
            models: vec!["opus".into(), "sonnet".into(), "haiku".into()],
            // Claude Code reads its model from here; the ACP model option is the other half.
            model_env: Some("ANTHROPIC_MODEL".into()),
            ..Self::default()
        }
    }

    /// OpenAI Codex: `codex app-server` by default; the codex-acp adapter with
    /// `driver = "acp-generic"`.
    pub fn codex() -> Self {
        Self {
            command: npx("@agentclientprotocol/codex-acp"),
            driver: Some(DriverKind::CodexNative),
            ..Self::default()
        }
    }

    /// A built-in adjusted by its `config.toml` entry: launch commands and driver.
    fn overlay(mut self, over: &PluginConfig) -> Self {
        if !over.command.is_empty() {
            self.command = over.command.clone();
            // An ACP command alone means the entry is about ACP, as before drivers existed.
            if over.native_command.is_empty() {
                self.driver = None;
            }
        }
        if over.driver.is_some() {
            self.driver = over.driver;
        }
        if !over.native_command.is_empty() {
            self.native_command = over.native_command.clone();
        }
        self
    }
}

fn npx(package: &str) -> Vec<String> {
    vec!["npx".into(), "-y".into(), package.into()]
}

/// A driver for an agent, with a note when it is not the one asked for.
pub struct Resolved {
    pub driver: Arc<dyn Driver>,
    pub warning: Option<String>,
}

/// One plugin and the drivers built for it so far. Drivers are kept: `codex-native`
/// shares one app-server process between the plugin's sessions.
struct Profile {
    name: String,
    config: PluginConfig,
    drivers: Mutex<HashMap<DriverKind, Arc<dyn Driver>>>,
}

impl Profile {
    fn new(name: &str, config: PluginConfig) -> Self {
        Self {
            name: name.to_owned(),
            config,
            drivers: Mutex::default(),
        }
    }

    fn resolve(&self, kind: DriverKind) -> Result<Resolved, CoreError> {
        let warning = None;
        let mut drivers = self.drivers.lock().expect("driver cache poisoned");
        if let Some(driver) = drivers.get(&kind) {
            return Ok(Resolved {
                driver: driver.clone(),
                warning,
            });
        }
        let driver: Arc<dyn Driver> = match kind {
            DriverKind::AcpGeneric => {
                if self.config.command.is_empty() {
                    return Err(CoreError::Unsupported(format!(
                        "plugin {:?} has no ACP command for the acp-generic driver",
                        self.name
                    )));
                }
                Arc::new(AcpDriver::new(
                    &self.name,
                    self.config.command.clone(),
                    self.config.models.clone(),
                    self.config.model_env.clone(),
                ))
            }
            DriverKind::CodexNative => Arc::new(CodexNativeDriver::new(
                &self.name,
                self.config.native_command.clone(),
                self.config.models.clone(),
            )),
            DriverKind::ClaudeNative => Arc::new(ClaudeNativeDriver::new(
                &self.name,
                self.config.native_command.clone(),
                self.config.models.clone(),
            )),
        };
        drivers.insert(kind, driver.clone());
        Ok(Resolved { driver, warning })
    }
}

/// Plugins by name. Cheap to clone; clones share drivers.
#[derive(Clone, Default)]
pub struct PluginRegistry {
    profiles: BTreeMap<String, Arc<Profile>>,
}

impl PluginRegistry {
    /// The built-in plugins, adjusted and extended by `config.toml` entries.
    pub fn with_config(configs: &BTreeMap<String, PluginConfig>) -> Self {
        let mut registry = Self::default();
        for (name, builtin) in [
            ("claude", PluginConfig::claude()),
            ("codex", PluginConfig::codex()),
        ] {
            let config = match configs.get(name) {
                Some(over) => builtin.overlay(over),
                None => builtin,
            };
            registry.insert(name, config);
        }
        for (name, config) in configs {
            if !registry.profiles.contains_key(name) {
                registry.insert(name, config.clone());
            }
        }
        registry
    }

    /// Built-ins, then plugin directories under `plugins_dir`, then `config.toml` entries;
    /// later sources win. Returns warnings for plugin manifests that were skipped.
    pub fn load(
        configs: &BTreeMap<String, PluginConfig>,
        plugins_dir: &Path,
    ) -> (Self, Vec<String>) {
        let (found, warnings) = manifest::load_dir(plugins_dir);
        let mut registry = Self::with_config(configs);
        for (name, config) in found {
            // Directory plugins replace built-ins unless config.toml says otherwise.
            if !configs.contains_key(&name) {
                registry.insert(&name, config);
            }
        }
        (registry, warnings)
    }

    fn insert(&mut self, name: &str, config: PluginConfig) {
        self.profiles
            .insert(name.to_owned(), Arc::new(Profile::new(name, config)));
    }

    pub fn contains(&self, name: &str) -> bool {
        self.profiles.contains_key(name)
    }

    pub fn config(&self, name: &str) -> Option<&PluginConfig> {
        self.profiles.get(name).map(|p| &p.config)
    }

    /// The driver for an agent of plugin `name`: `driver` if the agent names one, else the
    /// plugin's default.
    pub fn driver(&self, name: &str, driver: Option<&str>) -> Result<Resolved, CoreError> {
        let profile = self.profiles.get(name).ok_or_else(|| CoreError::NotFound {
            kind: "plugin",
            id: name.to_owned(),
        })?;
        let kind = match driver {
            Some(driver) => driver.parse()?,
            None => profile.config.default_driver(),
        };
        profile.resolve(kind)
    }

    /// The plugin's default driver.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Driver>> {
        self.driver(name, None).ok().map(|r| r.driver)
    }

    pub fn names(&self) -> Vec<String> {
        self.profiles.keys().cloned().collect()
    }

    /// Every plugin's default driver, in name order (plugins whose driver cannot be built
    /// are skipped).
    pub fn defaults(&self) -> Vec<Arc<dyn Driver>> {
        self.profiles
            .values()
            .filter_map(|p| p.resolve(p.config.default_driver()).ok())
            .map(|r| r.driver)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_overrides_builtins_and_adds_plugins() {
        let mut configs = BTreeMap::new();
        configs.insert(
            "claude".to_owned(),
            PluginConfig {
                command: vec!["my-claude".into()],
                ..Default::default()
            },
        );
        configs.insert(
            "mock".to_owned(),
            PluginConfig {
                command: vec!["mock-agent".into()],
                ..Default::default()
            },
        );
        let registry = PluginRegistry::with_config(&configs);
        assert_eq!(registry.names(), ["claude", "codex", "mock"]);
        assert_eq!(
            registry.get("claude").unwrap().models(),
            ["opus", "sonnet", "haiku"]
        );
        assert_eq!(registry.config("claude").unwrap().command, ["my-claude"]);
        assert!(registry.get("nope").is_none());
    }

    #[test]
    fn drivers_default_to_acp_and_can_be_chosen() {
        let config: PluginConfig = toml::from_str("command = [\"x\"]").unwrap();
        assert_eq!(config.driver, None);
        assert_eq!(config.default_driver(), DriverKind::AcpGeneric);

        let mut configs = BTreeMap::new();
        configs.insert(
            "codex".to_owned(),
            toml::from_str::<PluginConfig>("driver = \"codex-native\"").unwrap(),
        );
        let registry = PluginRegistry::with_config(&configs);
        // The built-in keeps its ACP command, so agents can still pick acp-generic.
        let codex = registry.driver("codex", None).unwrap();
        assert_eq!(codex.driver.kind(), DriverKind::CodexNative);
        assert!(codex.driver.capabilities().resume);
        let acp = registry.driver("codex", Some("acp-generic")).unwrap();
        assert_eq!(acp.driver.kind(), DriverKind::AcpGeneric);
        // The same driver instance is handed out each time (it owns the app-server pool).
        let again = registry.driver("codex", Some("codex-native")).unwrap();
        assert!(Arc::ptr_eq(&codex.driver, &again.driver));

        // Without config the built-in codex is native; config can move it back to ACP.
        let plain = PluginRegistry::with_config(&BTreeMap::new());
        assert_eq!(plain.get("codex").unwrap().kind(), DriverKind::CodexNative);
        configs.insert(
            "codex".to_owned(),
            toml::from_str::<PluginConfig>("driver = \"acp-generic\"").unwrap(),
        );
        let acp = PluginRegistry::with_config(&configs);
        assert_eq!(acp.get("codex").unwrap().kind(), DriverKind::AcpGeneric);
        // So does an entry that only replaces the ACP command.
        configs.insert(
            "codex".to_owned(),
            toml::from_str::<PluginConfig>("command = [\"my-codex-acp\"]").unwrap(),
        );
        let custom = PluginRegistry::with_config(&configs);
        assert_eq!(custom.get("codex").unwrap().kind(), DriverKind::AcpGeneric);
    }

    #[test]
    fn claude_defaults_to_its_native_driver_and_unknown_drivers_fail() {
        let registry = PluginRegistry::with_config(&BTreeMap::new());
        assert_eq!(
            registry.get("claude").unwrap().kind(),
            DriverKind::ClaudeNative
        );
        let acp = registry.driver("claude", Some("acp-generic")).unwrap();
        assert_eq!(acp.driver.kind(), DriverKind::AcpGeneric);
        assert!(acp.warning.is_none());
        assert!(matches!(
            registry.driver("claude", Some("nope")),
            Err(CoreError::Unsupported(_))
        ));
        assert!(toml::from_str::<PluginConfig>("driver = \"nope\"").is_err());
    }
}
