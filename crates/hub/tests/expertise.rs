//! The expertise routes of the local API (docs/tasks/EXPERTISE.md §6.6): the profile's
//! entries, its candidates, and the views built from them.

use chrono::Utc;
use crossagents_api_types::{
    AcceptCandidateRequest, AcceptCandidateResponse, Candidate, CandidateOrigin, CandidateStatus,
    ConflictDecision, ConflictVerdict, DomainBudget, EntryKind, EntryStats, EntryStatus,
    ExpertiseCard, ExpertiseEntry, ExpertiseMaintenance, ListCandidatesResponse,
    ListExpertiseResponse, OwnerKind, SaveExpertiseRequest,
};
use crossagents_core::{PERSON_OWNER_ID, SourceRef, TaskId};
use crossagents_hub::{HubConfig, Paths, RunningHub};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tempfile::TempDir;

struct TestHub {
    hub: RunningHub,
    client: Client,
    base: String,
    _home: TempDir,
}

impl TestHub {
    async fn start() -> Self {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let hub = crossagents_hub::start(HubConfig::new(paths, 0))
            .await
            .unwrap();
        let base = format!("http://{}", hub.addr);
        Self {
            hub,
            client: Client::new(),
            base,
            _home: home,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.client.get(self.url(path)).bearer_auth(&self.hub.token)
    }

    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .post(self.url(path))
            .bearer_auth(&self.hub.token)
    }

    fn delete(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .delete(self.url(path))
            .bearer_auth(&self.hub.token)
    }

    async fn ok<T: serde::de::DeserializeOwned>(&self, resp: reqwest::Response) -> T {
        assert!(
            resp.status().is_success(),
            "{}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
        resp.json().await.unwrap()
    }

    fn entry(&self, domain: &str, title: &str) -> SaveExpertiseRequest {
        SaveExpertiseRequest {
            id: None,
            owner_kind: OwnerKind::Person,
            owner_id: PERSON_OWNER_ID.into(),
            domain: domain.into(),
            kind: EntryKind::Judgment,
            title: title.into(),
            text: "先红后绿".into(),
            example: None,
            applies_when: "crates/hub/**".into(),
            status: None,
            superseded_by: None,
        }
    }
}

#[tokio::test]
async fn expertise_entries_round_trip_through_the_api() {
    let t = TestHub::start().await;

    // Empty to begin with.
    let list: ListExpertiseResponse = t.ok(t.get("/v1/expertise").send().await.unwrap()).await;
    assert!(list.entries.is_empty());

    // Create.
    let created: ExpertiseEntry = t
        .ok(t
            .post("/v1/expertise")
            .json(&t.entry("rust", "测试先于实现"))
            .send()
            .await
            .unwrap())
        .await;
    assert_eq!(created.status, EntryStatus::Active);
    assert_eq!(created.owner_id, PERSON_OWNER_ID);
    assert!(!created.id.is_empty());

    // List, by owner and by domain.
    let list: ListExpertiseResponse = t
        .ok(t
            .get("/v1/expertise?owner_kind=person&domain=rust")
            .send()
            .await
            .unwrap())
        .await;
    assert_eq!(list.entries.len(), 1);
    let none: ListExpertiseResponse = t
        .ok(t
            .get("/v1/expertise?owner_kind=person&domain=gui")
            .send()
            .await
            .unwrap())
        .await;
    assert!(none.entries.is_empty());

    // Get one.
    let one: ExpertiseEntry = t
        .ok(t
            .get(&format!("/v1/expertise/{}", created.id))
            .send()
            .await
            .unwrap())
        .await;
    assert_eq!(one.title, "测试先于实现");

    // Search finds it, and a query without words is a 400.
    let found: ListExpertiseResponse = t
        .ok(t.get("/v1/expertise/search?q=测试").send().await.unwrap())
        .await;
    assert_eq!(found.entries.len(), 1);
    assert_eq!(found.entries[0].id, created.id);
    let empty = t.get("/v1/expertise/search?q=").send().await.unwrap();
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);

    // Replace: the body cannot reset what the store counted.
    let mut replace = t.entry("rust", "先写会失败的测试");
    replace.id = Some(created.id.clone());
    let replaced: ExpertiseEntry = t
        .ok(t.post("/v1/expertise").json(&replace).send().await.unwrap())
        .await;
    assert_eq!(replaced.id, created.id);
    assert_eq!(replaced.title, "先写会失败的测试");
    assert_eq!(replaced.created_at, created.created_at);

