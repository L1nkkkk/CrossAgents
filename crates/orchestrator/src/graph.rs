//! The commit graph (docs/DESIGN.md §11): the commits of a run's base, run and task
//! branches, the ones of an agent's repository, and what one commit changed. Read-only: it
//! runs `git log`, `rev-parse` and `diff` and changes nothing.

use std::path::Path;

use chrono::{DateTime, Utc};
use crossagents_core::process::NoWindow;
use crossagents_core::{Agent, CoreError, RunId};
use tokio::process::Command;

use crate::engine::Orchestrator;
use crate::review::Changes;

/// Commits the graph hands out when the client names no limit.
pub const GRAPH_LIMIT: usize = 200;
/// Most commits the graph hands out at once.
pub const GRAPH_MAX: usize = 500;

/// What a ref of the graph is: the run's base branch, its integration branch, a task's
/// branch, the repository's HEAD, or one of its other local branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    Base,
    Run,
    Task,
    Head,
    Other,
}

/// A branch shown in the commit graph, and the commit it points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefEntry {
    pub name: String,
    pub kind: RefKind,
    pub sha: String,
    pub task_key: Option<String>,
    pub task_title: Option<String>,
}

/// One commit of the graph; `refs` are the names in [`Graph::refs`] pointing at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitNode {
    pub sha: String,
    pub parents: Vec<String>,
    pub subject: String,
    pub author: String,
    pub time: DateTime<Utc>,
    pub refs: Vec<String>,
}

/// A repository's commits, newest first (topological), with the branches they belong to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Graph {
    pub base_branch: Option<String>,
    pub run_branch: Option<String>,
    pub refs: Vec<RefEntry>,
    pub commits: Vec<CommitNode>,
    /// There were more commits than the limit and they were cut.
    pub truncated: bool,
}

