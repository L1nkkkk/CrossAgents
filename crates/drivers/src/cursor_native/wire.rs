//! Cursor NDJSON, as documented at https://cursor.com/docs/cli/reference/output-format.
//! Only timestamped assistant deltas without `model_call_id` contain new text.

use std::collections::HashMap;
use std::path::Path;

use crossagents_core::{
    OutputStream, SessionEvent, SessionEventKind, StopReason, ToolDetail, ToolStatus,
};
use serde_json::Value;

pub struct ChatArgs<'a> {
    pub session_id: &'a str,
    pub cwd: &'a Path,
    pub prompt: &'a str,
    pub model: Option<&'a str>,
    pub bypass: bool,
    pub plugin: Option<&'a Path>,
}

pub fn chat_args(args: &ChatArgs<'_>) -> Vec<String> {
    let mut out = vec![
        "-p".into(),
        "--output-format=stream-json".into(),
        "--stream-partial-output".into(),
        format!("--resume={}", args.session_id),
    ];
    if let Some(model) = args.model.filter(|m| !m.is_empty()) {
        out.push(format!("--model={model}"));
    }
    if args.bypass {
        out.push("--force".into());
    } else {
        out.push("--trust".into());
        out.push("--sandbox=enabled".into());
    }
    out.push(format!("--workspace={}", args.cwd.display()));
    if let Some(plugin) = args.plugin {
        out.push(format!("--plugin-dir={}", plugin.display()));
        out.push("--approve-mcps".into());
    }
    // The prompt is positional; terminate option parsing even when it begins with '-'.
    out.push("--".into());
    out.push(args.prompt.into());
    out
}

#[derive(Debug, PartialEq)]
pub enum Line {
    Events(Vec<SessionEvent>),
    Text(String),
    Skip,
}

#[derive(Debug, Default)]
pub struct Turn {
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub request_id: Option<String>,
    pub stop_reason: Option<StopReason>,
    /// Raw `usage` object from the terminal `result` event. Present in both
    /// installed CLI versions; headless mode never fires turn-level hooks, so
    /// this is the authoritative per-turn token source.
    pub usage: Option<Value>,
    tools: HashMap<String, Value>,
}

impl Turn {
    pub fn line(&mut self, line: &str) -> Line {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            return Line::Skip;
        }
        let Ok(raw) = serde_json::from_str::<Value>(line) else {
            return Line::Text(line.to_owned());
        };
        let mut events = Vec::new();
        match raw["type"].as_str().unwrap_or_default() {
            "system" if raw["subtype"] == "init" => {
                self.session_id = text(&raw, "session_id");
                self.cwd = text(&raw, "cwd");
                self.model = text(&raw, "model");
            }
            "assistant"
                if raw.get("timestamp_ms").is_some() && raw.get("model_call_id").is_none() =>
            {
                for content in raw["message"]["content"].as_array().into_iter().flatten() {
                    if let Some(text) = content["text"].as_str().filter(|s| !s.is_empty()) {
                        events.push(SessionEventKind::Output {
                            stream: OutputStream::Text,
                            text: text.into(),
                        });
                    }
                }
            }
            "tool_call" => events = self.tool(&raw),
            "result" => {
                self.session_id = text(&raw, "session_id").or(self.session_id.take());
                self.request_id = text(&raw, "request_id");
                let failed = raw["is_error"].as_bool().unwrap_or(false);
                self.stop_reason = Some(if failed {
                    StopReason::Error
                } else {
                    StopReason::EndTurn
                });
                if failed {
                    events.push(SessionEventKind::Error {
                        message: text(&raw, "result")
                            .unwrap_or_else(|| "Cursor reported an error".into()),
                    });
                }
                // `result` repeats all deltas; `duration_ms` is not token usage,
                // but `usage` carries the turn's token counts in both versions.
                self.usage = raw.get("usage").filter(|u| u.is_object()).cloned();
            }
            _ => {}
        }
        if events.is_empty() {
            Line::Skip
        } else {
            Line::Events(
                events
                    .into_iter()
                    .map(|kind| SessionEvent::with_raw(kind, raw.clone()))
                    .collect(),
            )
        }
    }

    fn tool(&mut self, raw: &Value) -> Vec<SessionEventKind> {
        let Some(id) = raw["call_id"].as_str() else {
            return vec![];
        };
        let started = raw["subtype"] == "started";
        let completed = raw["subtype"] == "completed";
        if !started && !completed {
            return vec![];
        }
        let call = raw
            .get("tool_call")
            .filter(|c| c.is_object())
            .cloned()
            .or_else(|| self.tools.get(id).cloned())
            .unwrap_or_default();
        let Some((variant, body)) = call.as_object().and_then(|o| o.iter().next()) else {
            return vec![];
        };
        let name = if variant == "function" {
            body["name"].as_str().unwrap_or("function")
        } else {
            variant
        };
        let kind = match variant.as_str() {
            "readToolCall" => "read",
            "writeToolCall" => "edit",
            _ => "other",
        };
        let args = body.get("args").or_else(|| body.get("arguments"));
        let result = body.get("result").or_else(|| raw.get("result"));
        let failed = result.is_some_and(|r| r.get("error").is_some() || r["is_error"] == true);
        let status = if started {
            ToolStatus::InProgress
        } else if failed {
            ToolStatus::Failed
        } else {
            ToolStatus::Completed
        };
        let mut events = vec![if started {
            SessionEventKind::ToolCall {
                tool_call_id: id.into(),
                title: name.into(),
                kind: Some(kind.into()),
                status,
            }
        } else {
            SessionEventKind::ToolCallUpdated {
                tool_call_id: id.into(),
                title: Some(name.into()),
                status: Some(status),
            }
        }];
        events.push(SessionEventKind::ToolDetail {
            tool_call_id: id.into(),
            detail: ToolDetail {
                arguments: args.map(json_text),
                result: result.map(json_text),
                error: failed.then(|| result.map(json_text).unwrap_or_default()),
                ..ToolDetail::default()
            },
        });
        if completed {
            self.tools.remove(id);
        } else {
            self.tools.insert(id.into(), call);
        }
        events
    }
}

