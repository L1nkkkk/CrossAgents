//! Reading a workspace's files without changing anything: which path a client's string names
//! (relative, absolute, or a symlink that leads out of the workspace), one directory of it,
//! and one file — from the working copy, or from what a git ref held (docs/DESIGN.md §11).

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

use crossagents_core::process::NoWindow;
use crossagents_core::{Agent, CoreError, RunId, TaskId};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::engine::Orchestrator;

/// Bytes of a file handed out at once; more is cut and flagged.
pub const CONTENT_LIMIT: usize = 1024 * 1024;

/// Most paths one `files/search` answers with.
pub const SEARCH_LIMIT: usize = 200;

/// Bytes looked at to tell text from binary.
const SNIFF: usize = 8 * 1024;

/// Most paths one `files/resolve` may carry.
pub const RESOLVE_LIMIT: usize = 200;

/// Why a file request was refused. The API answers 403, 404, 400 and 500 for these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileError {
    /// The path leads out of the workspace, through `..` or a symlink.
    Outside(String),
    /// Nothing of that name is there.
    NotFound(String),
    /// Not something the file API reads: a bad ref, `..` at a ref, a directory as content.
    BadRequest(String),
    /// The file system or git failed.
    Failed(String),
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Outside(what) => write!(f, "path is outside the workspace: {what}"),
            Self::NotFound(what) => write!(f, "not found: {what}"),
            Self::BadRequest(what) => write!(f, "{what}"),
            Self::Failed(what) => write!(f, "{what}"),
        }
    }
}

impl std::error::Error for FileError {}

impl From<CoreError> for FileError {
    fn from(err: CoreError) -> Self {
        match err {
            err @ CoreError::NotFound { .. } => Self::NotFound(err.to_string()),
            err @ CoreError::Conflict(_) => Self::BadRequest(err.to_string()),
            err => Self::Failed(err.to_string()),
        }
    }
}

/// What an entry of a workspace is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    Symlink,
}

/// One entry of a directory listing; `path` is relative to the workspace root, with `/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub kind: Kind,
    pub size: Option<u64>,
    /// Matched by .gitignore (only listed with `all`).
    pub ignored: bool,
}

/// One directory of a workspace, as `files/tree` answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    pub root: PathBuf,
    /// The directory listed, relative to the root ("" for the root).
    pub path: String,
    pub entries: Vec<Entry>,
}

/// A file's bytes, as the file API hands them out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contents {
    /// The whole file's size, not what was read.
    pub size: u64,
    pub binary: bool,
    pub truncated: bool,
    pub text: Option<String>,
}

impl Contents {
    /// `bytes` is what was read (at most [`CONTENT_LIMIT`]); `size` is the whole file's.
    fn new(size: u64, bytes: &[u8], truncated: bool) -> Self {
        let binary = !is_text(bytes);
        Self {
            size,
            binary,
            truncated,
            text: (!binary).then(|| String::from_utf8_lossy(bytes).into_owned()),
        }
    }
}

/// What git says changed at one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// A new file, tracked or not.
    Added,
    /// Content changed, renamed, copied or conflicted.
    Modified,
    /// The working copy no longer has what the index does.
    Deleted,
}

/// One changed path of a working copy, relative to its workspace root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEntry {
    pub path: String,
    pub change: Change,
}

/// The files matching a name query across a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    pub paths: Vec<String>,
    /// There were more than [`SEARCH_LIMIT`] matches; only the first are here.
    pub truncated: bool,
}

/// One file of a workspace, as `files/content` answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileView {
    pub root: PathBuf,
    pub path: String,
    /// Where the file is on disk; at a ref, where it would be.
    pub abs_path: PathBuf,
    pub contents: Contents,
}

/// A path that names something inside a workspace, as `files/resolve` answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub path: String,
    pub abs_path: PathBuf,
    pub kind: Kind,
}

/// Whether `head` reads as text: UTF-8 without NUL, in its first [`SNIFF`] bytes. A
/// multi-byte character cut by the end of `head` does not make a file binary.
fn is_text(head: &[u8]) -> bool {
    let head = &head[..head.len().min(SNIFF)];
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        // An unexpected end of input is a character cut here, not a bad byte.
        Err(err) => err.error_len().is_none(),
    }
}

/// `path` as components for comparison: a drive as `C:`, a root as `""`.
fn parts(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|part| match part {
            Component::Prefix(prefix) => {
                Some(prefix.as_os_str().to_string_lossy().replace('\\', "/"))
            }
            Component::RootDir => Some(String::new()),
            Component::CurDir => None,
            Component::ParentDir => Some("..".to_owned()),
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
        })
        .collect()
}

