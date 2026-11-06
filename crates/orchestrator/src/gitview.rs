//! The read-only Git view of a project (docs/TODO.md §14): its local and remote branches,
//! the working copy's status, one commit's details, and the repository's whole graph.
//!
//! Every command only reads (`for-each-ref`, `rev-parse`, `status`, `log`, `show`); nothing
//! fetches, switches a branch or moves HEAD. Only directories the hub knows — where an agent
//! works, and the repository of a run — are read: a path that leads anywhere else, through
//! `..`, a symlink or an absolute path, is refused before git sees it.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use crossagents_core::CoreError;
use crossagents_core::process::NoWindow;
use tokio::process::Command;

use crate::engine::Orchestrator;
use crate::files::rel_of;
use crate::graph::{CommitNode, GRAPH_LIMIT, GRAPH_MAX, valid_sha};

/// Why a Git view request was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitError {
    /// The path does not name a project the hub knows.
    Outside(String),
    /// Nothing of that name is there: a missing directory, or a commit no ref reaches.
    NotFound(String),
    /// A known project directory is not in a git repository.
    Conflict(String),
    /// An invalid request.
    BadRequest(String),
    /// The file system or git failed.
    Failed(String),
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Outside(what) => write!(f, "not a project the hub knows: {what}"),
            Self::NotFound(what) => write!(f, "not found: {what}"),
            Self::Conflict(what) => write!(f, "{what}"),
            Self::BadRequest(what) => write!(f, "{what}"),
            Self::Failed(what) => write!(f, "{what}"),
        }
    }
}

impl std::error::Error for GitError {}

impl From<CoreError> for GitError {
    fn from(err: CoreError) -> Self {
        match err {
            err @ CoreError::NotFound { .. } => Self::NotFound(err.to_string()),
            err => Self::Failed(err.to_string()),
        }
    }
}

/// One branch of a project: a local one (`refs/heads/…`) or a remote-tracking one
/// (`refs/remotes/…`), with what it tracks and how far it has drifted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    /// The short name: `main`, `origin/main`.
    pub name: String,
    /// A remote-tracking branch rather than a local one.
    pub remote: bool,
    pub sha: String,
    /// The branch it tracks, by short name (`origin/main`).
    pub upstream: Option<String>,
    /// Commits the branch has that its upstream does not.
    pub ahead: u32,
    /// Commits the upstream has that the branch does not.
    pub behind: u32,
    /// HEAD points at this branch.
    pub current: bool,
}

/// A project's branches, local and remote, and where HEAD is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Branches {
    /// The project directory, canonical.
    pub root: PathBuf,
    pub entries: Vec<Branch>,
    /// The branch HEAD is on; `None` when HEAD is detached.
    pub head: Option<String>,
    /// The commit HEAD names; `None` in a repository without commits.
    pub head_sha: Option<String>,
}

/// One side of a file's state, as `git status --porcelain=v2` writes it: the index (`X`)
/// or the working tree (`Y`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    Unchanged,
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unmerged,
    Unknown,
}

/// One changed path of the working copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    /// Relative to the project root, with `/`.
    pub path: String,
    /// The path before a rename or copy.
    pub orig_path: Option<String>,
    pub index: FileState,
    pub worktree: FileState,
    /// Git does not track the file (a `?` record).
    pub untracked: bool,
    /// An unresolved merge (a `u` record).
    pub conflicted: bool,
}

/// A project's working copy status, including what its branch tracks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// The project directory, canonical.
    pub root: PathBuf,
    /// The commit HEAD is at; `None` in a repository without commits.
    pub head: Option<String>,
    /// The branch HEAD is on; `None` when HEAD is detached.
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub entries: Vec<StatusEntry>,
}

/// One commit, with the whole message and its parents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDetail {
    /// The project directory, canonical.
    pub root: PathBuf,
    pub sha: String,
    pub parents: Vec<String>,
    pub author: String,
    pub author_email: String,
    pub time: DateTime<Utc>,
    pub subject: String,
    /// The whole message, subject line and body.
    pub message: String,
}

