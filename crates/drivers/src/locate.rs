//! Finding a vendor CLI on this machine, from its agent description (docs/DESIGN.md §9.3).
//!
//! The description says where to look; this module does the looking, the same way for every
//! vendor: `native_command` first, then `env`, `PATH`, the vendor's own install paths, and the
//! copy another app keeps. A hit then goes through the description's named `shim`, for the two
//! cases a path cannot express: npm wrappers that must be run through `node` (their arguments
//! would not survive `cmd.exe`'s quoting), and Codex's wrapper, whose vendor binary has to be
//! started directly (killing the wrapper would leave the child running).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crossagents_core::{CliSource, CliStatus, Located};
use serde::{Deserialize, Deserializer};
use tokio::process::Command;

use crate::process::{find_in_path_in, hide_console};

/// How long a CLI gets to print its version.
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// The key a setting is given under (`windows`, `macos`, `linux`).
pub fn os_name() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// A setting for every system (`paths = [...]`), or one per system
/// (`[locate.paths] windows = [...]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerOs<T> {
    all: Option<T>,
    by_os: BTreeMap<String, T>,
}

impl<T> Default for PerOs<T> {
    fn default() -> Self {
        Self {
            all: None,
            by_os: BTreeMap::new(),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OsRepr<T> {
    All(T),
    ByOs(BTreeMap<String, T>),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for PerOs<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match OsRepr::deserialize(deserializer)? {
            OsRepr::All(value) => Self {
                all: Some(value),
                by_os: BTreeMap::new(),
            },
            OsRepr::ByOs(by_os) => Self { all: None, by_os },
        })
    }
}

impl<T> PerOs<T> {
    /// The value for this system.
    pub fn get(&self) -> Option<&T> {
        self.by_os.get(os_name()).or(self.all.as_ref())
    }

    /// The value for `os`, for tests.
    pub fn get_for(&self, os: &str) -> Option<&T> {
        self.by_os.get(os).or(self.all.as_ref())
    }

    pub fn is_empty(&self) -> bool {
        self.all.is_none() && self.by_os.is_empty()
    }
}

/// What `[install]` says: shown to the person, never executed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Install {
    /// The command that installs the CLI, e.g. `npm install -g @tencent-ai/codebuddy-code`.
    pub hint: Option<String>,
    pub docs: Option<String>,
    /// What to do after installing (sign in), part of the "not installed" message.
    pub login: Option<String>,
    /// Why another app's copy does not do, part of that message.
    pub notes: Option<String>,
    /// What the CLI is called in that message (`CodeBuddy`); the plugin's name by default.
    pub label: Option<String>,
}

impl Install {
    pub fn is_empty(&self) -> bool {
        self.hint.is_none()
            && self.docs.is_none()
            && self.login.is_none()
            && self.notes.is_none()
            && self.label.is_none()
    }

    /// What `other` says wins; what it leaves out stays.
    pub fn merge(&mut self, other: &Self) {
        for (mine, theirs) in [
            (&mut self.hint, &other.hint),
            (&mut self.docs, &other.docs),
            (&mut self.login, &other.login),
            (&mut self.notes, &other.notes),
            (&mut self.label, &other.label),
        ] {
            if theirs.is_some() {
                *mine = theirs.clone();
            }
        }
    }

    /// What to say when a session is asked of a CLI this machine does not have: what to
    /// install, how to sign in, and why another app's copy is not used.
    pub fn missing_message(&self, name: &str) -> Option<String> {
        let hint = self.hint.as_deref()?;
        let label = self.label.as_deref().unwrap_or(name);
        let mut text = format!("the {label} CLI is not installed: run `{hint}`");
        if let Some(login) = self.login.as_deref().filter(|l| !l.is_empty()) {
            text.push_str(", ");
            text.push_str(login);
        }
        text.push('.');
        if let Some(notes) = self.notes.as_deref().filter(|n| !n.is_empty()) {
            text.push(' ');
            text.push_str(notes);
        }
        Some(text)
    }
}

/// Where an agent's CLI is, as its description states it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Locator {
    /// Environment variable that names the CLI; checked before `commands`.
    pub env: PerOs<String>,
    /// Names to look up on `PATH`.
    pub commands: PerOs<Vec<String>>,
    /// Files to try in order; `~`, `%VAR%`, `$VAR` and `*` are expanded.
    pub paths: PerOs<Vec<String>>,
    /// Copies another app keeps for itself; the newest one wins.
    pub bundled: PerOs<Vec<String>>,
    /// How to get from an npm wrapper to the real program: `codex-vendor` or `node-script`.
    pub shim: Option<String>,
    /// Arguments that print the version (`--version`).
    pub version: PerOs<Vec<String>>,
}

