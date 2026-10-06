//! Errors of the LLM layer.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Coarse class of an [`LlmError`], stable for display and retry decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmErrorClass {
    /// No usable provider is configured (the default state).
    NotConfigured,
    /// The request would send more than the privacy tier allows.
    PrivacyViolation,
    /// A spend cap would be exceeded.
    BudgetExceeded,
    /// The provider is switched off by the signed policy manifest or by the user.
    DisabledByPolicy,
    /// Transient provider or network failure.
    ProviderUnavailable,
    /// The provider rejected the credential.
    AuthFailed,
    /// The provider answered but not with the requested structure.
    SchemaViolation,
    /// The feature is not implemented yet.
    NotImplemented,
    /// A bug or invariant violation.
    Internal,
}

/// Failure of an LLM operation. Messages never contain secrets.
#[derive(Debug, Error)]
pub enum LlmError {
    /// No provider rung is usable for this request.
    #[error("no LLM provider is configured or usable")]
    NotConfigured,
    /// The request exceeds the privacy tier.
    #[error("privacy tier {tier} does not allow this request: {reason}")]
    PrivacyViolation {
        /// Tier that was in force.
        tier: &'static str,
        /// Why the request was refused.
        reason: &'static str,
    },
    /// A hard spend cap would be exceeded.
    #[error("spend cap exceeded ({period}): {spent_micro_usd} of {cap_micro_usd} micro-USD")]
    BudgetExceeded {
        /// `daily` or `monthly`.
        period: &'static str,
        /// Already spent in the period, in micro-USD.
        spent_micro_usd: u64,
        /// The cap, in micro-USD.
        cap_micro_usd: u64,
    },
    /// The provider is disabled by policy.
    #[error("provider {0} is disabled by policy")]
    DisabledByPolicy(&'static str),
    /// The provider could not be reached or answered with a transient failure.
    #[error("provider unavailable: {0}")]
    ProviderUnavailable(String),
    /// The credential was rejected.
    #[error("provider rejected the credential")]
    AuthFailed,
    /// The response did not match the requested schema.
    #[error("provider response did not match the requested schema")]
    SchemaViolation,
    /// Not implemented yet.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
    /// Secret-store failure.
    #[error("secret store: {0}")]
    SecretStore(String),
    /// Policy manifest failure.
    #[error(transparent)]
    Policy(#[from] crate::policy::PolicyError),
}

impl LlmError {
    /// The coarse class of this error.
    pub fn class(&self) -> LlmErrorClass {
        match self {
            Self::NotConfigured => LlmErrorClass::NotConfigured,
            Self::PrivacyViolation { .. } => LlmErrorClass::PrivacyViolation,
            Self::BudgetExceeded { .. } => LlmErrorClass::BudgetExceeded,
            Self::DisabledByPolicy(_) | Self::Policy(_) => LlmErrorClass::DisabledByPolicy,
            Self::ProviderUnavailable(_) => LlmErrorClass::ProviderUnavailable,
            Self::AuthFailed => LlmErrorClass::AuthFailed,
            Self::SchemaViolation => LlmErrorClass::SchemaViolation,
            Self::NotImplemented(_) => LlmErrorClass::NotImplemented,
            Self::SecretStore(_) => LlmErrorClass::Internal,
        }
    }
}
