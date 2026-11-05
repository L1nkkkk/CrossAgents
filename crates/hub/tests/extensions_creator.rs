//! E3: creator tools are inspection and approval requests, never trial execution.
use crossagents_api_types::{ExtensionInfo, ExtensionLogsResponse};
use crossagents_core::Agent;
use crossagents_hub::{HubConfig, Paths, extensions::Registry};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn source(home: &Path) {
    let root = home.join("extensions/demo");
    fs::create_dir_all(root.join("gui")).unwrap();
    #[cfg(windows)]
    let (command, script) = (
        r#"["powershell", "-NoProfile", "-File", "marker.ps1"]"#,
        (
            "marker.ps1",
            "Set-Content -LiteralPath marker -Value executed\nWrite-Output '{}'\n",
        ),
    );
    #[cfg(not(windows))]
    let (command, script) = (
        r#"["sh", "marker.sh"]"#,
        ("marker.sh", "echo executed > marker\nprintf '{}'\n"),
    );
    fs::write(root.join(script.0), script.1).unwrap();
    fs::write(
        root.join("gui/index.js"),
        "throw new Error('must not import');",
    )
    .unwrap();
    fs::write(
        root.join("extension.toml"),
        format!(
            r#"name = "demo"
api = 1
[[tools]]
name = "marker"
description = "Writes a marker if executed"
groups = ["everyone", "human"]
command = {command}
input_schema = {{ type = "object" }}
[gui]
entry = "gui/index.js"
"#
        ),
    )
    .unwrap();
}

#[test]
fn extensions_request_enable_preserves_current_even_with_auto_trust() {
    let home = tempfile::tempdir().unwrap();
    source(home.path());
    let registry = Registry::load(home.path()).unwrap();
    let initial = registry
        .get("demo")
        .unwrap()
        .pending
        .unwrap()
        .version
        .fingerprint;
    registry.enable("demo", &initial).unwrap();
    registry.set_auto_trust("demo", true).unwrap();
    fs::write(home.path().join("extensions/demo/new.txt"), "changed").unwrap();
    let requested = registry.request_enable("demo", None).unwrap();
    assert!(requested.enabled && requested.auto_trust);
    assert_eq!(requested.current.unwrap().fingerprint, initial);
    let pending = requested.pending.unwrap().version.fingerprint;
    assert_ne!(pending, initial);
    assert!(
        home.path()
            .join("extension-versions/demo")
            .join(pending)
            .join("new.txt")
            .is_file()
    );
    let saved: Value =
        serde_json::from_slice(&fs::read(home.path().join("extensions.json")).unwrap()).unwrap();
    assert!(saved["demo"]["pending"].is_object());
    assert!(!home.path().join("extensions/demo/marker").exists());
}

