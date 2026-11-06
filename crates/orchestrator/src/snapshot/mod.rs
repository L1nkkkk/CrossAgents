//! A fixed starting point for workers when the lead works in the person's own directory
//! (docs/tasks/LEAD.md §5).
//!
//! A worker cannot start from the directory's `HEAD`: the person's uncommitted work — edited
//! files, new files, staged files — would be missing, and two workers started at different
//! moments would not share a base. A snapshot is a commit of what the directory holds *now*:
//!
//! * built in an index of its own (`GIT_INDEX_FILE`), so the person's staging area, branch,
//!   stash list and working tree are untouched — reading the directory writes nothing there;
//! * kept under `refs/crossagents/snapshots/<run>/<n>`, a namespace the hub never pushes (a
//!   `git push` sends branches and tags, not this), so nothing leaves the machine;
//! * bounded: a changed file above [`MAX_FILE`] is left out and reported, so one huge
//!   artefact cannot make every worker's checkout enormous;
//! * watched names (`.env`, `*.pem`, …) are reported to the person, since the snapshot is
//!   handed to workers.
//!
//! Ignored files are left out, as git leaves them out. Submodule *contents* are not captured
//! (the gitlink is): a worker touching a submodule is outside this scope.

use std::path::{Path, PathBuf};

use crossagents_core::CoreError;
use crossagents_core::process::NoWindow;

use crate::workspace::NEVER_ADDED;

/// Changed files bigger than this go into no snapshot: a worker's checkout would carry them
/// whole, and a single large artefact (a database dump, a recording) is rarely the work.
pub const MAX_FILE: u64 = 16 << 20;

/// A snapshot: what the directory held, as a commit only the hub's refs name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The snapshot commit; a worker's branch starts here.
    pub commit: String,
    /// The tree it points at.
    pub tree: String,
    /// The commit checked out when the snapshot was taken.
    pub head: String,
    /// Which snapshot of the run it is, from 1.
    pub number: u32,
    /// `refs/crossagents/snapshots/<run>/<n>`, which keeps the commit alive.
    pub reference: String,
    /// How many changed or new files it holds.
    pub files: usize,
    /// Changed files left out because they are above [`MAX_FILE`].
    pub skipped: Vec<String>,
    /// Changed or new files whose names look like secrets, for the person to check.
    pub secrets: Vec<String>,
}

/// Captures what `dir` holds now as a snapshot of `run`'s `number`th dispatch. Nothing in
/// `dir` (its index above all) is written.
pub async fn capture(dir: &Path, run: &str, number: u32) -> Result<Snapshot, CoreError> {
    let head = git(dir, &["rev-parse", "--verify", "HEAD"], None)
        .await
        .map_err(|err| {
            CoreError::Conflict(format!(
                "{} has no commit to snapshot from ({err}); a worker needs one",
                dir.display()
            ))
        })?;
    let head = head.trim().to_owned();
    let prefix = git(dir, &["rev-parse", "--show-prefix"], None)
        .await
        .unwrap_or_default()
        .trim()
        .to_owned();

    // What differs from HEAD (committed or not, staged or not) plus every new file git does
    // not ignore: both read-only.
    let mut changed = list(
        git(
            dir,
            &[
                "-c",
                "core.quotePath=false",
                "diff",
                "--name-only",
                "-z",
                "HEAD",
                "--",
            ],
            None,
        )
        .await?,
    );
    for path in list(
        git(
            dir,
            &[
                "-c",
                "core.quotePath=false",
                "ls-files",
                "--others",
                "--exclude-standard",
                "-z",
            ],
            None,
        )
        .await?,
    ) {
        changed.push(format!("{prefix}{path}"));
    }
    changed.sort();
    changed.dedup();

    let mut skipped = Vec::new();
    let mut secrets = Vec::new();
    let mut excluded = Vec::new();
    let mut files = 0usize;
    for path in &changed {
        if looks_like_secret(path) {
            secrets.push(path.clone());
        }
        let rel = path.strip_prefix(&prefix).unwrap_or(path);
        let file = dir.join(rel);
        match tokio::fs::metadata(&file).await {
            Ok(meta) if meta.is_file() && meta.len() > MAX_FILE => {
                skipped.push(path.clone());
                excluded.push(format!(":(exclude,top,literal){path}"));
            }
            // A directory (a submodule, an empty path) has no bytes to weigh.
            _ => files += 1,
        }
    }

    let index = TempIndex::new();
    let index_path = index.path();
    git(dir, &["read-tree", "HEAD"], Some(index_path)).await?;
    let mut add = vec!["add", "-A", "--", "."];
    add.extend(excluded.iter().map(String::as_str));
    add.extend(NEVER_ADDED);
    git(dir, &add, Some(index_path)).await?;
    let tree = git(dir, &["write-tree"], Some(index_path))
        .await?
        .trim()
        .to_owned();

    let message = format!("crossagents: snapshot {number} of run {run}");
    let commit = git(
        dir,
        &[
            "-c",
            "user.name=CrossAgents",
            "-c",
            "user.email=crossagents@localhost",
            "commit-tree",
            &tree,
            "-p",
            &head,
            "-m",
            &message,
        ],
        None,
    )
    .await?
    .trim()
    .to_owned();
    let reference = format!("refs/crossagents/snapshots/{run}/{number}");
    git(dir, &["update-ref", &reference, &commit], None).await?;

    Ok(Snapshot {
        commit,
        tree,
        head,
        number,
        reference,
        files,
        skipped,
        secrets,
    })
}

/// Whether a changed file's name suggests a secret (reported to the person; never a reason to
/// fail a snapshot).
fn looks_like_secret(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name.ends_with(".p12")
        || name.ends_with(".pfx")
        || name.ends_with(".keystore")
        || name == "id_rsa"
        || name == "id_ed25519"
        || name == "id_ecdsa"
        || name == ".netrc"
        || name == ".pgpass"
}

