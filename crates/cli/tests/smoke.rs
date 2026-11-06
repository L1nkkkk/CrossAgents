//! `crossagents hub sandbox smoke` against the binary this crate builds (docs/tasks/SELF.md
//! §C3): the check `crossagents.toml` runs for a change to the hub, run here as a test with
//! the `crossagents` build in `target/debug`.
//!
//! The end-to-end one is `--ignored`: it starts a hub and an agent process, and needs the mock
//! agent built beside this crate's binary (`cargo build -p crossagents-mock-agent`). The cheap
//! one runs with the rest: a smoke that cannot find its mock agent fails, and says so.

use std::path::PathBuf;
use std::process::{Command, Output};

/// The `crossagents` binary of this build.
fn crossagents() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_crossagents"))
}

/// The mock agent built beside it.
fn mock_agent() -> PathBuf {
    crossagents().with_file_name(format!(
        "crossagents-mock-agent{}",
        std::env::consts::EXE_SUFFIX
    ))
}

fn smoke(dir: &std::path::Path, mock: &std::path::Path) -> Output {
    Command::new(crossagents())
        .args(["hub", "sandbox", "smoke", "--dir"])
        .arg(dir)
        .arg("--mock-agent")
        .arg(mock)
        .output()
        .expect("the crossagents binary runs")
}

#[test]
fn a_smoke_without_its_mock_agent_fails_and_says_how_to_build_it() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("smoke");
    let output = smoke(&dir, &home.path().join("no-such-mock-agent"));
    assert!(
        !output.status.success(),
        "a failed smoke exits non-zero:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("crossagents-mock-agent"), "{stderr}");
    assert!(
        stderr.contains("cargo build -p crossagents-mock-agent"),
        "{stderr}"
    );
    // Nothing was created, so the next attempt is not told its home is in the way.
    assert!(!dir.exists(), "{} was created", dir.display());
}

#[test]
#[ignore = "starts a hub and a mock agent; run with --ignored after `cargo build -p crossagents-mock-agent`"]
fn a_build_of_the_hub_starts_an_agent_and_carries_a_message() {
    let mock = mock_agent();
    assert!(
        mock.is_file(),
        "build the mock agent first: cargo build -p crossagents-mock-agent (looked for {})",
        mock.display()
    );
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("smoke");
    let output = smoke(&dir, &mock);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the smoke failed:\n{stdout}\n{stderr}"
    );
    assert!(stdout.contains("hub started"), "{stdout}");
    assert!(stdout.contains("message answered"), "{stdout}");
    assert!(stdout.contains("sandbox smoke ok"), "{stdout}");
}
