//! Explicitly configured HTTP model adapters. No redirects, hidden retries, tools or background work.

use crate::provider::Locality;
use crate::secrets::{SecretKey, SecretStore};
use crate::{
    LlmContent, LlmError, LlmProvider, LlmRequest, LlmResponse, ProviderKind, Role, Usage,
    redact_text,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use url::Url;

pub use crate::provider::{HttpDialect, HttpProviderConfig, Pricing};

impl Pricing {
    /// Ceiling-rounded token cost without floating point.
    pub fn cost(self, input: u64, output: u64) -> Result<u64, LlmError> {
        let n = (u128::from(input) * u128::from(self.input_micro_usd_per_million))
            .checked_add(u128::from(output) * u128::from(self.output_micro_usd_per_million))
            .ok_or(LlmError::SchemaViolation)?;
        u64::try_from(n.div_ceil(1_000_000)).map_err(|_| LlmError::SchemaViolation)
    }
}

/// Concrete provider. Credentials are loaded for each call so revocation takes effect immediately.
pub struct HttpProvider {
    config: HttpProviderConfig,
    secrets: Arc<dyn SecretStore>,
    client: reqwest::Client,
}
impl std::fmt::Debug for HttpProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpProvider")
            .field("kind", &self.config.kind)
            .field("model", &self.config.model)
            .finish_non_exhaustive()
    }
}
impl HttpProvider {
    /// Validate the endpoint and construct a client. This performs no network request.
    pub fn new(
        config: HttpProviderConfig,
        secrets: Arc<dyn SecretStore>,
    ) -> Result<Self, LlmError> {
        validate_endpoint(&config.endpoint)?;
        let local = config.kind.locality() == Locality::Local;
        let compatible = matches!(
            (config.kind, config.dialect),
            (
                ProviderKind::ApiKeyOpenaiCompatible,
                HttpDialect::OpenAiResponses | HttpDialect::ChatCompletions
            ) | (ProviderKind::OpenrouterOauth, HttpDialect::ChatCompletions)
                | (
                    ProviderKind::ApiKeyAnthropic,
                    HttpDialect::AnthropicMessages
                )
                | (ProviderKind::LocalOllama, HttpDialect::Ollama)
                | (
                    ProviderKind::LocalLlamaCpp | ProviderKind::LocalLmStudio,
                    HttpDialect::ChatCompletions
                )
        );
        if !compatible
            || config.model.is_empty()
            || config.model.len() > 200
            || config.model.chars().any(char::is_control)
            || !(1..=300).contains(&config.timeout_seconds)
            || (local
                && (!is_loopback(&config.endpoint)
                    || config.model.ends_with(":cloud")
                    || config.model.ends_with("-cloud")))
            || (!local && config.pricing.is_none())
        {
            return Err(LlmError::NotConfigured);
        }
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(config.timeout_seconds))
            .connect_timeout(Duration::from_secs(10));
        if is_loopback(&config.endpoint) {
            builder = builder.no_proxy();
        }
        let client = builder
            .build()
            .map_err(|_| unavailable("cannot configure HTTP client"))?;
        Ok(Self {
            config,
            secrets,
            client,
        })
    }
    fn key(&self) -> SecretKey {
        SecretKey {
            provider: self.config.kind,
            purpose: "api_key".into(),
        }
    }
    /// Conservative quote used by the runtime before any network request.
    pub fn quote(&self, request: &LlmRequest) -> Result<u64, LlmError> {
        validate_request(request)?;
        if self.config.kind.locality() == Locality::Local {
            return Ok(0);
        }
        let pricing = self.config.pricing.ok_or(LlmError::NotConfigured)?;
        let body = self.body(request)?;
        // A byte-based bound plus message overhead rejects oversized input. The reservation
        // covers the full configured context, not an optimistic tokenizer estimate.
        let bytes = serde_json::to_vec(&body)
            .map_err(|_| LlmError::SchemaViolation)?
            .len() as u64;
        if bytes.saturating_add(1024) > pricing.max_input_tokens {
            return Err(LlmError::SchemaViolation);
        }
        pricing.cost(
            pricing.max_input_tokens,
            u64::from(request.max_output_tokens),
        )
    }
    fn body(&self, request: &LlmRequest) -> Result<Value, LlmError> {
        let messages: Vec<_> = request
            .messages
            .iter()
            .map(|m| json!({"role":m.role,"content":redact_text(&m.content)}))
            .collect();
        let mut body = match self.config.dialect {
            HttpDialect::OpenAiResponses => {
                json!({"model":self.config.model,"input":messages,"max_output_tokens":request.max_output_tokens,"store":false,"stream":false})
            }
            HttpDialect::ChatCompletions => {
                json!({"model":self.config.model,"messages":messages,"max_tokens":request.max_output_tokens,"stream":false})
            }
            HttpDialect::Ollama => {
                json!({"model":self.config.model,"messages":messages,"stream":false,"options":{"num_predict":request.max_output_tokens}})
            }
            HttpDialect::AnthropicMessages => {
                let system = request
                    .messages
                    .iter()
                    .filter(|m| m.role == Role::System)
                    .map(|m| redact_text(&m.content))
                    .collect::<Vec<_>>()
                    .join("\n");
                let non_system: Vec<_> = messages
                    .into_iter()
                    .filter(|m| m["role"] != "system")
                    .collect();
                let mut body = json!({"model":self.config.model,"messages":non_system,"max_tokens":request.max_output_tokens,"stream":false});
                if !system.is_empty() {
                    body["system"] = json!(system)
                }
                body
            }
        };
        if let Some(schema) = &request.response_schema {
            crate::runtime::validate_schema(schema)?;
            match self.config.dialect {
                HttpDialect::OpenAiResponses => {
                    body["text"] = json!({"format":{"type":"json_schema","name":"zeus_result","strict":true,"schema":schema}})
                }
                HttpDialect::ChatCompletions => {
                    body["response_format"] = json!({"type":"json_schema","json_schema":{"name":"zeus_result","strict":true,"schema":schema}})
                }
                HttpDialect::Ollama => body["format"] = schema.clone(),
                // The schema is a Zeus-authored instruction and is always validated locally.
                HttpDialect::AnthropicMessages => {
                    let prior = body["system"].as_str().unwrap_or("");
                    body["system"] = json!(format!(
                        "{prior}\nReturn only JSON satisfying this schema: {schema}"
                    ));
                }
            }
        }
        Ok(body)
    }
}
#[async_trait]
impl LlmProvider for HttpProvider {
    fn kind(&self) -> ProviderKind {
        self.config.kind
    }
    fn is_configured(&self) -> bool {
        self.config.kind.locality() == Locality::Local
            || self
                .secrets
                .get(&self.key())
                .is_ok_and(|k| k.is_some_and(|k| !k.is_empty()))
    }
    fn cost_ceiling(&self, request: &LlmRequest) -> Result<u64, LlmError> {
        self.quote(request)
    }
    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, LlmError> {
        let key = self.secrets.get(&self.key())?;
        if self.config.kind.locality() != Locality::Local
            && key.as_ref().is_none_or(|k| k.is_empty())
        {
            return Err(LlmError::NotConfigured);
        }
        self.complete_authorized(request, key, false).await
    }
}
impl HttpProvider {
    pub(crate) async fn complete_authorized(
        &self,
        request: LlmRequest,
        key: Option<zeroize::Zeroizing<String>>,
        stream: bool,
    ) -> Result<LlmResponse, LlmError> {
        request.privacy.check_provider(self.config.kind)?;
        self.quote(&request)?;
        let mut body = self.body(&request)?;
        if stream {
            body["stream"] = json!(true);
        }
        let mut call = self.client.post(self.config.endpoint.clone()).json(&body);
        if let Some(key) = key {
            let mut header =
                reqwest::header::HeaderValue::from_str(&key).map_err(|_| LlmError::AuthFailed)?;
            header.set_sensitive(true);
            call = match self.config.dialect {
                HttpDialect::AnthropicMessages => call.header("x-api-key", header),
                _ => call.bearer_auth(key.as_str()),
            };
        }
        if matches!(self.config.dialect, HttpDialect::AnthropicMessages) {
            call = call.header("anthropic-version", "2023-06-01")
        }
        let response = call
            .send()
            .await
            .map_err(|_| unavailable("request failed or timed out"))?;
        let value = if stream {
            read_response_stream(response).await?
        } else {
            read_json(response, 2 * 1024 * 1024).await?
        };
        let (text, input, output) = match self.config.dialect {
            HttpDialect::OpenAiResponses => {
                if value["status"] != "completed" {
                    return Err(LlmError::SchemaViolation);
                }
                let mut text = String::new();
                for item in value["output"]
                    .as_array()
                    .ok_or(LlmError::SchemaViolation)?
                {
                    if item["type"] == "message" {
                        for part in item["content"]
                            .as_array()
                            .ok_or(LlmError::SchemaViolation)?
                        {
                            if part["type"] == "output_text" {
                                text.push_str(
                                    part["text"].as_str().ok_or(LlmError::SchemaViolation)?,
                                )
                            } else if part["type"] == "refusal" {
                                return Err(LlmError::SchemaViolation);
                            }
                        }
                    }
                }
                (
                    text,
                    number(&value["usage"]["input_tokens"])?,
                    number(&value["usage"]["output_tokens"])?,
                )
            }
            HttpDialect::ChatCompletions => {
                if value["choices"][0]["finish_reason"] != "stop" {
                    return Err(LlmError::SchemaViolation);
                }
                (
                    value["choices"][0]["message"]["content"]
                        .as_str()
                        .ok_or(LlmError::SchemaViolation)?
                        .into(),
                    number(&value["usage"]["prompt_tokens"])?,
                    number(&value["usage"]["completion_tokens"])?,
                )
            }
            HttpDialect::AnthropicMessages => {
                if value["stop_reason"] != "end_turn" {
                    return Err(LlmError::SchemaViolation);
                }
                let mut text = String::new();
                for part in value["content"]
                    .as_array()
                    .ok_or(LlmError::SchemaViolation)?
                {
                    if part["type"] == "text" {
                        text.push_str(part["text"].as_str().ok_or(LlmError::SchemaViolation)?)
                    }
                }
                let usage = &value["usage"];
                let input = number(&usage["input_tokens"])?
                    .saturating_add(usage["cache_creation_input_tokens"].as_u64().unwrap_or(0))
                    .saturating_add(usage["cache_read_input_tokens"].as_u64().unwrap_or(0));
                (text, input, number(&usage["output_tokens"])?)
            }
            HttpDialect::Ollama => {
                if value["done"] != true || value["done_reason"] == "length" {
                    return Err(LlmError::SchemaViolation);
                }
                (
                    value["message"]["content"]
                        .as_str()
                        .ok_or(LlmError::SchemaViolation)?
                        .into(),
                    number(&value["prompt_eval_count"])?,
                    number(&value["eval_count"])?,
                )
            }
        };
        if text.is_empty() || output > u64::from(request.max_output_tokens) {
            return Err(LlmError::SchemaViolation);
        }
        let content = if let Some(schema) = &request.response_schema {
            let parsed: Value =
                serde_json::from_str(&text).map_err(|_| LlmError::SchemaViolation)?;
            crate::runtime::check_output(schema, &parsed)?;
            LlmContent::Json(parsed)
        } else {
            LlmContent::Text(text)
        };
        let cost = match self.config.pricing {
            Some(p) => p.cost(input, output)?,
            None => 0,
        };
        Ok(LlmResponse {
            content,
            usage: Usage {
                input_tokens: input,
                output_tokens: output,
                cost_micro_usd: Some(cost),
            },
            provider: self.config.kind,
            model: self.config.model.clone(),
        })
    }
}
fn number(value: &Value) -> Result<u64, LlmError> {
    value.as_u64().ok_or(LlmError::SchemaViolation)
}
pub(crate) fn unavailable(message: &str) -> LlmError {
    LlmError::ProviderUnavailable(message.into())
}
pub(crate) fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    }
}
pub(crate) fn validate_endpoint(endpoint: &Url) -> Result<(), LlmError> {
    if !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || !(endpoint.scheme() == "https" || endpoint.scheme() == "http" && is_loopback(endpoint))
    {
        return Err(LlmError::NotConfigured);
    }
    Ok(())
}
pub(crate) fn validate_request(request: &LlmRequest) -> Result<(), LlmError> {
    if !(1..=65536).contains(&request.max_output_tokens)
        || request.messages.is_empty()
        || request.messages.len() > 128
        || request
            .messages
            .iter()
            .map(|m| m.content.len())
            .sum::<usize>()
            > 256 * 1024
    {
        return Err(LlmError::SchemaViolation);
    }
    Ok(())
}
pub(crate) async fn read_json(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Value, LlmError> {
    if response.status() == reqwest::StatusCode::UNAUTHORIZED
        || response.status() == reqwest::StatusCode::FORBIDDEN
    {
        return Err(LlmError::AuthFailed);
    }
    if !response.status().is_success() {
        return Err(unavailable("provider returned an unsuccessful HTTP status"));
    }
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(LlmError::SchemaViolation);
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| unavailable("response read failed"))?
    {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(LlmError::SchemaViolation);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| LlmError::SchemaViolation)
}

