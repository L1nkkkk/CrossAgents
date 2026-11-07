//! Device credentials against a real hub, SQLite and HTTP streams.
use crossagents_api_types::{
    CreateDeviceResponse, Device, DeviceKind, DeviceLevel, ListDevicesResponse,
};
use crossagents_core::Store;
use crossagents_hub::{HubConfig, Paths, RunningHub};
use crossagents_store_sqlite::{SqliteStore, sqlx};
use futures::StreamExt;
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;

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
        let mut config = HubConfig::new(paths.clone(), 0);
        config.terminal_shell = Some(if cfg!(windows) {
            vec!["cmd.exe".into(), "/d".into()]
        } else {
            vec!["/bin/sh".into()]
        });
        let hub = crossagents_hub::start(config).await.unwrap();
        let base = format!("http://{}", hub.addr);
        Self {
            hub,
            client: Client::builder().no_proxy().build().unwrap(),
            base,
            paths,
            _home: home,
        }
    }
    fn request(&self, method: Method, path: &str, token: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(token)
    }
    async fn add(&self, name: &str, kind: &str, level: &str) -> CreateDeviceResponse {
        self.request(Method::POST, "/v1/devices", &self.hub.token)
            .json(&json!({"name":name,"kind":kind,"level":level}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn revoke(&self, id: &str) -> Device {
        self.request(
            Method::DELETE,
            &format!("/v1/devices/{id}"),
            &self.hub.token,
        )
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
    }
    async fn finish(self) {
        self.hub.shutdown();
        self.hub.wait().await.unwrap();
    }
}
#[tokio::test]
async fn issue_hash_only_defaults_names_and_idempotent_revoke() {
    let t = TestHub::start().await;
    let response = t
        .request(Method::POST, "/v1/devices", &t.hub.token)
        .json(&json!({"name":"laptop"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let created: CreateDeviceResponse = response.json().await.unwrap();
    assert_eq!(created.device.kind, DeviceKind::Client);
    assert_eq!(created.device.level, DeviceLevel::Write);
    assert_eq!(created.token.len(), 68);
    assert!(created.token.starts_with("cad_"));
    assert!(created.token[4..].chars().all(|c| c.is_ascii_hexdigit()));
    let store = SqliteStore::open(&t.paths.db()).await.unwrap();
    let hash: String = sqlx::query_scalar("SELECT token_hash FROM devices WHERE id = ?")
        .bind(&created.device.id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(hash, crossagents_hub::devices::hash(&created.token));
    assert_ne!(hash, created.token);
    assert_eq!(hash.len(), 64);
    // No plaintext token in any column of the credential table or the serialized API listing.
    let dump: String=sqlx::query_scalar("SELECT id || name || kind || level || token_hash || created_at || COALESCE(last_seen_at,'') || COALESCE(revoked_at,'') FROM devices WHERE id = ?").bind(&created.device.id).fetch_one(store.pool()).await.unwrap();
    assert!(!dump.contains(&created.token));
    let list: Value = t
        .request(Method::GET, "/v1/devices", &t.hub.read_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!list.to_string().contains(&created.token));
    assert!(list["devices"][0].get("token_hash").is_none());
    assert!(list["devices"][0].get("token").is_none());
    assert_eq!(
        t.request(Method::POST, "/v1/devices", &t.hub.token)
            .json(&json!({"name":"laptop"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    for name in ["".to_string(), "x".repeat(65)] {
        assert_eq!(
            t.request(Method::POST, "/v1/devices", &t.hub.token)
                .json(&json!({"name":name}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    t.add(&"设".repeat(64), "client", "read").await;
    let revoked = t.revoke(&created.device.id).await;
    assert!(revoked.revoked_at.is_some());
    assert_eq!(t.revoke(&created.device.id).await, revoked);
    t.add("laptop", "client", "read").await;
    assert_eq!(store.list_devices().await.unwrap().len(), 3);
    store.close().await;
    t.finish().await;
}
#[tokio::test]
async fn client_levels_follow_routes_and_device_management() {
    let t = TestHub::start().await;
    for (level, write, admin) in [
        ("read", false, false),
        ("write", true, false),
        ("admin", true, true),
    ] {
        let d = t.add(level, "client", level).await;
        for path in ["/v1/info", "/v1/devices", "/v1/agents"] {
            assert_eq!(
                t.request(Method::GET, path, &d.token)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::OK
            );
        }
        // This route is write, with no model invocation or other side effects.
        let res = t
            .request(Method::POST, "/v1/plugins/unknown/models", &d.token)
            .json(&json!({"shown":[]}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            if write {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::FORBIDDEN
            }
        );
        assert_eq!(
            t.request(Method::GET, "/v1/terminals", &d.token)
                .send()
                .await
                .unwrap()
                .status(),
            if admin {
                StatusCode::OK
            } else {
                StatusCode::FORBIDDEN
            }
        );
        let res = t
            .request(Method::POST, "/v1/devices", &d.token)
            .json(&json!({"name":format!("child-{level}")}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            if admin {
                StatusCode::OK
            } else {
                StatusCode::FORBIDDEN
            }
        );
        let res = t
            .request(Method::DELETE, "/v1/devices/unknown", &d.token)
            .send()
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            if admin {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::FORBIDDEN
            }
        );
        // Client write/admin can reach the human extension caller; an unknown tool is 404.
        let res = t
            .request(
                Method::POST,
                "/v1/extensions/unknown/tools/unknown",
                &d.token,
            )
            .json(&json!({"args":{}}))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status() == StatusCode::FORBIDDEN, !write);
    }
    assert_eq!(
        t.request(Method::GET, "/v1/devices", "cad_invalid")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    t.finish().await;
}
#[tokio::test]
async fn node_tokens_are_for_the_node_channel_only() {
    let t = TestHub::start().await;
    for level in ["read", "write", "admin"] {
        let d = t.add(level, "node", level).await;
        for (method, path) in [
            (Method::GET, "/v1/info"),
            (Method::GET, "/v1/devices"),
            (Method::POST, "/v1/devices"),
            (Method::DELETE, "/v1/devices/x"),
            (Method::GET, "/v1/events"),
            (Method::GET, "/v1/terminals"),
            (Method::POST, "/v1/extensions/x/tools/y"),
        ] {
            let res = t
                .request(method, path, &d.token)
                .json(&json!({"name":"bad"}))
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::FORBIDDEN);
            assert!(res.text().await.unwrap().contains("node channel (B1)"));
        }
        assert_eq!(
            t.request(Method::GET, "/v1/me", &d.token)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    t.finish().await;
}
#[tokio::test]
async fn last_seen_is_written_at_most_once_per_minute_and_again_after_a_minute() {
    let t = TestHub::start().await;
    let d = t.add("seen", "client", "read").await;
    let store = SqliteStore::open(&t.paths.db()).await.unwrap();
    assert!(
        store.list_devices().await.unwrap()[0]
            .last_seen_at
            .is_none()
    );
    t.request(Method::GET, "/v1/info", &d.token)
        .send()
        .await
        .unwrap();
    let first = store.list_devices().await.unwrap()[0].last_seen_at.unwrap();
    t.request(Method::GET, "/v1/info", &d.token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        store.list_devices().await.unwrap()[0].last_seen_at,
        Some(first)
    );
    // Independent cache (as after a hub restart): storage still enforces the minute limit.
    let cache = crossagents_hub::devices::Devices::default();
    cache.authenticate(&store, &d.token).await.unwrap().unwrap();
    assert_eq!(
        store.list_devices().await.unwrap()[0].last_seen_at,
        Some(first)
    );
    let old = (chrono::Utc::now() - chrono::Duration::minutes(2)).to_rfc3339();
    sqlx::query("UPDATE devices SET last_seen_at = ? WHERE id = ?")
        .bind(old)
        .bind(&d.device.id)
        .execute(store.pool())
        .await
        .unwrap();
    let cache = crossagents_hub::devices::Devices::default();
    cache.authenticate(&store, &d.token).await.unwrap().unwrap();
    assert!(store.list_devices().await.unwrap()[0].last_seen_at.unwrap() > first);
    store.close().await;
    t.finish().await;
}
#[tokio::test]
async fn revocation_closes_events_and_terminal_output_and_returns_401() {
    let t = TestHub::start().await;
    let d = t.add("streaming", "client", "admin").await;
    let other = t.add("other", "client", "read").await;
    let events = t
        .request(Method::GET, "/v1/events?live=true", &d.token)
        .send()
        .await
        .unwrap();
    assert_eq!(events.status(), StatusCode::OK);
    let independent = t
        .request(Method::GET, "/v1/events?live=true", &other.token)
        .send()
        .await
        .unwrap();
    let terminal: Value = t
        .request(Method::POST, "/v1/terminals", &d.token)
        .json(&json!({"cwd":t.paths.home.to_string_lossy(),"cols":80,"rows":24}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let output_path = format!(
        "/v1/terminals/{}/output?connection=device-test",
        terminal["id"].as_str().unwrap()
    );
    let output = t
        .request(Method::GET, &output_path, &d.token)
        .send()
        .await
        .unwrap();
    assert_eq!(output.status(), StatusCode::OK);
    let revoke_time = std::time::Instant::now();
    t.revoke(&d.device.id).await;
    for response in [events, output] {
        let mut stream = response.bytes_stream();
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(chunk) = stream.next().await {
                chunk.unwrap();
            }
        })
        .await
        .expect("revoked stream stayed open");
    }
    assert!(revoke_time.elapsed() < Duration::from_secs(5));
    for path in ["/v1/info", "/v1/events", &output_path] {
        assert_eq!(
            t.request(Method::GET, path, &d.token)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        t.request(Method::GET, "/v1/info", &other.token)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let mut independent = independent.bytes_stream();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), async {
            while independent.next().await.is_some() {}
        })
        .await
        .is_err()
    );
    drop(independent);
    t.finish().await;
}
#[tokio::test]
async fn device_credentials_survive_hub_restart() {
    let t = TestHub::start().await;
    let d = t.add("persistent", "client", "read").await;
    let id = d.device.id.clone();
    let paths = t.paths.clone();
    t.hub.shutdown();
    t.hub.wait().await.unwrap();
    let hub = crossagents_hub::start(HubConfig::new(paths, 0))
        .await
        .unwrap();
    let url = format!("http://{}/v1/devices", hub.addr);
    let list: ListDevicesResponse = t
        .client
        .get(url)
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.devices[0].id, id);
    hub.shutdown();
    hub.wait().await.unwrap();
}
