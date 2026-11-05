//! Built-in authoring tools. Inspection never launches or imports extension code.
use super::{Registry, Runner};
use crossagents_core::CoreError;
use crossagents_orchestrator::host::ExtensionCaller;
use serde_json::{Value, json};

pub fn call(
    runner: &Runner,
    registry: &Registry,
    caller: &ExtensionCaller,
    tool: &str,
    input: Value,
) -> Option<Result<Value, CoreError>> {
    if tool == "extension_api" {
        return Some(Ok(api_at(&registry.sources())));
    }
    if !matches!(
        tool,
        "extension_check" | "extension_request_enable" | "extension_logs"
    ) {
        return None;
    }
    Some((|| {
        let name = input
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| CoreError::InvalidName("name must be a string".into()))?;
        let value = match tool {
            "extension_check" => serde_json::to_value(registry.check(name)?),
            "extension_request_enable" => {
                serde_json::to_value(registry.request_enable(name, Some(caller.agent.id))?)
            }
            "extension_logs" => serde_json::to_value(runner.logs(name)?),
            _ => unreachable!(),
        };
        value.map_err(|e| CoreError::Storage(e.to_string()))
    })())
}

pub fn api() -> Value {
    api_at(std::path::Path::new("<CrossAgents home>/extensions"))
}

/// The inventory, naming where this hub looks for sources: an agent that guesses
/// `~/.crossagents` writes to a directory this hub never reads when its home is elsewhere.
pub fn api_at(sources: &std::path::Path) -> Value {
    // Read the authoritative event wire names at compile time, keeping this inventory in sync.
    let events: Vec<_> = include_str!("../../../core/src/event.rs")
        .lines()
        .filter_map(|line| line.trim().strip_prefix("#[serde(rename = \""))
        .filter_map(|line| line.strip_suffix("\")]"))
        .collect();
    json!({
        "api": 1,
        "source_directory": sources.join("<name>").display().to_string(),
        "events": events,
        "groups": ["everyone", "lead", "assignee", "reviewer", "conversation", "human"],
        "groups_help": "Tools may include human for GUI calls; prompts accept only agent groups. Empty groups allow nobody. Groups are checked again at call time.",
        "gui_slots": ["tabs.register", "pages.register", "toolCards.register", "settings.register", "styles.add"],
        "gui_helpers": ["hub.request", "events.on", "tools.call", "React", "html", "useState"],
        "protocol": "One process per call, cwd is the approved snapshot. stdin: {tool,input,caller}; stdout: one JSON value; exit 0 succeeds, nonzero reports stderr. Missing input is {}. No shell expansion. CROSSAGENTS_URL, CROSSAGENTS_TOKEN (read/write per manifest), CROSSAGENTS_EXTENSION_DATA are provided only by the hub. Write state to the data directory. stdout limit 1 MiB; stderr tail 64 KiB; default timeout 60s, maximum 600s. Output images follow tool_content convention.",
        "approval": "Only humans can approve or set auto-trust. extension_check is static only. extension_request_enable snapshots and records a pending request; it never enables. There is no trial-run or agent approval tool. Event hooks and services are not implemented in API 1 yet.",
        "mcp_timeouts": {
            "measured_on": "2026-11-05 Windows; results apply only to these tested versions",
            "Codex 0.166.0-alpha.5.1": "70s succeeded; 650s failed with timed out awaiting tools/call after 300s. tool_timeout_sec was not tested.",
            "Claude Code 2.1.317": "70s and 650s succeeded",
            "CodeBuddy 2.179.2": "70s and 650s succeeded",
            "guidance": "Use timeout_secs below 300 or split long work into start + poll. CLI timeout does not cancel the hub process; it runs until its own deadline. GUI calls return 202 and poll /v1/extensions/calls/{id}; never automatically retry side effects."
        },
        "example": {
            "requires": "Node.js on PATH; create all three files below in source_directory, with hello as <name>",
            "files": {
                "extension.toml": r#"name = "hello"
api = 1
description = "A greeting tool and panel"
access = "read"
[[tools]]
name = "greet"
description = "Greet someone"
groups = ["everyone", "human"]
command = ["node", "greet.mjs"]
input_schema = { type = "object", properties = { name = { type = "string" } }, additionalProperties = false }
[gui]
entry = "gui/index.js"
"#,
                "greet.mjs": r#"let text = "";
for await (const chunk of process.stdin) text += chunk;
const { input = {} } = JSON.parse(text);
console.error("greeting requested");
console.log(JSON.stringify({ text: `Hello, ${input.name ?? "world"}!` }));
"#,
                "gui/index.js": r#"export function activate(ca) {
  ca.tabs.register({ id: "hello", label: "Hello", render: () => {
    const [message, setMessage] = ca.useState("Ready");
    return ca.html`<div><button onClick=${() => ca.tools.call("greet", {name: "world"}).then(result => setMessage(result.text))}>Greet</button><p>${message}</p></div>`;
  }});
}
"#
            },
            "self_test": {
                "instructions": "From your own shell, cd to the source hello directory and feed stdin yourself. This uses your identity and permissions, outside the hub, without an extension token. No hub trial tool exists.",
                "powershell": r#"'{"tool":"greet","input":{"name":"Ada"},"caller":{"kind":"human"}}' | node greet.mjs"#,
                "posix": r#"printf '%s' '{"tool":"greet","input":{"name":"Ada"},"caller":{"kind":"human"}}' | node greet.mjs"#,
                "expected_stdout": {"text": "Hello, Ada!"},
                "next": "extension_check(name=hello), then extension_request_enable(name=hello); a human reviews the pending snapshot in the GUI."
            }
        }
    })
}
