//! Chat Completions: OpenAI and every compatible endpoint (Mistral, DeepSeek, Qwen, Kimi, GLM, Ollama, your own).
//!
//! Request: `messages` with `system`, `user`, `assistant` (with `tool_calls`) and `tool` (with `tool_call_id`) roles,
//! `tools` as `{type: "function", function: {name, description, parameters}}`, `stream: true`.
//! Stream: `data:` chunks of `{choices: [{delta: {content, tool_calls: [{index, id, function: {name, arguments}}]},
//! finish_reason}], usage}` ending with `data: [DONE]`. Tool-call arguments arrive as string fragments per `index` and
//! are joined; servers that send a whole call in one chunk work the same way. `reasoning_content` deltas (DeepSeek,
//! Kimi, GLM, Qwen thinking models) are not shown and not stored.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::http::{Built, Parser};
use super::{
    ApiKey, ChatRequest, ErrorKind, Kind, Msg, Outcome, ProviderError, Stop, Target, ToolCall,
    Usage, estimate_tokens,
};

/// The chat completions URL for a base URL.
pub fn chat_url(kind: Kind, base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if kind == Kind::Ollama
        && url::Url::parse(base).is_ok_and(|u| u.path() == "/" || u.path().is_empty())
    {
        return format!("{base}/v1/chat/completions");
    }
    format!("{base}/chat/completions")
}

/// The models URL for a base URL.
pub fn models_url(kind: Kind, base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if kind == Kind::Ollama
        && url::Url::parse(base).is_ok_and(|u| u.path() == "/" || u.path().is_empty())
    {
        return format!("{base}/v1/models");
    }
    format!("{base}/models")
}

fn messages(req: &ChatRequest) -> Vec<Value> {
    let mut out = vec![json!({"role": "system", "content": req.system})];
    for m in &req.messages {
        match m {
            Msg::User(t) => out.push(json!({"role": "user", "content": t})),
            Msg::Assistant { text, calls, .. } => {
                let mut v = json!({"role": "assistant", "content": if text.is_empty() && !calls.is_empty() { Value::Null } else { json!(text) }});
                if !calls.is_empty() {
                    v["tool_calls"] = Value::Array(
                        calls
                            .iter()
                            .map(|c| json!({"id": c.id, "type": "function", "function": {"name": c.name, "arguments": c.args.to_string()}}))
                            .collect(),
                    );
                }
                out.push(v);
            }
            Msg::ToolResult {
                call_id, content, ..
            } => out.push(json!({"role": "tool", "tool_call_id": call_id, "content": content})),
        }
    }
    out
}

/// Build the request.
pub fn build(target: &Target, key: Option<&ApiKey>, req: &ChatRequest) -> Built {
    let mut body = json!({
        "model": req.model,
        "messages": messages(req),
        "stream": true,
    });
    let limit = if target.kind.uses_completion_tokens() {
        "max_completion_tokens"
    } else {
        "max_tokens"
    };
    body[limit] = json!(req.max_tokens);
    if target.kind.asks_for_usage() {
        body["stream_options"] = json!({"include_usage": true});
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.schema}}))
                .collect(),
        );
    }
    let mut headers = vec![
        ("content-type".to_owned(), "application/json".to_owned()),
        ("accept".to_owned(), "text/event-stream".to_owned()),
    ];
    if let Some(k) = key {
        headers.push(("authorization".to_owned(), format!("Bearer {}", k.expose())));
    }
    Built {
        url: chat_url(target.kind, &target.base_url),
        headers,
        body,
    }
}

/// The request for the models list.
pub fn models_request(target: &Target, key: Option<&ApiKey>) -> Built {
    let mut headers = vec![("accept".to_owned(), "application/json".to_owned())];
    if let Some(k) = key {
        headers.push(("authorization".to_owned(), format!("Bearer {}", k.expose())));
    }
    Built {
        url: models_url(target.kind, &target.base_url),
        headers,
        body: Value::Null,
    }
}

