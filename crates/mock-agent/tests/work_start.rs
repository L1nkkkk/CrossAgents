//! A conversation proposes a Work and the person starts or declines it (docs/tasks/W1.md §6),
//! and a lead whose run ended starts another one in the same repository (docs/tasks/R1.md §2).

mod common;

use common::{TestHub, git_repo, native_plugin};
use crossagents_api_types::EventKind;
use serde_json::{Value, json};

fn call(tool: &str, args: Value) -> String {
    format!("[call {tool} {args}]")
}

/// The open Work card `lyra` put up.
async fn card(t: &TestHub) -> Value {
    let all: Value = t.get("/v1/questions").await.json().await.unwrap();
    all["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["action"] == "work" && i["status"] == "open")
        .cloned()
        .unwrap_or_else(|| panic!("no work card in {all}"))
}

async fn propose(t: &TestHub) -> Value {
    let mut w = t.watch().await;
    let proposal = call(
        "work_start",
        json!({ "request": "Add notes.txt with the agreed wording.", "title": "Add notes" }),
    );
    t.send("lyra", &format!("Let's do it on a branch. {proposal}"))
        .await;
    w.until(
        "card",
        |e| matches!(&e.kind, EventKind::HumanAsked { item } if item.text == "Add notes"),
    )
    .await;
    card(t).await
}

/// Answers a Work card with 开始, and returns the run it started.
async fn start(t: &TestHub, item: &Value) -> Value {
    let resp = t
        .post(
            &format!("/v1/questions/{}/answer", item["id"].as_str().unwrap()),
            json!({ "answer": "开始" }),
        )
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let all: Value = t.get("/v1/questions?all=true").await.json().await.unwrap();
    let answered = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == item["id"])
        .unwrap_or_else(|| panic!("the card is gone from {all}"))
        .clone();
    let id = answered["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("the card started no run: {answered}"));
    let detail: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    detail["run"].clone()
}

/// `Orchestrator::short`: the id without dashes, last 8 characters (it names the run's
/// worktrees).
fn short(id: &str) -> String {
    let id = id.replace('-', "");
    id[id.len() - 8..].to_owned()
}

/// Ends a run the person's way; the lead keeps its checkout (it is not archived).
async fn finish(t: &TestHub, run: &Value) -> Value {
    let resp = t
        .post(
            &format!("/v1/runs/{}/finish", run["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    resp.json::<Value>().await.unwrap()["run"].clone()
}

async fn agent(t: &TestHub, id: &str) -> Value {
    t.get(&format!("/v1/agents/{id}"))
        .await
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn starting_a_proposed_work_runs_it_on_its_own_branch() {
    let repo = git_repo();
    let t = TestHub::start().await;
    let lyra = t
        .add_agent_in("lyra", Some("opus"), "worker", repo.path())
        .await;

    let item = propose(&t).await;
    assert_eq!(item["detail"], "Add notes.txt with the agreed wording.");
    assert!(item["run_id"].is_null());

    let run = start(&t, &item).await;
    assert_eq!(run["title"], "Add notes");
    assert_eq!(run["request"], "Add notes.txt with the agreed wording.");
    // lyra's driver (ACP) cannot resume its session elsewhere: a copy leads it in the run's
    // checkout, the run says so, and the conversation stays where it was (S1 §3.2).
    assert_ne!(run["planner"], json!(lyra.id));
    assert_eq!(
        run["lead_note"],
        "主导是复制出来的，没带上原会话的上下文",
        "{run}"
    );
    let lead = agent(&t, run["planner"].as_str().unwrap()).await;
    assert!(
        lead["cwd"].as_str().unwrap().contains("integration"),
        "{lead}"
    );
    let me = agent(&t, "lyra").await;
    assert_eq!(me["cwd"], json!(lyra.cwd));

    let all: Value = t.get("/v1/questions?all=true").await.json().await.unwrap();
    let done = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == item["id"])
        .unwrap();
    assert_eq!(done["run_id"], run["id"]);
}

#[tokio::test]
async fn declining_starts_nothing() {
    let repo = git_repo();
    let t = TestHub::start().await;
    t.add_agent_in("lyra", Some("opus"), "worker", repo.path())
        .await;

    let item = propose(&t).await;
    let resp = t
        .post(
            &format!("/v1/questions/{}/answer", item["id"].as_str().unwrap()),
            json!({ "answer": "不用" }),
        )
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let runs: Value = t.get("/v1/runs").await.json().await.unwrap();
    assert_eq!(runs["runs"].as_array().unwrap().len(), 0, "{runs}");
}

async fn runs(t: &TestHub) -> Vec<Value> {
    let runs: Value = t.get("/v1/runs").await.json().await.unwrap();
    runs["runs"].as_array().unwrap().clone()
}

#[tokio::test]
async fn two_starts_at_once_open_one_work() {
    let repo = git_repo();
    let t = TestHub::start().await;
    t.add_agent_in("lyra", Some("opus"), "worker", repo.path())
        .await;

    let item = propose(&t).await;
    let path = format!("/v1/questions/{}/answer", item["id"].as_str().unwrap());
    let (a, b) = tokio::join!(
        t.post(&path, json!({ "answer": "开始" })),
        t.post(&path, json!({ "answer": "开始" })),
    );
    let mut codes = [a.status().as_u16(), b.status().as_u16()];
    codes.sort();
    assert_eq!(codes, [200, 409]);
    let late = if a.status() == 409 { a } else { b };
    let text = late.text().await.unwrap();
    assert!(text.contains("is already answered"), "{text}");
    assert_eq!(runs(&t).await.len(), 1);
}

#[tokio::test]
async fn a_dismissal_and_a_start_at_once_settle_one_way() {
    let repo = git_repo();
    let t = TestHub::start().await;
    t.add_agent_in("lyra", Some("opus"), "worker", repo.path())
        .await;

    let item = propose(&t).await;
    let id = item["id"].as_str().unwrap();
    let (answer, dismissal) = (
        format!("/v1/questions/{id}/answer"),
        format!("/v1/questions/{id}/dismiss"),
    );
    let (start, dismiss) = tokio::join!(
        t.post(&answer, json!({ "answer": "开始" })),
        t.post(&dismissal, json!({})),
    );
    let started = start.status() == 200;
    // Exactly one of them acted; the other was told it came too late.
    assert_eq!(
        (start.status().as_u16(), dismiss.status().as_u16()),
        if started { (200, 409) } else { (409, 204) }
    );
    let all: Value = t.get("/v1/questions?all=true").await.json().await.unwrap();
    let card = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == id)
        .cloned()
        .unwrap_or(Value::Null);
    let runs = runs(&t).await;
    if started {
        assert_eq!(runs.len(), 1);
        assert_eq!(card["status"], "answered", "{all}");
        assert_eq!(card["run_id"], runs[0]["id"]);
    } else {
        assert!(runs.is_empty(), "{runs:?}");
        assert_eq!(card["status"], "dismissed", "{all}");
    }
}

/// R1 §2.3: a lead whose run ended proposes another Work; it starts in the repository the
/// old run worked in, from its base branch, with a lead in its own checkout.
#[tokio::test]
async fn a_lead_whose_run_ended_starts_another_on_the_repository() {
    let repo = git_repo();
    let t = TestHub::start().await;
    t.add_agent_in("lyra", Some("opus"), "worker", repo.path())
        .await;

    let first = start(&t, &propose(&t).await).await;
    let old_lead = first["planner"].as_str().unwrap().to_owned();
    let old_cwd = agent(&t, &old_lead).await["cwd"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(old_cwd.contains("integration"), "{old_cwd}");

    // Ended, nothing merged, never landed. The lead is a copy made for the run (lyra cannot
    // move), so it stays in the old run's checkout until the run is deleted.
    let ended = finish(&t, &first).await;
    assert_eq!(ended["status"], "completed", "{ended}");
    assert!(ended["landed"].is_null(), "{ended}");
    assert_eq!(
        agent(&t, &old_lead).await["cwd"],
        json!(old_cwd),
        "the lead keeps its checkout"
    );

    // The lead can propose again, and the card says where the new Work starts.
    let mut w = t.watch().await;
    let proposal = call(
        "work_start",
        json!({ "request": "Add the Git view.", "title": "Git view" }),
    );
    t.send(&old_lead, &format!("Another one. {proposal}")).await;
    w.until(
        "card",
        |e| matches!(&e.kind, EventKind::HumanAsked { item } if item.text == "Git view"),
    )
    .await;
    let item = card(&t).await;
    assert_eq!(
        item["detail"],
        "Add the Git view.\n\n上一个 run 还没落地，新 Work 从 main 开始，不包含它的改动。"
    );

    let second = start(&t, &item).await;
    assert_ne!(second["id"], first["id"]);
    // The repository the old run worked in, not its worktree; main, not its branch.
    assert_eq!(second["repo"], json!(repo.path().display().to_string()));
    assert_eq!(second["base_branch"], "main");
    // What the person asked for is the brief, without the card's note.
    assert_eq!(second["request"], "Add the Git view.");
    // Its lead works in the new run's own checkout, next to the old one.
    let lead = agent(&t, second["planner"].as_str().unwrap()).await;
    let cwd = lead["cwd"].as_str().unwrap();
    assert!(cwd.contains("integration"), "{lead}");
    assert_ne!(cwd, old_cwd, "{lead}");
    assert!(
        cwd.contains(&short(second["id"].as_str().unwrap())),
        "the lead is not in run {}'s checkout: {lead}",
        second["id"]
    );
    assert!(std::path::Path::new(cwd).is_dir(), "{lead}");
}

/// R1 §2.3: while the run is open, the lead is refused, with the same words as before.
#[tokio::test]
async fn a_lead_whose_run_is_open_is_still_refused() {
    let repo = git_repo();
    let t = TestHub::start().await;
    t.add_agent_in("lyra", Some("opus"), "worker", repo.path())
        .await;

    let run = start(&t, &propose(&t).await).await;
    assert_eq!(run["status"], "planning");
    let lead = run["planner"].as_str().unwrap().to_owned();

    let mut w = t.watch().await;
    let proposal = call(
        "work_start",
        json!({ "request": "Something else.", "title": "Else" }),
    );
    t.send(&lead, &format!("Another one. {proposal}")).await;
    let refusal = format!(
        "you lead run {} already: work there (task_create, plan_submit) instead",
        run["id"].as_str().unwrap()
    );
    w.until(
        "refusal",
        |e| matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains(&refusal)),
    )
    .await;

    let all: Value = t.get("/v1/questions?all=true").await.json().await.unwrap();
    assert!(
        all["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["text"] != "Else"),
        "{all}"
    );
    let runs: Value = t.get("/v1/runs").await.json().await.unwrap();
    assert_eq!(runs["runs"].as_array().unwrap().len(), 1, "{runs}");
}

/// R1 §2.3: the same for a run the person starts from the home page, with the old lead as
/// its planner.
#[tokio::test]
async fn the_home_page_starts_a_run_for_a_lead_that_ended_one() {
    let repo = git_repo();
    let t = TestHub::start().await;
    t.add_agent_in("lyra", Some("opus"), "worker", repo.path())
        .await;

    let first = start(&t, &propose(&t).await).await;
    let old_lead = first["planner"].as_str().unwrap().to_owned();
    let ended = finish(&t, &first).await;
    assert_eq!(ended["status"], "completed", "{ended}");

    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": "Add the Git view.", "planner": old_lead }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let run: Value = resp.json().await.unwrap();
    assert_eq!(run["repo"], json!(repo.path().display().to_string()));
    assert_eq!(run["base_branch"], "main");
    // A copy leads it, in the new run's checkout.
    assert_ne!(run["planner"], json!(old_lead));
    let lead = agent(&t, run["planner"].as_str().unwrap()).await;
    let cwd = lead["cwd"].as_str().unwrap();
    assert!(
        cwd.contains(&short(run["id"].as_str().unwrap())),
        "the lead is not in run {}'s checkout: {lead}",
        run["id"]
    );
    assert!(std::path::Path::new(cwd).is_dir(), "{lead}");
}

/// S1 §3.2: a proposer whose driver resumes a session elsewhere leads its Work itself, with
/// this conversation, from the run's checkout; when the run ends it is back in the repository.
#[tokio::test]
async fn a_proposer_that_can_move_leads_the_work_itself() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    let lyra = t.add_native("lyra", "worker", repo.path()).await;
    let before: Value = t.get("/v1/agents").await.json().await.unwrap();

    let run = start(&t, &propose(&t).await).await;
    assert_eq!(run["planner"], json!(lyra.id), "{run}");
    assert!(run["lead_note"].is_null(), "{run}");
    let after: Value = t.get("/v1/agents").await.json().await.unwrap();
    assert_eq!(
        after["agents"].as_array().unwrap().len(),
        before["agents"].as_array().unwrap().len(),
        "no copy was made"
    );
    let lead = agent(&t, "lyra").await;
    let cwd = lead["cwd"].as_str().unwrap();
    assert!(
        cwd.contains("integration") && cwd.contains(&short(run["id"].as_str().unwrap())),
        "{lead}"
    );
    assert_eq!(lead["home"], json!(lyra.home), "{lead}");

    finish(&t, &run).await;
    let mut home = Value::Null;
    for _ in 0..100 {
        home = agent(&t, "lyra").await;
        if home["cwd"] == home["home"] {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(home["cwd"], home["home"], "{home}");
}
