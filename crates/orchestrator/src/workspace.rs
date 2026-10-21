//! Where tasks run and how their results are merged: git worktrees (docs/DESIGN.md §8).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use crossagents_core::CoreError;
use tokio::process::Command;

/// Isolated working copies for a run and its tasks.
#[async_trait]
pub trait WorkspaceProvider: Send + Sync {
    /// The branch `repo` currently has checked out.
    async fn current_branch(&self, repo: &Path) -> Result<String, CoreError>;
    /// Creates `branch` from `base` in a new working copy at `path`.
    async fn create(
        &self,
        repo: &Path,
        branch: &str,
        base: &str,
        path: &Path,
    ) -> Result<(), CoreError>;
    /// Commits everything in `path`. Returns `false` if there was nothing to commit.
    async fn commit_all(&self, path: &Path, message: &str) -> Result<bool, CoreError>;
    /// Summary of what `path` changed relative to `base`.
    async fn diff_stat(&self, path: &Path, base: &str) -> Result<String, CoreError>;
    /// What `path` changed relative to `base`, committed or not (new files included),
    /// limited to `paths` when given. A unified diff.
    async fn diff(&self, path: &Path, base: &str, paths: &[String]) -> Result<String, CoreError>;
    /// Merges `branch` into the branch checked out at `into`. On conflict the merge is
    /// aborted and the error describes it: the conflicting files and their conflict hunks.
    async fn merge(&self, into: &Path, branch: &str, message: &str) -> Result<(), CoreError>;
    /// Removes the working copy at `path`; with `delete_branch`, also `branch`.
    async fn remove(&self, repo: &Path, path: &Path, branch: Option<&str>)
    -> Result<(), CoreError>;
}

/// Bytes of conflict hunks a failed merge reports.
const CONFLICT_DETAIL: usize = 4_000;

/// [`WorkspaceProvider`] using `git worktree`.
#[derive(Debug, Clone, Default)]
pub struct GitWorkspaces;

const AUTHOR: [&str; 4] = [
    "-c",
    "user.name=CrossAgents",
    "-c",
    "user.email=crossagents@localhost",
];

async fn git(dir: &Path, args: &[&str]) -> Result<String, CoreError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .map_err(|e| CoreError::Agent(format!("cannot run git: {e}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if output.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(CoreError::Agent(format!(
            "git {} failed: {}",
            args.join(" "),
            if stderr.is_empty() { stdout } else { stderr }
        )))
    }
}

fn path_arg(path: &Path) -> String {
    path.display().to_string()
}

#[async_trait]
impl WorkspaceProvider for GitWorkspaces {
    async fn current_branch(&self, repo: &Path) -> Result<String, CoreError> {
        let branch = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
            .await
            .map_err(|e| {
                CoreError::Conflict(format!(
                    "{} must be a git repository with at least one commit ({e})",
                    repo.display()
                ))
            })?;
        if branch == "HEAD" {
            return Err(CoreError::Conflict(format!(
                "{} is in detached HEAD state; check out a branch first",
                repo.display()
            )));
        }
        Ok(branch)
    }

    async fn create(
        &self,
        repo: &Path,
        branch: &str,
        base: &str,
        path: &Path,
    ) -> Result<(), CoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Storage(e.to_string()))?;
        }
        git(
            repo,
            &["worktree", "add", "-b", branch, &path_arg(path), base],
        )
        .await?;
        Ok(())
    }

    async fn commit_all(&self, path: &Path, message: &str) -> Result<bool, CoreError> {
        git(path, &["add", "-A"]).await?;
        let staged = git(path, &["diff", "--cached", "--name-only"]).await?;
        if staged.is_empty() {
            return Ok(false);
        }
        let mut args: Vec<&str> = AUTHOR.to_vec();
        args.extend(["commit", "--no-verify", "-q", "-m", message]);
        git(path, &args).await?;
        Ok(true)
    }

    async fn diff_stat(&self, path: &Path, base: &str) -> Result<String, CoreError> {
        git(path, &["diff", "--stat", &format!("{base}...HEAD")]).await
    }

    async fn diff(&self, path: &Path, base: &str, paths: &[String]) -> Result<String, CoreError> {
        // Record new files as "intended to add" so they show up; nothing is staged.
        git(path, &["add", "-A", "--intent-to-add"]).await?;
        let merge_base = git(path, &["merge-base", base, "HEAD"]).await?;
        let mut args = vec!["diff", merge_base.as_str()];
        if !paths.is_empty() {
            args.push("--");
            args.extend(paths.iter().map(String::as_str));
        }
        git(path, &args).await
    }

    async fn merge(&self, into: &Path, branch: &str, message: &str) -> Result<(), CoreError> {
        let mut args: Vec<&str> = AUTHOR.to_vec();
        args.extend([
            "merge",
            "--no-ff",
            "--no-verify",
            "-q",
            "-m",
            message,
            branch,
        ]);
        if let Err(err) = git(into, &args).await {
            // Say what conflicts before putting the tree back.
            let files = git(into, &["diff", "--name-only", "--diff-filter=U"])
                .await
                .unwrap_or_default();
            let mut hunks = git(into, &["diff"]).await.unwrap_or_default();
            if hunks.len() > CONFLICT_DETAIL {
                let mut end = CONFLICT_DETAIL;
                while !hunks.is_char_boundary(end) {
                    end -= 1;
                }
                hunks.truncate(end);
                hunks.push_str("\n…");
            }
            let _ = git(into, &["merge", "--abort"]).await;
            return Err(CoreError::Conflict(if files.is_empty() {
                format!("merge of {branch} failed: {err}")
            } else {
                format!("merge of {branch} conflicts in:\n{files}\n\n{hunks}")
            }));
        }
        Ok(())
    }

    async fn remove(
        &self,
        repo: &Path,
        path: &Path,
        branch: Option<&str>,
    ) -> Result<(), CoreError> {
        if path.exists() {
            git(repo, &["worktree", "remove", "--force", &path_arg(path)]).await?;
        }
        if let Some(branch) = branch {
            git(repo, &["branch", "-D", branch]).await?;
        }
        Ok(())
    }
}

