//! Directives the mock agent executes, written anywhere in a prompt:
//!
//! - `[write PATH=CONTENT]`  write a file (relative to the session cwd; `\n` for newlines)
//! - `[call TOOL {JSON}]`    call a tool on the session's first MCP server
//!
//! Qualifiers right after the keyword limit a directive: `@N` only on attempt N,
//! `#MODEL` only when the session's model is MODEL (e.g. `[write@2 a.txt=ok]`).
//! `{task_id}` and `{attempt}` in a directive are replaced from the prompt's
//! `task_id: ...` / `attempt: ...` lines.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Write { path: String, content: String },
    Call { tool: String, args: Value },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Directive {
    pub action: Action,
    pub attempt: Option<u32>,
    pub model: Option<String>,
}

impl Directive {
    pub fn applies(&self, attempt: u32, model: &str) -> bool {
        self.attempt.is_none_or(|a| a == attempt)
            && self.model.as_deref().is_none_or(|m| m == model)
    }
}

/// Values of `task_id:` and `attempt:` lines in a prompt.
pub fn context(text: &str) -> (Option<String>, u32) {
    let mut task_id = None;
    let mut attempt = 1;
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("task_id:") {
            task_id = Some(v.trim().to_owned());
        } else if let Some(v) = line.strip_prefix("attempt:") {
            attempt = v.trim().parse().unwrap_or(1);
        }
    }
    (task_id, attempt)
}

/// Fills placeholders; without a task context they stay, so a planner can pass them on
/// inside a plan.
fn substitute(text: &str, task_id: Option<&str>, attempt: u32) -> String {
    match task_id {
        Some(id) => text
            .replace("{task_id}", id)
            .replace("{attempt}", &attempt.to_string()),
        None => text.to_owned(),
    }
}

/// Parses `@N` / `#model` qualifiers; returns them and the rest after the separating space.
fn qualifiers(rest: &str) -> Option<(Option<u32>, Option<String>, &str)> {
    let end = rest.find(' ')?;
    let (quals, rest) = (&rest[..end], &rest[end + 1..]);
    let mut attempt = None;
    let mut model = None;
    let mut cur = quals;
    while !cur.is_empty() {
        let kind = cur.chars().next()?;
        let next = cur[1..].find(['@', '#']).map_or(cur.len(), |i| i + 1);
        let value = &cur[1..next];
        match kind {
            '@' => attempt = value.parse().ok(),
            '#' => model = Some(value.to_owned()),
            _ => return None,
        }
        cur = &cur[next..];
    }
    Some((attempt, model, rest))
}

/// Every directive in `text`, in order. Directives inside a `[call ...]`'s JSON are part of
/// that call and not returned on their own.
pub fn parse(text: &str, task_id: Option<&str>, attempt: u32) -> Vec<Directive> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < text.len() {
        let next_write = text[pos..].find("[write");
        let next_call = text[pos..].find("[call");
        let (start, is_call) = match (next_write, next_call) {
            (Some(w), Some(c)) if c < w => (pos + c, true),
            (Some(w), _) => (pos + w, false),
            (None, Some(c)) => (pos + c, true),
            (None, None) => break,
        };
        let body_start = start + if is_call { 5 } else { 6 };
        let Some((attempt_q, model_q, rest)) = qualifiers(&text[body_start..]) else {
            pos = body_start;
            continue;
        };
        let rest_offset = text.len() - rest.len();
        if is_call {
            let Some(space) = rest.find(' ') else {
                pos = body_start;
                continue;
            };
            let tool = rest[..space].to_owned();
            let json_text = &rest[space + 1..];
            let mut stream = serde_json::Deserializer::from_str(json_text).into_iter::<Value>();
            match stream.next() {
                Some(Ok(args)) => {
                    let consumed = stream.byte_offset();
                    let after = rest_offset + space + 1 + consumed;
                    let close = text[after..]
                        .find(']')
                        .map_or(text.len(), |i| after + i + 1);
                    let args = substitute(&args.to_string(), task_id, attempt);
                    out.push(Directive {
                        action: Action::Call {
                            tool,
                            args: serde_json::from_str(&args).unwrap_or(Value::Null),
                        },
                        attempt: attempt_q,
                        model: model_q,
                    });
                    pos = close;
                }
                _ => pos = body_start,
            }
        } else {
            let Some(end) = rest.find(']') else { break };
            let body = &rest[..end];
            if let Some((path, content)) = body.split_once('=') {
                out.push(Directive {
                    action: Action::Write {
                        path: path.trim().to_owned(),
                        content: substitute(content, task_id, attempt).replace("\\n", "\n"),
                    },
                    attempt: attempt_q,
                    model: model_q,
                });
            }
            pos = rest_offset + end + 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_writes_calls_and_qualifiers() {
        let text = "do [write a.txt=hi\\nthere] then [write@2#mock-opus b.txt=x] \
                    and [call task_complete {\"task_id\":\"{task_id}\",\"summary\":\"done [ok]\"}] end";
        let d = parse(text, Some("T1"), 1);
        assert_eq!(d.len(), 3);
        assert_eq!(
            d[0].action,
            Action::Write {
                path: "a.txt".into(),
                content: "hi\nthere".into()
            }
        );
        assert_eq!(d[1].attempt, Some(2));
        assert_eq!(d[1].model.as_deref(), Some("mock-opus"));
        assert_eq!(
            d[2].action,
            Action::Call {
                tool: "task_complete".into(),
                args: json!({ "task_id": "T1", "summary": "done [ok]" })
            }
        );
        assert!(d[0].applies(3, "x"));
        assert!(!d[1].applies(1, "mock-opus"));
        assert!(!d[1].applies(2, "mock-haiku"));
        assert!(d[1].applies(2, "mock-opus"));
    }

    #[test]
    fn nested_directives_stay_inside_the_call() {
        let text =
            r#"[call plan_submit {"tasks":[{"key":"a","spec":{"goal":"[write a.txt=1]"}}]}]"#;
        let d = parse(text, None, 1);
        assert_eq!(d.len(), 1);
        assert!(matches!(&d[0].action, Action::Call { tool, .. } if tool == "plan_submit"));

        let text =
            r#"[call plan_submit {"goal":"[call task_complete {\"task_id\":\"{task_id}\"}]"}]"#;
        let Action::Call { args, .. } = &parse(text, None, 1)[0].action else {
            unreachable!()
        };
        assert!(args["goal"].as_str().unwrap().contains("{task_id}"));
    }

    #[test]
    fn nested_arrays_in_call_arguments_do_not_end_the_directive() {
        let text = r#"[call plan_submit {"tasks":[{"key":"a","spec":{"goal":"[call task_complete {\"result\":{\"files\":[\"a.txt\"]}}]","result_schema":{"required":["files"]}}}]}]"#;
        let parsed = parse(text, None, 1);
        assert_eq!(parsed.len(), 1);
        let Action::Call { tool, args } = &parsed[0].action else {
            panic!("expected call")
        };
        assert_eq!(tool, "plan_submit");
        assert_eq!(
            args["tasks"][0]["spec"]["result_schema"]["required"][0],
            "files"
        );
        assert_eq!(
            parse(args["tasks"][0]["spec"]["goal"].as_str().unwrap(), None, 1).len(),
            1
        );
    }

    #[test]
    fn reads_context_lines() {
        assert_eq!(
            context("x\ntask_id: abc\nattempt: 3\n"),
            (Some("abc".into()), 3)
        );
        assert_eq!(context("nothing"), (None, 1));
    }
}
