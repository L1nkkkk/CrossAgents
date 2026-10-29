//! Account management through the hub API, with a fake CODEX_HOME and usage server.

mod common;

use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::TestHub;
use crossagents_api_types::EventKind;
use serde_json::{Value, json};

const ACCESS_TOKEN_MARK: &str = "very-secret-access";

#[tokio::test]
async fn codebuddy_sites_are_persisted_and_require_separate_login_homes() {
    let t = TestHub::start().await;
    let root = tempfile::tempdir().unwrap();
    for (id, site) in [
        ("codebuddy-cn", "china"),
        ("codebuddy-intl", "international"),
    ] {
        let home = root.path().join(id);
        let response = t
            .post(
                "/v1/accounts",
                json!({
                    "id": id, "provider":"codebuddy", "home":home, "codebuddy_site":site
                }),
            )
            .await;
        assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
        let account: Value = response.json().await.unwrap();
        assert_eq!(account["codebuddy_site"], site);
        assert!(home.join("profile/AppData/Local").is_dir());
        let stored = t.hub.bus.store().get_account(id).await.unwrap();
        assert_eq!(
            serde_json::to_value(stored).unwrap()["codebuddy_site"],
            site
        );
    }
    for request in [
        json!({"id":"wrong-provider", "provider":"codex", "codebuddy_site":"china"}),
        json!({"id":"shared-login", "provider":"codebuddy", "codebuddy_site":"china", "import_default":true}),
        json!({"id":"same-home", "provider":"codebuddy", "codebuddy_site":"international", "home":root.path().join("codebuddy-cn")}),
    ] {
        let response = t.post("/v1/accounts", request).await;
        assert!(!response.status().is_success());
    }
    let shared = root.path().join("concurrent");
    let request =
        |id, site| json!({"id":id, "provider":"codebuddy", "codebuddy_site":site, "home":shared});
    let (a, b) = tokio::join!(
        t.post("/v1/accounts", request("race-cn", "china")),
        t.post("/v1/accounts", request("race-intl", "international")),
    );
    assert_ne!(
        a.status().is_success(),
        b.status().is_success(),
        "only one account can claim a profile"
    );
    let t = TestHub::start_in(t.restart_home().await, |_| {}).await;
    for id in ["codebuddy-cn", "codebuddy-intl"] {
        assert!(
            t.hub
                .bus
                .store()
                .get_account(id)
                .await
                .unwrap()
                .codebuddy_site
                .is_some()
        );
    }
}

fn jwt(claims: Value) -> String {
    let enc = |v: &Value| URL_SAFE_NO_PAD.encode(v.to_string());
    format!("{}.{}.sig", enc(&json!({ "alg": "none" })), enc(&claims))
}

/// A logged-in Codex home with one session started by CrossAgents' mock agent.
fn codex_home(dir: &Path) {
    let access =
        jwt(json!({ "exp": chrono::Utc::now().timestamp() + 3600, "mark": ACCESS_TOKEN_MARK }));
    let auth = json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": jwt(json!({
                "email": "person@example.com",
                "https://api.openai.com/auth": { "chatgpt_plan_type": "plus" }
            })),
            "access_token": access,
            "refresh_token": "refresh",
            "account_id": "acct"
        }
    });
    std::fs::write(dir.join("auth.json"), auth.to_string()).unwrap();
    std::fs::write(
        dir.join("models_cache.json"),
        json!({ "models": [
            { "slug": "strong", "visibility": "list", "priority": 1 },
            { "slug": "cheap", "visibility": "list", "priority": 5 }
        ]})
        .to_string(),
    )
    .unwrap();
    let day = dir.join("sessions/2026/10/21");
    std::fs::create_dir_all(&day).unwrap();
    let lines = [
        json!({ "timestamp": "2026-10-21T02:00:00Z", "type": "session_meta",
                "payload": { "id": "mock-session", "timestamp": "2026-10-21T02:00:00Z", "originator": "crossagents" } }),
        json!({ "timestamp": "2026-10-21T02:00:01Z", "type": "turn_context", "payload": { "model": "cheap" } }),
        json!({ "timestamp": "2026-10-21T02:00:02Z", "type": "event_msg", "payload": { "type": "token_count",
                "info": { "total_token_usage": { "input_tokens": 1000, "cached_input_tokens": 750, "output_tokens": 20, "reasoning_output_tokens": 5 } } } }),
    ];
    let text: Vec<String> = lines.iter().map(Value::to_string).collect();
    std::fs::write(day.join("rollout-x.jsonl"), text.join("\n")).unwrap();
}

