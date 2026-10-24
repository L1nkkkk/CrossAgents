//! Stable copies of the executable for processes that outlive the command that started
//! them (docs/tasks/DOGFOOD.md §1): runtime hosts and `crossagents mcp` started from
//! `target/debug/crossagents.exe` would keep it locked, and the next `cargo build` fails.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

use crate::Paths;

/// Copies older than this are removed at the next start, when nothing runs them.
const KEEP: Duration = Duration::from_secs(24 * 60 * 60);

impl Paths {
    /// Copies of development builds, by content hash. Apart from `bin/`, which belongs to
    /// the installed GUI (it upgrades and stops hubs running from there).
    pub fn shadow(&self) -> PathBuf {
        self.home.join("shadow")
    }
    /// Executables the installed GUI keeps, one directory per version.
    pub fn bin(&self) -> PathBuf {
        self.home.join("bin")
    }
}

/// The executable long-lived processes should run from: `exe` itself when it already lives
/// under `<home>/bin/` or `<home>/shadow/`, else a copy of it under `<home>/shadow/<hash>/`.
pub fn stable_exe(paths: &Paths, exe: &Path) -> Result<PathBuf> {
    if under(exe, &paths.bin()) || under(exe, &paths.shadow()) {
        return Ok(exe.to_path_buf());
    }
    let bytes = std::fs::read(exe).with_context(|| format!("cannot read {}", exe.display()))?;
    let hash = format!("{:016x}", fnv1a(&bytes));
    let dir = paths.shadow().join(&hash);
    let name = exe.file_name().context("executable has no file name")?;
    let copy = dir.join(name);
    if !std::fs::metadata(&copy).is_ok_and(|m| m.len() == bytes.len() as u64) {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
        // Write aside and rename, so a half-written copy is never run.
        let partial = dir.join(format!(".partial-{}", std::process::id()));
        std::fs::write(&partial, &bytes)
            .with_context(|| format!("cannot write {}", partial.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o755))?;
        }
        if let Err(err) = std::fs::rename(&partial, &copy) {
            let _ = std::fs::remove_file(&partial);
            // Another process put the same copy there first.
            if !copy.exists() {
                return Err(err).with_context(|| format!("cannot write {}", copy.display()));
            }
        }
    }
    touch(&dir);
    prune(&paths.shadow(), &hash);
    Ok(copy)
}

/// Removes old copies other than `keep`. One in use cannot be removed on Windows and stays.
fn prune(root: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy() == keep {
            continue;
        }
        let path = entry.path();
        let old = std::fs::metadata(path.join(".used"))
            .or_else(|_| entry.metadata())
            .and_then(|m| m.modified())
            .is_ok_and(|t| now.duration_since(t).unwrap_or_default() > KEEP);
        if old {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// Marks a copy as used now, so pruning leaves it alone.
fn touch(dir: &Path) {
    let _ = std::fs::write(dir.join(".used"), b"");
}

fn under(path: &Path, dir: &Path) -> bool {
    let canon = |p: &Path| dunce::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(path).starts_with(canon(dir))
}

/// FNV-1a over the file: changes with every rebuild, stable across runs.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, b| {
        (hash ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_by_content_and_reuses() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("home"));
        let build = dir.path().join("target");
        std::fs::create_dir_all(&build).unwrap();
        let exe = build.join("tool.exe");
        std::fs::write(&exe, b"one").unwrap();

        let first = stable_exe(&paths, &exe).unwrap();
        assert!(first.starts_with(paths.shadow()));
        assert_eq!(std::fs::read(&first).unwrap(), b"one");
        assert_eq!(stable_exe(&paths, &exe).unwrap(), first);
        // The copy is what runs, so the build output can be replaced.
        std::fs::write(&exe, b"two!").unwrap();
        let second = stable_exe(&paths, &exe).unwrap();
        assert_ne!(second, first);
        assert_eq!(std::fs::read(&second).unwrap(), b"two!");
        // Already stable: used as it is.
        assert_eq!(stable_exe(&paths, &second).unwrap(), second);
        let installed = paths.bin().join("1.0.0").join("tool.exe");
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(&installed, b"x").unwrap();
        assert_eq!(stable_exe(&paths, &installed).unwrap(), installed);
    }
}
