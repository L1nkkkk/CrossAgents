//! `crossagents mcp`: the stdio MCP server mounted into every agent session. It is a thin
//! proxy; tool definitions and logic live in the hub (`/v1/me/tools`).

use std::sync::Arc;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use crossagents_api_types::{ApiError, ListToolsResponse};
use crossagents_drivers::jsonrpc::{RpcError, RpcHandler, RpcPeer};
use serde_json::{Value, json};

use crossagents_hub::collab::{TOKEN_ENV, URL_ENV};

const PROTOCOL_VERSION: &str = "2025-06-18";

struct McpProxy {
    http: reqwest::Client,
    url: String,
    token: String,
}

impl McpProxy {
    async fn list_tools(&self) -> Result<Value, RpcError> {
        let resp = self
            .http
            .get(format!("{}/v1/me/tools", self.url))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(internal)?;
        if !resp.status().is_success() {
            return Err(RpcError::new(
                -32000,
                format!("hub returned {}", resp.status()),
            ));
        }
        let list: ListToolsResponse = resp.json().await.map_err(internal)?;
        let tools: Vec<Value> = list
            .tools
            .into_iter()
            .map(|t| json!({ "name": t.name, "description": t.description, "inputSchema": t.input_schema }))
            .collect();
        Ok(json!({ "tools": tools }))
    }

    async fn call_tool(&self, params: Value) -> Result<Value, RpcError> {
        let name = params["name"]
            .as_str()
            .ok_or_else(|| RpcError::invalid_params("missing tool name"))?;
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
        let resp = self
            .http
            .post(format!("{}/v1/me/tools/{name}", self.url))
            .bearer_auth(&self.token)
            .json(&arguments)
            .send()
            .await;
        // Tool failures are reported to the model as results, not protocol errors.
        let (text, is_error) = match resp {
            Err(err) => (format!("cannot reach the CrossAgents hub: {err}"), true),
            Ok(resp) if resp.status().is_success() => {
                let value: Value = resp.json().await.unwrap_or(Value::Null);
                return Ok(json!({ "content": tool_content(value), "isError": false }));
            }
            Ok(resp) => {
                let status = resp.status();
                match resp.json::<ApiError>().await {
                    Ok(err) => (err.error.message, true),
                    Err(_) => (format!("hub returned {status}"), true),
                }
            }
        };
        Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }))
    }
}

/// A tool's result as MCP content: the JSON as text, and its `images` (`{ mime, data }`,
/// base64) as image blocks the model sees (docs/tasks/DOGFOOD.md §3).
fn tool_content(mut value: Value) -> Vec<Value> {
    let images = value
        .as_object_mut()
        .and_then(|o| o.remove("images"))
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    let mut content = vec![
        json!({ "type": "text", "text": serde_json::to_string_pretty(&value).unwrap_or_default() }),
    ];
    content.extend(images.iter().filter_map(|image| {
        Some(json!({ "type": "image", "mimeType": image["mime"].as_str()?, "data": image["data"].as_str()? }))
    }));
    content
}

fn internal(err: impl std::fmt::Display) -> RpcError {
    RpcError::new(-32603, err.to_string())
}

#[async_trait]
impl RpcHandler for McpProxy {
    async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => Ok(json!({
                "protocolVersion": params["protocolVersion"].as_str().unwrap_or(PROTOCOL_VERSION),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "crossagents", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "CrossAgents collaboration tools: talk to other agents, submit plans, and report task results."
            })),
            "ping" => Ok(json!({})),
            "tools/list" => self.list_tools().await,
            "tools/call" => self.call_tool(params).await,
            _ => Err(RpcError::method_not_found(method)),
        }
    }

    async fn notification(&self, _method: &str, _params: Value) {}
}

pub async fn run() -> Result<()> {
    let url = std::env::var(URL_ENV).with_context(|| format!("{URL_ENV} is not set"))?;
    let token = std::env::var(TOKEN_ENV).with_context(|| format!("{TOKEN_ENV} is not set"))?;
    let proxy = Arc::new(McpProxy {
        http: reqwest::Client::new(),
        url,
        token,
    });
    let (_peer, reader) = RpcPeer::spawn(tokio::io::stdin(), tokio::io::stdout(), proxy);
    let _ = reader.await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_in_a_result_become_image_blocks() {
        let content = tool_content(json!({
            "path": "shot.png",
            "images": [{ "mime": "image/png", "data": "aW1n" }],
        }));
        assert_eq!(content.len(), 2);
        assert!(!content[0]["text"].as_str().unwrap().contains("aW1n"));
        assert_eq!(
            content[1],
            json!({ "type": "image", "mimeType": "image/png", "data": "aW1n" })
        );
        assert_eq!(tool_content(json!({ "ok": true })).len(), 1);
    }
}