/// `<home>/worktrees/<run>/<name>`.
pub fn worktree_path(root: &Path, run: &str, name: &str) -> PathBuf {
    root.join(run).join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"])
            .await
            .unwrap();
        std::fs::write(dir.path().join("README"), "hi\n").unwrap();
        let ws = GitWorkspaces;
        assert!(ws.commit_all(dir.path(), "init").await.unwrap());
        dir
    }

    #[tokio::test]
    async fn worktree_commit_merge_cycle() {
        let repo = repo().await;
        let trees = tempfile::tempdir().unwrap();
        let ws = GitWorkspaces;
        assert_eq!(ws.current_branch(repo.path()).await.unwrap(), "main");

        let integration = trees.path().join("integration");
        ws.create(repo.path(), "ca/run", "main", &integration)
            .await
            .unwrap();
        let task = trees.path().join("task");
        ws.create(repo.path(), "ca/run-a", "ca/run", &task)
            .await
            .unwrap();

        assert!(!ws.commit_all(&task, "nothing").await.unwrap());
        std::fs::write(task.join("a.txt"), "a\n").unwrap();
        assert!(ws.commit_all(&task, "task a").await.unwrap());
        let stat = ws.diff_stat(&task, "ca/run").await.unwrap();
        assert!(stat.contains("a.txt"), "{stat}");
        // Uncommitted and new files show in the full diff.
        std::fs::write(task.join("b.txt"), "b\n").unwrap();
        let diff = ws.diff(&task, "ca/run", &[]).await.unwrap();
        assert!(diff.contains("+a") && diff.contains("b.txt"), "{diff}");
        let only_b = ws.diff(&task, "ca/run", &["b.txt".into()]).await.unwrap();
        assert!(!only_b.contains("a.txt"), "{only_b}");
        std::fs::remove_file(task.join("b.txt")).unwrap();

        ws.merge(&integration, "ca/run-a", "merge a").await.unwrap();
        assert!(integration.join("a.txt").exists());

        ws.remove(repo.path(), &task, Some("ca/run-a"))
            .await
            .unwrap();
        assert!(!task.exists());
    }

    #[tokio::test]
    async fn conflicting_merge_is_aborted() {
        let repo = repo().await;
        let trees = tempfile::tempdir().unwrap();
        let ws = GitWorkspaces;
        let integration = trees.path().join("i");
        ws.create(repo.path(), "ca/r", "main", &integration)
            .await
            .unwrap();
        for (name, content) in [("x", "one\n"), ("y", "two\n")] {
            let path = trees.path().join(name);
            ws.create(repo.path(), &format!("ca/r-{name}"), "ca/r", &path)
                .await
                .unwrap();
            std::fs::write(path.join("README"), content).unwrap();
            ws.commit_all(&path, name).await.unwrap();
        }
        ws.merge(&integration, "ca/r-x", "x").await.unwrap();
        let err = ws.merge(&integration, "ca/r-y", "y").await.unwrap_err();
        assert!(matches!(err, CoreError::Conflict(_)), "{err}");
        assert!(err.to_string().contains("conflicts in:\nREADME"), "{err}");
        // The integration tree is clean again.
        let status = git(&integration, &["status", "--porcelain"]).await.unwrap();
        assert!(status.is_empty(), "{status}");
    }

    #[tokio::test]
    async fn non_repo_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(GitWorkspaces.current_branch(dir.path()).await.is_err());
    }
}
