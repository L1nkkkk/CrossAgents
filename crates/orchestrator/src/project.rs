//! What a project declares for the hub (docs/tasks/DOGFOOD.md §9): `crossagents.toml` at the
//! root of the checkout.
//!
//! ```toml
//! [run]
//! # Run by the hub in the run's checkout before the lead lands or finishes a run that
//! # changed something; all must pass. A check with `paths` runs only when the run changed
//! # one of them: a file, or a directory and everything under it.
//! checks = [
//!     "npm run lint",
//!     { command = "cargo test --workspace", paths = ["crates", "Cargo.toml", "Cargo.lock"] },
//! ]
//! timeout_secs = 1800   # per command; default 600
//! # Toolchain defaults (`presets.rs`): left out, every preset whose marker file is at the
//! # root; listed, only those; `[]`, none.
//! presets = ["rust"]
//! # Programs the run worktree's PATH sends through the executor (docs/tasks/LOCKS.md §4.6);
//! # left out, those the enabled presets carry (Rust: `cargo`); `[]`, none.
//! shims = ["cargo"]
//! # Run by the hub in a fresh worktree before an agent gets it, for dependencies
//! # (docs/tasks/TOOLCHAIN.md §7). `dir` is relative to the checkout; `paths` are what the
//! # worktree has to have for the command to be worth running (left out: every worktree).
//! setup = [{ command = "npm ci", dir = "apps/gui", paths = ["apps/gui/package.json"] }]
//!
//! [run.env]
//! # For agents and checks in the run's worktrees (docs/tasks/TOOLCHAIN.md §3), over a
//! # preset's; `false` drops a preset's. `{build}` is the worktree's own build directory,
//! # `{checkout}` the worktree.
//! CARGO_TARGET_DIR = "{build}/cargo"
//!
//! [resources]
//! # What a build in a run worktree holds while it runs (docs/tasks/LOCKS.md §3): the name an
//! # executor command asks for, where it is (the same placeholders as in [run.env]), and
//! # `access = "exclusive"` (the default) or `"shared"`. A `scope` other than `local` is not
//! # supported yet (§1). A resource named here replaces the preset's of that name.
//! build = { path = "{build}/cargo", access = "exclusive" }
//! ```

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::presets::{self, Preset};

pub const FILE: &str = "crossagents.toml";

/// The environment variable agents and checks in a run worktree get the resources in: a JSON
/// array of `{"name", "path", "access"}` (docs/tasks/LOCKS.md §3).
pub const RESOURCES_ENV: &str = "CROSSAGENTS_RESOURCES";

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

/// The checks every run of the project must pass; none when the project declares none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunChecks {
    pub checks: Vec<RunCheck>,
    pub timeout: Duration,
}

/// One command, and the paths that call for it (empty: every change does).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunCheck {
    pub command: String,
    pub paths: Vec<String>,
}

impl RunCheck {
    /// Whether a run that changed `changed` (paths relative to the checkout, `/`-separated)
    /// needs this check.
    pub fn applies_to(&self, changed: &[String]) -> bool {
        self.paths.is_empty()
            || changed.iter().any(|file| {
                self.paths.iter().any(|p| {
                    file == p
                        || file
                            .strip_prefix(p.as_str())
                            .is_some_and(|r| r.starts_with('/'))
                })
            })
    }
}

impl RunChecks {
    /// The commands a run that changed `changed` must pass; all of them when what changed is
    /// not known (empty), so that a failure to list the changes does not skip a check.
    pub fn commands_for(&self, changed: &[String]) -> Vec<String> {
        self.checks
            .iter()
            .filter(|c| changed.is_empty() || c.applies_to(changed))
            .map(|c| c.command.clone())
            .collect()
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    run: Run,
    #[serde(default)]
    resources: BTreeMap<String, ResourceDecl>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Run {
    #[serde(default)]
    checks: Vec<Check>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    presets: Option<Vec<String>>,
    #[serde(default)]
    shims: Option<Vec<String>>,
    #[serde(default)]
    env: BTreeMap<String, EnvValue>,
    /// Worktree setup commands (`setup.rs`, docs/tasks/TOOLCHAIN.md §7).
    #[serde(default)]
    setup: Vec<crate::setup::SetupDecl>,
}

/// One entry of `[resources]`: where the resource is (placeholders as in `[run.env]`), how it
/// is held, and whether it is this machine's.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ResourceDecl {
    path: String,
    #[serde(default)]
    access: ResourceAccess,
    #[serde(default)]
    scope: Option<String>,
}

/// How a command holds a resource: alone, or alongside others that only read it
/// (docs/tasks/LOCKS.md §3).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ResourceAccess {
    Shared,
    /// The default: a build directory, or anything else one writer owns.
    #[default]
    Exclusive,
}

