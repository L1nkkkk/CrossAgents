use std::{fs, path::Path, time::Duration};

use crossagents_core::EventKind;
use crossagents_hub::{HubConfig, Paths, extensions::manifest::Manifest};
use serde_json::Value;

#[cfg(windows)]
const COMMAND: &str = r#"["powershell", "-NoProfile", "-NonInteractive", "-File", "hook.ps1"]"#;
#[cfg(not(windows))]
const COMMAND: &str = r#"["sh", "hook.sh"]"#;
#[cfg(windows)]
const SCRIPT: &str = "hook.ps1";
#[cfg(not(windows))]
const SCRIPT: &str = "hook.sh";

fn write_extension(home: &Path, script: &str, event: &str) {
    let root = home.join("extensions/demo");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("extension.toml"),
        format!("name = 'demo'\napi = 1\n[[on]]\nevent = '{event}'\ncommand = {COMMAND}\n"),
    )
    .unwrap();
    fs::write(root.join(SCRIPT), script).unwrap();
}

fn capture_script(version: &str) -> String {
    #[cfg(windows)]
    {
        format!(
            "$s = [Console]::In.ReadToEnd()\n[IO.File]::WriteAllText((Join-Path $env:CROSSAGENTS_EXTENSION_DATA '{version}'), $s)\n"
        )
    }
    #[cfg(not(windows))]
    {
        format!("cat > \"$CROSSAGENTS_EXTENSION_DATA/{version}\"\n")
    }
}

async fn until(mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("condition timed out");
}

fn approve(registry: &crossagents_hub::extensions::Registry) {
    registry.scan().unwrap();
    let fp = registry
        .get("demo")
        .unwrap()
        .pending
        .unwrap()
        .version
        .fingerprint;
    registry.enable("demo", &fp).unwrap();
}

async fn publish(hub: &crossagents_hub::RunningHub, version: &str) {
    hub.bus
        .publish(EventKind::HubStarted {
            version: version.into(),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn extensions_hooks_approval_event_and_version() {
    let home = tempfile::tempdir().unwrap();
    write_extension(home.path(), &capture_script("one"), "hub.started");
    let hub = crossagents_hub::start(HubConfig::new(Paths::new(home.path()), 0))
        .await
        .unwrap();
    let data = home.path().join("extension-data/demo");
    publish(&hub, "unapproved").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!data.join("one").exists());
    approve(&hub.extensions);
    publish(&hub, "first").await;
    until(|| data.join("one").exists()).await;
    let input: Value = serde_json::from_slice(&fs::read(data.join("one")).unwrap()).unwrap();
    assert_eq!(input["caller"]["kind"], "event");
    assert_eq!(input["event"]["type"], "hub.started");
    assert_eq!(input["event"]["data"]["version"], "first");
    assert!(input["event"]["seq"].is_number());
    write_extension(home.path(), &capture_script("two"), "hub.started");
    approve(&hub.extensions);
    publish(&hub, "second").await;
    until(|| data.join("two").exists()).await;
    let input: Value = serde_json::from_slice(&fs::read(data.join("two")).unwrap()).unwrap();
    assert_eq!(input["event"]["data"]["version"], "second");
    hub.extensions.disable("demo").unwrap();
    fs::remove_file(data.join("two")).unwrap();
    publish(&hub, "disabled").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!data.join("two").exists());
    hub.shutdown();
    hub.wait().await.unwrap();
}

#[test]
fn extensions_hooks_reject_extension_events() {
    let home = tempfile::tempdir().unwrap();
    write_extension(home.path(), &capture_script("one"), "extension.changed");
    let error = Manifest::read(&home.path().join("extensions/demo"), "demo").unwrap_err();
    assert!(error.to_string().contains("extension.*"), "{error}");
}

#[tokio::test]
async fn extensions_hooks_busy_failure_and_shutdown() {
    let home = tempfile::tempdir().unwrap();
    #[cfg(windows)]
    let hold = "[IO.File]::WriteAllText((Join-Path $env:CROSSAGENTS_EXTENSION_DATA 'started'), 'yes')\nwhile ($true) { Start-Sleep -Seconds 1 }\n";
    #[cfg(not(windows))]
    let hold = "echo yes > \"$CROSSAGENTS_EXTENSION_DATA/started\"\nwhile :; do sleep 1; done\n";
    write_extension(home.path(), hold, "hub.started");
    let hub = crossagents_hub::start(HubConfig::new(Paths::new(home.path()), 0))
        .await
        .unwrap();
    approve(&hub.extensions);
    for _ in 0..4 {
        publish(&hub, "hold").await;
    }
    until(|| hub.extension_runner.live_tokens() == 4).await;
    until(|| home.path().join("extension-data/demo/started").exists()).await;
    publish(&hub, "busy").await;
    let log = home.path().join("extension-logs/demo.log");
    until(|| {
        fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("dropped: busy")
    })
    .await;
    let runner = hub.extension_runner.clone();
    hub.shutdown();
    tokio::time::timeout(Duration::from_secs(20), hub.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(runner.live_tokens(), 0);

    #[cfg(windows)]
    let fail = "[Console]::Error.WriteLine('hook failure marker')\nexit 3\n";
    #[cfg(not(windows))]
    let fail = "echo 'hook failure marker' >&2\nexit 3\n";
    write_extension(home.path(), fail, "hub.started");
    let hub = crossagents_hub::start(HubConfig::new(Paths::new(home.path()), 0))
        .await
        .unwrap();
    approve(&hub.extensions);
    publish(&hub, "failure").await;
    until(|| {
        fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("hook failure marker")
    })
    .await;
    publish(&hub, "still running").await;
    until(|| {
        fs::read_to_string(&log)
            .unwrap_or_default()
            .matches("hook failure marker")
            .count()
            >= 2
    })
    .await;
    hub.shutdown();
    hub.wait().await.unwrap();
}