/// Read ids from a models answer (`data[].id`, or Ollama's `models[].name`).
pub fn model_ids(v: &Value) -> Vec<String> {
    let mut ids: Vec<String> = v
        .get("data")
        .or_else(|| v.get("models"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    m.get("id")
                        .or_else(|| m.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids.dedup();
    ids
}

#[derive(Default)]
struct Partial {
    id: String,
    name: String,
    args: String,
}

/// The stream parser.
#[derive(Default)]
pub struct OpenaiParser {
    text: String,
    calls: BTreeMap<usize, Partial>,
    usage: Option<Usage>,
    finish: Option<String>,
    done: bool,
}

impl OpenaiParser {
    /// A new parser.
    pub fn new() -> Self {
        Self::default()
    }

    fn usage_of(v: &Value) -> Option<Usage> {
        let u = v
            .get("usage")
            .filter(|u| !u.is_null())
            .or_else(|| v.pointer("/choices/0/usage").filter(|u| !u.is_null()))?;
        Some(Usage {
            input: u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
            output: u
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            estimated: false,
        })
    }

    fn error_of(v: &Value) -> Option<ProviderError> {
        let e = v.get("error").filter(|e| !e.is_null())?;
        let message = e
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| e.as_str())
            .unwrap_or("The provider reported an error.");
        let kind = match e.get("type").and_then(Value::as_str).unwrap_or_default() {
            t if t.contains("auth") || t.contains("permission") => ErrorKind::Auth,
            t if t.contains("rate") || t.contains("quota") => ErrorKind::RateLimited,
            t if t.contains("overload") || t.contains("server") => ErrorKind::Overloaded,
            _ => ErrorKind::Server,
        };
        Some(ProviderError::new(
            kind,
            format!(
                "The provider reported an error mid-answer: {}",
                message.chars().take(300).collect::<String>()
            ),
        ))
    }

    fn apply(
        &mut self,
        v: &Value,
        sink: &mut (dyn FnMut(&str) + Send),
    ) -> Result<(), ProviderError> {
        if let Some(e) = Self::error_of(v) {
            return Err(e);
        }
        if let Some(u) = Self::usage_of(v) {
            self.usage = Some(u);
        }
        let Some(choice) = v.pointer("/choices/0") else {
            return Ok(());
        };
        let delta = choice.get("delta").or_else(|| choice.get("message"));
        if let Some(delta) = delta {
            if let Some(t) = delta.get("content").and_then(Value::as_str)
                && !t.is_empty()
            {
                self.text.push_str(t);
                sink(t);
            }
            if let Some(list) = delta.get("tool_calls").and_then(Value::as_array) {
                for tc in list {
                    let id = tc
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty());
                    let idx = match tc.get("index").and_then(Value::as_u64) {
                        Some(i) => i as usize,
                        None => match id.and_then(|id| {
                            self.calls.iter().find(|(_, p)| p.id == id).map(|(i, _)| *i)
                        }) {
                            Some(i) => i,
                            None if id.is_some() => self.calls.len(),
                            None => self.calls.keys().next_back().copied().unwrap_or(0),
                        },
                    };
                    let p = self.calls.entry(idx).or_default();
                    if let Some(id) = id {
                        p.id = id.to_owned();
                    }
                    if let Some(f) = tc.get("function") {
                        if let Some(n) = f.get("name").and_then(Value::as_str)
                            && !n.is_empty()
                        {
                            p.name = n.to_owned();
                        }
                        match f.get("arguments") {
                            Some(Value::String(a)) => p.args.push_str(a),
                            Some(obj @ Value::Object(_)) => p.args = obj.to_string(),
                            _ => {}
                        }
                    }
                }
            }
        }
        if let Some(f) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish = Some(f.to_owned());
        }
        Ok(())
    }
}

