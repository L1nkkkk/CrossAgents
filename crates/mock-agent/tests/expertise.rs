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
    EntryKind, EntryStats, EntryStatus, ExpertiseEntry, OwnerKind, PERSON_OWNER_ID, Store,
    UsageOutcome, UsageSource,
};
use crossagents_hub::Paths;
use crossagents_store_sqlite::SqliteStore;
use serde_json::{Value, json};

const MINE: &str = "01a19b48-0000-4000-8000-00000000a001";
/// The tag a brief shows for [`MINE`]: its id's first eight characters (push.rs `tag`).
const MINE_TAG: &str = "01a19b48";
/// An entry of another project: the same search terms, out of this caller's reach.
const OTHERS: &str = "02b28c34-0000-4000-8000-00000000b002";

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

fn entry(owner: (OwnerKind, &str), title: &str, text: &str) -> ExpertiseEntry {
    let now = Utc::now();
    ExpertiseEntry {
        id: match owner.0 {
            OwnerKind::Person => MINE.to_owned(),
            OwnerKind::Project => OTHERS.to_owned(),
        },
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
