//! Runtime files under the CrossAgents home. See docs/tasks/M0.md §3.2.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf, Prefix};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use crossagents_core::McpServerSpec;
use crossagents_orchestrator::files::{FileError, rel_of, resolve_in_root};
use crossagents_orchestrator::{FilesScope, Orchestrator};
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
    /// Editable sources of extensions; never executed directly.
    pub fn extensions(&self) -> PathBuf {
        self.home.join("extensions")
    }
    pub fn extension_versions(&self) -> PathBuf {
        self.home.join("extension-versions")
    }
    pub fn extension_data(&self) -> PathBuf {
        self.home.join("extension-data")
    }
    pub fn extensions_state(&self) -> PathBuf {
        self.home.join("extensions.json")
    }
    /// External agent plugins: one directory with a `plugin.toml` each.
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

/// Who named a path the hub is about to read (docs/TODO.md §七, docs/tasks/SELF.md B6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathSource {
    /// An agent, a tool call or anything else not the person: the path must stay inside the
    /// working directory it is judged against.
    Agent,
    /// A file the person picked in the GUI themselves: the one exception, allowed anywhere,
    /// and said so in [`Confined::source`]. Today a pick reaches the hub only as uploaded
    /// bytes (`attachments::store`, which takes no path at all); an entry that takes a picked
    /// path must pass this explicitly, so the exemption shows at the call site.
    PickedByPerson,
}

/// A path that passed [`confine`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confined {
    /// The canonical path: `..`, symlinks and junctions resolved, without `\\?\`.
    pub abs: PathBuf,
    /// Its place inside the root with `/`; `None` only for a person's pick outside it.
    pub rel: Option<String>,
    pub source: PathSource,
}

/// Where `input` points, if it may be read for `source`: inside `root` (the agent's working
/// directory) for [`PathSource::Agent`]. Refused as [`FileError::Outside`]:
///
/// - a relative path whose `..` climbs above the root, judged before the file system is asked;
/// - an absolute path on another drive, a UNC share (`\\server\share`, `//server/share`,
///   `\\?\UNC\…`), a device path (`\\.\`, `\\?\GLOBALROOT`), a drive-relative `C:x` or a
///   root-relative `\x`, also before the file system is asked (no share is ever contacted);
/// - anything whose canonical path is not under the canonical root: `..` through a
///   symlink or junction, a link that leads out, an absolute path elsewhere on the drive.
///
/// `\` and `/` are the same separator. A missing file is [`FileError::NotFound`].
pub fn confine(root: &Path, input: &str, source: PathSource) -> Result<Confined, FileError> {
    if input.contains('\0') {
        return Err(FileError::BadRequest(
            "the path contains a NUL byte".to_owned(),
        ));
    }
    let root = dunce::canonicalize(root)
        .map_err(|_| FileError::NotFound(format!("no workspace at {}", root.display())))?;
    let path = PathBuf::from(native_separators(input));
    if source == PathSource::Agent {
        precheck(&root, input)?;
    }
    let joined = if is_anchored(&path) {
        path
    } else {
        root.join(path)
    };
    let abs = dunce::canonicalize(&joined)
        .map_err(|_| FileError::NotFound(format!("{input} in {}", root.display())))?;
    let rel = rel_of(&root, &abs);
    if rel.is_none() && source == PathSource::Agent {
        return Err(FileError::Outside(input.to_owned()));
    }
    Ok(Confined { abs, rel, source })
}

/// A path an agent named, checked against several roots in order: the first root the path
/// is inside wins, and its [`Confined::rel`] is relative to that root. For each root, a
/// share or device path is refused before the file system is asked ([`refuse_remote`], so
/// no share is ever contacted), then [`resolve_in_root`] resolves it canonically and refuses
/// `..` above the root, a symlink or junction that leads out and an absolute path elsewhere
/// (and `.git`), exactly as before. Each root is judged on its own, so a path is never let
/// through by mixing them. Refused, the error is the first root's, unchanged.
///
/// The image endpoint uses it for "the working directory or the system temp directory"
/// (agents are told to leave images for the person in the temp directory).
pub fn confine_any(roots: &[&Path], input: &str) -> Result<Confined, FileError> {
    let mut first = None;
    for root in roots {
        let judged = refuse_remote(root, input).and_then(|()| resolve_in_root(root, input));
        match judged {
            Ok((abs, rel)) => {
                return Ok(Confined {
                    abs,
                    rel: Some(rel),
                    source: PathSource::Agent,
                });
            }
            Err(err) => {
                first.get_or_insert(err);
            }
        }
    }
    Err(first.unwrap_or_else(|| FileError::Outside(input.to_owned())))
}

