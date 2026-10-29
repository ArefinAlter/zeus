//! Explicit immutable source review, configuration and identity-bound dispatch.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
pub use zeus_sources::{ObjectMetadata, SourceAccountConfig, SourceBudget, SourceTarget};
use zeus_types::{JobId, SecretString, Timestamp};

/// Nonsecret configured acquisition scope. Changes revoke prior operation authority.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceAccountView {
    /// Human-selected alias, bounded to letters/digits/dash/underscore.
    pub account_id: String,
    /// Untrusted display name.
    pub untrusted_label: String,
    /// Exact provider scope; never private credential/keyring references.
    pub config: SourceAccountConfig,
    /// Monotonic revision required by plans and in-flight transfers.
    pub revision: u64,
    /// Credentials explicitly allowed to use this source, configured only by a human.
    pub allowed_credential_ids: Vec<String>,
    /// Maximum per-operation bytes and elapsed time.
    pub budget_ceiling: SourceBudget,
    /// Credential material is configured, without exposing its value or reference.
    pub credential_configured: bool,
    /// Explicit local revocation stops reads, including commit-time validation.
    pub revoked: bool,
}

/// Human-terminal account setup; this grants only the declared immutable source scope.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcesAccountConfigureParams {
    /// Alias to configure.
    pub account_id: String,
    /// Untrusted human label.
    pub untrusted_label: String,
    /// Native source and exact resource scope.
    pub config: SourceAccountConfig,
    /// Authenticated credentials allowed to use it; no caller-side owner override.
    pub allowed_credential_ids: Vec<String>,
    /// Reviewed cumulative receive/time ceiling.
    pub budget_ceiling: SourceBudget,
    /// Reviewed revision when replacing an existing account.
    pub expected_revision: Option<u64>,
    /// Write-only explicit token or S3 credential JSON; never discover another application's secrets.
    pub credential: Option<SecretString>,
    /// Exact request retry key; bound without persisting this secret payload.
    pub idempotency_key: String,
}
impl Serialize for SourcesAccountConfigureParams {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            account_id: &'a str,
            untrusted_label: &'a str,
            config: &'a SourceAccountConfig,
            allowed_credential_ids: &'a [String],
            budget_ceiling: SourceBudget,
            expected_revision: Option<u64>,
            credential: Option<&'a str>,
            idempotency_key: &'a str,
        }
        Wire {
            account_id: &self.account_id,
            untrusted_label: &self.untrusted_label,
            config: &self.config,
            allowed_credential_ids: &self.allowed_credential_ids,
            budget_ceiling: self.budget_ceiling,
            expected_revision: self.expected_revision,
            credential: self.credential.as_ref().map(SecretString::expose),
            idempotency_key: &self.idempotency_key,
        }
        .serialize(serializer)
    }
}

/// Only human terminal configuration/revocation may change credential scope.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcesAccountRevokeParams {
    /// Configured alias.
    pub account_id: String,
    /// Exact reviewed revision.
    pub expected_revision: u64,
    /// Request retry binding.
    pub idempotency_key: String,
}
/// Accounts visible to this authenticated requester.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SourcesAccountsResult {
    /// Exact public scopes and live revisions.
    pub accounts: Vec<SourceAccountView>,
}
/// Configuration/revocation receipt.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SourcesAccountMutationResult {
    /// Current nonsecret account scope.
    pub account: SourceAccountView,
    /// Exact prior request replayed.
    pub replayed: bool,
}

