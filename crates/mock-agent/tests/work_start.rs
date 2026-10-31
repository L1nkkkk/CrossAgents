//! A conversation proposes a Work and the person starts or declines it (docs/tasks/W1.md §6),
//! and a lead whose run ended starts another one in the same repository (docs/tasks/R1.md §2).

mod common;

use common::{TestHub, git_repo, native_plugin};
use crossagents_api_types::{AgentStatus, EventKind};
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
    // The card goes up while the proposer's turn is still running. Answering it then is
    // refused — rightly: it is answering a person. Waiting for the turn to end makes
    // starting the Work deterministic, instead of failing under load.
    w.until("the proposer's turn ended", |e| {
        matches!(
            e.kind,
            EventKind::AgentStatusChanged {
                to: AgentStatus::Idle,
                ..
            }
        )
    })
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

/// Ends a run the person's way. The run lets go of its lead: a copy is archived once the
/// turn that closed the run ends (its checkout goes then), one that moved goes home.
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
        run["lead_note"], "主导是复制出来的，没带上原会话的上下文",
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

/// R1 §2.3 (session-first): after a child run ended unlanded, its source conversation proposes
/// another Work; it starts in the same repository, from its base branch, with a new lead.
#[tokio::test]
async fn a_chat_whose_child_ended_starts_another_on_the_repository() {
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

    // Ended, nothing merged, never landed: the copied lead and its checkout stay.
    let ended = finish(&t, &first).await;
    assert_eq!(ended["status"], "completed", "{ended}");
    assert!(ended["landed"].is_null(), "{ended}");
    let copy = agent(&t, &old_lead).await;
    assert!(
        copy["archived_at"].is_null(),
        "finish keeps the lead active"
    );
    assert!(std::path::Path::new(&old_cwd).is_dir());
    assert_eq!(copy["cwd"], json!(old_cwd), "{copy}");
    // The source conversation proposes again, and the card says where the new Work starts.
    let mut w = t.watch().await;
    let proposal = call(
        "work_start",
        json!({ "request": "Add the Git view.", "title": "Git view" }),
    );
    t.send("lyra", &format!("Another one. {proposal}")).await;
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
    assert_eq!(second["origin_chat_id"], first["origin_chat_id"]);
    assert_ne!(second["planner"], json!(old_lead));
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

/// Session-first: a child run's lead cannot propose Work (only conversations can), while its
/// source conversation can start another child alongside the open one.
#[tokio::test]
async fn only_the_source_conversation_proposes_work_while_a_child_is_open() {
    let repo = git_repo();
    let t = TestHub::start().await;
    t.add_agent_in("lyra", Some("opus"), "worker", repo.path())
        .await;

    let first = start(&t, &propose(&t).await).await;
    assert_eq!(first["status"], "planning");
    let lead = first["planner"].as_str().unwrap().to_owned();

    let mut w = t.watch().await;
    let proposal = call(
        "work_start",
        json!({ "request": "Something else.", "title": "Else" }),
    );
    t.send(&lead, &format!("Another one. {proposal}")).await;
    w.until(
        "refusal",
        |e| matches!(&e.kind, EventKind::AgentOutput { text, .. } if text.contains("work_start is not available to you")),
    )
    .await;
    let runs: Value = t.get("/v1/runs").await.json().await.unwrap();
    assert_eq!(runs["runs"].as_array().unwrap().len(), 1, "{runs}");

    let mut w = t.watch().await;
    t.send("lyra", &format!("Another one. {proposal}")).await;
    w.until(
        "card",
        |e| matches!(&e.kind, EventKind::HumanAsked { item } if item.text == "Else"),
    )
    .await;
    w.until("the proposer's turn ended", |e| {
        matches!(
            e.kind,
            EventKind::AgentStatusChanged {
                to: AgentStatus::Idle,
                ..
            }
        )
    })
    .await;
    let second = start(&t, &card(&t).await).await;
    assert_eq!(second["origin_chat_id"], first["origin_chat_id"]);
    assert_ne!(second["planner"], first["planner"]);
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

/// Even a movable conversation stays home: a fresh, independently named lead does the work.
#[tokio::test]
async fn a_movable_proposer_keeps_its_conversation_and_gets_a_fresh_lead() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    let lyra = t.add_native("lyra", "worker", repo.path()).await;
    let before: Value = t.get("/v1/agents").await.json().await.unwrap();

    let run = start(&t, &propose(&t).await).await;
    assert_ne!(run["planner"], json!(lyra.id), "{run}");
    assert_eq!(run["origin_chat_id"], json!(lyra.id));
    assert_eq!(run["config"]["policy"]["land"], "auto");
    assert_eq!(run["config"]["policy"]["plan_approval"], "auto");
    let after: Value = t.get("/v1/agents").await.json().await.unwrap();
    assert_eq!(
        after["agents"].as_array().unwrap().len(),
        before["agents"].as_array().unwrap().len() + 1,
        "one independent lead was made"
    );
    let lead = agent(&t, run["planner"].as_str().unwrap()).await;
    assert_ne!(lead["name"], "lyra");
    assert_eq!(lead["name_source"], "generated");
    assert_eq!(lead["config"]["spawned_by_run"], run["id"]);
    let parent = agent(&t, "lyra").await;
    assert_eq!(parent["cwd"], json!(lyra.cwd));
    assert!(t.hub.bus.store().claim_of(lyra.id).await.unwrap().is_none());
    let cwd = lead["cwd"].as_str().unwrap();
    assert!(
        cwd.contains("integration") && cwd.contains(&short(run["id"].as_str().unwrap())),
        "{lead}"
    );
    assert_eq!(lead["home"], json!(lyra.home), "{lead}");

    finish(&t, &run).await;
    let retained = agent(&t, run["planner"].as_str().unwrap()).await;
    assert_eq!(retained["cwd"], lead["cwd"], "finish keeps its directory");
    assert_eq!(retained["home"], json!(lyra.home));
    assert!(retained["archived_at"].is_null());
    assert!(std::path::Path::new(cwd).is_dir());
}

#[tokio::test]
async fn ending_a_child_keeps_its_summary_until_the_parent_gets_a_message() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    let parent = t.add_native("lyra", "worker", repo.path()).await;
    let item = propose(&t).await;
    let mut w = t.watch().await;
    let run = start(&t, &item).await;
    w.until("parent's startup acknowledgement", |e| {
        matches!(e.kind,
        EventKind::AgentTurnCompleted { id, .. } if id == parent.id)
    })
    .await;
    let events = t
        .hub
        .bus
        .store()
        .agent_events(parent.id, 0, 1000)
        .await
        .unwrap();
    let turns = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::AgentTurnStarted { .. }))
        .count();
    let completed = t
        .post(
            &format!("/v1/runs/{}/finish", run["id"].as_str().unwrap()),
            json!({"summary":"Recorded the agreed result."}),
        )
        .await;
    assert_eq!(
        completed.status(),
        200,
        "{}",
        completed.text().await.unwrap()
    );
    assert!(
        !t.hub.runtime.is_busy(parent.id).await,
        "the completion did not wake the parent"
    );
    let events = t
        .hub
        .bus
        .store()
        .agent_events(parent.id, 0, 1000)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::AgentTurnStarted { .. }))
            .count(),
        turns
    );

    let mut w = t.watch().await;
    t.send("lyra", "How did that work go?").await;
    w.until("next parent reply", |e| {
        matches!(e.kind,
        EventKind::AgentTurnCompleted { id, .. } if id == parent.id)
    })
    .await;
    let output = w.output();
    assert!(output.contains("CrossAgents work result:"), "{output}");
    assert!(output.contains("Recorded the agreed result."), "{output}");
    assert!(output.contains(run["id"].as_str().unwrap()), "{output}");
    assert!(output.contains("not landed"), "{output}");

    let mut w = t.watch().await;
    t.send("lyra", "Next topic.").await;
    w.until("another reply", |e| {
        matches!(e.kind,
        EventKind::AgentTurnCompleted { id, .. } if id == parent.id)
    })
    .await;
    assert!(
        !w.output().contains("CrossAgents work result:"),
        "the notice is only consumed once"
    );
}

