use crossagents_hub::extensions::{Registry, snapshot};
use std::{fs, path::Path, sync::Arc};
fn source(home: &Path, content: &str) {
    let root = home.join("extensions/demo");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("extension.toml"),
        r#"name = "demo"
api = 1
[[tools]]
name = "report"
description = "report"
command = ["never-execute-this"]
input_schema = { type = "object" }
groups = ["human"]
"#,
    )
    .unwrap();
    fs::write(root.join("tool.txt"), content).unwrap();
}
fn pending(r: &Registry) -> String {
    r.get("demo").unwrap().pending.unwrap().version.fingerprint
}
#[test]
fn extensions_approval_switch_invalidation_and_restart() {
    let home = tempfile::tempdir().unwrap();
    source(home.path(), "first");
    let r = Registry::load(home.path()).unwrap();
    assert!(!r.get("demo").unwrap().enabled);
    assert!(r.enabled("demo").is_err());
    let first = pending(&r);
    assert!(r.enable("demo", "wrong").is_err());
    r.enable("demo", &first).unwrap();
    let path = r.enabled("demo").unwrap().path;
    assert!(
        fs::metadata(path.join("tool.txt"))
            .unwrap()
            .permissions()
            .readonly()
    );
    fs::write(
        home.path().join("extensions/demo/tool.txt"),
        "second version",
    )
    .unwrap();
    r.scan().unwrap();
    let second = pending(&r);
    assert_ne!(first, second);
    assert_eq!(r.enabled("demo").unwrap().version.fingerprint, first);
    assert_eq!(fs::read_to_string(path.join("tool.txt")).unwrap(), "first");
    let diff = r.pending_diff("demo").unwrap();
    assert_eq!(diff.against.as_deref(), Some(first.as_str()));
    assert!(diff.diff.contains("second version"));
    // Revalidate at approval, even if the two-second poll has not run yet.
    fs::write(
        home.path().join("extensions/demo/tool.txt"),
        "third version now",
    )
    .unwrap();
    assert!(r.enable("demo", &second).is_err());
    let third = pending(&r);
    assert_ne!(second, third);
    r.enable("demo", &third).unwrap();
    assert_eq!(
        r.get("demo").unwrap().previous_fingerprint.as_deref(),
        Some(first.as_str())
    );
    fs::write(r.enabled("demo").unwrap().data.join("state"), "kept").unwrap();
    drop(r);
    let r = Registry::load(home.path()).unwrap();
    assert_eq!(r.enabled("demo").unwrap().version.fingerprint, third);
    assert_eq!(
        fs::read_to_string(r.enabled("demo").unwrap().data.join("state")).unwrap(),
        "kept"
    );
    r.disable("demo").unwrap();
    drop(r);
    let r = Registry::load(home.path()).unwrap();
    assert!(r.enabled("demo").is_err());
}
#[test]
fn extensions_pending_survives_restart_and_invalid_source_invalidates() {
    let home = tempfile::tempdir().unwrap();
    source(home.path(), "first");
    let r = Registry::load(home.path()).unwrap();
    let original = r.get("demo").unwrap().pending.unwrap();
    drop(r);
    let r = Registry::load(home.path()).unwrap();
    let restored = r.get("demo").unwrap().pending.unwrap();
    assert_eq!(original.requested_at, restored.requested_at);
    assert_eq!(original.version.fingerprint, restored.version.fingerprint);
    fs::write(
        home.path().join("extensions/demo/extension.toml"),
        "invalid",
    )
    .unwrap();
    r.scan().unwrap();
    assert!(r.get("demo").unwrap().pending.is_none());
    assert!(r.get("demo").unwrap().error.is_some());
    assert!(r.enable("demo", &original.version.fingerprint).is_err());
}
#[test]
fn extensions_auto_trust_and_occupied_cleanup() {
    let home = tempfile::tempdir().unwrap();
    source(home.path(), "v1");
    let r = Arc::new(Registry::load(home.path()).unwrap());
    r.set_auto_trust("demo", true).unwrap();
    let lease = r.acquire("demo").unwrap();
    let first_path = lease.enabled.path.clone();
    for content in ["version two", "version three is here"] {
        fs::write(home.path().join("extensions/demo/tool.txt"), content).unwrap();
        r.scan().unwrap();
    }
    assert!(first_path.exists());
    assert!(r.get("demo").unwrap().pending.is_none());
    drop(lease);
    assert!(!first_path.exists());
    drop(r);
    let r = Registry::load(home.path()).unwrap();
    assert!(r.get("demo").unwrap().auto_trust);
}
#[test]
fn extensions_reject_invalid_manifests_and_oversize() {
    let home = tempfile::tempdir().unwrap();
    source(home.path(), "ok");
    let manifest = home.path().join("extensions/demo/extension.toml");
    let valid = fs::read_to_string(&manifest).unwrap();
    for invalid in [
        valid.replace("api = 1", "api = 2"),
        valid.replace("name = \"report\"", "name = \"bad__tool\""),
        valid.replace("type = \"object\"", "type = \"nonsense\""),
        valid.replace("name = \"demo\"", "name = \"other\""),
    ] {
        fs::write(&manifest, invalid).unwrap();
        let r = Registry::load(home.path()).unwrap();
        assert!(r.get("demo").unwrap().error.is_some());
        assert!(r.get("demo").unwrap().pending.is_none());
    }
    fs::write(manifest, valid).unwrap();
    let f = fs::File::create(home.path().join("extensions/demo/huge")).unwrap();
    f.set_len(snapshot::MAX_BYTES + 1).unwrap();
    let r = Registry::load(home.path()).unwrap();
    assert!(r.get("demo").unwrap().error.unwrap().contains("50 MB"));
}
/// `[[on]]` and `[service]` are part of the fingerprint, so both are validated as strictly
/// as tools and both show up in the version a person approves (docs/EXTENSIONS.md §3).
#[test]
fn extensions_hooks_and_service_are_validated_and_reported() {
    let home = tempfile::tempdir().unwrap();
    source(home.path(), "ok");
    let manifest = home.path().join("extensions/demo/extension.toml");
    let valid = fs::read_to_string(&manifest).unwrap();
    let hooked = format!(
        "{valid}\n[[on]]\nevent = \"run.landed\"\ncommand = [\"never-execute-this\"]\n\n[service]\ncommand = [\"never-execute-this\"]\n"
    );
    fs::write(&manifest, &hooked).unwrap();
    let r = Registry::load(home.path()).unwrap();
    let info = r.get("demo").unwrap();
    assert!(info.error.is_none(), "{:?}", info.error);
    let version = info.pending.unwrap().version;
    assert_eq!(version.hooks.len(), 1);
    assert_eq!(version.hooks[0].event, "run.landed");
    assert_eq!(version.hooks[0].timeout_secs, 60);
    assert!(version.service);

    for invalid in [
        // Extensions may not react to each other: a chain would never end.
        hooked.replace("run.landed", "extension.changed"),
        hooked.replace("run.landed", "run.nonexistent"),
        format!(
            "{valid}\n[[on]]\nevent = \"run.landed\"\ncommand = [\"x\"]\n[[on]]\nevent = \"run.landed\"\ncommand = [\"x\"]\n"
        ),
        format!("{valid}\n[[on]]\nevent = \"run.landed\"\ncommand = [\"x\"]\ntimeout_secs = 0\n"),
        format!("{valid}\n[service]\ncommand = []\n"),
    ] {
        fs::write(&manifest, invalid).unwrap();
        let r = Registry::load(home.path()).unwrap();
        let info = r.get("demo").unwrap();
        assert!(info.error.is_some(), "accepted an invalid hook/service");
        assert!(info.pending.is_none());
    }
}

