//! The little the hub knows about toolchains (docs/tasks/TOOLCHAIN.md §4): what a project
//! whose checkout has a marker file at its root gets in its run worktrees' environment, and
//! which lines in a failed check's output may come from a build running at the same time. The
//! rest of the hub knows no language; a project overrides or turns these off in
//! `crossagents.toml`.

use std::path::Path;

use crate::project::ResourceAccess;

/// One toolchain's defaults.
#[derive(Debug, PartialEq, Eq)]
pub struct Preset {
    pub name: &'static str,
    /// Any of these at the checkout's root turns it on.
    pub markers: &'static [&'static str],
    /// Variables for agents and checks in a run worktree; values may hold placeholders
    /// (`project::PLACEHOLDERS`).
    pub env: &'static [(&'static str, &'static str)],
    /// Local resources a build there holds (docs/tasks/LOCKS.md §3): name, path template
    /// (placeholders as in `env`), access. The project replaces one by name.
    pub resources: &'static [(&'static str, &'static str, ResourceAccess)],
    /// Programs the run worktree's `PATH` sends through the executor (LOCKS §4.6); the
    /// project's `[run] shims` replaces the list.
    pub shims: &'static [&'static str],
    /// Signs in a failed check's output that the failure may come from another build at
    /// the same time: noted, never taken as proof (`running::in_use_failure` is).
    pub hints: &'static [&'static str],
}

pub const PRESETS: &[Preset] = &[Preset {
    name: "rust",
    markers: &["Cargo.toml"],
    // Each worktree builds apart: workspace members have the same names and versions in
    // every worktree, so their artefacts would overwrite each other (docs/TODO.md §十六).
    env: &[("CARGO_TARGET_DIR", "{build}/cargo")],
    // The same directory as `CARGO_TARGET_DIR`: two cargos on it must not run at once
    // (LOCKS §3).
    resources: &[("build", "{build}/cargo", ResourceAccess::Exclusive)],
    // `cargo` in a run worktree holds `build` without the agent asking (LOCKS §4.6).
    shims: &["cargo"],
    // MSVC cannot open an output file (one a test still runs, or anything else); cargo
    // waits for another cargo's lock, and may well succeed after.
    hints: &["LNK1104", "Blocking waiting for file lock"],
}];

pub fn get(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name == name)
}

/// The presets whose marker is at `dir`'s root.
pub fn detect(dir: &Path) -> Vec<&'static Preset> {
    PRESETS
        .iter()
        .filter(|p| p.markers.iter().any(|m| dir.join(m).is_file()))
        .collect()
}
