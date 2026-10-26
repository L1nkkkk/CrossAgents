//! Profiles (still called plugins in the API and config) and the drivers that run them
//! (docs/DESIGN.md §9).
//!
//! A plugin entry names an agent and how to reach it. The built-in agents are description
//! files under `agents/`, compiled in ([`builtin`]): where their CLI is, how it is installed,
//! its models. An agent of your own uses the same format, dropped in as a directory with a
//! manifest ([`manifest`]) or added in `config.toml`; no code changes.
//!
//! Without a `driver` a plugin is an ACP agent (`acp-generic`), which is what every
//! configuration written before drivers existed means. Any ACP agent can be registered from
//! `config.toml` without code:
//!
//! ```toml
//! [plugins.gemini]
//! command = ["gemini", "--experimental-acp"]
//!
//! [plugins.codex]
//! driver = "acp-generic"    # use the codex-acp adapter instead of `codex app-server`
//! ```
//!
//! An agent can pick another driver than its plugin's default through its own `driver` field.

pub mod builtin;
pub mod claude_account;
pub mod codebuddy_account;
pub mod codex_account;
pub mod manifest;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crossagents_core::{CoreError, Driver, DriverKind};
use crossagents_drivers::acp::AcpDriver;
use crossagents_drivers::claude_native::ClaudeNativeDriver;
use crossagents_drivers::codebuddy_native::CodebuddyNativeDriver;
use crossagents_drivers::codex_native::CodexNativeDriver;
use crossagents_drivers::locate::{Cli, Install, Locator};
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
    /// `claude` for `claude-native`, `codebuddy` (or `node <script>`) for
    /// `codebuddy-native`.
    #[serde(default)]
    pub native_command: Vec<String>,
    /// Reasoning effort levels the CLI accepts (`effort` in an agent's config), lowest
    /// first; empty when it has none. Composers offer exactly these.
    #[serde(default)]
    pub efforts: Vec<String>,
    /// Where the CLI is, for the native drivers (docs/DESIGN.md §9.3).
    #[serde(default)]
    pub locate: Locator,
    /// What installing it takes; shown, never run.
    #[serde(default)]
    pub install: Install,
}

impl PluginConfig {
    pub fn default_driver(&self) -> DriverKind {
        self.driver.unwrap_or_default()
    }

    /// The CLI this entry launches, found once it is first asked for.
    pub fn cli(&self) -> Cli {
        Cli::new(
            self.native_command.clone(),
            self.locate.clone(),
            self.install.clone(),
        )
    }

    /// A built-in adjusted by its `config.toml` entry: launch commands, driver, models and
    /// effort levels.
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
        if !over.efforts.is_empty() {
            self.efforts = over.efforts.clone();
        }
        if !over.models.is_empty() {
            self.models = over.models.clone();
        }
        if !over.locate.is_empty() {
            self.locate.merge(&over.locate);
        }
        if !over.install.is_empty() {
            self.install.merge(&over.install);
        }
        self
    }
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
    /// The model list was written by hand (`models` in `config.toml` or a manifest), so it
    /// is offered as it is rather than replaced by what the accounts report.
    pinned_models: bool,
    drivers: Mutex<HashMap<DriverKind, Arc<dyn Driver>>>,
}