/// The resident-process supervisor learns about switches from here, with the lock released.
#[test]
fn extensions_version_change_callback_fires_for_switch_and_disable() {
    let home = tempfile::tempdir().unwrap();
    source(home.path(), "first");
    let r = Registry::load(home.path()).unwrap();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = seen.clone();
    r.on_version_changed(move |name| sink.lock().unwrap().push(name.to_owned()));

    let fp = pending(&r);
    r.enable("demo", &fp).unwrap();
    assert_eq!(*seen.lock().unwrap(), ["demo"]);

    source(home.path(), "second");
    r.set_auto_trust("demo", true).unwrap();
    assert_eq!(seen.lock().unwrap().len(), 2);

    r.disable("demo").unwrap();
    assert_eq!(seen.lock().unwrap().len(), 3);
}

#[cfg(unix)]
#[test]
fn extensions_reject_symlinks() {
    let home = tempfile::tempdir().unwrap();
    source(home.path(), "ok");
    std::os::unix::fs::symlink(home.path(), home.path().join("extensions/demo/link")).unwrap();
    let r = Registry::load(home.path()).unwrap();
    assert!(r.get("demo").unwrap().error.is_some());
    assert!(r.get("demo").unwrap().pending.is_none());
}
#[cfg(windows)]
#[test]
fn extensions_reject_junctions() {
    // Junction creation needs no administrator/developer-mode privilege.
    let home = tempfile::tempdir().unwrap();
    source(home.path(), "ok");
    let target = home.path().join("outside");
    fs::create_dir(&target).unwrap();
    let link = home.path().join("extensions/demo/link");
    let script = format!(
        "New-Item -ItemType Junction -Path '{}' -Target '{}' | Out-Null",
        link.display().to_string().replace("'", "''"),
        target.display().to_string().replace("'", "''")
    );
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(script)
        .status()
        .unwrap();
    assert!(status.success());
    let r = Registry::load(home.path()).unwrap();
    assert!(r.get("demo").unwrap().error.is_some());
    assert!(r.get("demo").unwrap().pending.is_none());
    fs::remove_dir(link).unwrap();
}

