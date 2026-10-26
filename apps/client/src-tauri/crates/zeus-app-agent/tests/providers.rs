//! Every adapter against a local server that speaks the provider's real wire format: streamed text, streamed tool
//! calls, usage, error bodies, rate-limit headers, retries, cancellation mid-stream, idle timeouts and size limits.
//! Hermetic: loopback only, no key but a canary string.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use zeus_app_agent::provider::http::{Http, Limits};
use zeus_app_agent::provider::{self, ApiKey, ChatRequest, ErrorKind, Kind, Msg, Stop, Target, ToolSpec};

const CANARY: &str = "ZEUS-CANARY-key-0123456789";

#[derive(Debug, Clone)]
struct Req {
    path: String,
    headers: HashMap<String, String>,
    body: Value,
}

enum Reply {
    /// Status, headers, then chunks with a pause before each; `hang` keeps the connection open at the end.
    Stream { status: u16, headers: Vec<(&'static str, String)>, chunks: Vec<(u64, Vec<u8>)>, hang: bool },
    /// A plain body with a content length.
    Plain { status: u16, headers: Vec<(&'static str, String)>, body: String },
}

fn sse(events: &[(&str, &str)]) -> Vec<(u64, Vec<u8>)> {
    events.iter().map(|(n, d)| (0, if n.is_empty() { format!("data: {d}\n\n") } else { format!("event: {n}\ndata: {d}\n\n") }.into_bytes())).collect()
}

fn ok_stream(chunks: Vec<(u64, Vec<u8>)>) -> Reply {
    Reply::Stream { status: 200, headers: vec![("content-type", "text/event-stream".into())], chunks, hang: false }
}

async fn serve(handler: impl Fn(usize, &Req) -> Reply + Send + Sync + 'static) -> (SocketAddr, Arc<Mutex<Vec<Req>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Arc<Mutex<Vec<Req>>> = Arc::default();
    let handler = Arc::new(handler);
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let handler = handler.clone();
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let head_end = loop {
                    let n = sock.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break p + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let mut lines = head.lines();
                let path = lines.next().unwrap_or("").split(' ').nth(1).unwrap_or("").to_owned();
                let headers: HashMap<String, String> = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_owned())).collect();
                let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
                while buf.len() < head_end + len {
                    let n = sock.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let body: Value = serde_json::from_slice(&buf[head_end..(head_end + len).min(buf.len())]).unwrap_or(Value::Null);
                let req = Req { path, headers, body };
                let index = {
                    let mut l = log.lock().unwrap();
                    l.push(req.clone());
                    l.len() - 1
                };
                match handler(index, &req) {
                    Reply::Plain { status, headers, body } => {
                        let mut out = format!("HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n", body.len());
                        for (k, v) in headers {
                            out.push_str(&format!("{k}: {v}\r\n"));
                        }
                        out.push_str("\r\n");
                        out.push_str(&body);
                        let _ = sock.write_all(out.as_bytes()).await;
                    }
                    Reply::Stream { status, headers, chunks, hang } => {
                        let mut out = format!("HTTP/1.1 {status} X\r\ntransfer-encoding: chunked\r\nconnection: close\r\n");
                        for (k, v) in headers {
                            out.push_str(&format!("{k}: {v}\r\n"));
                        }
                        out.push_str("\r\n");
                        let _ = sock.write_all(out.as_bytes()).await;
                        for (pause, c) in chunks {
                            if pause > 0 {
                                tokio::time::sleep(Duration::from_millis(pause)).await;
                            }
                            let frame = [format!("{:x}\r\n", c.len()).into_bytes(), c, b"\r\n".to_vec()].concat();
                            if sock.write_all(&frame).await.is_err() {
                                return;
                            }
                        }
                        if hang {
                            tokio::time::sleep(Duration::from_secs(30)).await;
                        } else {
                            let _ = sock.write_all(b"0\r\n\r\n").await;
                        }
                    }
                }
            });
        }
    });
    (addr, seen)
}

fn fast() -> Http {
    Http::with_limits(Limits { attempts: 3, backoff: Duration::from_millis(10), max_wait: Duration::from_millis(80), idle: Duration::from_millis(400), connect: Duration::from_secs(2), max_stream_bytes: 4000, max_error_bytes: 2048 })
}

fn target(kind: Kind, addr: SocketAddr, path: &str) -> Target {
    Target { kind, base_url: format!("http://{addr}{path}"), json_tools: false }
}

