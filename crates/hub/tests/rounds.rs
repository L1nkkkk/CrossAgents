//! Round accounting through a running hub: tool calls over HTTP, and the notices a turn took,
//! driven through the runtime. External agents stand in for the CLI: the hub hands them their
//! messages when they read them (`take_external`), which starts and ends their turn.

use std::path::Path;
use std::time::Duration;

use crossagents_core::{AgentKey, EventKind, MessageSource, NoticeClass, RoundNotice, RoundSource};
use crossagents_hub::{HubConfig, Paths, RunningHub};
use reqwest::Client;
use serde_json::json;
use tokio::sync::broadcast::{Receiver, error::RecvError};

async fn start(home: &Path) -> RunningHub {
    crossagents_hub::start(HubConfig::new(Paths::new(home), 0))
        .await
        .unwrap()
}

async fn external_agent(
    hub: &RunningHub,
    client: &Client,
    name: &str,
    cwd: &Path,
) -> crossagents_core::Agent {
    client
        .post(format!("http://{}/v1/agents", hub.addr))
        .bearer_auth(&hub.token)
        .json(&json!({ "name": name, "plugin": "external", "role": "worker", "cwd": cwd }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// The first event from now on that `matches` accepts.
async fn next_event(
    rx: &mut Receiver<crossagents_core::Event>,
    what: &str,
    matches: impl Fn(&EventKind) -> bool,
) -> EventKind {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match rx.recv().await {
                Ok(event) if matches(&event.kind) => return event.kind,
                Ok(_) | Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => panic!("the bus closed while waiting for {what}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

#[tokio::test]
async fn tool_calls_record_whether_they_succeeded() {
    let home = tempfile::tempdir().unwrap();
    let hub = start(home.path()).await;
    let client = Client::new();
    let mut rx = hub.bus.subscribe();
    let agent = external_agent(&hub, &client, "kai", home.path()).await;
    let session = hub.agent_tokens.issue(agent.id).await;
    let base = format!("http://{}", hub.addr);

    let ok = client
        .post(format!("{base}/v1/me/tools/whoami"))
        .bearer_auth(&session)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert!(ok.status().is_success());
    let failed = client
        .post(format!("{base}/v1/me/tools/no_such_tool"))
        .bearer_auth(&session)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert!(!failed.status().is_success());

    let used = next_event(
        &mut rx,
        "whoami's event",
        |k| matches!(k, EventKind::AgentToolUsed { tool, .. } if tool == "whoami"),
    )
    .await;
    assert!(matches!(
        used,
        EventKind::AgentToolUsed { id, ref tool, ok: true } if id == agent.id && tool == "whoami"
    ));
    let used = next_event(
        &mut rx,
        "the failed call's event",
        |k| matches!(k, EventKind::AgentToolUsed { tool, .. } if tool == "no_such_tool"),
    )
    .await;
    assert!(matches!(
        used,
        EventKind::AgentToolUsed { id, ref tool, ok: false } if id == agent.id && tool == "no_such_tool"
    ));
}

#[tokio::test]
async fn a_wake_turn_records_the_notice_that_woke_it() {
    let home = tempfile::tempdir().unwrap();
    let hub = start(home.path()).await;
    let client = Client::new();
    let mut rx = hub.bus.subscribe();
    let agent = external_agent(&hub, &client, "kai", home.path()).await;
    let key = AgentKey::Id(agent.id);

    hub.runtime
        .notify(&key, "need", "choose a route", NoticeClass::Decide)
        .await
        .unwrap();
    let taken = hub.runtime.take_external(agent.id).await;
    assert_eq!(taken.len(), 1, "the wake waits for the agent to read it");

    let round = next_event(
        &mut rx,
        "the wake's round",
        |k| matches!(k, EventKind::AgentRound { id, .. } if *id == agent.id),
    )
    .await;
    let EventKind::AgentRound {
        sources,
        notices,
        tokens,
        ..
    } = round
    else {
        unreachable!()
    };
    assert_eq!(sources, vec![RoundSource::Notice]);
    assert_eq!(
        notices,
        vec![RoundNotice {
            line: "choose a route".into(),
            class: NoticeClass::Decide,
        }]
    );
    assert!(tokens.is_none(), "an external agent reports no usage");
}

#[tokio::test]
async fn a_wake_merged_while_it_waits_carries_both_notices() {
    let home = tempfile::tempdir().unwrap();
    let hub = start(home.path()).await;
    let client = Client::new();
    let mut rx = hub.bus.subscribe();
    let agent = external_agent(&hub, &client, "kai", home.path()).await;
    let key = AgentKey::Id(agent.id);

    hub.runtime
        .notify(&key, "need", "first question", NoticeClass::Decide)
        .await
        .unwrap();
    // The wake is still waiting: the second notice joins it instead of starting another.
    hub.runtime
        .notify(&key, "other", "second question", NoticeClass::Decide)
        .await
        .unwrap();
    let taken = hub.runtime.take_external(agent.id).await;
    assert_eq!(taken.len(), 1, "one wake, carrying both");

    let round = next_event(
        &mut rx,
        "the merged wake's round",
        |k| matches!(k, EventKind::AgentRound { id, .. } if *id == agent.id),
    )
    .await;
    let EventKind::AgentRound { notices, .. } = round else {
        unreachable!()
    };
    let lines: Vec<&str> = notices.iter().map(|n| n.line.as_str()).collect();
    assert_eq!(
        lines,
        vec!["first question", "second question"],
        "{notices:?}"
    );
    assert!(notices.iter().all(|n| n.class == NoticeClass::Decide));
}

#[tokio::test]
async fn a_persons_message_takes_its_notices_and_is_a_person_round() {
    let home = tempfile::tempdir().unwrap();
    let hub = start(home.path()).await;
    let client = Client::new();
    let mut rx = hub.bus.subscribe();
    let agent = external_agent(&hub, &client, "kai", home.path()).await;
    let key = AgentKey::Id(agent.id);

    // A batch notice does not wake the agent by itself; it waits for the next message.
    hub.runtime
        .notify(&key, "bob", "bob finished a turn", NoticeClass::Batch)
        .await
        .unwrap();
    hub.runtime
        .send(&key, "hello".into(), MessageSource::Human)
        .await
        .unwrap();
    let taken = hub.runtime.take_external(agent.id).await;
    assert_eq!(taken.len(), 1);
    let round = next_event(
        &mut rx,
        "the person's round",
        |k| matches!(k, EventKind::AgentRound { id, .. } if *id == agent.id),
    )
    .await;
    let EventKind::AgentRound {
        sources, notices, ..
    } = round
    else {
        unreachable!()
    };
    assert_eq!(sources, vec![RoundSource::Human]);
    assert_eq!(
        notices,
        vec![RoundNotice {
            line: "bob finished a turn".into(),
            class: NoticeClass::Batch,
        }]
    );
}