/// What a lookup reads besides the filesystem: this process's own environment normally, a
/// test's own when it gives one (tests run in parallel and must not touch the real one).
#[derive(Debug, Clone, Default)]
pub struct Machine {
    /// `PATH`, as directories.
    pub path: Option<Vec<PathBuf>>,
    pub vars: Option<BTreeMap<String, String>>,
    /// What `~` means.
    pub home: Option<PathBuf>,
}

impl Machine {
    fn var(&self, name: &str) -> Option<String> {
        match &self.vars {
            Some(vars) => vars.get(name).cloned(),
            None => std::env::var(name).ok(),
        }
    }

    fn home(&self) -> Option<PathBuf> {
        if let Some(home) = &self.home {
            return Some(home.clone());
        }
        let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        std::env::var_os(key).map(PathBuf::from)
    }
}

impl Locator {
    pub fn is_empty(&self) -> bool {
        self.env.is_empty()
            && self.commands.is_empty()
            && self.paths.is_empty()
            && self.bundled.is_empty()
            && self.shim.is_none()
            && self.version.is_empty()
    }

    /// What `other` says wins; what it leaves out stays.
    pub fn merge(&mut self, other: &Self) {
        if !other.env.is_empty() {
            self.env = other.env.clone();
        }
        if !other.commands.is_empty() {
            self.commands = other.commands.clone();
        }
        if !other.paths.is_empty() {
            self.paths = other.paths.clone();
        }
        if !other.bundled.is_empty() {
            self.bundled = other.bundled.clone();
        }
        if other.shim.is_some() {
            self.shim = other.shim.clone();
        }
        if !other.version.is_empty() {
            self.version = other.version.clone();
        }
    }

    /// The CLI this description names, on this machine.
    pub fn locate(&self) -> Option<Located> {
        self.locate_in(&Machine::default())
    }

    /// [`Self::locate`], reading `machine` instead of this process's environment.
    pub fn locate_in(&self, machine: &Machine) -> Option<Located> {
        if let Some(var) = self.env.get()
            && let Some(value) = machine.var(var).filter(|value| !value.is_empty())
        {
            let path = self.resolve_in(&value, machine);
            return Some(self.launch(vec![display(&path)], path, CliSource::Env, machine));
        }
        for name in self.commands.get().into_iter().flatten() {
            let path = find_in_path_in(name, machine.path.as_deref());
            if path.is_file() {
                return Some(self.launch(vec![display(&path)], path, CliSource::Path, machine));
            }
        }
        for pattern in self.paths.get().into_iter().flatten() {
            if let Some(path) = first_file(pattern, machine) {
                return Some(self.launch(vec![display(&path)], path, CliSource::Native, machine));
            }
        }
        // A copy another app keeps is already the real program: no shim to see through.
        if let Some(path) = self.newest_bundled(machine) {
            return Some(Located {
                argv: vec![display(&path)],
                path,
                source: CliSource::Bundled,
                version: None,
            });
        }
        None
    }

    /// `program` resolved as this description resolves a program: a path as it is, a bare name
    /// on `PATH`, and for one of the description's own names, its install paths and the copies
    /// another app keeps.
    pub fn resolve_in(&self, program: &str, machine: &Machine) -> PathBuf {
        let given = Path::new(program);
        if given.components().count() > 1 {
            return given.to_path_buf();
        }
        let on_path = find_in_path_in(program, machine.path.as_deref());
        if on_path.is_file() {
            return on_path;
        }
        let described = self
            .commands
            .get()
            .is_some_and(|names| names.iter().any(|name| name == program));
        if described {
            if let Some(found) = self.first_path(machine) {
                return found;
            }
            if let Some(found) = self.newest_bundled(machine) {
                return found;
            }
        }
        on_path
    }

    /// The program `argv` names, as this description would find it (shim included), with the
    /// rest of the arguments after it.
    pub fn resolve(&self, argv: &[String], machine: &Machine) -> Vec<String> {
        let Some((program, rest)) = argv.split_first() else {
            return Vec::new();
        };
        let path = self.resolve_in(program, machine);
        let mut located = self.launch(vec![display(&path)], path, CliSource::Config, machine);
        located.argv.extend(rest.iter().cloned());
        located.argv
    }

