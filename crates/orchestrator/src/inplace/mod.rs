//! A lead at work in the person's own directory (docs/tasks/LEAD.md §4, §7).
//!
//! Nothing there is committed for it, so its checks have no commit to stand on. What they
//! ran on is the content itself: [`snapshot`] is a fingerprint of the files the person's
//! directory has now, and a result binds to the one recorded when the checks ran. When the
//! content has moved since, the result is about other content and cannot count as passed
//! (or as anything else) until the checks run again.

use std::path::Path;

use crossagents_core::CoreError;
use crossagents_core::process::NoWindow;

/// Bigger files are told apart by their size and modification time rather than read: the
/// fingerprint is checked against a directory a person may be working in, at the pace of a
/// run's checks, not hashed byte for byte over a whole tree.
const WHOLE_FILE: u64 = 16 << 20;

/// A fingerprint of what `dir` holds now: the commit it is on, every file that differs from
/// that commit (staged or not, new or deleted, unreadable ones by name), and for each of
/// them its content. Ignored files are left out, as git leaves them out: a build directory
/// changing is not the person's work changing.
pub async fn snapshot(dir: &Path) -> Result<String, CoreError> {
    let head = git(dir, &["rev-parse", "HEAD"]).await.unwrap_or_default();
    let status = git(
        dir,
        &[
            "-c",
            "core.quotePath=false",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ],
    )
    .await?;
    let mut hash = Hash::new();
    hash.bytes(b"git");
    hash.bytes(String::from_utf8_lossy(&head).trim().as_bytes());
    for entry in status.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        // `XY <path>`: the two status letters say what happened to it, the path which it is.
        let (status, path) = match entry.split_at_checked(3) {
            Some((status, path)) => (status, path),
            None => (entry, &b""[..]),
        };
        hash.byte(b'\n');
        hash.bytes(status);
        hash.bytes(path);
        let file = dir.join(String::from_utf8_lossy(path).as_ref());
        hash_content(&mut hash, &file).await;
    }
    Ok(hash.finish())
}

/// Whether `dir` still holds what `recorded` (a [`snapshot`]) says it did.
pub async fn unchanged(dir: &Path, recorded: &str) -> bool {
    matches!(snapshot(dir).await, Ok(now) if now == recorded)
}

/// A file's content, or what stands for it when it is gone or too big to read.
async fn hash_content(hash: &mut Hash, file: &Path) {
    let meta = match tokio::fs::metadata(file).await {
        Ok(meta) => meta,
        Err(_) => {
            hash.bytes(b"gone");
            return;
        }
    };
    if !meta.is_file() {
        hash.bytes(b"not a file");
        return;
    }
    if meta.len() > WHOLE_FILE {
        hash.bytes(b"big");
        hash.bytes(&meta.len().to_le_bytes());
        if let Ok(modified) = meta.modified()
            && let Ok(since) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            hash.bytes(&since.as_nanos().to_le_bytes());
        }
        return;
    }
    match tokio::fs::read(file).await {
        Ok(bytes) => {
            hash.bytes(&bytes.len().to_le_bytes());
            hash.bytes(&bytes);
        }
        // Named but unreadable: the name still says it is not what the checks saw.
        Err(_) => hash.bytes(b"unreadable"),
    }
}

async fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, CoreError> {
    let out = tokio::process::Command::new("git")
        .no_window()
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .map_err(|err| CoreError::Agent(format!("cannot read {}: {err}", dir.display())))?;
    if !out.status.success() {
        return Err(CoreError::Conflict(format!(
            "cannot read {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

/// FNV-1a, written out: the fingerprint only ever says "the same as before" or "not", and
/// storing it must not depend on a hash library staying the same version.
struct Hash(u64);

impl Hash {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn new() -> Self {
        Self(Self::OFFSET)
    }

    fn byte(&mut self, byte: u8) {
        self.0 ^= u64::from(byte);
        self.0 = self.0.wrapping_mul(Self::PRIME);
    }

    fn bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.byte(*byte);
        }
    }

    fn finish(self) -> String {
        format!("{:016x}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("tracked.txt"), "one\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "init"]);
        dir
    }

    #[tokio::test]
    async fn the_fingerprint_follows_what_the_directory_holds() {
        let dir = repo();
        let at_first = snapshot(dir.path()).await.unwrap();
        assert_eq!(
            at_first,
            snapshot(dir.path()).await.unwrap(),
            "nothing changed, the same fingerprint"
        );

        // An edit, a new file, a deletion and a staged change each move it.
        std::fs::write(dir.path().join("tracked.txt"), "two\n").unwrap();
        let edited = snapshot(dir.path()).await.unwrap();
        assert_ne!(edited, at_first);
        std::fs::write(dir.path().join("new.txt"), "new\n").unwrap();
        let added = snapshot(dir.path()).await.unwrap();
        assert_ne!(added, edited);
        std::fs::remove_file(dir.path().join("new.txt")).unwrap();
        assert_eq!(snapshot(dir.path()).await.unwrap(), edited);
        let out = std::process::Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_ne!(snapshot(dir.path()).await.unwrap(), edited);
    }

    #[tokio::test]
    async fn content_the_checks_ran_on_is_what_binds_a_result() {
        let dir = repo();
        let checked = snapshot(dir.path()).await.unwrap();
        assert!(unchanged(dir.path(), &checked).await);
        std::fs::write(dir.path().join("tracked.txt"), "other\n").unwrap();
        assert!(!unchanged(dir.path(), &checked).await);
    }

    #[tokio::test]
    async fn ignored_build_output_does_not_move_the_fingerprint() {
        let dir = repo();
        let before = snapshot(dir.path()).await.unwrap();
        std::fs::create_dir_all(dir.path().join("target").join("debug")).unwrap();
        std::fs::write(dir.path().join("target/debug/app"), "binary").unwrap();
        assert_eq!(snapshot(dir.path()).await.unwrap(), before);
    }
}
