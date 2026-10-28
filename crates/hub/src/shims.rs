//! Shims: a program name that is really this executable, so that a command a person or an
//! agent types goes through the executor (`docs/tasks/LOCKS.md` §4.6).
//!
//! ```text
//! <home>/shims/<version>/cargo(.exe)  a hard link to the crossagents executable (a copy
//!                                     when the file system gives no hard link)
//! ```
//!
//! A run worktree's `PATH` starts with that directory (`workspace::worktree_env`), so `cargo`
//! there is this executable. It sees its own name in `argv[0]` ([`invoked_as`]), finds the real
//! `cargo` later in `PATH` ([`real`], skipping the shim's own directory), and runs it under the
//! run's `build` resource like `crossagents exec --resource build -- cargo …`.
//!
//! The directory is per version: a hub upgrade never rewrites the shim a running agent is
//! executing (on Windows that write would fail; on Unix a hard link would silently move on to
//! the new binary). Nothing here writes `.cmd` wrappers: on Windows those would hand the
//! arguments through `cmd.exe`, with its quoting and exit codes in between.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use crossagents_core::exec;

use crate::Paths;

/// The programs a preset shims today: the names a shim file is made for, and the only names
/// `argv[0]` can use to enter shim mode.
pub const NAMES: &[&str] = &["cargo"];

/// `<home>/shims/<version>`: where this executable's shims are.
pub fn dir(paths: &Paths) -> PathBuf {
    paths.home.join("shims").join(env!("CARGO_PKG_VERSION"))
}

/// One shim file in `dir`: `cargo.exe` on Windows, `cargo` elsewhere.
pub fn file(dir: &Path, name: &str) -> PathBuf {
    if cfg!(windows) {
        dir.join(format!("{name}.exe"))
    } else {
        dir.join(name)
    }
}

/// Makes sure every [`NAMES`] has a shim in [`dir`], and returns that directory.
///
/// Called once when the hub starts; a failure is the hub's to report and to run without
/// shims, which only means `cargo` in a run worktree does not hold `build` by itself.
pub fn ensure(paths: &Paths) -> Result<PathBuf> {
    let dir = dir(paths);
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let exe = std::env::current_exe().context("cannot locate the crossagents executable")?;
    for name in NAMES {
        place(&exe, &file(&dir, name))?;
    }
    Ok(dir)
}

/// `target` as `exe` itself: a hard link when the file system gives one, a copy otherwise.
///
/// Nothing is done when `target` already is that: the same file, or — after a copy — a file of
/// the same size, which for a directory named after the version is the same build of it.
fn place(exe: &Path, target: &Path) -> Result<()> {
    let Some(existing) = std::fs::metadata(target).ok().filter(|m| m.is_file()) else {
        return place_with(exe, target, |from, to| std::fs::hard_link(from, to));
    };
    let same_size = std::fs::metadata(exe).is_ok_and(|exe| exe.len() == existing.len());
    if same_size || same_file(exe, target) {
        return Ok(());
    }
    // Another executable of this version: a rebuild. Replace it — unless it is in use, which
    // on Windows a running agent's shim is. What is there is then the same version, only not
    // the latest build of it, and that is better than losing the shims.
    match std::fs::remove_file(target) {
        Ok(()) => place_with(exe, target, |from, to| std::fs::hard_link(from, to)),
        Err(error) => {
            tracing::debug!(
                target = %target.display(),
                %error,
                "cannot replace a shim that is in use; keeping it"
            );
            Ok(())
        }
    }
}

/// [`place`]'s work, with the hard link a parameter so that the copy fallback can be tested on
/// a file system that does give hard links.
fn place_with(
    exe: &Path,
    target: &Path,
    hard_link: fn(&Path, &Path) -> io::Result<()>,
) -> Result<()> {
    match hard_link(exe, target) {
        Ok(()) => Ok(()),
        Err(error) => {
            tracing::debug!(
                exe = %exe.display(),
                target = %target.display(),
                %error,
                "cannot make a hard link for the shim; copying the executable"
            );
            std::fs::copy(exe, target).with_context(|| {
                format!(
                    "cannot copy {} to {} for the shim",
                    exe.display(),
                    target.display()
                )
            })?;
            Ok(())
        }
    }
}