#[tokio::test]
async fn a_child_cannot_borrow_its_source_conversation_as_a_worker() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    t.add_native("lyra", "worker", repo.path()).await;
    let run = start(&t, &propose(&t).await).await;
    let mut w = t.watch().await;
    t.send(
        run["planner"].as_str().unwrap(),
        &call(
            "plan_submit",
            json!({"tasks":[{
                "key":"borrow", "title":"borrow parent", "spec":{"goal":"do something"},
                "assignee":"lyra", "fresh":false
            }]}),
        ),
    )
    .await;
    w.until("source protection", |e| {
        matches!(&e.kind,
        EventKind::AgentOutput { text, .. } if text.contains("source conversation"))
    })
    .await;
    let parent = agent(&t, "lyra").await;
    assert_eq!(parent["cwd"], json!(repo.path()));
}

#[tokio::test]
async fn dirty_parent_checkout_refuses_landing_and_the_child_can_finish_on_its_branch() {
    for staged in [false, true] {
        let repo = git_repo();
        let t = TestHub::start_with(native_plugin).await;
        t.add_native("lyra", "worker", repo.path()).await;
        let run = start(&t, &propose(&t).await).await;
        let lead_id = run["planner"].as_str().unwrap();
        let lead = agent(&t, lead_id).await;
        let checkout = std::path::Path::new(lead["cwd"].as_str().unwrap());
        std::fs::write(checkout.join("README.md"), "child result\n").unwrap();
        std::fs::write(repo.path().join("README.md"), "parent's unfinished work\n").unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .unwrap();
            assert!(out.status.success());
            String::from_utf8(out.stdout).unwrap()
        };
        if staged {
            git(&["add", "README.md"]);
        }
        let head = git(&["rev-parse", "HEAD"]);
        let status = git(&["status", "--porcelain"]);
        let mut w = t.watch().await;
        t.send(
            lead_id,
            &format!(
                "{} {}",
                call("run_land", json!({})),
                call(
                    "run_finish",
                    json!({"summary":"Landing refused; result remains on the run branch."})
                )
            ),
        )
        .await;
        w.until("closed without landing", |e| {
            matches!(&e.kind,
            EventKind::RunStatusChanged { id, to: crossagents_api_types::RunStatus::Completed, .. }
                if id.to_string() == run["id"].as_str().unwrap())
        })
        .await;
        w.until("lead's final reply", |e| {
            matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if id.to_string() == lead_id)
        })
        .await;
        assert!(w.output().contains("Call run_finish"), "{}", w.output());
        assert!(
            w.output().contains("Do not retry landing"),
            "{}",
            w.output()
        );
        let detail: Value = t
            .get(&format!("/v1/runs/{}", run["id"].as_str().unwrap()))
            .await
            .json()
            .await
            .unwrap();
        assert!(detail["run"]["landed"].is_null());
        assert_eq!(
            std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
            "parent's unfinished work\n"
        );
        assert_eq!(git(&["rev-parse", "HEAD"]), head);
        assert_eq!(git(&["status", "--porcelain"]), status);
        assert!(git(&["stash", "list"]).is_empty());
        assert!(checkout.exists(), "the result's checkout stays available");
    }
}

