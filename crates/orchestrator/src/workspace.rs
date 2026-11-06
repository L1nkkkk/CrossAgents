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
    /// Create and check out a branch in an existing checkout.
    async fn create_branch(&self, path: &Path, branch: &str, base: &str) -> Result<(), CoreError> {
        let _ = (path, branch, base);
        Err(CoreError::Conflict("branch creation is unsupported by this workspace provider".into()))
    }
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
    /// Merging in a checkout of `base` leaves what is not committed there as it is, and
    /// refuses when it is staged or in a file the branch changes too. Returns the commit or the
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
    /// [`Self::land`], in a directory another run's lead is working in (`held`): refused
    /// before anything is written there, so that a merge never starts in a working copy
    /// someone else is changing (docs/tasks/LEAD.md §7). This is the check
    /// [`check_checkout`] makes for the person's own uncommitted work, taken first: the
    /// person's directory is held by the hub, which a working copy's git state cannot say.
    async fn land_held(
        &self,
        repo: &Path,
        base: &str,
        branch: &str,
        mode: LandMode,
        message: &str,
        held: Option<&crossagents_core::Run>,
    ) -> Result<String, CoreError> {
        if let Some(holder) = held {
            return Err(CoreError::Conflict(crate::dirlock::busy(holder, repo)));
        }
        self.land(repo, base, branch, mode, message).await
    }
    /// Merges what `base` gained into the branch checked out at `into`, before landing
    /// (docs/tasks/DOGFOOD.md §4). On conflict nothing changes and the conflicts are told.
    async fn sync_base(&self, into: &Path, base: &str) -> Result<Synced, CoreError>;
    /// Removes the working copy at `path`; with `delete_branch`, also `branch`.
    async fn remove(&self, repo: &Path, path: &Path, branch: Option<&str>)
    -> Result<(), CoreError>;
    /// Deletes `branch` in `repo`, whether or not a working copy has it (a merged task's
    /// branch outlives its worktree). A branch that is already gone is not an error.
    async fn delete_branch(&self, repo: &Path, branch: &str) -> Result<(), CoreError>;
    /// Whether `branch` exists in `repo`.
    async fn has_branch(&self, repo: &Path, branch: &str) -> bool {
        let _ = (repo, branch);
        false
    }
    /// The files `path` changed and did not commit: edits and untracked files, by path.
    async fn uncommitted(&self, path: &Path) -> Result<Vec<String>, CoreError>;
    /// The files `path` changed relative to `base`, committed or not, untracked included.
    async fn changed_files(&self, path: &Path, base: &str) -> Result<Vec<String>, CoreError> {
        let _ = (path, base);
        Ok(Vec::new())
    }
    /// The commit checked out at `path`; empty where there are no commits.
    async fn head(&self, path: &Path) -> Result<String, CoreError> {
        let _ = path;
        Ok(String::new())
    }
    /// The URL of `repo`'s `origin`, else of its first remote; `None` without one.
    async fn remote(&self, repo: &Path) -> Option<String> {
        let _ = repo;
        None
    }
    /// Forgets working copies of `repo` whose directory is gone (deleted by hand when
    /// removing them failed), so their branches can be checked out again.
    async fn prune(&self, repo: &Path) -> Result<(), CoreError> {
        let _ = repo;
        Ok(())
    }
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
pub(crate) const NEVER_ADDED: [&str; 8] = [
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

/// Before landing in someone's own checkout of `base`: what they have not committed stays
/// as it is, so landing goes ahead unless it would get mixed up with that. Staged changes
/// always stop it (the land commit would take them along), and so do edited or untracked
/// files that the branch changes too. Everything else is left alone by the merge.
async fn check_checkout(repo: &Path, base: &str, branch: &str) -> Result<(), CoreError> {
    let list = |text: String| -> Vec<String> {
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    };
    const PLAIN: [&str; 2] = ["-c", "core.quotePath=false"];
    let run = |args: &'static [&'static str]| async move {
        let mut all = PLAIN.to_vec();
        all.extend_from_slice(args);
        git(repo, &all).await
    };
    let staged = list(run(&["diff", "--cached", "--name-only"]).await?);
    if !staged.is_empty() {
        return Err(CoreError::Conflict(format!(
            "{} has staged changes on {base}; landing would commit them too. Landing refused. Call run_finish and report that the result remains on {branch} for the person to land. Do not retry landing or commit, unstage, stash or discard the person's changes:\n{}",
            repo.display(),
            staged.join("\n")
        )));
    }
    let edited = list(run(&["diff", "--name-only"]).await?);
    let untracked = list(run(&["ls-files", "--others", "--exclude-standard"]).await?);
    if edited.is_empty() && untracked.is_empty() {
        return Ok(());
    }
    // What the merge brings in: the branch's side since it left base, renames as both names.
    let range = format!("HEAD...{branch}");
    let mut args = PLAIN.to_vec();
    args.extend(["diff", "--name-only", "--no-renames", &range]);
    let incoming: std::collections::HashSet<String> =
        list(git(repo, &args).await?).into_iter().collect();
    let clash: Vec<&str> = edited
        .iter()
        .chain(&untracked)
        .filter(|file| incoming.contains(*file))
        .map(String::as_str)
        .collect();
    if clash.is_empty() {
        return Ok(());
    }
    Err(CoreError::Conflict(format!(
        "{} has uncommitted changes on {base} in files that {branch} also changes. Landing refused. Call run_finish and report that the result remains on {branch} for the person to land. Do not retry landing or commit, stash or discard the person's changes:\n{}",
        repo.display(),
        clash.join("\n")
    )))
}

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
    async fn create_branch(&self, path: &Path, branch: &str, base: &str) -> Result<(), CoreError> {
        git_locked(path, &["checkout", "-b", branch, base]).await?;
        Ok(())
    }
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
        add_changes(path, &["--intent-to-add"]).await?;
        let merge_base = git(path, &["merge-base", base, "HEAD"]).await?;
        git(path, &["diff", "--stat", &merge_base]).await
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
        // Merge where base is checked out (unless that would touch work not committed
        // there), else in a temporary worktree of base.
        let here = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).await? == base;
        let temp = if here {
            check_checkout(repo, base, branch).await?;
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

    async fn prune(&self, repo: &Path) -> Result<(), CoreError> {
        git(repo, &["worktree", "prune"]).await?;
        Ok(())
    }

    async fn has_branch(&self, repo: &Path, branch: &str) -> bool {
        git(
            repo,
            &[
                "rev-parse",
                "--verify",
                "-q",
                &format!("refs/heads/{branch}"),
            ],
        )
        .await
        .is_ok()
    }

    async fn delete_branch(&self, repo: &Path, branch: &str) -> Result<(), CoreError> {
        if git(
            repo,
            &[
                "rev-parse",
                "--verify",
                "-q",
                &format!("refs/heads/{branch}"),
            ],
        )
        .await
        .is_err()
        {
            return Ok(());
        }
        git(repo, &["branch", "-D", branch]).await?;
        Ok(())
    }

    async fn uncommitted(&self, path: &Path) -> Result<Vec<String>, CoreError> {
        // `XY <path>`, a rename as `<old> -> <new>`; unquoted so a path is readable.
        let out = git(
            path,
            &["-c", "core.quotePath=false", "status", "--porcelain"],
        )
        .await?;
        // `git` trims the output, which takes the first line's leading blank status column:
        // split at the space after the status letters rather than at a fixed column.
        Ok(out
            .lines()
            .filter_map(|line| line.trim_start().split_once(' ').map(|(_, rest)| rest))
            .map(|rest| rest.rsplit(" -> ").next().unwrap_or(rest).trim())
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect())
    }

    async fn changed_files(&self, path: &Path, base: &str) -> Result<Vec<String>, CoreError> {
        let merge_base = git(path, &["merge-base", base, "HEAD"]).await?;
        let tracked = git(
            path,
            &[
                "-c",
                "core.quotePath=false",
                "diff",
                "--name-only",
                &merge_base,
            ],
        )
        .await?;
        let mut files: Vec<String> = tracked.lines().map(str::to_owned).collect();
        for file in self.uncommitted(path).await? {
            if !files.contains(&file) {
                files.push(file);
            }
        }
        Ok(files)
    }

    async fn head(&self, path: &Path) -> Result<String, CoreError> {
        git(path, &["rev-parse", "HEAD"]).await
    }

    async fn remote(&self, repo: &Path) -> Option<String> {
        if let Ok(url) = git(repo, &["remote", "get-url", "origin"]).await {
            return Some(url);
        }
        let names = git(repo, &["remote"]).await.ok()?;
        let first = names.lines().next()?.trim().to_owned();
        git(repo, &["remote", "get-url", &first]).await.ok()
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

    async fn delete_branch(&self, _: &Path, _: &str) -> Result<(), CoreError> {
        Ok(())
    }

    async fn uncommitted(&self, _: &Path) -> Result<Vec<String>, CoreError> {
        Ok(Vec::new())
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

/// The directory a run's build directories live in, under its worktrees: `<root>/<run>/targets`.
pub const SHARED_TARGETS: &str = "targets";

/// `<root>/<run>/targets/<tree>`: one build directory per worktree - the run's checkout is
/// `integration`, a task's is its key. The worker and the hub's checks in the same worktree
/// share it; another worktree never does, because workspace members have the same package
/// names and versions there, so their artefacts overwrite each other (fake "unresolved import"
/// errors: docs/TODO.md §十六).
pub fn target_dir(root: &Path, run: &str, tree: &str) -> PathBuf {
    worktree_path(root, run, SHARED_TARGETS).join(tree)
}

/// The environment agents and the hub's checks get when `cwd` is in one of a run's worktrees
/// under `root`: what the checkout's presets and `crossagents.toml` declare (docs/tasks/
/// TOOLCHAIN.md §3), placeholders filled in, its resources in [`project::RESOURCES_ENV`]
/// (docs/tasks/LOCKS.md §3), and — when the project keeps any shims — `shims` at the front of
/// `PATH` (§4.6). The same for both, or they would build into different directories. Empty
/// outside a run worktree.
pub fn worktree_env(root: &Path, cwd: &Path, shims: Option<&Path>) -> Vec<(String, String)> {
    let Some(env) = worktree_run_env(root, cwd) else {
        return Vec::new();
    };
    let mut vars: Vec<(String, String)> = env.vars.into_iter().map(|v| (v.name, v.value)).collect();
    // The executor's commands in an agent's shell and the hub's checks hold the same
    // resources, at the same paths.
    if !env.resources.is_empty() {
        vars.push((
            crate::project::RESOURCES_ENV.to_owned(),
            crate::project::resources_json(&env.resources),
        ));
    }
    // `cargo` typed in the worktree is the executor's (§4.6). A project that shims nothing
    // gets no shim directory at all, which is what `[run] shims = []` asks for.
    if let Some(dir) = shims
        && !env.shims.is_empty()
    {
        vars.push(("PATH".to_owned(), path_with(dir)));
    }
    vars
}

/// `dir` in front of this process's `PATH`: the original value, not a replacement.
fn path_with(dir: &Path) -> String {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let parts = std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&current));
    match std::env::join_paths(parts) {
        Ok(path) => path.to_string_lossy().into_owned(),
        // A `PATH` entry with a separator in it cannot be joined; the shim alone still works.
        Err(_) => dir.display().to_string(),
    }
}

/// [`worktree_env`] with where each variable comes from, and what was left unset.
pub fn worktree_run_env(root: &Path, cwd: &Path) -> Option<crate::project::RunEnv> {
    let (checkout, build) = worktree_dirs(root, cwd)?;
    let mut env = crate::project::run_env(&checkout).unwrap_or_else(|err| {
        tracing::warn!(checkout = %checkout.display(), error = %err, "using the presets alone");
        crate::project::preset_env(&checkout)
    });
    for var in &mut env.vars {
        if var.value.contains("{build}") {
            let _ = std::fs::create_dir_all(&build);
        }
        var.value = crate::project::fill(&var.value, &build, &checkout);
    }
    if env.resources.iter().any(|r| r.path.contains("{build}")) {
        let _ = std::fs::create_dir_all(&build);
    }
    crate::project::fill_resources(&mut env.resources, &build, &checkout);
    Some(env)
}

/// The run worktree `cwd` is in under `root`, and its build directory ([`target_dir`]). The
/// build directory lies outside every checkout, so nothing commits it (DOGFOOD.md §9), and is
/// absolute: a relative one would follow every `cd`. `None` for a directory that is nobody's
/// checkout: the build directories themselves, the run's own directory, the old shared ones,
/// and everything outside.
pub fn worktree_dirs(root: &Path, cwd: &Path) -> Option<(PathBuf, PathBuf)> {
    let canon = |p: &Path| dunce::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (root, cwd) = (canon(root), canon(cwd));
    let mut parts = cwd.strip_prefix(&root).ok()?.components();
    let run = parts.next()?.as_os_str().to_string_lossy().into_owned();
    // `<root>/targets/<repo>-<hash>`: where every run of a repository used to build.
    if run == SHARED_TARGETS {
        return None;
    }
    let tree = parts.next()?.as_os_str().to_string_lossy().into_owned();
    // `<root>/<run>` itself is no checkout, and neither are `<run>/targets` (nor a shell in
    // the old `<run>/target`).
    if tree == SHARED_TARGETS || tree == "target" {
        return None;
    }
    Some((
        worktree_path(&root, &run, &tree),
        target_dir(&root, &run, &tree),
    ))
}

/// `<name>-<hash>` of the repository `worktree` is a working copy of: its `.git` file names
/// the worktree's git directory, whose `commondir` (or grandparent) is the repository's.
fn repo_key(worktree: &Path) -> Option<String> {
    let common = common_dir(worktree)?;
    let repo = if common.file_name()? == ".git" {
        common.parent()?
    } else {
        &common
    };
    let name: String = repo
        .file_name()?
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // FNV-1a: stable across builds, unlike `DefaultHasher`.
    let mut path = common.to_string_lossy().into_owned();
    if cfg!(windows) {
        path = path.to_lowercase();
    }
    let hash = path.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    Some(format!("{name}-{:08x}", hash & 0xffff_ffff))
}

/// The repository's git directory, from `worktree`'s `.git` file.
fn common_dir(worktree: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(worktree.join(".git")).ok()?;
    let gitdir = worktree.join(text.strip_prefix("gitdir:")?.trim());
    let common = match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(rel) => gitdir.join(rel.trim()),
        Err(_) => gitdir.parent()?.parent()?.to_path_buf(),
    };
    dunce::canonicalize(common).ok()
}

