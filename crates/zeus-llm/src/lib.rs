//! Zeus LLM access: the provider ladder, privacy tiers, spend accounting, the signed policy manifest, the
//! OpenRouter PKCE helper and the secret-store abstraction.
//!
//! Design: `docs/ARCHITECTURE.md` section 9 and ADR 0008. Default behaviour with no configuration is **no model
//! access**. Models see metadata only unless the user raises the [`PrivacyTier`]; cookies, tokens and signed-URL
//! queries are redacted at every tier. Consumer subscription tokens (Claude, Gemini) are never extracted or
//! replayed: there is deliberately no [`ProviderKind`] for them.
//!
//! The crate carries wire types and pure logic by default. Network and OS keyring backends sit behind the
//! `http` and `os-keyring` features so thin clients (`zeus-api`) do not link them.

#![forbid(unsafe_code)]

pub mod error;
pub mod ladder;
pub mod pkce;
pub mod policy;
pub mod privacy;
pub mod provider;
pub mod secrets;
pub mod spend;

pub use error::{LlmError, LlmErrorClass};
pub use ladder::{Ladder, LadderConfig};
pub use pkce::PkcePair;
pub use policy::{PolicyEnvelope, PolicyManifest, ProviderPolicy, TrustedKeys};
pub use privacy::{PrivacyTier, redact_text};
pub use provider::{
    LlmContent, LlmProvider, LlmRequest, LlmResponse, Message, ProviderKind, ProviderStatus, Role,
    TaskKind, Usage,
};
pub use secrets::{MemorySecretStore, SecretStore};
pub use spend::{SpendCaps, SpendLedger};
