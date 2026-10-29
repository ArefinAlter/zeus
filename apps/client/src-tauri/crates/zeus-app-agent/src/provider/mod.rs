//! Model providers: one neutral request and one neutral answer, three wire formats.
//!
//! * [`anthropic`]: the Anthropic Messages API (`/v1/messages`, SSE with `content_block_*` events, `tool_use` blocks).
//! * [`openai`]: Chat Completions, which OpenAI, Mistral, DeepSeek, Alibaba Qwen (DashScope compatible mode), Moonshot
//!   Kimi, Zhipu GLM, a local Ollama and any other OpenAI-compatible endpoint all speak. One adapter serves them all,
//!   which is why it is Chat Completions and not the newer Responses API: it is the format the compatible servers
//!   implement, it is still supported by OpenAI, and one code path means one set of tests.
//! * [`gemini`]: Google's `generateContent` (`streamGenerateContent?alt=sse`) with function calling.
//!
//! Every adapter is split in two: a pure builder (neutral request to URL, headers and JSON body) and an incremental
//! parser (SSE events to neutral deltas). The HTTP layer ([`http`]) streams, retries, cancels and limits sizes; the
//! parsers and builders are tested on their own with canned wire data.

#[path = "../../../../../../../shared/local_agent.rs"]
pub mod local_agent;

pub mod anthropic;
pub mod gemini;
pub mod http;
pub mod jsonproto;
pub mod openai;
pub mod sse;

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Which provider a configured entry talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The locally installed Codex, using its own sign-in.
    Codex,
    /// The locally installed Claude Code, using its own sign-in.
    Claude,
    /// The locally installed Cursor CLI, using its own sign-in.
    Cursor,
    /// The locally installed Antigravity CLI, using its own sign-in.
    Antigravity,
    /// The official Grok Build CLI, using a dedicated signed-in profile.
    Grok,
    /// Anthropic Messages API.
    Anthropic,
    /// OpenAI Chat Completions.
    Openai,
    /// Google Gemini `generateContent`.
    Google,
    /// Mistral (OpenAI-compatible chat).
    Mistral,
    /// DeepSeek (OpenAI-compatible chat).
    Deepseek,
    /// Alibaba Qwen through DashScope's OpenAI-compatible mode.
    Qwen,
    /// Moonshot Kimi (OpenAI-compatible chat).
    Kimi,
    /// Zhipu GLM (OpenAI-compatible chat).
    Glm,
    /// A local Ollama server.
    Ollama,
    /// Any other OpenAI-compatible base URL.
    OpenaiCompatible,
}

/// The wire format a [`Kind`] speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// Anthropic Messages.
    Anthropic,
    /// Gemini `generateContent`.
    Gemini,
    /// OpenAI Chat Completions.
    OpenaiChat,
}

impl Kind {
    /// Every kind, in the order the ladder lists them.
    pub const ALL: [Kind; 15] = [
        Kind::Codex,
        Kind::Claude,
        Kind::Cursor,
        Kind::Antigravity,
        Kind::Grok,
        Kind::Anthropic,
        Kind::Openai,
        Kind::Google,
        Kind::Mistral,
        Kind::Deepseek,
        Kind::Qwen,
        Kind::Kimi,
        Kind::Glm,
        Kind::Ollama,
        Kind::OpenaiCompatible,
    ];

    /// The wire format.
    pub fn wire(self) -> Wire {
        match self {
            Kind::Anthropic => Wire::Anthropic,
            Kind::Google => Wire::Gemini,
            _ => Wire::OpenaiChat,
        }
    }

    /// Whether this is an existing installed-agent sign-in.
    pub fn is_subscription(self) -> bool {
        matches!(
            self,
            Self::Codex | Self::Claude | Self::Cursor | Self::Antigravity | Self::Grok
        )
    }