/// The main checkout `worktree` is a working copy of (its git directory's parent; a bare
/// repository is its own).
pub fn common_repo(worktree: &Path) -> Option<PathBuf> {
    let common = common_dir(worktree)?;
    if common.file_name()? == ".git" {
        common.parent().map(Path::to_path_buf)
    } else {
        Some(common)
    }
}

/// Removes each old shared build directory (`<root>/targets/*`, where every run of a
/// repository used to build) that none of them uses any more. Nothing writes them now
/// (`worktree_env`), so this only finishes the old layout off: `limit` is 0 in practice, and a
/// directory some run's worktree still maps to its repository stays. Renamed first, so a build
/// that starts meanwhile gets a fresh directory, and a directory a process still holds (Windows
/// refuses the rename) stays. Returns what went, with its size. Blocking: walks the
/// directories to size them.
pub fn prune_shared_targets(root: &Path, limit: u64) -> Vec<(PathBuf, u64)> {
    let shared = root.join(SHARED_TARGETS);
    let Ok(entries) = std::fs::read_dir(&shared) else {
        return Vec::new();
    };
    let entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    // Left by an earlier pass that could not finish deleting.
    for trash in entries.iter().filter(|p| is_trash(p)) {
        let _ = std::fs::remove_dir_all(trash);
    }
    let in_use = keys_in_use(root);
    let mut removed = Vec::new();
    for dir in entries.iter().filter(|p| p.is_dir() && !is_trash(p)) {
        let Some(key) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        if in_use.contains(&key) {
            continue;
        }
        let size = dir_size(dir);
        if size <= limit {
            continue;
        }
        if let Some(gone) = remove_build_dir(dir, size) {
            removed.push(gone);
        }
    }
    removed
}

