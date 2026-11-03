//! Runtime files under the CrossAgents home. See docs/tasks/M0.md §3.2.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use crossagents_core::McpServerSpec;
use crossagents_plugins::PluginConfig;
use serde::{Deserialize, Serialize};

pub const HOME_ENV: &str = "CROSSAGENTS_HOME";
/// Set by `hub run` in its own environment, so every process under it (runtime hosts,
/// agents, their shells) knows which hub runs it and does not stop it by mistake.
pub const HUB_HOME_ENV: &str = "CROSSAGENTS_HUB_HOME";
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
    /// A token that only reads (docs/API-AUTH.md §3), for dashboards and remote viewers.
    pub fn token_read(&self) -> PathBuf {
        self.home.join("token-read")
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
    /// The executable the hub runs from. The GUI manages (starts, upgrades) only a hub that
    /// runs from its own install under `<home>/bin/`; absent in files of older hubs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    hub: HubSection,
    #[serde(default)]
    accounts: AccountsSection,
    #[serde(default)]
    plugins: BTreeMap<String, PluginConfig>,
    /// Extra MCP servers every agent session gets besides the collaboration one.
    #[serde(default)]
    mcp: BTreeMap<String, McpSection>,
}

/// One `[mcp.<name>]` entry: a stdio MCP server mounted into every agent session.
///
/// ```toml
/// [mcp.doubao]
/// command = ["node", "C:/tools/doubao-mcp/server.mjs"]
/// env = { DOUBAO_PAINTER_HOME = "C:/tools/dsh-doubao-painter" }
/// ```
#[derive(Debug, Default, Deserialize)]
struct McpSection {
    /// Program and its arguments; the first item is the executable.
    #[serde(default)]
    command: Vec<String>,
    /// Environment for the server process.
    #[serde(default)]
    env: BTreeMap<String, String>,
}

impl McpSection {
    /// The spec to mount, or why this entry cannot be one.
    fn spec(&self, name: &str) -> Result<McpServerSpec, String> {
        let Some((program, args)) = self.command.split_first() else {
            return Err(format!("[mcp.{name}] has no command"));
        };
        Ok(McpServerSpec {
            name: name.to_owned(),
            command: PathBuf::from(program),
            args: args.to_vec(),
            env: self.env.clone(),
        })
    }
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
    /// Write every event to a JSON-lines file per day under `logs/` (default on).
    event_log: Option<bool>,
    /// Gigabytes all build output under the worktrees may reach before the hub warns; what
    /// nothing uses is reclaimed first (default 120). See
    /// `OrchestratorConfig::build_cache_limit`.
    build_cache_gb: Option<u64>,
    /// The command a new terminal runs (default: `pwsh`, else `powershell`, on Windows; `$SHELL`
    /// elsewhere).
    terminal_shell: Option<Vec<String>>,
    /// Minutes a terminal nobody is connected to, with nothing running, stays (default 120).
    terminal_idle_minutes: Option<u64>,
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
    /// `[mcp.<name>]` entries: further MCP servers every agent session gets, besides the
    /// collaboration one (docs/DESIGN.md §9.3).
    pub mcp_servers: Vec<McpServerSpec>,
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
    /// Write every event to `logs/events-YYYY-MM-DD.jsonl` (`[hub] event_log`, on by default;
    /// off in [`HubConfig::new`] so tests never write to the user's log directory).
    pub event_log: bool,
    /// `[hub] build_cache_gb`, in bytes: see `OrchestratorConfig::build_cache_limit`.
    pub build_cache_limit: u64,
    /// `[hub] terminal_shell`; `None` is the system's usual shell.
    pub terminal_shell: Option<Vec<String>>,
    /// How long a terminal nobody is connected to, with nothing running, stays.
    pub terminal_idle: std::time::Duration,
}

