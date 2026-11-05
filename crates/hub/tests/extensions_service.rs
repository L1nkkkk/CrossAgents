use std::{fs, path::Path, sync::Arc, time::Duration};

use crossagents_api_types::ExtensionServiceState;
use crossagents_hub::extensions::{Registry, Runner, Services};
use tokio_util::sync::CancellationToken;

fn extension(home: &Path, script: &str) {
    let dir = home.join("extensions/demo");
    fs::create_dir_all(&dir).unwrap();
    #[cfg(windows)]
    let (command, file) = (
        "['powershell', '-NoProfile', '-File', 'service.ps1']",
        "service.ps1",
    );
    #[cfg(not(windows))]
    let (command, file) = ("['sh', 'service.sh']", "service.sh");
    fs::write(
        dir.join("extension.toml"),
        format!("name = 'demo'\napi = 1\naccess = 'write'\n[service]\ncommand = {command}\n"),
    )
    .unwrap();
    fs::write(dir.join(file), script).unwrap();
}

fn script(kind: &str) -> &'static str {
    #[cfg(windows)]
    {
        match kind {
            "crash" => "exit 3",
            "one" => {
                "$env:CROSSAGENTS_TOKEN | Set-Content (Join-Path $env:CROSSAGENTS_EXTENSION_DATA 'token'); 'one' | Set-Content (Join-Path $env:CROSSAGENTS_EXTENSION_DATA 'version'); while ($true) { Start-Sleep -Milliseconds 100 }"
            }
            "two" => {
                "$env:CROSSAGENTS_TOKEN | Set-Content (Join-Path $env:CROSSAGENTS_EXTENSION_DATA 'token'); 'two' | Set-Content (Join-Path $env:CROSSAGENTS_EXTENSION_DATA 'version'); while ($true) { Start-Sleep -Milliseconds 100 }"
            }
            "tree" => {
                "if ($args[0] -eq 'child') { while ($true) { Add-Content (Join-Path $env:CROSSAGENTS_EXTENSION_DATA child) x; Start-Sleep -Milliseconds 100 } }; Start-Process powershell -ArgumentList '-NoProfile','-File','service.ps1','child' -WindowStyle Hidden; while ($true) { Add-Content (Join-Path $env:CROSSAGENTS_EXTENSION_DATA parent) x; Start-Sleep -Milliseconds 100 }"
            }
            _ => unreachable!(),
        }
    }
    #[cfg(not(windows))]
    {
        match kind {
            "crash" => "exit 3",
            "one" => {
                "echo \"$CROSSAGENTS_TOKEN\" > \"$CROSSAGENTS_EXTENSION_DATA/token\"; echo one > \"$CROSSAGENTS_EXTENSION_DATA/version\"; while :; do sleep .1; done"
            }
            "two" => {
                "echo \"$CROSSAGENTS_TOKEN\" > \"$CROSSAGENTS_EXTENSION_DATA/token\"; echo two > \"$CROSSAGENTS_EXTENSION_DATA/version\"; while :; do sleep .1; done"
            }
            "tree" => {
                "(while :; do echo x >> \"$CROSSAGENTS_EXTENSION_DATA/child\"; sleep .1; done) & while :; do echo x >> \"$CROSSAGENTS_EXTENSION_DATA/parent\"; sleep .1; done"
            }
            _ => unreachable!(),
        }
    }
}

async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("condition timed out");
}

fn approve(registry: &Registry) -> String {
    let fp = registry
        .get("demo")
        .unwrap()
        .pending
        .unwrap()
        .version
        .fingerprint;
    registry.enable("demo", &fp).unwrap();
    fp
}

#[tokio::test]
async fn extensions_service_approval_switch_and_token() {
    let home = tempfile::tempdir().unwrap();
    extension(home.path(), script("one"));
    let registry = Arc::new(Registry::load(home.path()).unwrap());
    let runner = Runner::new(registry.clone(), None, home.path().to_owned());
    let services = Services::new(runner.clone(), registry.clone());
    let stop = CancellationToken::new();
    let task = services.start(stop.clone());
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(runner.live_tokens(), 0);
    approve(&registry);
    let data = home.path().join("extension-data/demo");
    until(|| data.join("token").exists()).await;
    let token = fs::read_to_string(data.join("token"))
        .unwrap()
        .trim()
        .to_owned();
    assert!(runner.token(&token).is_some());
    extension(home.path(), script("two"));
    registry.scan().unwrap();
    approve(&registry);
    until(|| {
        fs::read_to_string(data.join("version"))
            .unwrap_or_default()
            .trim()
            == "two"
    })
    .await;
    assert!(runner.token(&token).is_none());
    let new_token = fs::read_to_string(data.join("token"))
        .unwrap()
        .trim()
        .to_owned();
    assert_ne!(token, new_token);
    stop.cancel();
    task.await.unwrap();
    assert!(runner.token(&new_token).is_none());
}

#[tokio::test]
async fn extensions_service_crashes_back_off_and_count() {
    let home = tempfile::tempdir().unwrap();
    extension(home.path(), script("crash"));
    let registry = Arc::new(Registry::load(home.path()).unwrap());
    approve(&registry);
    let runner = Runner::new(registry.clone(), None, home.path().to_owned());
    let services = Services::new(runner, registry.clone());
    let stop = CancellationToken::new();
    let task = services.start(stop.clone());
    until(|| {
        registry
            .get("demo")
            .unwrap()
            .service
            .as_ref()
            .is_some_and(|s| s.state == ExtensionServiceState::Backoff && s.restarts == 1)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(registry.get("demo").unwrap().service.unwrap().restarts, 1);
    until(|| {
        registry
            .get("demo")
            .unwrap()
            .service
            .as_ref()
            .is_some_and(|s| s.restarts >= 2 && s.last_exit.is_some())
    })
    .await;
    stop.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn extensions_service_disable_kills_parent_and_child() {
    let home = tempfile::tempdir().unwrap();
    extension(home.path(), script("tree"));
    let registry = Arc::new(Registry::load(home.path()).unwrap());
    approve(&registry);
    let runner = Runner::new(registry.clone(), None, home.path().to_owned());
    let services = Services::new(runner.clone(), registry.clone());
    let stop = CancellationToken::new();
    let task = services.start(stop.clone());
    let data = home.path().join("extension-data/demo");
    until(|| data.join("parent").exists() && data.join("child").exists()).await;
    registry.disable("demo").unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(runner.live_tokens(), 1);
    until(|| runner.live_tokens() == 0).await;
    let parent = fs::metadata(data.join("parent")).unwrap().len();
    let child = fs::metadata(data.join("child")).unwrap().len();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(parent, fs::metadata(data.join("parent")).unwrap().len());
    assert_eq!(child, fs::metadata(data.join("child")).unwrap().len());
    stop.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn extensions_service_token_cannot_reach_admin_and_dies_on_hub_shutdown() {
    let home = tempfile::tempdir().unwrap();
    extension(home.path(), script("one"));
    let hub = crossagents_hub::start(crossagents_hub::HubConfig::new(
        crossagents_hub::Paths::new(home.path()),
        0,
    ))
    .await
    .unwrap();
    approve(&hub.extensions);
    let token_file = home.path().join("extension-data/demo/token");
    until(|| token_file.exists() && fs::metadata(&token_file).unwrap().len() > 0).await;
    let token = fs::read_to_string(token_file).unwrap().trim().to_owned();
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/shutdown", hub.addr))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    assert!(hub.extension_runner.token(&token).is_some());
    hub.shutdown_token().cancel();
    until(|| hub.extension_runner.token(&token).is_none()).await;
}
