//! The HTTP layer shared by every provider: send, retry, stream, cancel, limit.
//!
//! * One `reqwest` client with rustls, no redirects (an API key must never follow a redirect), a connect timeout and an
//!   idle timeout between chunks instead of a total timeout, so a long answer is fine and a stalled one ends.
//! * Up to three attempts for a request that has not delivered a byte yet when the provider answers 429, 5xx or the
//!   connection fails; `Retry-After` wins over the backoff (capped), and the person can stop the run during the wait.
//! * Error bodies are read to 64 KiB, turned into one sentence and scrubbed of the key.
//! * A stream is read to 8 MiB at most; more is a size error, not memory growth.

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::sse::Splitter;
use super::{ApiKey, ErrorKind, Outcome, ProviderError};

/// A request ready to send.
#[derive(Debug, Clone)]
pub struct Built {
    /// The full URL.
    pub url: String,
    /// Headers (the key is among them; this value is never logged or serialized).
    pub headers: Vec<(String, String)>,
    /// The JSON body.
    pub body: Value,
}

/// An incremental reader of one provider's stream.
pub trait Parser: Send {
    /// Handle one SSE event; text deltas go to `sink`.
    fn event(
        &mut self,
        name: Option<&str>,
        data: &str,
        sink: &mut (dyn FnMut(&str) + Send),
    ) -> Result<(), ProviderError>;
    /// Handle a whole non-streamed JSON answer (some compatible servers ignore `stream`).
    fn whole(
        &mut self,
        _body: &Value,
        _sink: &mut (dyn FnMut(&str) + Send),
    ) -> Result<(), ProviderError> {
        Err(ProviderError::new(
            ErrorKind::Protocol,
            "The provider did not stream its answer.",
        ))
    }
    /// The finished outcome. `estimated_input` fills in usage when the provider sent none.
    fn finish(self: Box<Self>, estimated_input: u64) -> Result<Outcome, ProviderError>;
}

/// Rate-limit facts a provider put in the response headers.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct RateInfo {
    /// Requests left in the window, when reported.
    pub remaining_requests: Option<String>,
    /// Tokens left in the window, when reported.
    pub remaining_tokens: Option<String>,
    /// `Retry-After` in seconds.
    pub retry_after_s: Option<u64>,
}

impl RateInfo {
    fn from(h: &HeaderMap) -> Self {
        let get = |names: &[&str]| {
            names
                .iter()
                .find_map(|n| h.get(*n).and_then(|v| v.to_str().ok()).map(str::to_owned))
        };
        Self {
            remaining_requests: get(&[
                "x-ratelimit-remaining-requests",
                "anthropic-ratelimit-requests-remaining",
            ]),
            remaining_tokens: get(&[
                "x-ratelimit-remaining-tokens",
                "anthropic-ratelimit-tokens-remaining",
            ]),
            retry_after_s: get(&["retry-after"]).and_then(|v| v.trim().parse::<u64>().ok()),
        }
    }
}

/// Limits and timings. The defaults are the product's; tests shorten them.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Attempts for a request that has delivered nothing.
    pub attempts: u32,
    /// First backoff; doubles each attempt.
    pub backoff: Duration,
    /// Longest wait honoured from `Retry-After`.
    pub max_wait: Duration,
    /// Longest silence between chunks.
    pub idle: Duration,
    /// Connect timeout.
    pub connect: Duration,
    /// Most bytes read from one streamed answer.
    pub max_stream_bytes: usize,
    /// Most bytes read from an error body.
    pub max_error_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            attempts: 3,
            backoff: Duration::from_secs(1),
            max_wait: Duration::from_secs(20),
            idle: Duration::from_secs(60),
            connect: Duration::from_secs(15),
            max_stream_bytes: 8 * 1024 * 1024,
            max_error_bytes: 64 * 1024,
        }
    }
}

/// The client.
#[derive(Clone)]
pub struct Http {
    client: reqwest::Client,
    limits: Limits,
}

