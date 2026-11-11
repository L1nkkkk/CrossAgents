//! Troubleshooting records (docs/tasks/LESSONS.md): problems recorded during a run as run
//! notes under `issue/`, sorted afterwards into lessons of the run's project, which any agent
//! of that project searches when it runs into trouble. Reference, never rules: nothing here
//! goes into an agent's instructions beyond one line saying the records exist.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use crossagents_core::{
    Agent, AgentId, CoreError, EventKind, Evidence, ISSUE_PREFIX, IssueRef, Lesson, LessonOutcome,
    LessonStatus, NoteScope, Project, ProjectPath, RelatedLesson, Run, RunId, Store, Task, Triage,
    TriageStep,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::Orchestrator;
use crate::projects::{LOCAL_NODE, main_repo, normalize_remote, resolve};
use crate::tools::args;

/// Longest title, and longest of the other text fields, in bytes.
const TITLE_MAX: usize = 200;
const FIELD_MAX: usize = 4 * 1024;
const KEYWORDS_MAX: usize = 20;
const SEARCH_LIMIT: usize = 10;
/// Evidence `lesson_get` shows unless asked for a page, and the most it shows at once.
const EVIDENCE_SHOWN: usize = 5;
const EVIDENCE_PAGE_MAX: usize = 50;
/// Sources `lesson_get` names; the rest are counted.
const SOURCES_SHOWN: usize = 3;
/// Longest excerpt of a field in a search result, in bytes.
const EXCERPT_MAX: usize = 300;
/// Most related lessons attached to a failure, and most words taken from its output.
pub const RELATED_MAX: usize = 3;
const STRONG_MAX: usize = 8;
const WEAK_MAX: usize = 8;
/// Longest a failure waits for its related lessons; past it, it goes without them.
const RELATED_TIMEOUT: Duration = Duration::from_secs(2);
/// What `related_lessons` means, said wherever it is attached (LESSONS §4.5 ②).
pub const RELATED_NOTE: &str = "related_lessons are possibly related only: words of this error were found in them, which does not confirm their resolution applies here. Read one with lesson_get and check its conditions before going by it.";

/// Words of an `error:` line too common to tell one problem from another.
const WEAK_STOP: &[&str] = &[
    "the", "and", "for", "not", "with", "from", "this", "that", "was", "were", "are", "has",
    "have", "had", "but", "can", "cannot", "could", "error", "errors", "failed", "failure",
    "fatal", "warning", "found", "into", "when", "while", "file", "line", "column", "exit",
    "exited", "code", "status", "process", "command", "returned", "note", "help",
];

fn ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether `word` is an error code: `E0277`, `TS2345`, `MSB3073`, `LNK2019`, `0x80070005`,
/// or an errno name such as `ENOENT`, `EADDRINUSE`.
fn is_error_code(word: &str) -> bool {
    let letters = word.chars().take_while(|c| c.is_ascii_uppercase()).count();
    let rest = &word[letters..];
    if (1..=5).contains(&letters)
        && (3..=6).contains(&rest.len())
        && rest.chars().all(|c| c.is_ascii_digit())
    {
        return true;
    }
    if let Some(hex) = word.strip_prefix("0x").or_else(|| word.strip_prefix("0X")) {
        return hex.len() >= 4 && hex.chars().all(|c| c.is_ascii_hexdigit());
    }
    ERRNO.contains(&word)
}

/// Errno names tools print (Node, libc, npm); a list, since `ERROR` or `EXPECTED` look alike.
const ERRNO: &[&str] = &[
    "ENOENT",
    "EACCES",
    "EPERM",
    "EEXIST",
    "EADDRINUSE",
    "EADDRNOTAVAIL",
    "ECONNREFUSED",
    "ECONNRESET",
    "ECONNABORTED",
    "ETIMEDOUT",
    "ENOTFOUND",
    "EPIPE",
    "EMFILE",
    "ENFILE",
    "ENOSPC",
    "EBUSY",
    "ENOTDIR",
    "EISDIR",
    "ENOTEMPTY",
    "EAI_AGAIN",
    "EINVAL",
    "EAGAIN",
    "ELIFECYCLE",
    "ERESOLVE",
    "EINTEGRITY",
    "ENOTSUP",
    "EROFS",
    "EXDEV",
    "ELOOP",
    "ENAMETOOLONG",
    "EHOSTUNREACH",
    "ENETUNREACH",
    "ECANCELED",
    "ENOMEM",
    "ESRCH",
    "ECHILD",
];

