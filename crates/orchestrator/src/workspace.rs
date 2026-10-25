//! Where tasks run and how their results are merged: git worktrees (docs/DESIGN.md §8), or,
//! for a directory outside version control, the directory itself.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use crossagents_core::process::NoWindow;
use crossagents_core::{CoreError, LandMode};
use tokio::process::Command;

/// Isolated working copies for a run and its tasks.
#[async_trait]
pub trait WorkspaceProvider: Send + Sync {
    /// Whether `dir` is in a git repository with at least one commit. Runs elsewhere
    /// work in place.
    async fn versioned(&self, dir: &Path) -> bool;
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
    /// What `branch` adds on top of `base` in `repo` (`git diff base...branch`): what landing
    /// it would bring into `base`.
    async fn branch_diff(&self, repo: &Path, base: &str, branch: &str)
    -> Result<String, CoreError>;
    /// Merges `branch` into the branch checked out at `into`. On conflict the merge is
    /// aborted and the error describes it: the conflicting files and their conflict hunks.
    async fn merge(&self, into: &Path, branch: &str, message: &str) -> Result<(), CoreError>;
    /// Brings `branch` into `base` in `repo` (merge or squash), or opens a pull request.
    /// Merging never touches a checkout with uncommitted changes. Returns the commit or the
    /// pull request's URL.
    async fn land(
        &self,
        repo: &Path,
        base: &str,
        branch: &str,
        mode: LandMode,
        message: &str,
    ) -> Result<String, CoreError>;
    /// A working copy of the existing `branch` at `path`.
    async fn checkout(&self, repo: &Path, branch: &str, path: &Path) -> Result<(), CoreError>;
    /// Merges what `base` gained into the branch checked out at `into`, before landing
    /// (docs/tasks/DOGFOOD.md §4). On conflict nothing changes and the conflicts are told.
    async fn sync_base(&self, into: &Path, base: &str) -> Result<Synced, CoreError>;
    /// Removes the working copy at `path`; with `delete_branch`, also `branch`.
    async fn remove(&self, repo: &Path, path: &Path, branch: Option<&str>)
    -> Result<(), CoreError>;
}

/// Bytes of conflict hunks a failed merge reports.
const CONFLICT_DETAIL: usize = 4_000;

/// What bringing the base branch into a run branch did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Synced {
    /// The base gained nothing the branch lacks.
    UpToDate,
    /// Merged in cleanly (a merge commit on the branch).
    Merged,
    /// It conflicts; the merge was undone. The conflicting files and their hunks.
    Conflicts { files: Vec<String>, detail: String },
}

/// [`WorkspaceProvider`] using `git worktree`.
#[derive(Debug, Clone, Default)]
pub struct GitWorkspaces;

/// Untracked files the hub never adds: caches and build output an agent's commands leave
/// behind, which a repository without a .gitignore would otherwise take into its history.
/// Tracked files are committed as they are.
const NEVER_ADDED: [&str; 8] = [
    ":(exclude,glob)**/__pycache__/**",
    ":(exclude,glob)**/*.py[co]",
    ":(exclude,glob)**/.pytest_cache/**",
    ":(exclude,glob)**/.mypy_cache/**",
    ":(exclude,glob)**/.ruff_cache/**",
    ":(exclude,glob)**/node_modules/**",
    ":(exclude,glob)**/.DS_Store",
    ":(exclude,glob)**/Thumbs.db",
];

/// `git add` for every change in `path`: tracked files as they are, new files but for
/// [`NEVER_ADDED`]. `extra` goes before the pathspec (`--intent-to-add`).
async fn add_changes(path: &Path, extra: &[&str]) -> Result<(), CoreError> {
    let mut tracked = vec!["add", "-u"];
    tracked.extend(extra);
    git_locked(path, &tracked).await?;
    let mut args = vec!["add", "-A"];
    args.extend(extra);
    args.extend(["--", "."]);
    args.extend(NEVER_ADDED);
    git_locked(path, &args).await?;
    Ok(())
}