#[tokio::test]
async fn extensions_creator_http_static_check_request_and_logs() {
    let home = tempfile::tempdir().unwrap();
    source(home.path());
    // Establish that the script really would write a marker if run.
    let root = home.path().join("extensions/demo");
    #[cfg(windows)]
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-File", "marker.ps1"])
        .current_dir(&root)
        .status()
        .unwrap();
    #[cfg(not(windows))]
    let status = std::process::Command::new("sh")
        .arg("marker.sh")
        .current_dir(&root)
        .status()
        .unwrap();
    assert!(status.success() && root.join("marker").is_file());
    fs::remove_file(root.join("marker")).unwrap();
    let hub = crossagents_hub::start(HubConfig::new(Paths::new(home.path()), 0))
        .await
        .unwrap();
    let base = format!("http://{}", hub.addr);
    let client = Client::new();
    let agent: Agent = client.post(format!("{base}/v1/agents")).bearer_auth(&hub.token)
        .json(&json!({"name":"creator", "plugin":"claude", "model":"haiku", "role":"worker", "cwd":home.path()}))
        .send().await.unwrap().json().await.unwrap();
    let session = hub.agent_tokens.issue(agent.id).await;
    let listed: Value = client
        .get(format!("{base}/v1/me/tools"))
        .bearer_auth(&session)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: Vec<_> = listed["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    for name in [
        "extension_api",
        "extension_check",
        "extension_request_enable",
        "extension_logs",
    ] {
        assert!(names.contains(&name));
    }
    assert!(!names.contains(&"extension_enable") && !names.contains(&"extension_auto_trust"));
    let call = |name: &str| {
        client
            .post(format!("{base}/v1/me/tools/{name}"))
            .bearer_auth(&session)
            .json(&json!({"name":"demo"}))
    };
    let api: Value = call("extension_api")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(api["api"], 1);
    assert!(
        api["events"]
            .as_array()
            .unwrap()
            .contains(&json!("extension.changed"))
    );
    assert!(
        api["example"]["files"]["greet.mjs"]
            .as_str()
            .unwrap()
            .contains("process.stdin")
    );
    let state_before_check = fs::read(home.path().join("extensions.json")).unwrap();
    let checked = call("extension_check").send().await.unwrap();
    assert_eq!(checked.status(), StatusCode::OK);
    assert_eq!(
        fs::read(home.path().join("extensions.json")).unwrap(),
        state_before_check
    );
    assert!(!root.join("marker").exists());
    let info: ExtensionInfo = call("extension_request_enable")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!info.enabled && info.current.is_none() && !info.auto_trust);
    let pending = info.pending.unwrap();
    assert_eq!(pending.requested_by, Some(agent.id));
    assert!(
        home.path()
            .join("extension-versions/demo")
            .join(&pending.version.fingerprint)
            .join("marker.ps1")
            .exists()
            || home
                .path()
                .join("extension-versions/demo")
                .join(&pending.version.fingerprint)
                .join("marker.sh")
                .exists()
    );
    for action in ["enable", "auto-trust"] {
        let status = client
            .post(format!("{base}/v1/extensions/demo/{action}"))
            .bearer_auth(&session)
            .json(&json!({"fingerprint":pending.version.fingerprint,"auto_trust":true}))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    for tool in [
        "extension_enable",
        "extension_auto_trust",
        "extension_test",
        "extension_run",
    ] {
        assert_ne!(call(tool).send().await.unwrap().status(), StatusCode::OK);
    }
    let empty: ExtensionLogsResponse = call("extension_logs")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(empty.content, "");
    fs::create_dir_all(home.path().join("extension-logs")).unwrap();
    let mut bytes = vec![b'x'; 70_000];
    bytes.extend_from_slice(b"\nfinished: stderr example\n");
    fs::write(home.path().join("extension-logs/demo.log"), bytes).unwrap();
    let logs: ExtensionLogsResponse = call("extension_logs")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(logs.truncated && logs.content.ends_with("stderr example\n"));
    assert_eq!(logs.content.len(), 64 * 1024);
    let gui = client
        .get(format!("{base}/v1/extensions/demo/logs"))
        .bearer_auth(&hub.read_token)
        .send()
        .await
        .unwrap();
    assert_eq!(gui.status(), StatusCode::OK);
    assert_eq!(gui.json::<ExtensionLogsResponse>().await.unwrap(), logs);
    assert_eq!(
        client
            .get(format!("{base}/v1/extensions/demo/logs"))
            .bearer_auth(&session)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    fs::remove_file(root.join("gui/index.js")).unwrap();
    assert_ne!(
        call("extension_check").send().await.unwrap().status(),
        StatusCode::OK
    );
    assert!(!root.join("marker").exists());
    for version in fs::read_dir(home.path().join("extension-versions/demo")).unwrap() {
        assert!(!version.unwrap().path().join("marker").exists());
    }
    hub.shutdown();
}

#[test]
fn extensions_check_validates_schema_entries_and_the_documented_example() {
    let home = tempfile::tempdir().unwrap();
    source(home.path());
    let registry = Registry::load(home.path()).unwrap();
    let root = home.path().join("extensions/demo");
    let manifest = fs::read_to_string(root.join("extension.toml")).unwrap();
    fs::write(
        root.join("extension.toml"),
        manifest.replace("type = \"object\"", "type = \"nonsense\""),
    )
    .unwrap();
    assert!(registry.check("demo").is_err());
    fs::write(root.join("extension.toml"), manifest).unwrap();
    fs::remove_file(root.join("gui/index.js")).unwrap();
    assert!(registry.check("demo").is_err());
    assert!(registry.check("../demo").is_err());
    assert!(!root.join("marker").exists());

    let api = crossagents_hub::extensions::creator::api();
    let hello = home.path().join("extensions/hello");
    for (name, content) in api["example"]["files"].as_object().unwrap() {
        let path = hello.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content.as_str().unwrap()).unwrap();
    }
    let checked = registry.check("hello").unwrap();
    assert_eq!(checked.api, 1);
    assert_eq!(checked.tools[0].qualified_name, "hello__greet");
    assert_eq!(checked.gui_entry.as_deref(), Some("gui/index.js"));
}