/// Whether two components name the same thing: Windows ignores case.
fn same(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// `abs` relative to `root` with `/`, when it is inside it; Windows compares without case
/// and ignores a `\\?\` prefix.
pub fn rel_of(root: &Path, abs: &Path) -> Option<String> {
    let root = parts(root);
    let abs = parts(abs);
    if abs.len() < root.len() || !root.iter().zip(&abs).all(|(r, a)| same(r, a)) {
        return None;
    }
    Some(abs[root.len()..].join("/"))
}

/// Whether `input` (already with `/`) names an absolute path: a root, or a drive letter on
/// Windows (`C:/x`) — which is what a client pasting a Windows path sends.
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

/// The workspace root as the file system names it; missing, it is not there to read.
fn canonical_dir(dir: &Path) -> Result<PathBuf, FileError> {
    let abs = dunce::canonicalize(dir)
        .map_err(|_| FileError::NotFound(format!("no workspace at {}", dir.display())))?;
    if !abs.is_dir() {
        return Err(FileError::NotFound(format!(
            "no workspace at {}",
            dir.display()
        )));
    }
    Ok(abs)
}

/// The path `input` names inside the workspace at `root`: the canonical path and its place
/// relative to the root with `/`. A relative path is taken from the root, an absolute one
/// (POSIX, or Windows `C:\x` / `C:/x`) as it is; `\` counts as `/`. A path that leads out of
/// the root, through `..` or a symlink, is refused.
pub fn resolve_in_root(root: &Path, input: &str) -> Result<(PathBuf, String), FileError> {
    let root = canonical_dir(root)?;
    let path = input.replace('\\', "/");
    if path.contains('\0') {
        return Err(FileError::BadRequest(
            "the path contains a NUL byte".to_owned(),
        ));
    }
    let joined = if is_absolute(&path) {
        PathBuf::from(&path)
    } else {
        root.join(&path)
    };
    let abs = dunce::canonicalize(&joined)
        .map_err(|_| FileError::NotFound(format!("{input} in {}", root.display())))?;
    let rel = rel_of(&root, &abs).ok_or_else(|| FileError::Outside(input.to_owned()))?;
    // `.git` is never part of a workspace's files, not even by a path that names it.
    if rel.split('/').any(|part| part.eq_ignore_ascii_case(".git")) {
        return Err(FileError::NotFound(format!(
            "{input} is not part of the file API"
        )));
    }
    Ok((abs, rel))
}

/// The path inside a tree that `input` names for a read at a git ref: relative, with `/`,
/// without `..` (a ref holds one tree; there is nothing above it to read).
pub fn check_rel_for_ref(input: &str) -> Result<String, FileError> {
    let path = input.replace('\\', "/");
    if path.contains('\0') {
        return Err(FileError::BadRequest(
            "the path contains a NUL byte".to_owned(),
        ));
    }
    if is_absolute(&path) {
        return Err(FileError::Outside(path));
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                return Err(FileError::BadRequest(format!(
                    "{input:?} has a `..` segment, and a ref holds one tree"
                )));
            }
            name => parts.push(name),
        }
    }
    Ok(parts.join("/"))
}

/// Checked before git sees it: a branch, tag or sha, never an option, a range or a path
/// (`--output=x`, `a..b`, `HEAD:file`).
pub fn check_ref(git_ref: &str) -> Result<(), FileError> {
    let name = git_ref.trim();
    let bad = |why: &str| FileError::BadRequest(format!("{git_ref:?} is not a ref to read: {why}"));
    if name.is_empty() {
        return Err(bad("it is empty"));
    }
    if name.len() > 256 {
        return Err(bad("it is too long"));
    }
    if name.starts_with('-') {
        return Err(bad("it starts with `-`"));
    }
    if name.starts_with('/') || name.ends_with('/') || name.ends_with('.') {
        return Err(bad(
            "a ref has no leading or trailing `/` and no trailing `.`",
        ));
    }
    if name.contains("..") || name.contains("//") || name.contains("@{") {
        return Err(bad("it is a range, not a ref"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./+@#".contains(c))
    {
        return Err(bad("it has characters no ref has"));
    }
    Ok(())
}

/// One directory of a working copy: `.git` never, what `.gitignore` hides only with `all`
/// (marked `ignored`). A directory outside a git repository filters nothing.
pub async fn list_dir(dir: &Path, rel: &str, all: bool) -> Result<Vec<Entry>, FileError> {
    if !dir.is_dir() {
        return Err(FileError::BadRequest(format!("{rel} is not a directory")));
    }
    let read = std::fs::read_dir(dir)
        .map_err(|e| FileError::Failed(format!("cannot read {}: {e}", dir.display())))?;
    let mut entries = Vec::new();
    for item in read {
        let item =
            item.map_err(|e| FileError::Failed(format!("cannot read {}: {e}", dir.display())))?;
        let name = item.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        // Symlink metadata: an entry that is a link is listed as one, whatever it points at
        // (reading through it is what the resolver refuses when it leads out).
        let meta = item
            .metadata()
            .map_err(|e| FileError::Failed(format!("cannot read {name}: {e}")))?;
        let kind = if meta.is_symlink() {
            Kind::Symlink
        } else if meta.is_dir() {
            Kind::Dir
        } else {
            Kind::File
        };
        entries.push(Entry {
            path: rel_path(rel, &name),
            name,
            kind,
            size: (kind == Kind::File).then_some(meta.len()),
            ignored: false,
        });
    }
    if let Some(ignored) = ignored_in(dir, &entries).await {
        for entry in &mut entries {
            entry.ignored = ignored.contains(&entry.name);
        }
    }
    if !all {
        entries.retain(|entry| !entry.ignored);
    }
    sort_entries(&mut entries);
    Ok(entries)
}

/// `rel` with `name` under it, with `/`.
fn rel_path(rel: &str, name: &str) -> String {
    if rel.is_empty() {
        name.to_owned()
    } else {
        format!("{rel}/{name}")
    }
}

/// Directories first, then by name.
fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        (b.kind == Kind::Dir)
            .cmp(&(a.kind == Kind::Dir))
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// The names of `entries` that `.gitignore` hides; `None` when `dir` is not in a git
/// repository (or git is not there), in which case nothing is hidden.
async fn ignored_in(dir: &Path, entries: &[Entry]) -> Option<std::collections::HashSet<String>> {
    if entries.is_empty() {
        return Some(std::collections::HashSet::new());
    }
    let mut child = Command::new("git")
        .no_window()
        .args(["check-ignore", "--stdin"])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let names: Vec<&str> = entries
        .iter()
        .map(|entry| entry.name.as_str())
        .filter(|name| !name.contains('\n'))
        .collect();
    let mut input = names.join("\n");
    input.push('\n');
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes()).await.ok()?;
        // Dropping it closes the pipe, so git answers.
    }
    let output = child.wait_with_output().await.ok()?;
    // 0: some are ignored, 1: none; anything else is "not a repository".
    if !matches!(output.status.code(), Some(0) | Some(1)) {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| line.trim_end_matches('/').to_owned())
            .collect(),
    )
}

