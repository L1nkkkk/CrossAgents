//! Expertise profiles (docs/tasks/EXPERTISE.md §6.2, task exp-tools): a worker of a run
//! searches this project's and the person's profiles, names an entry in
//! `task_complete.used`, and how the task ends is counted on that entry: a plain accept is a
//! first-try merge, a task sent back once was reworked, a dropped one dropped. Another
//! project's entry is neither found nor counted.

mod common;

use chrono::Utc;
use common::{TestHub, Watcher, git_repo};
use crossagents_api_types::{EventKind, TaskStatus};
use crossagents_core::{
    CandidateOrigin, CandidateStatus, EntryKind, EntryStats, EntryStatus, ExpertiseEntry,
    OwnerKind, PERSON_OWNER_ID, SourceRef, Store, UsageOutcome, UsageSource,
};
use crossagents_hub::Paths;
use crossagents_store_sqlite::SqliteStore;
use serde_json::{Value, json};

const MINE: &str = "01a19b48-0000-4000-8000-00000000a001";
/// The tag a brief shows for [`MINE`]: its id's first eight characters (push.rs `tag`).
const MINE_TAG: &str = "01a19b48";
/// An entry of another project: the same search terms, out of this caller's reach.
const OTHERS: &str = "02b28c34-0000-4000-8000-00000000b002";
/// The project's judgement (EXPERTISE §6.3): a `fail` on it must land under the project.
const PROJECT_ENTRY: &str = "03c39d45-0000-4000-8000-00000000c003";
const PROJECT_TAG: &str = "03c39d45";

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

fn accept(key: &str) -> String {
    call(
        "task_accept",
        json!({ "task": key, "force": true, "reason": "no checks configured" }),
    )
}

fn task_is(key: &'static str, want: TaskStatus) -> impl Fn(&crossagents_api_types::Event) -> bool {
    move |e| matches!(&e.kind, EventKind::TaskStatusChanged { key: k, to, .. } if k == key && *to == want)
}

fn judgment(id: &str, owner: (OwnerKind, &str), title: &str, text: &str) -> ExpertiseEntry {
    let now = Utc::now();
    ExpertiseEntry {
        id: id.to_owned(),
        owner_kind: owner.0,
        owner_id: owner.1.to_owned(),
        domain: "testing".into(),
        kind: EntryKind::Judgment,
        title: title.into(),
        text: text.into(),
        example: None,
        applies_when: "crates/**".into(),
        evidence: 0,
        last_confirmed_at: None,
        status: EntryStatus::Active,
        superseded_by: None,
        stats: EntryStats::default(),
        sources: Vec::new(),
        created_at: now,
        updated_at: now,
    }
}

fn entry(owner: (OwnerKind, &str), title: &str, text: &str) -> ExpertiseEntry {
    let id = match owner.0 {
        OwnerKind::Person => MINE,
        OwnerKind::Project => OTHERS,
    };
    judgment(id, owner, title, text)
}

/// The person's judgement, and one of another project that must stay out of reach.
async fn profiles(home: &std::path::Path) {
    let store = SqliteStore::open(&Paths::new(home).db()).await.unwrap();
    store
        .expertise_put(&entry(
            (OwnerKind::Person, PERSON_OWNER_ID),
            "先写会失败的测试",
            "flaky 的用例先红后绿，再实现\n\n只有改错字或纯重命名可以跳",
        ))
        .await
        .unwrap();
    store
        .expertise_put(&entry(
            (OwnerKind::Project, "another-project"),
            "别的项目的判断",
            "flaky 的用例在这里另有处理",
        ))
        .await
        .unwrap();
    store.close().await;
}

/// A hub on a fresh home that already holds [`profiles`], with a planner and a worker in a
/// git repository of its own.
async fn hub_with_profiles() -> (TestHub, tempfile::TempDir) {
    let repo = git_repo();
    let home = tempfile::tempdir().unwrap();
    profiles(home.path()).await;
    let t = TestHub::start_in(home, |_| {}).await;
    t.add_agent_in("boss", Some("opus"), "planner", repo.path())
        .await;
    t.add_agent_in("coder", Some("haiku"), "worker", repo.path())
        .await;
    (t, repo)
}

