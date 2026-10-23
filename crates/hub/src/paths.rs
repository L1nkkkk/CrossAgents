//! Runtime files under the CrossAgents home. See docs/tasks/M0.md §3.2.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use crossagents_plugins::PluginConfig;
use serde::{Deserialize, Serialize};

pub const HOME_ENV: &str = "CROSSAGENTS_HOME";
pub const DEFAULT_PORT: u16 = 7420;

#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    /// `$CROSSAGENTS_HOME`, else `~/.crossagents`.
    pub fn from_env() -> Result<Self> {
        if let Some(home) = std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
            return Ok(Self::new(home));
        }
        let user_home = dirs::home_dir().context("cannot determine the user's home directory")?;
        Ok(Self::new(user_home.join(".crossagents")))
    }

    pub fn config(&self) -> PathBuf {
        self.home.join("config.toml")
    }
    pub fn db(&self) -> PathBuf {
        self.home.join("state.db")
    }
    pub fn token(&self) -> PathBuf {
        self.home.join("token")
    }
    pub fn hub_file(&self) -> PathBuf {
        self.home.join("hub.json")
    }
    pub fn lock(&self) -> PathBuf {
        self.home.join("hub.lock")
    }
    pub fn logs(&self) -> PathBuf {
        self.home.join("logs")
    }
    /// Models picked on the settings page, per plugin.
    pub fn models(&self) -> PathBuf {
        self.home.join("models.json")
    }
    /// Homes of accounts created by CrossAgents.
    pub fn accounts(&self) -> PathBuf {
        self.home.join("accounts")
    }
    /// External plugins: one directory with a `plugin.toml` each.
    pub fn plugins(&self) -> PathBuf {
        self.home.join("plugins")
    }
    /// Records of runtime hosts: agent processes that outlive the hub.
    pub fn runtimes(&self) -> PathBuf {
        self.home.join("runtimes")
    }
    /// Agents' tokens for the hub's tools, kept so agents taken over after a restart still
    /// reach them.
    pub fn agent_tokens(&self) -> PathBuf {
        self.home.join("agent-tokens.json")
    }
    /// Where the hub notes how far the event log was when it started.
    pub fn hub_state(&self) -> PathBuf {
        self.home.join("hub-state.json")
    }

    pub fn read_token(&self) -> Result<String> {
        let token = std::fs::read_to_string(self.token())
            .with_context(|| format!("cannot read {}", self.token().display()))?;
        Ok(token.trim().to_owned())
    }

    /// Returns the running hub's descriptor, if `hub.json` exists.
    pub fn read_hub_file(&self) -> Result<Option<HubFile>> {
        read_json_if_exists(&self.hub_file())
    }
}

fn read_json_if_exists<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(
            serde_json::from_str(&text).with_context(|| format!("invalid {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Written to `hub.json` while the hub is running.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HubFile {
    pub pid: u32,
    pub port: u16,
    pub version: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    hub: HubSection,
    #[serde(default)]
    accounts: AccountsSection,
    #[serde(default)]
    plugins: BTreeMap<String, PluginConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct AccountsSection {
    codex_usage_url: Option<String>,
    /// Register the vendors' own logins (e.g. `~/.codex`) as accounts at startup.
    detect: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct HubSection {
    port: Option<u16>,
    mcp_command: Option<Vec<String>>,
    /// Run Codex and Claude Code in runtime hosts that outlive the hub (default on).
    runtime_hosts: Option<bool>,
}

/// Hub settings. `port = 0` binds a random free port.
#[derive(Debug, Clone)]
pub struct HubConfig {
    pub paths: Paths,
    pub port: u16,
    /// `[plugins.<name>]` entries: launch overrides and extra ACP plugins.
    pub plugins: BTreeMap<String, PluginConfig>,
    /// Command for the collaboration MCP server; defaults to `<this executable> mcp`.
    pub mcp_command: Option<Vec<String>>,
    /// Override of the Codex usage endpoint (`[accounts] codex_usage_url`), for tests.
    pub codex_usage_url: Option<String>,
    /// Register logged-in vendor homes (e.g. `~/.codex`) as accounts at startup
    /// (`[accounts] detect`, on by default; off in [`HubConfig::new`] so tests never touch
    /// the user's own logins).
    pub detect_accounts: bool,
    /// Command that runs a runtime host (`<this executable> runtime-host`): Codex and Claude
    /// Code then run in hosts that outlive the hub, and the next hub takes them over. `None`
    /// starts them as direct children (they end with the hub). Off in [`HubConfig::new`].
    pub runtime_host: Option<Vec<String>>,
}

impl HubConfig {
    /// Defaults for `paths`, without reading `config.toml`.
    pub fn new(paths: Paths, port: u16) -> Self {
        Self {
            paths,
            port,
            plugins: BTreeMap::new(),
            mcp_command: None,
            codex_usage_url: None,
            detect_accounts: false,
            runtime_host: None,
        }
    }
}

impl HubConfig {
    /// Reads `config.toml` if it exists; a missing file means defaults.
    pub fn load(paths: Paths) -> Result<Self> {
        let config: ConfigFile = match std::fs::read_to_string(paths.config()) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("invalid {}", paths.config().display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => ConfigFile::default(),
            Err(e) => return Err(e).context("cannot read config.toml"),
        };
        let runtime_host = if config.hub.runtime_hosts.unwrap_or(true) {
            let exe =
                std::env::current_exe().context("cannot locate the crossagents executable")?;
            Some(vec![exe.display().to_string(), "runtime-host".into()])
        } else {
            None
        };
        Ok(Self {
            paths,
            port: config.hub.port.unwrap_or(DEFAULT_PORT),
            plugins: config.plugins,
            mcp_command: config.hub.mcp_command,
            codex_usage_url: config.accounts.codex_usage_url,
            detect_accounts: config.accounts.detect.unwrap_or(true),
            runtime_host,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        assert_eq!(HubConfig::load(paths.clone()).unwrap().port, DEFAULT_PORT);
        std::fs::write(paths.config(), "[hub]\nport = 0\n").unwrap();
        assert_eq!(HubConfig::load(paths).unwrap().port, 0);
    }
}