/// Whether both paths are the same file (by identity, not by name).
fn same_file(left: &Path, right: &Path) -> bool {
    match (exec::path_id(left), exec::path_id(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// The shim name `argv[0]` asks for, if any: `C:\…\cargo.exe` and `cargo` are `cargo`, in any
/// case.
pub fn invoked_as(argv0: &OsStr) -> Option<&'static str> {
    let name = Path::new(argv0).file_name()?.to_string_lossy().into_owned();
    let stem = match name.to_lowercase().strip_suffix(".exe") {
        Some(_) => &name[..name.len() - 4],
        None => name.as_str(),
    };
    NAMES
        .iter()
        .copied()
        .find(|known| stem.eq_ignore_ascii_case(known))
}

/// The real `name` on this process's `PATH`, skipping the directory the shim is in.
pub fn real(name: &str, skip: &Path) -> Option<PathBuf> {
    real_in(name, &std::env::var_os("PATH")?, skip)
}

/// [`real`] against a `PATH` value, for tests and callers that carry their own.
pub fn real_in(name: &str, path: &OsStr, skip: &Path) -> Option<PathBuf> {
    let skip = exec::identity(skip);
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() || exec::identity(&dir) == skip {
            continue;
        }
        if let Some(found) = find(&dir, name) {
            return Some(found);
        }
    }
    None
}

/// `name` in `dir`, as Windows would run it: `PATHEXT` decides which files count, in its
/// order, so `cargo.exe` and a `cargo.cmd` beside it stay apart.
fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    if cfg!(windows) {
        let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
        for ext in exts.split(';').map(str::trim).filter(|ext| !ext.is_empty()) {
            let file = dir.join(format!("{name}{}", ext.to_lowercase()));
            if file.is_file() {
                return Some(file);
            }
        }
        return None;
    }
    let file = dir.join(name);
    file.is_file().then_some(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_knows_when_it_was_started_as_a_shim() {
        for argv0 in [
            "cargo",
            "cargo.exe",
            "C:\\home\\shims\\0.1.0\\CARGO.EXE",
            "/home/shims/0.1.0/cargo",
        ] {
            assert_eq!(invoked_as(OsStr::new(argv0)), Some("cargo"), "{argv0}");
        }
        for argv0 in [
            "crossagents",
            "crossagents.exe",
            "/usr/bin/cargo-clippy",
            "",
        ] {
            assert_eq!(invoked_as(OsStr::new(argv0)), None, "{argv0}");
        }
    }

    /// A file that looks like a program: enough for the search, which is about names.
    fn program(dir: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let file = if cfg!(windows) {
            dir.join(format!("{name}.exe"))
        } else {
            dir.join(name)
        };
        std::fs::write(&file, b"not really").unwrap();
        file
    }

    #[test]
    fn the_shim_directory_is_skipped_when_looking_for_the_real_program() {
        let temp = tempfile::tempdir().unwrap();
        let shims = temp.path().join("shims");
        let real = temp.path().join("real");
        let shim = program(&shims, "cargo");
        let real = program(&real, "cargo");
        let path = std::env::join_paths([shims.as_path(), real.parent().unwrap()]).unwrap();

        // The shim's own directory comes first and is skipped: the real one is found.
        let found = real_in("cargo", &path, &shims).expect("the real cargo");
        assert_eq!(exec::identity(&found), exec::identity(&real));

        // Without the skip it would find itself, which is what it must never do.
        let found = real_in("cargo", &path, &temp.path().join("elsewhere")).expect("a cargo");
        assert_eq!(exec::identity(&found), exec::identity(&shim));

        // A program that is not there is not found.
        assert!(real_in("cargo", &path, &shims).is_some());
        let empty = std::env::join_paths([shims.as_path()]).unwrap();
        assert!(real_in("cargo", &empty, &shims).is_none());
    }

    #[test]
    fn the_shim_directory_holds_one_link_per_name_and_is_left_alone_the_second_time() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path());
        let dir = ensure(&paths).unwrap();
        assert_eq!(dir, super::dir(&paths));
        let exe = std::env::current_exe().unwrap();
        for name in NAMES {
            let shim = file(&dir, name);
            assert!(shim.is_file(), "{}", shim.display());
            // A hard link where the file system gives one: the same file, not a copy.
            assert_eq!(
                std::fs::metadata(&shim).unwrap().len(),
                std::fs::metadata(&exe).unwrap().len()
            );
            assert!(
                same_file(&exe, &shim),
                "{} is not the executable",
                shim.display()
            );
        }

        // Again: the file is already the executable, so it is not rewritten.
        let shim = file(&dir, "cargo");
        let before = std::fs::metadata(&shim).unwrap();
        ensure(&paths).unwrap();
        let after = std::fs::metadata(&shim).unwrap();
        assert_eq!(before.len(), after.len());
        assert!(same_file(&exe, &shim));
    }

    #[test]
    fn a_hard_link_that_fails_leaves_a_copy() {
        let temp = tempfile::tempdir().unwrap();
        let exe = temp.path().join("crossagents.exe");
        std::fs::write(&exe, b"the executable").unwrap();
        let target = temp.path().join("shims").join("cargo.exe");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();

        // A file system without hard links (a network share, a different volume) fails here.
        place_with(&exe, &target, |_, _| {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        })
        .unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"the executable");
        assert!(!same_file(&exe, &target), "the fallback must copy");
    }

    #[test]
    fn a_stale_shim_of_another_build_is_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let exe = temp.path().join("crossagents.exe");
        std::fs::write(&exe, b"the new executable").unwrap();
        let target = temp.path().join("shims").join("cargo.exe");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"old").unwrap();

        place(&exe, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"the new executable");
        assert!(same_file(&exe, &target));
    }
}