/// Whether `word` names an exception or error type: `ModuleNotFoundError`,
/// `NullPointerException`, `IOError` (not the bare word).
fn is_exception(word: &str) -> bool {
    ["Error", "Exception"].iter().any(|suffix| {
        word.len() > suffix.len()
            && word.ends_with(suffix)
            && word.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn push_unique(terms: &mut Vec<String>, word: &str, max: usize) {
    if terms.len() < max && !terms.iter().any(|t| t.eq_ignore_ascii_case(word)) {
        terms.push(word.to_owned());
    }
}

/// The words of a failure's output worth looking up (LESSONS §4.5 ②): strong ones, error
/// codes and exception names, which tell one problem from another; and weak ones, the words
/// of the lines after `error:`. Exit codes count as strong when the output says so
/// (`exit code 9009`) and they have at least three digits.
pub fn error_terms(output: &str) -> (Vec<String>, Vec<String>) {
    let mut strong: Vec<String> = Vec::new();
    let mut weak: Vec<String> = Vec::new();
    for line in output.lines() {
        let words: Vec<&str> = line
            .split(|c: char| !ident_char(c))
            .filter(|w| !w.is_empty())
            .collect();
        for (i, word) in words.iter().enumerate() {
            if is_error_code(word)
                || is_exception(word)
                || (word.len() >= 3
                    && word.chars().all(|c| c.is_ascii_digit())
                    && !word.chars().all(|c| c == '0')
                    && i > 0
                    && matches!(
                        words[i - 1].to_ascii_lowercase().as_str(),
                        "code" | "status" | "exitcode" | "errno"
                    ))
            {
                push_unique(&mut strong, word, STRONG_MAX);
            }
        }
        let lower = line.to_ascii_lowercase();
        let Some(at) = lower.find("error") else {
            continue;
        };
        let Some(colon) = line[at..].find(':') else {
            continue;
        };
        // `error:` or `error[E0277]:`, not a colon far along the line.
        if colon > 16 {
            continue;
        }
        for word in line[at + colon + 1..].split(|c: char| !ident_char(c) && c != '.' && c != '-') {
            let word = word.trim_matches(|c: char| c == '.' || c == '-');
            if word.chars().count() < 3
                || word.chars().all(|c| c.is_ascii_digit())
                || WEAK_STOP.contains(&word.to_ascii_lowercase().as_str())
                || strong.iter().any(|s| s.eq_ignore_ascii_case(word))
            {
                continue;
            }
            push_unique(&mut weak, word, WEAK_MAX);
        }
    }
    (strong, weak)
}

/// Whether `needle` stands as a word of its own in `hay` (both lower case).
fn has_word(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(i) = hay[from..].find(needle) {
        let start = from + i;
        let end = start + needle.len();
        let before = hay[..start].chars().next_back();
        let after = hay[end..].chars().next();
        if !before.is_some_and(ident_char) && !after.is_some_and(ident_char) {
            return true;
        }
        from = start + hay[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// The lessons that hold at least one strong word of a failure, the most strong words first
/// (then the most weak ones, then the order given), at most [`RELATED_MAX`]; none when no
/// lesson holds a strong word. Superseded lessons are left out. How well a lesson ranks in a
/// full-text search is not used: it says nothing about whether the lesson applies.
pub fn match_lessons(lessons: &[Lesson], strong: &[String], weak: &[String]) -> Vec<RelatedLesson> {
    let mut found: Vec<(usize, usize, usize, RelatedLesson)> = Vec::new();
    for (order, l) in lessons.iter().enumerate() {
        if l.status == LessonStatus::Superseded || found.iter().any(|f| f.3.id == l.id) {
            continue;
        }
        let hay = format!(
            "{}\n{}\n{}\n{}\n{}",
            l.title,
            l.symptom,
            l.keywords.join("\n"),
            l.conditions,
            l.resolution
        )
        .to_lowercase();
        let strong_hits: Vec<&String> = strong
            .iter()
            .filter(|w| has_word(&hay, &w.to_lowercase()))
            .collect();
        if strong_hits.is_empty() {
            continue;
        }
        let weak_hits: Vec<&String> = weak
            .iter()
            .filter(|w| has_word(&hay, &w.to_lowercase()))
            .collect();
        let matched = strong_hits
            .iter()
            .chain(weak_hits.iter())
            .map(|w| (*w).clone())
            .collect();
        found.push((
            strong_hits.len(),
            weak_hits.len(),
            order,
            RelatedLesson {
                id: l.id.clone(),
                title: l.title.clone(),
                matched,
            },
        ));
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    found.into_iter().take(RELATED_MAX).map(|f| f.3).collect()
}

/// Waits at most `limit` for a lookup of related lessons. A lookup that fails or is too slow
/// gives none and a warning: the failure it is for is reported as it would be without it.
pub async fn bounded_related(
    lookup: impl Future<Output = Result<Vec<RelatedLesson>, CoreError>>,
    limit: Duration,
) -> Vec<RelatedLesson> {
    match tokio::time::timeout(limit, lookup).await {
        Ok(Ok(found)) => found,
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "cannot look up related lessons; the failure goes without them");
            Vec::new()
        }
        Err(_) => {
            tracing::warn!(
                limit_ms = limit.as_millis() as u64,
                "looking up related lessons took too long; the failure goes without them"
            );
            Vec::new()
        }
    }
}

/// A paragraph naming related lessons, to add to a failure's text; nothing when none.
pub fn related_text(related: &[RelatedLesson]) -> Option<String> {
    if related.is_empty() {
        return None;
    }
    let list = serde_json::to_string(related).unwrap_or_default();
    Some(format!("related_lessons: {list}\n({RELATED_NOTE})"))
}

pub(crate) fn outcome_schema() -> Value {
    json!({ "type": "string", "enum": ["resolved", "workaround", "unresolved"] })
}

fn checked(field: &str, text: &str, max: usize) -> Result<String, CoreError> {
    let text = text.trim();
    if text.len() > max {
        return Err(CoreError::Conflict(format!(
            "{field} holds at most {max} bytes"
        )));
    }
    Ok(text.to_owned())
}

/// The issue keys of a call, with the prefix added when left out.
fn issue_keys(keys: Vec<String>) -> Result<Vec<String>, CoreError> {
    if keys.is_empty() {
        return Err(CoreError::Conflict("name at least one issue".into()));
    }
    Ok(keys
        .into_iter()
        .map(|k| {
            let k = k.trim().to_owned();
            if k.starts_with(ISSUE_PREFIX) {
                k
            } else {
                format!("{ISSUE_PREFIX}{k}")
            }
        })
        .collect())
}

/// Words worth searching for in a query: split on whitespace and on punctuation that is not
/// part of an identifier, error code or path.
pub fn search_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for word in query.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                ',' | ';'
                    | '"'
                    | '\''
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '，'
                    | '。'
                    | '：'
                    | '；'
            )
    }) {
        let word = word.trim_matches(|c: char| matches!(c, ':' | '.' | '!' | '?' | '`'));
        if !word.is_empty() && !terms.iter().any(|t| t == word) {
            terms.push(word.to_owned());
        }
    }
    terms.truncate(12);
    terms
}

/// The start of `text` for a search result: its first paragraph, at most [`EXCERPT_MAX`]
/// bytes, and whether anything was left out.
fn excerpt(text: &str) -> (String, bool) {
    let text = text.trim();
    let mut cut = text.find("\n\n").unwrap_or(text.len()).min(EXCERPT_MAX);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let short = text[..cut].trim_end();
    if short.len() == text.len() {
        (text.to_owned(), false)
    } else {
        (format!("{short}…"), true)
    }
}

// ---------------------------------------------------------------------------------------
// Review clues: a record's files changed after its last check (LESSONS §4.4)

/// Most files a lesson keeps as its own.
const FILES_MAX: usize = 20;
/// Most different commits one listing asks git about (one call each).
const COMMITS_MAX: usize = 16;

/// `path` as compared across git's output and a record: `/` separators, no leading `./`,
/// lower case (the checkouts the hub knows are case-insensitive on Windows, and a clue that
/// is wrong costs a second look, not an error).
pub fn path_key(path: &str) -> String {
    let mut key = path.trim().replace('\\', "/");
    while let Some(rest) = key.strip_prefix("./") {
        key = rest.to_owned();
    }
    key.to_lowercase()
}

/// The record's files that `changed` (paths as git reports them) names, in the record's
/// order.
pub fn changed_among(files: &[String], changed: &[String]) -> Vec<String> {
    let changed: std::collections::HashSet<String> = changed.iter().map(|c| path_key(c)).collect();
    files
        .iter()
        .filter(|f| changed.contains(&path_key(f)))
        .cloned()
        .collect()
}

/// The commit a record was last checked at: the newest evidence that names one. That is the
/// point its files are compared with, so a later check at a newer commit makes the clue
/// quieter rather than noisier.
pub fn last_commit(l: &Lesson) -> Option<&str> {
    l.evidence
        .iter()
        .rev()
        .find_map(|e| e.commit.as_deref().filter(|c| !c.trim().is_empty()))
}

/// Files changed in `dir`'s repository since `commit`: committed or not, since a worker's
/// checkout with uncommitted work still tells a later reader the files moved on. Empty when
/// git cannot say (not a repository, unknown commit, git missing): a clue that cannot be read
/// is left out rather than guessed.
pub async fn changed_since(dir: &Path, commit: &str) -> Vec<String> {
    use crossagents_core::process::NoWindow;
    let root = main_repo(dir).unwrap_or_else(|| dir.to_path_buf());
    let output = tokio::process::Command::new("git")
        .no_window()
        .args(["-c", "core.quotePath=false", "diff", "--name-only", commit])
        .current_dir(&root)
        .output()
        .await;
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// For each record that names files and a commit: those changed since that commit, by
/// lesson id. Records that name neither, or whose files all still look the same, are left
/// out. At most [`COMMITS_MAX`] different commits are asked about.
pub async fn files_changed(lessons: &[Lesson], dir: &Path) -> BTreeMap<String, Vec<String>> {
    let mut by_commit: HashMap<String, Vec<String>> = HashMap::new();
    let mut out = BTreeMap::new();
    for l in lessons {
        if l.files.is_empty() {
            continue;
        }
        let Some(commit) = last_commit(l) else {
            continue;
        };
        if !by_commit.contains_key(commit) {
            if by_commit.len() >= COMMITS_MAX {
                continue;
            }
            let changed = changed_since(dir, commit).await;
            by_commit.insert(commit.to_owned(), changed);
        }
        let hit = changed_among(&l.files, &by_commit[commit]);
        if !hit.is_empty() {
            out.insert(l.id.clone(), hit);
        }
    }
    out
}

/// One search hit: enough to tell whether it is worth reading in full (USAGE-TRIM §4.2).
fn search_hit(l: &Lesson) -> Value {
    let (conditions, a) = excerpt(&l.conditions);
    let (symptom, b) = excerpt(&l.symptom);
    let (resolution, c) = excerpt(&l.resolution);
    let mut hit = json!({
        "id": l.id,
        "title": l.title,
        "status": l.status,
        "outcome": l.outcome,
        "conditions": conditions,
        "symptom": symptom,
        "resolution": resolution,
        "last_checked": l.evidence.last().map(|e| e.at),
    });
    if a || b || c {
        hit["truncated"] = json!(true);
    }
    hit
}

/// A lesson as `lesson_get` gives it (USAGE-TRIM §4.1): every field of its own, but of its
/// evidence only counts, a page (newest first, `offset` from the newest) and the latest
/// failure however old, and of its sources a count and the latest few.
fn lesson_view(l: &Lesson, offset: usize, limit: usize) -> Value {
    let count = |o: LessonOutcome| l.evidence.iter().filter(|e| e.outcome == o).count();
    let page: Vec<&Evidence> = l.evidence.iter().rev().skip(offset).take(limit).collect();
    let rest = l.evidence.len().saturating_sub(offset + page.len());
    let last_failure = l
        .evidence
        .iter()
        .rev()
        .find(|e| e.outcome == LessonOutcome::Unresolved);
    let mut view = json!({
        "lesson": {
            "id": l.id,
            "title": l.title,
            "status": l.status,
            "superseded_by": l.superseded_by,
            "outcome": l.outcome,
            "symptom": l.symptom,
            "keywords": l.keywords,
            "conditions": l.conditions,
            "resolution": l.resolution,
            "created_at": l.created_at,
            "updated_at": l.updated_at,
        },
        "evidence": {
            "total": l.evidence.len(),
            "resolved": count(LessonOutcome::Resolved),
            "workaround": count(LessonOutcome::Workaround),
            "unresolved": count(LessonOutcome::Unresolved),
            "first_at": l.evidence.first().map(|e| e.at),
            "last_at": l.evidence.last().map(|e| e.at),
            "offset": offset,
            "newest_first": page,
        },
        "last_failure": last_failure,
        "sources": {
            "total": l.sources.len(),
            "latest": l.sources.iter().rev().take(SOURCES_SHOWN).collect::<Vec<_>>(),
        },
    });
    if rest > 0 {
        view["evidence"]["more"] = json!(format!(
            "{rest} older; lesson_get with evidence_offset {} for the next page",
            offset + page.len()
        ));
    }
    match l.status {
        LessonStatus::Review => {
            view["review"] = json!(
                "Someone reported this no longer works: see last_failure before relying on it."
            )
        }
        LessonStatus::Superseded => {
            view["review"] = json!("Replaced by superseded_by; kept for its history.")
        }
        LessonStatus::Active => {}
    }
    view
}

/// A project's lessons as Markdown, for a person to read or keep (LESSONS L2): one section
/// per lesson, newest first as given, superseded ones marked. Empty for no lessons.
pub fn export_markdown(project: &str, lessons: &[Lesson]) -> String {
    if lessons.is_empty() {
        return String::new();
    }
    let mut out = format!("# Troubleshooting records: {project}\n\n");
    out.push_str("Reference, not rules: check a record's conditions before going by it.\n");
    for l in lessons {
        let status = match l.status {
            LessonStatus::Active => "active",
            LessonStatus::Review => "review",
            LessonStatus::Superseded => "superseded",
        };
        let outcome = match l.outcome {
            LessonOutcome::Resolved => "resolved",
            LessonOutcome::Workaround => "workaround",
            LessonOutcome::Unresolved => "unresolved",
        };
        out.push_str(&format!("\n## {}\n\n", l.title.trim()));
        out.push_str(&format!(
            "- id: `{}`\n- status: {status}\n- outcome: {outcome}\n",
            l.id
        ));
        if let Some(by) = &l.superseded_by {
            out.push_str(&format!("- superseded by: `{by}`\n"));
        }
        if !l.keywords.is_empty() {
            out.push_str(&format!("- keywords: {}\n", l.keywords.join(", ")));
        }
        out.push_str(&format!(
            "- tried: {} time(s), last {}\n- updated: {}\n",
            l.evidence.len(),
            l.evidence
                .last()
                .map(|e| e.at.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "never".into()),
            l.updated_at.format("%Y-%m-%d"),
        ));
        for (name, text) in [
            ("Symptom", &l.symptom),
            ("Conditions", &l.conditions),
            ("Resolution", &l.resolution),
        ] {
            if !text.trim().is_empty() {
                out.push_str(&format!("\n### {name}\n\n{}\n", text.trim()));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// Projects: binding a remote or a checkout, merging two (LESSONS §4.2)

fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

fn project_of(projects: &[Project], id: &str) -> Result<Project, CoreError> {
    projects
        .iter()
        .find(|p| p.id == id)
        .cloned()
        .ok_or_else(|| CoreError::NotFound {
            kind: "project",
            id: id.to_owned(),
        })
}

/// Binds what a person confirms to a project (LESSONS §4.2): a `remote` becomes an alias of
/// it — a rename or a migrated repository, never a fork of someone else's — and a `path`
/// moves to it from whatever project held that checkout. Either says the person has settled
/// the project's identity, so its `unconfirmed_remote` is cleared. Nothing else moves: a
/// remote another project already owns is refused, and merging is a separate call.
///
/// With `split`, the path is not bound to `id` but taken out of it into a project of its own
/// — the other half of the same decision, when the directory turns out to hold another
/// repository (a fork the person does not want mixed with this one). The new project takes
/// the remote that was waiting to be confirmed.
pub async fn bind_project(
    store: &dyn Store,
    id: &str,
    remote: Option<&str>,
    path: Option<&str>,
    split: bool,
) -> Result<Project, CoreError> {
    let projects = store.list_projects().await?;
    let mut project = project_of(&projects, id)?;
    if split {
        let given = path
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| {
                CoreError::Conflict("name the checkout that is another project".into())
            })?;
        let at = dunce::canonicalize(given)
            .map_err(|e| CoreError::Conflict(format!("cannot read {given}: {e}")))?;
        let mut fresh = Project {
            id: uuid::Uuid::new_v4().to_string(),
            name: at
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| at.display().to_string()),
            remotes: Vec::new(),
            paths: vec![ProjectPath {
                node: LOCAL_NODE.into(),
                path: at.clone(),
            }],
            unconfirmed_remote: None,
            created_at: Utc::now(),
        };
        if let Some(url) = remote.map(str::trim).filter(|u| !u.is_empty()) {
            let remote = normalize_remote(url).ok_or_else(|| {
                CoreError::Conflict(format!("{url} is not a remote the hub can recognise"))
            })?;
            fresh.remotes.push(remote);
        } else if project
            .paths
            .iter()
            .any(|bound| same_path(&bound.path, &at))
        {
            // The remote the hub could not place is this checkout's, which is now its own.
            fresh.remotes.extend(project.unconfirmed_remote.take());
        }
        // The checkout leaves whichever project held it, and becomes one of its own.
        for other in projects.iter().filter(|p| p.id != project.id) {
            if other.paths.iter().any(|bound| same_path(&bound.path, &at)) {
                let mut other = other.clone();
                other.paths.retain(|bound| !same_path(&bound.path, &at));
                store.put_project(&other).await?;
            }
        }
        project.paths.retain(|bound| !same_path(&bound.path, &at));
        store.put_project(&project).await?;
        store.put_project(&fresh).await?;
        return Ok(fresh);
    }
    if let Some(url) = remote.map(str::trim).filter(|u| !u.is_empty()) {
        let remote = normalize_remote(url).ok_or_else(|| {
            CoreError::Conflict(format!(
                "{url} is not a remote the hub can recognise; give one like git@host:owner/repo or https://host/owner/repo"
            ))
        })?;
        if let Some(other) = projects
            .iter()
            .find(|p| p.id != project.id && p.remotes.contains(&remote))
        {
            return Err(CoreError::Conflict(format!(
                "{remote} already belongs to project {} ({}); merge the two projects instead",
                other.id, other.name
            )));
        }
        if !project.remotes.contains(&remote) {
            project.remotes.push(remote.clone());
        }
        if project.unconfirmed_remote.as_deref() == Some(remote.as_str()) {
            project.unconfirmed_remote = None;
        }
    }
    if let Some(given) = path.map(str::trim).filter(|p| !p.is_empty()) {
        let at = dunce::canonicalize(given)
            .map_err(|e| CoreError::Conflict(format!("cannot read {given}: {e}")))?;
        // A checkout is bound to one project: take it from wherever else it was.
        for other in projects.iter().filter(|p| p.id != project.id) {
            if other.paths.iter().any(|bound| same_path(&bound.path, &at)) {
                let mut other = other.clone();
                other.paths.retain(|bound| !same_path(&bound.path, &at));
                store.put_project(&other).await?;
            }
        }
        if !project
            .paths
            .iter()
            .any(|bound| same_path(&bound.path, &at))
        {
            project.paths.push(ProjectPath {
                node: LOCAL_NODE.into(),
                path: at,
            });
        }
        project.unconfirmed_remote = None;
    }
    store.put_project(&project).await?;
    Ok(project)
}

/// Merges project `from` into `into` (LESSONS §4.2): every lesson of `from` — its evidence
/// and sources go with it — becomes `into`'s, `from`'s checkouts and remotes join `into`'s,
/// and `from` is left with none, so no later run resolves to it. Repeating a merge that was
/// interrupted writes the same result: the lessons have moved and the path lists are sets.
pub async fn merge_projects(
    store: &dyn Store,
    from: &str,
    into: &str,
) -> Result<Project, CoreError> {
    if from == into {
        return Err(CoreError::Conflict(
            "a project cannot be merged into itself".into(),
        ));
    }
    let projects = store.list_projects().await?;
    let mut source = project_of(&projects, from)?;
    let mut target = project_of(&projects, into)?;
    for mut lesson in store.list_lessons(from).await? {
        lesson.project_id = target.id.clone();
        store.update_lesson(&lesson).await?;
    }
    for at in &source.paths {
        if !target
            .paths
            .iter()
            .any(|bound| same_path(&bound.path, &at.path))
        {
            target.paths.push(at.clone());
        }
    }
    for remote in &source.remotes {
        if !target.remotes.contains(remote) {
            target.remotes.push(remote.clone());
        }
    }
    if target.unconfirmed_remote.is_none() {
        target.unconfirmed_remote = source.unconfirmed_remote.take();
    }
    store.put_project(&target).await?;
    // The source keeps its id (its lessons and runs still point at nothing else) but no
    // checkout and no remote: nothing resolves to it again.
    source.paths.clear();
    source.remotes.clear();
    store.put_project(&source).await?;
    Ok(target)
}

impl Orchestrator {
    /// Lessons of `dir`'s project that may bear on a failure whose output is `output`
    /// (LESSONS §4.5 ②): see [`match_lessons`]. Empty when the project is unknown, the
    /// output holds no strong word, or the lookup fails or takes longer than
    /// [`RELATED_TIMEOUT`] (then with a warning): this never fails or holds up the failure.
    pub async fn related_lessons(&self, dir: &Path, output: &str) -> Vec<RelatedLesson> {
        bounded_related(self.find_related(dir, output), RELATED_TIMEOUT).await
    }

    async fn find_related(
        &self,
        dir: &Path,
        output: &str,
    ) -> Result<Vec<RelatedLesson>, CoreError> {
        let (strong, weak) = error_terms(output);
        if strong.is_empty() {
            return Ok(Vec::new());
        }
        let Some(project) = self.known_project(dir).await? else {
            return Ok(Vec::new());
        };
        // Superseded records are left out of the search, and again by `match_lessons`.
        let found = self
            .store
            .search_lessons(&project.id, &strong, false, SEARCH_LIMIT * 2)
            .await?;
        Ok(match_lessons(&found, &strong, &weak))
    }

    /// Puts the lessons related to a failure on the task's report, next to its `failure`
    /// (replacing earlier ones), and gives the lines to add to the message that reports it:
    /// empty when there are none. A hint for whoever reads it; nothing is run or reported.
    pub(crate) async fn attach_related(&self, dir: &Path, task: &mut Task, output: &str) -> String {
        task.report.related_lessons = self.related_lessons(dir, output).await;
        related_text(&task.report.related_lessons)
            .map(|t| format!("\n{t}"))
            .unwrap_or_default()
    }

    /// Says a problem was recorded: a run note under `issue/` was written (LESSONS §4.1).
    pub(crate) async fn issue_recorded(
        &self,
        scope: NoteScope,
        run_id: Option<RunId>,
        key: &str,
        author: AgentId,
    ) {
        if let (NoteScope::Run, Some(run_id), true) = (scope, run_id, key.starts_with(ISSUE_PREFIX))
        {
            self.publish(EventKind::IssueRecorded {
                run_id,
                key: key.to_owned(),
                author,
            })
            .await;
        }
    }

    /// The project of `dir` if the hub knows it already; nothing is made or bound.
    async fn known_project(&self, dir: &Path) -> Result<Option<Project>, CoreError> {
        let root = main_repo(dir).unwrap_or_else(|| dir.to_path_buf());
        let projects = self.store.list_projects().await?;
        let (project, _) = resolve(&projects, &root, None);
        Ok(projects.into_iter().find(|p| p.id == project.id))
    }

    /// The one line an agent's instructions get about its project's records, when there are
    /// any (LESSONS §4.5): how many, and how to search them. Never their text.
    pub async fn lessons_hint(&self, agent: &Agent) -> Option<String> {
        let project = self.known_project(&agent.cwd).await.ok()??;
        let lessons = self.store.list_lessons(&project.id).await.ok()?;
        let usable = lessons
            .iter()
            .filter(|l| l.status != LessonStatus::Superseded)
            .count();
        (usable > 0).then(|| {
            format!(
                "# Troubleshooting records\n\nThis project has {usable} troubleshooting record(s) from earlier runs: problems agents hit here and what they did. When you hit an error, `lesson_search` it before digging in; a match is a lead to check, not a rule. If you use one, say how it went with `lesson_report`."
            )
        })
    }

    /// How many problems recorded in `run` nobody has sorted yet.
    pub async fn unsorted_issues(&self, run: RunId) -> usize {
        let Ok(notes) = self
            .store
            .list_notes(NoteScope::Run, Some(run), ISSUE_PREFIX)
            .await
        else {
            return 0;
        };
        let sorted = self.store.list_triage(run).await.unwrap_or_default();
        notes
            .iter()
            .filter(|n| !sorted.iter().any(|t| t.issue.key == n.key))
            .count()
    }

    /// The run a call names (an id or the start or end of one), else the caller's own, and
    /// only one of the caller's project (LESSONS §4.3).
    async fn issue_run(&self, caller: &Agent, run: Option<String>) -> Result<Run, CoreError> {
        let run = match run.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
            None => match self.run_of(caller.id).await? {
                Some((run, _)) => run,
                None => {
                    return Err(CoreError::Conflict(
                        "you are in no run; name the run whose issues you sort".into(),
                    ));
                }
            },
            Some(key) => self.run_by_key(key).await?,
        };
        let theirs = self.project_for(&run.repo).await?;
        let mine = self.project_for(&caller.cwd).await?;
        if theirs.id != mine.id {
            return Err(CoreError::Conflict(
                "that run belongs to another project".into(),
            ));
        }
        Ok(run)
    }

    async fn run_by_key(&self, key: &str) -> Result<Run, CoreError> {
        if let Ok(id) = key.parse::<RunId>() {
            return self.store.get_run(id).await;
        }
        let key = key.to_lowercase().replace('-', "");
        if key.len() < 4 {
            return Err(CoreError::Conflict(
                "give at least 4 characters of the run id".into(),
            ));
        }
        let mut found = self.store.list_runs().await?.into_iter().filter(|r| {
            let hex = r.id.to_string().replace('-', "");
            hex.starts_with(&key) || hex.ends_with(&key)
        });
        match (found.next(), found.next()) {
            (Some(run), None) => Ok(run),
            (None, _) => Err(CoreError::NotFound {
                kind: "run",
                id: key,
            }),
            _ => Err(CoreError::Conflict(format!(
                "{key} matches more than one run"
            ))),
        }
    }

    /// A run's recorded problems not sorted yet. Read straight from the store by run, so a
    /// run that ended (or failed, or was cancelled) can still be sorted by anyone of its
    /// project; read only.
    pub(crate) async fn issue_pending(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            #[serde(default)]
            run: Option<String>,
        }
        let a: A = args(input)?;
        let run = self.issue_run(caller, a.run).await?;
        let sorted = self.store.list_triage(run.id).await?;
        let notes = self
            .store
            .list_notes(NoteScope::Run, Some(run.id), ISSUE_PREFIX)
            .await?;
        let pending: Vec<Value> = notes
            .into_iter()
            .filter(|n| !sorted.iter().any(|t| t.issue.key == n.key))
            .map(|n| json!({ "key": n.key, "author": n.author, "updated_at": n.updated_at, "text": n.text }))
            .collect();
        Ok(json!({
            "run": run.id,
            "status": run.status,
            "pending": pending,
            "sorted": sorted.len(),
        }))
    }

    /// Sorts issues of a run: into a new lesson, into an existing one, or kept in the run.
    pub(crate) async fn issue_sort(
        &self,
        caller: &Agent,
        how: &str,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            #[serde(default)]
            run: Option<String>,
            issues: Vec<String>,
            // lesson_create
            #[serde(default)]
            title: String,
            #[serde(default)]
            symptom: String,
            #[serde(default)]
            keywords: Vec<String>,
            #[serde(default)]
            conditions: String,
            /// Files the record is about: the clue `lesson_search` checks against later
            /// changes (LESSONS §4.4).
            #[serde(default)]
            files: Vec<String>,
            #[serde(default)]
            resolution: Option<String>,
            #[serde(default)]
            outcome: Option<LessonOutcome>,
            // lesson_merge
            #[serde(default)]
            lesson: Option<String>,
            #[serde(default)]
            note: String,
            // issue_keep
            #[serde(default)]
            why: String,
        }
        let a: A = args(input)?;
        let run = self.issue_run(caller, a.run).await?;
        let keys = issue_keys(a.issues)?;
        let notes = self
            .store
            .list_notes(NoteScope::Run, Some(run.id), ISSUE_PREFIX)
            .await?;
        for key in &keys {
            if !notes.iter().any(|n| &n.key == key) {
                return Err(CoreError::NotFound {
                    kind: "issue",
                    id: key.clone(),
                });
            }
        }
        let issues: Vec<IssueRef> = keys
            .into_iter()
            .map(|key| IssueRef {
                run_id: run.id,
                key,
            })
            .collect();
        let project = self.project_for(&run.repo).await?;
        let now = Utc::now();
        let evidence = |outcome: LessonOutcome, note: String, commit: Option<String>| Evidence {
            run_id: Some(run.id),
            agent: caller.id,
            node: LOCAL_NODE.into(),
            at: now,
            commit,
            dirty: None,
            outcome,
            note,
        };
        // Where the run's work is, if its checkout is still there.
        let checkout = self.integration_path(&run);
        let commit = if checkout.is_dir() {
            self.workspaces
                .head(&checkout)
                .await
                .ok()
                .filter(|c| !c.is_empty())
        } else {
            None
        };
        let step = match how {
            "lesson_create" => {
                let title = checked("title", &a.title, TITLE_MAX)?;
                let resolution = checked(
                    "resolution",
                    a.resolution.as_deref().unwrap_or(""),
                    FIELD_MAX,
                )?;
                if title.is_empty() || resolution.is_empty() {
                    return Err(CoreError::Conflict(
                        "a lesson needs a title and a resolution".into(),
                    ));
                }
                let outcome = a.outcome.ok_or_else(|| {
                    CoreError::Conflict(
                        "say how it ended: outcome resolved, workaround or unresolved".into(),
                    )
                })?;
                let mut keywords: Vec<String> = a
                    .keywords
                    .iter()
                    .map(|k| k.trim().to_owned())
                    .filter(|k| !k.is_empty())
                    .collect();
                keywords.truncate(KEYWORDS_MAX);
                let mut files: Vec<String> = a
                    .files
                    .iter()
                    .map(|f| f.trim().to_owned())
                    .filter(|f| !f.is_empty())
                    .collect();
                files.truncate(FILES_MAX);
                TriageStep::Create(Lesson {
                    id: uuid::Uuid::new_v4().to_string(),
                    project_id: project.id.clone(),
                    title,
                    symptom: checked("symptom", &a.symptom, FIELD_MAX)?,
                    keywords,
                    conditions: checked("conditions", &a.conditions, FIELD_MAX)?,
                    files,
                    resolution,
                    outcome,
                    status: LessonStatus::Active,
                    superseded_by: None,
                    evidence: vec![evidence(outcome, String::new(), commit)],
                    sources: Vec::new(),
                    created_at: now,
                    updated_at: now,
                })
            }
            "lesson_merge" => {
                let id = a
                    .lesson
                    .ok_or_else(|| CoreError::Conflict("name the lesson to merge into".into()))?;
                let mut lesson = self.store.get_lesson(&id).await?;
                if lesson.project_id != project.id {
                    return Err(CoreError::Conflict(
                        "that lesson belongs to another project".into(),
                    ));
                }
                let mut revised = false;
                if let Some(resolution) = a.resolution {
                    let resolution = checked("resolution", &resolution, FIELD_MAX)?;
                    if !resolution.is_empty() {
                        lesson.resolution = resolution;
                        revised = true;
                    }
                }
                if let Some(outcome) = a.outcome {
                    lesson.outcome = outcome;
                    revised = true;
                }
                // A new resolution or outcome answers the report that put it up for review.
                if revised && lesson.status == LessonStatus::Review {
                    lesson.status = LessonStatus::Active;
                }
                lesson.evidence.push(evidence(
                    a.outcome.unwrap_or(lesson.outcome),
                    checked("note", &a.note, FIELD_MAX)?,
                    commit,
                ));
                lesson.updated_at = now;
                TriageStep::Merge(lesson)
            }
            _ => TriageStep::Keep {
                why: checked("why", &a.why, FIELD_MAX)?,
            },
        };
        let created = matches!(step, TriageStep::Create(_));
        let (records, lesson) = self.store.triage(&issues, step, caller.id).await?;
        // Records written now, not those of an earlier call this one repeats.
        let fresh: Vec<_> = records
            .iter()
            .filter(|r| r.at >= now && r.by == caller.id)
            .cloned()
            .collect();
        if !fresh.is_empty() {
            self.publish(EventKind::IssueTriaged {
                run_id: run.id,
                records: fresh,
            })
            .await;
            if let Some(lesson) = &lesson {
                let lesson = lesson.clone();
                self.publish(if created {
                    EventKind::LessonCreated { lesson }
                } else {
                    EventKind::LessonUpdated { lesson }
                })
                .await;
            }
        }
        let sorted: Vec<Value> = records
            .iter()
            .map(|r| {
                let result = match &r.triage {
                    Triage::Lesson { lesson_id } => json!({ "lesson": lesson_id }),
                    Triage::Merged { lesson_id } => json!({ "merged": lesson_id }),
                    Triage::Kept { .. } => json!("kept"),
                };
                json!({ "issue": r.issue.key, "result": result })
            })
            .collect();
        Ok(json!({ "sorted": sorted, "lesson": lesson.map(|l| l.id) }))
    }

    /// The caller's project's lessons matching a query, best first.
    pub(crate) async fn lesson_search(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            query: String,
            #[serde(default)]
            limit: Option<usize>,
            #[serde(default)]
            include_superseded: bool,
        }
        let a: A = args(input)?;
        let project = self.project_for(&caller.cwd).await?;
        let terms = search_terms(&a.query);
        let limit = a.limit.unwrap_or(5).clamp(1, SEARCH_LIMIT);
        let found = self
            .store
            .search_lessons(&project.id, &terms, a.include_superseded, limit)
            .await?;
        // A review clue, not a verdict: the files a record is about moved on after its last
        // check (LESSONS §4.4). Never turns a hit down.
        let changed = files_changed(&found, &caller.cwd).await;
        let lessons: Vec<Value> = found
            .iter()
            .map(|l| {
                let mut hit = search_hit(l);
                if let Some(files) = changed.get(&l.id) {
                    hit["files_changed"] = json!(files);
                }
                hit
            })
            .collect();
        Ok(
            json!({ "lessons": lessons, "note": "A match may not apply here: check its conditions and evidence (lesson_get) before relying on it. Text ending in … is cut; lesson_get has it in full. files_changed lists the record's files that changed after its last check: read it again before going by it, but a change alone does not make it wrong." }),
        )
    }

    pub(crate) async fn lesson_get(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            id: String,
            #[serde(default)]
            evidence_offset: usize,
            #[serde(default)]
            evidence_limit: Option<usize>,
        }
        let a: A = args(input)?;
        let lesson = self.own_lesson(caller, &a.id).await?;
        let limit = a
            .evidence_limit
            .unwrap_or(EVIDENCE_SHOWN)
            .clamp(1, EVIDENCE_PAGE_MAX);
        let mut view = lesson_view(&lesson, a.evidence_offset, limit);
        let changed = files_changed(std::slice::from_ref(&lesson), &caller.cwd).await;
        if let Some(files) = changed.get(&lesson.id) {
            view["files_changed"] = json!(files);
        }
        Ok(view)
    }

    /// How a lesson went when the caller used it: one more piece of evidence; one that did
    /// not work puts the lesson up for review, its history kept.
    pub(crate) async fn lesson_report(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        #[derive(Deserialize)]
        struct A {
            id: String,
            worked: bool,
            #[serde(default)]
            note: String,
        }
        let a: A = args(input)?;
        let mut lesson = self.own_lesson(caller, &a.id).await?;
        let (commit, dirty) = if self.workspaces.versioned(&caller.cwd).await {
            let commit = self
                .workspaces
                .head(&caller.cwd)
                .await
                .ok()
                .filter(|c| !c.is_empty());
            let dirty = self
                .workspaces
                .uncommitted(&caller.cwd)
                .await
                .ok()
                .map(|f| !f.is_empty());
            (commit, dirty)
        } else {
            (None, None)
        };
        let now = Utc::now();
        lesson.evidence.push(Evidence {
            run_id: self.run_of(caller.id).await?.map(|(run, _)| run.id),
            agent: caller.id,
            node: LOCAL_NODE.into(),
            at: now,
            commit,
            dirty,
            outcome: if a.worked {
                lesson.outcome
            } else {
                LessonOutcome::Unresolved
            },
            note: checked("note", &a.note, FIELD_MAX)?,
        });
        if !a.worked && lesson.status == LessonStatus::Active {
            lesson.status = LessonStatus::Review;
        }
        lesson.updated_at = now;
        self.store.update_lesson(&lesson).await?;
        self.publish(EventKind::LessonUpdated {
            lesson: lesson.clone(),
        })
        .await;
        Ok(json!({ "id": lesson.id, "status": lesson.status, "evidence": lesson.evidence.len() }))
    }

    async fn own_lesson(&self, caller: &Agent, id: &str) -> Result<Lesson, CoreError> {
        let lesson = self.store.get_lesson(id.trim()).await?;
        let project = self.project_for(&caller.cwd).await?;
        if lesson.project_id != project.id {
            // Other projects' records are not there for this caller.
            return Err(CoreError::NotFound {
                kind: "lesson",
                id: id.to_owned(),
            });
        }
        Ok(lesson)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn record(id: &str, title: &str, symptom: &str, keywords: &[&str]) -> Lesson {
        let mut l = lesson_with(&[LessonOutcome::Resolved], 0);
        l.id = id.into();
        l.title = title.into();
        l.symptom = symptom.into();
        l.keywords = keywords.iter().map(|k| (*k).to_owned()).collect();
        l
    }

    #[test]
    fn the_words_of_an_error_are_split_into_strong_and_weak() {
        let (strong, weak) = error_terms(
            "Compiling x\nerror[E0277]: the trait bound `Foo: Serialize` is not satisfied\n  --> src/a.rs:12\n`python` exited with code 9009 on line 120\nModuleNotFoundError: No module named 'yaml'\nENOENT at 0x80070005",
        );
        assert_eq!(
            strong,
            [
                "E0277",
                "9009",
                "ModuleNotFoundError",
                "ENOENT",
                "0x80070005"
            ]
        );
        assert!(weak.contains(&"trait".to_owned()) && weak.contains(&"Serialize".to_owned()));
        assert!(!weak.iter().any(|w| w == "the" || w == "120"));
        // Line numbers, ports and plain words are not strong.
        let (strong, _) =
            error_terms("listening on 8080\nat line 1234\nError: something went wrong");
        assert!(strong.is_empty(), "{strong:?}");
    }

    #[test]
    fn failed_checks_that_name_a_recorded_error_get_its_record() {
        let lessons = vec![
            record(
                "a",
                "python 9009 on Windows",
                "`python` exited with code 9009",
                &["WindowsApps"],
            ),
            record(
                "b",
                "trait bound",
                "error[E0277] when deriving",
                &["E0277", "Serialize"],
            ),
        ];
        let output = "error[E0277]: the trait bound `Foo: Serialize` is not satisfied";
        let (strong, weak) = error_terms(output);
        let related = match_lessons(&lessons, &strong, &weak);
        assert_eq!(related.len(), 1);
        assert_eq!(related[0].id, "b");
        assert_eq!(related[0].title, "trait bound");
        assert_eq!(related[0].matched[0], "E0277");
        assert!(related[0].matched.contains(&"Serialize".to_owned()));
        let text = related_text(&related).unwrap();
        assert!(text.contains("possibly related") && text.contains(r#""id":"b""#));
    }

    #[test]
    fn unrelated_output_gets_no_records() {
        let lessons = vec![record("a", "python 9009", "exited with code 9009", &[])];
        // Shared weak words alone are not enough.
        let (strong, weak) = error_terms("error: python not found\nlistening on 9009");
        assert!(match_lessons(&lessons, &strong, &weak).is_empty());
        let (strong, weak) = error_terms("error[E0599]: no method named `python`");
        assert!(match_lessons(&lessons, &strong, &weak).is_empty());
        // A code inside a longer word is not a match.
        let lessons = vec![record("c", "x", "see E02771", &[])];
        let (strong, weak) = error_terms("error[E0277]: x");
        assert!(match_lessons(&lessons, &strong, &weak).is_empty());
        assert!(related_text(&[]).is_none());
    }

    #[test]
    fn at_most_three_records_the_closest_first() {
        let mut lessons: Vec<Lesson> = (0..5)
            .map(|i| record(&format!("l{i}"), &format!("r{i}"), "ENOENT", &[]))
            .collect();
        lessons[4].symptom = "ENOENT and EACCES".into();
        lessons[1].status = LessonStatus::Superseded;
        let (strong, weak) = error_terms("ENOENT then EACCES");
        let related = match_lessons(&lessons, &strong, &weak);
        assert_eq!(related.len(), RELATED_MAX);
        let ids: Vec<&str> = related.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["l4", "l0", "l2"]);
        assert_eq!(related[0].matched, ["ENOENT", "EACCES"]);
    }

    #[tokio::test]
    async fn a_lookup_that_fails_or_hangs_gives_no_records() {
        let failed = bounded_related(
            async { Err(CoreError::Conflict("index is locked".into())) },
            Duration::from_secs(1),
        )
        .await;
        assert!(failed.is_empty());
        let slow = bounded_related(
            std::future::pending::<Result<Vec<RelatedLesson>, CoreError>>(),
            Duration::from_millis(10),
        )
        .await;
        assert!(slow.is_empty());
    }

    #[test]
    fn a_pasted_error_becomes_search_terms() {
        assert_eq!(
            search_terms("error[E0277]: `python` exited with code 9009 (WindowsApps)"),
            vec![
                "error",
                "E0277",
                "python",
                "exited",
                "with",
                "code",
                "9009",
                "WindowsApps"
            ]
        );
        assert_eq!(search_terms("连接超时，走代理"), vec!["连接超时", "走代理"]);
        assert!(search_terms("  ,; ").is_empty());
    }

    fn lesson_with(outcomes: &[LessonOutcome], sources: usize) -> Lesson {
        let start = Utc::now() - chrono::Duration::hours(outcomes.len() as i64);
        let run = RunId::new();
        Lesson {
            id: "l1".into(),
            project_id: "p".into(),
            title: "t".into(),
            symptom: "s".into(),
            keywords: vec![],
            conditions: "c".into(),
            files: vec![],
            resolution: "r".into(),
            outcome: LessonOutcome::Resolved,
            status: LessonStatus::Active,
            superseded_by: None,
            evidence: outcomes
                .iter()
                .enumerate()
                .map(|(i, o)| Evidence {
                    run_id: None,
                    agent: AgentId::new(),
                    node: LOCAL_NODE.into(),
                    at: start + chrono::Duration::hours(i as i64),
                    commit: None,
                    dirty: None,
                    outcome: *o,
                    note: format!("try {i}"),
                })
                .collect(),
            sources: (0..sources)
                .map(|i| IssueRef {
                    run_id: run,
                    key: format!("issue/{i}"),
                })
                .collect(),
            created_at: start,
            updated_at: start,
        }
    }

    fn notes(page: &Value) -> Vec<String> {
        page["evidence"]["newest_first"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["note"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn a_long_history_shows_counts_and_the_latest_tries() {
        let mut outcomes = vec![LessonOutcome::Resolved; 12];
        outcomes[3] = LessonOutcome::Unresolved;
        let lesson = lesson_with(&outcomes, 7);
        let view = lesson_view(&lesson, 0, EVIDENCE_SHOWN);
        assert_eq!(view["evidence"]["total"], 12);
        assert_eq!(view["evidence"]["resolved"], 11);
        assert_eq!(view["evidence"]["unresolved"], 1);
        assert_eq!(
            notes(&view),
            ["try 11", "try 10", "try 9", "try 8", "try 7"]
        );
        assert!(
            view["evidence"]["more"]
                .as_str()
                .unwrap()
                .contains("evidence_offset 5"),
            "{view}"
        );
        // The old failure is not on this page but is shown anyway.
        assert_eq!(view["last_failure"]["note"], "try 3");
        assert_eq!(view["sources"]["total"], 7);
        assert_eq!(view["sources"]["latest"][0]["key"], "issue/6");
        assert_eq!(view["sources"]["latest"].as_array().unwrap().len(), 3);
        assert_eq!(view["lesson"]["resolution"], "r");
        assert!(view.get("review").is_none());

        // Paging back reaches every try, then says there is no more.
        let mut seen = notes(&view);
        let mut offset = 5;
        loop {
            let page = lesson_view(&lesson, offset, EVIDENCE_SHOWN);
            let got = notes(&page);
            if got.is_empty() {
                break;
            }
            offset += got.len();
            seen.extend(got);
            if page["evidence"].get("more").is_none() {
                break;
            }
        }
        assert_eq!(seen.len(), 12);
        assert_eq!(seen.last().unwrap(), "try 0");
    }

    #[test]
    fn a_lesson_under_review_says_why_up_front() {
        let mut lesson = lesson_with(&[LessonOutcome::Resolved, LessonOutcome::Unresolved], 1);
        lesson.status = LessonStatus::Review;
        let view = lesson_view(&lesson, 0, EVIDENCE_SHOWN);
        assert!(view["review"].as_str().unwrap().contains("last_failure"));
        assert_eq!(view["last_failure"]["note"], "try 1");
        assert_eq!(view["lesson"]["status"], "review");
        assert!(view["evidence"].get("more").is_none());

        let clean = lesson_with(&[LessonOutcome::Resolved], 0);
        assert!(lesson_view(&clean, 0, EVIDENCE_SHOWN)["last_failure"].is_null());
    }

    #[test]
    fn a_search_hit_cuts_long_text_and_says_so() {
        let mut lesson = lesson_with(&[LessonOutcome::Resolved], 0);
        let hit = search_hit(&lesson);
        assert_eq!(hit["resolution"], "r");
        assert!(hit.get("truncated").is_none());

        lesson.resolution = format!("第一段{}\n\n第二段", "长".repeat(40));
        lesson.symptom = "错".repeat(200);
        let hit = search_hit(&lesson);
        let resolution = hit["resolution"].as_str().unwrap();
        assert!(resolution.ends_with('…') && !resolution.contains("第二段"));
        let symptom = hit["symptom"].as_str().unwrap();
        assert!(symptom.ends_with('…') && symptom.len() <= EXCERPT_MAX + '…'.len_utf8());
        assert_eq!(hit["truncated"], true);
    }

    // -----------------------------------------------------------------------------------
    // Review clues and projects

    #[test]
    fn a_record_says_which_of_its_files_moved_on_since_its_last_check() {
        let mut lesson = lesson_with(&[LessonOutcome::Resolved, LessonOutcome::Resolved], 0);
        lesson.files = vec![
            "crates/hub/src/api.rs".into(),
            "apps\\gui\\src\\api.ts".into(),
            "crates/orchestrator/src/lessons.rs".into(),
        ];
        // The latest evidence names the commit the files are compared with.
        lesson.evidence[0].commit = Some("aaa".into());
        lesson.evidence[1].commit = Some("bbb".into());
        assert_eq!(last_commit(&lesson), Some("bbb"));
        // git reports `/` and the record may hold `\`; both are the same file.
        let changed = vec![
            "crates/hub/src/api.rs".to_owned(),
            "apps/gui/src/api.ts".to_owned(),
        ];
        assert_eq!(
            changed_among(&lesson.files, &changed),
            vec![
                "crates/hub/src/api.rs".to_owned(),
                "apps\\gui\\src\\api.ts".to_owned()
            ]
        );
        // Nothing of it changed: no clue, and so no mark.
        assert!(changed_among(&lesson.files, &["README.md".to_owned()]).is_empty());
        // A record without a commit, or without files, is never marked.
        let mut blind = lesson.clone();
        blind.evidence.iter_mut().for_each(|e| e.commit = None);
        assert_eq!(last_commit(&blind), None);
        let mut bare = lesson.clone();
        bare.files.clear();
        assert!(changed_among(&bare.files, &changed).is_empty());
    }

    #[tokio::test]
    async fn files_changed_since_a_commit_come_from_git() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        git(&["init", "-q"]);
        std::fs::write(dir.path().join("changed.rs"), "a").unwrap();
        std::fs::write(dir.path().join("same.rs"), "a").unwrap();
        git(&["add", "."]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "one",
        ]);
        let first = git(&["rev-parse", "HEAD"]);
        std::fs::write(dir.path().join("changed.rs"), "b").unwrap();
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-am",
            "two",
        ]);
        // An edit that is still in the working tree counts too.
        std::fs::write(dir.path().join("changed.rs"), "c").unwrap();
        let changed = changed_since(dir.path(), &first).await;
        assert_eq!(changed, vec!["changed.rs".to_owned()], "{changed:?}");
        // A commit at the working tree's content names nothing.
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-am",
            "three",
        ]);
        let head = git(&["rev-parse", "HEAD"]);
        assert!(changed_since(dir.path(), &head).await.is_empty());
        // A directory without git says nothing rather than guessing.
        let plain = tempfile::tempdir().unwrap();
        assert!(changed_since(plain.path(), &first).await.is_empty());
    }

    fn project_at(id: &str, name: &str, path: &str, remotes: &[&str]) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            remotes: remotes.iter().map(|r| (*r).to_owned()).collect(),
            paths: vec![ProjectPath {
                node: LOCAL_NODE.into(),
                path: PathBuf::from(path),
            }],
            unconfirmed_remote: None,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn merging_projects_brings_their_lessons_and_evidence_into_one() {
        let store = crossagents_store_sqlite::SqliteStore::open_in_memory()
            .await
            .unwrap();
        store
            .put_project(&project_at("a", "repo", "/a/repo", &["github.com/o/r"]))
            .await
            .unwrap();
        store
            .put_project(&project_at("b", "repo elsewhere", "/b/repo", &[]))
            .await
            .unwrap();
        let mut theirs = lesson_with(&[LessonOutcome::Resolved, LessonOutcome::Unresolved], 1);
        theirs.id = "l-b".into();
        theirs.project_id = "b".into();
        let mut mine = lesson_with(&[LessonOutcome::Resolved], 2);
        mine.id = "l-a".into();
        mine.project_id = "a".into();
        for (lesson, key) in [(theirs, "issue/b/1"), (mine, "issue/a/1")] {
            store
                .triage(
                    &[IssueRef {
                        run_id: RunId::new(),
                        key: key.into(),
                    }],
                    TriageStep::Create(lesson),
                    AgentId::new(),
                )
                .await
                .unwrap();
        }
        assert_eq!(merge_projects(&store, "b", "a").await.unwrap().id, "a");
        // Both records and everything they hold are the surviving project's now.
        let lessons = store.list_lessons("a").await.unwrap();
        assert_eq!(lessons.len(), 2);
        let moved = store.get_lesson("l-b").await.unwrap();
        assert_eq!(moved.project_id, "a");
        assert_eq!(moved.evidence.len(), 2);
        assert_eq!(moved.sources.len(), 1);
        assert!(store.list_lessons("b").await.unwrap().is_empty());
        // Its checkout resolves to the surviving project; the source resolves to nothing.
        let projects = store.list_projects().await.unwrap();
        let (resolved, _) = resolve(&projects, Path::new("/b/repo"), None);
        assert_eq!(resolved.id, "a");
        let source = projects.iter().find(|p| p.id == "b").unwrap();
        assert!(source.paths.is_empty() && source.remotes.is_empty());
        // A repeat of an interrupted merge writes nothing new.
        merge_projects(&store, "b", "a").await.unwrap();
        assert_eq!(store.list_lessons("a").await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_person_binds_a_remote_or_a_checkout_to_a_project() {
        let store = crossagents_store_sqlite::SqliteStore::open_in_memory()
            .await
            .unwrap();
        store
            .put_project(&project_at("a", "repo", "/a/repo", &["github.com/o/r"]))
            .await
            .unwrap();
        let mut waiting = project_at("b", "x", "/b/x", &[]);
        waiting.unconfirmed_remote = Some("github.com/me/x".into());
        store.put_project(&waiting).await.unwrap();
        // The alias is recognised however it is written, and settles the identity.
        let bound = bind_project(&store, "b", Some("git@github.com:me/x.git"), None, false)
            .await
            .unwrap();
        assert_eq!(bound.remotes, vec!["github.com/me/x".to_owned()]);
        assert!(bound.unconfirmed_remote.is_none());
        // A remote another project owns is that project's: merging is the other call.
        let refused = bind_project(&store, "b", Some("https://github.com/o/r"), None, false)
            .await
            .unwrap_err();
        assert!(matches!(refused, CoreError::Conflict(_)), "{refused}");
        // A checkout belongs to one project: binding it elsewhere moves it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap().to_owned();
        bind_project(&store, "b", None, Some(&path), false)
            .await
            .unwrap();
        bind_project(&store, "a", None, Some(&path), false)
            .await
            .unwrap();
        let projects = store.list_projects().await.unwrap();
        let b = projects.iter().find(|p| p.id == "b").unwrap();
        assert!(
            !b.paths.iter().any(|at| at.path == dir.path()),
            "{:?}",
            b.paths
        );
        let a = projects.iter().find(|p| p.id == "a").unwrap();
        assert!(
            a.paths.iter().any(|at| at.path == dir.path()),
            "{:?}",
            a.paths
        );
    }

    #[tokio::test]
    async fn a_checkout_that_is_another_repository_is_split_out() {
        let store = crossagents_store_sqlite::SqliteStore::open_in_memory()
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut project = project_at("a", "repo", dir.path().to_str().unwrap(), &[]);
        project.unconfirmed_remote = Some("github.com/someone/CrossAgents".into());
        store.put_project(&project).await.unwrap();
        let fresh = bind_project(&store, "a", None, Some(dir.path().to_str().unwrap()), true)
            .await
            .unwrap();
        assert_ne!(fresh.id, "a");
        // The remote that was waiting belongs to the checkout, so it goes with it.
        assert_eq!(
            fresh.remotes,
            vec!["github.com/someone/CrossAgents".to_owned()]
        );
        assert!(fresh.unconfirmed_remote.is_none());
        let projects = store.list_projects().await.unwrap();
        let old = projects.iter().find(|p| p.id == "a").unwrap();
        assert!(old.paths.is_empty());
        assert!(old.unconfirmed_remote.is_none());
        // The directory now resolves to the project it was split into.
        let (resolved, _) = resolve(&projects, dir.path(), None);
        assert_eq!(resolved.id, fresh.id);
    }
}