#[tokio::test]
async fn a_solo_child_can_land_before_finishing() {
    let repo = git_repo();
    let t = TestHub::start_with(native_plugin).await;
    t.add_native("lyra", "worker", repo.path()).await;
    let run = start(&t, &propose(&t).await).await;
    let lead_id = run["planner"].as_str().unwrap();
    let lead = agent(&t, lead_id).await;
    std::fs::write(
        std::path::Path::new(lead["cwd"].as_str().unwrap()).join("README.md"),
        "solo result\n",
    )
    .unwrap();
    let mut w = t.watch().await;
    t.send(
        lead_id,
        &format!(
            "{} {}",
            call("run_land", json!({})),
            call("run_finish", json!({"summary":"Landed the solo result."}))
        ),
    )
    .await;
    w.until("solo run completed", |e| {
        matches!(&e.kind,
        EventKind::RunStatusChanged { id, to: crossagents_api_types::RunStatus::Completed, .. }
            if id.to_string() == run["id"].as_str().unwrap())
    })
    .await;
    let detail: Value = t
        .get(&format!("/v1/runs/{}", run["id"].as_str().unwrap()))
        .await
        .json()
        .await
        .unwrap();
    assert!(detail["run"]["landed"].is_object(), "{detail}");
    assert!(detail["tasks"].as_array().unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md"))
            .unwrap()
            .replace("\r\n", "\n"),
        "solo result\n"
    );
}