/// What a ref of the whole-repository graph is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoRefKind {
    Head,
    Local,
    Remote,
    Tag,
}

/// A ref shown in the whole-repository graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef {
    pub name: String,
    pub kind: RepoRefKind,
    pub sha: String,
}

/// A repository's commits (`git log --all`), newest first, and the refs they belong to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoGraph {
    /// The project directory, canonical.
    pub root: PathBuf,
    pub refs: Vec<RepoRef>,
    pub commits: Vec<CommitNode>,
    /// There were more commits than the limit and they were cut.
    pub truncated: bool,
}

struct Outcome {
    ok: bool,
    stdout: String,
    stderr: String,
}

/// Runs git in `dir`; a git that says no is an outcome, not an error. Optional locks are
/// off, so even `status` writes nothing into the repository.
async fn run_git(dir: &Path, args: &[&str]) -> Result<Outcome, GitError> {
    let output = Command::new("git")
        .no_window()
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .map_err(|e| GitError::Failed(format!("cannot run git: {e}")))?;
    Ok(Outcome {
        ok: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

/// Whether `dir` is inside a git repository.
async fn is_repo(dir: &Path) -> Result<bool, GitError> {
    Ok(run_git(dir, &["rev-parse", "--git-dir"]).await?.ok)
}

/// Whether `input` names an absolute path: a root, or a drive letter on Windows
/// (`C:/x`), which is what a client pasting a Windows path sends.
fn is_absolute(input: &str) -> bool {
    if input.starts_with('/') {
        return true;
    }
    if cfg!(windows) {
        let bytes = input.as_bytes();
        return bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic();
    }
    false
}

/// A field of a `-z` record, without the newline a non-`-z` git would have written.
fn field(text: &str) -> &str {
    text.trim_end_matches(['\n', '\r'])
}

/// A non-empty field, as an `Option`.
fn non_empty(text: &str) -> Option<String> {
    let text = field(text).trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// The commit HEAD names; `None` in a repository without commits (or a detached HEAD).
async fn head_commit(dir: &Path) -> Result<Option<String>, GitError> {
    let out = run_git(dir, &["rev-parse", "--verify", "-q", "HEAD^{commit}"]).await?;
    Ok(non_empty(&out.stdout).filter(|_| out.ok))
}

/// The branch HEAD is on; `None` when HEAD is detached (or unborn).
async fn head_branch(dir: &Path) -> Result<Option<String>, GitError> {
    let out = run_git(dir, &["symbolic-ref", "--short", "-q", "HEAD"]).await?;
    Ok(non_empty(&out.stdout).filter(|_| out.ok))
}

/// `ahead N`, `behind N` (or both) from `%(upstream:track)`, which is empty when the
/// branch and its upstream are even.
fn parse_track(text: &str) -> (u32, u32) {
    let mut ahead = 0;
    let mut behind = 0;
    let inner = field(text)
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']');
    for part in inner.split(',') {
        let part = part.trim();
        if let Some(n) = part.strip_prefix("ahead ") {
            ahead = n.trim().parse().unwrap_or(0);
        } else if let Some(n) = part.strip_prefix("behind ") {
            behind = n.trim().parse().unwrap_or(0);
        }
    }
    (ahead, behind)
}

/// The local and remote-tracking branches of `dir`.
async fn read_branches(dir: &Path) -> Result<Branches, GitError> {
    // A symbolic ref (`origin/HEAD`) is not a branch: it points at one.
    let out = run_git(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)%00%(upstream:short)%00%(upstream:track)%00%(HEAD)%00%(symref)",
            "refs/heads",
            "refs/remotes",
        ],
    )
    .await?;
    if !out.ok {
        return Err(GitError::Failed(format!(
            "git for-each-ref failed: {}",
            out.stderr
        )));
    }
    let mut entries = Vec::new();
    for line in out.stdout.lines() {
        let fields: Vec<&str> = line.split('\0').collect();
        if fields.len() < 6 || !field(fields[5]).trim().is_empty() {
            continue;
        }
        let full = field(fields[0]);
        let (name, remote) = if let Some(name) = full.strip_prefix("refs/heads/") {
            (name, false)
        } else if let Some(name) = full.strip_prefix("refs/remotes/") {
            (name, true)
        } else {
            continue;
        };
        let Some(sha) = non_empty(fields[1]) else {
            continue;
        };
        let (ahead, behind) = parse_track(fields[3]);
        entries.push(Branch {
            name: name.to_owned(),
            remote,
            sha,
            upstream: non_empty(fields[2]),
            ahead,
            behind,
            current: field(fields[4]).trim() == "*",
        });
    }
    Ok(Branches {
        root: dir.to_path_buf(),
        entries,
        head: head_branch(dir).await?,
        head_sha: head_commit(dir).await?,
    })
}

/// The state one porcelain-v2 column means.
fn state_of(byte: u8) -> FileState {
    match byte {
        b'.' => FileState::Unchanged,
        b'M' => FileState::Modified,
        b'A' => FileState::Added,
        b'D' => FileState::Deleted,
        b'R' => FileState::Renamed,
        b'C' => FileState::Copied,
        b'T' => FileState::TypeChanged,
        b'U' => FileState::Unmerged,
        _ => FileState::Unknown,
    }
}

/// The two states of a record's `XY` field.
fn parse_xy(xy: &str) -> (FileState, FileState) {
    let bytes = xy.as_bytes();
    (
        state_of(bytes.first().copied().unwrap_or(b'.')),
        state_of(bytes.get(1).copied().unwrap_or(b'.')),
    )
}

/// `# branch.*` headers of a porcelain-v2 status.
fn parse_header(header: &str, status: &mut Status) {
    let Some((name, value)) = header.split_once(' ') else {
        return;
    };
    let value = value.trim();
    match name {
        "branch.oid" if value != "(initial)" => status.head = Some(value.to_owned()),
        "branch.head" if value != "(detached)" => status.branch = Some(value.to_owned()),
        "branch.upstream" => status.upstream = Some(value.to_owned()),
        "branch.ab" => {
            for part in value.split_whitespace() {
                if let Some(n) = part.strip_prefix('+') {
                    status.ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix('-') {
                    status.behind = n.parse().unwrap_or(0);
                }
            }
        }
        _ => {}
    }
}

/// Parses `git status --porcelain=v2 -z --branch --untracked-files=all`.
fn parse_status(output: &str) -> Status {
    let mut status = Status::default();
    let records: Vec<&str> = output.split('\0').collect();
    let mut i = 0;
    while i < records.len() {
        let record = field(records[i]);
        i += 1;
        let Some(kind) = record.as_bytes().first().copied() else {
            continue;
        };
        match kind {
            b'#' => parse_header(record.trim_start_matches("# "), &mut status),
            // `1 XY sub mH mI mW hH hI path`
            b'1' => {
                let fields: Vec<&str> = record.splitn(9, ' ').collect();
                if fields.len() == 9 {
                    let (index, worktree) = parse_xy(fields[1]);
                    status.entries.push(StatusEntry {
                        path: fields[8].to_owned(),
                        orig_path: None,
                        index,
                        worktree,
                        untracked: false,
                        conflicted: false,
                    });
                }
            }
            // `2 XY sub mH mI mW hH hI Xscore path`, then the old path as its own record.
            b'2' => {
                let fields: Vec<&str> = record.splitn(10, ' ').collect();
                let orig_path = records.get(i).and_then(|old| non_empty(old));
                i += 1;
                if fields.len() == 10 {
                    let (index, worktree) = parse_xy(fields[1]);
                    status.entries.push(StatusEntry {
                        path: fields[9].to_owned(),
                        orig_path,
                        index,
                        worktree,
                        untracked: false,
                        conflicted: false,
                    });
                }
            }
            // `u XY sub m1 m2 m3 mW h1 h2 h3 path`
            b'u' => {
                let fields: Vec<&str> = record.splitn(11, ' ').collect();
                if fields.len() == 11 {
                    let (index, worktree) = parse_xy(fields[1]);
                    status.entries.push(StatusEntry {
                        path: fields[10].to_owned(),
                        orig_path: None,
                        index,
                        worktree,
                        untracked: false,
                        conflicted: true,
                    });
                }
            }
            // `? path`: untracked. An ignored `!` record is not a change: it is skipped.
            b'?' => {
                let Some(path) = record.get(2..).filter(|path| !path.is_empty()) else {
                    continue;
                };
                status.entries.push(StatusEntry {
                    path: path.to_owned(),
                    orig_path: None,
                    index: FileState::Unchanged,
                    worktree: FileState::Unchanged,
                    untracked: true,
                    conflicted: false,
                });
            }
            _ => {}
        }
    }
    status
}

/// The working copy status of `dir`.
async fn read_status(dir: &Path) -> Result<Status, GitError> {
    let out = run_git(
        dir,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=all",
        ],
    )
    .await?;
    if !out.ok {
        return Err(GitError::Failed(format!(
            "git status failed: {}",
            out.stderr
        )));
    }
    Ok(Status {
        root: dir.to_path_buf(),
        ..parse_status(&out.stdout)
    })
}

/// What one commit says about itself: the NUL-separated fields, then the whole message.
const COMMIT_FORMAT: &str = "--format=%H%x00%P%x00%an%x00%ae%x00%aI%x00%s%x00%B";

/// One commit's details.
async fn read_commit(dir: &Path, sha: &str) -> Result<CommitDetail, GitError> {
    if !valid_sha(sha) {
        return Err(GitError::BadRequest(format!(
            "{sha:?} is not a commit sha (4 to 40 lowercase hex digits)"
        )));
    }
    let spec = format!("{sha}^{{commit}}");
    let found = run_git(dir, &["rev-parse", "--verify", "-q", &spec]).await?;
    let Some(full) = non_empty(&found.stdout).filter(|_| found.ok) else {
        return Err(GitError::NotFound(format!("no commit {sha}")));
    };
    let out = run_git(dir, &["show", "-s", COMMIT_FORMAT, &full]).await?;
    if !out.ok {
        return Err(GitError::NotFound(format!("no commit {sha}")));
    }
    let fields: Vec<&str> = out.stdout.split('\0').collect();
    if fields.len() < 7 {
        return Err(GitError::Failed(format!("git show {full} printed nothing")));
    }
    Ok(CommitDetail {
        root: dir.to_path_buf(),
        sha: field(fields[0]).to_owned(),
        parents: field(fields[1])
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
        author: field(fields[2]).to_owned(),
        author_email: field(fields[3]).to_owned(),
        time: DateTime::parse_from_rfc3339(field(fields[4]))
            .map(|time| time.with_timezone(&Utc))
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH),
        subject: field(fields[5]).to_owned(),
        message: field(fields[6].trim_end_matches(['\n', '\r'])).to_owned(),
    })
}