/// Starts a run whose single task `a` does `goal` and lets the planner act on the directives
/// its worker's summary carries.
async fn one_task(t: &TestHub, goal: String) -> Watcher {
    let w = t.watch().await;
    let plan = json!([{ "key": "a", "title": "task a", "spec": { "goal": goal } }]);
    let req = format!(
        "Build it. {}",
        call("plan_submit", json!({ "tasks": plan }))
    );
    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": req, "planner": "boss", "worker": "coder", "policy": { "plan_approval": "auto" } }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    w
}

/// The store of a stopped hub's home, to look up what was written.
async fn reopened(t: TestHub) -> SqliteStore {
    let home = t.restart_home().await;
    SqliteStore::open(&Paths::new(home.path()).db())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_worker_searches_names_an_entry_and_a_first_try_accept_counts_it() {
    let (t, _repo) = hub_with_profiles().await;
    // The worker searches, reads one in full by the short tag a brief would show, then names
    // it by that same tag (plus one tag that names nothing) when it reports.
    let goal = format!(
        "{} {} {}",
        call("expertise_search", json!({ "query": "flaky" })),
        call("expertise_get", json!({ "id": MINE_TAG })),
        call(
            "task_complete",
            json!({
                "task_id": "{task_id}",
                "summary": format!("wrote a.txt {}", accept("a")),
                "used": [MINE_TAG, "deadbeef"],
            })
        ),
    );
    let mut w = one_task(&t, goal).await;
    w.until("a merged", task_is("a", TaskStatus::Merged)).await;
    let output = w.output();
    assert!(output.contains("先写会失败的测试"), "{output}");
    // The hit carries the same short tag the brief shows...
    assert!(output.contains("\"tag\": \"01a19b48\""), "{output}");
    // ...and only the full entry has the second paragraph.
    assert!(output.contains("只有改错字或纯重命名可以跳"), "{output}");
    // The tag that names no entry is skipped and listed back, the good one is taken.
    assert!(
        output.contains("unrecognized_used") && output.contains("deadbeef"),
        "{output}"
    );
    assert!(output.contains("\"used\": 1"), "{output}");
    assert!(
        !output.contains("别的项目的判断"),
        "another project's entry is out of reach: {output}"
    );

    let store = reopened(t).await;
    let mine = store.expertise_get(MINE).await.unwrap();
    assert_eq!(mine.stats.used, 1, "{mine:?}");
    assert_eq!(mine.stats.merged_first_try, 1, "{mine:?}");
    assert_eq!(mine.stats.reworked, 0, "{mine:?}");
    assert_eq!(mine.stats.dropped, 0, "{mine:?}");
    // The judgement held on a real task: one more piece of evidence.
    assert_eq!(mine.evidence, 1, "{mine:?}");
    assert!(mine.last_confirmed_at.is_some(), "{mine:?}");

    let usages = store.usage_list(Some(MINE), None).await.unwrap();
    // Whatever the worker typed, the usage names the entry by its full id.
    assert!(usages.iter().all(|u| u.entry_id == MINE), "{usages:?}");
    let sources: Vec<UsageSource> = usages.iter().map(|u| u.source).collect();
    assert!(sources.contains(&UsageSource::Pulled), "{sources:?}");
    assert!(sources.contains(&UsageSource::Declared), "{sources:?}");
    for usage in &usages {
        assert_eq!(
            usage.outcome,
            Some(UsageOutcome::MergedFirstTry),
            "{usage:?}"
        );
    }

    // The other project's entry was neither found nor counted.
    let others = store.expertise_get(OTHERS).await.unwrap();
    assert_eq!(others.stats, EntryStats::default());
    assert!(
        store
            .usage_list(Some(OTHERS), None)
            .await
            .unwrap()
            .is_empty()
    );
    store.close().await;
}

#[tokio::test]
async fn a_task_sent_back_once_counts_as_reworked() {
    let (t, _repo) = hub_with_profiles().await;
    let again = format!(
        "[write a.txt=beta] {}",
        call(
            "task_complete",
            json!({
                "task_id": "{task_id}",
                "summary": format!("wrote beta {}", accept("a")),
                "used": [MINE_TAG],
            })
        )
    );
    let goal = format!(
        "[write a.txt=alpha] {}",
        call(
            "task_complete",
            json!({
                "task_id": "{task_id}",
                "summary": format!(
                    "wrote alpha {}",
                    call("task_rework", json!({ "task": "a", "notes": format!("say beta instead {again}") }))
                ),
                "used": [MINE_TAG],
            })
        )
    );
    let mut w = one_task(&t, goal).await;
    w.until("a merged", task_is("a", TaskStatus::Merged)).await;

    let store = reopened(t).await;
    let mine = store.expertise_get(MINE).await.unwrap();
    assert_eq!(mine.stats.used, 1, "named twice, counted once: {mine:?}");
    assert_eq!(mine.stats.reworked, 1, "{mine:?}");
    assert_eq!(mine.stats.merged_first_try, 0, "{mine:?}");
    assert_eq!(mine.evidence, 1, "{mine:?}");
    let usages = store.usage_list(Some(MINE), None).await.unwrap();
    assert!(
        usages
            .iter()
            .all(|u| u.outcome == Some(UsageOutcome::Reworked)),
        "{usages:?}"
    );
    store.close().await;
}

#[tokio::test]
async fn a_dropped_task_counts_as_dropped_without_evidence() {
    let (t, _repo) = hub_with_profiles().await;
    let goal = format!(
        "[write a.txt=alpha] {}",
        call(
            "task_complete",
            json!({
                "task_id": "{task_id}",
                "summary": format!(
                    "wrote alpha {}",
                    call("task_drop", json!({ "task": "a", "reason": "not what was wanted" }))
                ),
                "used": [MINE_TAG],
            })
        )
    );
    let mut w = one_task(&t, goal).await;
    w.until("a cancelled", task_is("a", TaskStatus::Cancelled))
        .await;

    let store = reopened(t).await;
    let mine = store.expertise_get(MINE).await.unwrap();
    assert_eq!(mine.stats.used, 1, "{mine:?}");
    assert_eq!(mine.stats.dropped, 1, "{mine:?}");
    assert_eq!(mine.stats.merged_first_try, 0, "{mine:?}");
    // Dropped says nothing about the judgement itself.
    assert_eq!(mine.evidence, 0, "{mine:?}");
    assert!(mine.last_confirmed_at.is_none(), "{mine:?}");
    let usages = store.usage_list(Some(MINE), None).await.unwrap();
    assert!(
        usages
            .iter()
            .all(|u| u.outcome == Some(UsageOutcome::Dropped)),
        "{usages:?}"
    );
    store.close().await;
}

/// The person's judgement, seeded before the hub starts. The project's own is seeded later,
/// once a first run has bound this checkout to a project and its id is known.
async fn person_profile(home: &std::path::Path) {
    let store = SqliteStore::open(&Paths::new(home).db()).await.unwrap();
    store
        .expertise_put(&judgment(
            MINE,
            (OwnerKind::Person, PERSON_OWNER_ID),
            "先写会失败的测试",
            "flaky 的用例先红后绿，再实现",
        ))
        .await
        .unwrap();
    store.close().await;
}

#[tokio::test]
async fn a_reviewers_failed_item_proposes_a_candidate_under_the_entries_owner() {
    let repo = git_repo();
    let home = tempfile::tempdir().unwrap();
    person_profile(home.path()).await;
    let t = TestHub::start_in(home, |_| {}).await;
    t.add_agent_in("boss", Some("opus"), "planner", repo.path())
        .await;
    t.add_agent_in("coder", Some("haiku"), "worker", repo.path())
        .await;
    t.add_agent_in("critic", Some("opus"), "reviewer", repo.path())
        .await;
    let mut w = t.watch().await;

    // A first run binds the checkout to a project; then its own judgement is seeded.
    let bind = format!(
        "[write a.txt=alpha] {}",
        call(
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": "wrote a" })
        )
    );
    let plan = json!([{ "key": "a", "title": "task a", "spec": { "goal": bind } }]);
    let req = format!(
        "Build it. {}",
        call("plan_submit", json!({ "tasks": plan }))
    );
    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": req, "planner": "boss", "worker": "coder", "policy": { "plan_approval": "auto" } }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    w.until("a reported", task_is("a", TaskStatus::Passed))
        .await;

    let store = SqliteStore::open(&Paths::new(t.home.path()).db())
        .await
        .unwrap();
    let projects = store.list_projects().await.unwrap();
    assert_eq!(projects.len(), 1, "{projects:?}");
    let project_id = projects[0].id.clone();
    store
        .expertise_put(&judgment(
            PROJECT_ENTRY,
            (OwnerKind::Project, &project_id),
            "项目的判断",
            "不要为了复用抽第三个调用点的抽象",
        ))
        .await
        .unwrap();
    store.close().await;

    // A verdict only the reviewer (opus) runs; the worker is haiku. It sits in the reviewer's
    // focus, nested out of the planner's and the worker's own directives.
    let submit = |entry_id: &str, note: &str, notes: &str| {
        call(
            "review_submit",
            json!({
                "task_id": "{task_id}",
                "approve": false,
                "notes": notes,
                "checklist": [
                    { "entry_id": entry_id, "verdict": "fail", "note": note },
                    { "entry_id": PROJECT_TAG, "verdict": "pass" },
                    { "entry_id": MINE_TAG, "verdict": "na" },
                    { "entry_id": "deadbeef", "verdict": "fail", "note": "names nothing" }
                ]
            }),
        )
        .replacen("[call ", "[call#mock-opus ", 1)
    };
    // The first review's notes carry the second request: the planner reads them as its turn.
    let again = call(
        "review_request",
        json!({
            "task": "g",
            "reviewer": "critic",
            "focus": submit(MINE_TAG, "人的判断也没做到", ""),
        }),
    );
    let first = call(
        "review_request",
        json!({
            "task": "g",
            "reviewer": "critic",
            "focus": submit(PROJECT_TAG, "又抽了第三个调用点", &again),
        }),
    );
    let goal = format!(
        "[write#mock-haiku g.txt=v1] {}",
        call(
            "task_complete",
            json!({ "task_id": "{task_id}", "summary": format!("wrote g {first}") })
        )
        .replacen("[call ", "[call#mock-haiku ", 1)
    );
    let plan = json!([{ "key": "g", "title": "task g", "spec": { "goal": goal } }]);
    let req = format!(
        "Build it. {}",
        call("plan_submit", json!({ "tasks": plan }))
    );
    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": req, "planner": "boss", "worker": "coder", "policy": { "plan_approval": "auto" } }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());

    w.until("g in review", task_is("g", TaskStatus::Reviewing))
        .await;
    w.until("g reviewed", task_is("g", TaskStatus::Passed))
        .await;
    w.until("g in review again", task_is("g", TaskStatus::Reviewing))
        .await;
    w.until("g reviewed again", task_is("g", TaskStatus::Passed))
        .await;

    // The reviewer's brief carried both judgements as numbered items.
    let brief = w
        .seen
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::AgentInput { text, .. } if text.contains("Reviewer checklist") => {
                Some(text.clone())
            }
            _ => None,
        })
        .expect("the reviewer got a checklist");
    assert!(brief.contains("[01a19b48] 先写会失败的测试"), "{brief}");
    assert!(brief.contains("[03c39d45] 项目的判断"), "{brief}");
    assert!(brief.contains("review_submit.checklist"), "{brief}");

    // A verdict is written after the task's status changes: wait until both are in the store.
    let deadline = tokio::time::Instant::now() + common::wait_limit();
    let (store, candidates) = loop {
        let store = SqliteStore::open(&Paths::new(t.home.path()).db())
            .await
            .unwrap();
        let candidates = store
            .candidate_list(None, Some(CandidateStatus::Pending))
            .await
            .unwrap();
        if candidates.len() == 2 || tokio::time::Instant::now() > deadline {
            break (store, candidates);
        }
        store.close().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    // One fail on the project's judgement and one on the person's: nothing for pass, na, or
    // an id that names no entry.
    assert_eq!(candidates.len(), 2, "{candidates:?}");
    let under = |kind: OwnerKind| {
        candidates
            .iter()
            .find(|c| c.owner_kind == kind)
            .unwrap_or_else(|| panic!("no candidate under {kind:?}: {candidates:?}"))
    };

    let project = under(OwnerKind::Project);
    assert_eq!(project.owner_id, project_id);
    assert_eq!(project.conflicts_with, [PROJECT_ENTRY.to_owned()]);
    assert_eq!(project.origin, CandidateOrigin::Review);
    assert_eq!(project.status, CandidateStatus::Pending);
    assert_eq!(project.note, "又抽了第三个调用点");
    assert!(project.proposed.text.contains("又抽了第三个调用点"));
    assert!(matches!(
        project.sources.as_slice(),
        [SourceRef::Task { .. }]
    ));

    let person = under(OwnerKind::Person);
    assert_eq!(person.owner_id, PERSON_OWNER_ID);
    assert_eq!(person.conflicts_with, [MINE.to_owned()]);
    assert_eq!(person.note, "人的判断也没做到");
    assert!(matches!(
        person.sources.as_slice(),
        [SourceRef::Task { .. }]
    ));
    store.close().await;
}