    /// `program` resolved to the file it names, as [`Self::resolve_in`] does it on this
    /// machine.
    pub fn locate_program(&self, program: &str) -> PathBuf {
        self.resolve_in(program, &Machine::default())
    }

    /// The first of `paths` that exists.
    fn first_path(&self, machine: &Machine) -> Option<PathBuf> {
        self.paths
            .get()
            .into_iter()
            .flatten()
            .find_map(|pattern| first_file(pattern, machine))
    }

    /// The newest file the `bundled` patterns name (a desktop app keeps one per build).
    fn newest_bundled(&self, machine: &Machine) -> Option<PathBuf> {
        self.bundled
            .get()
            .into_iter()
            .flatten()
            .flat_map(|pattern| expand(pattern, machine))
            .filter(|path| path.is_file())
            .filter_map(|path| Some((std::fs::metadata(&path).ok()?.modified().ok()?, path)))
            .max()
            .map(|(_, path)| path)
    }

    /// A located CLI, with the description's shim applied.
    fn launch(
        &self,
        mut argv: Vec<String>,
        path: PathBuf,
        source: CliSource,
        machine: &Machine,
    ) -> Located {
        if let Some(name) = &self.shim
            && let Some(replacement) = shim(name, &path, machine)
        {
            argv = replacement;
        }
        Located {
            argv,
            path,
            source,
            version: None,
        }
    }
}

/// A CLI the description names, as this machine has it now (docs/DESIGN.md §9.3).
///
/// The lookup itself is not kept: it is a filesystem walk, cheap next to the session it
/// precedes, and a hub outlives installs and updates. A hit is reused only while the file it
/// named is still there -- a desktop app that updates removes the build directory of the
/// build it replaces -- and a miss is never reused, so a CLI installed while the hub runs is
/// found. What is worth keeping, asking the CLI its version, is cached by (path, modification
/// time).
#[derive(Clone)]
pub struct Cli {
    inner: std::sync::Arc<CliInner>,
}

struct CliInner {
    /// `native_command`: this wins over everything the description says.
    configured: Vec<String>,
    locator: Locator,
    install: Install,
    cache: std::sync::Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    /// The last hit, reused while `path` is still a file.
    found: Option<Located>,
    /// What the version probe last answered, for one (path, modification time).
    version: Option<VersionEntry>,
    /// The file a version probe is running for.
    probing: Option<PathBuf>,
}

struct VersionEntry {
    path: PathBuf,
    modified: SystemTime,
    value: Option<String>,
}

impl Cli {
    pub fn new(configured: Vec<String>, locator: Locator, install: Install) -> Self {
        Self {
            inner: std::sync::Arc::new(CliInner {
                configured,
                locator,
                install,
                cache: std::sync::Mutex::default(),
            }),
        }
    }

    /// A CLI at `argv`, as a configuration names it (tests, and callers without a
    /// description).
    pub fn at(argv: Vec<String>) -> Self {
        Self::new(argv, Locator::default(), Install::default())
    }

    pub fn configured(&self) -> &[String] {
        &self.inner.configured
    }

    pub fn locator(&self) -> &Locator {
        &self.inner.locator
    }

    pub fn install(&self) -> &Install {
        &self.inner.install
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.inner.cache.lock().expect("cli cache")
    }

    /// The CLI as it is now.
    fn found(&self) -> Option<Located> {
        self.found_in(&Machine::default())
    }

    /// [`Self::found`], on `machine` rather than this process's environment (tests).
    fn found_in(&self, machine: &Machine) -> Option<Located> {
        let mut cache = self.cache();
        if let Some(found) = &cache.found
            && found.path.is_file()
        {
            return Some(found.clone());
        }
        // Not found last time, or what was found is gone: look again.
        let found = self.find_in(machine);
        cache.found = found.clone();
        found
    }

    fn find_in(&self, machine: &Machine) -> Option<Located> {
        let inner = &self.inner;
        if let Some(first) = inner.configured.first() {
            let path = inner.locator.resolve_in(first, machine);
            let mut argv = vec![display(&path)];
            argv.extend(inner.configured[1..].iter().cloned());
            return Some(inner.locator.launch(argv, path, CliSource::Config, machine));
        }
        inner.locator.locate_in(machine)
    }

    /// The arguments that launch it, when it is on this machine.
    pub fn command(&self) -> Option<Vec<String>> {
        self.found().map(|found| found.argv)
    }