/// SESSION-FIRST §4.2: the source is on the run from its first record, so the first
/// `RunCreated` already names it; nothing writes it in afterwards.
#[tokio::test]
async fn the_first_run_created_event_names_the_source_chat() {
    let repo = git_repo();
    let t = TestHub::start().await;
    let lyra = t
        .add_agent_in("lyra", Some("opus"), "worker", repo.path())
        .await;

    let item = propose(&t).await;
    let mut w = t.watch().await;
    let run = start(&t, &item).await;
    let created = w
        .find("run created", |e| {
            matches!(&e.kind, EventKind::RunCreated { run: r } if r.id.to_string() == run["id"])
        })
        .await;
    let EventKind::RunCreated { run: created } = created.kind else {
        unreachable!()
    };
    assert_eq!(created.origin_chat_id, Some(lyra.id));
    assert_ne!(created.planner, lyra.id);
}

/// SESSION-FIRST §9: the source is kept with the run, so a restarted hub still knows it.
#[tokio::test]
async fn the_source_chat_survives_a_hub_restart() {
    let repo = git_repo();
    let t = TestHub::start().await;
    let lyra = t
        .add_agent_in("lyra", Some("opus"), "worker", repo.path())
        .await;
    let run = start(&t, &propose(&t).await).await;
    let id = run["id"].as_str().unwrap().to_owned();

    let t = TestHub::start_in(t.restart_home().await, |_| {}).await;
    let detail: Value = t.get(&format!("/v1/runs/{id}")).await.json().await.unwrap();
    assert_eq!(detail["run"]["origin_chat_id"], json!(lyra.id), "{detail}");
    assert_eq!(detail["run"]["planner"], run["planner"], "{detail}");
    let runs: Value = t.get("/v1/runs").await.json().await.unwrap();
    assert!(
        runs["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == json!(id) && r["origin_chat_id"] == json!(lyra.id)),
        "{runs}"
    );
}

/// SESSION-FIRST §5.2: deleting a child run leaves its source conversation as it was.
#[tokio::test]
async fn deleting_a_child_run_keeps_its_source_chat() {
    let repo = git_repo();
    let t = TestHub::start().await;
    let lyra = t
        .add_agent_in("lyra", Some("opus"), "worker", repo.path())
        .await;
    let run = start(&t, &propose(&t).await).await;
    let id = run["id"].as_str().unwrap();

    let resp = t.post(&format!("/v1/runs/{id}/cancel"), json!({})).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let resp = t.delete(&format!("/v1/runs/{id}")).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());

    let chat = agent(&t, &lyra.id.to_string()).await;
    assert!(chat["archived_at"].is_null(), "{chat}");
    assert_eq!(chat["cwd"], json!(lyra.cwd), "{chat}");
    // Its conversation is still there and still answers.
    let mut w = t.watch().await;
    t.send("lyra", "Still here?").await;
    w.until(
        "the source chat answered",
        |e| matches!(&e.kind, EventKind::AgentTurnCompleted { id, .. } if *id == lyra.id),
    )
    .await;
}
