//! Applying workers' results to the person's own directory (docs/tasks/LEAD.md §6).
//!
//! A run whose lead works in place gathers its workers on a hub-owned integration branch.
//! When its result is applied, the person's directory is one side of a three-way merge:
//!
//! * **base** — the last integration result already applied (the dispatch snapshot when
//!   nothing was), so what is written is exactly what the workers added since;
//! * **ours** — the directory's content *now*, uncommitted work and all, as a tree built in
//!   an index of its own;
//! * **theirs** — the integration branch's tree.
//!
//! `git merge-tree --write-tree` does the merge. A conflict writes **nothing**: the files and
//! what the workers did to them are returned, and the person decides. A clean merge is then
//! materialized file by file: all changed contents are written into a staging directory first
//! (`git checkout-index`, which applies `autocrlf`, `core.eol` and `.gitattributes` filters,
//! keeps binary content binary and executable bits executable), the directory is checked to
//! have stood still meanwhile, and only then are the files moved into place and deletions
//! performed. The person's index, branch, stash list and `HEAD` are never touched, and nothing
//! is committed.
//!
//! Applying is idempotent. A journal ([`State`]) records the result being applied and every
//! path's content before and after *before* the first byte is written, and the applied result
//! afterwards; an interrupted apply is resumed from it (paths already in their after state are
//! left, paths still in their before state are written, anything else is a conflict), and
//! applying the same integration result again writes nothing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use crossagents_core::CoreError;
use crossagents_core::process::NoWindow;
use serde::{Deserialize, Serialize};

use crate::workspace::NEVER_ADDED;

/// Bytes of conflict detail a refusal reports.
const CONFLICT_DETAIL: usize = 4_000;

/// What applying one integration result did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// Written: `files` were added, changed or deleted in the directory.
    Applied { result: String, files: Vec<String> },
    /// This very result was applied before; nothing was written.
    AlreadyApplied { result: String },
    /// The directory already held the result (someone applied it, or the result changes
    /// nothing); recorded as applied.
    UpToDate { result: String },
    /// Nothing was written: the person's directory changed the same files.
    Conflict { files: Vec<String>, detail: String },
}

impl Outcome {
    pub fn applied(&self) -> bool {
        matches!(self, Outcome::Applied { .. } | Outcome::UpToDate { .. })
    }
}

/// The journal: on disk next to the run's worktrees, so a hub that restarts still knows what
/// was applied and what was half-written.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied: Option<Applied>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<Pending>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    pub result: String,
    pub tree: String,
    pub at: DateTime<Utc>,
    #[serde(default)]
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub result: String,
    pub base: String,
    pub current: String,
    pub merged: String,
    pub at: DateTime<Utc>,
    pub files: Vec<String>,
}

impl State {
    pub fn read(path: &Path) -> Result<Self, CoreError> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|err| {
                CoreError::Conflict(format!(
                    "cannot read the apply record {}: {err}; nothing was written",
                    path.display()
                ))
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(CoreError::Storage(format!(
                "cannot read {}: {err}",
                path.display()
            ))),
        }
    }

    fn write(&self, path: &Path) -> Result<(), CoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| CoreError::Storage(err.to_string()))?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|err| CoreError::Storage(format!("cannot write the apply record: {err}")))?;
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, text)
            .map_err(|err| CoreError::Storage(format!("cannot write {}: {err}", temp.display())))?;
        // Rename is atomic where the target may be replaced; Windows refuses that, so an
        // existing record goes first (the window is a missing record, never a wrong one).
        if std::fs::rename(&temp, path).is_err() {
            let _ = std::fs::remove_file(path);
            std::fs::rename(&temp, path).map_err(|err| {
                CoreError::Storage(format!("cannot write {}: {err}", path.display()))
            })?;
        }
        Ok(())
    }
}

/// One changed path between two trees.
#[derive(Debug, Clone)]
struct Op {
    /// Repository-relative, as git names it.
    path: String,
    /// Content before, `None` for a new file.
    before: Option<String>,
    /// Content after, `None` for a deletion.
    after: Option<String>,
    /// The mode the file must have after (`100644`, `100755`, `120000`).
    mode: String,
    /// Whether only the mode changed (same content on both sides).
    mode_only: bool,
}

