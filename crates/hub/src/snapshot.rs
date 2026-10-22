//! What a turn changed, for drivers that do not report it (docs/tasks/V6-GUI.md §5).
//!
//! Before a turn the hub writes the working copy's content as a git tree, and again after it;
//! the turn's diff is the difference between the two trees. Trees are written through a
//! private copy of the index, so the agent's index, refs and stash stay untouched. Files that
//! `.gitignore` excludes are left out, as in `git status`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::Command;

/// A snapshot (or the diff) taking longer than this is given up.
const TIMEOUT: Duration = Duration::from_secs(30);
/// Diffs are cut at a file boundary past this size.
pub const DIFF_LIMIT: usize = 1 << 20;

/// A tree id of the repository containing `dir`, with everything as it is on disk now. `None`
/// when `dir` is not in a git repository or git fails.
pub async fn take(dir: &Path) -> Option<String> {
    tokio::time::timeout(TIMEOUT, write_tree(dir))
        .await
        .ok()
        .flatten()
}

/// `git diff` from tree `base` to tree `head`; empty when nothing changed.
pub async fn diff(dir: &Path, base: &str, head: &str) -> Option<String> {
    if base == head {
        return Some(String::new());
    }
    let out = tokio::time::timeout(
        TIMEOUT,
        git(
            dir,
            &["diff", "--no-color", "--no-ext-diff", "-M", base, head],
            None,
        ),
    )
    .await
    .ok()??;
    Some(cut(out, DIFF_LIMIT))
}

async fn write_tree(dir: &Path) -> Option<String> {
    let top = PathBuf::from(git(dir, &["rev-parse", "--show-toplevel"], None).await?);
    let index = git(&top, &["rev-parse", "--git-path", "index"], None).await?;
    let index = top.join(index);
    let private = std::env::temp_dir().join(format!("crossagents-index-{}", uuid::Uuid::now_v7()));
    // Starting from the real index keeps its stat cache: only changed files are hashed. A
    // repository without commits has none, and starts empty.
    let _ = tokio::fs::copy(&index, &private).await;
    let tree = match git(&top, &["add", "-A", "--", "."], Some(&private)).await {
        Some(_) => git(&top, &["write-tree"], Some(&private)).await,
        None => None,
    };
    let _ = tokio::fs::remove_file(&private).await;
    tree
}

async fn git(dir: &Path, args: &[&str], index: Option<&Path>) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .kill_on_drop(true);
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }
    let output = cmd.output().await.ok()?;
    if !output.status.success() {
        tracing::debug!(
            args = ?args,
            stderr = %String::from_utf8_lossy(&output.stderr).trim(),
            "git failed"
        );
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(if args[0] == "diff" {
        text.into_owned()
    } else {
        text.trim().to_owned()
    })
}

/// `diff` limited to about `limit` bytes, keeping whole files.
fn cut(diff: String, limit: usize) -> String {
    if diff.len() <= limit {
        return diff;
    }
    let mut end = limit;
    while !diff.is_char_boundary(end) {
        end -= 1;
    }
    // The last file header that starts within the limit.
    let at = diff
        .match_indices("\ndiff --git ")
        .map(|(i, _)| i)
        .take_while(|&i| i < end)
        .last();
    match at {
        Some(at) => diff[..=at].to_owned(),
        None => diff[..end].to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn run(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .await
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    #[tokio::test]
    async fn diffs_a_turn_without_touching_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        run(root, &["init", "-q"]).await;
        run(root, &["config", "user.email", "t@t"]).await;
        run(root, &["config", "user.name", "t"]).await;
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        std::fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        run(root, &["add", "-A"]).await;
        run(root, &["commit", "-qm", "init"]).await;
        // Uncommitted work from before the turn is not the turn's.
        std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();

        let base = take(root).await.expect("snapshot");
        std::fs::write(root.join("a.txt"), "one\nTWO\n").unwrap();
        std::fs::write(root.join("new.txt"), "hi\n").unwrap();
        std::fs::write(root.join("ignored.txt"), "x\n").unwrap();
        let head = take(root).await.expect("snapshot");

        let text = diff(root, &base, &head).await.unwrap();
        assert!(text.contains("-two") && text.contains("+TWO"), "{text}");
        assert!(text.contains("+++ b/new.txt"), "{text}");
        assert!(!text.contains("ignored.txt"), "{text}");
        assert_eq!(diff(root, &head, &head).await.unwrap(), "");

        // The real index still has only the committed content.
        let staged = Command::new("git")
            .args(["diff", "--cached", "--name-only"])
            .current_dir(root)
            .output()
            .await
            .unwrap();
        assert!(staged.stdout.is_empty());
    }

    #[tokio::test]
    async fn not_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        assert!(take(dir.path()).await.is_none());
    }

    #[test]
    fn cuts_at_a_file_boundary() {
        let diff = "diff --git a/a b/a\n+1\ndiff --git a/b b/b\n+2222222\n".to_owned();
        assert_eq!(cut(diff.clone(), 1000), diff);
        assert_eq!(cut(diff, 30), "diff --git a/a b/a\n+1\n");
    }
}