    /// The public base URL of the provider (empty for a custom endpoint).
    pub fn default_base_url(self) -> &'static str {
        match self {
            Kind::Codex => "https://codex.local-agent.invalid/v1",
            Kind::Claude => "https://claude.local-agent.invalid/v1",
            Kind::Cursor => "https://cursor.local-agent.invalid/v1",
            Kind::Antigravity => "https://antigravity.local-agent.invalid/v1",
            Kind::Grok => "https://grok.local-agent.invalid/v1",
            Kind::Anthropic => "https://api.anthropic.com",
            Kind::Openai => "https://api.openai.com/v1",
            Kind::Google => "https://generativelanguage.googleapis.com/v1beta",
            Kind::Mistral => "https://api.mistral.ai/v1",
            Kind::Deepseek => "https://api.deepseek.com",
            Kind::Qwen => "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
            Kind::Kimi => "https://api.moonshot.ai/v1",
            Kind::Glm => "https://api.z.ai/api/paas/v4",
            Kind::Ollama => "http://127.0.0.1:11434",
            Kind::OpenaiCompatible => "",
        }
    }

    /// Whether a key is needed (a local Ollama and a custom endpoint may run without one).
    pub fn needs_key(self) -> bool {
        !matches!(
            self,
            Kind::Ollama
                | Kind::OpenaiCompatible
                | Kind::Codex
                | Kind::Claude
                | Kind::Cursor
                | Kind::Antigravity
                | Kind::Grok
        )
    }

    /// A display name.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Codex => "Codex (existing sign-in)",
            Kind::Claude => "Claude Code (existing sign-in)",
            Kind::Cursor => "Cursor (existing sign-in)",
            Kind::Antigravity => "Antigravity (existing sign-in)",
            Kind::Grok => "Grok Build (dedicated sign-in)",
            Kind::Anthropic => "Anthropic",
            Kind::Openai => "OpenAI",
            Kind::Google => "Google Gemini",
            Kind::Mistral => "Mistral",
            Kind::Deepseek => "DeepSeek",
            Kind::Qwen => "Qwen (DashScope)",
            Kind::Kimi => "Kimi (Moonshot)",
            Kind::Glm => "GLM (Z.ai)",
            Kind::Ollama => "Ollama, local",
            Kind::OpenaiCompatible => "OpenAI-compatible",
        }
    }

    /// Whether the usage block is only sent when asked for (`stream_options.include_usage`).
    pub fn asks_for_usage(self) -> bool {
        matches!(
            self,
            Kind::Openai | Kind::Deepseek | Kind::Qwen | Kind::Ollama
        )
    }

    /// Whether the output limit is named `max_completion_tokens` (current OpenAI models reject `max_tokens`).
    pub fn uses_completion_tokens(self) -> bool {
        matches!(self, Kind::Openai)
    }
}

/// One tool offered to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    /// `zeus_<verb>`.
    pub name: String,
    /// What it does.
    pub description: String,
    /// JSON Schema of the arguments.
    pub schema: Value,
}

/// A tool call the model asked for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The provider's id for the call (generated when the provider sends none).
    pub id: String,
    /// The tool name.
    pub name: String,
    /// The arguments.
    pub args: Value,
    /// Set when the model's arguments were not valid JSON; the call is refused with this text.
    pub args_error: Option<String>,
}

/// Provider-specific parts of a model turn that must be sent back unchanged (Gemini thought signatures).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawTurn {
    /// The provider the turn came from; used only for that kind.
    pub kind: Kind,
    /// The raw value.
    pub value: Value,
}

/// A message of the neutral transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Msg {
    /// What the person wrote.
    User(String),
    /// What the model answered, with the tool calls it made.
    Assistant {
        /// Text, possibly empty.
        text: String,
        /// Tool calls.
        calls: Vec<ToolCall>,
        /// Raw provider parts to replay.
        raw: Option<RawTurn>,
    },
    /// The answer to one tool call. The content is data, never instructions.
    ToolResult {
        /// The call this answers.
        call_id: String,
        /// The tool name.
        name: String,
        /// Text given to the model.
        content: String,
        /// Whether the call failed or was refused.
        is_error: bool,
    },
}

/// A request in neutral form.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    /// The model id the person typed.
    pub model: String,
    /// The system prompt.
    pub system: String,
    /// The transcript.
    pub messages: Vec<Msg>,
    /// The tools offered (empty: none).
    pub tools: Vec<ToolSpec>,
    /// The output limit.
    pub max_tokens: u32,
}

/// Token usage of one call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    /// Prompt tokens.
    pub input: u64,
    /// Completion tokens.
    pub output: u64,
    /// True when the provider sent no usage and these are estimates.
    pub estimated: bool,
}

