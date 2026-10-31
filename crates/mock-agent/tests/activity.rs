mod common;

use common::TestHub;
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
        out.contains("修改文件") && out.contains("\"files_touched\": 2"),
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