fn tool() -> ToolSpec {
    ToolSpec { name: "zeus_probe".into(), description: "Probe a URL".into(), schema: json!({"type": "object", "properties": {"url": {"type": "string"}}, "required": ["url"]}) }
}

fn request(model: &str, with_tools: bool) -> ChatRequest {
    ChatRequest { model: model.into(), system: "system".into(), messages: vec![Msg::User("probe https://e.org/a".into())], tools: if with_tools { vec![tool()] } else { vec![] }, max_tokens: 256 }
}

async fn chat(http: &Http, t: &Target, key: Option<&ApiKey>, req: &ChatRequest, cancel: &CancellationToken) -> (Result<provider::Outcome, provider::ProviderError>, String) {
    let mut streamed = String::new();
    let r = provider::chat(http, t, key, req, &mut |s| streamed.push_str(s), cancel).await.map(|(o, _)| o);
    (r, streamed)
}

#[tokio::test]
async fn openai_compatible_streams_text_tool_calls_and_usage_with_the_key_in_a_header() {
    let (addr, seen) = serve(|_, _| {
        ok_stream(sse(&[
            ("", r#"{"choices":[{"delta":{"content":"Checking "}}]}"#),
            ("", r#"{"choices":[{"delta":{"content":"now."}}]}"#),
            ("", r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"zeus_probe","arguments":"{\"url\":\"https://e.org/a\"}"}}]},"finish_reason":"tool_calls"}]}"#),
            ("", r#"{"choices":[],"usage":{"prompt_tokens":50,"completion_tokens":11}}"#),
            ("", "[DONE]"),
        ]))
    })
    .await;
    let key = ApiKey::new(CANARY.into());
    let (r, streamed) = chat(&fast(), &target(Kind::Openai, addr, "/v1"), Some(&key), &request("m", true), &CancellationToken::new()).await;
    let o = r.unwrap();
    assert_eq!(streamed, "Checking now.");
    assert_eq!(o.stop, Stop::ToolUse);
    assert_eq!(o.calls[0].name, "zeus_probe");
    assert_eq!((o.usage.input, o.usage.output), (50, 11));
    let req = seen.lock().unwrap()[0].clone();
    assert_eq!(req.path, "/v1/chat/completions");
    assert_eq!(req.headers["authorization"], format!("Bearer {CANARY}"));
    assert!(!req.path.contains("CANARY"));
    assert_eq!(req.body["stream"], true);
    assert_eq!(req.body["tools"][0]["function"]["name"], "zeus_probe");
    assert_eq!(req.body["max_completion_tokens"], 256);
}