fn headers_of(built: &Built) -> Result<HeaderMap, ProviderError> {
    let mut map = HeaderMap::new();
    for (k, v) in &built.headers {
        let name = HeaderName::from_bytes(k.as_bytes()).map_err(|_| {
            ProviderError::new(ErrorKind::BadRequest, "A request header is not valid.")
        })?;
        let mut value = HeaderValue::from_str(v).map_err(|_| {
            ProviderError::new(ErrorKind::BadRequest, "A request header is not valid.")
        })?;
        if name.as_str().contains("key") || name == reqwest::header::AUTHORIZATION {
            value.set_sensitive(true);
        }
        map.insert(name, value);
    }
    Ok(map)
}

/// Turn an error status and body into a person-readable failure without the key.
pub fn error_from(status: u16, body: &str, rate: &RateInfo, key: Option<&str>) -> ProviderError {
    let kind = match status {
        401 | 403 => ErrorKind::Auth,
        408 => ErrorKind::Timeout,
        413 => ErrorKind::TooLarge,
        429 => ErrorKind::RateLimited,
        503 | 529 => ErrorKind::Overloaded,
        s if s >= 500 => ErrorKind::Server,
        _ => ErrorKind::BadRequest,
    };
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            [
                v.pointer("/error/message"),
                v.pointer("/message"),
                v.pointer("/error"),
                v.pointer("/detail"),
            ]
            .into_iter()
            .flatten()
            .find_map(|x| x.as_str().map(str::to_owned))
        })
        .unwrap_or_else(|| body.chars().take(200).collect());
    let lead = match kind {
        ErrorKind::Auth => "The provider refused the key",
        ErrorKind::RateLimited => "The provider is limiting requests",
        ErrorKind::Overloaded => "The provider is overloaded",
        ErrorKind::Server => "The provider had an error",
        ErrorKind::TooLarge => "The request was too large",
        ErrorKind::Timeout => "The provider timed out",
        _ => "The provider refused the request",
    };
    let mut message = format!(
        "{lead} ({status}){}",
        if detail.trim().is_empty() {
            String::new()
        } else {
            format!(": {}", detail.trim())
        }
    );
    if let Some(r) = &rate.remaining_requests {
        message.push_str(&format!(" Requests left in the window: {r}."));
    }
    if let Some(s) = rate.retry_after_s {
        message.push_str(&format!(" It asks to wait {s} s."));
    }
    ProviderError {
        kind,
        status: Some(status),
        message: scrub(&message, key).chars().take(400).collect(),
        retry_after_s: rate.retry_after_s,
    }
}

/// Remove the key (and anything that looks like a bearer header) from a message.
pub fn scrub(text: &str, key: Option<&str>) -> String {
    let mut out = text.to_owned();
    if let Some(k) = key.filter(|k| k.len() >= 6) {
        out = out.replace(k, "[key]");
    }
    out
}

impl Http {
    /// A client with the product limits.
    pub fn new() -> Self {
        Self::with_limits(Limits::default())
    }