/// Whether `sha` is a commit id the API hands out: 4 to 40 lowercase hex digits. Checked
/// before git sees it, so nothing else can be passed as a revision.
pub fn valid_sha(sha: &str) -> bool {
    (4..=40).contains(&sha.len())
        && sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The log's one record per commit: sha, parents, author, author date, subject.
const LOG_FORMAT: &str = "--format=%H%x00%P%x00%an%x00%aI%x00%s%x1e";

struct Outcome {
    ok: bool,
    stdout: String,
    stderr: String,
}

/// Runs git in `dir`; a git that says no is an outcome, not an error.
async fn run_git(dir: &Path, args: &[&str]) -> Result<Outcome, CoreError> {
    let output = Command::new("git")
        .no_window()
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .map_err(|e| CoreError::Agent(format!("cannot run git: {e}")))?;
    Ok(Outcome {
        ok: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

/// Whether `dir` is inside a git repository (a missing directory is not one).
async fn is_repo(dir: &Path) -> Result<bool, CoreError> {
    // git cannot even start in a directory that is gone (os error 267 on Windows).
    if !dir.is_dir() {
        return Ok(false);
    }
    Ok(run_git(dir, &["rev-parse", "--git-dir"]).await?.ok)
}

/// Where a run's branches are read: its integration checkout, or the repository itself once
/// that checkout is gone (a run that ended without changes, or after its lead left). The
/// branches live in the repository either way.
fn run_dir(integration: std::path::PathBuf, run: &crossagents_core::Run) -> std::path::PathBuf {
    if integration.is_dir() {
        integration
    } else {
        run.repo.clone()
    }
}

/// The commit `name` is a local branch of `dir`; `None` when there is no such branch.
async fn branch_commit(dir: &Path, name: &str) -> Result<Option<String>, CoreError> {
    let spec = format!("refs/heads/{name}^{{commit}}");
    let out = run_git(dir, &["rev-parse", "--verify", "-q", &spec]).await?;
    Ok(out
        .ok
        .then(|| out.stdout.trim().to_owned())
        .filter(|sha| !sha.is_empty()))
}

/// The commit HEAD is at; `None` in a repository without commits (or a detached HEAD).
async fn head_commit(dir: &Path) -> Result<Option<String>, CoreError> {
    let out = run_git(dir, &["rev-parse", "--verify", "-q", "HEAD^{commit}"]).await?;
    Ok(out
        .ok
        .then(|| out.stdout.trim().to_owned())
        .filter(|sha| !sha.is_empty()))
}

/// The local branches of `dir`, by name, with the commits they point at.
async fn local_branches(dir: &Path) -> Result<Vec<(String, String)>, CoreError> {
    let out = run_git(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname:short)%00%(objectname)",
            "refs/heads",
        ],
    )
    .await?;
    if !out.ok {
        return Ok(Vec::new());
    }
    Ok(out
        .stdout
        .lines()
        .filter_map(|line| {
            let (name, sha) = line.split_once('\0')?;
            let (name, sha) = (name.trim(), sha.trim());
            (!name.is_empty() && !sha.is_empty()).then(|| (name.to_owned(), sha.to_owned()))
        })
        .collect())
}

/// `git log` over `revs`, at most `limit` commits: newest first (topological), and whether
/// there were more. The Git view's whole-repository graph reads `--all` through it.
pub(crate) async fn log(
    dir: &Path,
    revs: &[String],
    limit: usize,
) -> Result<(Vec<CommitNode>, bool), CoreError> {
    let revs: Vec<&str> = revs
        .iter()
        .map(String::as_str)
        .filter(|rev| !rev.is_empty())
        .collect();
    if revs.is_empty() {
        return Ok((Vec::new(), false));
    }
    let count = limit.saturating_add(1).to_string();
    let mut args: Vec<String> = vec![
        "log".into(),
        "--topo-order".into(),
        "-n".into(),
        count,
        LOG_FORMAT.into(),
    ];
    args.extend(revs.iter().map(|rev| (*rev).to_owned()));
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run_git(dir, &borrowed).await?;
    if !out.ok {
        // A repository without commits, or a revision that is not there (a branch removed
        // while we looked): no commits, rather than a failure.
        return Ok((Vec::new(), false));
    }
    let mut commits: Vec<CommitNode> = Vec::new();
    for record in out.stdout.split('\x1e') {
        let record = record.trim_start_matches(['\n', '\r']);
        if record.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = record.split('\0').collect();
        if fields.len() < 5 {
            continue;
        }
        commits.push(CommitNode {
            sha: fields[0].to_owned(),
            parents: fields[1].split_whitespace().map(str::to_owned).collect(),
            author: fields[2].to_owned(),
            time: parse_time(fields[3]),
            subject: fields[4].to_owned(),
            refs: Vec::new(),
        });
    }
    let truncated = commits.len() > limit;
    commits.truncate(limit);
    Ok((commits, truncated))
}

/// An ISO 8601 author date; a date git wrote in a way chrono cannot read counts as epoch.
fn parse_time(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text.trim())
        .map(|time| time.with_timezone(&Utc))
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

/// The commits of `refs`, cut to `limit` (200 when none was asked, at most 500), each
/// naming the refs that point at it.
async fn graph_of(
    dir: &Path,
    refs: Vec<RefEntry>,
    base_branch: Option<String>,
    run_branch: Option<String>,
    limit: usize,
) -> Result<Graph, CoreError> {
    let limit = if limit == 0 {
        GRAPH_LIMIT
    } else {
        limit.min(GRAPH_MAX)
    };
    let mut revs: Vec<String> = Vec::new();
    for entry in &refs {
        if !entry.sha.is_empty() && !revs.contains(&entry.sha) {
            revs.push(entry.sha.clone());
        }
    }
    let (mut commits, truncated) = log(dir, &revs, limit).await?;
    for commit in &mut commits {
        commit.refs = refs
            .iter()
            .filter(|entry| entry.sha == commit.sha)
            .map(|entry| entry.name.clone())
            .collect();
    }
    Ok(Graph {
        base_branch,
        run_branch,
        refs,
        commits,
        truncated,
    })
}

impl Orchestrator {
    /// The graph of `run`: its base branch, its integration branch and the branch of every
    /// task that has one, as they are in the run's integration checkout.
    pub async fn run_graph(&self, run: RunId, limit: usize) -> Result<Graph, CoreError> {
        let run = self.store.get_run(run).await?;
        let dir = run_dir(self.integration_path(&run), &run);
        let mut wanted: Vec<(String, RefKind, Option<String>, Option<String>)> = Vec::new();
        if !run.base_branch.is_empty() {
            wanted.push((run.base_branch.clone(), RefKind::Base, None, None));
        }
        if !run.branch.is_empty() {
            wanted.push((run.branch.clone(), RefKind::Run, None, None));
        }
        for task in self.store.list_tasks(run.id).await? {
            if let Some(branch) = task.branch.clone().filter(|b| !b.is_empty()) {
                wanted.push((
                    branch,
                    RefKind::Task,
                    Some(task.key.clone()),
                    Some(task.title.clone()),
                ));
            }
        }
        let mut refs = Vec::new();
        for (name, kind, task_key, task_title) in wanted {
            // A branch that is gone (its task merged, or the run's not made yet) is skipped.
            let Some(sha) = branch_commit(&dir, &name).await? else {
                continue;
            };
            refs.push(RefEntry {
                name,
                kind,
                sha,
                task_key,
                task_title,
            });
        }
        let base = (!run.base_branch.is_empty()).then(|| run.base_branch.clone());
        let branch = (!run.branch.is_empty()).then(|| run.branch.clone());
        graph_of(&dir, refs, base, branch, limit).await
    }

    /// The graph of the repository `agent` works in: its HEAD and local branches. When its
    /// directory is gone (a run's checkout removed under an archived lead), its workspace.
    pub async fn agent_graph(&self, agent: &Agent, limit: usize) -> Result<Graph, CoreError> {
        let dir = agent_dir(agent);
        if !is_repo(&dir).await? {
            return Err(CoreError::NotFound {
                kind: "git repository",
                id: dir.display().to_string(),
            });
        }
        let mut refs = Vec::new();
        if let Some(sha) = head_commit(&dir).await? {
            refs.push(RefEntry {
                name: "HEAD".to_owned(),
                kind: RefKind::Head,
                sha,
                task_key: None,
                task_title: None,
            });
        }
        for (name, sha) in local_branches(&dir).await? {
            refs.push(RefEntry {
                name,
                kind: RefKind::Other,
                sha,
                task_key: None,
                task_title: None,
            });
        }
        graph_of(&dir, refs, None, None, limit).await
    }

    /// What one commit of `run` changed, in its integration checkout.
    pub async fn run_commit_diff(&self, run: RunId, sha: &str) -> Result<Changes, CoreError> {
        let run = self.store.get_run(run).await?;
        commit_diff(&run_dir(self.integration_path(&run), &run), sha).await
    }

    /// What one commit of the repository `agent` works in changed.
    pub async fn agent_commit_diff(&self, agent: &Agent, sha: &str) -> Result<Changes, CoreError> {
        commit_diff(&agent_dir(agent), sha).await
    }
}

/// The directory an agent's graph is read in: where it works, or its workspace once that is
/// gone.
fn agent_dir(agent: &Agent) -> std::path::PathBuf {
    if agent.cwd.is_dir() {
        agent.cwd.clone()
    } else {
        agent.home().to_path_buf()
    }
}

/// What one commit changed against its first parent (the empty tree for a root commit):
/// the parent as `base`, the commit as `head`, and the diff cut at [`crate::review::DIFF_LIMIT`].
pub async fn commit_diff(dir: &Path, sha: &str) -> Result<Changes, CoreError> {
    if !valid_sha(sha) {
        return Err(CoreError::Unsupported(format!(
            "{sha:?} is not a commit sha (4 to 40 lowercase hex digits)"
        )));
    }
    let out = run_git(dir, &["rev-list", "--parents", "-n", "1", sha]).await?;
    let line = out.stdout.trim();
    let Some(head) = line.split_whitespace().next().filter(|_| out.ok) else {
        return Err(not_found(dir, sha, &out.stderr));
    };
    let parent = line.split_whitespace().nth(1).map(str::to_owned);
    let args = match &parent {
        Some(parent) => vec!["diff", "--no-color", parent.as_str(), head],
        // A root commit: everything it added, against the empty tree.
        None => vec!["show", "--format=", "--patch", "--no-color", head],
    };
    let diff = run_git(dir, &args).await?;
    if !diff.ok {
        return Err(not_found(dir, sha, &diff.stderr));
    }
    Ok(Changes::new(
        parent.unwrap_or_default(),
        head.to_owned(),
        diff.stdout,
    ))
}

/// The error of a commit that is not there to show: a missing repository, or a sha that
/// names no commit in it.
fn not_found(dir: &Path, sha: &str, stderr: &str) -> CoreError {
    if stderr.contains("not a git repository") {
        return CoreError::NotFound {
            kind: "git repository",
            id: dir.display().to_string(),
        };
    }
    CoreError::NotFound {
        kind: "commit",
        id: sha.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs git in `dir`, failing the test if it says no.
    async fn git(dir: &Path, args: &[&str]) {
        let out = run_git(dir, args).await.unwrap();
        assert!(out.ok, "git {}: {}", args.join(" "), out.stderr);
    }

    /// A repository with one commit, on `main`.
    async fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]).await;
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        commit(dir.path(), "one").await;
        dir
    }

    /// Stages everything in `dir` and commits it as `message`.
    async fn commit(dir: &Path, message: &str) {
        git(dir, &["add", "-A"]).await;
        git(
            dir,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-q",
                "-m",
                message,
            ],
        )
        .await;
    }

    async fn sha_of(dir: &Path, rev: &str) -> String {
        run_git(dir, &["rev-parse", rev])
            .await
            .unwrap()
            .stdout
            .trim()
            .to_owned()
    }

    #[tokio::test]
    async fn a_merge_commit_has_two_parents_and_refs_are_named() {
        let dir = repo().await;
        let root = sha_of(dir.path(), "HEAD").await;
        git(dir.path(), &["checkout", "-q", "-b", "topic"]).await;
        std::fs::write(dir.path().join("b.txt"), "two\n").unwrap();
        commit(dir.path(), "two").await;
        let topic = sha_of(dir.path(), "HEAD").await;
        git(dir.path(), &["checkout", "-q", "main"]).await;
        std::fs::write(dir.path().join("c.txt"), "three\n").unwrap();
        commit(dir.path(), "three").await;
        git(
            dir.path(),
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "merge",
                "--no-ff",
                "-q",
                "-m",
                "merge topic",
                "topic",
            ],
        )
        .await;
        let merge = sha_of(dir.path(), "HEAD").await;

        let refs = vec![
            RefEntry {
                name: "main".into(),
                kind: RefKind::Base,
                sha: merge.clone(),
                task_key: None,
                task_title: None,
            },
            RefEntry {
                name: "topic".into(),
                kind: RefKind::Task,
                sha: topic.clone(),
                task_key: Some("auth".into()),
                task_title: Some("Add auth".into()),
            },
        ];
        let graph = graph_of(dir.path(), refs, Some("main".into()), None, 0)
            .await
            .unwrap();
        assert_eq!(graph.refs.len(), 2);
        assert_eq!(graph.base_branch.as_deref(), Some("main"));
        assert!(!graph.truncated);
        assert_eq!(graph.commits[0].sha, merge);
        assert_eq!(graph.commits[0].parents.len(), 2, "{:?}", graph.commits[0]);
        assert_eq!(graph.commits[0].refs, ["main"]);
        assert_eq!(graph.commits[0].subject, "merge topic");
        let topic_commit = graph.commits.iter().find(|c| c.sha == topic).unwrap();
        assert_eq!(topic_commit.refs, ["topic"]);
        assert_eq!(topic_commit.parents, [root]);
        // Newest first: the merge, the commit on main, then the topic's.
        assert_eq!(graph.commits.len(), 4);
    }

    #[tokio::test]
    async fn the_limit_cuts_the_log_and_says_so() {
        let dir = repo().await;
        for n in 0..4 {
            std::fs::write(dir.path().join(format!("f{n}.txt")), "x\n").unwrap();
            commit(dir.path(), &format!("c{n}")).await;
        }
        let head = sha_of(dir.path(), "HEAD").await;
        let refs = vec![RefEntry {
            name: "HEAD".into(),
            kind: RefKind::Head,
            sha: head,
            task_key: None,
            task_title: None,
        }];
        let graph = graph_of(dir.path(), refs.clone(), None, None, 2)
            .await
            .unwrap();
        assert_eq!(graph.commits.len(), 2);
        assert!(graph.truncated);

        let whole = graph_of(dir.path(), refs.clone(), None, None, 0)
            .await
            .unwrap();
        assert_eq!(whole.commits.len(), 5);
        assert!(!whole.truncated);
        // Above the cap, the extra commit is still fetched, so truncation stays honest.
        let capped = graph_of(dir.path(), refs, None, None, GRAPH_MAX)
            .await
            .unwrap();
        assert_eq!(capped.commits.len(), 5);
        assert!(!capped.truncated);
    }

    #[tokio::test]
    async fn a_root_commit_is_shown_against_the_empty_tree() {
        let dir = repo().await;
        let root = sha_of(dir.path(), "HEAD").await;
        let changes = commit_diff(dir.path(), &root).await.unwrap();
        assert_eq!(changes.base, "");
        assert_eq!(changes.head, root);
        assert!(changes.diff.contains("a.txt"), "{}", changes.diff);
        assert!(!changes.truncated);

        std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();
        commit(dir.path(), "two").await;
        let second = sha_of(dir.path(), "HEAD").await;
        let changes = commit_diff(dir.path(), &second).await.unwrap();
        assert_eq!(changes.base, root);
        assert!(
            changes.diff.contains("-one") && changes.diff.contains("+two"),
            "{}",
            changes.diff
        );
    }

    #[tokio::test]
    async fn a_sha_is_a_sha_and_nothing_else() {
        let dir = repo().await;
        let long = "x".repeat(41);
        for bad in [
            "",
            "abc",
            "HEAD",
            "main",
            "--output=x",
            "ABCDEF",
            "abcg",
            long.as_str(),
        ] {
            let err = commit_diff(dir.path(), bad).await.unwrap_err();
            assert!(matches!(err, CoreError::Unsupported(_)), "{bad}: {err}");
        }
        assert!(valid_sha("abc1"));
        assert!(valid_sha(&sha_of(dir.path(), "HEAD").await));
        // A well-formed sha that names no commit is not found, not a failure.
        let err = commit_diff(dir.path(), "deadbeef").await.unwrap_err();
        assert!(matches!(err, CoreError::NotFound { .. }), "{err}");
    }

    #[tokio::test]
    async fn a_directory_outside_a_repository_has_no_graph() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_repo(dir.path()).await.unwrap());
        assert!(head_commit(dir.path()).await.unwrap().is_none());
        assert!(local_branches(dir.path()).await.unwrap().is_empty());
        let err = commit_diff(dir.path(), "deadbeef").await.unwrap_err();
        assert!(matches!(err, CoreError::NotFound { .. }), "{err}");

        let empty = graph_of(dir.path(), Vec::new(), None, None, 0)
            .await
            .unwrap();
        assert!(empty.commits.is_empty() && !empty.truncated);
    }

    #[tokio::test]
    async fn branches_and_head_are_read_from_the_repository() {
        let dir = repo().await;
        git(dir.path(), &["branch", "other"]).await;
        let mut refs = Vec::new();
        if let Some(sha) = head_commit(dir.path()).await.unwrap() {
            refs.push(RefEntry {
                name: "HEAD".into(),
                kind: RefKind::Head,
                sha,
                task_key: None,
                task_title: None,
            });
        }
        for (name, sha) in local_branches(dir.path()).await.unwrap() {
            refs.push(RefEntry {
                name,
                kind: RefKind::Other,
                sha,
                task_key: None,
                task_title: None,
            });
        }
        let graph = graph_of(dir.path(), refs, None, None, 0).await.unwrap();
        let names: Vec<&str> = graph.refs.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["HEAD", "main", "other"]);
        assert_eq!(graph.commits.len(), 1);
        assert_eq!(graph.commits[0].refs, ["HEAD", "main", "other"]);
    }
}