/// A resource of a run worktree: its name, its path (placeholders not filled in yet), and how
/// it is held.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ResolvedResource {
    pub name: String,
    pub path: String,
    pub access: ResourceAccess,
}

/// A value in `[run.env]`: set, or `false` to drop a preset's.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(untagged)]
enum EnvValue {
    Set(String),
    Drop(bool),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Check {
    Command(String),
    Scoped(Scoped),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Scoped {
    command: String,
    paths: Vec<String>,
}

/// `./crates/` and `crates\` are `crates`.
fn normalize(path: &str) -> String {
    let path = path.trim().replace('\\', "/");
    let path = path.trim_start_matches("./");
    path.trim_matches('/').to_owned()
}

/// `dir`'s file; none is an empty one. One that does not parse is an error, so that a typo
/// does not quietly turn something off.
fn read(dir: &Path) -> Result<File, String> {
    let text = match std::fs::read_to_string(dir.join(FILE)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read {FILE}: {e}")),
    };
    toml::from_str(&text).map_err(|e| format!("{FILE} is not valid: {e}"))
}

/// The run checks `dir` declares. No file is no checks.
pub fn run_checks(dir: &Path) -> Result<RunChecks, String> {
    let file = read(dir)?;
    let mut checks = Vec::new();
    for check in file.run.checks {
        let (command, paths) = match check {
            Check::Command(command) => (command, Vec::new()),
            Check::Scoped(s) => {
                let paths: Vec<String> = s
                    .paths
                    .iter()
                    .map(|p| normalize(p))
                    .filter(|p| !p.is_empty())
                    .collect();
                // An empty list would quietly mean "always": say so instead.
                if paths.is_empty() {
                    return Err(format!(
                        "{FILE} is not valid: `{}` has no paths; leave `paths` out to run it on every change",
                        s.command.trim()
                    ));
                }
                (s.command, paths)
            }
        };
        let command = command.trim().to_owned();
        if !command.is_empty() {
            checks.push(RunCheck { command, paths });
        }
    }
    Ok(RunChecks {
        checks,
        timeout: file
            .run
            .timeout_secs
            .map_or(DEFAULT_TIMEOUT, Duration::from_secs),
    })
}

/// What a run worktree's environment may name: `{build}` its build directory, `{checkout}`
/// the worktree itself.
pub const PLACEHOLDERS: [&str; 2] = ["build", "checkout"];

/// The variables agents and checks get in a run worktree, placeholders not filled in yet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunEnv {
    pub vars: Vec<EnvVar>,
    /// Variables two presets set differently and the project did not settle: left unset.
    pub conflicts: Vec<Conflict>,
    /// The presets in use.
    pub presets: Vec<&'static Preset>,
    /// The worktree's resources ([`run_resources`]), placeholders not filled in yet.
    pub resources: Vec<ResolvedResource>,
    /// The programs the worktree's `PATH` sends through the executor ([`run_shims`]).
    pub shims: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
    /// `preset rust`, or the file's name.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub name: String,
    pub presets: Vec<&'static str>,
}

/// The presets `dir`'s file turns on: what it lists, or every one whose marker is at the root.
fn presets_for(file: &File, dir: &Path) -> Result<Vec<&'static Preset>, String> {
    match &file.run.presets {
        None => Ok(presets::detect(dir)),
        Some(names) => names
            .iter()
            .map(|name| {
                presets::get(name).ok_or_else(|| {
                    let known: Vec<&str> = presets::PRESETS.iter().map(|p| p.name).collect();
                    format!(
                        "{FILE} is not valid: there is no preset `{name}` (there are: {})",
                        known.join(", ")
                    )
                })
            })
            .collect(),
    }
}