const AUTHOR: [&str; 4] = [
    "-c",
    "user.name=CrossAgents",
    "-c",
    "user.email=crossagents@localhost",
];

async fn git(dir: &Path, args: &[&str]) -> Result<String, CoreError> {
    let output = Command::new("git")
        .no_window()
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

/// Like [`git`], waiting (up to two seconds) while another git process holds the
/// checkout's index: the hub commits a reported task in the background while others may
/// look at its diff.
async fn git_locked(dir: &Path, args: &[&str]) -> Result<String, CoreError> {
    let mut tries = 0;
    loop {
        match git(dir, args).await {
            Err(err) if err.to_string().contains("index.lock") && tries < 20 => {
                tries += 1;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            other => return other,
        }
    }
}

fn path_arg(path: &Path) -> String {
    path.display().to_string()
}

/// Merges `branch` into the checkout at `into`. A conflict is undone and comes back as the
/// conflicting files and their hunks (cut to [`CONFLICT_DETAIL`]); other failures are errors.
async fn merge_or_undo(
    into: &Path,
    branch: &str,
    message: &str,
) -> Result<Result<(), (Vec<String>, String)>, CoreError> {
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
    let Err(err) = git(into, &args).await else {
        return Ok(Ok(()));
    };
    // Say what conflicts before putting the tree back.
    let files: Vec<String> = git(into, &["diff", "--name-only", "--diff-filter=U"])
        .await
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();
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
    if files.is_empty() {
        return Err(CoreError::Conflict(format!(
            "merge of {branch} failed: {err}"
        )));
    }
    Ok(Err((files, hunks)))
}

#[async_trait]
impl WorkspaceProvider for GitWorkspaces {
    async fn versioned(&self, dir: &Path) -> bool {
        git(dir, &["rev-parse", "--verify", "-q", "HEAD"])
            .await
            .is_ok()
    }

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
        add_changes(path, &[]).await?;
        let staged = git(path, &["diff", "--cached", "--name-only"]).await?;
        if staged.is_empty() {
            return Ok(false);
        }
        let mut args: Vec<&str> = AUTHOR.to_vec();
        args.extend(["commit", "--no-verify", "-q", "-m", message]);
        git_locked(path, &args).await?;
        Ok(true)
    }

    async fn diff_stat(&self, path: &Path, base: &str) -> Result<String, CoreError> {
        git(path, &["diff", "--stat", &format!("{base}...HEAD")]).await
    }

    async fn diff(&self, path: &Path, base: &str, paths: &[String]) -> Result<String, CoreError> {
        // Record new files as "intended to add" so they show up; nothing is staged. The
        // worktree's index may be busy for a moment (the hub commits a reported task in the
        // background): wait for it rather than fail.
        add_changes(path, &["--intent-to-add"]).await?;
        let merge_base = git(path, &["merge-base", base, "HEAD"]).await?;
        let mut args = vec!["diff", merge_base.as_str()];
        if !paths.is_empty() {
            args.push("--");
            args.extend(paths.iter().map(String::as_str));
        }
        git(path, &args).await
    }

    async fn branch_diff(
        &self,
        repo: &Path,
        base: &str,
        branch: &str,
    ) -> Result<String, CoreError> {
        git(repo, &["diff", &format!("{base}...{branch}")]).await
    }

    async fn merge(&self, into: &Path, branch: &str, message: &str) -> Result<(), CoreError> {
        match merge_or_undo(into, branch, message).await? {
            Ok(()) => Ok(()),
            Err((files, hunks)) => Err(CoreError::Conflict(format!(
                "merge of {branch} conflicts in:\n{}\n\n{hunks}",
                files.join("\n")
            ))),
        }
    }

    async fn checkout(&self, repo: &Path, branch: &str, path: &Path) -> Result<(), CoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Storage(e.to_string()))?;
        }
        git(repo, &["worktree", "add", &path_arg(path), branch]).await?;
        Ok(())
    }

    async fn sync_base(&self, into: &Path, base: &str) -> Result<Synced, CoreError> {
        let behind = git(into, &["rev-list", "--count", &format!("HEAD..{base}")]).await?;
        if behind.trim() == "0" {
            return Ok(Synced::UpToDate);
        }
        let message = format!("crossagents: bring {base} into the run before landing");
        Ok(match merge_or_undo(into, base, &message).await? {
            Ok(()) => Synced::Merged,
            Err((files, detail)) => Synced::Conflicts { files, detail },
        })
    }

    async fn land(
        &self,
        repo: &Path,
        base: &str,
        branch: &str,
        mode: LandMode,
        message: &str,
    ) -> Result<String, CoreError> {
        if mode == LandMode::Pr {
            git(repo, &["push", "-u", "origin", branch]).await?;
            let output = Command::new("gh")
                .no_window()
                .args([
                    "pr", "create", "--base", base, "--head", branch, "--title", message, "--body",
                    message,
                ])
                .current_dir(repo)
                .output()
                .await
                .map_err(|e| CoreError::Agent(format!("cannot run gh: {e}")))?;
            let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !output.status.success() {
                return Err(CoreError::Agent(format!(
                    "gh pr create failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            return Ok(text.lines().last().unwrap_or_default().to_owned());
        }
        // Merge where base is checked out (if that checkout is clean), else in a temporary
        // worktree of base.
        let here = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).await? == base;
        let temp = if here {
            let dirty = git(repo, &["status", "--porcelain", "--untracked-files=no"]).await?;
            if !dirty.is_empty() {
                return Err(CoreError::Conflict(format!(
                    "{} has uncommitted changes on {base}; commit or stash them first:\n{dirty}",
                    repo.display()
                )));
            }
            None
        } else {
            let dir = tempfile_dir(repo)?;
            git(repo, &["worktree", "add", &path_arg(&dir), base])
                .await
                .map_err(|e| {
                    CoreError::Conflict(format!("cannot check out {base} to land on it: {e}"))
                })?;
            Some(dir)
        };
        let dir = temp.clone().unwrap_or_else(|| repo.to_path_buf());
        let result = async {
            let mut args: Vec<&str> = AUTHOR.to_vec();
            match mode {
                LandMode::Merge => args.extend([
                    "merge",
                    "--no-ff",
                    "--no-verify",
                    "-q",
                    "-m",
                    message,
                    branch,
                ]),
                _ => args.extend(["merge", "--squash", "-q", branch]),
            }
            if let Err(err) = git(&dir, &args).await {
                let _ = git(&dir, &["merge", "--abort"]).await;
                let _ = git(&dir, &["reset", "--merge"]).await;
                return Err(CoreError::Conflict(format!(
                    "cannot land {branch} on {base}: {err}"
                )));
            }
            if mode == LandMode::Squash {
                let mut args: Vec<&str> = AUTHOR.to_vec();
                args.extend(["commit", "--no-verify", "-q", "-m", message]);
                git(&dir, &args).await?;
            }
            git(&dir, &["rev-parse", "--short", "HEAD"]).await
        }
        .await;
        if let Some(dir) = temp {
            let _ = git(repo, &["worktree", "remove", "--force", &path_arg(&dir)]).await;
        }
        result
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

impl GitWorkspaces {
    /// Puts `dir` under version control: `git init` and a first commit of what it holds,
    /// as the person's git identity when one is set. Refuses a directory already versioned.
    pub async fn init(&self, dir: &Path) -> Result<(), CoreError> {
        if !dir.is_dir() {
            return Err(CoreError::Conflict(format!(
                "{} is not a directory",
                dir.display()
            )));
        }
        if self.versioned(dir).await {
            return Err(CoreError::Conflict(format!(
                "{} is already a git repository",
                dir.display()
            )));
        }
        git(dir, &["init", "-q"]).await?;
        add_changes(dir, &[]).await?;
        let commit = [
            "commit",
            "--no-verify",
            "--allow-empty",
            "-q",
            "-m",
            "Initial commit",
        ];
        if git(dir, &commit).await.is_err() {
            let mut args: Vec<&str> = AUTHOR.to_vec();
            args.extend(commit);
            git(dir, &args).await?;
        }
        Ok(())
    }
}

/// [`WorkspaceProvider`] for a directory outside version control: every task works in the
/// directory itself, so there is nothing to create, commit, merge, land or remove.
#[derive(Debug, Clone, Default)]
pub struct InPlaceWorkspaces;

const NOT_VERSIONED: &str =
    "this directory is not under version control, so there is no diff; read the files instead";

#[async_trait]
impl WorkspaceProvider for InPlaceWorkspaces {
    async fn versioned(&self, _: &Path) -> bool {
        false
    }

    async fn current_branch(&self, _: &Path) -> Result<String, CoreError> {
        Ok(String::new())
    }

    async fn create(&self, _: &Path, _: &str, _: &str, _: &Path) -> Result<(), CoreError> {
        Ok(())
    }

    async fn commit_all(&self, _: &Path, _: &str) -> Result<bool, CoreError> {
        Ok(false)
    }

    async fn diff_stat(&self, _: &Path, _: &str) -> Result<String, CoreError> {
        Err(CoreError::Conflict(NOT_VERSIONED.into()))
    }

    async fn diff(&self, _: &Path, _: &str, _: &[String]) -> Result<String, CoreError> {
        Err(CoreError::Conflict(NOT_VERSIONED.into()))
    }

    async fn branch_diff(&self, _: &Path, _: &str, _: &str) -> Result<String, CoreError> {
        Err(CoreError::Conflict(NOT_VERSIONED.into()))
    }

    async fn merge(&self, _: &Path, _: &str, _: &str) -> Result<(), CoreError> {
        Ok(())
    }

    async fn land(
        &self,
        repo: &Path,
        _: &str,
        _: &str,
        _: LandMode,
        _: &str,
    ) -> Result<String, CoreError> {
        Ok(repo.display().to_string())
    }

    async fn checkout(&self, _: &Path, _: &str, _: &Path) -> Result<(), CoreError> {
        Ok(())
    }

    async fn sync_base(&self, _: &Path, _: &str) -> Result<Synced, CoreError> {
        Ok(Synced::UpToDate)
    }

    async fn remove(&self, _: &Path, _: &Path, _: Option<&str>) -> Result<(), CoreError> {
        Ok(())
    }
}

/// A fresh directory next to the repository's git dir for a temporary worktree.
fn tempfile_dir(repo: &Path) -> Result<PathBuf, CoreError> {
    let base = std::env::temp_dir().join("crossagents-land");
    std::fs::create_dir_all(&base).map_err(|e| CoreError::Storage(e.to_string()))?;
    let name = format!(
        "{}-{}",
        repo.file_name().and_then(|n| n.to_str()).unwrap_or("repo"),
        std::process::id()
    );
    let mut dir = base.join(&name);
    let mut n = 0;
    while dir.exists() {
        n += 1;
        dir = base.join(format!("{name}-{n}"));
    }
    Ok(dir)
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
        // What running the code left behind stays out of the commit.
        std::fs::create_dir_all(task.join("pkg/__pycache__")).unwrap();
        std::fs::write(task.join("pkg/__pycache__/a.cpython-314.pyc"), "x").unwrap();
        std::fs::write(task.join("b.pyc"), "x").unwrap();
        assert!(ws.commit_all(&task, "task a").await.unwrap());
        let files = git(&task, &["ls-files"]).await.unwrap();
        assert!(files.contains("a.txt") && !files.contains("pyc"), "{files}");
        std::fs::remove_dir_all(task.join("pkg")).unwrap();
        std::fs::remove_file(task.join("b.pyc")).unwrap();
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
    async fn landing_merges_or_squashes_and_respects_dirty_checkouts() {
        let repo = repo().await;
        let trees = tempfile::tempdir().unwrap();
        let ws = GitWorkspaces;
        let run = trees.path().join("run");
        ws.create(repo.path(), "ca/run", "main", &run)
            .await
            .unwrap();
        std::fs::write(run.join("new.txt"), "x\n").unwrap();
        ws.commit_all(&run, "work").await.unwrap();

        // A dirty checkout of main is left alone.
        std::fs::write(repo.path().join("README"), "edited\n").unwrap();
        let err = ws
            .land(repo.path(), "main", "ca/run", LandMode::Merge, "land")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("uncommitted"), "{err}");
        git(repo.path(), &["checkout", "--", "README"])
            .await
            .unwrap();

        let commit = ws
            .land(repo.path(), "main", "ca/run", LandMode::Squash, "land it")
            .await
            .unwrap();
        assert!(!commit.is_empty());
        assert!(repo.path().join("new.txt").exists());
        let log = git(repo.path(), &["log", "-1", "--format=%s"])
            .await
            .unwrap();
        assert_eq!(log, "land it");
    }

    #[tokio::test]
    async fn landing_on_a_branch_that_is_not_checked_out() {
        let repo = repo().await;
        let trees = tempfile::tempdir().unwrap();
        let ws = GitWorkspaces;
        git(repo.path(), &["branch", "release"]).await.unwrap();
        let run = trees.path().join("run");
        ws.create(repo.path(), "ca/run", "release", &run)
            .await
            .unwrap();
        std::fs::write(run.join("r.txt"), "r\n").unwrap();
        ws.commit_all(&run, "work").await.unwrap();
        ws.land(repo.path(), "release", "ca/run", LandMode::Merge, "land")
            .await
            .unwrap();
        let files = git(repo.path(), &["ls-tree", "--name-only", "release"])
            .await
            .unwrap();
        assert!(files.contains("r.txt"), "{files}");
        // main's checkout did not move.
        assert!(!repo.path().join("r.txt").exists());
    }

    #[tokio::test]
    async fn non_repo_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(GitWorkspaces.current_branch(dir.path()).await.is_err());
        assert!(!GitWorkspaces.versioned(dir.path()).await);
        // A repository without a commit is not one to branch from either.
        git(dir.path(), &["init", "-q"]).await.unwrap();
        assert!(!GitWorkspaces.versioned(dir.path()).await);
        assert!(GitWorkspaces.versioned(repo().await.path()).await);
    }

    #[tokio::test]
    async fn init_versions_a_plain_directory_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("cv.md"),
            "me
",
        )
        .unwrap();
        GitWorkspaces.init(dir.path()).await.unwrap();
        assert!(GitWorkspaces.versioned(dir.path()).await);
        let files = git(dir.path(), &["ls-files"]).await.unwrap();
        assert_eq!(files, "cv.md");
        assert!(GitWorkspaces.init(dir.path()).await.is_err());
    }

    #[tokio::test]
    async fn in_place_never_touches_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("cv.md"),
            "me
",
        )
        .unwrap();
        let ws = InPlaceWorkspaces;
        ws.create(dir.path(), "b", "", dir.path()).await.unwrap();
        assert!(!ws.commit_all(dir.path(), "x").await.unwrap());
        assert!(ws.diff(dir.path(), "", &[]).await.is_err());
        ws.merge(dir.path(), "b", "m").await.unwrap();
        ws.remove(dir.path(), dir.path(), Some("b")).await.unwrap();
        assert!(dir.path().join("cv.md").exists());
    }
}