/// Applies `result` (an integration commit) to `dir`, whose changes are the ones made on top
/// of `base` (the last applied result, or the dispatch snapshot). `state_path` is the journal.
pub async fn apply(
    dir: &Path,
    base: &str,
    result: &str,
    state_path: &Path,
) -> Result<Outcome, CoreError> {
    let mut state = State::read(state_path)?;
    if let Some(applied) = &state.applied
        && applied.result == result
    {
        return Ok(Outcome::AlreadyApplied {
            result: result.to_owned(),
        });
    }
    // An earlier attempt was interrupted. Finish it first: the same result is resumed, and
    // an older one has to land before a later one can be described against it.
    if let Some(pending) = state.pending.clone() {
        let resumed = resume(dir, state_path, &mut state).await?;
        if pending.result == result {
            return Ok(resumed);
        }
        if let Outcome::Conflict { files, detail } = resumed {
            return Ok(Outcome::Conflict { files, detail });
        }
        state = State::read(state_path)?;
        if state.applied.as_ref().is_some_and(|a| a.result == result) {
            return Ok(Outcome::AlreadyApplied {
                result: result.to_owned(),
            });
        }
    }
    // What was applied last is the base of the new delta, whatever the caller read before an
    // interrupted apply was finished.
    let base = state
        .applied
        .as_ref()
        .map(|a| a.result.clone())
        .unwrap_or_else(|| base.to_owned());
    let base = base.as_str();

    let prefix = prefix_of(dir).await?;
    // What the directory held when the merge was about to be computed. Before a byte is
    // written it is read once more; if it moved, nothing is written and the paths that moved
    // are named (docs/tasks/LEAD.md §6).
    let fingerprint = crate::inplace::snapshot(dir).await?;
    let current = current_tree(dir).await?;
    let merged = match merge_trees(dir, base, &current, result).await? {
        Merged::Tree(tree) => tree,
        Merged::Conflicts(files) => {
            let detail = worker_diff(dir, base, result, &files).await;
            return Ok(Outcome::Conflict { files, detail });
        }
    };
    let ops = ops_between(dir, &prefix, &current, &merged).await?;
    if ops.is_empty() {
        record_applied(&mut state, state_path, result, &merged, Vec::new())?;
        return Ok(Outcome::UpToDate {
            result: result.to_owned(),
        });
    }
    // The directory was read a moment ago; if it moved since (an editor, a build), what was
    // merged is about other content: stop before writing anything.
    let now = crate::inplace::snapshot(dir).await?;
    if now != fingerprint {
        let latest = current_tree(dir).await?;
        let files = changed_between(dir, &current, &latest).await?;
        return Ok(Outcome::Conflict {
            detail: format!(
                "{} changed while the result was being prepared; nothing was written. Apply \
                 again when the directory stands still.",
                dir.display()
            ),
            files,
        });
    }
    let files: Vec<String> = ops.iter().map(|op| op.path.clone()).collect();
    let pending = Pending {
        result: result.to_owned(),
        base: base.to_owned(),
        current,
        merged: merged.clone(),
        at: Utc::now(),
        files: files.clone(),
    };
    state.pending = Some(pending.clone());
    state.write(state_path)?;
    write_ops(dir, &prefix, &merged, &ops).await?;
    if let Some(bad) = verify(dir, &pending).await? {
        return Ok(Outcome::Conflict {
            files: bad,
            detail: format!(
                "the result was written but {} changed again at the same time; the apply is \
                 kept as unfinished — apply again to finish it",
                dir.display()
            ),
        });
    }
    record_applied(&mut state, state_path, result, &merged, files.clone())?;
    Ok(Outcome::Applied {
        result: result.to_owned(),
        files,
    })
}

/// Finishes an interrupted apply recorded in `state`.
async fn resume(dir: &Path, state_path: &Path, state: &mut State) -> Result<Outcome, CoreError> {
    let Some(pending) = state.pending.clone() else {
        return Ok(Outcome::AlreadyApplied {
            result: state
                .applied
                .as_ref()
                .map(|a| a.result.clone())
                .unwrap_or_default(),
        });
    };
    let prefix = prefix_of(dir).await?;
    let now = current_tree(dir).await?;
    let ops = ops_between(dir, &prefix, &pending.current, &pending.merged).await?;
    let now_entries = entries(dir, &now).await?;
    let mut remaining = Vec::new();
    let mut clashes = Vec::new();
    for op in &ops {
        match now_entries.get(&op.path).map(|e| e.oid.as_str()) {
            Some(oid) if Some(oid) == op.after.as_deref() => {}
            None if op.after.is_none() => {}
            Some(oid) if Some(oid) == op.before.as_deref() => remaining.push(op.clone()),
            None if op.before.is_none() => remaining.push(op.clone()),
            _ => clashes.push(op.path.clone()),
        }
    }
    // Mode-only changes have the same content on both sides; the check above cannot tell
    // whether they were done, so they are written again (idempotent).
    for op in &ops {
        if op.mode_only
            && !remaining.iter().any(|r| r.path == op.path)
            && !clashes.contains(&op.path)
        {
            remaining.push(op.clone());
        }
    }
    if !clashes.is_empty() {
        return Ok(Outcome::Conflict {
            detail: format!(
                "an apply to {} was interrupted, and these files changed since: {}",
                dir.display(),
                clashes.join(", ")
            ),
            files: clashes,
        });
    }
    if !remaining.is_empty() {
        write_ops(dir, &prefix, &pending.merged, &remaining).await?;
    }
    if let Some(bad) = verify(dir, &pending).await? {
        return Ok(Outcome::Conflict {
            detail: format!(
                "the interrupted apply could not be finished: {} did not come out as planned",
                bad.join(", ")
            ),
            files: bad,
        });
    }
    let files = pending.files.clone();
    record_applied(
        state,
        state_path,
        &pending.result,
        &pending.merged,
        files.clone(),
    )?;
    Ok(Outcome::Applied {
        result: pending.result,
        files,
    })
}