/// A file of a working copy, its size from the file system.
pub fn read_file(abs: &Path, rel: &str) -> Result<Contents, FileError> {
    let meta = std::fs::metadata(abs).map_err(|_| FileError::NotFound(rel.to_owned()))?;
    if !meta.is_file() {
        return Err(FileError::BadRequest(format!(
            "{rel} is not a regular file"
        )));
    }
    let size = meta.len();
    let mut reader = std::fs::File::open(abs)
        .map_err(|e| FileError::Failed(format!("cannot read {rel}: {e}")))?
        .take(CONTENT_LIMIT as u64);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| FileError::Failed(format!("cannot read {rel}: {e}")))?;
    Ok(Contents::new(size, &bytes, size > CONTENT_LIMIT as u64))
}

/// The workspace's uncommitted changes, relative to its root; `None` outside a git
/// repository (or when git is not there), where there is nothing to show.
pub async fn changes(root: &Path) -> Result<Option<Vec<ChangeEntry>>, FileError> {
    let prefix = match git_text(root, &["rev-parse", "--show-prefix"]).await {
        Ok(Some(prefix)) => prefix,
        Ok(None) => return Ok(None),
        // `git` says so when the directory is not inside a repository.
        Err(FileError::BadRequest(_)) => return Ok(None),
        Err(err) => return Err(err),
    };
    let Some(out) = git(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )
    .await?
    else {
        return Ok(Some(Vec::new()));
    };
    let fields: Vec<&[u8]> = out
        .split(|byte| *byte == 0)
        .filter(|f| !f.is_empty())
        .collect();
    let mut found = std::collections::BTreeMap::new();
    let mut i = 0;
    while i < fields.len() {
        let field = fields[i];
        i += 1;
        if field.len() < 4 {
            continue;
        }
        let code = &field[..2];
        let path = String::from_utf8_lossy(&field[3..]).replace('\\', "/");
        // A rename or copy carries its old path as the next record.
        if code.contains(&b'R') || code.contains(&b'C') {
            i += 1;
        }
        let Some(rel) = workspace_path(&path, &prefix) else {
            continue;
        };
        if rel.split('/').any(|part| part.eq_ignore_ascii_case(".git")) {
            continue;
        }
        found.insert(rel, change_of(code));
    }
    Ok(Some(
        found
            .into_iter()
            .map(|(path, change)| ChangeEntry { path, change })
            .collect(),
    ))
}

/// `path` (relative to the repository root) inside the workspace `prefix` names; `None`
/// for another part of the repository.
fn workspace_path(path: &str, prefix: &str) -> Option<String> {
    if prefix.is_empty() {
        return Some(path.trim_start_matches('/').to_owned());
    }
    path.strip_prefix(prefix)
        .map(str::to_owned)
        .filter(|rel| !rel.is_empty())
}

fn change_of(code: &[u8]) -> Change {
    if code == b"??" || code.contains(&b'A') {
        Change::Added
    } else if code.contains(&b'D') {
        Change::Deleted
    } else {
        Change::Modified
    }
}

/// The workspace's files whose path contains `query`, case-insensitively, across every
/// directory (not only what a client loaded). At most [`SEARCH_LIMIT`].
pub async fn search(root: &Path, query: &str, all: bool) -> Result<Search, FileError> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(Search {
            paths: Vec::new(),
            truncated: false,
        });
    }
    let mut paths = match git_names(root, all).await? {
        Some(paths) => paths,
        None => walk_names(root)?,
    };
    paths.retain(|path| is_regular_file(root, path));
    paths.sort();
    paths.dedup();
    paths.retain(|path| path.to_lowercase().contains(&needle));
    let truncated = paths.len() > SEARCH_LIMIT;
    paths.truncate(SEARCH_LIMIT);
    Ok(Search { paths, truncated })
}

/// The paths git knows in the workspace (tracked and untracked); `None` outside a
/// repository, where the caller walks the file system instead.
async fn git_names(root: &Path, all: bool) -> Result<Option<Vec<String>>, FileError> {
    let found = match git(
        root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )
    .await
    {
        Ok(found) => found,
        Err(FileError::BadRequest(_)) => return Ok(None),
        Err(err) => return Err(err),
    };
    let Some(out) = found else {
        return Ok(Some(Vec::new()));
    };
    let mut paths = nul_paths(&out);
    if all
        && let Ok(Some(out)) = git(
            root,
            &[
                "ls-files",
                "-z",
                "--others",
                "--ignored",
                "--exclude-standard",
            ],
        )
        .await
    {
        paths.extend(nul_paths(&out));
    }
    Ok(Some(paths))
}