    /// A client with explicit limits.
    pub fn with_limits(limits: Limits) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(limits.connect)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("Zeus/", env!("CARGO_PKG_VERSION"), " (agent)"))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client, limits }
    }

    /// The limits in force.
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    async fn send_once(
        &self,
        built: &Built,
        method_get: bool,
        cancel: &CancellationToken,
        key: Option<&str>,
    ) -> Result<reqwest::Response, ProviderError> {
        let headers = headers_of(built)?;
        let request = if method_get {
            self.client.get(&built.url).headers(headers)
        } else {
            self.client
                .post(&built.url)
                .headers(headers)
                .json(&built.body)
        };
        let response = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ProviderError::new(ErrorKind::Cancelled, "Stopped.")),
            r = request.send() => r,
        };
        response.map_err(|e| {
            let kind = if e.is_timeout() {
                ErrorKind::Timeout
            } else {
                ErrorKind::Network
            };
            let what = if e.is_connect() {
                "Could not connect to the provider"
            } else if e.is_timeout() {
                "The provider did not answer in time"
            } else {
                "The connection to the provider failed"
            };
            ProviderError::new(kind, scrub(&format!("{what}."), key))
        })
    }

    async fn read_error(
        &self,
        response: reqwest::Response,
        key: Option<&str>,
        cancel: &CancellationToken,
    ) -> ProviderError {
        let status = response.status().as_u16();
        let rate = RateInfo::from(response.headers());
        let mut body = Vec::new();
        let mut response = response;
        loop {
            let next = tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                r = tokio::time::timeout(Duration::from_secs(10), response.chunk()) => r,
            };
            match next {
                Ok(Ok(Some(chunk))) => {
                    body.extend_from_slice(&chunk);
                    if body.len() >= self.limits.max_error_bytes {
                        break;
                    }
                }
                _ => break,
            }
        }
        error_from(status, &String::from_utf8_lossy(&body), &rate, key)
    }

    async fn wait(
        &self,
        error: &ProviderError,
        attempt: u32,
        cancel: &CancellationToken,
    ) -> Result<(), ProviderError> {
        let backoff = self.limits.backoff.saturating_mul(1 << attempt.min(6));
        let wait = error
            .retry_after_s
            .map_or(backoff, |s| {
                Duration::from_secs(s).min(self.limits.max_wait)
            })
            .min(self.limits.max_wait);
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(ProviderError::new(ErrorKind::Cancelled, "Stopped.")),
            () = tokio::time::sleep(wait) => Ok(()),
        }
    }

    /// Send a streaming request and feed the parser. Returns the rate facts and the attempts made.
    pub async fn stream(
        &self,
        built: &Built,
        mut parser: Box<dyn Parser>,
        sink: &mut (dyn FnMut(&str) + Send),
        cancel: &CancellationToken,
        key: Option<&ApiKey>,
        estimated_input: u64,
    ) -> Result<(Outcome, RateInfo), ProviderError> {
        if super::local_agent::agent_for_url(&built.url).is_some() {
            let v = tokio::select! {
                v = super::local_agent::complete(&built.url, &built.body, Duration::from_secs(240)) => v.map_err(|e| ProviderError::new(ErrorKind::BadRequest, e.to_string()))?,
                () = cancel.cancelled() => return Err(ProviderError::new(ErrorKind::Cancelled, "Stopped.")),
            };
            parser.whole(&v, sink)?;
            return Ok((parser.finish(estimated_input)?, RateInfo::default()));
        }
        let key_text = key.map(ApiKey::expose);
        let mut attempt = 0;
        let response = loop {
            let result = match self.send_once(built, false, cancel, key_text).await {
                Ok(r) if r.status().is_success() => Ok(r),
                Ok(r) => Err(self.read_error(r, key_text, cancel).await),
                Err(e) => Err(e),
            };
            match result {
                Ok(r) => break r,
                Err(e) if e.retryable() && attempt + 1 < self.limits.attempts => {
                    self.wait(&e, attempt, cancel).await?;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        };
        let rate = RateInfo::from(response.headers());
        let is_stream = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("text/event-stream"));
        let mut response = response;
        let mut splitter = Splitter::new();
        let mut total = 0usize;
        let mut whole = Vec::new();
        loop {
            let next = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(ProviderError::new(ErrorKind::Cancelled, "Stopped.")),
                r = tokio::time::timeout(self.limits.idle, response.chunk()) => r,
            };
            let chunk = match next {
                Err(_) => {
                    return Err(ProviderError::new(
                        ErrorKind::Timeout,
                        "The provider stopped sending the answer.",
                    ));
                }
                Ok(Err(_)) => {
                    return Err(ProviderError::new(
                        ErrorKind::Network,
                        "The connection to the provider dropped while it was answering.",
                    ));
                }
                Ok(Ok(None)) => break,
                Ok(Ok(Some(c))) => c,
            };
            total += chunk.len();
            if total > self.limits.max_stream_bytes {
                return Err(ProviderError::new(
                    ErrorKind::TooLarge,
                    "The provider's answer is larger than Zeus accepts (8 MiB). It was cut off.",
                ));
            }
            if is_stream {
                for ev in splitter.feed(&chunk) {
                    parser.event(ev.name.as_deref(), &ev.data, sink)?;
                }
            } else {
                whole.extend_from_slice(&chunk);
            }
        }
        if is_stream {
            if let Some(ev) = splitter.finish() {
                parser.event(ev.name.as_deref(), &ev.data, sink)?;
            }
        } else {
            let v: Value = serde_json::from_slice(&whole).map_err(|_| {
                ProviderError::new(
                    ErrorKind::Protocol,
                    "The provider's answer was not in the expected format.",
                )
            })?;
            parser.whole(&v, sink)?;
        }
        let outcome = parser.finish(estimated_input)?;
        Ok((outcome, rate))
    }

    /// A small GET returning JSON (model lists).
    pub async fn get_json(
        &self,
        built: &Built,
        cancel: &CancellationToken,
        key: Option<&ApiKey>,
    ) -> Result<Value, ProviderError> {
        let key_text = key.map(ApiKey::expose);
        let mut attempt = 0;
        loop {
            let error = match self.send_once(built, true, cancel, key_text).await {
                Ok(r) if r.status().is_success() => {
                    let bytes = tokio::select! {
                        biased;
                        () = cancel.cancelled() => return Err(ProviderError::new(ErrorKind::Cancelled, "Stopped.")),
                        r = r.bytes() => r,
                    };
                    let bytes = bytes.map_err(|_| {
                        ProviderError::new(
                            ErrorKind::Network,
                            "The connection to the provider dropped.",
                        )
                    })?;
                    if bytes.len() > 2 * 1024 * 1024 {
                        return Err(ProviderError::new(
                            ErrorKind::TooLarge,
                            "The provider's list is too large.",
                        ));
                    }
                    return serde_json::from_slice(&bytes).map_err(|_| {
                        ProviderError::new(
                            ErrorKind::Protocol,
                            "The provider's answer was not in the expected format.",
                        )
                    });
                }
                Ok(r) => self.read_error(r, key_text, cancel).await,
                Err(e) => e,
            };
            if error.retryable() && attempt + 1 < self.limits.attempts {
                self.wait(&error, attempt, cancel).await?;
                attempt += 1;
                continue;
            }
            return Err(error);
        }
    }
}

