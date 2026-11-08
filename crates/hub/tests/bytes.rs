use crossagents_hub::{HubConfig, Paths};
use reqwest::{Client, StatusCode};
use serde_json::json;

#[tokio::test]
async fn assets_and_raw_files() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("extensions/demo");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("extension.toml"), "name = \"demo\"\napi = 1\n").unwrap();
    std::fs::write(source.join("index.js"), "first").unwrap();
    let hub = crossagents_hub::start(HubConfig::new(Paths::new(home.path()), 0))
        .await
        .unwrap();
    let registry = &hub.extensions;
    let first = registry
        .get("demo")
        .unwrap()
        .pending
        .unwrap()
        .version
        .fingerprint;
    registry.enable("demo", &first).unwrap();
    std::fs::write(source.join("index.js"), "second version").unwrap();
    registry.scan().unwrap();
    let second = registry
        .get("demo")
        .unwrap()
        .pending
        .unwrap()
        .version
        .fingerprint;
    registry.enable("demo", &second).unwrap();
    let base = format!("http://{}", hub.addr);
    let client = Client::new();
    let get = |path: String| {
        client
            .get(format!("{base}{path}"))
            .bearer_auth(&hub.read_token)
    };
    for (fp, body) in [(&first, "first"), (&second, "second version")] {
        let r = get(format!("/v1/extensions/demo/assets/{fp}/index.js"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()["x-content-type-options"], "nosniff");
        assert_eq!(r.text().await.unwrap(), body);
    }
    assert_eq!(
        get("/v1/extensions/demo/assets/other/index.js".into())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    for path in [
        "%2e%2e%2fsecret",
        "C%3Asecret",
        "a%5Cb",
        "a%3Astream",
        "%00",
        "a//b",
    ] {
        assert_eq!(
            get(format!("/v1/extensions/demo/assets/{second}/{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST,
            "{path}"
        );
    }
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.js"), "secret").unwrap();
    let snapshot = home.path().join("extension-versions/demo").join(&second);
    let link = snapshot.join("escape");
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(outside.path(), &link).is_ok();
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_dir(outside.path(), &link).is_ok()
        || std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(outside.path())
            .output()
            .is_ok_and(|o| o.status.success());
    #[cfg(not(any(unix, windows)))]
    let linked = false;
    if linked {
        assert_eq!(
            get(format!(
                "/v1/extensions/demo/assets/{second}/escape/secret.js"
            ))
            .send()
            .await
            .unwrap()
            .status(),
            StatusCode::NOT_FOUND
        );
    } else {
        eprintln!("Skipping junction/symlink escape test: link creation unavailable");
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("中文 空.txt"), [0, 1, 255]).unwrap();
    let response = client.post(format!("{base}/v1/agents")).bearer_auth(&hub.token).json(&json!({"name":"bytes-test", "plugin":"claude", "role":"worker", "cwd":root.path().display().to_string()})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let r = get("/v1/agents/bytes-test/files/raw?path=%E4%B8%AD%E6%96%87%20%E7%A9%BA.txt".into())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(
        r.headers()["content-disposition"],
        "attachment; filename*=UTF-8''%E4%B8%AD%E6%96%87%20%E7%A9%BA.txt"
    );
    assert_eq!(&r.bytes().await.unwrap()[..], &[0, 1, 255]);
    assert_eq!(
        get("/v1/agents/bytes-test/files/raw?path=missing".into())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let outside_file = outside.path().join("secret.js").display().to_string();
    let encoded: String = outside_file.bytes().map(|b| format!("%{b:02X}")).collect();
    let r = get(format!("/v1/agents/bytes-test/files/raw?path={encoded}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let big = std::fs::File::create(root.path().join("big")).unwrap();
    big.set_len(crossagents_hub::api_bytes::MAX_RAW_FILE_BYTES + 1)
        .unwrap();
    assert_eq!(
        get("/v1/agents/bytes-test/files/raw?path=big".into())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
}