/// NUL-terminated paths of a `-z` listing, trimmed of the trailing empty entry.
fn list(text: String) -> Vec<String> {
    text.split('\0')
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect()
}

async fn git(dir: &Path, args: &[&str], index: Option<&Path>) -> Result<String, CoreError> {
    let mut command = tokio::process::Command::new("git");
    command.no_window().args(args).current_dir(dir);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let out = command
        .output()
        .await
        .map_err(|err| CoreError::Agent(format!("cannot run git in {}: {err}", dir.display())))?;
    if !out.status.success() {
        return Err(CoreError::Conflict(format!(
            "git {} in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// An index of this snapshot's own, in the temp directory, removed when the capture ends.
struct TempIndex(PathBuf);

impl TempIndex {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "crossagents-snapshot-{}-{}.index",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        )))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("index.lock"));
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
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("tracked.txt"), "one\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "init"]);
        dir
    }

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// Everything about the person's index that a snapshot must leave alone.
    fn index_state(dir: &Path) -> String {
        format!(
            "{}\n{}\n{}",
            git_out(dir, &["status", "--porcelain"]),
            git_out(dir, &["diff", "--cached"]),
            git_out(dir, &["ls-files", "--stage"]),
        )
    }

    #[tokio::test]
    async fn a_snapshot_holds_uncommitted_staged_and_new_files_and_leaves_the_index_alone() {
        let dir = repo();
        std::fs::write(dir.path().join("tracked.txt"), "two\n").unwrap();
        std::fs::write(dir.path().join("staged.txt"), "staged\n").unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["add", "staged.txt"])
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(dir.path().join("new.txt"), "new\n").unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/out.bin"), "ignored").unwrap();

        let before = index_state(dir.path());
        let snap = capture(dir.path(), "ab12cd34", 1).await.unwrap();
        assert_eq!(
            index_state(dir.path()),
            before,
            "the snapshot wrote in the index"
        );
        assert_eq!(snap.number, 1);
        assert_eq!(snap.reference, "refs/crossagents/snapshots/ab12cd34/1");
        assert_eq!(
            git_out(dir.path(), &["rev-parse", &snap.reference]),
            snap.commit
        );
        let files = git_out(dir.path(), &["ls-tree", "-r", "--name-only", &snap.commit]);
        for file in ["tracked.txt", "staged.txt", "new.txt"] {
            assert!(files.contains(file), "{file} missing from\n{files}");
        }
        assert!(!files.contains("target/out.bin"), "{files}");
        // What the snapshot holds is the working copy, not HEAD.
        let content = git_out(
            dir.path(),
            &["show", &format!("{}:tracked.txt", snap.commit)],
        );
        assert_eq!(content, "two");
        // The working tree is what it was too.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
            "two\n"
        );
        // Nothing was committed on the branch.
        assert!(!git_out(dir.path(), &["log", "--format=%s"]).contains("snapshot"));
    }

    #[tokio::test]
    async fn a_second_snapshot_is_its_own_and_keeps_the_first() {
        let dir = repo();
        let first = capture(dir.path(), "ab12cd34", 1).await.unwrap();
        std::fs::write(dir.path().join("tracked.txt"), "two\n").unwrap();
        let second = capture(dir.path(), "ab12cd34", 2).await.unwrap();
        assert_ne!(first.commit, second.commit);
        assert_eq!(
            git_out(dir.path(), &["rev-parse", &first.reference]),
            first.commit,
            "the first snapshot stays reachable"
        );
    }

    #[tokio::test]
    async fn huge_files_are_skipped_and_said() {
        let dir = repo();
        let big = dir.path().join("big.bin");
        let file = std::fs::File::create(&big).unwrap();
        file.set_len(MAX_FILE + 1).unwrap();
        drop(file);
        let snap = capture(dir.path(), "ab12cd34", 1).await.unwrap();
        assert_eq!(snap.skipped, ["big.bin"]);
        let files = git_out(dir.path(), &["ls-tree", "-r", "--name-only", &snap.commit]);
        assert!(!files.contains("big.bin"), "{files}");
        // The file is still the person's, where it was.
        assert!(big.exists());
    }

    #[tokio::test]
    async fn names_that_look_like_secrets_are_reported() {
        let dir = repo();
        std::fs::write(dir.path().join(".env"), "TOKEN=1\n").unwrap();
        std::fs::write(dir.path().join("server.pem"), "key\n").unwrap();
        std::fs::write(dir.path().join("notes.md"), "fine\n").unwrap();
        let snap = capture(dir.path(), "ab12cd34", 1).await.unwrap();
        assert_eq!(snap.secrets, [".env", "server.pem"]);
        // Reported, not removed: the worker may need them and the person decides.
        let files = git_out(dir.path(), &["ls-tree", "-r", "--name-only", &snap.commit]);
        assert!(
            files.contains(".env") && files.contains("server.pem"),
            "{files}"
        );
    }

    #[tokio::test]
    async fn a_subdirectory_workspace_snapshots_what_is_below_it() {
        let dir = repo();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/inner.txt"), "inner\n").unwrap();
        let snap = capture(&dir.path().join("sub"), "ab12cd34", 1)
            .await
            .unwrap();
        assert_eq!(snap.files, 1);
        let files = git_out(dir.path(), &["ls-tree", "-r", "--name-only", &snap.commit]);
        assert!(files.contains("sub/inner.txt"), "{files}");
    }

    #[tokio::test]
    async fn a_directory_without_commits_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        let err = capture(dir.path(), "ab12cd34", 1).await.unwrap_err();
        assert!(err.to_string().contains("no commit"), "{err}");
    }
}
