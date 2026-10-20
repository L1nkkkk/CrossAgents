//! The subset of ACP v1 (https://agentclientprotocol.com) that CrossAgents speaks, as client.
//! Unknown fields are ignored so newer agents keep working.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u16 = 1;

pub mod method {
    pub const INITIALIZE: &str = "initialize";
    pub const SESSION_NEW: &str = "session/new";
    pub const SESSION_SET_MODE: &str = "session/set_mode";
    pub const SESSION_SET_CONFIG_OPTION: &str = "session/set_config_option";
    pub const SESSION_PROMPT: &str = "session/prompt";
    pub const SESSION_CANCEL: &str = "session/cancel";
    pub const SESSION_UPDATE: &str = "session/update";
    pub const SESSION_REQUEST_PERMISSION: &str = "session/request_permission";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Implementation {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSystemCapabilities {
    pub read_text_file: bool,
    pub write_text_file: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    pub fs: FileSystemCapabilities,
    pub terminal: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequest {
    pub protocol_version: u16,
    pub client_capabilities: ClientCapabilities,
    pub client_info: Implementation,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCapabilities {
    #[serde(default)]
    pub http: bool,
    #[serde(default)]
    pub sse: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(default)]
    pub load_session: bool,
    #[serde(default)]
    pub mcp_capabilities: McpCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub protocol_version: u16,
    #[serde(default)]
    pub agent_capabilities: AgentCapabilities,
    #[serde(default)]
    pub agent_info: Option<Implementation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvVariable {
    pub name: String,
    pub value: String,
}

/// Stdio MCP server (the variant without a `type` tag).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerStdio {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<EnvVariable>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionRequest {
    pub cwd: String,
    pub mcp_servers: Vec<McpServerStdio>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMode {
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModeState {
    pub current_mode_id: String,
    #[serde(default)]
    pub available_modes: Vec<SessionMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResponse {
    pub session_id: String,
    #[serde(default)]
    pub modes: Option<SessionModeState>,
    /// Kept loose: see [`find_select_option`].
    #[serde(default)]
    pub config_options: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetSessionModeRequest {
    pub session_id: String,
    pub mode_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetSessionConfigOptionRequest {
    pub session_id: String,
    pub config_id: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptRequest {
    pub session_id: String,
    pub prompt: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResponse {
    pub stop_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelNotification {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotification {
    pub session_id: String,
    /// Parsed by `sessionUpdate` in the session handler; unknown kinds are skipped.
    pub update: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub option_id: String,
    #[serde(default)]
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionRequest {
    pub session_id: String,
    #[serde(default)]
    pub tool_call: Value,
    pub options: Vec<PermissionOption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PermissionOutcome {
    Cancelled,
    Selected {
        #[serde(rename = "optionId")]
        option_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestPermissionResponse {
    pub outcome: PermissionOutcome,
}

/// A selectable value of a session config option.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectValue {
    pub value: String,
    pub name: String,
}

/// A `select` config option (e.g. the model picker).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectOption {
    pub id: String,
    pub current: String,
    pub values: Vec<SelectValue>,
}

/// Finds the first `select` option matching `pred(id, category)`, flattening grouped values.
pub fn find_select_option(
    options: &[Value],
    pred: impl Fn(&str, Option<&str>) -> bool,
) -> Option<SelectOption> {
    options.iter().find_map(|opt| {
        let id = opt.get("id")?.as_str()?;
        let category = opt.get("category").and_then(Value::as_str);
        if opt.get("type").and_then(Value::as_str) != Some("select") || !pred(id, category) {
            return None;
        }
        let mut values = Vec::new();
        for item in opt.get("options")?.as_array()? {
            match item.get("options").and_then(Value::as_array) {
                Some(group) => values.extend(group.iter().filter_map(select_value)),
                None => values.extend(select_value(item)),
            }
        }
        Some(SelectOption {
            id: id.to_owned(),
            current: opt
                .get("currentValue")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            values,
        })
    })
}

fn select_value(item: &Value) -> Option<SelectValue> {
    let value = item.get("value")?.as_str()?.to_owned();
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(&value)
        .to_owned();
    Some(SelectValue { value, name })
}

impl SelectOption {
    /// Exact (case-insensitive) match on value or name first, then substring.
    pub fn resolve(&self, wanted: &str) -> Option<&SelectValue> {
        let wanted = wanted.to_lowercase();
        let exact = self
            .values
            .iter()
            .find(|v| v.value.to_lowercase() == wanted || v.name.to_lowercase() == wanted);
        exact.or_else(|| {
            self.values.iter().find(|v| {
                v.value.to_lowercase().contains(&wanted) || v.name.to_lowercase().contains(&wanted)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn permission_outcome_wire_format() {
        let selected = serde_json::to_value(PermissionOutcome::Selected {
            option_id: "allow".into(),
        })
        .unwrap();
        assert_eq!(
            selected,
            json!({ "outcome": "selected", "optionId": "allow" })
        );
        let cancelled = serde_json::to_value(PermissionOutcome::Cancelled).unwrap();
        assert_eq!(cancelled, json!({ "outcome": "cancelled" }));
    }

    #[test]
    fn finds_and_resolves_grouped_model_option() {
        let options = vec![
            json!({ "id": "mode", "name": "Mode", "type": "select", "currentValue": "a", "options": [] }),
            json!({
                "id": "model", "name": "Model", "category": "model", "type": "select",
                "currentValue": "default",
                "options": [
                    { "value": "default", "name": "Default (Sonnet)" },
                    { "group": "older", "name": "Older", "options": [
                        { "value": "claude-haiku-5-5", "name": "Haiku 5.5" }
                    ]}
                ]
            }),
        ];
        let model =
            find_select_option(&options, |id, cat| cat == Some("model") || id == "model").unwrap();
        assert_eq!(model.id, "model");
        assert_eq!(model.values.len(), 2);
        assert_eq!(model.resolve("haiku").unwrap().value, "claude-haiku-5-5");
        assert_eq!(model.resolve("DEFAULT").unwrap().value, "default");
        assert!(model.resolve("gpt").is_none());
    }

    #[test]
    fn content_block_is_tagged() {
        let block = serde_json::to_value(ContentBlock::Text { text: "hi".into() }).unwrap();
        assert_eq!(block, json!({ "type": "text", "text": "hi" }));
    }
}