#[tokio::test]
async fn extensions_http_routes_poll_and_events() {
    use crossagents_api_types::{ExtensionDiffResponse, ExtensionInfo, ExtensionsResponse};
    use crossagents_hub::{HubConfig, Paths};
    use reqwest::{Client, StatusCode};
    let home = tempfile::tempdir().unwrap();
    source(home.path(), "initial");
    let hub = crossagents_hub::start(HubConfig::new(Paths::new(home.path()), 0))
        .await
        .unwrap();
    let base = format!("http://{}/v1/extensions", hub.addr);
    let client = Client::new();
    let list: ExtensionsResponse = client
        .get(&base)
        .bearer_auth(&hub.read_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let fp = list.extensions[0]
        .pending
        .as_ref()
        .unwrap()
        .version
        .fingerprint
        .clone();
    let enable = format!("{base}/demo/enable");
    assert_eq!(
        client
            .post(&enable)
            .bearer_auth(&hub.read_token)
            .json(&serde_json::json!({"fingerprint":fp}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .post(&enable)
            .bearer_auth(&hub.token)
            .json(&serde_json::json!({"fingerprint":"wrong"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let info: ExtensionInfo = client
        .post(&enable)
        .bearer_auth(&hub.token)
        .json(&serde_json::json!({"fingerprint":fp}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(info.enabled);
    assert_eq!(
        client
            .get(format!("{base}/demo/pending/diff"))
            .bearer_auth(&hub.token)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let mut events = hub.bus.subscribe();
    fs::write(
        home.path().join("extensions/demo/tool.txt"),
        "polled new content",
    )
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let event = events.recv().await.unwrap();
            if matches!(
                event.kind,
                crossagents_core::EventKind::ExtensionChanged { .. }
            ) {
                let list: ExtensionsResponse = client
                    .get(&base)
                    .bearer_auth(&hub.token)
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                if list.extensions[0].pending.is_some() {
                    break;
                }
            }
        }
    })
    .await
    .unwrap();
    let diff: ExtensionDiffResponse = client
        .get(format!("{base}/demo/pending/diff"))
        .bearer_auth(&hub.read_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(diff.against, Some(fp));
    let info: ExtensionInfo = client
        .post(format!("{base}/demo/auto-trust"))
        .bearer_auth(&hub.token)
        .json(&serde_json::json!({"auto_trust": true}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(info.auto_trust);
    assert!(info.pending.is_none());
    let info: ExtensionInfo = client
        .post(format!("{base}/demo/disable"))
        .bearer_auth(&hub.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!info.enabled);
    hub.shutdown();
    hub.wait().await.unwrap();
}
#[test]
fn extensions_diff_includes_binary_removals_and_access_change() {
    let home = tempfile::tempdir().unwrap();
    source(home.path(), "v1");
    fs::write(home.path().join("extensions/demo/blob"), [0, 1]).unwrap();
    let r = Registry::load(home.path()).unwrap();
    r.enable("demo", &pending(&r)).unwrap();
    fs::remove_file(home.path().join("extensions/demo/blob")).unwrap();
    let path = home.path().join("extensions/demo/extension.toml");
    let m = fs::read_to_string(&path).unwrap();
    fs::write(path, m.replace("api = 1", "api = 1\naccess = \"write\"")).unwrap();
    r.scan().unwrap();
    let diff = r.pending_diff("demo").unwrap();
    assert!(diff.access_changed);
    let blob = diff.files.iter().find(|f| f.path == "blob").unwrap();
    assert!(blob.binary);
    assert_eq!(
        blob.change,
        crossagents_api_types::ExtensionFileChangeKind::Removed
    );
}

#[cfg(windows)]
#[test]
fn extensions_locked_old_snapshot_does_not_fail_approval_or_load_and_is_retried() {
    use std::os::windows::fs::OpenOptionsExt;
    let home = tempfile::tempdir().unwrap();
    source(home.path(), "first");
    let r = Registry::load(home.path()).unwrap();
    r.enable("demo", &pending(&r)).unwrap();
    let old = r.enabled("demo").unwrap().path;
    // Allow reading/writing, but deny FILE_SHARE_DELETE, as an external reader may do.
    let handle = fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(old.join("tool.txt"))
        .unwrap();
    for text in ["second version", "third version now"] {
        fs::write(home.path().join("extensions/demo/tool.txt"), text).unwrap();
        r.scan().unwrap();
        let fp = pending(&r);
        assert!(r.enable("demo", &fp).unwrap().enabled);
        let state: serde_json::Value =
            serde_json::from_slice(&fs::read(home.path().join("extensions.json")).unwrap())
                .unwrap();
        assert_eq!(state["demo"]["current"], fp);
    }
    assert!(old.exists());
    // A failed cleanup must not prevent later extensions from being scanned.
    let later = home.path().join("extensions/zzz");
    fs::create_dir(&later).unwrap();
    fs::write(later.join("extension.toml"), "name = \"zzz\"\napi = 1\n").unwrap();
    r.scan().unwrap();
    assert!(r.get("zzz").unwrap().pending.is_some());
    drop(r);
    let r = Registry::load(home.path()).unwrap();
    assert!(r.get("demo").unwrap().enabled);
    assert!(old.exists());
    drop(handle);
    // Source metadata has not changed: a later poll must still retry cleanup.
    r.scan().unwrap();
    assert!(!old.exists());
}
#[test]
fn extensions_scan_continues_after_error_and_load_tolerates_scan_failure() {
    let home = tempfile::tempdir().unwrap();
    let r = Registry::load(home.path()).unwrap();
    source(home.path(), "first");
    // Saving demo fails, while the later invalid name needs no persistent record.
    fs::create_dir(home.path().join("extensions.json.tmp")).unwrap();
    fs::create_dir(home.path().join("extensions/zzz_invalid")).unwrap();
    r.scan().unwrap();
    assert!(r.get("zzz_invalid").unwrap().error.is_some());
    drop(r);
    let r = Registry::load_with_bus(home.path(), None).unwrap();
    assert!(r.get("zzz_invalid").unwrap().error.is_some());
}
#[test]
fn extensions_request_missing_does_not_create_or_modify_records() {
    let home = tempfile::tempdir().unwrap();
    let r = Registry::load(home.path()).unwrap();
    let state = home.path().join("extensions.json");
    let before = fs::read(&state).ok();
    assert!(matches!(
        r.request_enable("missing", None),
        Err(crossagents_core::CoreError::NotFound { .. })
    ));
    assert_eq!(fs::read(&state).ok(), before);
    assert!(r.list().is_empty());
    // A new directory is still discovered immediately, without waiting for polling.
    source(home.path(), "first");
    assert!(r.request_enable("demo", None).unwrap().pending.is_some());
    fs::remove_dir_all(home.path().join("extensions/demo")).unwrap();
    let before = fs::read(&state).unwrap();
    assert!(matches!(
        r.request_enable("demo", None),
        Err(crossagents_core::CoreError::NotFound { .. })
    ));
    assert_eq!(fs::read(&state).unwrap(), before);
}
#[test]
fn extensions_reenable_current_preserves_pending_and_ignores_bad_or_missing_source() {
    let home = tempfile::tempdir().unwrap();
    source(home.path(), "first");
    let r = Registry::load(home.path()).unwrap();
    let current = pending(&r);
    r.enable("demo", &current).unwrap();
    r.disable("demo").unwrap();
    fs::write(
        home.path().join("extensions/demo/tool.txt"),
        "second version",
    )
    .unwrap();
    r.scan().unwrap();
    let pending = r.get("demo").unwrap().pending;
    let info = r.enable("demo", &current).unwrap();
    assert!(info.enabled);
    assert_eq!(info.pending, pending);
    assert_eq!(info.current.unwrap().fingerprint, current);
    for delete in [false, true] {
        r.disable("demo").unwrap();
        if delete {
            fs::remove_dir_all(home.path().join("extensions/demo")).unwrap();
        } else {
            fs::write(home.path().join("extensions/demo/extension.toml"), "broken").unwrap();
        }
        r.scan().unwrap();
        let before = r.get("demo").unwrap();
        assert!(before.error.is_some());
        let info = r.enable("demo", &current).unwrap();
        assert!(info.enabled);
        assert_eq!(info.pending, before.pending);
        assert_eq!(r.enabled("demo").unwrap().version.fingerprint, current);
    }
}
#[test]
fn extensions_groups_validate_tools_and_prompts_with_clear_errors() {
    use crossagents_hub::extensions::manifest::Manifest;
    let home = tempfile::tempdir().unwrap();
    source(home.path(), "first");
    let root = home.path().join("extensions/demo");
    let path = root.join("extension.toml");
    let valid = fs::read_to_string(&path).unwrap();
    fs::write(root.join("prompt.md"), "instructions").unwrap();
    for group in ["everyone", "lead", "assignee", "reviewer", "conversation"] {
        let manifest = format!(
            "{}\n[[prompts]]\nfile = \"prompt.md\"\ngroups = [\"{group}\"]\n",
            valid.replace(
                "groups = [\"human\"]",
                &format!("groups = [\"{group}\", \"human\"]")
            )
        );
        fs::write(&path, manifest).unwrap();
        Manifest::read(&root, "demo").unwrap();
    }
    fs::write(&path, valid.replace("human", "leadd")).unwrap();
    let error = Manifest::read(&root, "demo").unwrap_err().to_string();
    assert!(error.contains("tool report groups"));
    assert!(error.contains("leadd"));
    assert!(error.contains("accepted groups"));
    for group in ["human", "worker", "LEAD"] {
        fs::write(
            &path,
            format!("{valid}\n[[prompts]]\nfile = \"prompt.md\"\ngroups = [\"{group}\"]\n"),
        )
        .unwrap();
        let error = Manifest::read(&root, "demo").unwrap_err().to_string();
        assert!(error.contains("prompt prompt.md groups"));
        assert!(error.contains(group));
        assert!(error.contains("accepted groups"));
    }
}
#[test]
fn extensions_unchanged_records_do_not_rewrite_state_after_bad_or_deleted_source() {
    let home = tempfile::tempdir().unwrap();
    source(home.path(), "first");
    let r = Registry::load(home.path()).unwrap();
    r.enable("demo", &pending(&r)).unwrap();
    let state = home.path().join("extensions.json");
    for delete in [false, true] {
        if delete {
            fs::remove_dir_all(home.path().join("extensions/demo")).unwrap();
        } else {
            fs::write(home.path().join("extensions/demo/extension.toml"), "broken").unwrap();
        }
        r.scan().unwrap();
        // Fixed timestamp detects rewrites without depending on filesystem clock resolution.
        let sentinel =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        fs::File::options()
            .write(true)
            .open(&state)
            .unwrap()
            .set_modified(sentinel)
            .unwrap();
        let before = fs::metadata(&state).unwrap().modified().unwrap();
        for _ in 0..3 {
            r.scan().unwrap();
        }
        assert_eq!(fs::metadata(&state).unwrap().modified().unwrap(), before);
        // No-op mutations should avoid writes too.
        r.disable("demo").unwrap();
        let before = fs::metadata(&state).unwrap().modified().unwrap();
        r.disable("demo").unwrap();
        assert_eq!(fs::metadata(&state).unwrap().modified().unwrap(), before);
    }
}