/// Why the model stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stop {
    /// A natural end.
    End,
    /// It asked for tools.
    ToolUse,
    /// The output limit was reached.
    Length,
    /// The provider's safety system stopped it.
    Filtered,
    /// Unknown.
    Other,
}

/// What a finished call produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// The text, in full.
    pub text: String,
    /// The tool calls.
    pub calls: Vec<ToolCall>,
    /// Parts to replay next turn.
    pub raw: Option<RawTurn>,
    /// Usage.
    pub usage: Usage,
    /// Why it stopped.
    pub stop: Stop,
}

/// What kind of failure a provider call had.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The key was refused (401, 403).
    Auth,
    /// Too many requests or a quota (429).
    RateLimited,
    /// The provider is overloaded (503, 529).
    Overloaded,
    /// The request was refused as malformed (400, 404, 422).
    BadRequest,
    /// The provider failed (5xx).
    Server,
    /// The connection failed.
    Network,
    /// Nothing arrived for too long.
    Timeout,
    /// The person stopped the run.
    Cancelled,
    /// A size limit was hit.
    TooLarge,
    /// The answer was not in the expected format.
    Protocol,
}

/// A failed provider call. The message never contains the key.
#[derive(Debug, Clone, thiserror::Error, Serialize)]
#[error("{message}")]
pub struct ProviderError {
    /// What kind of failure.
    pub kind: ErrorKind,
    /// The HTTP status, when there was a response.
    pub status: Option<u16>,
    /// A sentence for a person.
    pub message: String,
    /// The provider's `Retry-After`, in seconds.
    pub retry_after_s: Option<u64>,
}

impl ProviderError {
    /// A failure without an HTTP status.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            status: None,
            message: message.into(),
            retry_after_s: None,
        }
    }

    /// Whether retrying the same request may succeed.
    pub fn retryable(&self) -> bool {
        matches!(
            self.kind,
            ErrorKind::RateLimited | ErrorKind::Overloaded | ErrorKind::Server | ErrorKind::Network
        )
    }
}

/// An API key. It has no `Serialize`, no `Clone` and a `Debug` that shows nothing.
pub struct ApiKey(zeroize::Zeroizing<String>);

impl ApiKey {
    /// Wrap a key.
    pub fn new(key: String) -> Self {
        Self(zeroize::Zeroizing::new(key))
    }

    /// The key text, for the one place that puts it in a header.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The last four characters, for display.
    pub fn last4(&self) -> String {
        let chars: Vec<char> = self.0.chars().collect();
        chars[chars.len().saturating_sub(4)..].iter().collect()
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(..)")
    }
}

/// Everything about one provider that a call needs, apart from the key.
#[derive(Debug, Clone)]
pub struct Target {
    /// The kind.
    pub kind: Kind,
    /// The base URL.
    pub base_url: String,
    /// Offer tools natively, or describe them in the prompt and read a JSON reply.
    pub json_tools: bool,
}

/// Whether `base` may be called over plain http: loopback and private-network addresses typed by the person.
pub fn plain_http_allowed(base: &url::Url) -> bool {
    if base.scheme() != "http" {
        return false;
    }
    match base.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(d)) => {
            d == "localhost" || d.ends_with(".local") || d.ends_with(".localhost")
        }
        None => false,
    }
}

/// Check a base URL: https, or http to a loopback or private-network address. Returns the parsed URL.
pub fn check_base_url(raw: &str) -> Result<url::Url, ProviderError> {
    let parsed = url::Url::parse(raw.trim()).map_err(|_| {
        ProviderError::new(
            ErrorKind::BadRequest,
            "The base URL is not a valid address.",
        )
    })?;
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(ProviderError::new(
            ErrorKind::BadRequest,
            "The base URL must not contain a user name, a password or a query string.",
        ));
    }
    if parsed.scheme() == "https" || plain_http_allowed(&parsed) {
        Ok(parsed)
    } else {
        Err(ProviderError::new(
            ErrorKind::BadRequest,
            "Model requests use https. Plain http is allowed only for this computer or your own network.",
        ))
    }
}

/// A rough token estimate (four characters a token), for previews, caps and providers that send no usage.
pub fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

