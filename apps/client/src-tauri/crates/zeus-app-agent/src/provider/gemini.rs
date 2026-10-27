//! Google Gemini `generateContent` with function calling.
//!
//! Request: `POST {base}/models/{model}:streamGenerateContent?alt=sse`, key in the `x-goog-api-key` header (never in the
//! URL). `systemInstruction` carries the system prompt, `contents` alternate `user` and `model` roles with `parts`
//! (`text`, `functionCall {name, args}`, `functionResponse {name, response}`), tools are
//! `[{functionDeclarations: [{name, description, parameters}]}]`. Gemini's `parameters` is a subset of OpenAPI, so the
//! catalogue's JSON Schema is reduced to it ([`sanitize_schema`]); a free-form object becomes a string holding JSON,
//! which the tool layer parses back. A model turn is replayed with the exact parts it came with, because Gemini 3
//! thinking models attach a `thoughtSignature` to function-call parts that must come back unchanged.
//! Stream: SSE `data:` chunks of `{candidates: [{content: {parts}, finishReason}], usageMetadata}`.

use serde_json::{Map, Value, json};

use super::http::{Built, Parser};
use super::{
    ApiKey, ChatRequest, ErrorKind, Kind, Msg, Outcome, ProviderError, RawTurn, Stop, Target,
    ToolCall, Usage, estimate_tokens,
};