/// Every ref of `dir`: HEAD, local and remote-tracking branches, and tags. An annotated
/// tag names the commit it is on, not its own tag object.
async fn repo_refs(dir: &Path) -> Result<Vec<RepoRef>, GitError> {
    let out = run_git(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)%00%(*objectname)%00%(symref)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ],
    )
    .await?;
    if !out.ok {
        return Err(GitError::Failed(format!(
            "git for-each-ref failed: {}",
            out.stderr
        )));
    }
    let mut refs = Vec::new();
    if let Some(sha) = head_commit(dir).await? {
        refs.push(RepoRef {
            name: "HEAD".to_owned(),
            kind: RepoRefKind::Head,
            sha,
        });
    }
    for line in out.stdout.lines() {
        let fields: Vec<&str> = line.split('\0').collect();
        if fields.len() < 4 || !field(fields[3]).trim().is_empty() {
            continue;
        }
        let full = field(fields[0]);
        let (name, kind) = if let Some(name) = full.strip_prefix("refs/heads/") {
            (name, RepoRefKind::Local)
        } else if let Some(name) = full.strip_prefix("refs/remotes/") {
            (name, RepoRefKind::Remote)
        } else if let Some(name) = full.strip_prefix("refs/tags/") {
            (name, RepoRefKind::Tag)
        } else {
            continue;
        };
        // An annotated tag's own object is not a commit; its peeled target is.
        let sha = non_empty(fields[2]).or_else(|| non_empty(fields[1]));
        let Some(sha) = sha else {
            continue;
        };
        refs.push(RepoRef {
            name: name.to_owned(),
            kind,
            sha,
        });
    }
    Ok(refs)
}

