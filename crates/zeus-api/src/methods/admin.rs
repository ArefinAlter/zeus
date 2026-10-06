//! `audit.*`, `config.*`, `doctor.*`, `llm.*`, `clients.*`.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeus_llm::{PrivacyTier, ProviderKind, ProviderStatus, SpendCaps};
use zeus_types::{AuditHead, SecretString, Timestamp};

use crate::kind::{ClientKind, ClientRole};

/// `audit.tail` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditTailParams {
    /// Number of newest records, 1 to 100. Absent: 20.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Only records with a larger `seq`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_seq: Option<u64>,
}

/// `audit.tail` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditTailResult {
    /// Records as in `audit-record.v1.schema.json` (04), oldest first.
    pub records: Vec<serde_json::Value>,
    /// Head of the chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<AuditHead>,
}

/// `audit.verify` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditVerifyParams {
    /// Start verification at this `seq` (default: genesis).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_seq: Option<u64>,
}

/// `audit.verify` result. Tamper evidence has honest limits (04): a clean result proves consistency of the chain,
/// not that the latest records were not truncated unless an anchor was kept elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditVerifyResult {
    /// The chain verified end to end.
    pub ok: bool,
    /// Records checked.
    pub records_checked: u64,
    /// First `seq` that fails verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_broken_seq: Option<u64>,
    /// Head after verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<AuditHead>,
}

/// `config.get` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigGetParams {
    /// A dotted key, or absent for everything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// `config.get` result. Secrets are never returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigGetResult {
    /// Values by dotted key.
    pub values: BTreeMap<String, serde_json::Value>,
    /// Keys withheld because they hold secrets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redacted_keys: Vec<String>,
}

/// `config.set` parameters (human only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigSetParams {
    /// Dotted key.
    pub key: String,
    /// New value.
    pub value: serde_json::Value,
}

/// `config.set` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigSetResult {
    /// Previous value, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<serde_json::Value>,
    /// The change took effect.
    pub applied: bool,
    /// A daemon restart is needed for it to apply.
    pub restart_required: bool,
}

/// `doctor.run` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DoctorParams {}

/// Outcome of one health check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// Fine.
    Ok,
    /// Works but worth attention.
    Warn,
    /// Broken.
    Fail,
}

/// One health check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DoctorCheck {
    /// Stable id, for example `journal.writable`.
    pub id: String,
    /// Outcome.
    pub status: CheckStatus,
    /// One line.
    pub summary: String,
    /// What to do, in plain words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

/// `doctor.run` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DoctorResult {
    /// The checks, in a stable order.
    pub checks: Vec<DoctorCheck>,
}

/// `llm.status` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LlmStatusParams {}

/// One rung of the ladder as the user sees it in `zeus auth`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RungStatus {
    /// The provider.
    pub provider: ProviderKind,
    /// Rung 1 to 6.
    pub rung: u8,
    /// Status after policy.
    pub status: ProviderStatus,
    /// Credentials or endpoint configured.
    pub configured: bool,
    /// Why the status is not `enabled`, from the policy manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `llm.status` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LlmStatusResult {
    /// The ladder in order.
    pub rungs: Vec<RungStatus>,
    /// Current privacy tier.
    pub privacy: PrivacyTier,
    /// Spend caps.
    pub spend_caps: SpendCaps,
    /// Spent today in micro-USD.
    pub spent_day_micro_usd: u64,
    /// Spent this month in micro-USD.
    pub spent_month_micro_usd: u64,
    /// Sequence of the applied policy manifest, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_sequence: Option<u64>,
}

/// `llm.configure` parameters (human only). The key travels once over the local pipe and goes straight to the OS
/// keyring; it is never echoed, logged or stored anywhere else.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct LlmConfigureParams {
    /// Privacy tier.
    #[serde(default)]
    pub privacy: Option<PrivacyTier>,
    /// Allow providers marked experimental.
    #[serde(default)]
    pub allow_experimental: Option<bool>,
    /// New spend caps.
    #[serde(default)]
    pub spend_caps: Option<SpendCaps>,
    /// Provider the credential belongs to.
    #[serde(default)]
    pub provider: Option<ProviderKind>,
    /// API key for `provider`.
    #[serde(default)]
    pub api_key: Option<SecretString>,
}

impl Serialize for LlmConfigureParams {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        // Explicit and grep-able: the key is sent over the local pipe by the human's CLI, nowhere else.
        let mut state = serializer.serialize_struct("LlmConfigureParams", 5)?;
        state.serialize_field("privacy", &self.privacy)?;
        state.serialize_field("allow_experimental", &self.allow_experimental)?;
        state.serialize_field("spend_caps", &self.spend_caps)?;
        state.serialize_field("provider", &self.provider)?;
        state.serialize_field("api_key", &self.api_key.as_ref().map(SecretString::expose))?;
        state.end()
    }
}

/// `llm.configure` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LlmConfigureResult {
    /// The ladder after the change.
    pub rungs: Vec<RungStatus>,
}

/// `clients.list` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientsListParams {}

/// A client credential record (never the token).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientRecord {
    /// Credential id.
    pub credential_id: String,
    /// Authority class.
    pub kind: ClientKind,
    /// Role.
    pub role: ClientRole,
    /// Creation.
    pub created_at: Timestamp,
    /// Last successful authentication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<Timestamp>,
    /// Revoked credentials stay listed.
    pub revoked: bool,
}

/// `clients.list` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientsListResult {
    /// All credentials.
    pub clients: Vec<ClientRecord>,
}

/// `clients.revoke` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientsRevokeParams {
    /// Credential id.
    pub credential_id: String,
}

/// `clients.revoke` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientsRevokeResult {
    /// The credential after revocation; existing connections using it are closed.
    pub client: ClientRecord,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_api_key_is_never_debug_printed_but_is_serialised_for_the_pipe() {
        let params = LlmConfigureParams {
            api_key: Some(SecretString::from("ZEUS-CANARY-key")),
            ..Default::default()
        };
        assert!(!format!("{params:?}").contains("ZEUS-CANARY"));
        let wire = serde_json::to_string(&params).unwrap();
        assert!(wire.contains("ZEUS-CANARY-key"));
        let back: LlmConfigureParams = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.api_key.unwrap().expose(), "ZEUS-CANARY-key");
    }
}