    /// The file that was found.
    pub fn path(&self) -> Option<PathBuf> {
        self.found().map(|found| found.path)
    }

    pub fn source(&self) -> Option<CliSource> {
        self.found().map(|found| found.source)
    }

    /// The name the description looks for (`codex`), for the arguments a driver adds itself.
    pub fn default_name(&self) -> Option<String> {
        self.inner
            .locator
            .commands
            .get()
            .and_then(|names| names.first().cloned())
    }

    pub fn installed(&self) -> bool {
        self.found().is_some()
    }

    /// `argv` with its program resolved as the description resolves one.
    pub fn launch(&self, argv: &[String]) -> Vec<String> {
        self.inner.locator.resolve(argv, &Machine::default())
    }

    /// What to say when a session is asked of a CLI that is not installed.
    pub fn missing_message(&self, name: &str) -> Option<String> {
        self.inner.install.missing_message(name)
    }

    /// Where the CLI is, and what installing it takes.
    ///
    /// The version is reported only once it has been asked: `/v1/plugins` must not wait for
    /// a subprocess, so a stale one is asked for in the background and the next call reports
    /// it (`None` meanwhile).
    pub async fn status(&self) -> CliStatus {
        self.status_in(&Machine::default()).await
    }

    /// [`Self::status`], on `machine` rather than this process's environment (tests).
    pub async fn status_in(&self, machine: &Machine) -> CliStatus {
        let mut located = self.found_in(machine);
        if let Some(found) = &mut located {
            match self.known_version(found) {
                Some(version) => found.version = version,
                None => {
                    found.version = None;
                    self.ask_in_background(found);
                }
            }
        }
        CliStatus {
            located,
            install_hint: self.inner.install.hint.clone(),
            docs: self.inner.install.docs.clone(),
        }
    }

    /// What `<argv> <version args>` prints, asked again when the file changed since it was
    /// last asked (5 s at most).
    pub async fn version(&self) -> Option<String> {
        self.version_in(&Machine::default()).await
    }

    /// [`Self::version`], on `machine` rather than this process's environment (tests).
    pub async fn version_in(&self, machine: &Machine) -> Option<String> {
        let found = self.found_in(machine)?;
        if let Some(value) = self.known_version(&found) {
            return value;
        }
        self.ask(found).await
    }

    /// The version last asked for this very file: `None` when it has not been asked for this
    /// (path, modification time) yet.
    fn known_version(&self, found: &Located) -> Option<Option<String>> {
        let cache = self.cache();
        let entry = cache.version.as_ref()?;
        let modified = modified(&found.path)?;
        (entry.path == found.path && entry.modified == modified).then(|| entry.value.clone())
    }

    /// Asks the version without waiting for it: `/v1/plugins` reports it once it answers.
    fn ask_in_background(&self, found: &Located) {
        {
            let mut cache = self.cache();
            if cache.probing.as_deref() == Some(found.path.as_path()) {
                return;
            }
            cache.probing = Some(found.path.clone());
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            // No runtime to ask from (a lookup-only test): nothing to wait for.
            self.cache().probing = None;
            return;
        };
        let cli = self.clone();
        let found = found.clone();
        runtime.spawn(async move {
            cli.ask(found).await;
        });
    }

    /// Asks the version and keeps it with the file it came from.
    async fn ask(&self, found: Located) -> Option<String> {
        let value = self.read_version(&found).await;
        let mut cache = self.cache();
        if cache.probing.as_deref() == Some(found.path.as_path()) {
            cache.probing = None;
        }
        // The modification time now, not before: what the answer is about.
        if let Some(modified) = modified(&found.path) {
            cache.version = Some(VersionEntry {
                path: found.path,
                modified,
                value: value.clone(),
            });
        }
        value
    }

    async fn read_version(&self, found: &Located) -> Option<String> {
        let args = self.inner.locator.version.get()?;
        let (program, rest) = found.argv.split_first()?;
        let mut command = Command::new(program);
        command
            .args(rest)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        hide_console(&mut command);
        let output = tokio::time::timeout(VERSION_TIMEOUT, command.output())
            .await
            .ok()?
            .ok()?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        parse_version(&text)
    }
}

/// When `path` was last changed: the other half of the version cache's key.
fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

/// `path` behind the npm wrapper `shim` names, as the arguments that run the real program.
fn shim(name: &str, path: &Path, machine: &Machine) -> Option<Vec<String>> {
    match name {
        "codex-vendor" => codex_vendor(path).map(|real| vec![display(&real)]),
        "node-script" => npm_script(path).map(|script| {
            let node = find_in_path_in("node", machine.path.as_deref());
            vec![display(&node), display(&script)]
        }),
        _ => None,
    }
}