/// Reduce a JSON Schema to the OpenAPI subset Gemini accepts.
pub fn sanitize_schema(schema: &Value) -> Value {
    match schema {
        Value::Object(o) => {
            let mut out = Map::new();
            if let Some(c) = o.get("const") {
                out.insert("enum".into(), json!([c]));
                out.insert(
                    "type".into(),
                    json!(match c {
                        Value::Number(_) => "number",
                        Value::Bool(_) => "boolean",
                        _ => "string",
                    }),
                );
            }
            for (k, v) in o {
                match k.as_str() {
                    "type" => {
                        // `type` may be a list such as ["string", "null"].
                        if let Value::Array(a) = v {
                            let non_null: Vec<&Value> = a.iter().filter(|x| *x != "null").collect();
                            if let Some(t) = non_null.first() {
                                out.insert("type".into(), (*t).clone());
                            }
                            if a.iter().any(|x| x == "null") {
                                out.insert("nullable".into(), json!(true));
                            }
                        } else {
                            out.insert("type".into(), v.clone());
                        }
                    }
                    "description" | "enum" | "format" | "required" | "minItems" | "maxItems"
                    | "minimum" | "maximum" | "nullable" => {
                        out.insert(k.clone(), v.clone());
                    }
                    "items" => {
                        out.insert("items".into(), sanitize_schema(v));
                    }
                    "properties" => {
                        if let Value::Object(props) = v {
                            let cleaned: Map<String, Value> = props
                                .iter()
                                .map(|(n, s)| (n.clone(), sanitize_schema(s)))
                                .collect();
                            out.insert("properties".into(), Value::Object(cleaned));
                        }
                    }
                    "anyOf" | "oneOf" => {
                        if let Value::Array(branches) = v {
                            let live: Vec<&Value> = branches
                                .iter()
                                .filter(|b| b.get("type") != Some(&json!("null")))
                                .collect();
                            let has_null = live.len() != branches.len();
                            if live.len() == 1 {
                                if let Value::Object(only) = sanitize_schema(live[0]) {
                                    for (ik, iv) in only {
                                        out.entry(ik).or_insert(iv);
                                    }
                                }
                            } else if !live.is_empty() {
                                out.insert(
                                    "anyOf".into(),
                                    Value::Array(live.iter().map(|b| sanitize_schema(b)).collect()),
                                );
                            }
                            if has_null {
                                out.insert("nullable".into(), json!(true));
                            }
                        }
                    }
                    _ => {}
                }
            }
            // An object with no declared properties cannot be described to Gemini: pass it as a JSON string.
            if out.get("type") == Some(&json!("object"))
                && out
                    .get("properties")
                    .is_none_or(|p| p.as_object().is_none_or(Map::is_empty))
            {
                let desc = out
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                return json!({"type": "string", "description": format!("{desc} Pass a JSON object encoded as a string.").trim().to_owned()});
            }
            if out.get("type").is_none() && out.get("anyOf").is_none() && out.get("enum").is_none()
            {
                out.insert("type".into(), json!("string"));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// The `functionResponse.response` object for a tool result.
fn response_object(content: &str, is_error: bool) -> Value {
    if is_error {
        return json!({"error": content});
    }
    match serde_json::from_str::<Value>(content) {
        Ok(v) => json!({"result": v}),
        Err(_) => json!({"result": content}),
    }
}

fn contents(req: &ChatRequest) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut pending: Vec<Value> = Vec::new();
    let flush = |out: &mut Vec<Value>, pending: &mut Vec<Value>| {
        if !pending.is_empty() {
            out.push(json!({"role": "user", "parts": std::mem::take(pending)}));
        }
    };
    for m in &req.messages {
        match m {
            Msg::ToolResult {
                name,
                content,
                is_error,
                ..
            } => {
                pending.push(json!({"functionResponse": {"name": name, "response": response_object(content, *is_error)}}));
            }
            Msg::User(t) => {
                flush(&mut out, &mut pending);
                out.push(json!({"role": "user", "parts": [{"text": t}]}));
            }
            Msg::Assistant { text, calls, raw } => {
                flush(&mut out, &mut pending);
                let parts = match raw {
                    Some(RawTurn {
                        kind: Kind::Google,
                        value,
                    }) if value.as_array().is_some_and(|a| !a.is_empty()) => value.clone(),
                    _ => {
                        let mut parts: Vec<Value> = Vec::new();
                        if !text.is_empty() {
                            parts.push(json!({"text": text}));
                        }
                        for c in calls {
                            parts.push(json!({"functionCall": {"name": c.name, "args": c.args}}));
                        }
                        if parts.is_empty() {
                            parts.push(json!({"text": "(no answer)"}));
                        }
                        Value::Array(parts)
                    }
                };
                out.push(json!({"role": "model", "parts": parts}));
            }
        }
    }
    flush(&mut out, &mut pending);
    out
}

/// Build the request.
pub fn build(target: &Target, key: Option<&ApiKey>, req: &ChatRequest) -> Built {
    let model = req.model.trim().trim_start_matches("models/");
    let mut body = json!({
        "systemInstruction": {"parts": [{"text": req.system}]},
        "contents": contents(req),
        "generationConfig": {"maxOutputTokens": req.max_tokens},
    });
    if !req.tools.is_empty() {
        body["tools"] = json!([{"functionDeclarations": req.tools.iter().map(|t| json!({"name": t.name, "description": t.description, "parameters": sanitize_schema(&t.schema)})).collect::<Vec<_>>()}]);
    }
    let mut headers = vec![
        ("content-type".to_owned(), "application/json".to_owned()),
        ("accept".to_owned(), "text/event-stream".to_owned()),
    ];
    if let Some(k) = key {
        headers.push(("x-goog-api-key".to_owned(), k.expose().to_owned()));
    }
    Built {
        url: format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            target.base_url.trim().trim_end_matches('/'),
            model
        ),
        headers,
        body,
    }
}

/// The request for the models list.
pub fn models_request(target: &Target, key: Option<&ApiKey>) -> Built {
    let mut headers = vec![("accept".to_owned(), "application/json".to_owned())];
    if let Some(k) = key {
        headers.push(("x-goog-api-key".to_owned(), k.expose().to_owned()));
    }
    Built {
        url: format!(
            "{}/models?pageSize=200",
            target.base_url.trim().trim_end_matches('/')
        ),
        headers,
        body: Value::Null,
    }
}

/// Read ids from a models answer (`models[].name` is `models/<id>`; only models that can `generateContent`).
pub fn model_ids(v: &Value) -> Vec<String> {
    let mut ids: Vec<String> = v
        .get("models")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|m| {
                    m.get("supportedGenerationMethods")
                        .and_then(Value::as_array)
                        .is_none_or(|s| s.iter().any(|x| x == "generateContent"))
                })
                .filter_map(|m| {
                    m.get("name")
                        .and_then(Value::as_str)
                        .map(|n| n.trim_start_matches("models/").to_owned())
                })
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids.dedup();
    ids
}

/// The stream parser.
#[derive(Default)]
pub struct GeminiParser {
    text: String,
    parts: Vec<Value>,
    calls: Vec<ToolCall>,
    input: u64,
    output: u64,
    got_usage: bool,
    finish: Option<String>,
}

impl GeminiParser {
    /// A new parser.
    pub fn new() -> Self {
        Self::default()
    }

    fn apply(
        &mut self,
        v: &Value,
        sink: &mut (dyn FnMut(&str) + Send),
    ) -> Result<(), ProviderError> {
        if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
            let m = e
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("The provider reported an error.");
            let status = e.get("status").and_then(Value::as_str).unwrap_or_default();
            let kind = match status {
                "UNAUTHENTICATED" | "PERMISSION_DENIED" => ErrorKind::Auth,
                "RESOURCE_EXHAUSTED" => ErrorKind::RateLimited,
                "UNAVAILABLE" | "DEADLINE_EXCEEDED" => ErrorKind::Overloaded,
                "INVALID_ARGUMENT" | "NOT_FOUND" | "FAILED_PRECONDITION" => ErrorKind::BadRequest,
                _ => ErrorKind::Server,
            };
            return Err(ProviderError::new(
                kind,
                format!(
                    "The provider reported an error: {}",
                    m.chars().take(300).collect::<String>()
                ),
            ));
        }
        if let Some(b) = v
            .pointer("/promptFeedback/blockReason")
            .and_then(Value::as_str)
        {
            return Err(ProviderError::new(
                ErrorKind::BadRequest,
                format!("The provider blocked the request ({b})."),
            ));
        }
        if let Some(u) = v.get("usageMetadata") {
            let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
            self.input = n("promptTokenCount");
            self.output = n("candidatesTokenCount") + n("thoughtsTokenCount");
            self.got_usage = true;
        }
        let Some(cand) = v.pointer("/candidates/0") else {
            return Ok(());
        };
        if let Some(parts) = cand.pointer("/content/parts").and_then(Value::as_array) {
            for p in parts {
                self.parts.push(p.clone());
                let thought = p.get("thought").and_then(Value::as_bool).unwrap_or(false);
                if let Some(t) = p.get("text").and_then(Value::as_str)
                    && !thought
                    && !t.is_empty()
                {
                    self.text.push_str(t);
                    sink(t);
                }
                if let Some(fc) = p.get("functionCall") {
                    let name = fc
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    if name.is_empty() {
                        continue;
                    }
                    let id = fc
                        .get("id")
                        .and_then(Value::as_str)
                        .map_or_else(|| format!("call_{}", self.calls.len() + 1), str::to_owned);
                    let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                    let (args, args_error) = if args.is_object() {
                        (args, None)
                    } else {
                        (
                            json!({}),
                            Some("The arguments were not a JSON object.".to_owned()),
                        )
                    };
                    self.calls.push(ToolCall {
                        id,
                        name,
                        args,
                        args_error,
                    });
                }
            }
        }
        if let Some(f) = cand.get("finishReason").and_then(Value::as_str) {
            self.finish = Some(f.to_owned());
        }
        Ok(())
    }
}