impl HubConfig {
    /// Defaults for `paths`, without reading `config.toml`.
    pub fn new(paths: Paths, port: u16) -> Self {
        Self {
            paths,
            port,
            plugins: BTreeMap::new(),
            mcp_command: None,
            mcp_servers: Vec::new(),
            codex_usage_url: None,
            detect_accounts: false,
            runtime_host: None,
            event_log: false,
            build_cache_limit: crossagents_orchestrator::DEFAULT_BUILD_CACHE_LIMIT,
            terminal_shell: None,
            terminal_idle: crate::terminal::DEFAULT_IDLE,
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
        // Hosts and `mcp` outlive whoever started them: they run from a copy, so a build
        // can replace the executable meanwhile (docs/tasks/DOGFOOD.md §1).
        let exe = std::env::current_exe().context("cannot locate the crossagents executable")?;
        let exe = crate::shadow::stable_exe(&paths, &exe).unwrap_or_else(|err| {
            tracing::warn!(error = %err, "cannot copy the executable; running it in place");
            exe
        });
        let runtime_host = config
            .hub
            .runtime_hosts
            .unwrap_or(true)
            .then(|| vec![exe.display().to_string(), "runtime-host".into()]);
        let mcp_command = config
            .hub
            .mcp_command
            .or_else(|| Some(vec![exe.display().to_string(), "mcp".into()]));
        // An `[mcp.<name>]` entry with no command is a mistake in the file, not a reason to
        // refuse to start: the rest of the hub works without it.
        let mut mcp_servers = Vec::with_capacity(config.mcp.len());
        for (name, section) in &config.mcp {
            match section.spec(name) {
                Ok(spec) => mcp_servers.push(spec),
                Err(why) => tracing::warn!("skipping MCP server: {why}"),
            }
        }
        Ok(Self {
            paths,
            port: config.hub.port.unwrap_or(DEFAULT_PORT),
            plugins: config.plugins,
            mcp_command,
            mcp_servers,
            codex_usage_url: config.accounts.codex_usage_url,
            detect_accounts: config.accounts.detect.unwrap_or(true),
            runtime_host,
            event_log: config.hub.event_log.unwrap_or(true),
            build_cache_limit: config
                .hub
                .build_cache_gb
                .map_or(crossagents_orchestrator::DEFAULT_BUILD_CACHE_LIMIT, |gb| {
                    gb << 30
                }),
            terminal_shell: config
                .hub
                .terminal_shell
                .filter(|command| !command.is_empty()),
            terminal_idle: config
                .hub
                .terminal_idle_minutes
                .map_or(crate::terminal::DEFAULT_IDLE, |minutes| {
                    std::time::Duration::from_secs(minutes * 60)
                }),
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
        let defaults = HubConfig::load(paths.clone()).unwrap();
        assert_eq!(defaults.port, DEFAULT_PORT);
        // One build directory per worktree, and a full test build of this workspace is about
        // 10 GB, so a run at the default parallelism holds ~40 GB (docs/TODO.md §九 ②).
        assert_eq!(defaults.build_cache_limit, 120 << 30);
        std::fs::write(paths.config(), "[hub]\nport = 0\nbuild_cache_gb = 8\n").unwrap();
        let config = HubConfig::load(paths).unwrap();
        assert_eq!(config.port, 0);
        assert_eq!(config.build_cache_limit, 8 << 30);
    }

    #[test]
    fn extra_mcp_servers_come_from_their_own_table() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        std::fs::write(
            paths.config(),
            r#"
[mcp.doubao]
command = ["node", "C:/tools/doubao-mcp/server.mjs"]
env = { DOUBAO_PAINTER_HOME = "C:/tools/dsh-doubao-painter" }

[mcp.broken]
env = { A = "b" }
"#,
        )
        .unwrap();
        let config = HubConfig::load(paths).unwrap();
        // The entry without a command is a mistake in the file, not a failure to start.
        assert_eq!(config.mcp_servers.len(), 1);
        let server = &config.mcp_servers[0];
        assert_eq!(server.name, "doubao");
        assert_eq!(server.command, PathBuf::from("node"));
        assert_eq!(
            server.args,
            vec!["C:/tools/doubao-mcp/server.mjs".to_owned()]
        );
        assert_eq!(
            server.env.get("DOUBAO_PAINTER_HOME").map(String::as_str),
            Some("C:/tools/dsh-doubao-painter")
        );
    }
}
