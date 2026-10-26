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
use std::time::{Duration, Instant, SystemTime};

use crossagents_core::{CliSource, CliStatus, Located};
use serde::{Deserialize, Deserializer};
use tokio::process::Command;

use crate::process::{find_in_path_in, hide_console};

/// How long a CLI gets to print its version.
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a failed version probe stands before it is tried again. A CLI that could not
/// answer just now (it was being installed, or its runtime was busy) may answer later; a
/// successful answer is kept with the file it came from and needs no retry.
const VERSION_RETRY: Duration = Duration::from_secs(60);

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

/// One way to install a CLI (docs/DESIGN.md §9.3): `[[install.methods]]`.
///
/// `run` and `upgrade` are argv, never a shell line: the hub starts the program itself.
/// When a pipe is needed, the interpreter that reads it is written out (`bash -c`,
/// `powershell -Command`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct InstallMethod {
    /// Names the way it installs (`native`, `npm`); also what `installed_via` reports.
    pub id: String,
    /// Systems it is for (`windows`, `macos`, `linux`); empty means all of them.
    pub os: Vec<String>,
    /// Programs that have to be on `PATH` for this method to be usable (`npm`).
    pub requires: Vec<String>,
    /// The command that installs the CLI, as argv.
    pub run: Vec<String>,
    /// The command that upgrades what this method installed; without one, `run` again.
    pub upgrade: Option<Vec<String>>,
}

impl InstallMethod {
    /// Whether this method is for this system and has what it needs.
    pub fn usable(&self, machine: &Machine) -> bool {
        let this_os = self.os.is_empty() || self.os.iter().any(|os| os == os_name());
        if !this_os {
            return false;
        }
        if self.requires.is_empty() {
            return true;
        }
        let dirs = machine.path_dirs();
        self.requires
            .iter()
            .all(|name| find_in_path_in(name, Some(&dirs)).is_file())
    }

    /// The command that upgrades a CLI this method installed.
    pub fn upgrade_command(&self) -> Vec<String> {
        self.upgrade.clone().unwrap_or_else(|| self.run.clone())
    }

    /// The command as one line to copy, quoting what has spaces in it.
    pub fn line(&self) -> String {
        argv_line(&self.run)
    }
}