/// Inspect one immutable source under an existing human-configured scope.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcesInspectParams {
    /// Configured source alias.
    pub account_id: String,
    /// Revision reviewed by the requester.
    pub account_revision: u64,
    /// Exact immutable object or selected torrent files.
    pub target: SourceTarget,
    /// One whole task's receive/time ceiling, within the account's grant.
    pub budget: SourceBudget,
    /// Private base64 .torrent bytes when reviewing a new metainfo digest.
    /// This is never an arbitrary file path or magnet parser with hidden discovery.
    pub metainfo_base64: Option<SecretString>,
    /// Exact inspect retry key.
    pub idempotency_key: String,
}
impl Serialize for SourcesInspectParams {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            account_id: &'a str,
            account_revision: u64,
            target: &'a SourceTarget,
            budget: SourceBudget,
            metainfo_base64: Option<&'a str>,
            idempotency_key: &'a str,
        }
        Wire {
            account_id: &self.account_id,
            account_revision: self.account_revision,
            target: &self.target,
            budget: self.budget,
            metainfo_base64: self.metainfo_base64.as_ref().map(SecretString::expose),
            idempotency_key: &self.idempotency_key,
        }
        .serialize(serializer)
    }
}
/// A validated, immutable review. Remote-derived strings remain explicitly untrusted.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SourcePlanView {
    /// Stable prefixed ULID; not a secret or approval token.
    pub plan_id: String,
    /// Account/revision whose authority remains required.
    pub account_id: String,
    /// Exact account configuration revision.
    pub account_revision: u64,
    /// Untrusted source path/name for review, without bearer addresses or tracker credentials.
    pub untrusted_label: String,
    /// Immutable provider bytes/revision and digest semantics.
    pub metadata: ObjectMetadata,
    /// Explicit cumulative receive/time ceiling.
    pub budget: SourceBudget,
    /// Last moment at which this unchanged review may be submitted.
    pub expires_at: Timestamp,
    /// Honest networking and capability limits, including torrent control traffic.
    pub notices: Vec<String>,
    /// Bounded metainfo file table for selecting exact indices; empty for HTTP sources.
    #[serde(default)]
    pub torrent_files: Vec<SourceTorrentFileView>,
}
/// Submit a reviewed source to a configured destination without credential/header input.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcesSubmitParams {
    /// Exact review owned by this authenticated requester.
    pub plan_id: String,
    /// Destination within current write roots and the requester's service scope.
    pub dest_dir: Option<String>,
    /// Safe final filename; multi-file torrents require their dedicated selected-file mapping.
    pub filename: String,
    /// One through thirty-two engine range connections, independently bounded by scheduler limits.
    pub max_connections: u8,
    /// Exact final leaf names by selected torrent file index; required for multiple files.
    #[serde(default)]
    pub torrent_filenames: std::collections::BTreeMap<usize, String>,
    /// Durable exact-submit retry key.
    pub idempotency_key: String,
}
/// Redacted identity-bound source progress and final lineage.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SourceOperationView {
    /// Durable prefixed ULID.
    pub operation_id: String,
    /// Immutable source review.
    pub plan_id: String,
    /// Configured source account.
    pub account_id: String,
    /// Exact account revision retained for all request/commit checks.
    pub account_revision: u64,
    /// Engine download identity, bound durably before scheduling HTTP work.
    pub job_id: Option<JobId>,
    /// queued, running, paused, completed, failed or canceled.
    pub state: String,
    /// Untrusted source name, never signed URLs or tracker credentials.
    pub untrusted_label: String,
    /// Cumulative network body bytes reserved durably, including retry bytes.
    pub received_bytes: u64,
    /// Torrent piece payload uploaded; source adapters never auto-seed.
    pub payload_uploaded_bytes: u64,
    /// Reviewed operation limits.
    pub budget: SourceBudget,
    /// Exact provider version/commit and integrity semantics.
    pub metadata: ObjectMetadata,
    /// Local final output SHA-256, only after verified publication.
    pub output_sha256: Option<String>,
    /// Verified final torrent files; empty until publication succeeds.
    #[serde(default)]
    pub files: Vec<SourcePublishedFile>,
    /// Fixed safe failure code without upstream diagnostics.
    pub error_code: Option<String>,
}
/// Own operations only; no mutation accepts an arbitrary owner selector.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SourcesListResult {
    /// Bounded durable history, at most one hundred entries.
    pub operations: Vec<SourceOperationView>,
}
/// Inspect one source operation belonging to this requester.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcesGetParams {
    /// Stable source identity.
    pub operation_id: String,
}
/// Explicit lifecycle action. Resume never broadens an expired or revoked budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceControlAction {
    /// Cooperatively pause and preserve verified bytes.
    Pause,
    /// Explicitly resume within the original deadline/account authority.
    Resume,
    /// Stop and cancel this source's engine job/peer operation.
    Cancel,
}
/// Owner-bound lifecycle control with durable retry semantics.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcesControlParams {
    /// Existing operation identity.
    pub operation_id: String,
    /// Requested lifecycle action.
    pub action: SourceControlAction,
    /// Exact control retry key.
    pub idempotency_key: String,
}

/// Empty authenticated source query.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcesReadParams {}

/// One verified torrent output, with no source tracker or credential data.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SourcePublishedFile {
    /// Selected metainfo file index.
    pub index: usize,
    /// Reviewed destination leaf name.
    pub filename: String,
    /// Verified file length.
    pub bytes: u64,
    /// SHA-256 of published bytes.
    pub sha256: String,
}

/// Untrusted metainfo file metadata, without trackers or credential-bearing URLs.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SourceTorrentFileView {
    /// Stable metainfo file index.
    pub index: usize,
    /// Validated relative path components, displayed as untrusted data.
    pub untrusted_path: Vec<String>,
    /// Declared file length.
    pub bytes: u64,
}
