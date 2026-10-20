//! `audit.*`, `config.*`, `doctor.*`, `llm.*`, `clients.*`.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeus_llm::{PrivacyTier, ProviderKind, ProviderStatus, SpendCaps};
use zeus_types::{AuditHead, SecretString, Timestamp};

use crate::kind::{ClientKind, ClientRole, Permission};

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
    /// True when this response replays a durable mutation receipt.
    #[serde(default)]
    pub replayed: bool,
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
    /// Explicitly enable or disable model tasks. Default is disabled.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Explicit model, endpoint and pricing ceilings for one provider; contains no credential.
    #[serde(default)]
    pub adapter: Option<zeus_llm::provider::HttpProviderConfig>,
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
        let fields = [
            self.enabled.is_some(),
            self.adapter.is_some(),
            self.privacy.is_some(),
            self.allow_experimental.is_some(),
            self.spend_caps.is_some(),
            self.provider.is_some(),
            self.api_key.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        let mut state = serializer.serialize_struct("LlmConfigureParams", fields)?;
        if let Some(value) = &self.enabled {
            state.serialize_field("enabled", value)?;
        }
        if let Some(value) = &self.adapter {
            state.serialize_field("adapter", value)?;
        }
        if let Some(value) = &self.privacy {
            state.serialize_field("privacy", value)?;
        }
        if let Some(value) = &self.allow_experimental {
            state.serialize_field("allow_experimental", value)?;
        }
        if let Some(value) = &self.spend_caps {
            state.serialize_field("spend_caps", value)?;
        }
        if let Some(value) = &self.provider {
            state.serialize_field("provider", value)?;
        }
        if let Some(value) = &self.api_key {
            state.serialize_field("api_key", value.expose())?;
        }
        state.end()
    }
}

/// `llm.configure` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LlmConfigureResult {
    /// True when this response replays a durable mutation receipt.
    #[serde(default)]
    pub replayed: bool,
    /// The ladder after the change.
    pub rungs: Vec<RungStatus>,
}

/// `clients.list` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientsListParams {
    /// Also list revoked credentials. Default false.
    #[serde(default)]
    pub include_revoked: bool,
}

/// A client credential as the daemon describes it. Never contains the token or its verifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientRecord {
    /// Credential id.
    pub credential_id: String,
    /// File name stem under `<state>/credentials/`.
    pub name: String,
    /// Authority class.
    pub kind: ClientKind,
    /// Role.
    pub role: ClientRole,
    /// Effective permissions, a subset of the kind's ceiling.
    pub permissions: BTreeSet<Permission>,
    /// Creation.
    pub created_at: Timestamp,
    /// Last successful authentication since the daemon started; `null` when unknown.
    #[serde(default)]
    pub last_used_at: Option<Timestamp>,
    /// Revoked credentials stay listed.
    pub revoked: bool,
    /// When it was revoked.
    #[serde(default)]
    pub revoked_at: Option<Timestamp>,
    /// Service clients only: their registered roots (inactive once revoked).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<ServiceRoot>,
}

/// A write root registered by a service client (05 section 7.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceRoot {
    /// Absolute, canonical path of the root.
    pub path: String,
    /// Optional label, untrusted display text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The owning service's credential id.
    pub client_id: String,
    /// Registration time.
    pub created_at: Timestamp,
}

/// `clients.list` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientsListResult {
    /// Credentials, oldest first.
    pub clients: Vec<ClientRecord>,
}

/// `clients.add` parameters (human-only; 05 section 9.11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientsAddParams {
    /// `[a-z][a-z0-9_-]{0,31}`, unique, not `cli`, `agent`, `a2a` or `host`.
    pub name: String,
    /// `agent` or `service`. Human and admin credentials come only from provisioning.
    pub kind: ClientKind,
    /// Default `service` for kind `service`, `mcp` for kind `agent`; must be allowed for the kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ClientRole>,
    /// Narrowing within the kind's ceiling. Default: the ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<BTreeSet<Permission>>,
    /// Idempotency key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `clients.add` result. The token is written to `credential_file` and never returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientsAddResult {
    /// True when this response replays a durable mutation receipt.
    #[serde(default)]
    pub replayed: bool,
    /// The new credential.
    pub client: ClientRecord,
    /// Absolute path of the owner-only token file.
    pub credential_file: String,
}

