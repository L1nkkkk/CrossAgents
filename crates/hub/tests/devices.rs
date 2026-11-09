//! Device credentials against a real hub, SQLite and HTTP streams.
use crossagents_api_types::{
    CreateDeviceResponse, Device, DeviceKind, DeviceLevel, DeviceOs, ListDevicesResponse,
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
    for name in [
        "".to_string(),
        "   ".to_string(),
        "bad\nname".to_string(),
        "x".repeat(65),
    ] {
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
    let events = store.events_since(0, 1000).await.unwrap();
    assert_eq!(events.iter().filter(|event| matches!(&event.kind, crossagents_core::EventKind::DeviceRevoked { device } if device.id == created.device.id)).count(), 1);
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
async fn client_os_header_is_recorded_only_when_it_changes() {
    let t = TestHub::start().await;
    let d = t.add("laptop", "client", "read").await;
    assert!(d.device.os.is_none());
    let store = SqliteStore::open(&t.paths.db()).await.unwrap();
    let os_on_file = || async {
        sqlx::query_scalar::<_, Option<String>>("SELECT os FROM devices WHERE id = ?")
            .bind(&d.device.id)
            .fetch_one(store.pool())
            .await
            .unwrap()
    };
    let info = |os: Option<&str>| {
        let mut req = t.request(Method::GET, "/v1/info", &d.token);
        if let Some(os) = os {
            req = req.header("x-crossagents-client-os", os);
        }
        async move { assert!(req.send().await.unwrap().status().is_success()) }
    };
    // Missing or empty: nothing recorded.
    info(None).await;
    info(Some("")).await;
    assert_eq!(os_on_file().await, None);
    info(Some("macos")).await;
    assert_eq!(os_on_file().await.as_deref(), Some("macos"));
    let listed: ListDevicesResponse = t
        .request(Method::GET, "/v1/devices", &t.hub.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.devices[0].os, Some(DeviceOs::Macos));
    // A request without the header leaves it as it was.
    info(None).await;
    assert_eq!(os_on_file().await.as_deref(), Some("macos"));
    // The same report does not write: change the row behind the hub's back, report again.
    sqlx::query("UPDATE devices SET os = 'linux' WHERE id = ?")
        .bind(&d.device.id)
        .execute(store.pool())
        .await
        .unwrap();
    info(Some("macos")).await;
    assert_eq!(os_on_file().await.as_deref(), Some("linux"));
    // A different report writes, while the device is still cached; unknown words are other.
    info(Some("windows")).await;
    assert_eq!(os_on_file().await.as_deref(), Some("windows"));
    info(Some("plan9")).await;
    assert_eq!(os_on_file().await.as_deref(), Some("other"));
    // The cache followed the write: reporting other again does not write.
    sqlx::query("UPDATE devices SET os = 'linux' WHERE id = ?")
        .bind(&d.device.id)
        .execute(store.pool())
        .await
        .unwrap();
    info(Some("haiku")).await;
    assert_eq!(os_on_file().await.as_deref(), Some("linux"));
    store.close().await;
    t.finish().await;
}
#[tokio::test]
async fn info_reports_the_hub_machine() {
    let t = TestHub::start().await;
    let info: Value = t
        .request(Method::GET, "/v1/info", &t.hub.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let expected = serde_json::to_value(crossagents_core::DeviceOs::current()).unwrap();
    assert_eq!(info["machine"]["os"], expected);
    assert!(info["machine"]["hostname"].is_string());
    t.finish().await;
}
#[tokio::test]
async fn issued_device_may_carry_its_os() {
    let t = TestHub::start().await;
    let created: CreateDeviceResponse = t
        .request(Method::POST, "/v1/devices", &t.hub.token)
        .json(&json!({"name": "mac", "os": "macos"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(created.device.os, Some(DeviceOs::Macos));
    let store = SqliteStore::open(&t.paths.db()).await.unwrap();
    assert_eq!(
        store.list_devices().await.unwrap()[0].os,
        Some(DeviceOs::Macos)
    );
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
    let remaining: Value = t
        .request(Method::GET, "/v1/terminals", &t.hub.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !remaining["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == terminal["id"])
    );
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

#[tokio::test]
async fn device_events_contain_only_public_data_and_names_are_trimmed() {
    let t = TestHub::start().await;
    let d = t.add("  event-device  ", "client", "admin").await;
    assert_eq!(d.device.name, "event-device");
    t.revoke(&d.device.id).await;
    let store = SqliteStore::open(&t.paths.db()).await.unwrap();
    let events = store.events_since(0, 1000).await.unwrap();
    for name in ["device.created", "device.revoked"] {
        let event = events.iter().find(|e| e.kind.type_name() == name).unwrap();
        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["data"]["device"]["id"], d.device.id);
        assert!(!value.to_string().contains(&d.token));
        assert!(value["data"]["device"].get("token_hash").is_none());
    }
    t.finish().await;
}

#[tokio::test]
async fn cached_device_is_reloaded_after_expiry() {
    let t = TestHub::start().await;
    let d = t.add("refresh", "client", "read").await;
    let store = SqliteStore::open(&t.paths.db()).await.unwrap();
    let cache = crossagents_hub::devices::Devices::with_ttl(Duration::from_millis(20));
    assert_eq!(
        cache
            .authenticate(&store, &d.token)
            .await
            .unwrap()
            .unwrap()
            .0
            .level,
        DeviceLevel::Read
    );
    sqlx::query("UPDATE devices SET level = 'admin' WHERE id = ?")
        .bind(&d.device.id)
        .execute(store.pool())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        cache
            .authenticate(&store, &d.token)
            .await
            .unwrap()
            .unwrap()
            .0
            .level,
        DeviceLevel::Admin
    );
    store.revoke_device(&d.device.id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        cache
            .authenticate(&store, &d.token)
            .await
            .unwrap()
            .is_none()
    );
    t.finish().await;
}
#[tokio::test]
async fn sqlite_device_conflicts_and_timestamp_comparison() {
    let t = TestHub::start().await;
    let d = t.add("conflict", "client", "read").await;
    let store = SqliteStore::open(&t.paths.db()).await.unwrap();
    let hash = crossagents_hub::devices::hash(&d.token);
    let mut other = d.device.clone();
    other.id = uuid::Uuid::new_v4().to_string();
    let name_error = store
        .insert_device(&other, "another-hash")
        .await
        .unwrap_err()
        .to_string();
    assert!(name_error.contains("name"), "{name_error}");
    other.name = "another-name".into();
    let hash_error = store
        .insert_device(&other, &hash)
        .await
        .unwrap_err()
        .to_string();
    assert!(hash_error.contains("hash"), "{hash_error}");
    sqlx::query("UPDATE devices SET last_seen_at = ? WHERE id = ?")
        .bind(
            (chrono::Utc::now() - chrono::Duration::minutes(2))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        )
        .bind(&d.device.id)
        .execute(store.pool())
        .await
        .unwrap();
    store.touch_device(&d.device.id).await.unwrap();
    let seen = store
        .list_devices()
        .await
        .unwrap()
        .into_iter()
        .find(|x| x.id == d.device.id)
        .unwrap()
        .last_seen_at
        .unwrap();
    assert!(seen > chrono::Utc::now() - chrono::Duration::seconds(30));
    t.finish().await;
}

#[tokio::test]
async fn concurrent_first_authentication_and_revoke_never_repopulate_the_cache() {
    let t = TestHub::start().await;
    let d = t.add("race", "client", "read").await;
    let store = SqliteStore::open(&t.paths.db()).await.unwrap();
    let cache = crossagents_hub::devices::Devices::default();
    let (auth, revoked) = tokio::join!(
        cache.authenticate(&store, &d.token),
        cache.revoke(&store, &d.device.id)
    );
    if let Some((_, cancel)) = auth.unwrap() {
        assert!(cancel.is_cancelled());
    }
    revoked.unwrap();
    assert!(
        cache
            .authenticate(&store, &d.token)
            .await
            .unwrap()
            .is_none()
    );
    t.finish().await;
}

#[tokio::test]
async fn device_requests_log_only_admin_routes_at_info() {
    use std::sync::{Arc, Mutex};
    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<(tracing::Level, String)>>>);
    struct Fields(String);
    impl tracing::field::Visit for Fields {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            use std::fmt::Write;
            let _ = write!(self.0, "{}={:?} ", field.name(), value);
        }
    }
    impl tracing::Subscriber for Capture {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut fields = Fields(String::new());
            event.record(&mut fields);
            self.0
                .lock()
                .unwrap()
                .push((*event.metadata().level(), fields.0));
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }
    let logs = Arc::new(Mutex::new(Vec::new()));
    tracing::subscriber::set_global_default(Capture(logs.clone())).unwrap();
    let t = TestHub::start().await;
    let d = t.add("log-test-unique", "client", "admin").await;
    t.request(Method::GET, "/v1/info", &d.token)
        .send()
        .await
        .unwrap();
    t.request(Method::POST, "/v1/devices", &d.token)
        .json(&json!({"name":"log-child"}))
        .send()
        .await
        .unwrap();
    let logs = logs.lock().unwrap().clone();
    let lines: Vec<_> = logs
        .iter()
        .filter(|(_, line)| line.contains("device request") && line.contains("log-test-unique"))
        .collect();
    assert!(
        lines
            .iter()
            .any(|(level, line)| *level == tracing::Level::DEBUG && line.contains("/v1/info")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|(level, line)| *level == tracing::Level::INFO && line.contains("/v1/devices")),
        "{lines:?}"
    );
    t.finish().await;
}
