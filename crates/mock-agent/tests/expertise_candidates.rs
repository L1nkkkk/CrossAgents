//! Expertise candidates (docs/tasks/EXPERTISE.md §6.4, task exp-candidates): a rework and a
//! task merged without rework each leave a candidate for the person, naming the entries they
//! may repeat or contradict; the lead can list and flag them but never decide one; and
//! `run_finish` reports how many wait.

mod common;

use chrono::Utc;
use common::{TestHub, git_repo};
use crossagents_api_types::{EventKind, TaskStatus};
use crossagents_core::{
    Candidate, CandidateOrigin, CandidateStatus, EntryKind, EntryStatus, ExpertiseEntry, OwnerKind,
    PERSON_OWNER_ID, Store,
};
use crossagents_hub::Paths;
use crossagents_store_sqlite::SqliteStore;
use serde_json::{Value, json};

/// A candidate seeded before a run, so a test can name it without a uuid.
const SEEDED: &str = "0c0ffee0-0000-4000-8000-00000000d001";

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

fn task_moved(
    key: &'static str,
    from: TaskStatus,
    to: TaskStatus,
) -> impl Fn(&crossagents_api_types::Event) -> bool {
    move |e| {
        matches!(
            &e.kind,
            EventKind::TaskStatusChanged { key: k, from: f, to: t, .. }
                if k == key && *f == from && *t == to
        )
    }
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
        stats: Default::default(),
        sources: Vec::new(),
        created_at: now,
        updated_at: now,
    }
}

fn seeded_candidate(id: &str, owner: (OwnerKind, &str), title: &str, note: &str) -> Candidate {
    Candidate {
        id: id.to_owned(),
        owner_kind: owner.0,
        owner_id: owner.1.to_owned(),
        domain: "testing".into(),
        proposed: judgment("", owner, title, "the body of the proposal"),
        conflicts_with: Vec::new(),
        origin: CandidateOrigin::Rework,
        status: CandidateStatus::Pending,
        suggested_by_lead: false,
        note: note.to_owned(),
        sources: Vec::new(),
        created_at: Utc::now(),
        decided_at: None,
    }
}

/// A hub on a fresh home with a planner and a worker in a git repository of its own.
async fn hub() -> (TestHub, tempfile::TempDir) {
    let repo = git_repo();
    let home = tempfile::tempdir().unwrap();
    let t = TestHub::start_in(home, |_| {}).await;
    t.add_agent_in("boss", Some("opus"), "planner", repo.path())
        .await;
    t.add_agent_in("coder", Some("haiku"), "worker", repo.path())
        .await;
    (t, repo)
}