fn record_applied(
    state: &mut State,
    state_path: &Path,
    result: &str,
    tree: &str,
    files: Vec<String>,
) -> Result<(), CoreError> {
    state.applied = Some(Applied {
        result: result.to_owned(),
        tree: tree.to_owned(),
        at: Utc::now(),
        files,
    });
    state.pending = None;
    state.write(state_path)
}

/// Whether every planned path is in its after state in the working copy. `Some` names those
/// that are not.
async fn verify(dir: &Path, pending: &Pending) -> Result<Option<Vec<String>>, CoreError> {
    let now = current_tree(dir).await?;
    let prefix = prefix_of(dir).await?;
    let ops = ops_between(dir, &prefix, &pending.current, &pending.merged).await?;
    let entries = entries(dir, &now).await?;
    let mut bad = Vec::new();
    for op in &ops {
        let entry = entries.get(&op.path);
        match (entry, &op.after) {
            (None, None) => {}
            (Some(entry), Some(after)) if &entry.oid == after => {
                if op.mode_only && !entry.mode_ok(&op.mode) {
                    bad.push(op.path.clone());
                }
            }
            _ => bad.push(op.path.clone()),
        }
    }
    Ok((!bad.is_empty()).then_some(bad))
}

/// An index entry as `ls-tree` shows it.
struct Entry {
    oid: String,
    mode: String,
}

impl Entry {
    /// Whether the mode the working tree got is the one planned. Windows has no executable
    /// bit for git to see, so there 100644 and 100755 are the same entry.
    fn mode_ok(&self, want: &str) -> bool {
        if cfg!(windows) {
            matches!(
                (self.mode.as_str(), want),
                ("100644" | "100755", "100644" | "100755")
            )
        } else {
            self.mode == want
        }
    }
}

/// The tree git would record for `dir` now, built in an index of its own.
///
/// The index is seeded from the person's (a byte copy, never written): it carries the stat
/// cache, so an apply over a large repository need not hash every file for every step.
async fn current_tree(dir: &Path) -> Result<String, CoreError> {
    let index = Scratch::file("apply-index");
    let path = index.path();
    // Only at the repository's top: below it, the person's index carries content outside
    // the workspace, which is not part of what this run's result is compared with.
    let at_top = prefix_of(dir)
        .await
        .map(|prefix| prefix.is_empty())
        .unwrap_or(false);
    let seeded = at_top
        && match real_index(dir).await {
            Some(real) => tokio::fs::copy(real, path).await.is_ok(),
            None => false,
        };
    if !seeded {
        git(dir, &["read-tree", "HEAD"], Some(path)).await?;
    }
    let mut add = vec!["add", "-A", "--", "."];
    add.extend(NEVER_ADDED);
    if let Err(err) = git(dir, &add, Some(path)).await {
        if !seeded {
            return Err(err);
        }
        // A copied index the hub could not use (the person was writing it at that moment):
        // start from the commit instead.
        git(dir, &["read-tree", "HEAD"], Some(path)).await?;
        git(dir, &add, Some(path)).await?;
    }
    Ok(git(dir, &["write-tree"], Some(path))
        .await?
        .trim()
        .to_owned())
}

/// Where `dir`'s real index is, if it has one.
async fn real_index(dir: &Path) -> Option<PathBuf> {
    let out = git(dir, &["rev-parse", "--git-path", "index"], None)
        .await
        .ok()?;
    let path = PathBuf::from(out.trim());
    Some(if path.is_absolute() {
        path
    } else {
        dir.join(path)
    })
}

enum Merged {
    Tree(String),
    Conflicts(Vec<String>),
}