fn text(value: &Value, key: &str) -> Option<String> {
    value[key].as_str().map(str::to_owned)
}
fn json_text(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn output(line: Line) -> String {
        match line {
            Line::Events(events) => events
                .into_iter()
                .filter_map(|e| match e.kind {
                    SessionEventKind::Output {
                        stream: OutputStream::Text,
                        text,
                    } => Some(text),
                    _ => None,
                })
                .collect(),
            _ => String::new(),
        }
    }

    #[test]
    fn init_and_result_from_official_ndjson() {
        let mut turn = Turn::default();
        assert_eq!(turn.line(r#"{"type":"system","subtype":"init","apiKeySource":"login","cwd":"/Users/user/project","session_id":"c6b62c6f-7ead-4fd6-9922-e952131177ff","model":"Claude 4 Sonnet","permissionMode":"default"}"#), Line::Skip);
        assert_eq!(turn.cwd.as_deref(), Some("/Users/user/project"));
        assert_eq!(turn.model.as_deref(), Some("Claude 4 Sonnet"));
        assert_eq!(
            turn.session_id.as_deref(),
            Some("c6b62c6f-7ead-4fd6-9922-e952131177ff")
        );
        assert_eq!(turn.line(r#"{"type":"result","subtype":"success","duration_ms":5234,"duration_api_ms":5234,"is_error":false,"result":"all text","session_id":"chat","request_id":"request"}"#), Line::Skip);
        assert_eq!(turn.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(turn.session_id.as_deref(), Some("chat"));
        assert_eq!(turn.request_id.as_deref(), Some("request"));
        assert!(
            matches!(turn.line(r#"{"type":"result","is_error":true,"result":"failed"}"#), Line::Events(e) if matches!(&e[0].kind, SessionEventKind::Error { message } if message == "failed"))
        );
        assert_eq!(turn.stop_reason, Some(StopReason::Error));
    }

    #[test]
    fn result_captures_usage_object_but_not_duration() {
        // Both installed CLI versions embed a `usage` object in the terminal
        // `result` event. It is the headless token source; `duration_ms` alone
        // must not be mistaken for usage.
        let mut with_usage = Turn::default();
        assert_eq!(
            with_usage.line(r#"{"type":"result","subtype":"success","duration_ms":5234,"duration_api_ms":5234,"is_error":false,"result":"ok","session_id":"chat","usage":{"inputTokens":7730,"outputTokens":22,"cacheReadTokens":3968,"cacheWriteTokens":0,"reasoningTokens":21}}"#),
            Line::Skip
        );
        let usage = with_usage.usage.expect("usage captured");
        assert_eq!(usage["inputTokens"], json!(7730));
        assert_eq!(usage["outputTokens"], json!(22));
        assert_eq!(usage["cacheReadTokens"], json!(3968));
        assert_eq!(usage["cacheWriteTokens"], json!(0));

        let mut without_usage = Turn::default();
        assert_eq!(
            without_usage.line(r#"{"type":"result","subtype":"success","duration_ms":5234,"is_error":false,"result":"ok","session_id":"chat"}"#),
            Line::Skip
        );
        assert!(without_usage.usage.is_none());
    }

    #[test]
    fn three_assistant_forms_only_delta_emits_text() {
        let mut turn = Turn::default();
        assert_eq!(output(turn.line(r#"{"type":"assistant","timestamp_ms":1,"message":{"content":[{"type":"text","text":"new"}]}}"#)), "new");
        assert_eq!(turn.line(r#"{"type":"assistant","timestamp_ms":2,"model_call_id":"m","message":{"content":[{"type":"text","text":"new"}]}}"#), Line::Skip);
        assert_eq!(
            turn.line(
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"new"}]}}"#
            ),
            Line::Skip
        );
    }

    #[test]
    fn read_write_and_function_preserve_arguments_and_results() {
        let mut turn = Turn::default();
        for (variant, args, result) in [
            (
                "readToolCall",
                json!({"path":"README.md"}),
                json!({"success":{"content":"# Project","totalLines":54,"totalChars":1254,"isEmpty":false,"exceededLimit":false}}),
            ),
            (
                "writeToolCall",
                json!({"path":"summary.txt","fileText":"# README Summary","toolCallId":"id"}),
                json!({"success":{"path":"/Users/user/project/summary.txt","linesCreated":19,"fileSize":942}}),
            ),
            (
                "function",
                json!("{\"query\":\"abc\"}"),
                json!({"success":"ok"}),
            ),
        ] {
            let mut body = json!({"args": args});
            if variant == "function" {
                body = json!({"name":"search","arguments":args});
            }
            let started = json!({"type":"tool_call","subtype":"started","call_id":"id","tool_call":{variant:body.clone()}});
            assert!(
                matches!(turn.line(&started.to_string()), Line::Events(e) if matches!(e[0].kind, SessionEventKind::ToolCall {status:ToolStatus::InProgress,..}))
            );
            body["result"] = result.clone();
            let completed = json!({"type":"tool_call","subtype":"completed","call_id":"id","tool_call":{variant:body}});
            let Line::Events(events) = turn.line(&completed.to_string()) else {
                panic!("tool events");
            };
            assert!(matches!(
                events[0].kind,
                SessionEventKind::ToolCallUpdated {
                    status: Some(ToolStatus::Completed),
                    ..
                }
            ));
            let SessionEventKind::ToolDetail { detail, .. } = &events[1].kind else {
                panic!("detail");
            };
            assert_eq!(detail.arguments, Some(json_text(&args)));
            assert_eq!(detail.result, Some(result.to_string()));
        }
    }

    #[test]
    fn whole_stream_never_repeats_flushes_or_result() {
        let lines = [
            r#"{"type":"assistant","timestamp_ms":1,"message":{"content":[{"text":"I'll read "},{"text":"README.md."}]}}"#,
            r#"{"type":"assistant","timestamp_ms":2,"model_call_id":"m","message":{"content":[{"text":"I'll read README.md."}]}}"#,
            r#"{"type":"tool_call","subtype":"started","call_id":"id","tool_call":{"readToolCall":{"args":{"path":"README.md"}}}}"#,
            r##"{"type":"tool_call","subtype":"completed","call_id":"id","tool_call":{"readToolCall":{"args":{"path":"README.md"},"result":{"success":{"content":"# Project"}}}}}"##,
            r#"{"type":"assistant","timestamp_ms":3,"message":{"content":[{"text":" Done!"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"text":"I'll read README.md. Done!"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":1234,"result":"I'll read README.md. Done!"}"#,
        ];
        let mut turn = Turn::default();
        let text: String = lines
            .into_iter()
            .map(|line| output(turn.line(line)))
            .collect();
        assert_eq!(text, "I'll read README.md. Done!");
        assert_eq!(turn.stop_reason, Some(StopReason::EndTurn));
    }
}
