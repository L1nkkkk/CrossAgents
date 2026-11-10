//! The built-in hole-punching entrance against a real hub (docs/MULTI-NODE.md §14「内置打洞」).
use crossagents_api_types::{CreateDeviceResponse, LinkInvite, LinkState, LinkStatus};
use crossagents_hub::{HubConfig, Paths, RunningHub};
use crossagents_link::{Event, Invite};
use reqwest::{Client, Method, StatusCode};
use serde_json::json;
use std::time::Duration;

struct TestHub {
    hub: RunningHub,
    client: Client,
    base: String,
    _home: tempfile::TempDir,
}

impl TestHub {
    async fn start() -> Self {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let hub = crossagents_hub::start(HubConfig::new(paths, 0))
            .await
            .unwrap();
        let base = format!("http://{}", hub.addr);
        Self {
            hub,
            client: Client::builder().no_proxy().build().unwrap(),
            base,
            _home: home,
        }
    }

    fn request(&self, method: Method, path: &str, token: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(token)
    }

    async fn device(&self, level: &str) -> CreateDeviceResponse {
        self.request(Method::POST, "/v1/devices", &self.hub.token)
            .json(&json!({ "name": format!("{level}-gui"), "kind": "client", "level": level }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn status(&self) -> LinkStatus {
        self.request(Method::GET, "/v1/link", &self.hub.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn the_entrance_is_admin_and_codes_name_the_hub() {
    let t = TestHub::start().await;
    let status = t.status().await;
    assert!(!status.enabled);
    assert_eq!(status.state, LinkState::Off);

    // Opening the hub to the outside and making a code are admin; a write device reads only.
    let write = t.device("write").await;
    for (method, path) in [
        (Method::POST, "/v1/link"),
        (Method::POST, "/v1/link/invites"),
    ] {
        let response = t
            .request(method, path, &write.token)
            .json(&json!({ "enabled": true }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
    }
    assert_eq!(
        t.request(Method::GET, "/v1/link", &write.token)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    // No code while the entrance is closed.
    let closed = t
        .request(Method::POST, "/v1/link/invites", &t.hub.token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(closed.status(), StatusCode::CONFLICT);

    let bad = t
        .request(Method::POST, "/v1/link", &t.hub.token)
        .json(&json!({ "enabled": true, "relay": "not a url" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    let opened: LinkStatus = t
        .request(Method::POST, "/v1/link", &t.hub.token)
        .json(&json!({ "enabled": true }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(opened.enabled);
    assert_ne!(opened.state, LinkState::Off);
    let endpoint = opened.endpoint.clone().expect("an endpoint id while open");

    let invite: LinkInvite = t
        .request(Method::POST, "/v1/link/invites", &t.hub.token)
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let decoded = Invite::decode(&invite.code).unwrap();
    assert_eq!(decoded.endpoint, endpoint);
    assert_eq!(decoded.expires, invite.expires_at.timestamp());
    assert_eq!(t.status().await.pending_invites, 1);

    // Turning it off forgets the codes; turning it on again keeps the same id.
    let off: LinkStatus = t
        .request(Method::POST, "/v1/link", &t.hub.token)
        .json(&json!({ "enabled": false }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(off.state, LinkState::Off);
    assert_eq!(off.pending_invites, 0);
    let again: LinkStatus = t
        .request(Method::POST, "/v1/link", &t.hub.token)
        .json(&json!({ "enabled": true }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(again.endpoint.as_deref(), Some(endpoint.as_str()));
    t.hub.shutdown();
    t.hub.wait().await.unwrap();
}

/// Pairs by code and talks to the hub through the forward, then revokes the device. Uses n0's
/// relays: run with `--include-ignored` (and `HTTPS_PROXY` where they need one).
#[tokio::test]
#[ignore = "uses the network (n0 relays)"]
async fn pair_by_code_and_tunnel_until_revoked() {
    let t = TestHub::start().await;
    t.request(Method::POST, "/v1/link", &t.hub.token)
        .json(&json!({ "enabled": true }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let invite: LinkInvite = t
        .request(Method::POST, "/v1/link/invites", &t.hub.token)
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let invite = Invite::decode(&invite.code).unwrap();
    let paired = crossagents_link::redeem(&invite, "mac-gui", Some("macos"), None)
        .await
        .unwrap();
    let paired: CreateDeviceResponse = serde_json::from_value(paired).unwrap();
    assert_eq!(paired.device.name, "mac-gui");
    // A code works once.
    assert!(
        crossagents_link::redeem(&invite, "again", None, None)
            .await
            .is_err()
    );

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(crossagents_link::forward(
        invite.target().unwrap(),
        paired.token.clone(),
        "127.0.0.1:0".parse().unwrap(),
        None,
        move |event| {
            let _ = tx.send(event);
        },
    ));
    let Some(Event::Listening { port }) = rx.recv().await else {
        panic!("no listening event")
    };
    let through = Client::builder().no_proxy().build().unwrap();
    let info = through
        .get(format!("http://127.0.0.1:{port}/v1/info"))
        .bearer_auth(&paired.token)
        .send()
        .await
        .unwrap();
    assert_eq!(info.status(), StatusCode::OK);

    t.request(
        Method::DELETE,
        &format!("/v1/devices/{}", paired.device.id),
        &t.hub.token,
    )
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap();
    // The next local connection is refused at the entrance and the forward says why.
    let fresh = Client::builder().no_proxy().build().unwrap();
    let _ = fresh
        .get(format!("http://127.0.0.1:{port}/v1/health"))
        .timeout(Duration::from_secs(10))
        .send()
        .await;
    let revoked = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(event) = rx.recv().await {
            if matches!(event, Event::Unauthorized { .. }) {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(revoked, "the forward reports the revoked device");
    t.hub.shutdown();
    t.hub.wait().await.unwrap();
}