fn nul_paths(out: &[u8]) -> Vec<String> {
    out.split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .map(|field| String::from_utf8_lossy(field).replace('\\', "/"))
        .collect()
}

fn is_regular_file(root: &Path, rel: &str) -> bool {
    rel.split('/')
        .all(|part| !part.is_empty() && part != ".git")
        && std::fs::symlink_metadata(root.join(rel))
            .map(|meta| meta.is_file())
            .unwrap_or(false)
}

/// Every regular file under `root`, without following symlinks (so a link out of the
/// workspace cannot bring its target in).
fn walk_names(root: &Path) -> Result<Vec<String>, FileError> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let read = std::fs::read_dir(&dir)
            .map_err(|e| FileError::Failed(format!("cannot read {}: {e}", dir.display())))?;
        for item in read {
            let item =
                item.map_err(|e| FileError::Failed(format!("cannot read {}: {e}", dir.display())))?;
            let path = item.path();
            let name = item.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                continue;
            }
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|e| FileError::Failed(format!("cannot read {}: {e}", path.display())))?;
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file()
                && let Ok(rel) = path.strip_prefix(root)
            {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    Ok(out)
}

/// One directory of `git_ref`'s tree; everything in a tree is tracked, so nothing is hidden.
pub async fn list_dir_at(root: &Path, git_ref: &str, rel: &str) -> Result<Vec<Entry>, FileError> {
    let commit = commit_of(root, git_ref).await?;
    let spec = tree_spec(&commit, rel);
    let kind = git_text(root, &["cat-file", "-t", &spec])
        .await?
        .ok_or_else(|| FileError::NotFound(format!("no {rel} at {git_ref}")))?;
    if kind != "tree" {
        return Err(FileError::BadRequest(format!(
            "{rel} is not a directory at {git_ref}"
        )));
    }
    let out = git(root, &["ls-tree", "-z", "-l", &spec])
        .await?
        .ok_or_else(|| FileError::NotFound(format!("no {rel} at {git_ref}")))?;
    let mut entries = Vec::new();
    for record in out.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
        let record = String::from_utf8_lossy(record);
        let Some((head, name)) = record.split_once('\t') else {
            continue;
        };
        let fields: Vec<&str> = head.split_whitespace().collect();
        let [mode, _, _, size] = fields.as_slice() else {
            continue;
        };
        let kind = match *mode {
            "120000" => Kind::Symlink,
            "040000" | "160000" => Kind::Dir,
            _ => Kind::File,
        };
        entries.push(Entry {
            path: rel_path(rel, name),
            name: name.to_owned(),
            kind,
            size: if kind == Kind::File {
                size.parse::<u64>().ok()
            } else {
                None
            },
            ignored: false,
        });
    }
    sort_entries(&mut entries);
    Ok(entries)
}

/// A file at `git_ref`, its size from the tree; the content itself is cut at
/// [`CONTENT_LIMIT`].
pub async fn read_file_at(root: &Path, git_ref: &str, rel: &str) -> Result<Contents, FileError> {
    let commit = commit_of(root, git_ref).await?;
    if rel.is_empty() {
        return Err(FileError::BadRequest(
            "the workspace root is a directory".to_owned(),
        ));
    }
    let spec = format!("{commit}:{rel}");
    let missing = || FileError::NotFound(format!("no {rel} at {git_ref}"));
    let kind = git_text(root, &["cat-file", "-t", &spec])
        .await?
        .ok_or_else(missing)?;
    if kind != "blob" {
        return Err(FileError::BadRequest(format!(
            "{rel} is not a regular file at {git_ref}"
        )));
    }
    let size = git_text(root, &["cat-file", "-s", &spec])
        .await?
        .and_then(|text| text.parse::<u64>().ok())
        .ok_or_else(missing)?;
    let bytes = blob(root, &spec, size).await?;
    Ok(Contents::new(size, &bytes, size > CONTENT_LIMIT as u64))
}

/// `<commit>:` is the commit's root tree; `<commit>:<rel>` the tree of a directory in it.
fn tree_spec(commit: &str, rel: &str) -> String {
    format!("{commit}:{rel}")
}

/// Runs git in `dir`: its stdout, or `None` when it says no (exit 1, or a path that is not
/// in the tree). Outside a repository that is the request's fault, not a failure.
async fn git(dir: &Path, args: &[&str]) -> Result<Option<Vec<u8>>, FileError> {
    let output = Command::new("git")
        .no_window()
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .map_err(|e| FileError::Failed(format!("cannot run git: {e}")))?;
    if output.status.success() {
        return Ok(Some(output.stdout));
    }
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.contains("not a git repository") {
        return Err(FileError::BadRequest(format!(
            "{} is not in a git repository",
            dir.display()
        )));
    }
    // A spec that names nothing in a tree is an answer, not a failure.
    if stderr.contains("does not exist") || stderr.contains("Not a valid object name") {
        return Ok(None);
    }
    Err(FileError::Failed(format!(
        "git {} failed: {stderr}",
        args.join(" ")
    )))
}