/// Read the final Responses event, rejecting interrupted or failed streams without partial output.
pub(crate) async fn read_response_stream(
    mut response: reqwest::Response,
) -> Result<Value, LlmError> {
    if response.status() == reqwest::StatusCode::UNAUTHORIZED
        || response.status() == reqwest::StatusCode::FORBIDDEN
    {
        return Err(LlmError::AuthFailed);
    }
    if !response.status().is_success() {
        return Err(unavailable("stream request failed"));
    }
    let mut pending = Vec::new();
    let mut total = 0usize;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| unavailable("inference stream interrupted"))?
    {
        total = total.saturating_add(chunk.len());
        if total > 16 * 1024 * 1024 || pending.len().saturating_add(chunk.len()) > 4 * 1024 * 1024 {
            return Err(LlmError::SchemaViolation);
        }
        pending.extend_from_slice(&chunk);
        loop {
            let lf = pending
                .windows(2)
                .position(|p| p == b"\n\n")
                .map(|p| (p, 2));
            let crlf = pending
                .windows(4)
                .position(|p| p == b"\r\n\r\n")
                .map(|p| (p, 4));
            let boundary = match (lf, crlf) {
                (Some(a), Some(b)) => Some(if a.0 < b.0 { a } else { b }),
                (a, b) => a.or(b),
            };
            let Some((end, width)) = boundary else { break };
            let event =
                std::str::from_utf8(&pending[..end]).map_err(|_| LlmError::SchemaViolation)?;
            let data = event
                .lines()
                .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
                .collect::<Vec<_>>()
                .join("\n");
            if !data.is_empty() && data != "[DONE]" {
                let mut value: Value =
                    serde_json::from_str(&data).map_err(|_| LlmError::SchemaViolation)?;
                match value["type"].as_str() {
                    Some("response.completed") => return Ok(value["response"].take()),
                    Some("response.failed" | "response.incomplete" | "error") => {
                        return Err(unavailable("inference stream did not complete"));
                    }
                    _ => {}
                }
            }
            pending.drain(..end + width);
        }
    }
    Err(unavailable("inference stream ended before completion"))
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn stream(body: &str) -> Result<Value, LlmError> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let body = body.to_owned();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut header = [0; 4096];
            let _ = socket.read(&mut header).await.unwrap();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
            for chunk in body.as_bytes().chunks(13) {
                socket.write_all(chunk).await.unwrap();
            }
        });
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(url)
            .send()
            .await
            .unwrap();
        let result = read_response_stream(response).await;
        server.await.unwrap();
        result
    }
    #[tokio::test]
    async fn stream_requires_terminal_success_and_handles_fragmented_mixed_line_endings() {
        assert!(
            stream("data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n")
                .await
                .is_err()
        );
        assert!(stream("data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":\"ZEUS-CANARY\"}}\n\n").await.unwrap_err().to_string().find("ZEUS-CANARY").is_none());
        assert_eq!(stream("data: {\"type\":\"response.created\"}\r\n\r\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n").await.unwrap(),json!({"status":"completed"}));
    }
}

#[cfg(test)]
mod pricing_tests {
    use super::*;
    #[test]
    fn untrusted_usage_and_prices_cannot_overflow_cost_arithmetic() {
        let pricing = Pricing {
            input_micro_usd_per_million: u64::MAX,
            output_micro_usd_per_million: u64::MAX,
            max_input_tokens: u64::MAX,
        };
        assert!(pricing.cost(u64::MAX, u64::MAX).is_err());
        assert!(pricing.cost(u64::MAX, 0).is_err());
        let bounded = Pricing {
            input_micro_usd_per_million: 1,
            output_micro_usd_per_million: 1,
            max_input_tokens: 100,
        };
        assert_eq!(bounded.cost(1, 1).unwrap(), 1);
    }
}