impl Parser for GeminiParser {
    fn event(
        &mut self,
        _name: Option<&str>,
        data: &str,
        sink: &mut (dyn FnMut(&str) + Send),
    ) -> Result<(), ProviderError> {
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
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
        if let Some(list) = body.as_array() {
            for v in list {
                self.apply(v, sink)?;
            }
            Ok(())
        } else {
            self.apply(body, sink)
        }
    }

    fn finish(self: Box<Self>, estimated_input: u64) -> Result<Outcome, ProviderError> {
        let stop = match self.finish.as_deref() {
            Some("MAX_TOKENS") => Stop::Length,
            Some(
                "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII"
                | "IMAGE_SAFETY",
            ) => Stop::Filtered,
            Some("STOP") | None => {
                if self.calls.is_empty() {
                    Stop::End
                } else {
                    Stop::ToolUse
                }
            }
            Some(_) => Stop::Other,
        };
        let stop = if !self.calls.is_empty() && stop == Stop::End {
            Stop::ToolUse
        } else {
            stop
        };
        let usage = if self.got_usage {
            Usage {
                input: self.input,
                output: self.output,
                estimated: false,
            }
        } else {
            Usage {
                input: estimated_input,
                output: estimate_tokens(&self.text),
                estimated: true,
            }
        };
        let Self {
            text, parts, calls, ..
        } = *self;
        let raw = (!parts.is_empty()).then_some(RawTurn {
            kind: Kind::Google,
            value: Value::Array(parts),
        });
        Ok(Outcome {
            text,
            calls,
            raw,
            usage,
            stop,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ToolSpec;

    #[test]
    fn the_catalogue_schema_is_reduced_to_what_gemini_accepts() {
        let s = json!({
            "type": "object", "additionalProperties": false, "required": ["url"],
            "properties": {
                "url": {"type": "string", "pattern": "^https?://", "minLength": 8, "description": "A URL."},
                "mode": {"const": "organize"},
                "rules": {"type": "object", "description": "Free-form rules."},
                "since": {"anyOf": [{"type": "string"}, {"type": "null"}]},
                "pick": {"oneOf": [{"type": "string"}, {"type": "integer"}]},
                "list": {"type": "array", "uniqueItems": true, "items": {"type": "string", "maxLength": 9}, "maxItems": 5}
            }
        });
        let out = sanitize_schema(&s);
        let text = out.to_string();
        for banned in [
            "additionalProperties",
            "pattern",
            "minLength",
            "maxLength",
            "uniqueItems",
            "oneOf",
            "const",
        ] {
            assert!(!text.contains(banned), "{banned} in {text}");
        }
        assert_eq!(out["properties"]["mode"]["enum"][0], "organize");
        assert_eq!(out["properties"]["rules"]["type"], "string");
        assert_eq!(out["properties"]["since"]["type"], "string");
        assert_eq!(out["properties"]["since"]["nullable"], true);
        assert_eq!(
            out["properties"]["pick"]["anyOf"].as_array().unwrap().len(),
            2
        );
        assert_eq!(out["required"][0], "url");
    }

    #[test]
    fn function_calls_text_and_usage_stream_in() {
        let mut p = Box::new(GeminiParser::new());
        let mut streamed = String::new();
        p.event(
            None,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Checking"}]}}]}"#,
            &mut |t| streamed.push_str(t),
        )
        .unwrap();
        p.event(None, r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"zeus_probe","args":{"url":"https://e.org/a"}},"thoughtSignature":"SIG"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":40,"candidatesTokenCount":9,"thoughtsTokenCount":6}}"#, &mut |t| streamed.push_str(t)).unwrap();
        let o = p.finish(1).unwrap();
        assert_eq!(streamed, "Checking");
        assert_eq!(o.stop, Stop::ToolUse);
        assert_eq!(o.calls[0].args["url"], "https://e.org/a");
        assert_eq!(
            o.usage,
            Usage {
                input: 40,
                output: 15,
                estimated: false
            }
        );
        // The raw turn keeps the signature for replay.
        let raw = o.raw.unwrap();
        assert_eq!(raw.value[1]["thoughtSignature"], "SIG");
    }

    #[test]
    fn an_error_chunk_and_a_blocked_prompt_are_errors() {
        let mut p = GeminiParser::new();
        let e = p.event(None, r#"{"error":{"code":429,"message":"Quota exceeded","status":"RESOURCE_EXHAUSTED"}}"#, &mut |_| ()).unwrap_err();
        assert_eq!(e.kind, ErrorKind::RateLimited);
        let e = p
            .event(
                None,
                r#"{"promptFeedback":{"blockReason":"SAFETY"}}"#,
                &mut |_| (),
            )
            .unwrap_err();
        assert_eq!(e.kind, ErrorKind::BadRequest);
    }

    #[test]
    fn the_request_puts_the_key_in_a_header_and_replays_model_parts() {
        let req = ChatRequest {
            model: "models/gemini-x".into(),
            system: "sys".into(),
            messages: vec![
                Msg::User("hi".into()),
                Msg::Assistant {
                    text: String::new(),
                    calls: vec![ToolCall {
                        id: "call_1".into(),
                        name: "zeus_status".into(),
                        args: json!({}),
                        args_error: None,
                    }],
                    raw: Some(RawTurn {
                        kind: Kind::Google,
                        value: json!([{"functionCall": {"name": "zeus_status", "args": {}}, "thoughtSignature": "SIG"}]),
                    }),
                },
                Msg::ToolResult {
                    call_id: "call_1".into(),
                    name: "zeus_status".into(),
                    content: "{\"jobs\":[]}".into(),
                    is_error: false,
                },
            ],
            tools: vec![ToolSpec {
                name: "zeus_status".into(),
                description: "d".into(),
                schema: json!({"type": "object", "properties": {"limit": {"type": "integer"}}}),
            }],
            max_tokens: 256,
        };
        let key = ApiKey::new("AIzaTestKey000000000000".into());
        let b = build(
            &Target {
                kind: Kind::Google,
                base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
                json_tools: false,
            },
            Some(&key),
            &req,
        );
        assert_eq!(
            b.url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-x:streamGenerateContent?alt=sse"
        );
        assert!(!b.url.contains("AIza"));
        assert!(
            b.headers
                .iter()
                .any(|(k, v)| k == "x-goog-api-key" && v == "AIzaTestKey000000000000")
        );
        assert_eq!(b.body["contents"][1]["parts"][0]["thoughtSignature"], "SIG");
        assert_eq!(
            b.body["contents"][2]["parts"][0]["functionResponse"]["response"]["result"]["jobs"],
            json!([])
        );
        assert_eq!(b.body["generationConfig"]["maxOutputTokens"], 256);
        assert!(
            b.body["tools"][0]["functionDeclarations"][0]["parameters"]
                .get("additionalProperties")
                .is_none()
        );
    }
}