/// `argv` as a line a person can paste into a terminal.
pub fn argv_line(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| {
            if arg.chars().any(char::is_whitespace) {
                format!("\"{arg}\"")
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// What `[install]` says: shown to the person, and run by the hub only when the person
/// presses install or upgrade (docs/DESIGN.md §9.3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Install {
    /// The command that installs the CLI, e.g. `npm install -g @tencent-ai/codebuddy-code`.
    /// Kept as a fallback for descriptions written before `methods`: what one of those says
    /// is shown, never run.
    pub hint: Option<String>,
    pub docs: Option<String>,
    /// What to do after installing (sign in), part of the "not installed" message.
    pub login: Option<String>,
    /// Why another app's copy does not do, part of that message.
    pub notes: Option<String>,
    /// What the CLI is called in that message (`CodeBuddy`); the plugin's name by default.
    pub label: Option<String>,
    /// How to install it, in the order to try them: the first one this machine can use wins.
    pub methods: Vec<InstallMethod>,
}

impl Install {
    pub fn is_empty(&self) -> bool {
        self.hint.is_none()
            && self.docs.is_none()
            && self.login.is_none()
            && self.notes.is_none()
            && self.label.is_none()
            && self.methods.is_empty()
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
        if !other.methods.is_empty() {
            self.methods = other.methods.clone();
        }
    }

    /// The first method this machine can use.
    pub fn method_for(&self, machine: &Machine) -> Option<&InstallMethod> {
        self.methods.iter().find(|method| method.usable(machine))
    }

    /// The method that put the file at `path` there, when its path says which one did: npm
    /// installs under `node_modules` (or a `.cmd` shell), the vendors' own installers beside
    /// the user's programs. The table is here, keyed by method id, rather than regular
    /// expressions in the description: a path is a fact about npm, not about one vendor.
    ///
    /// A description with one usable method needs no guessing: that is the method.
    pub fn method_of(&self, path: &Path, machine: &Machine) -> Option<&InstallMethod> {
        let mut usable = self.methods.iter().filter(|method| method.usable(machine));
        let first = usable.next()?;
        let mut rest = usable.peekable();
        if rest.peek().is_none() {
            return Some(first);
        }
        std::iter::once(first)
            .chain(rest)
            .find(|method| path_says(&method.id, path))
    }

    /// The command that installs it here, as one line; the old `hint` when the description
    /// names no usable method, so a "not installed" message still says something.
    pub fn hint_for(&self, machine: &Machine) -> Option<String> {
        self.method_for(machine)
            .map(InstallMethod::line)
            .or_else(|| self.hint.clone())
            .or_else(|| self.methods.first().map(InstallMethod::line))
    }

    /// The command that installs it here, as argv.
    pub fn install_command(&self, machine: &Machine) -> Option<Vec<String>> {
        self.method_for(machine).map(|method| method.run.clone())
    }

    /// The command that upgrades it here, and whether upgrading is this description's to do.
    ///
    /// A copy another app keeps is the desktop app's to update, and a program the person
    /// named themselves (`config` / `env`) is theirs: neither is touched. What an install
    /// method put there is upgraded by that method.
    pub fn upgrade_for(
        &self,
        source: CliSource,
        path: &Path,
        machine: &Machine,
    ) -> Option<Vec<String>> {
        match source {
            CliSource::Config | CliSource::Env | CliSource::Bundled => None,
            CliSource::Path | CliSource::Native => self
                .method_of(path, machine)
                .map(InstallMethod::upgrade_command),
        }
    }

    /// What to say when a session is asked of a CLI this machine does not have: what to
    /// install, how to sign in, and why another app's copy is not used.
    pub fn missing_message(&self, name: &str, machine: &Machine) -> Option<String> {
        let hint = self.hint_for(machine)?;
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

/// Whether a file at `path` was put there by the method called `id`.
fn path_says(id: &str, path: &Path) -> bool {
    let text = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    match id {
        // `npm install -g` writes into <prefix>/node_modules/<scope>/<package>.
        "npm" => text.contains("/node_modules/"),
        // The vendors' own installers write beside the user's programs.
        "native" => !text.contains("/node_modules/"),
        _ => false,
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
    /// The directories the registry adds beyond `path`; `None` reads them on Windows, and a
    /// test that must not touch the real registry gives `Some(Vec::new())`.
    pub registry_path: Option<Vec<PathBuf>>,
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

    /// Where a program is looked up: this process's `PATH`, then what the registry says the
    /// `PATH` is now.
    ///
    /// A hub runs for a long time. A CLI installed while it runs lands in a directory that
    /// was not in the `PATH` the hub inherited at start (the installer adds it to the user's
    /// environment, which this process never sees again), so the registry is read as well.
    /// Only the lookup is affected: the hub's own environment is left alone, and sessions
    /// start the program by the absolute path that was found.
    fn path_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = match &self.path {
            Some(dirs) => dirs.clone(),
            None => std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).collect())
                .unwrap_or_default(),
        };
        let extra = match &self.registry_path {
            Some(dirs) => dirs.clone(),
            None => registry_path().unwrap_or_default(),
        };
        for dir in extra {
            if !dirs.iter().any(|have| same_dir(have, &dir)) {
                dirs.push(dir);
            }
        }
        dirs
    }
}

/// Whether two directories name the same place, for the merge above (Windows ignores case).
fn same_dir(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    } else {
        a == b
    }
}

/// How long what the registry says `PATH` is stays good: lookups come in bursts (every
/// plugin, several times per `GET /v1/plugins`), and each read starts two `reg` processes.
#[cfg(windows)]
const REGISTRY_PATH_TTL: Duration = Duration::from_secs(30);

/// `PATH` as Windows now has it (the user's, then the machine's), or `None` where there is
/// no registry to read.
fn registry_path() -> Option<Vec<PathBuf>> {
    #[cfg(windows)]
    {
        type Cached = Option<(Instant, Option<Vec<PathBuf>>)>;
        static CACHE: std::sync::Mutex<Cached> = std::sync::Mutex::new(None);
        let mut cache = CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((at, dirs)) = cache.as_ref()
            && at.elapsed() < REGISTRY_PATH_TTL
        {
            return dirs.clone();
        }
        let user = reg_path(r"HKCU\Environment");
        let machine =
            reg_path(r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment");
        let dirs = (user.is_some() || machine.is_some()).then(|| {
            registry_dirs(user.as_deref(), machine.as_deref(), &|name| {
                std::env::var(name).ok()
            })
        });
        *cache = Some((Instant::now(), dirs.clone()));
        dirs
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// `Path` under `key` as `reg query` prints it.
#[cfg(windows)]
fn reg_path(key: &str) -> Option<String> {
    use crossagents_core::process::NoWindow;
    // The hub runs without a console: without this every lookup flashes a window.
    let output = std::process::Command::new("reg")
        .no_window()
        .args(["query", key, "/v", "Path"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_reg_path(&String::from_utf8_lossy(&output.stdout))
}

/// The value in `reg query`'s output: `    Path    REG_EXPAND_SZ    C:\a;%USERPROFILE%\b`.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_reg_path(reg: &str) -> Option<String> {
    reg.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        if !words.next()?.eq_ignore_ascii_case("Path") {
            return None;
        }
        let kind = words.next()?;
        if !kind.starts_with("REG_") {
            return None;
        }
        // Everything after the type, which may hold spaces of its own.
        let at = line.find(kind)? + kind.len();
        Some(line[at..].trim().to_owned())
    })
}

/// The directories the user's and the machine's `PATH` name: `%VAR%` expanded (a name that
/// is not set contributes nothing, as Windows does), in order, without repeats.
fn registry_dirs(
    user: Option<&str>,
    machine: Option<&str>,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for value in [user, machine].into_iter().flatten() {
        for part in expand_path_text(value, lookup).split(';') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let dir = PathBuf::from(part.trim_matches('"'));
            if !dirs.iter().any(|have| same_dir(have, &dir)) {
                dirs.push(dir);
            }
        }
    }
    dirs
}

/// `text` with `%VAR%` replaced; a variable that is not set becomes nothing.
fn expand_path_text(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut name = String::new();
        let mut closed = false;
        for c in chars.by_ref() {
            if c == '%' {
                closed = true;
                break;
            }
            name.push(c);
        }
        if !closed {
            out.push('%');
            out.push_str(&name);
            break;
        }
        out.push_str(&lookup(&name).unwrap_or_default());
    }
    out
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
        let dirs = machine.path_dirs();
        for name in self.commands.get().into_iter().flatten() {
            let path = find_in_path_in(name, Some(&dirs));
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
        let on_path = find_in_path_in(program, Some(&machine.path_dirs()));
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

    /// Everywhere this description looked on `machine`, in the order it looked: what a
    /// failure report lists when a successful command left no usable program behind.
    pub fn searched(&self, machine: &Machine) -> Vec<String> {
        let mut places = Vec::new();
        if let Some(var) = self.env.get() {
            places.push(format!("${var}"));
        }
        let dirs = machine.path_dirs();
        for name in self.commands.get().into_iter().flatten() {
            let found = find_in_path_in(name, Some(&dirs));
            places.push(format!("PATH: {name} ({})", found.display()));
        }
        for pattern in self.paths.get().into_iter().flatten() {
            places.push(match first_file(pattern, machine) {
                Some(path) => path.display().to_string(),
                None => format!("{} (not there)", pattern),
            });
        }
        for pattern in self.bundled.get().into_iter().flatten() {
            places.push(format!("{pattern} (a copy another app keeps)"));
        }
        places
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
    /// Where the description came from, reported with the status.
    described_by: Option<String>,
    cache: std::sync::Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    /// The last hit, reused while `path` is still a file (never for a copy another app
    /// keeps: a desktop app that updates leaves the old build in place).
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
    /// When a failed probe was made; `None` when it answered.
    failed_at: Option<Instant>,
}

impl Cli {
    pub fn new(configured: Vec<String>, locator: Locator, install: Install) -> Self {
        Self {
            inner: std::sync::Arc::new(CliInner {
                configured,
                locator,
                install,
                described_by: None,
                cache: std::sync::Mutex::default(),
            }),
        }
    }

    /// Where the description that says all this came from (`built-in`, a manifest's path).
    pub fn with_description(mut self, described_by: Option<String>) -> Self {
        std::sync::Arc::get_mut(&mut self.inner)
            .expect("a fresh Cli has no other owner")
            .described_by = described_by;
        self
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
            // A copy another app keeps is scanned again every time: a desktop app that
            // updates brings a newer build than the one remembered, and a CLI installed on
            // `PATH` afterwards should win over it.
            && found.source != CliSource::Bundled
        {
            return Some(found.clone());
        }
        // Not found last time, or what was found is gone: look again.
        let found = self.find_in(machine);
        cache.found = found.clone();
        found
    }

    /// Drops what the last lookup and the version cache remember: after an install or an
    /// upgrade the next lookup looks again, and the version is asked for the new file.
    pub fn forget(&self) {
        let mut cache = self.cache();
        cache.found = None;
        cache.version = None;
        cache.probing = None;
    }

    /// Looks again, cache aside (after an install or an upgrade).
    pub fn relocate(&self) -> Option<Located> {
        self.forget();
        self.found()
    }

    /// Where the description would look on this machine, for a failure report: the
    /// variable, the names on `PATH`, the paths tried, the copies another app keeps.
    pub fn searched(&self) -> Vec<String> {
        self.inner.locator.searched(&Machine::default())
    }

    /// Whether the description can install its CLI on this machine, and with what.
    pub fn install_command(&self) -> Option<Vec<String>> {
        self.inner.install.install_command(&Machine::default())
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
        self.inner
            .install
            .missing_message(name, &Machine::default())
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
        let install = &self.inner.install;
        // What upgrading would run, or nothing when it is not this description's to do.
        let upgrade_command = located
            .as_ref()
            .and_then(|found| install.upgrade_for(found.source, &found.path, machine));
        CliStatus {
            installed_via: located.as_ref().and_then(|found| {
                install
                    .method_of(&found.path, machine)
                    .map(|method| method.id.clone())
            }),
            install_command: install.install_command(machine),
            can_upgrade: upgrade_command.is_some(),
            upgrade_command,
            described_by: self.inner.described_by.clone(),
            located,
            install_hint: install.hint_for(machine),
            docs: install.docs.clone(),
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
    /// (path, modification time) yet, or when a failed answer is old enough to ask again.
    ///
    /// A success is kept. A failure is not kept for good: a CLI that could not answer while
    /// it was being installed, or whose runtime was busy, may answer a minute later.
    fn known_version(&self, found: &Located) -> Option<Option<String>> {
        let cache = self.cache();
        let entry = cache.version.as_ref()?;
        let modified = modified(&found.path)?;
        if entry.path != found.path || entry.modified != modified {
            return None;
        }
        if entry.value.is_some() {
            return Some(entry.value.clone());
        }
        let failed_at = entry.failed_at?;
        (failed_at.elapsed() < VERSION_RETRY).then_some(None)
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
                failed_at: value.is_none().then(Instant::now),
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
            let node = find_in_path_in("node", Some(&machine.path_dirs()));
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
            // The real registry is never read in a test.
            registry_path: Some(Vec::new()),
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
            registry_path: Some(Vec::new()),
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
            registry_path: Some(Vec::new()),
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
            registry_path: Some(Vec::new()),
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
            registry_path: Some(Vec::new()),
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
            install
                .missing_message("codebuddy", &machine(Path::new("/no/such/dir")))
                .unwrap(),
            "the CodeBuddy CLI is not installed: run `npm install -g @tencent-ai/codebuddy-code`, \
             then sign in once with `codebuddy` and `/login` (or set CODEBUDDY_API_KEY). \
             The copy inside WorkBuddy cannot run outside the WorkBuddy app."
        );
        assert_eq!(
            Install::default().missing_message("codex", &Machine::default()),
            None
        );
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

    /// The first method this machine can use wins: its `os` has to match and everything it
    /// `requires` has to be on `PATH`.
    #[test]
    fn the_first_usable_install_method_wins() {
        let dir = tempfile::tempdir().unwrap();
        let install: Install = toml::from_str(
            r#"
            [[methods]]
            id = "npm"
            requires = ["crossagents-no-such-tool"]
            run = ["npm", "install", "-g", "demo"]

            [[methods]]
            id = "native"
            run = ["demo-install"]
            "#,
        )
        .unwrap();
        let m = machine(dir.path());
        // The first needs something this machine does not have.
        assert_eq!(
            install.method_for(&m).map(|method| method.id.as_str()),
            Some("native")
        );

        // Nothing usable: the old hint still says what to copy.
        let mut only_missing: Install = toml::from_str(
            r#"
            [[methods]]
            id = "npm"
            requires = ["crossagents-no-such-tool"]
            run = ["npm", "install", "-g", "demo"]
            "#,
        )
        .unwrap();
        assert!(only_missing.method_for(&m).is_none());
        only_missing.hint = Some("npm install -g demo".into());
        assert_eq!(
            only_missing.hint_for(&m).as_deref(),
            Some("npm install -g demo")
        );
    }

    #[test]
    fn a_method_for_another_system_is_not_used() {
        let dir = tempfile::tempdir().unwrap();
        let other = if cfg!(windows) { "macos" } else { "windows" };
        let install: Install = toml::from_str(&format!(
            "[[methods]]\nid = \"native\"\nos = [\"{other}\"]\nrun = [\"demo-install\"]\n"
        ))
        .unwrap();
        assert!(install.method_for(&machine(dir.path())).is_none());
    }

    /// Where a file is says which method put it there: npm writes under `node_modules`,
    /// the vendors' own installers beside the user's programs.
    #[test]
    fn only_what_a_method_installed_can_be_upgraded() {
        let dir = tempfile::tempdir().unwrap();
        let install: Install = toml::from_str(
            r#"
            [[methods]]
            id = "npm"
            run = ["npm", "install", "-g", "demo"]
            upgrade = ["npm", "install", "-g", "demo@latest"]

            [[methods]]
            id = "native"
            run = ["demo-install"]
            upgrade = ["demo", "update"]
            "#,
        )
        .unwrap();
        let m = machine(dir.path());
        let npm = dir.path().join("node_modules").join("@demo").join("cli");
        let native = dir.path().join(".local").join("bin").join("demo");

        assert_eq!(
            install.upgrade_for(CliSource::Path, &npm, &m),
            Some(vec![
                "npm".into(),
                "install".into(),
                "-g".into(),
                "demo@latest".into()
            ])
        );
        assert_eq!(
            install.upgrade_for(CliSource::Path, &native, &m),
            Some(vec!["demo".into(), "update".into()])
        );
        // A copy another app keeps is that app's to update; a configured program is the
        // person's. Neither is touched.
        assert_eq!(install.upgrade_for(CliSource::Bundled, &npm, &m), None);
        assert_eq!(install.upgrade_for(CliSource::Config, &npm, &m), None);
        assert_eq!(install.upgrade_for(CliSource::Env, &npm, &m), None);
    }

    /// A desktop app that updates brings a newer build while the old one stays: the copy is
    /// scanned again every time, so the newest one is used without a restart.
    #[test]
    fn a_bundled_copy_is_scanned_again_every_time() {
        let dir = tempfile::tempdir().unwrap();
        let build = |app: &str| {
            dir.path()
                .join("Claude-Data")
                .join(app)
                .join("claude-code")
                .join("1.0.0")
                .join("build")
                .join("claude.exe")
        };
        let older = build("app-1.0.0");
        touch(&older);
        let locator: Locator = toml::from_str(
            r#"
            commands = ["claude"]
            bundled = ["%LOCALAPPDATA%/Claude-Data/*/claude-code/*/*/claude.exe"]
            "#,
        )
        .unwrap();
        let m = Machine {
            path: Some(Vec::new()),
            vars: Some(BTreeMap::from([(
                "LOCALAPPDATA".to_owned(),
                dir.path().display().to_string(),
            )])),
            home: Some(dir.path().to_path_buf()),
            registry_path: Some(Vec::new()),
        };
        let cli = Cli::new(Vec::new(), locator, Install::default());
        assert_eq!(cli.found_in(&m).unwrap().path, older);
        // The old build is still there, but a newer one arrived.
        std::thread::sleep(Duration::from_millis(20));
        let newer = build("app-2.0.0");
        touch(&newer);
        assert_eq!(cli.found_in(&m).unwrap().path, newer);
    }

    /// A failed version probe is not remembered for good: a CLI that could not answer while
    /// it was being installed is asked again a minute later.
    #[test]
    fn a_failed_version_probe_is_tried_again_after_a_minute() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join(if cfg!(windows) {
            "claude.cmd"
        } else {
            "claude"
        });
        touch(&shim);
        let locator: Locator = toml::from_str(r#"commands = ["claude"]"#).unwrap();
        let cli = Cli::new(Vec::new(), locator, Install::default());
        let found = cli.found_in(&machine(dir.path())).unwrap();
        let modified = modified(&found.path).unwrap();

        let fail_at = |when: Instant| VersionEntry {
            path: found.path.clone(),
            modified,
            value: None,
            failed_at: Some(when),
        };
        cli.cache().version = Some(fail_at(Instant::now()));
        assert_eq!(cli.known_version(&found), Some(None), "asked, no answer");
        cli.cache().version = Some(fail_at(
            Instant::now() - VERSION_RETRY - Duration::from_secs(1),
        ));
        assert_eq!(cli.known_version(&found), None, "time to ask again");
        // A success is kept with the file it came from.
        cli.cache().version = Some(VersionEntry {
            path: found.path.clone(),
            modified,
            value: Some("1.2.3".into()),
            failed_at: None,
        });
        assert_eq!(cli.known_version(&found), Some(Some("1.2.3".into())));
    }

    /// The registry's `PATH` is merged with the process's: a CLI installed while the hub
    /// runs lands in a directory the hub's own `PATH` never had.
    #[test]
    fn the_lookup_sees_the_registry_path_too() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = dir.path().join("fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        let cli = fresh.join(if cfg!(windows) { "demo.exe" } else { "demo" });
        touch(&cli);
        let locator: Locator = toml::from_str(r#"commands = ["demo"]"#).unwrap();
        let m = Machine {
            path: Some(vec![dir.path().join("empty")]),
            vars: Some(BTreeMap::new()),
            home: Some(dir.path().to_path_buf()),
            registry_path: Some(vec![fresh]),
        };
        assert_eq!(locator.locate_in(&m).unwrap().path, cli);
    }

    #[test]
    fn the_registry_path_is_merged_and_expanded() {
        let vars = BTreeMap::from([("USERPROFILE".to_owned(), r"C:\Users\me".to_owned())]);
        let lookup = |name: &str| vars.get(name).cloned();
        let dirs = registry_dirs(
            Some(r"C:\one;%USERPROFILE%\bin;C:\one"),
            Some(r"C:\two;%NOT_SET%\x"),
            &lookup,
        );
        let names: Vec<String> = dirs.iter().map(|dir| dir.display().to_string()).collect();
        // User first, then machine; repeats dropped; an unset name contributes nothing.
        assert_eq!(names.len(), 4, "{names:?}");
        assert_eq!(names[0], r"C:\one");
        assert_eq!(names[1], r"C:\Users\me\bin");
        assert_eq!(names[2], r"C:\two");
        assert_eq!(names[3], r"\x");
    }

    #[test]
    fn a_registry_value_is_read_from_reg_query_output() {
        let out = "\r\nHKEY_CURRENT_USER\\Environment\r\n    \
                   Path    REG_EXPAND_SZ    %USERPROFILE%\\bin;C:\\tools\r\n\r\n";
        assert_eq!(
            parse_reg_path(out).as_deref(),
            Some("%USERPROFILE%\\bin;C:\\tools")
        );
        assert_eq!(parse_reg_path("no value here"), None);
    }
}