/// Answers every request with the same usage document.
async fn usage_server() -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/usage", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0; 8192];
            let _ = socket.read(&mut buf).await;
            let body = json!({
                "plan_type": "plus",
                "rate_limit": { "limit_reached": false,
                    "primary_window": { "used_percent": 42, "limit_window_seconds": 18000, "reset_at": 1800000000 } },
                "credits": { "has_credits": false, "unlimited": false, "balance": "0" }
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    url
}

#[tokio::test]
async fn account_status_usage_models_and_binding() {
    let url = usage_server().await;
    let t = TestHub::start_with(|config| {
        config.codex_usage_url = Some(url.clone());
        // The "codex" plugin runs the mock agent, so binding can be checked end to end.
        let mock = config.plugins["mock"].clone();
        config.plugins.insert("codex".into(), mock);
    })
    .await;
    let home = tempfile::tempdir().unwrap();
    codex_home(home.path());

    let resp = t
        .post(
            "/v1/accounts",
            json!({ "id": "codex-main", "provider": "codex", "home": home.path().display().to_string() }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());

    let status = t.get("/v1/accounts/codex-main/status").await;
    let raw = status.text().await.unwrap();
    assert!(
        !raw.contains(ACCESS_TOKEN_MARK) && !raw.contains("person@"),
        "{raw}"
    );
    let status: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(status["logged_in"], true);
    assert_eq!(status["plan"], "plus");
    assert_eq!(status["source"], "live");
    assert_eq!(status["rate_limits"][0]["used_percent"], 42.0);

    let models: Value = t
        .get("/v1/accounts/codex-main/models")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(models[0]["id"], "strong");

    // An agent bound to the account runs with its CODEX_HOME.
    let resp = t
        .post(
            "/v1/agents",
            json!({ "name": "cx", "plugin": "codex", "role": "worker",
                    "cwd": t.home.path().display().to_string(), "config": { "account": "codex-main" } }),
        )
        .await;
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let mut w = t.watch().await;
    t.send("cx", "[env CODEX_HOME]").await;
    w.until("turn completed", |e| {
        matches!(e.kind, EventKind::AgentTurnCompleted { .. })
    })
    .await;
    assert!(
        w.output()
            .contains(&format!("env CODEX_HOME={}", home.path().display())),
        "{}",
        w.output()
    );
    assert!(w.seen.iter().any(|e| matches!(&e.kind,
        EventKind::AgentSessionStarted { session_id, .. } if session_id == "mock-session")));

    // Its session (id "mock-session") is attributed to it.
    let by_agent: Value = t
        .get("/v1/accounts/codex-main/usage?by=agent")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(by_agent["rows"][0]["key"], "cx");
    assert_eq!(by_agent["rows"][0]["totals"]["cached_input_tokens"], 750);
    assert_eq!(by_agent["total_cache_hit_rate"], 0.75);
    let by_model: Value = t
        .get("/v1/accounts/codex-main/usage?by=model")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(by_model["rows"][0]["key"], "cheap");

    // A codex account cannot run a mock agent.
    let resp = t
        .post(
            "/v1/agents",
            json!({ "name": "bad", "plugin": "mock", "role": "worker",
                    "cwd": t.home.path().display().to_string(), "config": { "account": "codex-main" } }),
        )
        .await;
    assert_eq!(resp.status(), 409);

    // Removing the account leaves its files alone.
    let resp = t
        .client
        .delete(format!("{}/v1/accounts/codex-main", t.base))
        .bearer_auth(&t.hub.token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    assert!(home.path().join("auth.json").exists());
}

#[tokio::test]
async fn new_account_gets_its_own_home() {
    let t = TestHub::start().await;
    let account: Value = t
        .post(
            "/v1/accounts",
            json!({ "id": "codex-work", "provider": "codex" }),
        )
        .await
        .json()
        .await
        .unwrap();
    let home = account["home"].as_str().unwrap();
    assert!(
        home.starts_with(&t.paths.home.display().to_string()),
        "{home}"
    );
    let status: Value = t
        .get("/v1/accounts/codex-work/status")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(status["logged_in"], false);
    let resp = t
        .post("/v1/accounts", json!({ "id": "x", "provider": "nope" }))
        .await;
    assert_eq!(resp.status(), 409);
}