/// The real `codex` behind an npm wrapper. npm installs `codex` as a Node script
/// (`codex.cmd` / `codex.ps1` on Windows) that starts the vendor binary as a child; killing
/// the script would leave that child running, so the binary is started directly.
fn codex_vendor(found: &Path) -> Option<PathBuf> {
    let ext = found
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_lowercase);
    let package = if matches!(ext.as_deref(), Some("cmd" | "ps1" | "bat")) {
        found
            .parent()?
            .join("node_modules")
            .join("@openai")
            .join("codex")
    } else {
        let real = std::fs::canonicalize(found).ok()?;
        if real.file_name()? != "codex.js" {
            return None;
        }
        real.parent()?.parent()?.to_path_buf()
    };
    let exe = if cfg!(windows) { "codex.exe" } else { "codex" };
    let mut vendors = vec![package.join("vendor")];
    // Platform packages: nested under the package, or hoisted next to it.
    for scope in [
        package.join("node_modules").join("@openai"),
        package.parent()?.to_path_buf(),
    ] {
        let Ok(entries) = std::fs::read_dir(&scope) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("codex-") {
                vendors.push(entry.path().join("vendor"));
            }
        }
    }
    for vendor in vendors {
        let Ok(triples) = std::fs::read_dir(&vendor) else {
            continue;
        };
        for triple in triples.flatten() {
            for dir in ["bin", "codex"] {
                let candidate = triple.path().join(dir).join(exe);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// The script behind npm's `@tencent-ai/codebuddy-code` shim. Run through `node` directly,
/// the arguments skip `cmd.exe`'s quoting, which the JSON of `--mcp-config` does not survive.
fn npm_script(shim: &Path) -> Option<PathBuf> {
    let is_cmd = shim
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd"));
    let script = shim
        .parent()?
        .join("node_modules")
        .join("@tencent-ai")
        .join("codebuddy-code")
        .join("bin")
        .join("codebuddy");
    (is_cmd && script.is_file()).then_some(script)
}

/// The files `pattern` names: `~`, `%VAR%`, `$VAR` and `*` (one directory level) expanded.
/// Empty when a variable in it is not set: the pattern names nothing on this system.
fn expand(pattern: &str, machine: &Machine) -> Vec<PathBuf> {
    let Some(text) = expand_vars(pattern, machine) else {
        return Vec::new();
    };
    let mut paths = vec![PathBuf::new()];
    for component in Path::new(&text).components() {
        let part = component.as_os_str();
        if part == "*" {
            let mut next = Vec::new();
            for base in &paths {
                let dir = if base.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    base.as_path()
                };
                let Ok(entries) = std::fs::read_dir(dir) else {
                    continue;
                };
                let mut found: Vec<PathBuf> = entries
                    .flatten()
                    .map(|entry| base.join(entry.file_name()))
                    .collect();
                found.sort();
                next.extend(found);
            }
            paths = next;
        } else {
            for path in &mut paths {
                path.push(part);
            }
        }
    }
    if cfg!(windows) {
        // `~/.local/bin/claude` names `claude.exe` there.
        paths = paths
            .into_iter()
            .flat_map(|path| {
                if path.extension().is_none() {
                    vec![path.with_extension("exe"), path]
                } else {
                    vec![path]
                }
            })
            .collect();
    }
    paths
}

fn first_file(pattern: &str, machine: &Machine) -> Option<PathBuf> {
    expand(pattern, machine)
        .into_iter()
        .find(|path| path.is_file())
}

/// `~`, `%VAR%` and `$VAR` (or `${VAR}`) replaced; `None` when one of them is not set.
fn expand_vars(text: &str, machine: &Machine) -> Option<String> {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '~' if out.is_empty() => out.push_str(&display(&machine.home()?)),
            '%' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('%') => break,
                        Some(c) => name.push(c),
                        None => return None,
                    }
                }
                out.push_str(&machine.var(&name)?);
            }
            '$' => {
                let braced = chars.peek() == Some(&'{');
                let mut name = String::new();
                if braced {
                    chars.next();
                    for c in chars.by_ref() {
                        if c == '}' {
                            break;
                        }
                        name.push(c);
                    }
                } else {
                    while let Some(&c) = chars.peek() {
                        if c.is_alphanumeric() || c == '_' {
                            name.push(c);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                }
                if name.is_empty() {
                    out.push('$');
                    continue;
                }
                out.push_str(&machine.var(&name)?);
            }
            c => out.push(c),
        }
    }
    Some(out)
}

