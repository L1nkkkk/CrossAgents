//! A run's trace (docs/tasks/DOGFOOD.md §8): which task failed where, from the events, by
//! the API and from the event log on disk.

mod common;

use common::{TestHub, git_repo};
use crossagents_api_types::EventKind;
use serde_json::{Value, json};

#[tokio::test]
async fn a_failed_check_is_traced_to_its_task() {
    let repo = git_repo();
    let t = TestHub::start_with(|config| config.event_log = true).await;
    t.add_agent_in("boss", Some("opus"), "planner", repo.path())
        .await;
    t.add_agent_in("coder", Some("haiku"), "worker", repo.path())
        .await;
    let mut w = t.watch().await;

    let goal = format!(
        "[write a.txt=alpha] [call task_complete {}]",
        json!({ "task_id": "{task_id}", "summary": "wrote a" })
    );
    let plan = json!([{ "key": "a", "title": "task a",
        "spec": { "goal": goal, "acceptance": ["git grep -q omega -- a.txt"] } }]);
    let req = format!("Build it. [call plan_submit {}]", json!({ "tasks": plan }));
    let resp = t
        .post(
            "/v1/runs",
            json!({ "request": req, "planner": "boss", "worker": "coder", "policy": { "plan_approval": "auto" } }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let id = resp.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    w.until(
        "a checked",
        |e| matches!(&e.kind, EventKind::TaskChecked { key, .. } if key == "a"),
    )
    .await;

    let trace: Value = t
        .get(&format!("/v1/runs/{id}/trace"))
        .await
        .json()
        .await
        .unwrap();
    let a = &trace["tasks"][0];
    assert_eq!(a["key"], "a");
    assert_eq!(a["title"], "task a");
    assert_eq!(a["checks"][0]["passed"], false);
    let first = a["first_failure"].as_str().unwrap();
    assert!(first.contains("git grep -q omega"), "{first}");
    assert!(!a["lane"]["turns"].as_array().unwrap().is_empty(), "{a}");
    assert!(
        !trace["lead"]["turns"].as_array().unwrap().is_empty(),
        "{trace}"
    );

    // The same story from the log on disk, with no hub to ask.
    let events = crossagents_hub::trace::read_log(&t.paths.logs()).unwrap();
    let mut tracer = crossagents_hub::trace::Tracer::new(id.parse().unwrap());
    for event in &events {
        tracer.push(event);
    }
    let from_log = tracer.finish();
    assert_eq!(from_log.tasks[0].key, "a");
    assert_eq!(
        from_log.tasks[0].first_failure.as_deref(),
        Some(first),
        "{from_log:?}"
    );
}