fn is_trash(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with(".trash-"))
}

/// Removes one build directory, renamed to `.trash-*` first so a build that starts meanwhile
/// gets a fresh one, and a directory a process still holds (Windows refuses the rename) stays
/// for the next pass. Renaming proves nothing about use: what may go is decided by the callers
/// (`prune_run_targets`: its checkout is gone; on Unix a rename never fails, so the rules are
/// what keeps a directory a build is writing to).
fn remove_build_dir(dir: &Path, size: u64) -> Option<(PathBuf, u64)> {
    let name = dir.file_name()?.to_string_lossy().into_owned();
    let trash = dir
        .parent()?
        .join(format!(".trash-{name}-{}", std::process::id()));
    if let Err(err) = std::fs::rename(dir, &trash) {
        // Not silent: on Windows a process still holding a file refuses the rename. The
        // directory stays, and the hub's startup sweep (`prune_build_cache`) tries again.
        tracing::warn!(dir = %dir.display(), error = %err, "build directory busy; keeping it, retrying at the next startup");
        return None;
    }
    if let Err(err) = std::fs::remove_dir_all(&trash) {
        tracing::warn!(dir = %trash.display(), error = %err, "cannot finish removing a build directory; retrying next time");
    }
    Some((dir.to_path_buf(), size))
}