/// [`git`] as trimmed text.
async fn git_text(dir: &Path, args: &[&str]) -> Result<Option<String>, FileError> {
    Ok(git(dir, args)
        .await?
        .map(|out| String::from_utf8_lossy(&out).trim().to_owned()))
}

/// The commit a checked ref names.
async fn commit_of(root: &Path, git_ref: &str) -> Result<String, FileError> {
    check_ref(git_ref)?;
    let spec = format!("{git_ref}^{{commit}}");
    git_text(root, &["rev-parse", "--verify", "-q", &spec])
        .await?
        .filter(|sha| !sha.is_empty())
        .ok_or_else(|| FileError::NotFound(format!("no commit named {git_ref}")))
}

/// The first [`CONTENT_LIMIT`] bytes of a blob (all of it when smaller); `size` is what
/// `cat-file -s` said.
async fn blob(root: &Path, spec: &str, size: u64) -> Result<Vec<u8>, FileError> {
    let mut child = Command::new("git")
        .no_window()
        .args(["cat-file", "blob", spec])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| FileError::Failed(format!("cannot run git: {e}")))?;
    let Some(stdout) = child.stdout.take() else {
        return Err(FileError::Failed("cannot read git's output".to_owned()));
    };
    let mut bytes = Vec::new();
    let read = stdout
        .take(CONTENT_LIMIT as u64)
        .read_to_end(&mut bytes)
        .await;
    if size > CONTENT_LIMIT as u64 {
        // What was read is all that is handed out; stop git writing the rest.
        let _ = child.kill().await;
        read.map_err(|e| FileError::Failed(format!("cannot read {spec}: {e}")))?;
        return Ok(bytes);
    }
    let status = child
        .wait()
        .await
        .map_err(|e| FileError::Failed(format!("cannot read {spec}: {e}")))?;
    read.map_err(|e| FileError::Failed(format!("cannot read {spec}: {e}")))?;
    if !status.success() {
        return Err(FileError::NotFound(format!("no {spec}")));
    }
    Ok(bytes)
}

/// The path a read at `git_ref` names: relative, or absolute inside the root.
fn rel_at(root: &Path, input: &str) -> Result<String, FileError> {
    let path = input.replace('\\', "/");
    if path.split('/').any(|part| part == "..") {
        return Err(FileError::BadRequest(format!(
            "{input:?} has a `..` segment, and a ref holds one tree"
        )));
    }
    if path.contains('\0') {
        return Err(FileError::BadRequest(
            "the path contains a NUL byte".to_owned(),
        ));
    }
    if is_absolute(&path) {
        return rel_of(root, Path::new(&path)).ok_or_else(|| FileError::Outside(input.to_owned()));
    }
    check_rel_for_ref(&path)
}

/// What `abs` is now; `None` when it is gone.
fn kind_of(abs: &Path) -> Option<Kind> {
    let meta = std::fs::metadata(abs).ok()?;
    Some(if meta.is_dir() { Kind::Dir } else { Kind::File })
}

/// Whose files a request reads.
#[derive(Debug, Clone, Copy)]
pub enum FilesScope<'a> {
    /// A session's workspace (`agent.cwd`).
    Agent(&'a Agent),
    /// A run's integration checkout.
    Run(RunId),
    /// A task's worktree.
    Task { run: RunId, task: TaskId },
}

impl Orchestrator {
    /// The directory whose files `scope` offers, as the file system names it. A task
    /// without a worktree (not started, or ended) has nothing to read.
    pub async fn files_root(&self, scope: FilesScope<'_>) -> Result<PathBuf, FileError> {
        let dir = match scope {
            FilesScope::Agent(agent) => agent.cwd.clone(),
            FilesScope::Run(run) => {
                let run = self.store.get_run(run).await?;
                self.integration_path(&run)
            }
            FilesScope::Task { run, task } => {
                let task = self.store.get_task(task).await?;
                if task.run_id != run {
                    return Err(FileError::NotFound(format!("no task {}", task.key)));
                }
                task.worktree.clone().ok_or_else(|| {
                    FileError::NotFound(format!(
                        "#{} has no worktree (not started, or ended)",
                        task.key
                    ))
                })?
            }
        };
        canonical_dir(&dir)
    }

    /// One directory of `scope`, from the working copy or from `git_ref`.
    pub async fn files_tree(
        &self,
        scope: FilesScope<'_>,
        path: &str,
        git_ref: Option<&str>,
        all: bool,
    ) -> Result<Tree, FileError> {
        let root = self.files_root(scope).await?;
        match git_ref {
            None => {
                let (dir, rel) = resolve_in_root(&root, path)?;
                let entries = list_dir(&dir, &rel, all).await?;
                Ok(Tree {
                    root,
                    path: rel,
                    entries,
                })
            }
            Some(git_ref) => {
                let rel = rel_at(&root, path)?;
                let entries = list_dir_at(&root, git_ref, &rel).await?;
                Ok(Tree {
                    root,
                    path: rel,
                    entries,
                })
            }
        }
    }

