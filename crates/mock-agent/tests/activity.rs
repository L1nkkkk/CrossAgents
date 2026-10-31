mod common;

use common::{TestHub, git_repo};
use crossagents_core::{EventKind, FileChange, ToolDetail, ToolStatus};
use serde_json::json;

#[tokio::test]
async fn activity_is_visible_during_a_turn_and_cleared_afterwards() {
    let t = TestHub::start().await;
    let worker = t.add_agent("worker", None).await;
    let reader = t.add_agent("reader", None).await;
    let mut w = t.watch().await;
    t.send("worker", "[slow]").await;
    w.until(
        "worker output",
        |e| matches!(&e.kind, EventKind::AgentOutput { id, .. } if *id == worker.id),
    )
    .await;
    t.hub
        .bus
        .publish(EventKind::AgentToolCall {
            id: worker.id,
            tool_call_id: "edit".into(),
            title: "edit".into(),
            kind: Some("edit".into()),
            status: ToolStatus::InProgress,
        })
        .await
        .unwrap();
    t.hub
        .bus
        .publish(EventKind::AgentToolDetail {
            id: worker.id,
            tool_call_id: "edit".into(),
            detail: ToolDetail {
                changes: ["a.rs", "a.rs", "b.rs"]
                    .into_iter()
                    .map(|path| FileChange {
                        path: path.into(),
                        kind: "update".into(),
                        diff: String::new(),
                    })
                    .collect(),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    t.send(
        "reader",
        r#"[call agent_get {"name":"worker"}] [call agent_list {}]"#,
    )
    .await;
    w.until(
        "reader done",
        |e| matches!(e.kind, EventKind::AgentTurnCompleted { id, .. } if id == reader.id),
    )
    .await;
    let out = w.output();
    assert!(
        out.contains("turn_elapsed_seconds") && out.contains("last_activity_ago_seconds"),
        "{out}"
    );
    assert!(
        out.contains("edit_files") && out.contains("\"files_touched\": 2"),
        "{out}"
    );
    assert!(out.matches("\"files_touched\": 2").count() >= 2, "{out}");
    t.post("/v1/agents/worker/cancel", json!({})).await;
    w.until(
        "worker done",
        |e| matches!(e.kind, EventKind::AgentTurnCompleted { id, .. } if id == worker.id),
    )
    .await;
    let mut after = t.watch().await;
    t.send("reader", r#"[call agent_get {"name":"worker"}]"#)
        .await;
    after
        .until(
            "reader done again",
            |e| matches!(e.kind, EventKind::AgentTurnCompleted { id, .. } if id == reader.id),
        )
        .await;
    assert!(
        after.output().contains("\"activity\": null"),
        "{}",
        after.output()
    );
}

#[tokio::test]
async fn closed_lead_is_directed_to_next_round_while_chat_can_propose_work() {
    let repo = git_repo();
    let t = TestHub::start().await;
    let lead = t.add_agent_in("Pia", None, "planner", repo.path()).await;
    let chat = t.add_agent_in("chat", None, "worker", repo.path()).await;
    let work: serde_json::Value = t
        .post("/v1/works", json!({"title":"rounds", "lead":"Pia"}))
        .await
        .json()
        .await
        .unwrap();
    let mut w = t.watch().await;
    let response = t
        .post(
            &format!("/v1/works/{}/runs", work["id"].as_str().unwrap()),
            json!({"request":"[call run_finish {\"summary\":\"done\"}]"}),
        )
        .await;
    assert_eq!(response.status(), 201);
    w.until(
        "turn ended",
        |e| matches!(e.kind, EventKind::AgentTurnCompleted { id, .. } if id == lead.id),
    )
    .await;
    let err = t
        .hub
        .orchestrator
        .call_tool(
            &t.agent("Pia").await,
            "work_start",
            json!({"request":"new goal"}),
        )
        .await
        .unwrap_err();
    assert!(matches!(&err, crossagents_core::CoreError::Conflict(_)));
    assert!(err.to_string().contains("run_next"), "{err}");
    assert!(err.to_string().contains("separate: true"), "{err}");
    assert!(
        t.hub
            .orchestrator
            .call_tool(&chat, "work_start", json!({"request":"new work"}))
            .await
            .is_ok()
    );
    // The way out the refusal names has to work, or it sends the lead round in circles: the
    // person really did ask for a Work of its own, and saying so gets past the check.
    assert!(
        t.hub
            .orchestrator
            .call_tool(
                &t.agent("Pia").await,
                "work_start",
                json!({"request":"an unrelated Work", "separate": true}),
            )
            .await
            .is_ok()
    );
    t.send(
        "Pia",
        r#"[call run_next {"title":"next", "request":"new goal"}]"#,
    )
    .await;
    w.until(
        "next round",
        |e| matches!(&e.kind, EventKind::RunCreated { run } if run.title == "next"),
    )
    .await;
}