/// Removes the build directories under `<root>/<run>/targets` that nothing uses any more: the
/// worktree they belong to is gone and `keep` (a task still being verified, say) does not name
/// them. Leftovers of a delete that could not finish (`.trash-*`) go too. Once none are left,
/// the `targets` directory itself goes. Blocking: sizes what it removes.
pub fn prune_run_targets(
    root: &Path,
    run: &str,
    keep: &std::collections::HashSet<String>,
) -> Vec<(PathBuf, u64)> {
    let targets = worktree_path(root, run, SHARED_TARGETS);
    let Ok(entries) = std::fs::read_dir(&targets) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    let mut left = 0;
    for dir in entries.flatten().map(|e| e.path()) {
        if is_trash(&dir) {
            let _ = std::fs::remove_dir_all(&dir);
            continue;
        }
        if !dir.is_dir() {
            continue;
        }
        let Some(tree) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        // Its checkout is still there, or it is being verified: whatever builds there needs it.
        if keep.contains(&tree) || worktree_path(root, run, &tree).exists() {
            left += 1;
            continue;
        }
        let size = dir_size(&dir);
        match remove_build_dir(&dir, size) {
            Some(gone) => removed.push(gone),
            None => left += 1,
        }
    }
    if left == 0 {
        let _ = std::fs::remove_dir(&targets);
    }
    removed
}