impl Parser for OpenaiParser {
    fn event(
        &mut self,
        _name: Option<&str>,
        data: &str,
        sink: &mut (dyn FnMut(&str) + Send),
    ) -> Result<(), ProviderError> {
        let data = data.trim();
        if data.is_empty() {
            return Ok(());
        }
        if data == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let v: Value = serde_json::from_str(data).map_err(|_| {
            ProviderError::new(
                ErrorKind::Protocol,
                "The provider sent a chunk Zeus could not read.",
            )
        })?;
        self.apply(&v, sink)
    }

    fn whole(
        &mut self,
        body: &Value,
        sink: &mut (dyn FnMut(&str) + Send),
    ) -> Result<(), ProviderError> {
        self.apply(body, sink)?;
        self.done = true;
        Ok(())
    }

    fn finish(self: Box<Self>, estimated_input: u64) -> Result<Outcome, ProviderError> {
        let mut calls = Vec::new();
        for (n, (_, p)) in self.calls.into_iter().enumerate() {
            if p.name.is_empty() {
                continue;
            }
            let id = if p.id.is_empty() {
                format!("call_{}", n + 1)
            } else {
                p.id
            };
            let (args, args_error) = if p.args.trim().is_empty() {
                (json!({}), None)
            } else {
                match serde_json::from_str::<Value>(&p.args) {
                    Ok(v) if v.is_object() => (v, None),
                    _ => (
                        json!({}),
                        Some("The arguments were not a JSON object.".to_owned()),
                    ),
                }
            };
            calls.push(ToolCall {
                id,
                name: p.name,
                args,
                args_error,
            });
        }
        let stop = match self.finish.as_deref() {
            Some("tool_calls" | "function_call") => Stop::ToolUse,
            Some("length") => Stop::Length,
            Some("content_filter") => Stop::Filtered,
            Some("stop") | None => {
                if calls.is_empty() {
                    Stop::End
                } else {
                    Stop::ToolUse
                }
            }
            Some(_) => Stop::Other,
        };
        let usage = self.usage.unwrap_or_else(|| Usage {
            input: estimated_input,
            output: estimate_tokens(&self.text)
                + calls
                    .iter()
                    .map(|c| estimate_tokens(&c.args.to_string()))
                    .sum::<u64>(),
            estimated: true,
        });
        Ok(Outcome {
            text: self.text,
            calls,
            raw: None,
            usage,
            stop,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ToolSpec;

    fn run(chunks: &[&str]) -> Outcome {
        let mut p = Box::new(OpenaiParser::new());
        let mut streamed = String::new();
        for c in chunks {
            p.event(None, c, &mut |t| streamed.push_str(t)).unwrap();
        }
        let o = p.finish(10).unwrap();
        assert_eq!(streamed, o.text);
        o
    }

    #[test]
    fn text_deltas_and_usage_are_joined() {
        let o = run(&[
            r#"{"choices":[{"delta":{"role":"assistant","content":"Hel"}}]}"#,
            r#"{"choices":[{"delta":{"content":"lo"},"finish_reason":"stop"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":3}}"#,
            "[DONE]",
        ]);
        assert_eq!(o.text, "Hello");
        assert_eq!(o.stop, Stop::End);
        assert_eq!(
            o.usage,
            Usage {
                input: 12,
                output: 3,
                estimated: false
            }
        );
    }

    #[test]
    fn tool_call_fragments_are_joined_by_index() {
        let o = run(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"zeus_probe","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"url\":\"https://"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"e.org/a\"}"}},{"index":1,"id":"call_b","function":{"name":"zeus_status","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
        ]);
        assert_eq!(o.stop, Stop::ToolUse);
        assert_eq!(o.calls.len(), 2);
        assert_eq!(o.calls[0].name, "zeus_probe");
        assert_eq!(o.calls[0].args["url"], "https://e.org/a");
        assert_eq!(o.calls[1].id, "call_b");
        assert!(o.usage.estimated);
    }

    #[test]
    fn a_whole_call_in_one_chunk_without_an_index_works() {
        let o = run(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"x1","function":{"name":"zeus_status","arguments":"{\"limit\":5}"}}]},"finish_reason":"tool_calls"}]}"#,
        ]);
        assert_eq!(o.calls[0].args["limit"], 5);
    }

    #[test]
    fn broken_arguments_become_a_refusal_not_a_crash() {
        let o = run(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"zeus_status","arguments":"{\"limit\":"}}]},"finish_reason":"tool_calls"}]}"#,
        ]);
        assert!(o.calls[0].args_error.is_some());
    }

    #[test]
    fn reasoning_deltas_are_not_shown() {
        let o = run(&[
            r#"{"choices":[{"delta":{"reasoning_content":"thinking...","content":null}}]}"#,
            r#"{"choices":[{"delta":{"content":"Done"},"finish_reason":"stop"}]}"#,
        ]);
        assert_eq!(o.text, "Done");
    }

    #[test]
    fn a_mid_stream_error_object_is_an_error() {
        let mut p = OpenaiParser::new();
        let e = p
            .event(
                None,
                r#"{"error":{"message":"The server is overloaded","type":"server_error"}}"#,
                &mut |_| (),
            )
            .unwrap_err();
        assert!(e.message.contains("overloaded"));
    }

    #[test]
    fn the_request_follows_the_kind() {
        let req = ChatRequest {
            model: "m".into(),
            system: "sys".into(),
            messages: vec![
                Msg::User("hi".into()),
                Msg::Assistant {
                    text: String::new(),
                    calls: vec![ToolCall {
                        id: "c1".into(),
                        name: "zeus_status".into(),
                        args: json!({"limit": 1}),
                        args_error: None,
                    }],
                    raw: None,
                },
                Msg::ToolResult {
                    call_id: "c1".into(),
                    name: "zeus_status".into(),
                    content: "{}".into(),
                    is_error: false,
                },
            ],
            tools: vec![ToolSpec {
                name: "zeus_status".into(),
                description: "d".into(),
                schema: json!({"type": "object"}),
            }],
            max_tokens: 512,
        };
        let key = ApiKey::new("sk-test-key-123456".into());
        let openai = build(
            &Target {
                kind: Kind::Openai,
                base_url: "https://api.openai.com/v1".into(),
                json_tools: false,
            },
            Some(&key),
            &req,
        );
        assert_eq!(openai.url, "https://api.openai.com/v1/chat/completions");
        assert_eq!(openai.body["max_completion_tokens"], 512);
        assert!(openai.body.get("max_tokens").is_none());
        assert_eq!(openai.body["stream_options"]["include_usage"], true);
        assert_eq!(
            openai.body["messages"][2]["tool_calls"][0]["function"]["arguments"],
            "{\"limit\":1}"
        );
        assert_eq!(openai.body["messages"][3]["role"], "tool");
        assert!(
            openai
                .headers
                .iter()
                .any(|(k, v)| k == "authorization" && v == "Bearer sk-test-key-123456")
        );
        assert!(!openai.body.to_string().contains("sk-test-key"));
        let mistral = build(
            &Target {
                kind: Kind::Mistral,
                base_url: "https://api.mistral.ai/v1".into(),
                json_tools: false,
            },
            Some(&key),
            &req,
        );
        assert_eq!(mistral.body["max_tokens"], 512);
        assert!(mistral.body.get("stream_options").is_none());
        let ollama = build(
            &Target {
                kind: Kind::Ollama,
                base_url: "http://127.0.0.1:11434".into(),
                json_tools: false,
            },
            None,
            &req,
        );
        assert_eq!(ollama.url, "http://127.0.0.1:11434/v1/chat/completions");
        assert!(!ollama.headers.iter().any(|(k, _)| k == "authorization"));
    }

    #[test]
    fn model_lists_read_ids_and_names() {
        assert_eq!(
            model_ids(&json!({"data": [{"id": "b"}, {"id": "a"}]})),
            ["a", "b"]
        );
        assert_eq!(
            model_ids(&json!({"models": [{"name": "llama3"}]})),
            ["llama3"]
        );
    }
}