/// The three-way merge of the directory with the integration result, base `base`.
async fn merge_trees(
    dir: &Path,
    base: &str,
    ours: &str,
    theirs: &str,
) -> Result<Merged, CoreError> {
    let out = run(
        dir,
        &[
            "merge-tree",
            "--write-tree",
            "-z",
            "--name-only",
            "--no-messages",
            &format!("--merge-base={base}"),
            ours,
            theirs,
        ],
    )
    .await?;
    match out.code {
        0 => Ok(Merged::Tree(first_field(&out.stdout))),
        1 => {
            let mut fields = out.stdout.split(|b| *b == 0).map(bytes);
            let _tree = fields.next();
            Ok(Merged::Conflicts(
                fields.filter(|p| !p.is_empty()).collect(),
            ))
        }
        _ => Err(CoreError::Conflict(format!(
            "cannot merge the workers' result ({theirs}) with {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))),
    }
}

/// The changed paths between `from` and `to`, with their content before and after.
async fn ops_between(dir: &Path, prefix: &str, from: &str, to: &str) -> Result<Vec<Op>, CoreError> {
    let out = git(
        dir,
        &["diff-tree", "-r", "-z", "--no-renames", "--raw", from, to],
        None,
    )
    .await?;
    let fields: Vec<&str> = out.split('\0').collect();
    let mut ops = Vec::new();
    let mut i = 0;
    while i < fields.len() {
        let header = fields[i].trim();
        i += 1;
        if header.is_empty() {
            continue;
        }
        let path = match fields.get(i) {
            Some(path) => (*path).to_owned(),
            None => break,
        };
        i += 1;
        let Some(rest) = header.strip_prefix(':') else {
            continue;
        };
        let parts: Vec<&str> = rest.split(' ').collect();
        if parts.len() < 5 {
            continue;
        }
        let (old_mode, new_mode, old_sha, new_sha, status) =
            (parts[0], parts[1], parts[2], parts[3], parts[4]);
        if !path.starts_with(prefix) {
            return Err(CoreError::Conflict(format!(
                "the workers' result changes {path}, which is outside {}; it was not applied",
                dir.display()
            )));
        }
        let deleted = status.starts_with('D');
        let added = old_mode == "000000";
        ops.push(Op {
            before: (!added).then(|| old_sha.to_owned()),
            after: (!deleted).then(|| new_sha.to_owned()),
            mode: new_mode.to_owned(),
            mode_only: !deleted && !added && old_sha == new_sha && old_mode != new_mode,
            path,
        });
    }
    Ok(ops)
}

/// Writes every op into `dir`: filtered contents first staged with `checkout-index`, then
/// moved into place; deletions are removed last.
async fn write_ops(dir: &Path, prefix: &str, merged: &str, ops: &[Op]) -> Result<(), CoreError> {
    let stage = Scratch::dir("apply-stage");
    let index = Scratch::file("apply-out-index");
    git(dir, &["read-tree", merged], Some(index.path())).await?;
    let mut writes: Vec<(&Op, String)> = ops
        .iter()
        .filter(|op| op.after.is_some())
        .map(|op| {
            (
                op,
                op.path.strip_prefix(prefix).unwrap_or(&op.path).to_owned(),
            )
        })
        .collect();
    // `checkout-index` writes every path's *filtered* content (eol conversion, smudge
    // filters), exactly as a checkout would; batching keeps the command line short. Paths
    // are relative to the workspace, as the command reads them, not to the repository.
    for batch in writes.chunks_mut(100) {
        let prefix_arg = format!(
            "--prefix={}/",
            stage.path().display().to_string().replace('\\', "/")
        );
        let mut args = vec!["checkout-index", "-f", "-q", &prefix_arg, "--"];
        args.extend(batch.iter().map(|(_, rel)| rel.as_str()));
        git(dir, &args, Some(index.path())).await?;
    }
    for op in ops {
        let rel = op.path.strip_prefix(prefix).unwrap_or(&op.path);
        let dest = dir.join(rel);
        match &op.after {
            None => match tokio::fs::remove_file(&dest).await {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(CoreError::Storage(format!(
                        "cannot remove {}: {err}",
                        dest.display()
                    )));
                }
            },
            Some(_) => {
                // The prefix is joined to the repository-relative path, whatever the
                // argument was read as.
                let staged = stage.path().join(&op.path);
                place(&staged, &dest).await?;
            }
        }
    }
    Ok(())
}

/// Moves one staged file into the working tree, replacing what is there. A cross-device
/// temporary directory (Windows) falls back to copying the bytes and the permission bits.
async fn place(staged: &Path, dest: &Path) -> Result<(), CoreError> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| CoreError::Storage(err.to_string()))?;
    }
    match tokio::fs::symlink_metadata(dest).await {
        Ok(meta) if meta.is_dir() => {
            return Err(CoreError::Conflict(format!(
                "{} is a directory where the workers' result has a file; nothing was written",
                dest.display()
            )));
        }
        Ok(_) => {
            tokio::fs::remove_file(dest).await.map_err(|err| {
                CoreError::Storage(format!("cannot replace {}: {err}", dest.display()))
            })?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(CoreError::Storage(err.to_string())),
    }
    match tokio::fs::rename(staged, dest).await {
        Ok(()) => Ok(()),
        // Different volumes on Windows: copy the bytes and permissions, then drop the stage.
        Err(_) => {
            let bytes = tokio::fs::read(staged).await.map_err(|err| {
                CoreError::Storage(format!("cannot read {}: {err}", staged.display()))
            })?;
            tokio::fs::write(dest, bytes).await.map_err(|err| {
                CoreError::Storage(format!("cannot write {}: {err}", dest.display()))
            })?;
            if let Ok(meta) = tokio::fs::metadata(staged).await {
                let _ = tokio::fs::set_permissions(dest, meta.permissions()).await;
            }
            let _ = tokio::fs::remove_file(staged).await;
            Ok(())
        }
    }
}