/// Run one chat call against `target` and return its outcome. Text deltas go to `sink` as they arrive.
///
/// With `target.json_tools` the tools are described in the prompt and a JSON reply is read as a call
/// ([`jsonproto`]); otherwise they are offered natively in the provider's own format.
pub async fn chat(
    http: &http::Http,
    target: &Target,
    key: Option<&ApiKey>,
    req: &ChatRequest,
    sink: &mut (dyn FnMut(&str) + Send),
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(Outcome, http::RateInfo), ProviderError> {
    let json_mode = target.json_tools && !req.tools.is_empty();
    let prepared;
    let effective = if json_mode {
        prepared = ChatRequest {
            model: req.model.clone(),
            system: format!("{}{}", req.system, jsonproto::system_addendum(&req.tools)),
            messages: jsonproto::flatten(&req.messages),
            tools: Vec::new(),
            max_tokens: req.max_tokens,
        };
        &prepared
    } else {
        req
    };
    let (built, parser): (http::Built, Box<dyn http::Parser>) = match target.kind.wire() {
        Wire::Anthropic => (
            anthropic::build(target, key, effective),
            Box::new(anthropic::AnthropicParser::new()),
        ),
        Wire::Gemini => (
            gemini::build(target, key, effective),
            Box::new(gemini::GeminiParser::new()),
        ),
        Wire::OpenaiChat => (
            openai::build(target, key, effective),
            Box::new(openai::OpenaiParser::new()),
        ),
    };
    let estimate = estimate_tokens(&effective.system)
        + effective
            .messages
            .iter()
            .map(|m| match m {
                Msg::User(t) => estimate_tokens(t),
                Msg::Assistant { text, calls, .. } => {
                    estimate_tokens(text)
                        + calls
                            .iter()
                            .map(|c| estimate_tokens(&c.args.to_string()))
                            .sum::<u64>()
                }
                Msg::ToolResult { content, .. } => estimate_tokens(content),
            })
            .sum::<u64>()
        + effective
            .tools
            .iter()
            .map(|t| estimate_tokens(&t.schema.to_string()) + estimate_tokens(&t.description))
            .sum::<u64>();
    let (outcome, rate) = http
        .stream(&built, parser, sink, cancel, key, estimate)
        .await?;
    let outcome = if json_mode {
        jsonproto::convert(outcome, &req.tools)
    } else {
        outcome
    };
    Ok((outcome, rate))
}

/// Ask the provider which models it offers (ids only).
pub async fn list_models(
    http: &http::Http,
    target: &Target,
    key: Option<&ApiKey>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<Vec<String>, ProviderError> {
    let (built, read): (http::Built, fn(&Value) -> Vec<String>) = match target.kind.wire() {
        Wire::Anthropic => (anthropic::models_request(target, key), openai::model_ids),
        Wire::Gemini => (gemini::models_request(target, key), gemini::model_ids),
        Wire::OpenaiChat => (openai::models_request(target, key), openai::model_ids),
    };
    let v = http.get_json(&built, cancel, key).await?;
    Ok(read(&v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_never_show_in_debug_and_last4_is_the_end() {
        let key = ApiKey::new("ZEUS-CANARY-abcdef1234".into());
        assert_eq!(format!("{key:?}"), "ApiKey(..)");
        assert_eq!(key.last4(), "1234");
    }

    #[test]
    fn base_urls_need_tls_unless_local() {
        assert!(check_base_url("https://api.openai.com/v1").is_ok());
        assert!(check_base_url("http://127.0.0.1:11434").is_ok());
        assert!(check_base_url("http://localhost:8080/v1").is_ok());
        assert!(check_base_url("http://192.168.1.20:8000/v1").is_ok());
        assert!(check_base_url("http://api.example.com/v1").is_err());
        assert!(check_base_url("https://user:pw@api.example.com").is_err());
        assert!(check_base_url("https://api.example.com/v1?key=abc").is_err());
        assert!(check_base_url("ftp://x").is_err());
    }

    #[test]
    fn kinds_map_to_the_three_wire_formats() {
        assert_eq!(Kind::Anthropic.wire(), Wire::Anthropic);
        assert_eq!(Kind::Google.wire(), Wire::Gemini);
        for k in [
            Kind::Openai,
            Kind::Mistral,
            Kind::Deepseek,
            Kind::Qwen,
            Kind::Kimi,
            Kind::Glm,
            Kind::Ollama,
            Kind::OpenaiCompatible,
        ] {
            assert_eq!(k.wire(), Wire::OpenaiChat);
        }
    }
}
