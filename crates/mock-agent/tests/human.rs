//! H1-2 (docs/tasks/H1.md): questions for the person, the run policy's answer timeout, and
//! the budget pause, with mock agents driven by directives.

mod common;

use common::{TestHub, git_repo};
use crossagents_api_types::{EventKind, RunStatus};
use crossagents_core::{HumanItemStatus, MessageSource};
use serde_json::{Value, json};

async fn setup(repo: &std::path::Path) -> TestHub {
    let t = TestHub::start().await;
    t.add_agent_in("boss", Some("opus"), "planner", repo).await;
    t.add_agent_in("coder", Some("haiku"), "worker", repo).await;
    t
}

async fn start_run(t: &TestHub, request: &str, policy: Value) -> String {
    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": request, "planner": "boss", "worker": "coder", "policy": policy }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let run: Value = resp.json().await.unwrap();
    run["id"].as_str().unwrap().to_owned()
}

fn input_to(
    name_id: crossagents_core::AgentId,
    needle: &'static str,
) -> impl Fn(&crossagents_api_types::Event) -> bool {
    move |e| matches!(&e.kind, EventKind::AgentInput { id, text, .. } if *id == name_id && text.contains(needle))
}

#[tokio::test]
async fn questions_wait_for_the_answer() {
    let t = TestHub::start().await;
    let alice = t.add_agent("alice", None).await;
    let mut w = t.watch().await;
    t.send(
        "alice",
        r#"[call human_ask {"text":"Which database?","options":["postgres","sqlite"]}]"#,
    )
    .await;
    let asked = w
        .until("asked", |e| matches!(e.kind, EventKind::HumanAsked { .. }))
        .await;
    let EventKind::HumanAsked { item } = asked.kind else {
        unreachable!()
    };
    assert_eq!(item.timeout_at, None, "outside a run questions wait");
    let open: Value = t.get("/v1/questions").await.json().await.unwrap();
    assert_eq!(open["items"].as_array().unwrap().len(), 1);

    let resp = t
        .post(
            &format!("/v1/questions/{}/answer", item.id),
            json!({ "answer": "sqlite" }),
        )
        .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let answer = w
        .until("answer delivered", input_to(alice.id, "sqlite"))
        .await;
    let EventKind::AgentInput { from, .. } = answer.kind else {
        unreachable!()
    };
    assert_eq!(from, Some(MessageSource::Human));
    let open: Value = t.get("/v1/questions").await.json().await.unwrap();
    assert!(open["items"].as_array().unwrap().is_empty());
    // Answering twice is refused.
    let again = t
        .post(
            &format!("/v1/questions/{}/answer", item.id),
            json!({ "answer": "postgres" }),
        )
        .await;
    assert_eq!(again.status(), 409);
}

#[tokio::test]
async fn unanswered_questions_take_the_default_and_say_so() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let boss = t
        .get("/v1/agents/boss")
        .await
        .json::<Value>()
        .await
        .unwrap();
    let boss_id = boss["id"].as_str().unwrap().parse().unwrap();
    let mut w = t.watch().await;
    let run = start_run(
        &t,
        r#"Plan it. [call human_ask {"text":"Ship on Friday?","options":["yes","no"]}]"#,
        json!({ "human_ask": "timeout", "ask_timeout_minutes": 0.02 }),
    )
    .await;
    let defaulted = w
        .until("defaulted", |e| {
            matches!(&e.kind, EventKind::HumanAnswered { item } if item.status == HumanItemStatus::Defaulted)
        })
        .await;
    let EventKind::HumanAnswered { item } = defaulted.kind else {
        unreachable!()
    };
    assert_eq!(item.answer.as_deref(), Some("yes"));
    assert_eq!(item.run_id.map(|r| r.to_string()), Some(run));
    w.until(
        "boss told",
        input_to(boss_id, "go ahead with the default: yes"),
    )
    .await;

    // The person can still answer; the agent hears that they decided otherwise.
    let resp = t
        .post(
            &format!("/v1/questions/{}/answer", item.id),
            json!({ "answer": "no" }),
        )
        .await;
    assert_eq!(resp.status(), 200);
    w.until("late answer", input_to(boss_id, "has now answered"))
        .await;
}

#[tokio::test]
async fn deny_policy_refuses_questions() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    start_run(
        &t,
        r#"Plan it. [call human_ask {"text":"Anything?"}]"#,
        json!({ "human_ask": "deny" }),
    )
    .await;
    w.until("turn completed", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;
    let out = w.output();
    assert!(out.contains("decide yourself"), "{out}");
}

#[tokio::test]
async fn budget_pauses_the_run_until_the_person_decides() {
    let repo = git_repo();
    let t = setup(repo.path()).await;
    let mut w = t.watch().await;
    let plan = json!({ "tasks": [{
        "key": "work", "title": "some work",
        "spec": { "goal": "[call task_ask {\"task_id\":\"{task_id}\",\"question\":\"which way?\"}]", "acceptance": ["git status"] }
    }]});
    // The planner's first turn costs $0.01 (mock opus); the cap is half of that.
    let run = start_run(
        &t,
        &format!("Plan it. [call plan_submit {plan}]"),
        json!({ "plan_approval": "auto", "budget": { "max_usd": 0.005 } }),
    )
    .await;
    let paused = |e: &crossagents_api_types::Event| {
        matches!(
            &e.kind,
            EventKind::RunStatusChanged {
                to: RunStatus::Paused,
                ..
            }
        )
    };
    w.until("paused", paused).await;
    let asked = w
        .until("budget question", |e| {
            matches!(e.kind, EventKind::HumanAsked { .. })
        })
        .await;
    let EventKind::HumanAsked { item } = asked.kind else {
        unreachable!()
    };
    assert_eq!(item.options.len(), 2);

    t.post(
        &format!("/v1/questions/{}/answer", item.id),
        json!({ "answer": item.options[0] }),
    )
    .await;
    w.until("resumed", |e| {
        matches!(
            &e.kind,
            EventKind::RunStatusChanged {
                to: RunStatus::Running,
                from: RunStatus::Paused,
                ..
            }
        )
    })
    .await;
    let detail: Value = t
        .get(&format!("/v1/runs/{run}"))
        .await
        .json()
        .await
        .unwrap();
    let cap = detail["run"]["config"]["policy"]["budget"]["max_usd"]
        .as_f64()
        .unwrap();
    assert!((cap - 0.0075).abs() < 1e-9, "{cap}");
    t.post(&format!("/v1/runs/{run}/cancel"), json!({})).await;
}