/// The whole repository's graph: `git log --all`, cut to `limit` (200 when none was asked,
/// at most 500), each commit naming the refs that point at it.
async fn read_graph(dir: &Path, limit: usize) -> Result<RepoGraph, GitError> {
    let limit = if limit == 0 {
        GRAPH_LIMIT
    } else {
        limit.min(GRAPH_MAX)
    };
    let refs = repo_refs(dir).await?;
    let (mut commits, truncated) = crate::graph::log(dir, &["--all".to_owned()], limit).await?;
    for commit in &mut commits {
        commit.refs = refs
            .iter()
            .filter(|entry| entry.sha == commit.sha)
            .map(|entry| entry.name.clone())
            .collect();
    }
    Ok(RepoGraph {
        root: dir.to_path_buf(),
        refs,
        commits,
        truncated,
    })
}

impl Orchestrator {
    /// The project directories the Git view may read: where every agent works, and every
    /// run's repository, as the file system names them. A directory that is gone is skipped.
    pub async fn git_roots(&self) -> Result<Vec<PathBuf>, CoreError> {
        let mut roots: Vec<PathBuf> = Vec::new();
        for agent in self.store.list_agents().await? {
            push_dir(&mut roots, &agent.cwd);
        }
        for run in self.store.list_runs().await? {
            push_dir(&mut roots, &run.repo);
        }
        Ok(roots)
    }

