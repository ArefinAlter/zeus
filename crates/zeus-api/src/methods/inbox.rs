//! Durable owner-scoped intake packages. Stored source requests stay private; public projections
//! contain redacted URLs and daemon-derived destination/approval facts. Intake never grants access.

use crate::{
    ApiError,
    methods::jobs::{EnqueuedJob, NewJob},
    paging::{Page, PageRequest},
};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::{borrow::Cow, fmt, str::FromStr};
use zeus_types::{JobId, RedactedUrl, Timestamp};

macro_rules! identifier {
    ($name:ident,$prefix:literal,$description:literal)=>{
        #[doc=$description]
        #[derive(Debug,Clone,PartialEq,Eq,PartialOrd,Ord,Hash)]
        pub struct $name(String);
        impl $name {
            /// Wrap a daemon-minted monotonic ULID under this intake identifier's namespace.
            pub fn from_job_id(source:JobId)->Self{Self(format!(concat!($prefix,"_{}"),source.ulid()))}
            /// The validated wire identifier.
            pub fn as_str(&self)->&str{&self.0}
        }
        impl fmt::Display for $name{fn fmt(&self,f:&mut fmt::Formatter<'_>)->fmt::Result{f.write_str(&self.0)}}
        impl FromStr for $name {
            type Err=ApiError;
            fn from_str(value:&str)->Result<Self,Self::Err>{
                let invalid=||ApiError::invalid_params("invalid intake identifier");
                let body=value.strip_prefix(concat!($prefix,"_")).ok_or_else(invalid)?;
                if body.len()!=26{return Err(invalid());}
                format!("job_{body}").parse::<JobId>().map_err(|_|invalid())?;
                Ok(Self(value.to_owned()))
            }
        }
        impl Serialize for $name{fn serialize<S:Serializer>(&self,serializer:S)->Result<S::Ok,S::Error>{serializer.serialize_str(self.as_str())}}
        impl<'de> Deserialize<'de> for $name{fn deserialize<D:Deserializer<'de>>(deserializer:D)->Result<Self,D::Error>{let value=String::deserialize(deserializer)?;value.parse().map_err(de::Error::custom)}}
        impl JsonSchema for $name{
            fn inline_schema()->bool{true}
            fn schema_name()->Cow<'static,str>{stringify!($name).into()}
            fn json_schema(_: &mut SchemaGenerator)->Schema{json_schema!({"type":"string","pattern":concat!("^",$prefix,"_[0-7][0-9A-HJKMNP-TV-Z]{25}$"),"description":$description})}
        }
    }
}
identifier!(
    InboxPackageId,
    "pkg",
    "One durable intake package (`pkg_<ULID>`)."
);
identifier!(
    InboxEntryId,
    "inb",
    "One private intake request (`inb_<ULID>`)."
);
identifier!(
    InboxPreviewId,
    "ipv",
    "One immutable reviewed intake batch (`ipv_<ULID>`)."
);
identifier!(
    InboxBatchId,
    "iba",
    "One durable enqueue-dispatch receipt (`iba_<ULID>`)."
);

/// One bounded first-match package rule. It changes reviewed defaults, never execution authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxRule {
    /// Stable local identifier,1–64 ASCII letters, digits, underscores or hyphens.
    pub id: String,
    /// Exact normalized HTTP(S) origin; no credentials, path, query or wildcard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<zeus_types::Origin>,
    /// Case-insensitive suffix of the requested filename, such as `.pdf` or `.tar.gz`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename_suffix: Option<String>,
    /// Default destination for matching entries without an explicit destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_dir: Option<String>,
    /// ASCII filename prefix (letters, digits, `_` or `-`), applied before safe-name planning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename_prefix: Option<String>,
}

/// Public package metadata, derived for the authenticated owner only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InboxPackageView {
    /// Daemon-minted package identity.
    pub id: InboxPackageId,
    /// Revision required by every edit and preview.
    pub revision: u64,
    /// Person/agent-provided display text; never executable instructions.
    pub untrusted_name: String,
    /// Suggested destination; actual scope and collision decisions belong to preview/enqueue.
    pub default_dest_dir: Option<String>,
    /// Ordered first-match rules, at most32. Empty in older saved packages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<InboxRule>,
    /// Archived packages retain their receipts and do not cancel existing downloads.
    pub archived: bool,
    /// Still awaiting a successful enqueue.
    pub pending_entries: u32,
    /// Accepted download references retained in this package.
    pub enqueued_entries: u32,
    /// Any unresolved dispatch prevents editing until the same batch is reconciled/retried.
    pub active_batch: Option<InboxBatchId>,
    /// Creation time, UTC with three fractional digits.
    pub created_at: Timestamp,
    /// Last successful metadata/receipt change.
    pub updated_at: Timestamp,
}
/// One public entry; raw URLs, hint URLs, referrers and headers never appear here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InboxEntryView {
    /// Entry identity.
    pub id: InboxEntryId,
    /// URL with userinfo/query/fragment redacted.
    pub redacted_url: RedactedUrl,
    /// Optional caller-provided description.
    pub untrusted_label: Option<String>,
    /// Caller-provided filename hint, not a destination guarantee.
    pub untrusted_filename: Option<String>,
    /// Requested or package-default destination, subject to current roots at preview.
    pub dest_dir: Option<String>,
    /// Download reference only after a durable successful enqueue.
    pub job_id: Option<JobId>,
    /// Last bounded enqueue rejection; no raw source request is included.
    pub error: Option<zeus_types::ErrorEnvelope>,
    /// Time the private request was retained.
    pub created_at: Timestamp,
}
/// Create or update one owned package. Updates require the current revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxPackagePutParams {
    /// Absent creates a daemon-minted package; present edits an existing owned package.
    #[serde(default)]
    pub package_id: Option<InboxPackageId>,
    /// Required for update, absent for create.
    #[serde(default)]
    pub expected_revision: Option<u64>,
    /// Untrusted display name, bounded to160 UTF-8 bytes.
    pub untrusted_name: String,
    /// Optional destination default inside engine write roots at preview/dispatch time.
    #[serde(default)]
    pub default_dest_dir: Option<String>,
    /// Replace ordered rules (at most32). Omission preserves existing rules; [] explicitly clears.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<InboxRule>>,
    /// Create only: atomically retain1–200 private requests with the package (512KiB total call).
    /// Omission creates an empty package. Updates must omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_entries: Option<Vec<InboxIntakeItem>>,
    /// Binds this entire mutation to one owner-scoped retry key.
    pub idempotency_key: String,
}
/// A durable metadata mutation result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InboxMutationResult {
    /// Current result at the time this receipt was first committed.
    pub package: InboxPackageView,
    /// The response replays an identical mutation receipt.
    pub replayed: bool,
}
/// List only the authenticated owner's packages.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InboxPackagesListParams {
    /// Explicit read-only inspection of another credential’s redacted intake; human-capable callers only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_credential_id: Option<String>,
    /// Owner/filter-bound cursor and at most100 metadata records.
    #[serde(flatten)]
    pub page: PageRequest,
    /// Include explicitly archived metadata.
    #[serde(default)]
    pub include_archived: bool,
}
/// Public package page.
pub type InboxPackagesListResult = Page<InboxPackageView>;
/// Read one owned package and one bounded page of its entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InboxPackageGetParams {
    /// Include bounded retained redacted previews/dispatches explicitly; ordinary reads default to metadata/entries only.
    #[serde(default)]
    pub include_history: bool,
    /// Explicit read-only inspection of another credential’s redacted intake; human-capable callers only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_credential_id: Option<String>,
    /// Existing owned package.
    pub package_id: InboxPackageId,
    /// Owner/package/filter/revision-bound entry cursor.
    #[serde(flatten)]
    pub page: PageRequest,
    /// Omit previously accepted entries when true.
    #[serde(default)]
    pub pending_only: bool,
}
/// Metadata and redacted entry page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InboxPackageGetResult {
    /// Package metadata.
    pub package: InboxPackageView,
    /// Bounded redacted entry views.
    pub entries: Page<InboxEntryView>,
    /// At most16 retained redacted review handles, newest first.
    pub previews: Vec<InboxPreviewResult>,
    /// At most20 most recent dispatch projections; no original requests or keys.
    pub submissions: Vec<InboxSubmissionView>,
}
/// One input record. The `NewJob` is persisted privately and revalidated before any execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxIntakeItem {
    /// Requested HTTP download; source identity always comes from authenticated dispatch context.
    pub job: NewJob,
    /// Optional untrusted label, at most500 UTF-8 bytes.
    #[serde(default)]
    pub untrusted_label: Option<String>,
}
/// Add1–200 private requests with exact deduplication inside an owned package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxEntriesAddParams {
    /// Target owned package.
    pub package_id: InboxPackageId,
    /// Current revision; stale metadata is refused.
    pub expected_revision: u64,
    /// Exact request records, at most200 and512KiB serialized per call.
    pub entries: Vec<InboxIntakeItem>,
    /// Exact duplicate additions and retries return the original receipt.
    pub idempotency_key: String,
}
/// Accepted metadata intake, without publishing raw URLs back to clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InboxEntriesAddResult {
    /// Package after the first successful addition.
    pub package: InboxPackageView,
    /// Newly retained identifiers, in input order.
    pub added_ids: Vec<InboxEntryId>,
    /// Existing identifiers for byte-equivalent typed intake records.
    pub duplicate_ids: Vec<InboxEntryId>,
    /// Identical key/payload replay.
    pub replayed: bool,
}
/// Remove inbox metadata only; associated downloads are never cancelled or deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxEntriesRemoveParams {
    /// Owned package.
    pub package_id: InboxPackageId,
    /// Current revision.
    pub expected_revision: u64,
    /// Existing owned entry identifiers, at most500.
    pub entry_ids: Vec<InboxEntryId>,
    /// Durable retry binding.
    pub idempotency_key: String,
}
/// Archive/unarchive an owned package's metadata, preserving jobs and receipts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxPackageArchiveParams {
    /// Owned package.
    pub package_id: InboxPackageId,
    /// Current revision.
    pub expected_revision: u64,
    /// Desired visibility in the ordinary package list.
    pub archived: bool,
    /// Durable retry binding.
    pub idempotency_key: String,
}
/// Preview at most50 still-pending requests using current daemon destination and approval facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxPreviewParams {
    /// Owned package.
    pub package_id: InboxPackageId,
    /// Exact package revision being reviewed.
    pub expected_revision: u64,
    /// Explicit pending records, or the next at most50 pending records when empty.
    #[serde(default)]
    pub entry_ids: Vec<InboxEntryId>,
    /// Preview creation is retained so a retry cannot silently change reviewed facts.
    pub idempotency_key: String,
}
/// One daemon-derived preview row; remote/caller content remains untrusted and redacted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InboxPreviewEntry {
    /// Private request reference.
    pub entry_id: InboxEntryId,
    /// Redacted resource address.
    pub redacted_url: RedactedUrl,
    /// Reviewed filename after safe naming/collision planning, if valid.
    pub filename: Option<String>,
    /// First matching rule, if any; bound to the exact reviewed package revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_rule: Option<String>,
    /// Reviewed destination path, never an authority grant.
    pub dest_path: Option<String>,
    /// Enqueue would wait for a separate human approval.
    pub approval_needed: bool,
    /// Neutral daemon warning codes/facts.
    pub warnings: Vec<String>,
    /// Invalid source/destination/permission facts for this row.
    pub error: Option<zeus_types::ErrorEnvelope>,
}
/// Immutable reviewed batch handle; no session material or approval decision is returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InboxPreviewResult {
    /// Owner-bound immutable preview reference.
    pub preview_id: InboxPreviewId,
    /// Owned package.
    pub package_id: InboxPackageId,
    /// Exact metadata revision.
    pub package_revision: u64,
    /// Fresh submissions must start before this UTC instant.
    pub expires_at: Timestamp,
    /// At most50 destination/approval projections.
    pub entries: Vec<InboxPreviewEntry>,
    /// Rows eligible for the reviewed enqueue batch.
    pub ready_entries: u32,
    /// Pending requests outside this preview.
    pub remaining_pending: u32,
    /// Identical preview-key replay.
    pub replayed: bool,
}
/// Dispatch one reviewed immutable batch with restart-safe request binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxSubmitParams {
    /// Owned package.
    pub package_id: InboxPackageId,
    /// Revision bound into the saved preview.
    pub expected_revision: u64,
    /// Exact preview handle; caller cannot replace its hidden enqueue payload.
    pub preview_id: InboxPreviewId,
    /// Durable owner-scoped dispatch key.
    pub idempotency_key: String,
}
/// One entry's existing enqueue response, in reviewed order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InboxSubmissionEntry {
    /// Intake reference.
    pub entry_id: InboxEntryId,
    /// Existing daemon enqueue contract; approvals remain undecided.
    pub result: EnqueuedJob,
}
/// Durable completion of one bounded batch, including per-entry rejection facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InboxSubmitResult {
    /// Stable dispatch identity for recovery.
    pub batch_id: InboxBatchId,
    /// Owned package.
    pub package_id: InboxPackageId,
    /// Package after accepted jobs/rejections were recorded.
    pub package_revision: u64,
    /// At most50 per-entry results.
    pub entries: Vec<InboxSubmissionEntry>,
    /// Still-pending metadata records, including rejected rows.
    pub remaining_pending: u32,
    /// Exact dispatch result was already committed.
    pub replayed: bool,
}
/// Public dispatch state; a pending batch is retried with its original private payload/key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InboxDispatchState {
    /// Durable intent exists; dispatch/completion has not been reconciled.
    Pending,
    /// A full immutable result is retained.
    Complete,
}
/// Inspect an owned dispatch without exposing its hidden request or idempotency key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxSubmissionGetParams {
    /// Explicit read-only inspection of another credential’s redacted intake; human-capable callers only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_credential_id: Option<String>,
    /// Owned package.
    pub package_id: InboxPackageId,
    /// Dispatch reference.
    pub batch_id: InboxBatchId,
}
/// Recoverable owner-scoped dispatch projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InboxSubmissionView {
    /// Dispatch reference.
    pub batch_id: InboxBatchId,
    /// Owned package.
    pub package_id: InboxPackageId,
    /// Pending or complete; uncertainty is never treated as a new batch.
    pub state: InboxDispatchState,
    /// Reviewed preview reference for diagnostics, not a raw source request.
    pub preview_id: InboxPreviewId,
    /// Completed response, when durable.
    pub result: Option<InboxSubmitResult>,
    /// Most recent bounded API error code; retry never changes the private request.
    pub error_code: Option<String>,
}
/// Explicitly retry/reconcile an already-owned pending batch using its saved request/key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxSubmissionRetryParams {
    /// Owned package.
    pub package_id: InboxPackageId,
    /// Existing dispatch; the caller cannot supply different URLs or destinations.
    pub batch_id: InboxBatchId,
    /// Retry operation binding; underlying enqueue keeps its original durable key.
    pub idempotency_key: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifiers_are_typed_prefixed_ulids() {
        let id = InboxPackageId::from_job_id(zeus_types::IdGenerator::new().job_id());
        assert_eq!(id.as_str().parse::<InboxPackageId>().unwrap(), id);
        assert!(id.as_str().parse::<InboxEntryId>().is_err());
        for bad in [
            "pkg_invalid",
            "pkg_8ZZZZZZZZZZZZZZZZZZZZZZZZZ",
            "pkg_01invalid000000000000000000",
        ] {
            assert!(bad.parse::<InboxPackageId>().is_err());
        }
    }
}