/// The first version-shaped word in `text`: `2.1.304` in `2.1.304 (Claude Code)`,
/// `0.158.0` in `codex-cli 0.158.0`.
fn parse_version(text: &str) -> Option<String> {
    text.split_whitespace().find_map(|word| {
        let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.');
        let digits = word.strip_prefix('v').unwrap_or(word);
        let mut parts = digits.split('.');
        let major = parts.next()?;
        if major.is_empty() || !major.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let rest: Vec<&str> = parts.collect();
        if rest.is_empty() {
            return None;
        }
        let ok = rest
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
        ok.then(|| digits.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(dir: &Path) -> Machine {
        Machine {
            path: Some(vec![dir.to_path_buf()]),
            vars: Some(BTreeMap::new()),
            home: Some(dir.to_path_buf()),
        }
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    /// A stand-in CLI that answers `--version` with `version`.
    #[cfg(windows)]
    fn version_script(path: &Path, version: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("@echo off\r\necho {version}\r\n")).unwrap();
    }

    #[cfg(not(windows))]
    fn version_script(path: &Path, version: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("#!/bin/sh\necho {version}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn set_modified(path: &Path, when: SystemTime) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(when))
            .unwrap();
    }

    #[test]
    fn a_command_on_path_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let shim = if cfg!(windows) {
            dir.path().join("claude.cmd")
        } else {
            dir.path().join("claude")
        };
        touch(&shim);
        let locator: Locator = toml::from_str(
            r#"
            commands = ["claude"]
            paths = ["~/.local/bin/claude"]
            "#,
        )
        .unwrap();
        let found = locator.locate_in(&machine(dir.path())).unwrap();
        assert_eq!(found.source, CliSource::Path);
        assert_eq!(found.path, shim);
        assert_eq!(found.argv, [display(&shim)]);
    }

    #[test]
    fn the_vendors_native_install_comes_after_path() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir
            .path()
            .join(".local")
            .join("bin")
            .join(if cfg!(windows) {
                "claude.exe"
            } else {
                "claude"
            });
        touch(&exe);
        let locator: Locator = toml::from_str(
            r#"
            commands = ["claude"]
            paths = ["~/.local/bin/claude"]
            "#,
        )
        .unwrap();
        // Not on PATH: the vendor's own install is next.
        let machine = Machine {
            path: Some(vec![dir.path().join("empty")]),
            vars: Some(BTreeMap::new()),
            home: Some(dir.path().to_path_buf()),
        };
        let found = locator.locate_in(&machine).unwrap();
        assert_eq!(found.source, CliSource::Native);
        assert_eq!(found.path, exe);
    }

    #[test]
    fn the_newest_bundled_copy_wins() {
        let dir = tempfile::tempdir().unwrap();
        let older = dir
            .path()
            .join("Claude-Data")
            .join("app")
            .join("claude-code");
        let older = older.join("1.0.0").join("build");
        let newer = dir
            .path()
            .join("Claude-Data")
            .join("app2")
            .join("claude-code");
        let newer = newer.join("2.0.0").join("build");
        touch(&older.join("claude.exe"));
        std::thread::sleep(Duration::from_millis(20));
        touch(&newer.join("claude.exe"));
        let locator: Locator = toml::from_str(
            r#"
            commands = ["claude"]
            bundled = ["%LOCALAPPDATA%/Claude-Data/*/claude-code/*/*/claude.exe"]
            "#,
        )
        .unwrap();
        let machine = Machine {
            path: Some(Vec::new()),
            vars: Some(BTreeMap::from([(
                "LOCALAPPDATA".to_owned(),
                dir.path().display().to_string(),
            )])),
            home: Some(dir.path().to_path_buf()),
        };
        let found = locator.locate_in(&machine).unwrap();
        assert_eq!(found.source, CliSource::Bundled);
        assert_eq!(found.path, newer.join("claude.exe"));
    }

    #[test]
    fn an_npm_wrapper_runs_its_script_on_node() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir
            .path()
            .join("node_modules")
            .join("@tencent-ai")
            .join("codebuddy-code")
            .join("bin");
        touch(&bin.join("codebuddy"));
        let shim = dir.path().join("codebuddy.cmd");
        touch(&shim);
        let node = dir
            .path()
            .join(if cfg!(windows) { "node.exe" } else { "node" });
        touch(&node);
        let locator: Locator = toml::from_str(
            r#"
            commands = ["codebuddy"]
            shim = "node-script"
            "#,
        )
        .unwrap();
        let found = locator.locate_in(&machine(dir.path())).unwrap();
        assert_eq!(found.path, shim);
        assert_eq!(
            found.argv,
            [display(&node), display(&bin.join("codebuddy"))]
        );
    }

    #[test]
    fn a_codex_wrapper_is_replaced_by_its_vendor_binary() {
        let dir = tempfile::tempdir().unwrap();
        let exe = if cfg!(windows) { "codex.exe" } else { "codex" };
        let native = dir
            .path()
            .join("node_modules")
            .join("@openai")
            .join("codex")
            .join("node_modules")
            .join("@openai")
            .join("codex-test-x64")
            .join("vendor")
            .join("triple")
            .join("bin");
        touch(&native.join(exe));
        let shim = if cfg!(windows) {
            dir.path().join("codex.cmd")
        } else {
            dir.path().join("codex")
        };
        touch(&shim);
        let locator: Locator = toml::from_str(
            r#"
            commands = ["codex"]
            shim = "codex-vendor"
            "#,
        )
        .unwrap();
        let found = locator.locate_in(&machine(dir.path())).unwrap();
        assert_eq!(found.argv, [display(&native.join(exe))]);
    }

    #[test]
    fn the_environment_variable_comes_first() {
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join("custom/codebuddy");
        touch(&cli);
        let on_path = dir.path().join("codebuddy");
        touch(&on_path);
        let locator: Locator = toml::from_str(
            r#"
            env = "CODEBUDDY_CLI"
            commands = ["codebuddy"]
            "#,
        )
        .unwrap();
        let mut machine = machine(dir.path());
        machine.vars = Some(BTreeMap::from([(
            "CODEBUDDY_CLI".to_owned(),
            display(&cli),
        )]));
        let found = locator.locate_in(&machine).unwrap();
        assert_eq!(found.source, CliSource::Env);
        assert_eq!(found.path, cli);
    }

    #[test]
    fn a_configured_command_wins_over_the_description() {
        let dir = tempfile::tempdir().unwrap();
        let configured = dir.path().join("my-claude");
        touch(&configured);
        let locator: Locator = toml::from_str(
            r#"
            commands = ["claude"]
            paths = ["~/.local/bin/claude"]
            "#,
        )
        .unwrap();
        let cli = Cli::new(
            vec![display(&configured), "--flag".into()],
            locator,
            Install::default(),
        );
        assert!(cli.installed());
        assert_eq!(cli.source(), Some(CliSource::Config));
        assert_eq!(
            cli.command().unwrap(),
            [display(&configured), "--flag".into()]
        );
    }

    #[test]
    fn nothing_found_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let locator: Locator = toml::from_str(
            r#"
            env = "CROSSAGENTS_NO_SUCH_CLI"
            commands = ["crossagents-no-such-cli"]
            paths = ["~/.local/bin/crossagents-no-such-cli"]
            version = ["--version"]
            "#,
        )
        .unwrap();
        let machine = Machine {
            path: Some(Vec::new()),
            vars: Some(BTreeMap::new()),
            home: Some(dir.path().to_path_buf()),
        };
        assert!(locator.locate_in(&machine).is_none());
        let cli = Cli::new(Vec::new(), locator, Install::default());
        assert!(!cli.installed());
        assert_eq!(cli.command(), None);
    }

    /// A CLI installed after the hub started is still found: a miss is not remembered.
    #[test]
    fn a_cli_installed_while_the_hub_runs_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let locator: Locator = toml::from_str(r#"commands = ["claude"]"#).unwrap();
        let cli = Cli::new(Vec::new(), locator, Install::default());
        let machine = machine(dir.path());
        assert!(cli.found_in(&machine).is_none());
        let shim = dir.path().join(if cfg!(windows) {
            "claude.cmd"
        } else {
            "claude"
        });
        touch(&shim);
        let found = cli.found_in(&machine).expect("found once installed");
        assert_eq!(found.path, shim);
        assert_eq!(found.source, CliSource::Path);
    }

    /// A desktop app that updates removes the build directory of the build it replaces: the
    /// hit is checked before it is used, and the build now there is found instead.
    #[test]
    fn a_removed_build_is_looked_up_again() {
        let dir = tempfile::tempdir().unwrap();
        let build = |app: &str, version: &str| {
            dir.path()
                .join("Claude-Data")
                .join(app)
                .join("claude-code")
                .join(version)
                .join("build")
                .join("claude.exe")
        };
        let older = build("app-1.0.0", "1.0.0");
        touch(&older);
        let locator: Locator = toml::from_str(
            r#"
            commands = ["claude"]
            bundled = ["%LOCALAPPDATA%/Claude-Data/*/claude-code/*/*/claude.exe"]
            "#,
        )
        .unwrap();
        let machine = Machine {
            path: Some(Vec::new()),
            vars: Some(BTreeMap::from([(
                "LOCALAPPDATA".to_owned(),
                dir.path().display().to_string(),
            )])),
            home: Some(dir.path().to_path_buf()),
        };
        let cli = Cli::new(Vec::new(), locator, Install::default());
        assert_eq!(cli.found_in(&machine).unwrap().path, older);
        // The update removes that build and brings a newer one.
        std::fs::remove_dir_all(dir.path().join("Claude-Data").join("app-1.0.0")).unwrap();
        let newer = build("app-2.0.0", "2.0.0");
        touch(&newer);
        assert_eq!(cli.found_in(&machine).unwrap().path, newer);
    }

    /// The version is kept with the file it came from: a new modification time (the CLI was
    /// updated in place) is asked again, an unchanged one is not.
    #[tokio::test]
    async fn the_version_is_asked_again_when_the_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join(if cfg!(windows) {
            "claude.cmd"
        } else {
            "claude"
        });
        version_script(&shim, "1.0.0");
        let locator: Locator = toml::from_str(
            r#"
            commands = ["claude"]
            version = ["--version"]
            "#,
        )
        .unwrap();
        let cli = Cli::new(Vec::new(), locator, Install::default());
        let machine = machine(dir.path());
        let installed_at = std::fs::metadata(&shim).unwrap().modified().unwrap();
        assert_eq!(cli.version_in(&machine).await.as_deref(), Some("1.0.0"));
        // Rewritten, but with the modification time it had: the answer still stands.
        version_script(&shim, "2.0.0");
        set_modified(&shim, installed_at);
        assert_eq!(cli.version_in(&machine).await.as_deref(), Some("1.0.0"));
        // Updated: a newer modification time, so it is asked again.
        set_modified(&shim, installed_at + Duration::from_secs(60));
        assert_eq!(cli.version_in(&machine).await.as_deref(), Some("2.0.0"));
    }

    #[test]
    fn settings_can_be_given_per_system() {
        let locator: Locator = toml::from_str(
            r#"
            [bundled]
            windows = ["%LOCALAPPDATA%/OpenAI/Codex/bin/*/codex.exe"]
            macos = ["/Applications/Codex.app/Contents/Resources/codex"]
            "#,
        )
        .unwrap();
        assert!(locator.bundled.get_for("windows").is_some());
        assert!(locator.bundled.get_for("macos").is_some());
        assert_eq!(locator.bundled.get_for("linux"), None);
    }

    #[test]
    fn the_missing_message_is_built_from_the_description() {
        let install: Install = toml::from_str(
            r#"
            label = "CodeBuddy"
            hint = "npm install -g @tencent-ai/codebuddy-code"
            login = "then sign in once with `codebuddy` and `/login` (or set CODEBUDDY_API_KEY)"
            notes = "The copy inside WorkBuddy cannot run outside the WorkBuddy app."
            "#,
        )
        .unwrap();
        assert_eq!(
            install.missing_message("codebuddy").unwrap(),
            "the CodeBuddy CLI is not installed: run `npm install -g @tencent-ai/codebuddy-code`, \
             then sign in once with `codebuddy` and `/login` (or set CODEBUDDY_API_KEY). \
             The copy inside WorkBuddy cannot run outside the WorkBuddy app."
        );
        assert_eq!(Install::default().missing_message("codex"), None);
    }

    #[test]
    fn versions_are_read_from_what_the_cli_prints() {
        assert_eq!(
            parse_version("2.1.304 (Claude Code)").as_deref(),
            Some("2.1.304")
        );
        assert_eq!(
            parse_version("codex-cli 0.158.0\n").as_deref(),
            Some("0.158.0")
        );
        assert_eq!(parse_version("v1.2.3").as_deref(), Some("1.2.3"));
        assert_eq!(parse_version("no version here"), None);
        assert_eq!(parse_version(""), None);
    }
}