/// [`prune_run_targets`] for every run under `root`, each with the build directories `keep`
/// says to leave alone (run's short id → its trees).
pub fn prune_all_targets(
    root: &Path,
    keep: &std::collections::HashMap<String, std::collections::HashSet<String>>,
) -> Vec<(PathBuf, u64)> {
    let empty = std::collections::HashSet::new();
    let mut removed = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return removed;
    };
    for run in entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n != SHARED_TARGETS))
    {
        let Some(name) = run.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        removed.extend(prune_run_targets(
            root,
            &name,
            keep.get(&name).unwrap_or(&empty),
        ));
    }
    removed
}

/// Removes one worktree's build directory (`<run>/targets/<tree>`), then the `targets`
/// directory and the run's directory when nothing is left in them. Never recursive over
/// `targets`: other trees may still be building there. Its checkout is gone, which is what
/// makes this safe (the caller removes the checkout first). Blocking: sizes what it removes.
pub fn remove_target(root: &Path, run: &str, tree: &str) -> Vec<(PathBuf, u64)> {
    let mut removed = Vec::new();
    let dir = target_dir(root, run, tree);
    if dir.is_dir() {
        let size = dir_size(&dir);
        if let Some(gone) = remove_build_dir(&dir, size) {
            removed.push(gone);
        }
    }
    let _ = std::fs::remove_dir(worktree_path(root, run, SHARED_TARGETS));
    // The old fallback for a worktree whose repository could not be told; nothing writes it.
    let legacy = worktree_path(root, run, "target");
    if legacy.is_dir() && !has_other_trees(root, run) {
        let size = dir_size(&legacy);
        if let Some(gone) = remove_build_dir(&legacy, size) {
            removed.push(gone);
        }
    }
    let _ = std::fs::remove_dir(root.join(run));
    removed
}

/// Whether anything besides build directories is left in a run's directory.
fn has_other_trees(root: &Path, run: &str) -> bool {
    std::fs::read_dir(root.join(run))
        .map(|rd| {
            rd.flatten().map(|e| e.path()).any(|p| {
                p.is_dir()
                    && p.file_name()
                        .is_some_and(|n| n != "target" && n != SHARED_TARGETS && !is_trash(&p))
            })
        })
        .unwrap_or(false)
}

/// The whole build area of a run that is going away: every `<run>/targets/*` (not only the
/// ones whose checkout is gone) and the old `<run>/target`. What could not be removed is
/// returned as words; the run's directory stays when something is still in it.
pub fn remove_run_build_dirs(root: &Path, run: &str) -> Vec<(PathBuf, u64)> {
    let mut removed = Vec::new();
    let targets = worktree_path(root, run, SHARED_TARGETS);
    if let Ok(entries) = std::fs::read_dir(&targets) {
        for dir in entries.flatten().map(|e| e.path()) {
            if !dir.is_dir() {
                continue;
            }
            let size = dir_size(&dir);
            if is_trash(&dir) {
                let _ = std::fs::remove_dir_all(&dir);
                continue;
            }
            if let Some(gone) = remove_build_dir(&dir, size) {
                removed.push(gone);
            }
        }
    }
    let _ = std::fs::remove_dir(&targets);
    let legacy = worktree_path(root, run, "target");
    if legacy.is_dir() {
        let size = dir_size(&legacy);
        if let Some(gone) = remove_build_dir(&legacy, size) {
            removed.push(gone);
        }
    }
    let _ = std::fs::remove_dir(root.join(run));
    removed
}

/// The build directories of a run that has ended: every `<run>/targets/*` except the trees
/// `keep` names (a task still being verified builds there), whether or not their checkout is
/// still there - a finished run builds nothing more, and a build directory is rebuilt by the
/// next build that needs it. Leftovers of an earlier delete (`.trash-*`) go too, and
/// `targets` itself once it is empty. The checkouts and the run's directory stay. What could
/// not be removed is logged ([`remove_build_dir`]) and retried at the next startup. Blocking:
/// sizes what it removes.
pub fn remove_run_targets_except(
    root: &Path,
    run: &str,
    keep: &std::collections::HashSet<String>,
) -> Vec<(PathBuf, u64)> {
    let mut removed = Vec::new();
    let targets = worktree_path(root, run, SHARED_TARGETS);
    let Ok(entries) = std::fs::read_dir(&targets) else {
        return removed;
    };
    for dir in entries.flatten().map(|e| e.path()) {
        if is_trash(&dir) {
            if let Err(err) = std::fs::remove_dir_all(&dir) {
                tracing::warn!(dir = %dir.display(), error = %err, "cannot finish removing a build directory; retrying at the next startup");
            }
            continue;
        }
        if !dir.is_dir() {
            continue;
        }
        let Some(tree) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        if keep.contains(&tree) {
            continue;
        }
        let size = dir_size(&dir);
        if let Some(gone) = remove_build_dir(&dir, size) {
            removed.push(gone);
        }
    }
    let _ = std::fs::remove_dir(&targets);
    removed
}