impl Profile {
    fn new(name: &str, config: PluginConfig, pinned_models: bool) -> Self {
        Self {
            name: name.to_owned(),
            config,
            pinned_models,
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
                self.config.cli(),
                self.config.models.clone(),
            )),
            DriverKind::ClaudeNative => Arc::new(ClaudeNativeDriver::new(
                &self.name,
                self.config.cli(),
                self.config.models.clone(),
            )),
            DriverKind::CodebuddyNative => Arc::new(CodebuddyNativeDriver::new(
                &self.name,
                self.config.cli(),
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
        for (name, built_in) in builtin::all() {
            let config = match configs.get(name) {
                Some(over) => built_in.clone().overlay(over),
                None => built_in.clone(),
            };
            let pinned = configs.get(name).is_some_and(|c| !c.models.is_empty());
            registry.insert(name, config, pinned);
        }
        for (name, config) in configs {
            if !registry.profiles.contains_key(name) {
                registry.insert(name, config.clone(), !config.models.is_empty());
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
                let pinned = !config.models.is_empty();
                registry.insert(&name, config, pinned);
            }
        }
        (registry, warnings)
    }

    fn insert(&mut self, name: &str, config: PluginConfig, pinned_models: bool) {
        self.profiles.insert(
            name.to_owned(),
            Arc::new(Profile::new(name, config, pinned_models)),
        );
    }

    pub fn contains(&self, name: &str) -> bool {
        self.profiles.contains_key(name)
    }

    pub fn config(&self, name: &str) -> Option<&PluginConfig> {
        self.profiles.get(name).map(|p| &p.config)
    }

    /// Whether the plugin's models were listed by hand (`models` in its `config.toml` entry
    /// or its manifest): then they are what is offered, not what its accounts report.
    pub fn models_pinned(&self, name: &str) -> bool {
        self.profiles.get(name).is_some_and(|p| p.pinned_models)
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

    /// The fixed point of the migration (2026-10-26): field by field, what
    /// `PluginConfig::claude()/codex()/codebuddy()` returned before they went away. A change
    /// here changes behaviour, not formatting.
    #[test]
    fn the_built_in_descriptions_say_what_the_functions_said() {
        assert_eq!(builtin::all().len(), 3);

        let claude = builtin::config("claude").unwrap();
        assert_eq!(
            claude.command,
            ["npx", "-y", "@agentclientprotocol/claude-agent-acp"]
        );
        assert_eq!(claude.models, ["opus", "sonnet", "haiku"]);
        assert_eq!(claude.efforts, ["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(claude.model_env.as_deref(), Some("ANTHROPIC_MODEL"));
        assert_eq!(claude.driver, Some(DriverKind::ClaudeNative));
        assert!(claude.native_command.is_empty());

        let codex = builtin::config("codex").unwrap();
        assert_eq!(
            codex.command,
            ["npx", "-y", "@agentclientprotocol/codex-acp"]
        );
        assert!(codex.models.is_empty());
        assert_eq!(codex.efforts, ["low", "medium", "high"]);
        assert_eq!(codex.model_env, None);
        assert_eq!(codex.driver, Some(DriverKind::CodexNative));
        assert!(codex.native_command.is_empty());

        let codebuddy = builtin::config("codebuddy").unwrap();
        assert!(codebuddy.command.is_empty());
        assert_eq!(
            codebuddy.models,
            [
                "default-model",
                "hy4-preview",
                "hy3",
                "deepseek-v4.1-flash",
                "glm-5.3",
                "kimi-k3-1"
            ]
        );
        assert_eq!(
            codebuddy.efforts,
            ["minimal", "low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(codebuddy.driver, Some(DriverKind::CodebuddyNative));
        assert!(codebuddy.native_command.is_empty());
        // The copy inside WorkBuddy is never looked for: nowhere to point at.
        assert!(codebuddy.locate.paths.is_empty());
        assert!(codebuddy.locate.bundled.is_empty());
    }

    /// What a session gets from a CodeBuddy that is not installed: the description's install
    /// hint, how to sign in, and why not WorkBuddy's copy -- the sentence the driver used to
    /// hard-code, word for word.
    #[test]
    fn codebuddy_says_how_to_install_itself() {
        let cli = builtin::cli("codebuddy").unwrap();
        assert_eq!(
            cli.missing_message("codebuddy").unwrap(),
            "the CodeBuddy CLI is not installed: run `npm install -g @tencent-ai/codebuddy-code`, \
             then sign in once with `codebuddy` and `/login` (or set CODEBUDDY_API_KEY). \
             The copy inside WorkBuddy cannot run outside the WorkBuddy app."
        );
    }

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
        assert_eq!(registry.names(), ["claude", "codebuddy", "codex", "mock"]);
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

    #[test]
    fn built_ins_declare_their_effort_levels_and_config_can_replace_them() {
        let registry = PluginRegistry::with_config(&BTreeMap::new());
        let efforts = |name: &str| registry.config(name).unwrap().efforts.clone();
        assert_eq!(efforts("claude"), ["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(efforts("codex"), ["low", "medium", "high"]);
        assert_eq!(
            efforts("codebuddy"),
            ["minimal", "low", "medium", "high", "xhigh", "max"]
        );
        let over: PluginConfig = toml::from_str(r#"efforts = ["minimal", "low", "high"]"#).unwrap();
        let registry = PluginRegistry::with_config(&BTreeMap::from([("codex".to_owned(), over)]));
        assert_eq!(
            registry.config("codex").unwrap().efforts,
            ["minimal", "low", "high"]
        );
    }

    #[test]
    fn config_can_replace_a_built_in_model_list() {
        let plain = PluginRegistry::with_config(&BTreeMap::new());
        assert!(!plain.models_pinned("codebuddy"));
        let over: PluginConfig = toml::from_str(r#"models = ["hy3", "glm-5.3"]"#).unwrap();
        let registry =
            PluginRegistry::with_config(&BTreeMap::from([("codebuddy".to_owned(), over)]));
        assert_eq!(
            registry.get("codebuddy").unwrap().models(),
            ["hy3", "glm-5.3"]
        );
        assert!(registry.models_pinned("codebuddy"));
        assert!(!registry.models_pinned("codex"));
    }

    #[test]
    fn codebuddy_is_built_in_and_runs_on_its_native_driver() {
        let registry = PluginRegistry::with_config(&BTreeMap::new());
        let driver = registry.get("codebuddy").unwrap();
        assert_eq!(driver.kind(), DriverKind::CodebuddyNative);
        // The same protocol as Claude Code: sessions stream, resume, fork and ask.
        let capabilities = driver.capabilities();
        assert!(capabilities.streaming && capabilities.resume && capabilities.fork);
        assert!(capabilities.permission_callback && capabilities.mcp);
        assert!(!driver.models().is_empty());
        // Without a native command it finds the CLI itself.
        assert!(
            registry
                .config("codebuddy")
                .unwrap()
                .native_command
                .is_empty()
        );
    }
}