/// Paths whose content differs between two trees.
async fn changed_between(dir: &Path, from: &str, to: &str) -> Result<Vec<String>, CoreError> {
    Ok(git(
        dir,
        &[
            "-c",
            "core.quotePath=false",
            "diff-tree",
            "-r",
            "-z",
            "--no-renames",
            "--name-only",
            from,
            to,
        ],
        None,
    )
    .await?
    .split('\0')
    .filter(|p| !p.is_empty())
    .map(str::to_owned)
    .collect())
}

/// Every blob in a tree, by repository-relative path.
async fn entries(dir: &Path, tree: &str) -> Result<BTreeMap<String, Entry>, CoreError> {
    let out = git(dir, &["ls-tree", "-r", "--full-tree", "-z", tree], None).await?;
    let mut map = BTreeMap::new();
    for record in out.split('\0').filter(|r| !r.is_empty()) {
        let Some((head, path)) = record.split_once('\t') else {
            continue;
        };
        let mut parts = head.split(' ');
        let mode = parts.next().unwrap_or_default().to_owned();
        let _kind = parts.next();
        let oid = parts.next().unwrap_or_default().to_owned();
        map.insert(path.to_owned(), Entry { oid, mode });
    }
    Ok(map)
}

/// What the workers changed in the conflicting paths, cut to [`CONFLICT_DETAIL`].
async fn worker_diff(dir: &Path, base: &str, result: &str, files: &[String]) -> String {
    if files.is_empty() {
        return String::new();
    }
    // From the repository's top: the paths a tree names are relative to it, not to the
    // workspace when the workspace is a directory below.
    let top = git(dir, &["rev-parse", "--show-toplevel"], None)
        .await
        .map(|top| top.trim().to_owned())
        .unwrap_or_else(|_| dir.display().to_string());
    let mut args = vec![
        "-C",
        &top,
        "diff",
        "--no-color",
        "--no-ext-diff",
        base,
        result,
        "--",
    ];
    args.extend(files.iter().map(String::as_str));
    let mut text = git(dir, &args, None).await.unwrap_or_default();
    if text.len() > CONFLICT_DETAIL {
        let mut end = CONFLICT_DETAIL;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n…");
    }
    text
}

/// The workspace's path inside its repository (`""` at the top): tree paths are relative to
/// the repository, the files to the workspace.
async fn prefix_of(dir: &Path) -> Result<String, CoreError> {
    Ok(git(dir, &["rev-parse", "--show-prefix"], None)
        .await?
        .trim()
        .to_owned())
}

struct Output {
    code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

async fn run(dir: &Path, args: &[&str]) -> Result<Output, CoreError> {
    let out = command(dir, args, None)
        .output()
        .await
        .map_err(|err| CoreError::Agent(format!("cannot run git in {}: {err}", dir.display())))?;
    Ok(Output {
        code: out.status.code().unwrap_or(-1),
        stdout: out.stdout,
        stderr: out.stderr,
    })
}

async fn git(dir: &Path, args: &[&str], index: Option<&Path>) -> Result<String, CoreError> {
    let out = command(dir, args, index)
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

fn command(dir: &Path, args: &[&str], index: Option<&Path>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("git");
    command.no_window().args(args).current_dir(dir);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    command
}

fn bytes(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).into_owned()
}

fn first_field(stdout: &[u8]) -> String {
    let end = stdout.iter().position(|b| *b == 0).unwrap_or(stdout.len());
    String::from_utf8_lossy(&stdout[..end]).trim().to_owned()
}

/// A temporary file or directory, gone when the apply step ends.
struct Scratch(PathBuf);

impl Scratch {
    fn file(kind: &str) -> Self {
        Self(std::env::temp_dir().join(format!(
            "crossagents-{kind}-{}-{}.tmp",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        )))
    }

    fn dir(kind: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "crossagents-{kind}-{}-{}.tmp",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let _ = std::fs::create_dir_all(&path);
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("tmp.lock"));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git_out(dir.path(), &["init", "-q", "-b", "main"]);
        git_out(dir.path(), &["config", "core.autocrlf", "false"]);
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "two\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        git_out(dir.path(), &["add", "-A"]);
        git_out(dir.path(), &["commit", "-q", "-m", "init"]);
        dir
    }