/// Removes `<root>/<run>/target` (the old fallback, where a worktree whose repository could not
/// be told used to build) once the run has no checkout left to build anything, and the run's
/// directory when it is empty. Blocking: sizes what it removes.
pub fn prune_legacy_run_targets(root: &Path) -> Vec<(PathBuf, u64)> {
    let mut removed = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return removed;
    };
    for run in entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n != SHARED_TARGETS))
    {
        let target = run.join("target");
        let busy = std::fs::read_dir(&run)
            .map(|rd| {
                rd.flatten().map(|e| e.path()).any(|p| {
                    p.is_dir()
                        && p.file_name()
                            .is_some_and(|n| n != "target" && n != SHARED_TARGETS && !is_trash(&p))
                })
            })
            .unwrap_or(true);
        if target.is_dir() && !busy {
            let size = dir_size(&target);
            if let Some(gone) = remove_build_dir(&target, size) {
                removed.push(gone);
            }
        }
        let _ = std::fs::remove_dir(&run);
    }
    removed
}

/// Bytes of build output under `root`: each run's `<run>/targets/*`, the old `<run>/target`,
/// and the old shared `<root>/targets/*`. Blocking: walks them.
pub fn build_cache_size(root: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(root) else {
        return total;
    };
    for run in entries.flatten().map(|e| e.path()) {
        if !run.is_dir() {
            continue;
        }
        if run.file_name().is_some_and(|n| n == SHARED_TARGETS) {
            total += std::fs::read_dir(&run)
                .map(|rd| rd.flatten().map(|e| dir_size(&e.path())).sum::<u64>())
                .unwrap_or(0);
            continue;
        }
        total += std::fs::read_dir(&run)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.is_dir()
                            && p.file_name()
                                .is_some_and(|n| n == "target" || n == SHARED_TARGETS)
                    })
                    .map(|p| dir_size(&p))
                    .sum::<u64>()
            })
            .unwrap_or(0);
    }
    total
}

/// The old shared build directories some worktree under `root` (`<root>/<run>/<tree>`) maps to:
/// nothing builds there any more, but a run that is still working may be about to.
fn keys_in_use(root: &Path) -> std::collections::HashSet<String> {
    let dirs = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default()
    };
    dirs(root)
        .iter()
        .filter(|run| run.file_name().is_some_and(|n| n != SHARED_TARGETS))
        .flat_map(|run| dirs(run))
        // `<run>/targets` and `<run>/target` are build directories, not checkouts.
        .filter(|tree| {
            tree.file_name()
                .is_some_and(|n| n != SHARED_TARGETS && n != "target")
        })
        .filter_map(|tree| repo_key(&tree))
        .collect()
}