    /// One file of `scope`, from the working copy or from `git_ref`.
    pub async fn files_content(
        &self,
        scope: FilesScope<'_>,
        path: &str,
        git_ref: Option<&str>,
    ) -> Result<FileView, FileError> {
        let root = self.files_root(scope).await?;
        match git_ref {
            None => {
                let (abs_path, path) = resolve_in_root(&root, path)?;
                let contents = read_file(&abs_path, &path)?;
                Ok(FileView {
                    root,
                    path,
                    abs_path,
                    contents,
                })
            }
            Some(git_ref) => {
                let path = rel_at(&root, path)?;
                let contents = read_file_at(&root, git_ref, &path).await?;
                let abs_path = if path.is_empty() {
                    root.clone()
                } else {
                    root.join(&path)
                };
                Ok(FileView {
                    root,
                    path,
                    abs_path,
                    contents,
                })
            }
        }
    }

    /// The workspace's uncommitted changes; `None` outside a git repository.
    pub async fn files_status(
        &self,
        scope: FilesScope<'_>,
    ) -> Result<(PathBuf, Option<Vec<ChangeEntry>>), FileError> {
        let root = self.files_root(scope).await?;
        let changes = changes(&root).await?;
        Ok((root, changes))
    }

    /// Files across the whole workspace whose path contains `query`.
    pub async fn files_search(
        &self,
        scope: FilesScope<'_>,
        query: &str,
        all: bool,
    ) -> Result<(PathBuf, Search), FileError> {
        let root = self.files_root(scope).await?;
        let found = search(&root, query, all).await?;
        Ok((root, found))
    }