/// The part of [`confine`] made before the file system is asked, for an entry that resolves
/// the path itself afterwards (canonically, as `resolve_in_root` does): refuses a NUL byte,
/// a `..` above `root`, and an absolute path on another drive or share than `root`'s, so a
/// UNC path an agent names never makes the hub contact that share.
pub fn precheck(root: &Path, input: &str) -> Result<(), FileError> {
    if input.contains('\0') {
        return Err(FileError::BadRequest(
            "the path contains a NUL byte".to_owned(),
        ));
    }
    let path = PathBuf::from(native_separators(input));
    lexically_within(root, &path).ok_or_else(|| FileError::Outside(input.to_owned()))
}

/// Whether `input` names a network share or a device (`\\server\share`, `//server/share`,
/// `\\?\UNC\…`, `\\.\…`, `\\?\GLOBALROOT…`) rather than a local drive. Always `false` off
/// Windows, where `//x` is just the root.
pub fn names_remote(input: &str) -> bool {
    let path = PathBuf::from(native_separators(input));
    matches!(
        path.components().next(),
        Some(Component::Prefix(prefix)) if !matches!(
            prefix.kind(),
            Prefix::Disk(_) | Prefix::VerbatimDisk(_)
        )
    )
}

/// What an entry that resolves a path canonically afterwards (`resolve_in_root`, which
/// already refuses `..`, links and absolute paths that land outside) adds before it: a path
/// on a share or device is refused unless it is on `root`'s own share and inside it, so the
/// hub never contacts a share an agent names (and never leaks its credentials to it).
pub fn refuse_remote(root: &Path, input: &str) -> Result<(), FileError> {
    if names_remote(input) {
        precheck(root, input)
    } else {
        Ok(())
    }
}

/// [`refuse_remote`] for an entry of the file API, which knows its scope rather than its
/// root: the root is looked up only when `input` names a share or device.
pub async fn refuse_remote_in(
    orchestrator: &Orchestrator,
    scope: FilesScope<'_>,
    input: &str,
) -> Result<(), FileError> {
    if !names_remote(input) {
        return Ok(());
    }
    let root = orchestrator.files_root(scope).await?;
    refuse_remote(&root, input)
}

/// `inputs` for `files/resolve`, with each one [`refuse_remote`] refuses replaced by a path
/// that resolves to nothing (a NUL byte), so it answers `None` like any outside path while
/// the share is never contacted; the response still names the inputs as they were sent.
pub async fn without_remote(
    orchestrator: &Orchestrator,
    scope: FilesScope<'_>,
    inputs: &[String],
) -> Result<Vec<String>, FileError> {
    if !inputs.iter().any(|input| names_remote(input)) {
        return Ok(inputs.to_vec());
    }
    let root = orchestrator.files_root(scope).await?;
    Ok(inputs
        .iter()
        .map(|input| match refuse_remote(&root, input) {
            Ok(()) => input.clone(),
            Err(_) => "\0".to_owned(),
        })
        .collect())
}

/// `input` with the separator of this system, so a Windows prefix (`\\server\share`) is
/// read as one whichever way it was written.
fn native_separators(input: &str) -> String {
    if cfg!(windows) {
        input.replace('/', "\\")
    } else {
        input.replace('\\', "/")
    }
}

/// Whether `path` starts at a drive, share or root rather than at the working directory.
fn is_anchored(path: &Path) -> bool {
    matches!(
        path.components().next(),
        Some(Component::Prefix(_) | Component::RootDir)
    )
}

/// One component as compared: a drive as `C:` (verbatim or not), a share as
/// `//server/share`, a root as `""`; `None` for prefixes no workspace is under (`\\.\`,
/// `\\?\GLOBALROOT`).
fn part(component: Component<'_>) -> Option<String> {
    Some(match component {
        Component::Prefix(prefix) => match prefix.kind() {
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => {
                format!("{}:", (d as char).to_ascii_uppercase())
            }
            Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                format!("//{}/{}", server.to_string_lossy(), share.to_string_lossy())
            }
            Prefix::Verbatim(_) | Prefix::DeviceNS(_) => return None,
        },
        Component::RootDir => String::new(),
        Component::CurDir => ".".to_owned(),
        Component::ParentDir => "..".to_owned(),
        Component::Normal(name) => name.to_string_lossy().into_owned(),
    })
}