/// `clients.revoke` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientsRevokeParams {
    /// Credential id.
    pub credential_id: String,
    /// Idempotency key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `clients.revoke` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientsRevokeResult {
    /// True when this response replays a durable mutation receipt.
    #[serde(default)]
    pub replayed: bool,
    /// The credential after revocation.
    pub client: ClientRecord,
    /// Live connections of this credential that were closed.
    pub connections_closed: u32,
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

/// Begin a human-controlled model-provider browser sign-in.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LlmAuthStartParams {
    /// Only `chatgpt_oauth` and `openrouter_oauth` support this flow.
    pub provider: ProviderKind,
    /// Existing ChatGPT registration to reauthorize; absent creates a new registration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}
/// One-use sign-in URL. It must never be written to logs or analytics.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct LlmAuthStartResult {
    /// Opaque flow identifier owned by the requesting credential.
    pub flow_id: String,
    /// Browser URL returned only to the initiating trusted human client.
    pub authorization_url: String,
    /// Deadline of this flow.
    pub expires_at: Timestamp,
}
impl std::fmt::Debug for LlmAuthStartResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmAuthStartResult")
            .field("flow_id", &self.flow_id)
            .field("authorization_url", &"[REDACTED]")
            .finish()
    }
}
/// Inspect one owned flow, or list the current user's account registrations.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct LlmAuthStatusParams {
    /// Opaque flow identifier, absent to list saved accounts only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_id: Option<String>,
}
/// Browser sign-in lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LlmAuthState {
    /// Waiting for browser completion.
    Waiting,
    /// Credentials were saved successfully.
    Connected,
    /// Login failed; no secret-bearing diagnostics are exposed.
    Failed,
    /// The owner canceled the pending flow.
    Canceled,
    /// The listener deadline elapsed.
    Expired,
}
/// One flow's non-secret status.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LlmAuthFlow {
    /// Flow identifier.
    pub flow_id: String,
    /// Provider being connected.
    pub provider: ProviderKind,
    /// Current lifecycle state.
    pub state: LlmAuthState,
    /// Issued ChatGPT account registration after successful connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Expiry time.
    pub expires_at: Timestamp,
}
/// Account identity visible only on the human auth surface.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LlmAccount {
    /// Issued registration id.
    pub account_id: String,
    /// Verified email for display, not identity.
    pub email: Option<String>,
    /// ChatGPT plan permission was granted.
    pub plan_enabled: bool,
    /// The registration is selected for inference.
    pub active: bool,
}
/// Flow status and saved account picker data.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LlmAuthStatusResult {
    /// Requested flow, absent when only accounts were requested.
    pub flow: Option<LlmAuthFlow>,
    /// Saved ChatGPT registrations; never tokens.
    pub accounts: Vec<LlmAccount>,
}
/// Cancel a pending sign-in owned by this credential.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LlmAuthCancelParams {
    /// Flow identifier.
    pub flow_id: String,
}
/// Stop using saved credentials and attempt provider revocation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LlmAuthSignOutParams {
    /// Provider to disconnect.
    pub provider: ProviderKind,
    /// ChatGPT registration; absent selects the current registration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}
/// Sign-out result, explicitly distinguishing local clearing from remote revocation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LlmAuthSignOutResult {
    /// Local credentials were removed.
    pub cleared: bool,
    /// Remote revocation was confirmed; OpenRouter key deletion is managed on its account website.
    pub remote_revocation_confirmed: bool,
}

/// Canceling a flow returns its current lifecycle record.
pub type LlmAuthCancelResult = LlmAuthFlow;
