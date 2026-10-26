//! The Anthropic Messages API.
//!
//! Request: `POST {base}/v1/messages` with `x-api-key` and `anthropic-version: 2023-06-01`; `system` is a top-level
//! string, `messages` alternate `user` and `assistant` (consecutive same-role messages are merged by the API), tools are
//! `{name, description, input_schema}`, a tool call is an assistant `tool_use` block and its answer is a `tool_result`
//! block in the next user message. Stream: `message_start` (input tokens), `content_block_start` / `content_block_delta`
//! (`text_delta`, `input_json_delta` with `partial_json`) / `content_block_stop`, `message_delta` (`stop_reason`,
//! output tokens), `message_stop`, `ping`, and `error` events.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::http::{Built, Parser};
use super::{ApiKey, ChatRequest, ErrorKind, Msg, Outcome, ProviderError, Stop, Target, ToolCall, Usage, estimate_tokens};

/// The API version header value.
pub const VERSION: &str = "2023-06-01";

fn messages(req: &ChatRequest) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut pending: Vec<Value> = Vec::new();
    let flush = |out: &mut Vec<Value>, pending: &mut Vec<Value>| {
        if !pending.is_empty() {
            out.push(json!({"role": "user", "content": std::mem::take(pending)}));
        }
    };
    for m in &req.messages {
        match m {
            Msg::ToolResult { call_id, content, is_error, .. } => {
                pending.push(json!({"type": "tool_result", "tool_use_id": call_id, "content": content, "is_error": is_error}));
            }
            Msg::User(t) => {
                flush(&mut out, &mut pending);
                out.push(json!({"role": "user", "content": [{"type": "text", "text": t}]}));
            }
            Msg::Assistant { text, calls, .. } => {
                flush(&mut out, &mut pending);
                let mut blocks: Vec<Value> = Vec::new();
                if !text.is_empty() {
                    blocks.push(json!({"type": "text", "text": text}));
                }
                for c in calls {
                    blocks.push(json!({"type": "tool_use", "id": c.id, "name": c.name, "input": c.args}));
                }
                if blocks.is_empty() {
                    blocks.push(json!({"type": "text", "text": "(no answer)"}));
                }
                out.push(json!({"role": "assistant", "content": blocks}));
            }
        }
    }
    flush(&mut out, &mut pending);
    out
}

/// Build the request.
pub fn build(target: &Target, key: Option<&ApiKey>, req: &ChatRequest) -> Built {
    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "stream": true,
        "system": req.system,
        "messages": messages(req),
    });
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(req.tools.iter().map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.schema})).collect());
    }
    let mut headers = vec![
        ("content-type".to_owned(), "application/json".to_owned()),
        ("accept".to_owned(), "text/event-stream".to_owned()),
        ("anthropic-version".to_owned(), VERSION.to_owned()),
    ];
    if let Some(k) = key {
        headers.push(("x-api-key".to_owned(), k.expose().to_owned()));
    }
    Built { url: format!("{}/v1/messages", target.base_url.trim().trim_end_matches('/')), headers, body }
}

/// The request for the models list.
pub fn models_request(target: &Target, key: Option<&ApiKey>) -> Built {
    let mut headers = vec![("anthropic-version".to_owned(), VERSION.to_owned())];
    if let Some(k) = key {
        headers.push(("x-api-key".to_owned(), k.expose().to_owned()));
    }
    Built { url: format!("{}/v1/models?limit=100", target.base_url.trim().trim_end_matches('/')), headers, body: Value::Null }
}

enum Block {
    Text,
    Tool { id: String, name: String, json: String },
    Ignored,
}

/// The stream parser.
#[derive(Default)]
pub struct AnthropicParser {
    blocks: BTreeMap<u64, Block>,
    text: String,
    input: u64,
    output: u64,
    stop: Option<String>,
    got_usage: bool,
}

