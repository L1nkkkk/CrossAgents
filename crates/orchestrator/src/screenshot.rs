//! `gui_screenshot` (docs/tasks/DOGFOOD.md §3): an agent working on the GUI sees what its
//! change looks like. The hub runs `apps/gui/scripts/screenshot.mjs` in the caller's working
//! directory (the mock preview, no hub involved) and returns the PNG as an image.

use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use crossagents_core::process::NoWindow;
use crossagents_core::{Agent, CoreError};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;

use crate::engine::Orchestrator;

const SCRIPT: &str = "apps/gui/scripts/screenshot.mjs";
const TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Deserialize)]
struct Args {
    #[serde(default = "default_scenario")]
    scenario: String,
    #[serde(default)]
    route: Option<String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    theme: Option<String>,
    #[serde(default)]
    wait_ms: Option<u32>,
}

fn default_scenario() -> String {
    "running".into()
}

/// The script's options for `a`, checked: they become arguments, never a shell line.
fn script_args(a: &Args, out: &Path) -> Result<Vec<String>, CoreError> {
    let word = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    if !word(&a.scenario) {
        return Err(CoreError::Conflict(format!(
            "not a scenario key: {:?}",
            a.scenario
        )));
    }
    let mut args = vec![
        "--scenario".to_owned(),
        a.scenario.clone(),
        "--out".to_owned(),
        out.display().to_string(),
        "--width".to_owned(),
        a.width.unwrap_or(1280).clamp(320, 2560).to_string(),
        "--height".to_owned(),
        a.height.unwrap_or(800).clamp(320, 1600).to_string(),
        "--wait".to_owned(),
        a.wait_ms.unwrap_or(1500).min(10_000).to_string(),
    ];
    if let Some(theme) = &a.theme {
        if theme != "light" && theme != "dark" {
            return Err(CoreError::Conflict("theme is \"light\" or \"dark\"".into()));
        }
        args.extend(["--theme".to_owned(), theme.clone()]);
    }
    if let Some(route) = a.route.as_deref().filter(|r| !r.is_empty()) {
        args.extend(["--route".to_owned(), route.to_owned()]);
    }
    Ok(args)
}

impl Orchestrator {
    pub(crate) async fn gui_screenshot(
        &self,
        caller: &Agent,
        input: Value,
    ) -> Result<Value, CoreError> {
        let a: Args = serde_json::from_value(input)
            .map_err(|e| CoreError::Conflict(format!("invalid arguments: {e}")))?;
        let gui = caller.cwd.join("apps").join("gui");
        if !caller.cwd.join(SCRIPT).exists() {
            return Err(CoreError::Conflict(format!(
                "gui_screenshot works in a CrossAgents checkout; {} has no {SCRIPT}",
                caller.cwd.display()
            )));
        }
        if !gui.join("node_modules").exists() {
            return Err(CoreError::Conflict(format!(
                "the GUI's dependencies are not installed; run `npm --prefix apps/gui ci` in {} first",
                caller.cwd.display()
            )));
        }
        let dir = std::env::temp_dir().join("crossagents-shots");
        std::fs::create_dir_all(&dir).map_err(|e| CoreError::Storage(e.to_string()))?;
        let out = dir.join(format!(
            "{}-{}.png",
            caller.id,
            chrono::Utc::now().timestamp_millis()
        ));
        let mut command = Command::new("node");
        command
            .no_window()
            .arg(SCRIPT)
            .args(script_args(&a, &out)?)
            .current_dir(&caller.cwd)
            .kill_on_drop(true);
        let output = tokio::time::timeout(TIMEOUT, command.output())
            .await
            .map_err(|_| {
                CoreError::Agent(format!("the screenshot took over {}s", TIMEOUT.as_secs()))
            })?
            .map_err(|e| CoreError::Agent(format!("cannot run node: {e}")))?;
        if !output.status.success() {
            let mut why = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            why.truncate(4000);
            return Err(CoreError::Agent(format!("the screenshot failed: {why}")));
        }
        let bytes = std::fs::read(&out).map_err(|e| CoreError::Storage(e.to_string()))?;
        Ok(json!({
            "path": out,
            "scenario": a.scenario,
            "images": [{ "mime": "image/png", "data": base64::engine::general_purpose::STANDARD.encode(bytes) }],
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: Value) -> Args {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn arguments_are_checked_and_bounded() {
        let out = Path::new("shot.png");
        let list = script_args(
            &args(json!({ "scenario": "review", "width": 99999, "theme": "dark" })),
            out,
        )
        .unwrap();
        assert!(list.windows(2).any(|w| w == ["--width", "2560"]));
        assert!(list.windows(2).any(|w| w == ["--theme", "dark"]));
        assert!(script_args(&args(json!({ "scenario": "a; rm -rf /" })), out).is_err());
        assert!(script_args(&args(json!({ "theme": "blue" })), out).is_err());
        let default = script_args(&args(json!({})), out).unwrap();
        assert_eq!(default[1], "running");
    }
}
