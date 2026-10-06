//! Provider kinds, the request and response model, and the [`LlmProvider`] trait.

use std::collections::BTreeMap;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::LlmError;
use crate::privacy::PrivacyTier;

/// Every way Zeus can obtain model output, in ladder order (see [`ProviderKind::LADDER`]).
///
/// There is intentionally no variant for Claude or Gemini consumer subscription tokens.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Rung 1: the host agent over MCP does the thinking; Zeus runs no model.
    HostAgent,
    /// Rung 2: MCP sampling, when the client supports it. Opportunistic.
    McpSampling,
    /// Rung 3: Sign in with ChatGPT (loopback OAuth for local apps).
    ChatgptOauth,
    /// Rung 4: delegate a narrow single-shot task to the user's own CLI (for example `codex exec`).
    CliDelegate,
    /// Rung 5: OpenRouter one-click OAuth with PKCE.
    OpenrouterOauth,
    /// Rung 6: raw API key, OpenAI-compatible endpoint.
    ApiKeyOpenaiCompatible,
    /// Rung 6: raw API key, Anthropic API.
    ApiKeyAnthropic,
    /// Rung 6: local model served by Ollama.
    LocalOllama,
    /// Rung 6: local model served by llama.cpp.
    LocalLlamaCpp,
    /// Rung 6: local model served by LM Studio.
    LocalLmStudio,
}

/// Where the prompt content goes when a provider is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locality {
    /// Stays on this machine.
    Local,
    /// Leaves the machine to a third-party service.
    Remote,
    /// Goes to the host agent that already holds the conversation; Zeus sends nothing new to a third party.
    HostAgent,
}

impl ProviderKind {
    /// Ladder order: first usable rung wins.
    pub const LADDER: [ProviderKind; 10] = [
        Self::HostAgent,
        Self::McpSampling,
        Self::ChatgptOauth,
        Self::CliDelegate,
        Self::OpenrouterOauth,
        Self::LocalOllama,
        Self::LocalLlamaCpp,
        Self::LocalLmStudio,
        Self::ApiKeyOpenaiCompatible,
        Self::ApiKeyAnthropic,
    ];

    /// Rung number 1 to 6 as in ADR 0008.
    pub const fn rung(self) -> u8 {
        match self {
            Self::HostAgent => 1,
            Self::McpSampling => 2,
            Self::ChatgptOauth => 3,
            Self::CliDelegate => 4,
            Self::OpenrouterOauth => 5,
            Self::ApiKeyOpenaiCompatible
            | Self::ApiKeyAnthropic
            | Self::LocalOllama
            | Self::LocalLlamaCpp
            | Self::LocalLmStudio => 6,
        }
    }

    /// Where content goes when this provider is used.
    pub const fn locality(self) -> Locality {
        match self {
            Self::HostAgent | Self::McpSampling => Locality::HostAgent,
            Self::LocalOllama | Self::LocalLlamaCpp | Self::LocalLmStudio => Locality::Local,
            // A user's own CLI may call a remote service; treat it as remote.
            Self::ChatgptOauth
            | Self::CliDelegate
            | Self::OpenrouterOauth
            | Self::ApiKeyOpenaiCompatible
            | Self::ApiKeyAnthropic => Locality::Remote,
        }
    }

    /// Stable `snake_case` name, identical to the serde form.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HostAgent => "host_agent",
            Self::McpSampling => "mcp_sampling",
            Self::ChatgptOauth => "chatgpt_oauth",
            Self::CliDelegate => "cli_delegate",
            Self::OpenrouterOauth => "openrouter_oauth",
            Self::ApiKeyOpenaiCompatible => "api_key_openai_compatible",
            Self::ApiKeyAnthropic => "api_key_anthropic",
            Self::LocalOllama => "local_ollama",
            Self::LocalLlamaCpp => "local_llama_cpp",
            Self::LocalLmStudio => "local_lm_studio",
        }
    }
}

/// Status of a provider according to the signed policy manifest or local configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatus {
    /// Usable.
    Enabled,
    /// Usable only when the user opted in to experimental providers.
    Experimental,
    /// Never used.
    Disabled,
}

/// What the request is for. A closed set so policy and spend reporting can be per task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TaskKind {
    /// Draft the selectors of a workflow plan from page samples.
    DraftPlan,
    /// Classify a file by metadata.
    ClassifyFile,
    /// Suggest a file name.
    SuggestName,
    /// Extract named fields from a short text.
    ExtractFields,
}

/// Speaker of a [`Message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Instructions from Zeus (never from a file or page).
    System,
    /// Content for the model to work on.
    User,
    /// A previous model turn.
    Assistant,
}

/// One chat message. Content passes through [`crate::privacy::redact_text`] before it leaves the process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Message {
    /// Speaker.
    pub role: Role,
    /// Text.
    pub content: String,
}

/// A request to a provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LlmRequest {
    /// What the request is for.
    pub task: TaskKind,
    /// Conversation.
    pub messages: Vec<Message>,
    /// JSON Schema the answer must satisfy, for structured output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<serde_json::Value>,
    /// Upper bound on generated tokens.
    pub max_output_tokens: u32,
    /// What may enter the prompt.
    pub privacy: PrivacyTier,
}

/// Model output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum LlmContent {
    /// Free text.
    Text(String),
    /// Structured output that matched the requested schema.
    Json(serde_json::Value),
}

/// Token and cost accounting for one call. Cost is an integer in micro-USD (1e-6 USD) when known.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Usage {
    /// Prompt tokens.
    pub input_tokens: u64,
    /// Generated tokens.
    pub output_tokens: u64,
    /// Cost in micro-USD, if the provider reports or the price table allows computing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micro_usd: Option<u64>,
}

/// A provider's answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LlmResponse {
    /// The output.
    pub content: LlmContent,
    /// Accounting.
    pub usage: Usage,
    /// Which provider answered.
    pub provider: ProviderKind,
    /// Model identifier as reported by the provider.
    pub model: String,
}

/// A usable model backend. Implementations live in the daemon or in optional feature modules; the router
/// ([`crate::ladder::Ladder`]) picks one per request.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Which rung this is.
    fn kind(&self) -> ProviderKind;

    /// True when credentials or endpoints are configured so a call can be attempted. Must not touch the network.
    fn is_configured(&self) -> bool;

    /// Run one request. The caller has already applied the privacy tier, redaction and spend check.
    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, LlmError>;
}

/// Per-provider status map, as carried by the policy manifest and local configuration.
pub type StatusMap = BTreeMap<ProviderKind, ProviderStatus>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_is_sorted_by_rung_and_unique() {
        let rungs: Vec<u8> = ProviderKind::LADDER.iter().map(|k| k.rung()).collect();
        let mut sorted = rungs.clone();
        sorted.sort();
        assert_eq!(rungs, sorted);
        let mut kinds = ProviderKind::LADDER.to_vec();
        kinds.sort();
        kinds.dedup();
        assert_eq!(kinds.len(), 10);
    }

    #[test]
    fn serde_name_matches_as_str() {
        for kind in ProviderKind::LADDER {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
        }
    }

    #[test]
    fn no_consumer_subscription_provider_exists() {
        for kind in ProviderKind::LADDER {
            let name = kind.as_str();
            assert!(
                !name.contains("claude") && !name.contains("gemini"),
                "{name}"
            );
        }
    }
}