fn same_part(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// The check made before the file system is asked: `Some(())` when `path` may be inside
/// `root` (canonical). A relative path must not climb above the root; an absolute one must
/// be on the root's own drive or share. Anything else on that drive is left to the canonical
/// check, which also knows short names (`LINKKZ~1`) and links.
fn lexically_within(root: &Path, path: &Path) -> Option<()> {
    let anchor_of = |path: &Path| -> Option<Vec<String>> {
        path.components()
            .take_while(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
            .map(part)
            .collect()
    };
    let anchor = anchor_of(path)?;
    if anchor.is_empty() {
        // Relative: from the root, and never above it.
        let mut depth = 0usize;
        for component in path.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => depth = depth.checked_sub(1)?,
                _ => depth += 1,
            }
        }
        return Some(());
    }
    // Anchored: the same drive or share and root as the working directory. A drive-relative
    // `C:x` or a root-relative `\x` has only half of the anchor, and is refused.
    let root_anchor = anchor_of(root)?;
    if anchor.len() != root_anchor.len()
        || !anchor
            .iter()
            .zip(&root_anchor)
            .all(|(a, b)| same_part(a, b))
    {
        return None;
    }
    Some(())
}

/// Writes `bytes` to `path` so a reader sees either the old file or the new one, never a
/// half-written one: the bytes go to a temporary file beside it, which is flushed and then
/// renamed over `path`. On Windows the rename replaces an existing file (MoveFileEx with
/// MOVEFILE_REPLACE_EXISTING); a reader holding the old file open can make it fail for a
/// moment, so it is retried briefly.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("no file name to write"))?
        .to_string_lossy();
    // The process id keeps two writers from sharing one temporary file.
    let temporary = path.with_file_name(format!("{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        let mut attempt = 0;
        loop {
            match std::fs::rename(&temporary, path) {
                Ok(()) => return Ok(()),
                Err(_) if attempt < 10 && cfg!(windows) => {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(err) => return Err(err),
            }
        }
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
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
    /// Seconds a batch of notices stays quiet after the latest one before it wakes (default 60).
    notice_batch_quiet_secs: Option<u64>,
    /// Seconds after the earliest batched notice at which it wakes anyway (default 180).
    notice_batch_max_secs: Option<u64>,
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
    /// Quiet interval after the latest batched notice before that batch wakes an agent.
    pub notice_batch_quiet: std::time::Duration,
    /// Longest a batched notice waits after the earliest one in the batch.
    pub notice_batch_max: std::time::Duration,
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
            notice_batch_quiet: std::time::Duration::from_secs(60),
            notice_batch_max: std::time::Duration::from_secs(180),
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
            notice_batch_quiet: std::time::Duration::from_secs(
                config.hub.notice_batch_quiet_secs.unwrap_or(60),
            ),
            notice_batch_max: std::time::Duration::from_secs(
                config.hub.notice_batch_max_secs.unwrap_or(180),
            ),
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

    fn workspace() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("a.txt"), "a").unwrap();
        std::fs::write(dir.path().join("secret.txt"), "s").unwrap();
        (dir, root)
    }

    fn outside(result: Result<Confined, FileError>) -> bool {
        matches!(result, Err(FileError::Outside(_)))
    }

    #[test]
    fn confine_keeps_paths_inside() {
        let (_dir, root) = workspace();
        let inside = confine(&root, "src/a.txt", PathSource::Agent).unwrap();
        assert_eq!(inside.rel.as_deref(), Some("src/a.txt"));
        assert_eq!(inside.source, PathSource::Agent);
        assert_eq!(
            confine(&root, "src\\..\\src\\.\\a.txt", PathSource::Agent)
                .unwrap()
                .rel
                .as_deref(),
            Some("src/a.txt")
        );
        // An absolute path inside the root, written any way, is fine.
        let abs = dunce::canonicalize(&root)
            .unwrap()
            .join("src")
            .join("a.txt");
        let abs = abs.display().to_string();
        assert!(confine(&root, &abs, PathSource::Agent).is_ok());
        assert!(confine(&root, &abs.replace('\\', "/"), PathSource::Agent).is_ok());
        #[cfg(windows)]
        assert!(confine(&root, &abs.to_uppercase(), PathSource::Agent).is_ok());
        assert!(matches!(
            confine(&root, "src/none.txt", PathSource::Agent),
            Err(FileError::NotFound(_))
        ));
        assert!(matches!(
            confine(&root, "a\0b", PathSource::Agent),
            Err(FileError::BadRequest(_))
        ));
    }

    #[test]
    fn confine_refuses_parent_dirs() {
        let (_dir, root) = workspace();
        for input in [
            "../secret.txt",
            "..\\secret.txt",
            "src/../../secret.txt",
            "src\\..\\..\\secret.txt",
            "./../ws/../secret.txt",
            "..",
        ] {
            assert!(outside(confine(&root, input, PathSource::Agent)), "{input}");
            assert!(precheck(&root, input).is_err(), "{input}");
        }
        // Refused before the file system is asked: even a missing target is "outside".
        assert!(outside(confine(&root, "../nope", PathSource::Agent)));
        // Coming back in is fine.
        assert!(precheck(&root, "src/../src/a.txt").is_ok());
    }

    #[test]
    fn confine_refuses_absolute_paths_elsewhere() {
        let (dir, root) = workspace();
        let secret = dir.path().join("secret.txt").display().to_string();
        assert!(outside(confine(&root, &secret, PathSource::Agent)));
        let exe = std::env::current_exe().unwrap().display().to_string();
        assert!(outside(confine(&root, &exe, PathSource::Agent)));
        #[cfg(unix)]
        assert!(outside(confine(&root, "/etc/passwd", PathSource::Agent)));
    }

    #[cfg(windows)]
    #[test]
    fn confine_refuses_other_drives_and_half_anchored_paths() {
        let (_dir, root) = workspace();
        let canonical = dunce::canonicalize(&root).unwrap();
        let drive = canonical.display().to_string()[..1].to_ascii_uppercase();
        let other = if drive == "Z" { "Y" } else { "Z" };
        for input in [
            format!("{other}:\\secret.txt"),
            format!("{other}:/secret.txt"),
            format!("\\\\?\\{other}:\\secret.txt"),
            // Drive-relative and root-relative: each has half of the anchor.
            format!("{drive}:secret.txt"),
            "\\Windows\\win.ini".to_owned(),
            "/Windows/win.ini".to_owned(),
        ] {
            assert!(precheck(&root, &input).is_err(), "{input}");
            assert!(
                outside(confine(&root, &input, PathSource::Agent)),
                "{input}"
            );
        }
        // The root's own drive, written verbatim, is judged by its canonical place.
        let verbatim = format!("\\\\?\\{}", canonical.join("src").join("a.txt").display());
        assert!(precheck(&root, &verbatim).is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn confine_refuses_unc_and_device_paths_without_asking() {
        let (_dir, root) = workspace();
        // Hosts that do not exist: had the hub asked, it would wait on the network; these
        // return at once, and as "outside".
        for input in [
            "\\\\no-such-host.invalid\\share\\x.png",
            "//no-such-host.invalid/share/x.png",
            "\\\\?\\UNC\\no-such-host.invalid\\share\\x.png",
            "\\\\.\\C:\\Windows\\win.ini",
            "\\\\.\\PhysicalDrive0",
            "\\\\?\\GLOBALROOT\\Device\\HarddiskVolume1\\x",
            "//./pipe/x",
        ] {
            assert!(names_remote(input), "{input}");
            assert!(refuse_remote(&root, input).is_err(), "{input}");
            assert!(outside(confine(&root, input, PathSource::Agent)), "{input}");
        }
        for input in ["src/a.txt", "C:\\x", "\\\\?\\C:\\x", "../x", "\\x"] {
            assert!(!names_remote(input), "{input}");
            assert!(refuse_remote(&root, input).is_ok(), "{input}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn backslashes_are_separators_and_double_slashes_are_the_root() {
        let (_dir, root) = workspace();
        assert!(!names_remote("//server/share/x"));
        assert!(confine(&root, "\\\\server\\share\\x", PathSource::Agent).is_err());
        assert!(outside(confine(&root, "..\\secret.txt", PathSource::Agent)));
    }

    #[test]
    fn confine_refuses_links_that_lead_out() {
        let (dir, root) = workspace();
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("s.txt"), "s").unwrap();
        let link = root.join("link");
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&out, &link).is_ok();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&out, &link).is_ok()
            || std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(&out)
                .output()
                .is_ok_and(|o| o.status.success());
        if !made {
            eprintln!("skipped: cannot create a link or junction here");
            return;
        }
        // Lexically inside, so only the canonical check can see it.
        assert!(precheck(&root, "link/s.txt").is_ok());
        assert!(outside(confine(&root, "link/s.txt", PathSource::Agent)));
        assert!(outside(confine(&root, "link\\s.txt", PathSource::Agent)));
        assert!(outside(confine(&root, "link", PathSource::Agent)));
    }

    /// Several roots: inside any of them is fine; `..` or a link out of all of them is not.
    #[test]
    fn confine_any_takes_either_root_and_nothing_between() {
        let (dir, root) = workspace();
        let second = dir.path().join("tmp");
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(second.join("b.png"), "b").unwrap();
        let roots = [root.as_path(), second.as_path()];
        assert_eq!(
            confine_any(&roots, "src/a.txt").unwrap().rel.as_deref(),
            Some("src/a.txt")
        );
        // A relative path is tried from each root in turn.
        assert_eq!(
            confine_any(&roots, "b.png").unwrap().rel.as_deref(),
            Some("b.png")
        );
        let abs = second.join("b.png").display().to_string();
        assert!(confine_any(&roots, &abs).is_ok());
        // `..` from either root to the parent of both is refused.
        for input in [
            "../secret.txt",
            "../tmp/../secret.txt",
            "src/../../secret.txt",
        ] {
            assert!(
                matches!(confine_any(&roots, input), Err(FileError::Outside(_))),
                "{input}"
            );
        }
        let secret = dir.path().join("secret.txt").display().to_string();
        assert!(matches!(
            confine_any(&roots, &secret),
            Err(FileError::Outside(_))
        ));
        #[cfg(windows)]
        assert!(matches!(
            confine_any(&roots, "\\\\no-such-host.invalid\\share\\x.png"),
            Err(FileError::Outside(_))
        ));
        // A link inside the second root that leads out is refused too.
        let link = second.join("out");
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(dir.path(), &link).is_ok();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(dir.path(), &link).is_ok()
            || std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(dir.path())
                .output()
                .is_ok_and(|o| o.status.success());
        if made {
            // Refused: the working directory has no `out` (its error comes first, as
            // before), and the second root judges the link as leading out.
            assert!(confine_any(&roots, "out/secret.txt").is_err());
            assert!(matches!(
                confine_any(&[second.as_path()], "out/secret.txt"),
                Err(FileError::Outside(_))
            ));
        } else {
            eprintln!("skipped the link case: cannot create a link or junction here");
        }
        // `.git` stays out of reach, as through `resolve_in_root`.
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/x.png"), "x").unwrap();
        assert!(confine_any(&roots, ".git/x.png").is_err());
        // Missing everywhere: the first root's error, as before.
        assert!(matches!(
            confine_any(&roots, "nowhere.png"),
            Err(FileError::NotFound(what)) if what.starts_with("nowhere.png in ")
        ));
    }

    /// The one exception: a file the person picked may be anywhere, and says so.
    #[test]
    fn a_persons_pick_may_be_outside_and_is_marked() {
        let (dir, root) = workspace();
        let secret = dir.path().join("secret.txt").display().to_string();
        let picked = confine(&root, &secret, PathSource::PickedByPerson).unwrap();
        assert_eq!(picked.source, PathSource::PickedByPerson);
        assert_eq!(picked.rel, None);
        assert_eq!(
            picked.abs,
            dunce::canonicalize(dir.path().join("secret.txt")).unwrap()
        );
        let picked = confine(&root, "../secret.txt", PathSource::PickedByPerson).unwrap();
        assert_eq!(picked.rel, None);
        // Inside, it still gets its place.
        let picked = confine(&root, "src/a.txt", PathSource::PickedByPerson).unwrap();
        assert_eq!(picked.rel.as_deref(), Some("src/a.txt"));
        // The same paths for an agent are refused.
        assert!(outside(confine(&root, &secret, PathSource::Agent)));
    }

    #[test]
    fn write_atomic_replaces_the_file_and_leaves_no_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hub.json");
        write_atomic(&file, b"{\"a\":1}").unwrap();
        write_atomic(&file, b"{\"a\":2}").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{\"a\":2}");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["hub.json".to_owned()]);
    }

    /// The race `hub start` hit: a reader polling `hub.json` while the hub writes it must
    /// never see an empty or half-written file.
    #[test]
    fn a_reader_never_sees_a_half_written_hub_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hub.json");
        let big = |n: usize| format!("{{\"n\":{n},\"pad\":\"{}\"}}", "x".repeat(64 * 1024));
        write_atomic(&file, big(0).as_bytes()).unwrap();

        let reader = {
            let file = file.clone();
            std::thread::spawn(move || {
                let mut parsed = 0;
                for _ in 0..2000 {
                    // A read can fail while a rename is in flight on Windows; a read that
                    // succeeds must be a whole file.
                    if let Ok(text) = std::fs::read_to_string(&file) {
                        serde_json::from_str::<serde_json::Value>(&text).expect("a whole hub file");
                        parsed += 1;
                    }
                }
                parsed
            })
        };
        for n in 1..300 {
            write_atomic(&file, big(n).as_bytes()).unwrap();
        }
        assert!(reader.join().unwrap() > 0);
    }
}