/// Bytes of the files under `dir`, not following links.
fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    total
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
    async fn creates_round_branch_in_existing_checkout_without_overwriting() {
        let repo = repo().await;
        let ws = GitWorkspaces;
        ws.create_branch(repo.path(), "round", "main").await.unwrap();
        assert_eq!(ws.current_branch(repo.path()).await.unwrap(), "round");
        assert!(ws.create_branch(repo.path(), "round", "main").await.is_err());
        assert_eq!(ws.current_branch(repo.path()).await.unwrap(), "round");
    }

    #[tokio::test]
    async fn each_worktree_builds_in_its_own_dir() {
        let (repo, other) = (repo().await, repo().await);
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("worktrees");
        let ws = GitWorkspaces;
        let integration = worktree_path(&root, "ab12cd34", "integration");
        let task = worktree_path(&root, "ab12cd34", "auth");
        let sibling = worktree_path(&root, "ab12cd34", "api");
        let other_run = worktree_path(&root, "ff00ee11", "integration");
        let other_repo = worktree_path(&root, "99887766", "integration");
        ws.create(repo.path(), "ca/one", "main", &integration)
            .await
            .unwrap();
        ws.create(repo.path(), "ca/one-a", "ca/one", &task)
            .await
            .unwrap();
        ws.create(repo.path(), "ca/one-b", "ca/one", &sibling)
            .await
            .unwrap();
        ws.create(repo.path(), "ca/two", "main", &other_run)
            .await
            .unwrap();
        ws.create(other.path(), "ca/three", "main", &other_repo)
            .await
            .unwrap();

        let target = |cwd: &Path| worktree_dirs(&root, cwd).unwrap().1;
        let canon = dunce::canonicalize(&root).unwrap();
        let want = |run: &str, tree: &str| canon.join(run).join(SHARED_TARGETS).join(tree);
        // One per worktree: two tasks of one run, its checkout, and other runs never share.
        assert_eq!(target(&integration), want("ab12cd34", "integration"));
        assert_eq!(target(&task), want("ab12cd34", "auth"));
        assert_eq!(target(&sibling), want("ab12cd34", "api"));
        assert_eq!(target(&other_run), want("ff00ee11", "integration"));
        assert_eq!(target(&other_repo), want("99887766", "integration"));
        // The worker and the hub's checks in one checkout build together; a subdirectory too.
        std::fs::create_dir_all(task.join("src")).unwrap();
        assert_eq!(target(&task.join("src")), target(&task));
        // Build directories, the run's own directory and the old shared ones are no checkouts.
        for cwd in [
            target(&task),
            root.join("ab12cd34"),
            root.join("ab12cd34").join("target"),
            root.join(SHARED_TARGETS).join("repo-0000aaaa"),
        ] {
            assert_eq!(worktree_dirs(&root, &cwd), None, "{}", cwd.display());
        }
    }

    /// Fills a build directory with `bytes` of "artefacts".
    fn built(dir: &Path, bytes: usize) {
        std::fs::create_dir_all(dir.join("debug/deps")).unwrap();
        std::fs::write(dir.join("debug/deps/lib.rlib"), vec![0u8; bytes]).unwrap();
    }

    #[test]
    fn a_build_directory_goes_when_its_checkout_does() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("worktrees");
        let run = "ab12cd34";
        let (integration, task, gone) = (
            target_dir(&root, run, "integration"),
            target_dir(&root, run, "auth"),
            target_dir(&root, run, "gone"),
        );
        for dir in [&integration, &task, &gone] {
            built(dir, 10);
        }
        std::fs::create_dir_all(worktree_path(&root, run, "auth")).unwrap();
        std::fs::create_dir_all(worktree_path(&root, run, "integration")).unwrap();

        // `gone`'s checkout is not there any more; the others are.
        assert_eq!(
            prune_run_targets(&root, run, &std::collections::HashSet::new()),
            [(gone.clone(), 10)]
        );
        assert!(integration.exists() && task.exists());

        // Removing the run's checkout takes its own build directory and nothing else.
        std::fs::remove_dir_all(worktree_path(&root, run, "integration")).unwrap();
        assert_eq!(
            prune_run_targets(&root, run, &std::collections::HashSet::new()),
            [(integration.clone(), 10)]
        );
        assert!(task.exists(), "a task still builds there");
        assert!(integration.parent().unwrap().exists());
    }

    #[test]
    fn a_finished_run_loses_every_build_directory_but_the_verifying_ones() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("worktrees");
        let run = "ab12cd34";
        let (integration, task, verifying) = (
            target_dir(&root, run, "integration"),
            target_dir(&root, run, "auth"),
            target_dir(&root, run, "api"),
        );
        for dir in [&integration, &task, &verifying] {
            built(dir, 10);
        }
        // The checkouts are still there: a finished run's build dirs go regardless.
        std::fs::create_dir_all(worktree_path(&root, run, "integration")).unwrap();
        std::fs::create_dir_all(worktree_path(&root, run, "auth")).unwrap();

        let keep = std::collections::HashSet::from(["api".to_owned()]);
        let mut gone = remove_run_targets_except(&root, run, &keep);
        gone.sort();
        let mut want = vec![(task.clone(), 10), (integration.clone(), 10)];
        want.sort();
        assert_eq!(gone, want);
        assert!(verifying.exists(), "a tree being verified is kept");
        assert!(worktree_path(&root, run, "integration").exists());

        // Its checks done, the last one goes, and `targets` with it.
        assert_eq!(
            remove_run_targets_except(&root, run, &std::collections::HashSet::new()),
            [(verifying.clone(), 10)]
        );
        assert!(!verifying.parent().unwrap().exists());
        assert!(worktree_path(&root, run, "auth").exists());
    }

    #[test]
    fn what_is_being_verified_is_kept_and_leftovers_go() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("worktrees");
        let run = "ab12cd34";
        let verifying = target_dir(&root, run, "auth");
        let trash = target_dir(&root, run, ".trash-api-1");
        built(&verifying, 10);
        built(&trash, 10);
        // Its checkout is gone (a run cancelled mid-verification), the task still verifies.
        let keep = std::collections::HashMap::from([(
            run.to_owned(),
            std::collections::HashSet::from(["auth".to_owned()]),
        )]);
        assert!(prune_all_targets(&root, &keep).is_empty());
        assert!(verifying.exists(), "a check may still be writing there");
        assert!(!trash.exists(), "a leftover of an interrupted delete goes");
    }

    #[tokio::test]
    async fn old_shared_directories_go_once_no_run_maps_to_them() {
        let repo = repo().await;
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("worktrees");
        let open = worktree_path(&root, "ab12cd34", "integration");
        GitWorkspaces
            .create(repo.path(), "ca/open", "main", &open)
            .await
            .unwrap();
        let shared = root.join(SHARED_TARGETS);
        let busy = shared.join(repo_key(&open).unwrap());
        let (idle, leftover) = (shared.join("gone-0000aaaa"), shared.join(".trash-old-1"));
        for dir in [&busy, &idle, &leftover] {
            built(dir, 100);
        }

        // Every one of them is past the limit; only the one a live worktree maps to stays.
        assert_eq!(prune_shared_targets(&root, 0), [(idle.clone(), 100)]);
        assert!(!idle.exists() && !leftover.exists());
        assert!(busy.exists());
        // The old fallback `<run>/target` goes once nothing else is in the run's directory.
        let legacy = root.join("ab12cd34").join("target");
        built(&legacy, 100);
        std::fs::remove_dir_all(&open).unwrap();
        assert_eq!(prune_legacy_run_targets(&root), [(legacy.clone(), 100)]);
        assert!(!legacy.exists());
        // What is left is counted, and the old shared directory goes with it.
        assert!(build_cache_size(&root) >= 100);
        assert_eq!(prune_shared_targets(&root, 0), [(busy.clone(), 100)]);
        assert_eq!(build_cache_size(&home.path().join("nowhere")), 0);
        assert!(
            prune_run_targets(&home.path().join("nowhere"), "x", &Default::default()).is_empty()
        );
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
        // Committed and new files alike, each once.
        let changed = ws.changed_files(&task, "ca/run").await.unwrap();
        assert_eq!(changed, ["a.txt", "b.txt"]);
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
    async fn diff_stat_includes_uncommitted_and_untracked_changes() {
        let repo = repo().await;
        std::fs::write(repo.path().join("README"), "changed\n").unwrap();
        std::fs::write(repo.path().join("new.txt"), "new\n").unwrap();
        let stat = GitWorkspaces.diff_stat(repo.path(), "HEAD").await.unwrap();
        assert!(stat.contains("README"), "{stat}");
        assert!(stat.contains("new.txt"), "{stat}");
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
        std::fs::write(run.join("README"), "from the run\n").unwrap();
        ws.commit_all(&run, "work").await.unwrap();

        // An edit to a file the run changes too stops the land.
        std::fs::write(repo.path().join("README"), "edited\n").unwrap();
        let err = ws
            .land(repo.path(), "main", "ca/run", LandMode::Merge, "land")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("also changes") && err.to_string().contains("README"),
            "{err}"
        );
        git(repo.path(), &["checkout", "--", "README"])
            .await
            .unwrap();

        // So does an untracked file the run adds.
        std::fs::write(repo.path().join("new.txt"), "mine\n").unwrap();
        let err = ws
            .land(repo.path(), "main", "ca/run", LandMode::Merge, "land")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("new.txt"), "{err}");
        std::fs::remove_file(repo.path().join("new.txt")).unwrap();

        // Anything staged would go into the land commit.
        std::fs::write(repo.path().join("notes.md"), "draft\n").unwrap();
        git(repo.path(), &["add", "notes.md"]).await.unwrap();
        let err = ws
            .land(repo.path(), "main", "ca/run", LandMode::Squash, "land")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("staged"), "{err}");
        git(repo.path(), &["reset", "-q", "notes.md"])
            .await
            .unwrap();

        // Work elsewhere is no reason to wait, and it stays as it was, uncommitted.
        std::fs::write(repo.path().join("other.txt"), "base\n").unwrap();
        git(repo.path(), &["add", "other.txt"]).await.unwrap();
        let mut commit_other = AUTHOR.to_vec();
        commit_other.extend(["commit", "-q", "-m", "other"]);
        git(repo.path(), &commit_other).await.unwrap();
        std::fs::write(repo.path().join("other.txt"), "mine, not committed\n").unwrap();
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
        assert_eq!(
            std::fs::read_to_string(repo.path().join("other.txt")).unwrap(),
            "mine, not committed\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("notes.md")).unwrap(),
            "draft\n"
        );
        let landed = git(repo.path(), &["show", "--name-only", "--format=", "HEAD"])
            .await
            .unwrap();
        assert!(
            !landed.contains("other.txt") && !landed.contains("notes.md"),
            "{landed}"
        );
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