    /// The project directory `input` names, when the hub knows it.
    ///
    /// `input` must be an absolute path naming where an agent works or a run's repository.
    /// A path with a `..` segment, a relative path, a symlink that leaves the project, or
    /// any absolute path outside those directories is [`GitError::Outside`]; one that is
    /// not there is [`GitError::NotFound`].
    pub async fn git_root(&self, input: &str) -> Result<PathBuf, GitError> {
        let path = input.replace('\\', "/");
        if path.trim().is_empty() {
            return Err(GitError::BadRequest("give the path of a project".into()));
        }
        if path.contains('\0') {
            return Err(GitError::BadRequest("the path contains a NUL byte".into()));
        }
        if path.split('/').any(|part| part == "..") {
            return Err(GitError::Outside(format!("{input:?} has a `..` segment")));
        }
        if !is_absolute(&path) {
            return Err(GitError::Outside(format!(
                "{input:?} is not an absolute path"
            )));
        }
        let abs = dunce::canonicalize(&path)
            .map_err(|_| GitError::NotFound(format!("no project at {input}")))?;
        if !abs.is_dir() {
            return Err(GitError::NotFound(format!("no project at {input}")));
        }
        let roots = self.git_roots().await?;
        if roots
            .iter()
            .any(|root| rel_of(root, &abs).as_deref() == Some(""))
        {
            return Ok(abs);
        }
        Err(GitError::Outside(format!(
            "{input:?} is not a project the hub knows"
        )))
    }

    /// [`Self::git_root`], and then a check that git can read it.
    pub async fn git_repo(&self, input: &str) -> Result<PathBuf, GitError> {
        let root = self.git_root(input).await?;
        if !is_repo(&root).await? {
            return Err(GitError::Conflict(format!(
                "{} is not in a git repository",
                root.display()
            )));
        }
        Ok(root)
    }

    /// The local and remote-tracking branches of a project, with their upstreams.
    pub async fn git_branches(&self, path: &str) -> Result<Branches, GitError> {
        read_branches(&self.git_repo(path).await?).await
    }