    /// A commit holding the working copy of `dir` (as a worker's result would): the snapshot
    /// of it, and the worker's tree on top.
    fn snapshot_commit(dir: &Path, message: &str) -> String {
        let index = Scratch::file("test-index");
        let path = index.path();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(dir)
                .env("GIT_INDEX_FILE", path)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        run(&["read-tree", "HEAD"]);
        run(&["add", "-A", "--", "."]);
        let tree = run(&["write-tree"]);
        run(&["commit-tree", &tree, "-p", "HEAD", "-m", message])
    }

    /// Commits everything in `dir` on top of `parent`, as a worker's integration result.
    fn result_commit(dir: &Path, parent: &str, message: &str) -> String {
        let tree = current_tree_blocking(dir);
        git_out(dir, &["commit-tree", &tree, "-p", parent, "-m", message])
    }

    fn current_tree_blocking(dir: &Path) -> String {
        let index = Scratch::file("test-out");
        let path = index.path();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(dir)
                .env("GIT_INDEX_FILE", path)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        run(&["read-tree", "HEAD"]);
        run(&["add", "-A", "--", "."]);
        run(&["write-tree"])
    }

    /// Everything the person's index and HEAD say; an apply must leave both alone. (The
    /// working tree itself changes — that is what an apply is for.)
    fn personal_state(dir: &Path) -> String {
        format!(
            "{}\n{}\n{}",
            git_out(dir, &["diff", "--cached"]),
            git_out(dir, &["ls-files", "--stage"]),
            git_out(dir, &["rev-parse", "HEAD"]),
        )
    }