impl Default for Http {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_messages_carry_the_status_the_detail_and_never_the_key() {
        let rate = RateInfo {
            remaining_requests: Some("0".into()),
            remaining_tokens: None,
            retry_after_s: Some(7),
        };
        let e = error_from(
            429,
            "{\"error\":{\"message\":\"Rate limit reached for key ZEUS-CANARY-abcdef\"}}",
            &rate,
            Some("ZEUS-CANARY-abcdef"),
        );
        assert_eq!(e.kind, ErrorKind::RateLimited);
        assert_eq!(e.retry_after_s, Some(7));
        assert!(e.message.contains("429") && e.message.contains("7 s"));
        assert!(!e.message.contains("ZEUS-CANARY"), "{}", e.message);
    }

    #[test]
    fn statuses_map_to_kinds() {
        let r = RateInfo::default();
        assert_eq!(error_from(401, "{}", &r, None).kind, ErrorKind::Auth);
        assert_eq!(error_from(529, "{}", &r, None).kind, ErrorKind::Overloaded);
        assert_eq!(error_from(502, "x", &r, None).kind, ErrorKind::Server);
        assert_eq!(error_from(422, "x", &r, None).kind, ErrorKind::BadRequest);
        assert!(error_from(503, "x", &r, None).retryable());
        assert!(!error_from(401, "x", &r, None).retryable());
    }
}