    /// The working copy status of a project, `git status --porcelain=v2`.
    pub async fn git_status(&self, path: &str) -> Result<Status, GitError> {
        read_status(&self.git_repo(path).await?).await
    }

    /// One commit of a project: its author, time, whole message and parents.
    pub async fn git_commit(&self, path: &str, sha: &str) -> Result<CommitDetail, GitError> {
        read_commit(&self.git_repo(path).await?, sha).await
    }

    /// The whole repository's graph (`--all`), at most `limit` commits.
    pub async fn git_graph(&self, path: &str, limit: usize) -> Result<RepoGraph, GitError> {
        read_graph(&self.git_repo(path).await?, limit).await
    }
}

/// Adds `path` to the known project directories, canonicalized; a path that is gone, is not
/// a directory, or is already there is skipped.
fn push_dir(roots: &mut Vec<PathBuf>, path: &Path) {
    let Ok(abs) = dunce::canonicalize(path) else {
        return;
    };
    if !abs.is_dir() {
        return;
    }
    if roots
        .iter()
        .any(|root| rel_of(root, &abs).as_deref() == Some(""))
    {
        return;
    }
    roots.push(abs);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_track_field_reads_ahead_and_behind() {
        assert_eq!(parse_track(""), (0, 0));
        assert_eq!(parse_track(" "), (0, 0));
        assert_eq!(parse_track("[ahead 1]"), (1, 0));
        assert_eq!(parse_track("[behind 2]"), (0, 2));
        assert_eq!(parse_track("[ahead 3, behind 4]"), (3, 4));
        assert_eq!(parse_track("[gone]"), (0, 0));
    }

    #[test]
    fn porcelain_v2_records_are_parsed() {
        let output = concat!(
            "# branch.oid 1111111111111111111111111111111111111111\0",
            "# branch.head main\0",
            "# branch.upstream origin/main\0",
            "# branch.ab +2 -3\0",
            "1 .M N... 100644 100644 100644 aaa bbb src/a.txt\0",
            "2 R. N... 100644 100644 100644 aaa bbb R100 src/b.txt\0src/old.txt\0",
            "u UU N... 100644 100644 100644 100644 aaa bbb ccc src/c.txt\0",
            "? new file.txt\0",
            "! ignored.txt\0",
        );
        let status = parse_status(output);
        assert_eq!(
            status.head.as_deref(),
            Some("1111111111111111111111111111111111111111")
        );
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!((status.ahead, status.behind), (2, 3));
        assert_eq!(status.entries.len(), 4);

        let modified = &status.entries[0];
        assert_eq!(modified.path, "src/a.txt");
        assert_eq!(modified.index, FileState::Unchanged);
        assert_eq!(modified.worktree, FileState::Modified);

        let renamed = &status.entries[1];
        assert_eq!(renamed.path, "src/b.txt");
        assert_eq!(renamed.orig_path.as_deref(), Some("src/old.txt"));
        assert_eq!(renamed.index, FileState::Renamed);
        assert_eq!(renamed.worktree, FileState::Unchanged);

        let conflicted = &status.entries[2];
        assert!(conflicted.conflicted);
        assert_eq!(conflicted.index, FileState::Unmerged);

        let untracked = &status.entries[3];
        assert_eq!(untracked.path, "new file.txt");
        assert!(untracked.untracked);
    }

    #[test]
    fn an_empty_or_header_only_status_is_clean() {
        let status = parse_status("");
        assert!(status.entries.is_empty() && status.head.is_none());
        let status = parse_status("# branch.oid (initial)\0# branch.head main\0");
        assert!(status.head.is_none() && status.branch.as_deref() == Some("main"));
    }

    #[test]
    fn only_absolute_paths_are_project_paths() {
        assert!(is_absolute("/tmp/repo"));
        assert!(!is_absolute("repo"));
        assert!(!is_absolute("./repo"));
        if cfg!(windows) {
            assert!(is_absolute("C:/repo"));
            assert!(!is_absolute("1:/repo"));
        }
    }
}
