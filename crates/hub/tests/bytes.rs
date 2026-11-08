use crossagents_hub::{HubConfig, Paths};
use reqwest::{Client, StatusCode};
use serde_json::json;
use std::path::Path;

fn link_dir(target: &Path, link: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let output = std::process::Command::new("cmd")
            .raw_arg(format!(
                r#"/C mklink /J "{}" "{}""#,
                link.display(),
                target.display()
            ))
            .output()
            .expect("run mklink");
        assert!(
            output.status.success(),
            "junction: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("create symlink");
}

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
        assert_eq!(r.headers()["content-type"], "text/javascript");
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
        "%zz",
        "%",
        "%4",
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
    assert_ne!(
        get(format!("/v1/extensions/demo/assets/{second}/../secret"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.js"), "secret").unwrap();
    let snapshot = home.path().join("extension-versions/demo").join(&second);
    let link = snapshot.join("escape");
    link_dir(outside.path(), &link);
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
    let large_asset = std::fs::File::create(snapshot.join("large.bin")).unwrap();
    large_asset
        .set_len(crossagents_hub::extensions::snapshot::MAX_BYTES + 1)
        .unwrap();
    assert_eq!(
        get(format!("/v1/extensions/demo/assets/{second}/large.bin"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
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
    link_dir(outside.path(), &root.path().join("escape"));
    assert_eq!(
        get("/v1/agents/bytes-test/files/raw?path=escape/secret.js".into())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
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

#[tokio::test]
async fn raw_reads_run_and_task_by_key_and_id() {
    use crossagents_core::{Task, TaskId, TaskReport, TaskSpec, TaskStatus};
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let task_dir = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("run.bin"), [1, 0, 2]).unwrap();
    std::fs::write(task_dir.path().join("task.bin"), [3, 0, 4]).unwrap();
    let hub = crossagents_hub::start(HubConfig::new(Paths::new(home.path()), 0))
        .await
        .unwrap();
    let base = format!("http://{}", hub.addr);
    let client = Client::new();
    let created = client.post(format!("{base}/v1/agents")).bearer_auth(&hub.token)
        .json(&json!({"name":"planner", "plugin":"external", "role":"worker", "cwd":workspace.path().display().to_string()}))
        .send().await.unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = client
        .post(format!("{base}/v1/runs"))
        .bearer_auth(&hub.token)
        .json(&json!({"request":"read files", "planner":"planner"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let run: crossagents_api_types::Run = created.json().await.unwrap();
    let run_id = run.id.to_string().parse().unwrap();
    let now = chrono::Utc::now();
    let task = Task {
        id: TaskId::new(),
        run_id,
        key: "bytes-task".into(),
        title: "Bytes".into(),
        spec: TaskSpec::default(),
        depends_on: vec![],
        issuer: None,
        assignee: None,
        reviewer: None,
        worker_template: None,
        reviewer_template: None,
        worker_fresh: true,
        worker_restore: false,
        reviewer_fresh: true,
        status: TaskStatus::Pending,
        attempts: 0,
        runs: vec![],
        escalated: false,
        nudges: 0,
        branch: None,
        worktree: Some(task_dir.path().to_path_buf()),
        report: TaskReport::default(),
        parent: None,
        progress: None,
        handoffs: vec![],
        created_at: now,
        updated_at: now,
    };
    hub.bus.store().insert_task(&task).await.unwrap();
    let routes = [
        (
            format!("/v1/runs/{}/files/raw?path=run.bin", run.id),
            &[1, 0, 2][..],
        ),
        (
            format!(
                "/v1/runs/{}/tasks/{}/files/raw?path=task.bin",
                run.id, task.key
            ),
            &[3, 0, 4][..],
        ),
        (
            format!(
                "/v1/runs/{}/tasks/{}/files/raw?path=task.bin",
                run.id, task.id
            ),
            &[3, 0, 4][..],
        ),
    ];
    for (route, expected) in routes {
        let response = client
            .get(format!("{base}{route}"))
            .bearer_auth(&hub.read_token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{route}");
        assert_eq!(&response.bytes().await.unwrap()[..], expected, "{route}");
    }
}