    // An entry without a title is refused.
    let mut blank = t.entry("rust", "  ");
    blank.id = None;
    let refused = t.post("/v1/expertise").json(&blank).send().await.unwrap();
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);

    // Editing the text does not reset a status the person set.
    let mut warning = t.entry("rust", "先看清楚再用");
    warning.status = Some(EntryStatus::Review);
    let flagged: ExpertiseEntry = t
        .ok(t.post("/v1/expertise").json(&warning).send().await.unwrap())
        .await;
    assert_eq!(flagged.status, EntryStatus::Review);
    let mut edit = t.entry("rust", "先看清楚再用（改）");
    edit.id = Some(flagged.id.clone());
    let edited: ExpertiseEntry = t
        .ok(t.post("/v1/expertise").json(&edit).send().await.unwrap())
        .await;
    assert_eq!(edited.status, EntryStatus::Review);
    assert_eq!(edited.title, "先看清楚再用（改）");

    // Delete.
    let gone = t
        .delete(&format!("/v1/expertise/{}", created.id))
        .send()
        .await
        .unwrap();
    assert_eq!(gone.status(), StatusCode::NO_CONTENT);
    let missing = t
        .get(&format!("/v1/expertise/{}", created.id))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

/// Puts a candidate straight into the store: candidates come from the orchestrator, and the
/// API only decides them.
async fn seed_candidate(t: &TestHub, entry_id: &str, conflicts: &[&str]) -> Candidate {
    let now = Utc::now();
    let proposed = ExpertiseEntry {
        id: String::new(), // the hub fills it in on acceptance
        owner_kind: OwnerKind::Person,
        owner_id: PERSON_OWNER_ID.into(),
        domain: "rust".into(),
        kind: EntryKind::Judgment,
        title: "一次合并的写法".into(),
        text: "候选的正文".into(),
        example: None,
        applies_when: "crates/hub/**".into(),
        evidence: 0,
        last_confirmed_at: None,
        status: EntryStatus::Active,
        superseded_by: None,
        stats: EntryStats::default(),
        sources: vec![SourceRef::Task {
            run_id: crossagents_core::RunId::new(),
            task_id: TaskId::new(),
        }],
        created_at: now,
        updated_at: now,
    };
    let candidate = Candidate {
        id: entry_id.into(),
        owner_kind: OwnerKind::Person,
        owner_id: PERSON_OWNER_ID.into(),
        domain: "rust".into(),
        proposed,
        conflicts_with: conflicts.iter().map(|c| (*c).to_owned()).collect(),
        origin: CandidateOrigin::Rework,
        status: CandidateStatus::Pending,
        suggested_by_lead: false,
        note: "返工里看出来的".into(),
        sources: Vec::new(),
        created_at: now,
        decided_at: None,
    };
    t.hub.bus.store().candidate_put(&candidate).await.unwrap();
    candidate
}

async fn pinned_entry(t: &TestHub, domain: &str, title: &str) -> ExpertiseEntry {
    t.ok(t
        .post("/v1/expertise")
        .json(&t.entry(domain, title))
        .send()
        .await
        .unwrap())
        .await
}

#[tokio::test]
async fn expertise_accept_refuses_a_conflict_without_a_verdict() {
    let t = TestHub::start().await;
    let old = pinned_entry(&t, "rust", "旧的判断").await;
    seed_candidate(&t, "c1", &[&old.id]).await;

    // No decisions at all.
    let refused = t
        .post("/v1/expertise/candidates/c1/accept")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let body: Value = refused.json().await.unwrap();
    assert_eq!(body["error"]["code"], "bad_request");
    assert!(
        body["error"]["message"].as_str().unwrap().contains(&old.id),
        "{body}"
    );

    // A verdict for someone else does not count.
    let refused = t
        .post("/v1/expertise/candidates/c1/accept")
        .json(&AcceptCandidateRequest {
            decisions: vec![ConflictDecision {
                entry_id: "someone-else".into(),
                verdict: ConflictVerdict::KeepOld,
            }],
        })
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);

    // Nothing changed: the candidate is still pending, the entry still active.
    let list: ListCandidatesResponse = t
        .ok(t
            .get("/v1/expertise/candidates?status=pending")
            .send()
            .await
            .unwrap())
        .await;
    assert_eq!(list.candidates.len(), 1);
    assert_eq!(list.candidates[0].status, CandidateStatus::Pending);
    let still: ExpertiseEntry = t
        .ok(t
            .get(&format!("/v1/expertise/{}", old.id))
            .send()
            .await
            .unwrap())
        .await;
    assert_eq!(still.status, EntryStatus::Active);
}