async fn open(t: &TestHub) -> SqliteStore {
    SqliteStore::open(&Paths::new(t.home.path()).db())
        .await
        .unwrap()
}
/// Waits until `wanted` candidates are in the store, then reads them.
async fn candidates(t: &TestHub, wanted: usize) -> Vec<Candidate> {
    let deadline = tokio::time::Instant::now() + common::wait_limit();
    loop {
        let store = open(t).await;
        let found = store.candidate_list(None, None).await.unwrap();
        store.close().await;
        if found.len() >= wanted || tokio::time::Instant::now() > deadline {
            return found;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_rework_proposes_a_judgement_that_names_the_entry_it_contradicts() {
    let (t, repo) = hub().await;
    // The project exists once it is first seen, so its judgement can be seeded before the run.
    let project = t.hub.orchestrator.project_for(repo.path()).await.unwrap();
    let store = open(&t).await;
    store
        .expertise_put(&judgment(
            "0e0ffee0-0000-4000-8000-00000000e001",
            (OwnerKind::Project, &project.id),
            "先写会失败的测试",
            "新增行为先补一个会红的用例，再实现",
        ))
        .await
        .unwrap();
    store.close().await;

    // The second attempt writes beta and its issuer accepts it, so the task ends after one
    // rework: one candidate, not one per turn.
    let again = format!(
        "[write a.txt=beta] {}",
        call(
            "task_complete",
            json!({
                "task_id": "{task_id}",
                "summary": "wrote beta",
            })
        )
    );
    let notes = format!("先写会失败的测试：直接改实现，没先红后绿\n\n{again}");
    let goal = format!(
        "[write a.txt=alpha] {}",
        call(
            "task_complete",
            json!({
                "task_id": "{task_id}",
                "summary": "wrote alpha",
            })
        )
    );
    let mut w = t.watch().await;
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
    // The lead is not handed the report: it sends the rework once the task waits, then accepts
    // the second report.
    common::lead_accepts_when_waiting(
        &t,
        &mut w,
        "boss",
        "a",
        &call("task_rework", json!({ "task": "a", "notes": notes })),
    )
    .await;
    common::lead_accepts_when_waiting(
        &t,
        &mut w,
        "boss",
        "a",
        &call(
            "task_accept",
            json!({ "task": "a", "force": true, "reason": "no checks" }),
        ),
    )
    .await;
    w.until(
        "a merged",
        task_moved("a", TaskStatus::Passed, TaskStatus::Merged),
    )
    .await;

    let found = candidates(&t, 1).await;
    assert_eq!(found.len(), 1, "{found:?}");
    let candidate = &found[0];
    assert_eq!(candidate.origin, CandidateOrigin::Rework);
    assert_eq!(candidate.status, CandidateStatus::Pending);
    assert_eq!(candidate.owner_kind, OwnerKind::Project);
    assert_eq!(candidate.owner_id, project.id);
    // The title is the note's first line, the body the whole note.
    assert_eq!(
        candidate.proposed.title,
        "先写会失败的测试：直接改实现，没先红后绿"
    );
    assert!(candidate.proposed.text.contains("没先红后绿"));
    // The whole note is kept, the next round's directions included.
    assert!(
        candidate.proposed.text.contains("[write a.txt=beta]"),
        "{candidate:?}"
    );
    assert_eq!(candidate.proposed.kind, EntryKind::Judgment);
    assert!(candidate.note.contains("没先红后绿"));
    // The person's judgement reads like it, so the person has to rule between them.
    assert_eq!(
        candidate.conflicts_with,
        vec!["0e0ffee0-0000-4000-8000-00000000e001".to_owned()],
        "{candidate:?}"
    );
}

#[tokio::test]
async fn a_first_try_merge_proposes_the_goal_and_the_summary_as_an_example() {
    let (t, repo) = hub().await;
    let project = t.hub.orchestrator.project_for(repo.path()).await.unwrap();
    let summary = "wrote z.txt and added a test";
    let goal = format!(
        "[write z.txt=v1] {}",
        call(
            "task_complete",
            json!({
                "task_id": "{task_id}",
                "summary": summary,
            })
        )
    );
    let mut w = t.watch().await;
    let plan = json!([{ "key": "z", "title": "task z", "spec": { "goal": goal } }]);
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
    common::lead_accepts_when_waiting(
        &t,
        &mut w,
        "boss",
        "z",
        &call(
            "task_accept",
            json!({ "task": "z", "force": true, "reason": "no checks" }),
        ),
    )
    .await;
    w.until(
        "z merged",
        task_moved("z", TaskStatus::Passed, TaskStatus::Merged),
    )
    .await;

    let found = candidates(&t, 1).await;
    assert_eq!(found.len(), 1, "{found:?}");
    let candidate = &found[0];
    assert_eq!(candidate.origin, CandidateOrigin::MergedFirstTry);
    assert_eq!(candidate.status, CandidateStatus::Pending);
    assert_eq!(candidate.owner_kind, OwnerKind::Project);
    assert_eq!(candidate.owner_id, project.id);
    assert_eq!(candidate.proposed.kind, EntryKind::Example);
    assert_eq!(candidate.proposed.title, "task z");
    let example = candidate.proposed.example.as_ref().expect("an example");
    // Situation = the goal, chosen = the summary; nothing was rejected. The summary is the
    // worker's whole text, directives included.
    assert!(
        example.situation.contains("[write z.txt=v1]"),
        "{example:?}"
    );
    assert!(example.chosen.starts_with(summary), "{example:?}");
    assert!(example.rejected.is_empty() && example.rejected_why.is_empty());
}

#[tokio::test]
async fn a_lead_lists_and_flags_candidates_but_cannot_accept_or_reject_them() {
    let (t, _repo) = hub().await;
    let store = open(&t).await;
    store
        .candidate_put(&seeded_candidate(
            SEEDED,
            (OwnerKind::Person, PERSON_OWNER_ID),
            "先写会失败的测试",
            "a task was sent back for this",
        ))
        .await
        .unwrap();
    store.close().await;

    let boss = t.agent("boss").await;
    // The lead sees it, with enough to weigh and the entries it may conflict with.
    let pending = t
        .hub
        .orchestrator
        .call_tool(&boss, "expertise_pending", json!({}))
        .await
        .unwrap();
    let list = pending["candidates"].as_array().expect("a list");
    assert_eq!(list.len(), 1, "{pending}");
    assert_eq!(list[0]["id"], SEEDED);
    assert_eq!(list[0]["title"], "先写会失败的测试");
    assert_eq!(list[0]["suggested_by_lead"], false);

    // Deciding is the person's alone: an accept or a reject is refused outright.
    for decision in [
        json!({ "candidate": SEEDED, "accept": true }),
        json!({ "candidate": SEEDED, "reject": true }),
    ] {
        let err = t
            .hub
            .orchestrator
            .call_tool(&boss, "expertise_triage", decision)
            .await
            .unwrap_err();
        assert!(
            matches!(err, crossagents_core::CoreError::Conflict(_)),
            "{err:?}"
        );
        assert!(err.to_string().contains("only the person"), "{err}");
    }
    // Flagging needs a reason the person can weigh.
    let err = t
        .hub
        .orchestrator
        .call_tool(&boss, "expertise_triage", json!({ "candidate": SEEDED }))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("reason"), "{err}");

    // Flagging says so on the candidate and leaves its status alone.
    t.hub
        .orchestrator
        .call_tool(
            &boss,
            "expertise_triage",
            json!({ "candidate": SEEDED, "reason": "it matches what the person wants" }),
        )
        .await
        .unwrap();
    let store = open(&t).await;
    let flagged = store
        .candidate_list(None, None)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.id == SEEDED)
        .unwrap();
    assert!(flagged.suggested_by_lead, "{flagged:?}");
    assert_eq!(flagged.status, CandidateStatus::Pending);
    assert!(
        flagged.note.contains("a task was sent back for this")
            && flagged.note.contains("it matches what the person wants"),
        "{flagged:?}"
    );
    store.close().await;

    // A worker does not even have the tool.
    let coder = t.agent("coder").await;
    let err = t
        .hub
        .orchestrator
        .call_tool(
            &coder,
            "expertise_triage",
            json!({ "candidate": SEEDED, "reason": "x" }),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not available"), "{err}");
}

#[tokio::test]
async fn run_finish_reports_the_candidates_waiting_for_the_person() {
    let (t, _repo) = hub().await;
    let store = open(&t).await;
    for (id, title) in [
        ("0c0ffee0-0000-4000-8000-00000000d002", "one"),
        ("0c0ffee0-0000-4000-8000-00000000d003", "two"),
    ] {
        store
            .candidate_put(&seeded_candidate(
                id,
                (OwnerKind::Person, PERSON_OWNER_ID),
                title,
                "",
            ))
            .await
            .unwrap();
    }
    store.close().await;

    let mut w = t.watch().await;
    let lead = t.agent("boss").await;
    let resp = t
        .post(
            "/v1/runs",
            json!({
                "request": call("run_finish", json!({ "summary": "done" })),
                "planner": "boss",
            }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    w.until(
        "the lead's turn ended",
        |e| matches!(e.kind, EventKind::AgentTurnCompleted { id, .. } if id == lead.id),
    )
    .await;
    // Counted, like unsorted issues: the close is never held up by it.
    let output = w.output();
    assert!(
        output.contains("\"pending_expertise\": 2"),
        "run_finish should say how many candidates wait: {output}"
    );
}