impl AnthropicParser {
    /// A new parser.
    pub fn new() -> Self {
        Self::default()
    }
}

fn error_kind(t: &str) -> ErrorKind {
    match t {
        "authentication_error" | "permission_error" => ErrorKind::Auth,
        "rate_limit_error" => ErrorKind::RateLimited,
        "overloaded_error" => ErrorKind::Overloaded,
        "invalid_request_error" | "not_found_error" | "request_too_large" => ErrorKind::BadRequest,
        _ => ErrorKind::Server,
    }
}

impl Parser for AnthropicParser {
    fn event(&mut self, name: Option<&str>, data: &str, sink: &mut (dyn FnMut(&str) + Send)) -> Result<(), ProviderError> {
        if data.trim().is_empty() {
            return Ok(());
        }
        let v: Value = serde_json::from_str(data).map_err(|_| ProviderError::new(ErrorKind::Protocol, "The provider sent an event Zeus could not read."))?;
        let kind = v.get("type").and_then(Value::as_str).or(name).unwrap_or_default();
        match kind {
            "message_start" => {
                let u = v.pointer("/message/usage");
                let n = |k: &str| u.and_then(|u| u.get(k)).and_then(Value::as_u64).unwrap_or(0);
                self.input = n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens");
                self.output = n("output_tokens");
                self.got_usage = true;
            }
            "content_block_start" => {
                let index = v.get("index").and_then(Value::as_u64).unwrap_or(0);
                let b = v.get("content_block").cloned().unwrap_or(Value::Null);
                let block = match b.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = b.get("text").and_then(Value::as_str)
                            && !t.is_empty()
                        {
                            self.text.push_str(t);
                            sink(t);
                        }
                        Block::Text
                    }
                    Some("tool_use") => Block::Tool {
                        id: b.get("id").and_then(Value::as_str).unwrap_or_default().to_owned(),
                        name: b.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                        json: String::new(),
                    },
                    _ => Block::Ignored,
                };
                self.blocks.insert(index, block);
            }
            "content_block_delta" => {
                let index = v.get("index").and_then(Value::as_u64).unwrap_or(0);
                let d = v.get("delta").cloned().unwrap_or(Value::Null);
                match d.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(t) = d.get("text").and_then(Value::as_str) {
                            self.text.push_str(t);
                            sink(t);
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(Block::Tool { json, .. }) = self.blocks.get_mut(&index)
                            && let Some(p) = d.get("partial_json").and_then(Value::as_str)
                        {
                            json.push_str(p);
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(s) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop = Some(s.to_owned());
                }
                if let Some(o) = v.pointer("/usage/output_tokens").and_then(Value::as_u64) {
                    self.output = o;
                    self.got_usage = true;
                }
                if let Some(i) = v.pointer("/usage/input_tokens").and_then(Value::as_u64)
                    && i > 0
                {
                    self.input = i;
                }
            }
            "error" => {
                let t = v.pointer("/error/type").and_then(Value::as_str).unwrap_or_default();
                let m = v.pointer("/error/message").and_then(Value::as_str).unwrap_or("The provider reported an error.");
                return Err(ProviderError::new(error_kind(t), format!("The provider reported an error mid-answer: {}", m.chars().take(300).collect::<String>())));
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(self: Box<Self>, estimated_input: u64) -> Result<Outcome, ProviderError> {
        let mut calls = Vec::new();
        for (n, (_, b)) in self.blocks.into_iter().enumerate() {
            if let Block::Tool { id, name, json } = b {
                if name.is_empty() {
                    continue;
                }
                let (args, args_error) = if json.trim().is_empty() {
                    (json!({}), None)
                } else {
                    match serde_json::from_str::<Value>(&json) {
                        Ok(v) if v.is_object() => (v, None),
                        _ => (json!({}), Some("The arguments were not a JSON object.".to_owned())),
                    }
                };
                calls.push(ToolCall { id: if id.is_empty() { format!("call_{}", n + 1) } else { id }, name, args, args_error });
            }
        }
        let stop = match self.stop.as_deref() {
            Some("tool_use") => Stop::ToolUse,
            Some("max_tokens") => Stop::Length,
            Some("refusal") => Stop::Filtered,
            Some("end_turn" | "stop_sequence") => Stop::End,
            Some(_) => Stop::Other,
            None => {
                if calls.is_empty() { Stop::End } else { Stop::ToolUse }
            }
        };
        let usage = if self.got_usage {
            Usage { input: self.input, output: self.output, estimated: false }
        } else {
            Usage { input: estimated_input, output: estimate_tokens(&self.text), estimated: true }
        };
        Ok(Outcome { text: self.text, calls, raw: None, usage, stop })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{Kind, ToolSpec};

    fn feed(events: &[(&str, &str)]) -> Result<Outcome, ProviderError> {
        let mut p = Box::new(AnthropicParser::new());
        for (n, d) in events {
            p.event(Some(n), d, &mut |_| ())?;
        }
        p.finish(5)
    }

    #[test]
    fn text_and_tool_use_with_partial_json() {
        let o = feed(&[
            ("message_start", r#"{"type":"message_start","message":{"usage":{"input_tokens":472,"output_tokens":2}}}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
            ("ping", r#"{"type":"ping"}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Let me check."}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"zeus_probe","input":{}}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":""}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"url\": \"https://e.org/"}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"a\"}"}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":1}"#),
            ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":89}}"#),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ])
        .unwrap();
        assert_eq!(o.text, "Let me check.");
        assert_eq!(o.stop, Stop::ToolUse);
        assert_eq!(o.calls[0].id, "toolu_1");
        assert_eq!(o.calls[0].args["url"], "https://e.org/a");
        assert_eq!(o.usage, Usage { input: 472, output: 89, estimated: false });
    }

    #[test]
    fn an_overloaded_error_event_is_an_error() {
        let e = feed(&[("error", r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#)]).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Overloaded);
    }

    #[test]
    fn tool_results_merge_into_one_user_message_and_follow_their_call() {
        let req = ChatRequest {
            model: "m".into(),
            system: "sys".into(),
            messages: vec![
                Msg::User("go".into()),
                Msg::Assistant { text: "ok".into(), calls: vec![ToolCall { id: "a".into(), name: "zeus_status".into(), args: json!({}), args_error: None }, ToolCall { id: "b".into(), name: "zeus_probe".into(), args: json!({}), args_error: None }], raw: None },
                Msg::ToolResult { call_id: "a".into(), name: "zeus_status".into(), content: "r1".into(), is_error: false },
                Msg::ToolResult { call_id: "b".into(), name: "zeus_probe".into(), content: "r2".into(), is_error: true },
                Msg::User("thanks".into()),
            ],
            tools: vec![ToolSpec { name: "zeus_status".into(), description: "d".into(), schema: json!({"type": "object"}) }],
            max_tokens: 100,
        };
        let key = ApiKey::new("sk-ant-test-0000".into());
        let b = build(&Target { kind: Kind::Anthropic, base_url: "https://api.anthropic.com".into(), json_tools: false }, Some(&key), &req);
        assert_eq!(b.url, "https://api.anthropic.com/v1/messages");
        assert!(b.headers.iter().any(|(k, v)| k == "x-api-key" && v == "sk-ant-test-0000"));
        assert!(b.headers.iter().any(|(k, v)| k == "anthropic-version" && v == VERSION));
        let m = b.body["messages"].as_array().unwrap();
        assert_eq!(m.len(), 4);
        assert_eq!(m[2]["content"].as_array().unwrap().len(), 2);
        assert_eq!(m[2]["content"][1]["is_error"], true);
        assert_eq!(b.body["tools"][0]["input_schema"]["type"], "object");
        assert!(!b.body.to_string().contains("sk-ant"));
    }
}