/// The environment `dir` (a checkout) declares, with the presets it turns on.
pub fn run_env(dir: &Path) -> Result<RunEnv, String> {
    let file = read(dir)?;
    let presets = presets_for(&file, dir)?;
    let mut env = resolve(&presets, &file.run.env, &file.resources)?;
    env.shims = shims_from(&file, &presets);
    Ok(env)
}

/// What `dir`'s presets alone set: for a file that does not parse, whose mistake the checks
/// report.
pub fn preset_env(dir: &Path) -> RunEnv {
    let presets = presets::detect(dir);
    let mut env = resolve(&presets, &BTreeMap::new(), &BTreeMap::new()).unwrap_or_default();
    // No file to read: the presets' own shims, as if `[run] shims` were left out.
    env.shims = shims_from(&File::default(), &presets);
    env
}

/// The resources agents and checks hold in a run worktree: the presets', then the project's
/// over them by name, sorted by name (docs/tasks/LOCKS.md §3).
pub fn run_resources(dir: &Path) -> Result<Vec<ResolvedResource>, String> {
    let file = read(dir)?;
    let presets = presets_for(&file, dir)?;
    resolve_resources(&presets, &file.resources)
}

/// `dir`'s `[run] shims`: what it lists; left out, those the presets it turns on carry
/// (LOCKS §4.6).
pub fn run_shims(dir: &Path) -> Result<Vec<String>, String> {
    let file = read(dir)?;
    let presets = presets_for(&file, dir)?;
    Ok(shims_from(&file, &presets))
}

/// `dir`'s `[run] setup`, as written: the commands the hub runs in a fresh worktree before an
/// agent gets it (`setup.rs`, docs/tasks/TOOLCHAIN.md §7).
pub fn run_setup(dir: &Path) -> Result<Vec<crate::setup::SetupDecl>, String> {
    Ok(read(dir)?.run.setup)
}