#[tokio::test]
async fn anthropic_streams_tool_use_and_sends_its_version_header() {
    let (addr, seen) = serve(|_, _| {
        ok_stream(sse(&[
            ("message_start", r#"{"type":"message_start","message":{"usage":{"input_tokens":30,"output_tokens":1}}}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"zeus_probe","input":{}}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"url\":\"https://e.org/a\"}"}}"#),
            ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":20}}"#),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ]))
    })
    .await;
    let key = ApiKey::new(CANARY.into());
    let (r, _) = chat(&fast(), &target(Kind::Anthropic, addr, ""), Some(&key), &request("claude-x", true), &CancellationToken::new()).await;
    let o = r.unwrap();
    assert_eq!(o.calls[0].id, "toolu_1");
    assert_eq!(o.stop, Stop::ToolUse);
    let req = seen.lock().unwrap()[0].clone();
    assert_eq!(req.path, "/v1/messages");
    assert_eq!(req.headers["x-api-key"], CANARY);
    assert_eq!(req.headers["anthropic-version"], "2023-06-01");
    assert_eq!(req.body["system"], "system");
    assert_eq!(req.body["tools"][0]["name"], "zeus_probe");
}

#[tokio::test]
async fn gemini_streams_function_calls_with_the_key_in_a_header_never_the_url() {
    let (addr, seen) = serve(|_, _| {
        ok_stream(sse(&[
            ("", r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"zeus_probe","args":{"url":"https://e.org/a"}}}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":22,"candidatesTokenCount":7}}"#),
        ]))
    })
    .await;
    let key = ApiKey::new(CANARY.into());
    let (r, _) = chat(&fast(), &target(Kind::Google, addr, "/v1beta"), Some(&key), &request("gemini-x", true), &CancellationToken::new()).await;
    let o = r.unwrap();
    assert_eq!(o.calls[0].args["url"], "https://e.org/a");
    assert_eq!(o.stop, Stop::ToolUse);
    let req = seen.lock().unwrap()[0].clone();
    assert_eq!(req.path, "/v1beta/models/gemini-x:streamGenerateContent?alt=sse");
    assert_eq!(req.headers["x-goog-api-key"], CANARY);
    assert!(!req.path.contains(CANARY));
}

#[tokio::test]
async fn a_429_with_retry_after_is_retried_and_then_succeeds() {
    let (addr, seen) = serve(|i, _| {
        if i == 0 {
            Reply::Plain { status: 429, headers: vec![("retry-after", "1".into()), ("x-ratelimit-remaining-requests", "0".into())], body: r#"{"error":{"message":"slow down"}}"#.into() }
        } else {
            ok_stream(sse(&[("", r#"{"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]}"#), ("", "[DONE]")]))
        }
    })
    .await;
    let t0 = Instant::now();
    let (r, streamed) = chat(&fast(), &target(Kind::Mistral, addr, "/v1"), Some(&ApiKey::new(CANARY.into())), &request("m", false), &CancellationToken::new()).await;
    assert_eq!(r.unwrap().text, "ok");
    assert_eq!(streamed, "ok");
    assert_eq!(seen.lock().unwrap().len(), 2);
    // Retry-After said one second; the product cap here is 80 ms.
    assert!(t0.elapsed() < Duration::from_millis(900));
}

#[tokio::test]
async fn a_refused_key_is_not_retried_and_never_echoed() {
    let (addr, seen) = serve(|_, req| {
        let echoed = req.headers.get("authorization").cloned().unwrap_or_default();
        Reply::Plain { status: 401, headers: vec![], body: json!({"error": {"message": format!("Incorrect API key provided: {}", echoed.trim_start_matches("Bearer "))}}).to_string() }
    })
    .await;
    let key = ApiKey::new(CANARY.into());
    let (r, _) = chat(&fast(), &target(Kind::Deepseek, addr, ""), Some(&key), &request("m", false), &CancellationToken::new()).await;
    let e = r.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Auth);
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(!e.message.contains("ZEUS-CANARY"), "{}", e.message);
    assert!(!format!("{e:?}").contains("ZEUS-CANARY"));
}

#[tokio::test]
async fn a_server_that_keeps_failing_gives_up_after_three_attempts() {
    let (addr, seen) = serve(|_, _| Reply::Plain { status: 503, headers: vec![], body: "{}".into() }).await;
    let (r, _) = chat(&fast(), &target(Kind::Qwen, addr, "/v1"), None, &request("m", false), &CancellationToken::new()).await;
    assert_eq!(r.unwrap_err().kind, ErrorKind::Overloaded);
    assert_eq!(seen.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn a_redirect_is_not_followed_so_the_key_goes_nowhere_else() {
    let (addr, seen) = serve(|_, _| Reply::Plain { status: 302, headers: vec![("location", "http://example.org/steal".into())], body: String::new() }).await;
    let (r, _) = chat(&fast(), &target(Kind::Kimi, addr, "/v1"), Some(&ApiKey::new(CANARY.into())), &request("m", false), &CancellationToken::new()).await;
    assert!(r.is_err());
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn stopping_mid_stream_ends_the_call_at_once() {
    let (addr, _) = serve(|_, _| Reply::Stream { status: 200, headers: vec![("content-type", "text/event-stream".into())], chunks: sse(&[("", r#"{"choices":[{"delta":{"content":"partial"}}]}"#)]), hang: true }).await;
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        c.cancel();
    });
    let http = Http::with_limits(Limits { idle: Duration::from_secs(10), ..fast().limits().clone() });
    let t0 = Instant::now();
    let (r, streamed) = chat(&http, &target(Kind::Glm, addr, "/v4"), None, &request("m", false), &cancel).await;
    assert_eq!(r.unwrap_err().kind, ErrorKind::Cancelled);
    assert_eq!(streamed, "partial");
    assert!(t0.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn a_stalled_stream_times_out() {
    let (addr, _) = serve(|_, _| Reply::Stream { status: 200, headers: vec![("content-type", "text/event-stream".into())], chunks: vec![], hang: true }).await;
    let (r, _) = chat(&fast(), &target(Kind::Ollama, addr, ""), None, &request("m", false), &CancellationToken::new()).await;
    assert_eq!(r.unwrap_err().kind, ErrorKind::Timeout);
}

#[tokio::test]
async fn an_answer_over_the_size_limit_is_cut_off_with_a_clear_error() {
    let big = format!("data: {}\n\n", json!({"choices": [{"delta": {"content": "x".repeat(6000)}}]}).to_string());
    let (addr, _) = serve(move |_, _| ok_stream(vec![(0, big.clone().into_bytes())])).await;
    let (r, _) = chat(&fast(), &target(Kind::OpenaiCompatible, addr, "/v1"), None, &request("m", false), &CancellationToken::new()).await;
    assert_eq!(r.unwrap_err().kind, ErrorKind::TooLarge);
}

#[tokio::test]
async fn a_server_that_ignores_stream_still_works() {
    let (addr, _) = serve(|_, _| Reply::Plain { status: 200, headers: vec![("content-type", "application/json".into())], body: json!({"choices": [{"message": {"role": "assistant", "content": "whole answer"}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 3, "completion_tokens": 2}}).to_string() }).await;
    let (r, streamed) = chat(&fast(), &target(Kind::Ollama, addr, ""), None, &request("m", false), &CancellationToken::new()).await;
    assert_eq!(r.unwrap().text, "whole answer");
    assert_eq!(streamed, "whole answer");
}

#[tokio::test]
async fn a_connection_that_cannot_open_says_so_without_the_key() {
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let t = Target { kind: Kind::Openai, base_url: format!("http://127.0.0.1:{closed}/v1"), json_tools: false };
    let (r, _) = chat(&fast(), &t, Some(&ApiKey::new(CANARY.into())), &request("m", false), &CancellationToken::new()).await;
    let e = r.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Network);
    assert!(!e.message.contains("ZEUS-CANARY"));
}

#[tokio::test]
async fn the_json_protocol_describes_tools_in_the_prompt_and_reads_a_json_reply() {
    let (addr, seen) = serve(|_, _| ok_stream(sse(&[("", r#"{"choices":[{"delta":{"content":"{\"tool\":\"zeus_probe\",\"arguments\":{\"url\":\"https://e.org/a\"}}"},"finish_reason":"stop"}]}"#), ("", "[DONE]")]))).await;
    let mut t = target(Kind::Ollama, addr, "");
    t.json_tools = true;
    let (r, _) = chat(&fast(), &t, None, &request("llama", true), &CancellationToken::new()).await;
    let o = r.unwrap();
    assert_eq!(o.calls[0].name, "zeus_probe");
    assert_eq!(o.stop, Stop::ToolUse);
    let req = seen.lock().unwrap()[0].clone();
    assert!(req.body.get("tools").is_none());
    assert!(req.body["messages"][0]["content"].as_str().unwrap().contains("zeus_probe"));
}

#[tokio::test]
async fn model_lists_come_back_for_each_wire_format() {
    let (addr, seen) = serve(|_, req| {
        let body = if req.path.starts_with("/v1/models") && req.headers.contains_key("anthropic-version") {
            json!({"data": [{"id": "claude-a"}, {"id": "claude-b"}]})
        } else if req.path.contains("/models?") {
            json!({"models": [{"name": "models/gemini-a", "supportedGenerationMethods": ["generateContent"]}, {"name": "models/embed", "supportedGenerationMethods": ["embedContent"]}]})
        } else {
            json!({"data": [{"id": "gpt-a"}]})
        };
        Reply::Plain { status: 200, headers: vec![("content-type", "application/json".into())], body: body.to_string() }
    })
    .await;
    let key = ApiKey::new(CANARY.into());
    let cancel = CancellationToken::new();
    let http = fast();
    assert_eq!(provider::list_models(&http, &target(Kind::Anthropic, addr, ""), Some(&key), &cancel).await.unwrap(), ["claude-a", "claude-b"]);
    assert_eq!(provider::list_models(&http, &target(Kind::Google, addr, "/v1beta"), Some(&key), &cancel).await.unwrap(), ["gemini-a"]);
    assert_eq!(provider::list_models(&http, &target(Kind::Openai, addr, "/v1"), Some(&key), &cancel).await.unwrap(), ["gpt-a"]);
    assert!(seen.lock().unwrap().iter().all(|r| !r.path.contains("CANARY")));
}
