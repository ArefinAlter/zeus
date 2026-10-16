//! Explicit cloud destinations, immutable delivery plans and durable operation views.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
pub use zeus_cloud::{
    CapabilityReason, DeliveryMode, ProviderCapabilities, ProviderConfig, ProviderKind,
    RemoteArtifact,
};
use zeus_types::{ApprovalId, Hex256, HttpUrl, JobId, SecretString, Timestamp};

/// Enumerate configured account scopes; credentials and private config paths are withheld.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudAccountsListParams {}
/// Nonsecret scope established by an independently verified human terminal.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CloudAccountView {
    /// Human-selected account alias.
    pub account_id: String,
    /// Untrusted display label.
    pub label: String,
    /// Immutable configuration revision, changed by updates and revocation.
    pub revision: u64,
    /// Provider family.
    pub provider: ProviderKind,
    /// Configured remote folder, without credentials or local configuration paths.
    pub destination: String,
    /// Actual implementation capabilities.
    pub capabilities: ProviderCapabilities,
    /// Human-established per-delivery byte ceiling.
    pub max_bytes_per_delivery: u64,
    /// Account cannot authorize new or continuing transfers.
    pub revoked: bool,
}
/// Account list.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CloudAccountsListResult {
    /// Configured scopes.
    pub accounts: Vec<CloudAccountView>,
}
/// Human-terminal-only account setup. The credential goes directly to the OS keyring.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudAccountsConfigureParams {
    /// Lowercase alias, 1–64 ASCII letters, digits, underscores or hyphens.
    pub account_id: String,
    /// Display label, at most 128 characters.
    pub label: String,
    /// Explicit provider configuration; rclone never discovers ambient configuration.
    pub config: ProviderConfig,
    /// Account scope ceiling.
    pub max_bytes_per_delivery: u64,
    /// Required current revision for updates; absent only when creating an account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    /// Write-only credential. Omit to retain the existing token or for explicit rclone configuration.
    #[serde(default)]
    pub credential: Option<SecretString>,
    /// Durable request binding.
    pub idempotency_key: String,
}
impl Serialize for CloudAccountsConfigureParams {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct(
            "CloudAccountsConfigureParams",
            5 + usize::from(self.expected_revision.is_some())
                + usize::from(self.credential.is_some()),
        )?;
        s.serialize_field("account_id", &self.account_id)?;
        s.serialize_field("label", &self.label)?;
        s.serialize_field("config", &self.config)?;
        s.serialize_field("max_bytes_per_delivery", &self.max_bytes_per_delivery)?;
        if let Some(v) = self.expected_revision {
            s.serialize_field("expected_revision", &v)?;
        }
        if let Some(v) = &self.credential {
            s.serialize_field("credential", v.expose())?;
        }
        s.serialize_field("idempotency_key", &self.idempotency_key)?;
        s.end()
    }
}
/// Setup/revocation receipt.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CloudAccountMutationResult {
    /// Current account scope.
    pub account: CloudAccountView,
    /// Durable receipt was reused.
    pub replayed: bool,
}
/// Human-terminal-only revocation invalidates approvals and stops local transfer work.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudAccountsRevokeParams {
    /// Account to revoke.
    pub account_id: String,
    /// Revision the person reviewed.
    pub expected_revision: u64,
    /// Durable request binding.
    pub idempotency_key: String,
}
/// Approved source identity. Raw URLs occur only on incoming requests and private storage.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudSource {
    /// An existing completed download; Zeus never removes this user file.
    CompletedJob {
        /// Download identity.
        job_id: JobId,
    },
    /// A public HTTP(S) source with an exact bounded length and no ambient session.
    PublicUrl {
        /// Potentially sensitive URL; Debug is redacted.
        url: HttpUrl,
        /// Exact source length; mismatch fails.
        size_bytes: u64,
        /// Optional expected SHA-256.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<Hex256>,
        /// Optional exact 4 MiB block MD5 manifest for bounded Baidu streaming.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        block_md5: Option<Vec<String>>,
    },
}
/// Compute a dry-run plan. Does not fetch source bytes or mutate the remote destination.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudPlanParams {
    /// Existing configured account scope.
    pub account_id: String,
    /// Explicit mode, never substituted.
    pub mode: DeliveryMode,
    /// Source to bind.
    pub source: CloudSource,
    /// Filename only; the result shows its collision-resistant final name.
    pub remote_name: String,
    /// Maximum source/transmitted bytes for this delivery.
    pub max_bytes: u64,
    /// Maximum wall-clock operation lifetime, 1–86400 seconds.
    pub max_seconds: u32,
    /// Required opt-in for remote imports whose provider cannot enforce byte caps or cancel.
    #[serde(default)]
    pub allow_unbounded_remote: bool,
    /// Durable request binding.
    pub idempotency_key: String,
}
/// Redacted, immutable dry-run plan returned to every caller.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CloudPlanView {
    /// Prefixed plan ULID.
    pub plan_id: String,
    /// Reserved delivery ULID; retries of this plan never create a second remote object.
    pub delivery_id: String,
    /// Canonical hash bound by the human approval.
    pub plan_hash: Hex256,
    /// Account alias.
    pub account_id: String,
    /// Exact authorized account revision.
    pub account_revision: u64,
    /// Provider family.
    pub provider: ProviderKind,
    /// Explicit mode.
    pub mode: DeliveryMode,
    /// Source job identity or URL origin; all URL paths and query strings withheld.
    pub source_redacted: String,
    /// Hash of the complete private source identity.
    pub source_hash: Hex256,
    /// Configured destination root.
    pub destination: String,
    /// Final unique remote object name.
    pub remote_name: String,
    /// Exact planned bytes.
    pub size_bytes: u64,
    /// Maximum bytes authorized locally.
    pub max_bytes: u64,
    /// Maximum local operation lifetime.
    pub max_seconds: u32,
    /// Source SHA-256 when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_sha256: Option<Hex256>,
    /// The requested mode is available with these exact constraints.
    pub supported: bool,
    /// Machine-readable unavailable capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsupported_reason: Option<CapabilityReason>,
    /// Explicitly accepted weaker remote guarantees.
    pub allow_unbounded_remote: bool,
    /// True only when the provider can enforce cancellation.
    pub remote_cancellation_supported: bool,
    /// True only when Zeus can enforce the transmitted byte ceiling.
    pub hard_byte_cap: bool,
    /// Deadline to submit and approve this plan.
    pub expires_at: Timestamp,
}
/// Dry-run receipt.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CloudPlanResult {
    /// Immutable reviewed plan.
    pub plan: CloudPlanView,
    /// Durable receipt was reused.
    pub replayed: bool,
}
/// Reserve a durable delivery and request human approval; never self-authorizes.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudSubmitParams {
    /// Reviewed plan identifier.
    pub plan_id: String,
    /// Reviewed canonical hash.
    pub plan_hash: Hex256,
    /// Durable request binding.
    pub idempotency_key: String,
}
/// Durable delivery lifecycle, including ambiguous remote operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CloudDeliveryState {
    /// No remote work until an independently attested human decides.
    AwaitingApproval,
    /// Authorized and ready to start.
    Queued,
    /// Local transfer is active.
    Running,
    /// Provider accepted an asynchronous import; not completed.
    RemotePending,
    /// Paused; resuming still requires valid scope and approval.
    Paused,
    /// A private checkpoint needs remote reconciliation before any retry.
    Reconciling,
    /// Provider metadata was verified at the evidence level in artifact.
    Completed,
    /// Failed without claiming remote completion.
    Failed,
    /// Local work stopped and no continuing remote import was accepted.
    Canceled,
    /// Remote provider may continue despite local cancellation; checkpoint retained.
    RemoteCancellationUnavailable,
}
/// Public delivery status. Upload URLs, source query strings and credentials are never included.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CloudDeliveryView {
    /// Durable identity.
    pub delivery_id: String,
    /// Exact immutable plan.
    pub plan: CloudPlanView,
    /// Current state.
    pub state: CloudDeliveryState,
    /// Human approval; absent only during recoverable approval creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<ApprovalId>,
    /// Confirmed provider bytes.
    pub committed_bytes: u64,
    /// Actual remote completion evidence, never inferred from a 202.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<RemoteArtifact>,
    /// Sanitized stable reason code, no provider response text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Zeus-owned staging cleanup must be retried; never refers to a user download file.
    #[serde(default)]
    pub cleanup_pending: bool,
    /// Creation time.
    pub created_at: Timestamp,
    /// Last durable change.
    pub updated_at: Timestamp,
}
/// Mutation receipt.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CloudMutationResult {
    /// Durable delivery view.
    pub delivery: CloudDeliveryView,
    /// Durable receipt was reused.
    pub replayed: bool,
}
/// Read a caller-owned delivery; human clients may inspect all.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudGetParams {
    /// Delivery identifier.
    pub delivery_id: String,
}
/// List visible deliveries, newest first, with a bounded result.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudListParams {
    /// Result limit, 1–100; default 50.
    #[serde(default = "default_limit")]
    pub limit: u32,
}
fn default_limit() -> u32 {
    50
}
impl Default for CloudListParams {
    fn default() -> Self {
        Self {
            limit: default_limit(),
        }
    }
}
/// Bounded delivery list.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CloudListResult {
    /// Visible deliveries.
    pub deliveries: Vec<CloudDeliveryView>,
}
/// Local control; cannot manufacture or extend human approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CloudControlAction {
    /// Stop local reads and keep the checkpoint.
    Pause,
    /// Resume only with the same still-authorized immutable plan.
    Resume,
    /// Request cancellation, preserving remote uncertainty.
    Cancel,
}
/// Idempotent control request.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudControlParams {
    /// Delivery identity.
    pub delivery_id: String,
    /// Requested action.
    pub action: CloudControlAction,
    /// Durable request binding.
    pub idempotency_key: String,
}