/// [`run_shims`] for a file already read: `[run] shims` if the project says, else the presets'.
fn shims_from(file: &File, presets: &[&'static Preset]) -> Vec<String> {
    let listed: Vec<&str> = match &file.run.shims {
        Some(shims) => shims.iter().map(String::as_str).collect(),
        None => presets
            .iter()
            .flat_map(|preset| preset.shims.iter().copied())
            .collect(),
    };
    let mut shims: Vec<String> = Vec::new();
    for shim in listed {
        let shim = shim.trim();
        if !shim.is_empty() && !shims.iter().any(|s| s == shim) {
            shims.push(shim.to_owned());
        }
    }
    shims
}

fn resolve_resources(
    presets: &[&'static Preset],
    project: &BTreeMap<String, ResourceDecl>,
) -> Result<Vec<ResolvedResource>, String> {
    let mut resources: BTreeMap<String, ResolvedResource> = BTreeMap::new();
    for preset in presets {
        for &(name, path, access) in preset.resources {
            resources.insert(
                name.to_owned(),
                ResolvedResource {
                    name: name.to_owned(),
                    path: path.to_owned(),
                    access,
                },
            );
        }
    }
    for (name, decl) in project {
        check_resource(name, decl)?;
        resources.insert(
            name.clone(),
            ResolvedResource {
                name: name.clone(),
                path: decl.path.clone(),
                access: decl.access,
            },
        );
    }
    Ok(resources.into_values().collect())
}

/// A resource that is not this machine's, or cannot be a path, is a mistake: say so rather
/// than quietly treat it as a local one.
fn check_resource(name: &str, decl: &ResourceDecl) -> Result<(), String> {
    match decl.scope.as_deref() {
        None | Some("local") => {}
        Some(scope) => {
            return Err(format!(
                "{FILE} is not valid: [resources] {name} has scope = \"{scope}\"; this version \
                 has only local resources (a `shared` one would need several machines and a hub \
                 to coordinate them, docs/tasks/LOCKS.md §1)"
            ));
        }
    }
    if decl.path.trim().is_empty() {
        return Err(format!(
            "{FILE} is not valid: [resources] {name} has no path"
        ));
    }
    check_placeholders(&format!("[resources] {name}.path"), &decl.path)
}

/// `resources` with their placeholders filled in, for a worktree whose build directory is
/// `build` and whose checkout is `checkout`.
pub fn fill_resources(resources: &mut [ResolvedResource], build: &Path, checkout: &Path) {
    for resource in resources {
        resource.path = fill(&resource.path, build, checkout);
    }
}

/// The value of [`RESOURCES_ENV`]: `[{"name","path","access"}]`, one per resource.
pub fn resources_json(resources: &[ResolvedResource]) -> String {
    serde_json::to_string(resources).unwrap_or_else(|_| "[]".to_owned())
}

/// The presets' variables, then the project's over them. Two presets that set one variable
/// differently set neither: picking one by order would be harder to find than none.
fn resolve(
    presets: &[&'static Preset],
    project: &BTreeMap<String, EnvValue>,
    resources: &BTreeMap<String, ResourceDecl>,
) -> Result<RunEnv, String> {
    let mut set: BTreeMap<&str, (&str, Vec<&'static str>)> = BTreeMap::new();
    let mut conflicts: BTreeMap<&str, Vec<&'static str>> = BTreeMap::new();
    for preset in presets {
        for &(name, value) in preset.env {
            if let Some(names) = conflicts.get_mut(name) {
                names.push(preset.name);
            } else if let Some((other, mut names)) = set.remove(name) {
                names.push(preset.name);
                if other == value {
                    set.insert(name, (other, names));
                } else {
                    conflicts.insert(name, names);
                }
            } else {
                set.insert(name, (value, vec![preset.name]));
            }
        }
    }
    let mut vars: BTreeMap<String, EnvVar> = set
        .into_iter()
        .map(|(name, (value, names))| {
            let var = EnvVar {
                name: name.to_owned(),
                value: value.to_owned(),
                source: format!("preset {}", names.join(", ")),
            };
            (name.to_owned(), var)
        })
        .collect();
    for (name, value) in project {
        conflicts.remove(name.as_str());
        match value {
            EnvValue::Set(value) => {
                check_placeholders(&format!("[run.env] {name}"), value)?;
                let var = EnvVar {
                    name: name.clone(),
                    value: value.clone(),
                    source: FILE.to_owned(),
                };
                vars.insert(name.clone(), var);
            }
            EnvValue::Drop(false) => {
                vars.remove(name);
            }
            EnvValue::Drop(true) => {
                return Err(format!(
                    "{FILE} is not valid: [run.env] {name} = true; give it a value, or false to drop it"
                ));
            }
        }
    }
    Ok(RunEnv {
        vars: vars.into_values().collect(),
        conflicts: conflicts
            .into_iter()
            .map(|(name, presets)| Conflict {
                name: name.to_owned(),
                presets,
            })
            .collect(),
        presets: presets.to_vec(),
        resources: resolve_resources(presets, resources)?,
        // Filled in by `run_env` and `preset_env`, which have the presets.
        shims: Vec::new(),
    })
}

/// A `{word}` that is no placeholder is a mistake, not text to pass on. `what` names the key
/// for the message, e.g. `[run.env] CARGO_TARGET_DIR`.
fn check_placeholders(what: &str, value: &str) -> Result<(), String> {
    let mut rest = value;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else { break };
        let word = &after[..end];
        if !PLACEHOLDERS.contains(&word) {
            return Err(format!(
                "{FILE} is not valid: {what} names {{{word}}}; the placeholders are {}",
                PLACEHOLDERS.map(|p| format!("{{{p}}}")).join(", ")
            ));
        }
        rest = &after[end + 1..];
    }
    Ok(())
}

/// `value` with its placeholders filled in.
/// The `/` right after a placeholder is the platform's, so `{build}/cargo` reads as one path.
pub fn fill(value: &str, build: &Path, checkout: &Path) -> String {
    let sep = std::path::MAIN_SEPARATOR;
    let (build, checkout) = (build.display(), checkout.display());
    value
        .replace("{build}/", &format!("{build}{sep}"))
        .replace("{build}", &build.to_string())
        .replace("{checkout}/", &format!("{checkout}{sep}"))
        .replace("{checkout}", &checkout.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(files: &[&str]) -> Vec<String> {
        files.iter().map(|f| (*f).to_owned()).collect()
    }

    #[test]
    fn reads_the_run_checks() {
        let dir = tempfile::tempdir().unwrap();
        assert!(run_checks(dir.path()).unwrap().checks.is_empty());

        let file = dir.path().join(FILE);
        std::fs::write(
            &file,
            "[run]\nchecks = [\"cargo test\", \" \"]\ntimeout_secs = 5\n",
        )
        .unwrap();
        let checks = run_checks(dir.path()).unwrap();
        assert_eq!(checks.commands_for(&[]), ["cargo test"]);
        assert_eq!(checks.timeout, Duration::from_secs(5));

        // A misspelt key is an error, not "no checks".
        std::fs::write(&file, "[run]\ncheck = [\"cargo test\"]\n").unwrap();
        assert!(run_checks(dir.path()).unwrap_err().contains(FILE));
    }

    #[test]
    fn a_check_with_paths_runs_only_when_one_of_them_changed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE);
        std::fs::write(
            &file,
            r#"[run]
checks = [
    "npm run lint",
    { command = "cargo test", paths = ["./crates/", "Cargo.toml"] },
]
"#,
        )
        .unwrap();
        let checks = run_checks(dir.path()).unwrap();
        assert_eq!(checks.checks[1].paths, ["crates", "Cargo.toml"]);

        // Only the GUI changed: no cargo.
        assert_eq!(
            checks.commands_for(&changed(&["apps/gui/src/App.tsx"])),
            ["npm run lint"]
        );
        // A file under a listed directory, or a listed file itself.
        for files in [&["crates/hub/src/api.rs"][..], &["Cargo.toml"]] {
            assert_eq!(
                checks.commands_for(&changed(files)),
                ["npm run lint", "cargo test"]
            );
        }
        // A name that only starts like a listed one is not under it.
        assert_eq!(
            checks.commands_for(&changed(&["crates-old/x.rs", "Cargo.toml.bak"])),
            ["npm run lint"]
        );
        // Not knowing what changed runs everything.
        assert_eq!(checks.commands_for(&[]), ["npm run lint", "cargo test"]);

        // Empty paths would mean "always" by accident.
        std::fs::write(
            &file,
            "[run]\nchecks = [{ command = \"cargo test\", paths = [] }]\n",
        )
        .unwrap();
        assert!(run_checks(dir.path()).unwrap_err().contains("no paths"));
        // So would a misspelt `paths`.
        std::fs::write(
            &file,
            "[run]\nchecks = [{ command = \"cargo test\", path = [\"crates\"] }]\n",
        )
        .unwrap();
        assert!(run_checks(dir.path()).is_err());
    }

    #[test]
    fn this_repository_runs_cargo_only_for_rust_changes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let checks = run_checks(&root).unwrap();
        let cargo = ["cargo test --workspace"];
        // A change under `crates/hub` also runs the sandbox smoke that starts a hub build.
        let hub = checks.commands_for(&changed(&["crates/hub/src/api.rs"]));
        assert_eq!(hub.first().map(String::as_str), Some(cargo[0]), "{hub:?}");
        assert!(
            hub.iter().skip(1).all(|c| c.contains("hub sandbox smoke")),
            "{hub:?}"
        );
        // Any other Rust change runs cargo and nothing else.
        assert_eq!(
            checks.commands_for(&changed(&["crates/core/src/lib.rs"])),
            cargo
        );
        assert_eq!(checks.commands_for(&changed(&["Cargo.lock"])), cargo);
        // A GUI change waits for the GUI's own checks, not cargo.
        let gui = checks.commands_for(&changed(&["apps/gui/src/styles.css"]));
        assert!(
            !gui.is_empty() && gui.iter().all(|c| c.starts_with("npm ")),
            "{gui:?}"
        );
        // Docs alone run nothing.
        assert!(
            checks
                .commands_for(&changed(&["docs/DESIGN.md"]))
                .is_empty()
        );
    }

    fn vars(env: &RunEnv) -> Vec<(&str, &str)> {
        env.vars
            .iter()
            .map(|v| (v.name.as_str(), v.value.as_str()))
            .collect()
    }

    #[test]
    fn presets_follow_the_marker_files_and_the_project_has_the_last_word() {
        let dir = tempfile::tempdir().unwrap();
        // Nothing to go by: nothing set.
        assert_eq!(run_env(dir.path()).unwrap(), RunEnv::default());
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        assert!(run_env(dir.path()).unwrap().vars.is_empty());

        // A Cargo.toml next to it turns rust on as well.
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        let env = run_env(dir.path()).unwrap();
        assert_eq!(vars(&env), [("CARGO_TARGET_DIR", "{build}/cargo")]);
        assert_eq!(env.vars[0].source, "preset rust");

        let file = dir.path().join(FILE);
        std::fs::write(
            &file,
            "[run.env]\nCARGO_TARGET_DIR = \"{checkout}/out\"\nPYTHONDONTWRITEBYTECODE = \"1\"\n",
        )
        .unwrap();
        let env = run_env(dir.path()).unwrap();
        assert_eq!(
            vars(&env),
            [
                ("CARGO_TARGET_DIR", "{checkout}/out"),
                ("PYTHONDONTWRITEBYTECODE", "1")
            ]
        );
        assert_eq!(env.vars[0].source, FILE);

        std::fs::write(&file, "[run.env]\nCARGO_TARGET_DIR = false\n").unwrap();
        assert!(run_env(dir.path()).unwrap().vars.is_empty());
        std::fs::write(&file, "[run]\npresets = []\n").unwrap();
        let env = run_env(dir.path()).unwrap();
        assert!(env.vars.is_empty() && env.presets.is_empty());

        // Mistakes are errors, never quietly nothing.
        for text in [
            "[run]\npresets = [\"nope\"]\n",
            "[run.env]\nX = true\n",
            "[run.env]\nX = 3\n",
            "[run.env]\nX = \"{cache}/x\"\n",
        ] {
            std::fs::write(&file, text).unwrap();
            assert!(run_env(dir.path()).unwrap_err().contains(FILE), "{text}");
        }
    }

    #[test]
    fn two_presets_that_disagree_set_neither_until_the_project_says() {
        static A: Preset = Preset {
            name: "a",
            markers: &[],
            env: &[("SHARED", "1"), ("OUT", "{build}/a")],
            resources: &[],
            shims: &[],
            hints: &[],
        };
        static B: Preset = Preset {
            name: "b",
            markers: &[],
            env: &[("SHARED", "1"), ("OUT", "{build}/b")],
            resources: &[],
            shims: &[],
            hints: &[],
        };
        let env = resolve(&[&A, &B], &BTreeMap::new(), &BTreeMap::new()).unwrap();
        assert_eq!(vars(&env), [("SHARED", "1")]);
        assert_eq!(env.vars[0].source, "preset a, b");
        assert_eq!(
            env.conflicts,
            [Conflict {
                name: "OUT".into(),
                presets: vec!["a", "b"]
            }]
        );

        let project = BTreeMap::from([("OUT".to_owned(), EnvValue::Set("{build}/x".into()))]);
        let env = resolve(&[&A, &B], &project, &BTreeMap::new()).unwrap();
        assert!(env.conflicts.is_empty());
        assert_eq!(vars(&env), [("OUT", "{build}/x"), ("SHARED", "1")]);
    }

    fn resources(list: &[ResolvedResource]) -> Vec<(&str, &str, ResourceAccess)> {
        list.iter()
            .map(|r| (r.name.as_str(), r.path.as_str(), r.access))
            .collect()
    }

    #[test]
    fn the_rust_preset_declares_its_build_directory_and_the_project_has_the_last_word() {
        let dir = tempfile::tempdir().unwrap();
        // Nothing to go by: nothing to hold.
        assert!(run_resources(dir.path()).unwrap().is_empty());

        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        // The same directory as the CARGO_TARGET_DIR the preset sets, held exclusively.
        assert_eq!(
            resources(&run_resources(dir.path()).unwrap()),
            [("build", "{build}/cargo", ResourceAccess::Exclusive)]
        );
        assert_eq!(
            run_env(dir.path()).unwrap().resources,
            run_resources(dir.path()).unwrap()
        );

        // A project resource of the same name replaces the preset's, and one of its own adds to
        // it; `access` left out is exclusive, `shared` is kept.
        let file = dir.path().join(FILE);
        std::fs::write(
            &file,
            r#"[resources]
build = { path = "{checkout}/target" }
ui = { path = "{build}/ui", access = "shared" }
"#,
        )
        .unwrap();
        assert_eq!(
            resources(&run_resources(dir.path()).unwrap()),
            [
                ("build", "{checkout}/target", ResourceAccess::Exclusive),
                ("ui", "{build}/ui", ResourceAccess::Shared),
            ]
        );

        // `presets = []` takes the preset's resource with it.
        std::fs::write(&file, "[run]\npresets = []\n").unwrap();
        assert!(run_resources(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn a_resource_that_makes_no_sense_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE);
        // A resource several machines share is not this version's (LOCKS §1, §7 item 10).
        std::fs::write(
            &file,
            "[resources]\ndb = { path = \"//nas/db\", scope = \"shared\" }\n",
        )
        .unwrap();
        let err = run_resources(dir.path()).unwrap_err();
        assert!(err.contains("scope") && err.contains("LOCKS.md"), "{err}");
        // So is anything else but `local`.
        std::fs::write(
            &file,
            "[resources]\ndb = { path = \"//nas/db\", scope = \"remote\" }\n",
        )
        .unwrap();
        assert!(run_resources(dir.path()).is_err());

        // Placeholders, missing paths and unknown keys are mistakes, never quietly nothing.
        for text in [
            "[resources]\nx = { path = \"{cache}/x\" }\n",
            "[resources]\nx = { path = \"\" }\n",
            "[resources]\nx = { path = \"{build}/x\", acces = \"shared\" }\n",
        ] {
            std::fs::write(&file, text).unwrap();
            assert!(
                run_resources(dir.path()).unwrap_err().contains(FILE),
                "{text}"
            );
        }
    }

    #[test]
    fn shims_come_from_the_presets_unless_the_project_says() {
        let dir = tempfile::tempdir().unwrap();
        // Not Rust: nothing to shim.
        assert!(run_shims(dir.path()).unwrap().is_empty());

        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        assert_eq!(run_shims(dir.path()).unwrap(), ["cargo"]);

        let file = dir.path().join(FILE);
        // Listed explicitly: the same result, and a project's own list replaces the presets'.
        std::fs::write(&file, "[run]\nshims = [\"cargo\"]\n").unwrap();
        assert_eq!(run_shims(dir.path()).unwrap(), ["cargo"]);
        std::fs::write(
            &file,
            "[run]\nshims = [\"cargo\", \" cc \", \"cargo\", \"\"]\n",
        )
        .unwrap();
        assert_eq!(run_shims(dir.path()).unwrap(), ["cargo", "cc"]);
        // `[]` turns them all off (LOCKS §4.6).
        std::fs::write(&file, "[run]\nshims = []\n").unwrap();
        assert!(run_shims(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn resources_are_filled_in_and_serialized_for_the_environment() {
        let mut list = vec![
            ResolvedResource {
                name: "build".into(),
                path: "{build}/cargo".into(),
                access: ResourceAccess::Exclusive,
            },
            ResolvedResource {
                name: "ui".into(),
                path: "{checkout}/node_modules".into(),
                access: ResourceAccess::Shared,
            },
        ];
        let (build, checkout) = (
            Path::new("/wt/ab12cd34/targets/integration"),
            Path::new("/wt/ab12cd34/integration"),
        );
        fill_resources(&mut list, build, checkout);
        assert_eq!(list[0].path, fill("{build}/cargo", build, checkout));
        assert_eq!(
            list[1].path,
            fill("{checkout}/node_modules", build, checkout)
        );

        let json: serde_json::Value = serde_json::from_str(&resources_json(&list)).unwrap();
        assert_eq!(json[0]["name"], "build");
        assert_eq!(json[0]["path"], list[0].path.as_str());
        assert_eq!(json[0]["access"], "exclusive");
        assert_eq!(json[1]["access"], "shared");
        assert_eq!(json.as_array().unwrap().len(), 2);
        assert_eq!(resources_json(&[]), "[]");
    }
}
