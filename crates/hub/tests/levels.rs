//! Device levels must be enforced before the settings and repository handlers run.
use crossagents_api_types::CreateDeviceResponse;
use crossagents_hub::{HubConfig, Paths};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};

#[tokio::test]
async fn settings_and_repo_init_are_admin_but_creating_runs_is_write() {
    let home = tempfile::tempdir().unwrap();
    let hub = crossagents_hub::start(HubConfig::new(Paths::new(home.path()), 0))
        .await
        .unwrap();
    let client = Client::builder().no_proxy().build().unwrap();
    let base = format!("http://{}", hub.addr);
    let mut tokens = Vec::new();
    for level in ["write", "admin"] {
        let device: CreateDeviceResponse = client
            .post(format!("{base}/v1/devices"))
            .bearer_auth(&hub.token)
            .json(&json!({"name": level, "kind": "client", "level": level}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        tokens.push(device.token);
    }
    // Valid request bodies reach business validation for admin, with no CLI or git writes:
    // the agent and path intentionally do not exist inside this test's temporary home.
    let missing_path = home.path().join("missing-parent").join("repo");
    for (path, body, expected) in [
        (
            "/v1/agents/missing-agent/settings",
            json!({"permission":"bypass", "allow_hooks":true, "setting_sources":"user,project"}),
            StatusCode::NOT_FOUND,
        ),
        (
            "/v1/repos/init",
            json!({"path":missing_path}),
            StatusCode::CONFLICT,
        ),
    ] {
        let denied = client
            .post(format!("{base}{path}"))
            .bearer_auth(&tokens[0])
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN, "{path}");
        let error: Value = denied.json().await.unwrap();
        assert_eq!(error["error"]["code"], "forbidden");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("needs admin"),
            "{error}"
        );
        let allowed = client
            .post(format!("{base}{path}"))
            .bearer_auth(&tokens[1])
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_ne!(allowed.status(), StatusCode::FORBIDDEN, "{path}");
        assert_ne!(allowed.status(), StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(allowed.status(), expected, "business validation: {path}");
    }
    let run = client
        .post(format!("{base}/v1/runs"))
        .bearer_auth(&tokens[0])
        .json(&json!({"planner":"missing-agent", "request":"test"}))
        .send()
        .await
        .unwrap();
    assert_ne!(run.status(), StatusCode::FORBIDDEN);
    assert_ne!(run.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(run.status(), StatusCode::NOT_FOUND);
    hub.shutdown();
    hub.wait().await.unwrap();
}