    #[tokio::test]
    async fn a_clean_result_is_written_without_touching_the_index_or_committing() {
        let dir = repo();
        // The person's own work: an uncommitted edit and an untracked file.
        std::fs::write(dir.path().join("a.txt"), "person\n").unwrap();
        std::fs::write(dir.path().join("mine.txt"), "mine\n").unwrap();
        let base = snapshot_commit(dir.path(), "snapshot");
        // A worker edits b.txt, adds c.txt and deletes nothing, on top of the snapshot.
        std::fs::write(dir.path().join("b.txt"), "worker\n").unwrap();
        std::fs::write(dir.path().join("c.txt"), "new\n").unwrap();
        let result = result_commit(dir.path(), &base, "worker");
        // Back to the person's working copy.
        git_out(dir.path(), &["checkout", "--", "b.txt"]);
        std::fs::remove_file(dir.path().join("c.txt")).unwrap();

        let state = dir.path().join("state.json");
        let before = personal_state(dir.path());
        let outcome = apply(dir.path(), &base, &result, &state).await.unwrap();
        let Outcome::Applied { files, .. } = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(files, ["b.txt", "c.txt"]);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "worker\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("c.txt")).unwrap(),
            "new\n"
        );
        // The person's own work stayed as it was.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "person\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("mine.txt")).unwrap(),
            "mine\n"
        );
        assert_eq!(personal_state(dir.path()), before, "index or HEAD moved");
    }

    #[tokio::test]
    async fn deletions_moves_and_binaries_are_covered() {
        let dir = repo();
        let base = snapshot_commit(dir.path(), "snapshot");
        std::fs::remove_file(dir.path().join("a.txt")).unwrap();
        std::fs::rename(dir.path().join("b.txt"), dir.path().join("moved.txt")).unwrap();
        std::fs::write(dir.path().join("blob.bin"), [0u8, 159, 146, 150, 255]).unwrap();
        let result = result_commit(dir.path(), &base, "worker");
        // The person's working copy is back to the snapshot's.
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "two\n").unwrap();
        let _ = std::fs::remove_file(dir.path().join("moved.txt"));
        std::fs::remove_file(dir.path().join("blob.bin")).unwrap();

        let state = dir.path().join("state.json");
        let outcome = apply(dir.path(), &base, &result, &state).await.unwrap();
        assert!(outcome.applied(), "{outcome:?}");
        assert!(!dir.path().join("a.txt").exists());
        assert!(!dir.path().join("b.txt").exists());
        assert_eq!(
            std::fs::read(dir.path().join("blob.bin")).unwrap(),
            [0u8, 159, 146, 150, 255]
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("moved.txt")).unwrap(),
            "two\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_executable_bit_is_written() {
        use std::os::unix::fs::PermissionsExt;
        let dir = repo();
        let base = snapshot_commit(dir.path(), "snapshot");
        std::fs::write(dir.path().join("run.sh"), "#!/bin/sh\n").unwrap();
        let mut perms = std::fs::metadata(dir.path().join("run.sh"))
            .unwrap()
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(dir.path().join("run.sh"), perms).unwrap();
        let result = result_commit(dir.path(), &base, "worker");
        std::fs::remove_file(dir.path().join("run.sh")).unwrap();

        let outcome = apply(dir.path(), &base, &result, &dir.path().join("state.json"))
            .await
            .unwrap();
        assert!(outcome.applied(), "{outcome:?}");
        let mode = std::fs::metadata(dir.path().join("run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "mode {mode:o}");
    }

    #[tokio::test]
    async fn line_endings_follow_git_attributes() {
        let dir = repo();
        std::fs::write(dir.path().join(".gitattributes"), "*.txt text eol=crlf\n").unwrap();
        git_out(dir.path(), &["add", "-A"]);
        git_out(dir.path(), &["commit", "-q", "-m", "attributes"]);
        let base = snapshot_commit(dir.path(), "snapshot");
        std::fs::write(dir.path().join("b.txt"), "worker\n").unwrap();
        let result = result_commit(dir.path(), &base, "worker");
        // `commit-tree` took the normalized blobs; put the person's file back.
        std::fs::write(dir.path().join("b.txt"), "two\r\n").unwrap();

        let outcome = apply(dir.path(), &base, &result, &dir.path().join("state.json"))
            .await
            .unwrap();
        assert!(outcome.applied(), "{outcome:?}");
        let bytes = std::fs::read(dir.path().join("b.txt")).unwrap();
        assert_eq!(
            bytes, b"worker\r\n",
            "the checkout filter must write CRLF, got {bytes:?}"
        );
    }

    #[tokio::test]
    async fn a_conflict_with_the_person_writes_nothing() {
        let dir = repo();
        let base = snapshot_commit(dir.path(), "snapshot");
        std::fs::write(dir.path().join("b.txt"), "worker\n").unwrap();
        let result = result_commit(dir.path(), &base, "worker");
        // The person edited the same file after the snapshot.
        std::fs::write(dir.path().join("b.txt"), "person\n").unwrap();
        let before = personal_state(dir.path());

        let outcome = apply(dir.path(), &base, &result, &dir.path().join("state.json"))
            .await
            .unwrap();
        let Outcome::Conflict { files, detail } = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(files, ["b.txt"]);
        assert!(detail.contains("worker"), "{detail}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "person\n"
        );
        assert_eq!(personal_state(dir.path()), before);
    }

    #[tokio::test]
    async fn applying_the_same_result_again_writes_nothing() {
        let dir = repo();
        let base = snapshot_commit(dir.path(), "snapshot");
        std::fs::write(dir.path().join("b.txt"), "worker\n").unwrap();
        let result = result_commit(dir.path(), &base, "worker");
        std::fs::write(dir.path().join("b.txt"), "two\n").unwrap();
        let state = dir.path().join("state.json");

        let first = apply(dir.path(), &base, &result, &state).await.unwrap();
        assert!(matches!(first, Outcome::Applied { .. }), "{first:?}");
        // A second call — a retried accept, the person's button, a restart — is a no-op.
        std::fs::write(dir.path().join("b.txt"), "worker\nlater\n").unwrap();
        let again = apply(dir.path(), &base, &result, &state).await.unwrap();
        assert!(matches!(again, Outcome::AlreadyApplied { .. }), "{again:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "worker\nlater\n",
            "the person's later edit is not overwritten"
        );
    }

    #[tokio::test]
    async fn an_interrupted_apply_is_finished_without_redoing_or_overwriting() {
        let dir = repo();
        let base = snapshot_commit(dir.path(), "snapshot");
        std::fs::write(dir.path().join("a.txt"), "worker-a\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "worker-b\n").unwrap();
        std::fs::write(dir.path().join("c.txt"), "new\n").unwrap();
        let result = result_commit(dir.path(), &base, "worker");
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "two\n").unwrap();
        std::fs::remove_file(dir.path().join("c.txt")).unwrap();

        // What an interrupted apply leaves: a journal, one file written, one not, and one
        // the person touched after the interruption.
        let prefix = String::new();
        let current = current_tree(dir.path()).await.unwrap();
        let merged = match merge_trees(dir.path(), &base, &current, &result)
            .await
            .unwrap()
        {
            Merged::Tree(tree) => tree,
            Merged::Conflicts(files) => panic!("{files:?}"),
        };
        let ops = ops_between(dir.path(), &prefix, &current, &merged)
            .await
            .unwrap();
        let state_path = dir.path().join("state.json");
        let mut state = State::default();
        state.pending = Some(Pending {
            result: result.clone(),
            base: base.clone(),
            current,
            merged: merged.clone(),
            at: Utc::now(),
            files: ops.iter().map(|op| op.path.clone()).collect(),
        });
        state.write(&state_path).unwrap();
        // Write only a.txt out of the three.
        let only_a = ops
            .iter()
            .filter(|op| op.path == "a.txt")
            .cloned()
            .collect::<Vec<_>>();
        write_ops(dir.path(), &prefix, &merged, &only_a)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "worker-a\n"
        );
        // The person edits an unrelated file while the hub is down.
        std::fs::write(dir.path().join("mine.txt"), "mine\n").unwrap();

        let outcome = apply(dir.path(), &base, &result, &state_path)
            .await
            .unwrap();
        assert!(matches!(outcome, Outcome::Applied { .. }), "{outcome:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "worker-b\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("c.txt")).unwrap(),
            "new\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("mine.txt")).unwrap(),
            "mine\n",
            "the person's unrelated edit is kept"
        );
        // Done once.
        let again = apply(dir.path(), &base, &result, &state_path)
            .await
            .unwrap();
        assert!(matches!(again, Outcome::AlreadyApplied { .. }), "{again:?}");
    }

    #[tokio::test]
    async fn a_write_that_fails_halfway_leaves_a_record_and_finishes_on_the_next_call() {
        let dir = repo();
        let base = snapshot_commit(dir.path(), "snapshot");
        std::fs::write(dir.path().join("a.txt"), "worker-a\n").unwrap();
        std::fs::write(dir.path().join("c.txt"), "new\n").unwrap();
        let result = result_commit(dir.path(), &base, "worker");
        // Back to the person's content, with a directory where the result adds a file: the
        // write of a.txt comes first and works, c.txt then fails.
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        std::fs::remove_file(dir.path().join("c.txt")).unwrap();
        std::fs::create_dir(dir.path().join("c.txt")).unwrap();
        let state_path = dir.path().join("state.json");
        let before = personal_state(dir.path());

        let failed = apply(dir.path(), &base, &result, &state_path).await;
        assert!(failed.is_err(), "the write had to fail: {failed:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "worker-a\n",
            "the file written before the failure stays written"
        );
        assert!(dir.path().join("c.txt").is_dir());
        let journal = State::read(&state_path).unwrap();
        assert!(
            journal.pending.is_some() && journal.applied.is_none(),
            "a half-written apply is on record: {journal:?}"
        );

        // Whatever caused the failure is gone: the next call finishes it, without redoing
        // what is already there or touching the person's index.
        std::fs::remove_dir(dir.path().join("c.txt")).unwrap();
        let outcome = apply(dir.path(), &base, &result, &state_path)
            .await
            .unwrap();
        assert!(matches!(outcome, Outcome::Applied { .. }), "{outcome:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("c.txt")).unwrap(),
            "new\n"
        );
        assert_eq!(personal_state(dir.path()), before);
        assert!(State::read(&state_path).unwrap().applied.is_some());
    }

    #[tokio::test]
    async fn a_later_result_applies_on_top_of_the_last_one() {
        let dir = repo();
        let base = snapshot_commit(dir.path(), "snapshot");
        std::fs::write(dir.path().join("b.txt"), "first\n").unwrap();
        let first = result_commit(dir.path(), &base, "first");
        std::fs::write(dir.path().join("b.txt"), "two\n").unwrap();
        let state = dir.path().join("state.json");
        apply(dir.path(), &base, &first, &state).await.unwrap();
        // The person edits what the first worker wrote; a second worker adds another file.
        std::fs::write(dir.path().join("b.txt"), "first, edited\n").unwrap();
        std::fs::write(dir.path().join("d.txt"), "second\n").unwrap();
        let second = result_commit(dir.path(), &first, "second");
        std::fs::remove_file(dir.path().join("d.txt")).unwrap();

        // Base for the new delta is the first result, not the snapshot: the person's edit is
        // theirs, not a conflict with work already applied.
        let outcome = apply(dir.path(), &first, &second, &state).await.unwrap();
        assert!(matches!(outcome, Outcome::Applied { .. }), "{outcome:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "first, edited\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("d.txt")).unwrap(),
            "second\n"
        );
    }

    #[tokio::test]
    async fn a_workspace_below_the_repository_top_is_still_the_person_s() {
        let dir = repo();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/x.txt"), "x\n").unwrap();
        git_out(dir.path(), &["add", "-A"]);
        git_out(dir.path(), &["commit", "-q", "-m", "sub"]);
        let workspace = dir.path().join("sub");
        // A snapshot from the subdirectory holds the whole tree; its commit is the base.
        let base = snapshot_commit(&workspace, "snapshot");
        std::fs::write(workspace.join("x.txt"), "worker\n").unwrap();
        let result = result_commit(&workspace, &base, "worker");
        std::fs::write(workspace.join("x.txt"), "x\n").unwrap();

        let outcome = apply(&workspace, &base, &result, &workspace.join("state.json"))
            .await
            .unwrap();
        assert!(outcome.applied(), "{outcome:?}");
        assert_eq!(
            std::fs::read_to_string(workspace.join("x.txt")).unwrap(),
            "worker\n"
        );
        // Nothing above the workspace was touched.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "one\n"
        );
    }
}
