//! Built-in agent plugins and the plugin registry (docs/DESIGN.md §6).
//!
//! Both built-in plugins reach their agent over ACP, so a plugin is little more than a
//! launch command and a model mapping. Any other ACP agent can be registered from
//! `config.toml` without code:
//!
//! ```toml
//! [plugins.gemini]
//! command = ["gemini", "--experimental-acp"]
//! ```
//!
//! or dropped in as a directory with a manifest, see [`manifest`].

pub mod codex_account;
pub mod manifest;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use crossagents_core::{AgentPlugin, AgentSession, Capabilities, CoreError, SessionSpec};
use crossagents_drivers::acp::{AcpLaunch, AcpOptions, AcpSession};
use serde::Deserialize;

/// A plugin entry in `config.toml`. For a built-in plugin it overrides the launch command;
/// otherwise it defines a new ACP plugin.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct PluginConfig {
    pub command: Vec<String>,
    #[serde(default)]
    pub models: Vec<String>,
    /// Environment variable that receives the agent's model, e.g. `ANTHROPIC_MODEL`.
    #[serde(default)]
    pub model_env: Option<String>,
}

/// An agent reached over ACP.
pub struct AcpPlugin {
    name: String,
    command: Vec<String>,
    models: Vec<String>,
    model_env: Option<String>,
}

impl AcpPlugin {
    pub fn new(name: impl Into<String>, config: PluginConfig) -> Self {
        Self {
            name: name.into(),
            command: config.command,
            models: config.models,
            model_env: config.model_env,
        }
    }

    /// Claude Code through Anthropic's Agent SDK ACP adapter (needs Node.js).
    pub fn claude() -> Self {
        Self::new(
            "claude",
            PluginConfig {
                command: npx("@agentclientprotocol/claude-agent-acp"),
                models: vec!["opus".into(), "sonnet".into(), "haiku".into()],
                // Claude Code reads its model from here; the ACP model option is the other half.
                model_env: Some("ANTHROPIC_MODEL".into()),
            },
        )
    }

    /// OpenAI Codex through the codex-acp adapter.
    pub fn codex() -> Self {
        Self::new(
            "codex",
            PluginConfig {
                command: npx("@agentclientprotocol/codex-acp"),
                models: Vec::new(),
                model_env: None,
            },
        )
    }

    fn with_command(mut self, command: Vec<String>) -> Self {
        if !command.is_empty() {
            self.command = command;
        }
        self
    }
}

fn npx(package: &str) -> Vec<String> {
    vec!["npx".into(), "-y".into(), package.into()]
}

#[async_trait]
impl AgentPlugin for AcpPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: true,
            interrupt: true,
            model_switch: true,
            mcp: true,
            usage_report: true,
        }
    }

    fn models(&self) -> Vec<String> {
        self.models.clone()
    }

    async fn start(&self, spec: SessionSpec) -> Result<Box<dyn AgentSession>, CoreError> {
        let command = spec.command.clone().unwrap_or_else(|| self.command.clone());
        let mut launch = AcpLaunch::from_command(&command, spec.agent.cwd.clone())?;
        if let (Some(var), Some(model)) = (&self.model_env, &spec.agent.model) {
            launch.env.push((var.clone(), model.clone()));
        }
        launch.env.extend(spec.env);
        let session = AcpSession::start(AcpOptions {
            launch,
            mcp_servers: spec.mcp_servers,
            permission: spec.permission,
            model: spec.agent.model,
            instructions: spec.instructions,
            events: spec.events,
        })
        .await?;
        Ok(Box::new(session))
    }
}

/// Plugins by name.
#[derive(Clone, Default)]
pub struct PluginRegistry {
    plugins: BTreeMap<String, Arc<dyn AgentPlugin>>,
}

impl PluginRegistry {
    /// The built-in plugins, adjusted and extended by `config.toml` entries.
    pub fn with_config(configs: &BTreeMap<String, PluginConfig>) -> Self {
        let mut registry = Self::default();
        let builtins = [AcpPlugin::claude(), AcpPlugin::codex()];
        for plugin in builtins {
            let plugin = match configs.get(plugin.name()) {
                Some(config) => plugin.with_command(config.command.clone()),
                None => plugin,
            };
            registry.register(Arc::new(plugin));
        }
        for (name, config) in configs {
            if !registry.plugins.contains_key(name) {
                registry.register(Arc::new(AcpPlugin::new(name, config.clone())));
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
        let mut merged: BTreeMap<String, PluginConfig> = found.into_iter().collect();
        merged.extend(configs.iter().map(|(k, v)| (k.clone(), v.clone())));
        let mut registry = Self::with_config(configs);
        for (name, config) in merged {
            // Directory plugins replace built-ins unless config.toml says otherwise.
            if !configs.contains_key(&name) {
                registry.register(Arc::new(AcpPlugin::new(name, config)));
            }
        }
        (registry, warnings)
    }

    pub fn register(&mut self, plugin: Arc<dyn AgentPlugin>) {
        self.plugins.insert(plugin.name().to_owned(), plugin);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn AgentPlugin>> {
        self.plugins.get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        self.plugins.keys().cloned().collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn AgentPlugin>> {
        self.plugins.values()
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
        assert!(registry.get("nope").is_none());
    }
}
