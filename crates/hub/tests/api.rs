//! Integration tests of the local API against a real hub on a random port.

use std::time::Duration;

use crossagents_api_types::{Agent, ListAgentsResponse};
use crossagents_hub::{HubConfig, Paths, RunningHub};
use futures::StreamExt;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};

struct TestHub {
    hub: RunningHub,
    client: Client,
    base: String,
    paths: Paths,
    _home: tempfile::TempDir,
}

impl TestHub {
    async fn start() -> Self {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let hub = crossagents_hub::start(HubConfig::new(paths.clone(), 0))
            .await
            .unwrap();
        let base = format!("http://{}", hub.addr);
        Self {
            hub,
            client: Client::new(),
            base,
            paths,
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

    async fn create(&self, name: &str) -> reqwest::Response {
        let cwd = self._home.path().display().to_string();
        self.post("/v1/agents")
            .json(&json!({ "name": name, "plugin": "claude", "model": "haiku", "role": "worker", "cwd": cwd }))
            .send()
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn health_is_public_and_everything_else_needs_the_token() {
    let t = TestHub::start().await;
    let health: Value = t
        .client
        .get(t.url("/v1/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["status"], "ok");

    let anon = t.client.get(t.url("/v1/agents")).send().await.unwrap();
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);
    let body: Value = anon.json().await.unwrap();
    assert_eq!(body["error"]["code"], "unauthorized");

    let wrong = t
        .client
        .get(t.url("/v1/agents"))
        .bearer_auth("nope")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    assert_eq!(
        t.get("/v1/info").send().await.unwrap().status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn agent_lifecycle() {
    let t = TestHub::start().await;
    let created = t.create("alice").await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let alice: Agent = created.json().await.unwrap();
    assert_eq!(alice.name, "alice");

    // A name a person typed is theirs: taken (ignoring case) is a conflict, not "alice 2".
    assert_eq!(t.create("alice").await.status(), StatusCode::CONFLICT);
    assert_eq!(t.create("Alice").await.status(), StatusCode::CONFLICT);
    assert_eq!(t.create("Bad/Name").await.status(), StatusCode::BAD_REQUEST);

    let list: ListAgentsResponse = t
        .get("/v1/agents")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.agents, vec![alice.clone()]);

    let by_name: Agent = t
        .get("/v1/agents/alice")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let by_id: Agent = t
        .get(&format!("/v1/agents/{}", alice.id))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(by_name, alice);
    assert_eq!(by_id, alice);

    let deleted = t.delete("/v1/agents/alice").send().await.unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    let missing = t.get("/v1/agents/alice").send().await.unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn names_are_for_people() {
    let t = TestHub::start().await;
    assert_eq!(t.create("alice").await.status(), StatusCode::CREATED);
    // Names are for people: spaces and any script are fine; the address is the id.
    assert_eq!(t.create("Nova 2").await.status(), StatusCode::CREATED);
    // No name: the hub makes one up, and says so.
    let made: Value = t
        .post("/v1/agents")
        .json(&json!({ "plugin": "claude", "role": "worker", "cwd": "" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(made["name_source"], "generated", "{made}");
    assert!(
        !crossagents_core::names::is_vendor_word(made["name"].as_str().unwrap(), &[]),
        "{made}"
    );
}

#[tokio::test]
async fn bad_cwd_is_rejected() {
    let t = TestHub::start().await;
    let resp = t
        .post("/v1/agents")
        .json(&json!({ "name": "a", "plugin": "claude", "role": "worker", "cwd": "/definitely/not/here" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_agent_without_cwd_gets_a_chat_directory() {
    let t = TestHub::start().await;
    let resp = t
        .post("/v1/agents")
        .json(&json!({ "name": "Kai", "topic": "写个冒泡排序", "plugin": "claude", "role": "worker", "cwd": "" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let agent: Value = resp.json().await.unwrap();
    let cwd = std::path::PathBuf::from(agent["cwd"].as_str().unwrap());
    assert!(cwd.is_dir());
    // By id: the name can change.
    let id = agent["id"].as_str().unwrap();
    assert!(cwd.ends_with(format!("chats/{id}")), "{}", cwd.display());
    assert_eq!(agent["topic"], "写个冒泡排序");
}

#[tokio::test]
async fn model_and_effort_are_set_together_and_effort_can_be_cleared() {
    let t = TestHub::start().await;
    assert_eq!(t.create("a").await.status(), StatusCode::CREATED);
    let set = |body: Value| t.post("/v1/agents/a/model").json(&body).send();

    let agent: Value = set(json!({ "model": "opus", "effort": "high" }))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(agent["model"], "opus");
    assert_eq!(agent["config"]["effort"], "high");

    // No effort: the model changes, the effort stays.
    let agent: Value = set(json!({ "model": "sonnet" }))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(agent["model"], "sonnet");
    assert_eq!(agent["config"]["effort"], "high");

    // Empty effort: back to the model's default, also after a reload.
    set(json!({ "model": "sonnet", "effort": "" }))
        .await
        .unwrap();
    let agent: Value = t
        .get("/v1/agents/a")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        agent["config"].get("effort").is_none(),
        "{}",
        agent["config"]
    );
}

/// Reads SSE `data:` payloads until `n` events have arrived.
async fn read_events(resp: reqwest::Response, n: usize) -> Vec<Value> {
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut events = Vec::new();
    while events.len() < n {
        let chunk = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("timed out waiting for events")
            .expect("stream ended")
            .unwrap();
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(end) = buf.find("\n\n") {
            let block: String = buf.drain(..end + 2).collect();
            for line in block.lines() {
                if let Some(data) = line.strip_prefix("data: ").or(line.strip_prefix("data:")) {
                    events.push(serde_json::from_str(data).unwrap());
                }
            }
        }
    }
    events
}

#[tokio::test]
async fn events_replay_history_then_follow_live() {
    let t = TestHub::start().await;
    t.create("alice").await;

    let resp = t.get("/v1/events?since=0").send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let reader = tokio::spawn(read_events(resp, 3));

    // Give the stream time to replay history before producing a live event.
    tokio::time::sleep(Duration::from_millis(100)).await;
    t.delete("/v1/agents/alice").send().await.unwrap();

    let events = reader.await.unwrap();
    let types: Vec<_> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(types, ["hub.started", "agent.created", "agent.removed"]);
    let seqs: Vec<_> = events.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]));

    // Resuming after the first event skips it.
    let resumed = t
        .get("/v1/events")
        .header("Last-Event-ID", seqs[0].to_string())
        .send()
        .await
        .unwrap();
    let again = read_events(resumed, 2).await;
    assert_eq!(again[0]["seq"].as_u64().unwrap(), seqs[1]);
}

#[tokio::test]
async fn second_hub_on_same_home_fails() {
    let t = TestHub::start().await;
    let second = crossagents_hub::start(HubConfig::new(t.paths.clone(), 0)).await;
    let err = second.err().expect("second hub must not start");
    assert!(err.to_string().contains("already running"), "{err}");
}

#[tokio::test]
async fn shutdown_over_api_stops_hub_and_removes_hub_file() {
    let t = TestHub::start().await;
    assert!(t.paths.hub_file().exists());
    // An open event stream must not block shutdown.
    let _stream = t.get("/v1/events").send().await.unwrap();

    let resp = t.post("/v1/shutdown").send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    tokio::time::timeout(Duration::from_secs(5), t.hub.wait())
        .await
        .expect("hub did not stop")
        .unwrap();
    assert!(!t.paths.hub_file().exists());
}

/// docs/API-AUTH.md §3, docs/tasks/DOGFOOD.md §7: a valid credential below the route's level
/// gets 403 naming the level it lacks; an invalid one still 401.
#[tokio::test]
async fn credentials_have_levels() {
    let t = TestHub::start().await;
    let read = &t.hub.read_token;
    assert_eq!(
        std::fs::read_to_string(t.paths.token_read())
            .unwrap()
            .trim(),
        read
    );
    let as_read = |method: reqwest::Method, path: &str| {
        t.client.request(method, t.url(path)).bearer_auth(read)
    };
    assert_eq!(
        as_read(reqwest::Method::GET, "/v1/agents")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let resp = as_read(reqwest::Method::POST, "/v1/runs")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "forbidden");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("needs write")
    );
    let resp = as_read(reqwest::Method::POST, "/v1/shutdown")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body: Value = resp.json().await.unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("needs admin")
    );

    // An agent's session token reaches /v1/me only.
    let cwd = t.paths.home.display().to_string();
    let resp = t
        .post("/v1/agents")
        .json(&json!({ "name": "outside", "plugin": "external", "role": "worker", "cwd": cwd }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let info: Value = t
        .post("/v1/agents/outside/connect")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session = info["token"].as_str().unwrap();
    let as_agent = |path: &str| t.client.get(t.url(path)).bearer_auth(session).send();
    assert_eq!(as_agent("/v1/me").await.unwrap().status(), 200);
    assert_eq!(
        as_agent("/v1/agents").await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let resp = t
        .client
        .get(t.url("/v1/agents"))
        .bearer_auth(format!("ca-agent-{}", "0".repeat(64)))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "unknown tokens are not valid"
    );

    // The full token still does everything.
    assert_eq!(t.get("/v1/agents").send().await.unwrap().status(), 200);
}
