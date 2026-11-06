//! Troubleshooting records (docs/tasks/LESSONS.md): problems recorded during a run as run
//! notes under `issue/`, sorted afterwards into lessons of the run's project, which any agent
//! of that project searches when it runs into trouble. Reference, never rules: nothing here
//! goes into an agent's instructions beyond one line saying the records exist.

use std::path::Path;

use chrono::Utc;
use crossagents_core::{
    Agent, CoreError, Evidence, ISSUE_PREFIX, IssueRef, Lesson, LessonOutcome, LessonStatus,
    NoteScope, Project, Run, RunId, Triage, TriageStep,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::Orchestrator;
use crate::projects::{LOCAL_NODE, main_repo, resolve};
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

impl Orchestrator {
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
                TriageStep::Create(Lesson {
                    id: uuid::Uuid::new_v4().to_string(),
                    project_id: project.id.clone(),
                    title,
                    symptom: checked("symptom", &a.symptom, FIELD_MAX)?,
                    keywords,
                    conditions: checked("conditions", &a.conditions, FIELD_MAX)?,
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
        let (records, lesson) = self.store.triage(&issues, step, caller.id).await?;
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
        let lessons: Vec<Value> = found.iter().map(search_hit).collect();
        Ok(
            json!({ "lessons": lessons, "note": "A match may not apply here: check its conditions and evidence (lesson_get) before relying on it. Text ending in … is cut; lesson_get has it in full." }),
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
        Ok(lesson_view(&lesson, a.evidence_offset, limit))
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
    use super::*;
    use crossagents_core::AgentId;

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
}