    /// Which of `inputs` name something inside `scope`, in their order; `None` for a path
    /// that is missing or outside, rather than an error.
    pub async fn files_resolve(
        &self,
        scope: FilesScope<'_>,
        inputs: &[String],
    ) -> Result<Vec<Option<Resolved>>, FileError> {
        if inputs.len() > RESOLVE_LIMIT {
            return Err(FileError::BadRequest(format!(
                "at most {RESOLVE_LIMIT} paths at a time"
            )));
        }
        let root = self.files_root(scope).await?;
        Ok(inputs
            .iter()
            .map(|input| match resolve_in_root(&root, input) {
                Ok((abs_path, path)) => kind_of(&abs_path).map(|kind| Resolved {
                    path,
                    abs_path,
                    kind,
                }),
                Err(_) => None,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// A repository at `dir`, with `--file`, committed as `message`.
    async fn commit(dir: &Path, message: &str) {
        assert!(
            matches!(git(dir, &["add", "-A"]).await, Ok(Some(_))),
            "cannot stage {message}"
        );
        let done = git(
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
        assert!(matches!(done, Ok(Some(_))), "cannot commit {message}");
    }

    #[test]
    fn paths_resolve_inside_the_root() {
        let dir = temp();
        std::fs::create_dir_all(dir.path().join("a")).unwrap();
        std::fs::write(dir.path().join("a/b.txt"), "hello\n").unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();

        let (abs, rel) = resolve_in_root(&root, "a/b.txt").unwrap();
        assert_eq!(rel, "a/b.txt");
        assert_eq!(abs, root.join("a").join("b.txt"));

        // An absolute path inside the root names the same file.
        let (_, rel) = resolve_in_root(&root, &abs.display().to_string()).unwrap();
        assert_eq!(rel, "a/b.txt");

        // The root itself is the empty path.
        let (abs, rel) = resolve_in_root(&root, "").unwrap();
        assert_eq!(rel, "");
        assert_eq!(abs, root);

        // A path that is not there is not found.
        assert!(matches!(
            resolve_in_root(&root, "a/nope.txt"),
            Err(FileError::NotFound(_))
        ));
    }

    #[test]
    fn windows_style_paths_work() {
        let dir = temp();
        std::fs::create_dir_all(dir.path().join("a")).unwrap();
        std::fs::write(dir.path().join("a/b.txt"), "hi\n").unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();

        // Backslashes count as separator, the way a Windows client writes a path.
        let (_, rel) = resolve_in_root(&root, "a\\b.txt").unwrap();
        assert_eq!(rel, "a/b.txt");

        // A drive path (`C:/x`) is absolute, not taken from the root.
        let drive = root.display().to_string().replace('\\', "/");
        let (_, rel) = resolve_in_root(&root, &format!("{drive}/a/b.txt")).unwrap();
        assert_eq!(rel, "a/b.txt");
        assert!(resolve_in_root(&root, "C:\\definitely\\not\\here").is_err());
    }

    #[test]
    fn escapes_are_refused() {
        let root = temp();
        let outside = temp();
        std::fs::write(outside.path().join("secret.txt"), "secret\n").unwrap();
        let root_path = dunce::canonicalize(root.path()).unwrap();
        let name = outside
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();

        // `..` out of the root, and an absolute path outside it.
        assert!(matches!(
            resolve_in_root(&root_path, &format!("../{name}/secret.txt")),
            Err(FileError::Outside(_))
        ));
        let abs = dunce::canonicalize(outside.path())
            .unwrap()
            .join("secret.txt");
        assert!(matches!(
            resolve_in_root(&root_path, &abs.display().to_string()),
            Err(FileError::Outside(_))
        ));
    }

    #[test]
    fn a_symlink_out_of_the_root_is_refused() {
        let outside = temp();
        std::fs::write(outside.path().join("secret.txt"), "secret\n").unwrap();
        let root = temp();
        let link = root.path().join("link.txt");
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(outside.path().join("secret.txt"), &link).is_ok();
        #[cfg(windows)]
        let made =
            std::os::windows::fs::symlink_file(outside.path().join("secret.txt"), &link).is_ok();
        #[cfg(not(any(unix, windows)))]
        let made = false;
        // Creating a symlink needs a privilege Windows may not grant: skip then.
        if !made {
            return;
        }
        let root = dunce::canonicalize(root.path()).unwrap();
        assert!(matches!(
            resolve_in_root(&root, "link.txt"),
            Err(FileError::Outside(_))
        ));
    }

    #[test]
    fn binary_files_have_no_text_and_large_ones_are_cut() {
        let text = Contents::new(6, b"hello\n", false);
        assert!(!text.binary && !text.truncated);
        assert_eq!(text.text.as_deref(), Some("hello\n"));

        // A NUL byte, or a byte that is not UTF-8, makes a file binary.
        assert!(Contents::new(3, b"a\0b", false).binary);
        assert!(Contents::new(2, &[0x61, 0xff], false).binary);
        assert!(Contents::new(3, b"a\0b", false).text.is_none());

        // A multi-byte character cut by the sniffed head is still text.
        let mut bytes = vec![b'a'; SNIFF - 1];
        bytes.extend_from_slice("é".as_bytes());
        assert!(!Contents::new(bytes.len() as u64, &bytes, false).binary);

        // A file past the limit is read as far as it goes and flagged.
        let dir = temp();
        let file = dir.path().join("big.txt");
        std::fs::write(&file, vec![b'x'; CONTENT_LIMIT + 10]).unwrap();
        let read = read_file(&file, "big.txt").unwrap();
        assert_eq!(read.size, (CONTENT_LIMIT + 10) as u64);
        assert!(read.truncated);
        assert_eq!(read.text.map(|text| text.len()), Some(CONTENT_LIMIT));

        // A directory is not a file to read.
        assert!(matches!(
            read_file(dir.path(), "dir"),
            Err(FileError::BadRequest(_))
        ));
    }

    #[tokio::test]
    async fn gitignored_files_are_hidden_unless_all() {
        let dir = temp();
        assert!(git(dir.path(), &["init", "-q"]).await.is_ok());
        std::fs::write(dir.path().join(".gitignore"), "secret.txt\n").unwrap();
        std::fs::write(dir.path().join("secret.txt"), "x\n").unwrap();
        std::fs::write(dir.path().join("shown.txt"), "x\n").unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();

        let listed = list_dir(dir.path(), "", false).await.unwrap();
        let names: Vec<&str> = listed.iter().map(|entry| entry.name.as_str()).collect();
        // Directories first, then by name; .git never, the ignored file not at all.
        assert_eq!(names, ["sub", ".gitignore", "shown.txt"]);

        let all = list_dir(dir.path(), "", true).await.unwrap();
        let secret = all.iter().find(|entry| entry.name == "secret.txt").unwrap();
        assert!(secret.ignored);
        assert_eq!(secret.path, "secret.txt");
        assert!(!all.iter().any(|entry| entry.name == ".git"));

        // Not even by a path: the repository's own files stay out of the API.
        assert!(matches!(
            resolve_in_root(dir.path(), ".git/HEAD"),
            Err(FileError::NotFound(_))
        ));

        // Outside a repository nothing is hidden.
        let plain = temp();
        std::fs::write(plain.path().join("secret.txt"), "x\n").unwrap();
        let listed = list_dir(plain.path(), "", false).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].ignored);
    }

    #[tokio::test]
    async fn files_read_from_a_ref() {
        let dir = temp();
        assert!(git(dir.path(), &["init", "-q", "-b", "main"]).await.is_ok());
        std::fs::write(dir.path().join("a.txt"), "first\n").unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b.txt"), "b\n").unwrap();
        commit(dir.path(), "one").await;
        let one = git_text(dir.path(), &["rev-parse", "HEAD"])
            .await
            .unwrap()
            .unwrap();
        std::fs::write(dir.path().join("a.txt"), "second\n").unwrap();
        commit(dir.path(), "two").await;
        let root = dunce::canonicalize(dir.path()).unwrap();

        // The ref holds what it held, not what the working copy has now.
        let first = read_file_at(&root, &one, "a.txt").await.unwrap();
        assert_eq!(first.text.as_deref(), Some("first\n"));
        assert_eq!(first.size, 6);
        let (abs, rel) = resolve_in_root(&root, "a.txt").unwrap();
        assert_eq!(
            read_file(&abs, &rel).unwrap().text.as_deref(),
            Some("second\n")
        );

        let entries = list_dir_at(&root, &one, "").await.unwrap();
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["sub", "a.txt"]);
        assert_eq!(entries[1].size, Some(6));

        // One directory of the tree.
        let sub = list_dir_at(&root, &one, "sub").await.unwrap();
        assert_eq!(sub[0].path, "sub/b.txt");
        assert_eq!(sub[0].kind, Kind::File);

        assert!(matches!(
            read_file_at(&root, &one, "nope.txt").await,
            Err(FileError::NotFound(_))
        ));
        assert!(matches!(
            read_file_at(&root, "no-such-ref", "a.txt").await,
            Err(FileError::NotFound(_))
        ));
        // A directory is no file, a file is no directory.
        assert!(matches!(
            read_file_at(&root, "HEAD", "sub").await,
            Err(FileError::BadRequest(_))
        ));
        assert!(matches!(
            list_dir_at(&root, "HEAD", "a.txt").await,
            Err(FileError::BadRequest(_))
        ));

        // Paths at a ref: no `..`, and an absolute path must be inside the root.
        assert_eq!(check_rel_for_ref("sub//b.txt").unwrap(), "sub/b.txt");
        assert!(matches!(
            check_rel_for_ref("../a.txt"),
            Err(FileError::BadRequest(_))
        ));
        assert!(matches!(
            check_rel_for_ref("/etc/passwd"),
            Err(FileError::Outside(_))
        ));
        let inside = root.join("sub/b.txt");
        assert_eq!(
            rel_at(&root, &inside.display().to_string()).unwrap(),
            "sub/b.txt"
        );
        let outside = temp();
        assert!(matches!(
            rel_at(&root, &outside.path().display().to_string()),
            Err(FileError::Outside(_))
        ));
    }

    #[tokio::test]
    async fn status_is_none_outside_a_repository() {
        let dir = temp();
        std::fs::write(dir.path().join("plain.txt"), "x\n").unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        // No repository, no error: the client simply shows no status.
        assert_eq!(changes(&root).await.unwrap(), None);
    }

    #[tokio::test]
    async fn status_lists_added_modified_and_deleted_paths() {
        let dir = temp();
        assert!(git(dir.path(), &["init", "-q"]).await.is_ok());
        std::fs::write(dir.path().join("kept.txt"), "one\n").unwrap();
        std::fs::write(dir.path().join("gone.txt"), "bye\n").unwrap();
        commit(dir.path(), "one").await;

        std::fs::write(dir.path().join("kept.txt"), "two\n").unwrap();
        std::fs::remove_file(dir.path().join("gone.txt")).unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/new.txt"), "new\n").unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();

        let found = changes(&root).await.unwrap().unwrap();
        let by_path: Vec<(&str, Change)> = found
            .iter()
            .map(|entry| (entry.path.as_str(), entry.change))
            .collect();
        assert_eq!(
            by_path,
            [
                ("gone.txt", Change::Deleted),
                ("kept.txt", Change::Modified),
                ("sub/new.txt", Change::Added),
            ]
        );
    }

    #[tokio::test]
    async fn status_is_relative_to_the_workspace() {
        let dir = temp();
        assert!(git(dir.path(), &["init", "-q"]).await.is_ok());
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/a.txt"), "one\n").unwrap();
        std::fs::write(dir.path().join("outside.txt"), "one\n").unwrap();
        commit(dir.path(), "one").await;
        std::fs::write(dir.path().join("sub/a.txt"), "two\n").unwrap();
        std::fs::write(dir.path().join("outside.txt"), "two\n").unwrap();
        let workspace = dunce::canonicalize(dir.path().join("sub")).unwrap();

        let found = changes(&workspace).await.unwrap().unwrap();
        let by_path: Vec<(&str, Change)> = found
            .iter()
            .map(|entry| (entry.path.as_str(), entry.change))
            .collect();
        // The rest of the repository is not part of this workspace.
        assert_eq!(by_path, [("a.txt", Change::Modified)]);
    }

    #[tokio::test]
    async fn search_finds_files_across_the_workspace() {
        let dir = temp();
        assert!(git(dir.path(), &["init", "-q"]).await.is_ok());
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::create_dir_all(dir.path().join("crates/cli/src")).unwrap();
        std::fs::write(dir.path().join("docs/one.md"), "# one\n").unwrap();
        std::fs::write(dir.path().join("crates/cli/src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "/target\n").unwrap();
        commit(dir.path(), "one").await;

        // Untracked files are found too; ignored ones only with `all`.
        std::fs::write(dir.path().join("docs/two.md"), "# two\n").unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/blob.md"), "no\n").unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();

        let md = search(&root, ".md", false).await.unwrap();
        assert_eq!(md.paths, ["docs/one.md", "docs/two.md"]);
        assert!(!md.truncated);

        let all = search(&root, "blob", true).await.unwrap();
        assert_eq!(all.paths, ["target/blob.md"]);

        // Matching is by path, case-insensitively, across folders that were never opened.
        let cli = search(&root, "CLI/SRC", false).await.unwrap();
        assert_eq!(cli.paths, ["crates/cli/src/main.rs"]);

        // An empty query is no request, not a listing of the whole workspace.
        assert!(search(&root, "  ", false).await.unwrap().paths.is_empty());
    }

    #[tokio::test]
    async fn search_does_not_leave_the_root() {
        let outside = temp();
        std::fs::write(outside.path().join("secret.md"), "secret\n").unwrap();
        let root = temp();
        std::fs::write(root.path().join("inside.md"), "ok\n").unwrap();
        let link = root.path().join("linked");
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(outside.path(), &link).is_ok();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(outside.path(), &link).is_ok();
        #[cfg(not(any(unix, windows)))]
        let made = false;
        if !made {
            return;
        }
        let root = dunce::canonicalize(root.path()).unwrap();
        // The symlink is not followed, so nothing outside is listed.
        let found = search(&root, "secret", false).await.unwrap();
        assert!(found.paths.is_empty());
        // Reading it by path is still refused.
        assert!(matches!(
            resolve_in_root(&root, "linked/secret.md"),
            Err(FileError::Outside(_))
        ));
    }
}