#[tokio::test]
async fn expertise_accept_adds_the_entry_and_applies_the_verdicts() {
    let t = TestHub::start().await;
    let replaced = pinned_entry(&t, "rust", "要被替代的判断").await;
    let kept = pinned_entry(&t, "rust", "留着不动的判断").await;
    seed_candidate(&t, "c2", &[&replaced.id, &kept.id]).await;

    let accepted: AcceptCandidateResponse = t
        .ok(t
            .post("/v1/expertise/candidates/c2/accept")
            .json(&AcceptCandidateRequest {
                decisions: vec![
                    ConflictDecision {
                        entry_id: replaced.id.clone(),
                        verdict: ConflictVerdict::Replace,
                    },
                    ConflictDecision {
                        entry_id: kept.id.clone(),
                        verdict: ConflictVerdict::KeepOld,
                    },
                ],
            })
            .send()
            .await
            .unwrap())
        .await;

    assert_eq!(accepted.candidate.status, CandidateStatus::Accepted);
    assert!(accepted.candidate.decided_at.is_some());
    assert_ne!(accepted.entry.id, replaced.id);
    assert_eq!(accepted.entry.status, EntryStatus::Active);
    assert_eq!(accepted.entry.owner_id, PERSON_OWNER_ID);
    // The candidate's source came across.
    assert_eq!(accepted.entry.sources.len(), 1);

    let old: ExpertiseEntry = t
        .ok(t
            .get(&format!("/v1/expertise/{}", replaced.id))
            .send()
            .await
            .unwrap())
        .await;
    assert_eq!(old.status, EntryStatus::Superseded);
    assert_eq!(
        old.superseded_by.as_deref(),
        Some(accepted.entry.id.as_str())
    );

    let untouched: ExpertiseEntry = t
        .ok(t
            .get(&format!("/v1/expertise/{}", kept.id))
            .send()
            .await
            .unwrap())
        .await;
    assert_eq!(untouched.status, EntryStatus::Active);

    // A second accept is a conflict, not a second entry.
    let again = t
        .post("/v1/expertise/candidates/c2/accept")
        .json(&AcceptCandidateRequest::default())
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::CONFLICT);

    // Rejecting a decided candidate is refused too; an unknown one is a 404.
    let rejected = t
        .post("/v1/expertise/candidates/c2/reject")
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::CONFLICT);
    let unknown = t
        .post("/v1/expertise/candidates/nope/reject")
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn expertise_reject_only_changes_the_candidate() {
    let t = TestHub::start().await;
    let old = pinned_entry(&t, "rust", "旧的判断").await;
    seed_candidate(&t, "c3", &[&old.id]).await;

    let rejected: Candidate = t
        .ok(t
            .post("/v1/expertise/candidates/c3/reject")
            .send()
            .await
            .unwrap())
        .await;
    assert_eq!(rejected.status, CandidateStatus::Rejected);

    let still: ExpertiseEntry = t
        .ok(t
            .get(&format!("/v1/expertise/{}", old.id))
            .send()
            .await
            .unwrap())
        .await;
    assert_eq!(still.status, EntryStatus::Active);
    let list: ListExpertiseResponse = t.ok(t.get("/v1/expertise").send().await.unwrap()).await;
    assert_eq!(list.entries.len(), 1);
}

#[tokio::test]
async fn expertise_card_maintenance_and_export_describe_the_profile() {
    let t = TestHub::start().await;

    // Nothing yet: the card says so instead of zero.
    let card: ExpertiseCard = t
        .ok(t.get("/v1/expertise/card").send().await.unwrap())
        .await;
    assert_eq!(card.entries.judgment, 0);
    assert_eq!(card.first_try_rate, None);
    let empty: ExpertiseMaintenance = t
        .ok(t.get("/v1/expertise/maintenance").send().await.unwrap())
        .await;
    assert!(empty.budgets.is_empty() && empty.stale.is_empty() && empty.conflicts.is_empty());
    let text = t.get("/v1/expertise/export").send().await.unwrap();
    assert_eq!(text.status(), StatusCode::OK);
    assert!(
        text.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap()
            .starts_with("text/markdown")
    );
    assert!(text.text().await.unwrap().contains("no entries yet"));

    // One entry, one pending candidate with a conflict.
    let old = pinned_entry(&t, "rust", "测试先于实现").await;
    seed_candidate(&t, "c4", &[&old.id]).await;

    let card: ExpertiseCard = t
        .ok(t.get("/v1/expertise/card").send().await.unwrap())
        .await;
    assert_eq!(card.entries.judgment, 1);
    assert_eq!(card.judgment_budget, 1500);
    assert!(card.low_sample);

    let maintenance: ExpertiseMaintenance = t
        .ok(t.get("/v1/expertise/maintenance").send().await.unwrap())
        .await;
    let budget: &DomainBudget = maintenance
        .budgets
        .iter()
        .find(|b| b.domain == "rust")
        .expect("the rust domain has a budget");
    assert!(budget.tokens > 0 && budget.over == 0);
    assert_eq!(
        maintenance.conflicts[0].candidate_id, "c4",
        "a pending candidate with conflicts is listed"
    );

    let markdown = t
        .get("/v1/expertise/export?domain=rust")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(markdown.contains("# Expertise profile: person me"));
    assert!(markdown.contains("## rust"));
    assert!(markdown.contains("测试先于实现"));
}

/// The project profile needs its id: there is no default one.
#[tokio::test]
async fn expertise_routes_ask_which_project() {
    let t = TestHub::start().await;
    let refused = t
        .get("/v1/expertise?owner_kind=project")
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let wrong: Value = refused.json().await.unwrap();
    assert_eq!(wrong["error"]["code"], "bad_request");
}

/// The routes are behind the hub's bearer token like the rest of the authed router.
#[tokio::test]
async fn expertise_routes_need_the_token() {
    let t = TestHub::start().await;
    let anon = t.client.get(t.url("/v1/expertise")).send().await.unwrap();
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);
}
