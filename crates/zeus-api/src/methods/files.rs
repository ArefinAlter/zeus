//! `files.*`: listing and (later) organizing files under the write roots.
//!
//! Every mutation defaults to `dry_run: true`; deleting or overwriting needs an approval of kind `destructive_op`
//! (01 section 5.3) and moves files to the application trash with an undo journal, never straight to unlink.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeus_types::{ApprovalId, Hex256, JobId, RedactedUrl, Timestamp};

use crate::methods::admin::ServiceRoot;
use crate::paging::{Page, PageRequest};

/// `files.list` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilesListParams {
    /// Paging: `limit` and `cursor` sit beside the filters (00 section 10).
    #[serde(flatten)]
    pub page: PageRequest,
    /// A directory inside a write root. Absent: the default root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    /// Recurse into sub-directories.
    #[serde(default)]
    pub recursive: bool,
    /// Include `.zpart` files of unfinished jobs.
    #[serde(default)]
    pub include_partial: bool,
}

/// One file with its provenance. Names are untrusted data (01 section 12.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileEntry {
    /// The write root containing the file (absolute, canonical).
    pub root: String,
    /// Path relative to the write root.
    pub path: String,
    /// Size in bytes.
    pub size_bytes: u64,
    /// Last modification.
    pub modified_at: Timestamp,
    /// SHA-256 recorded when the file was completed by Zeus, if it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<Hex256>,
    /// Zeus's quarantine flag.
    pub quarantined: bool,
    /// The job that produced the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<JobId>,
    /// Where it came from, redacted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url_redacted: Option<RedactedUrl>,
    /// True for the `.zpart` file of an unfinished job.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub partial: bool,
}

/// `files.list` result.
pub type FilesListResult = Page<FileEntry>;

/// `files.roots.register` parameters (role `service` only; 05 section 9.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilesRootsRegisterParams {
    /// Absolute directory inside one of the user's write roots.
    pub path: String,
    /// Optional label, 1 to 64 characters. Untrusted display text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Idempotency key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `files.roots.register` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilesRootsRegisterResult {
    /// True when this response replays a durable mutation receipt.
    #[serde(default)]
    pub replayed: bool,
    /// The registered root.
    pub root: ServiceRoot,
    /// False when the same canonical path was already registered by this service.
    pub created: bool,
}

/// One step of a file plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileStep {
    /// `move`, `rename`, `convert` or `trash`.
    pub op: String,
    /// Source path relative to the root.
    pub from: String,
    /// Destination path relative to the root, for non-trash operations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Whether this step deletes or overwrites (needs approval to apply).
    pub destructive: bool,
}

/// `files.organize` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilesOrganizeParams {
    /// Directory inside a write root.
    pub dir: String,
    /// Rules, either a rule set or a natural-language instruction handled by the LLM ladder. Untrusted.
    pub rules: serde_json::Value,
    /// Plan only. **Default true.**
    #[serde(default = "yes")]
    pub dry_run: bool,
    /// Idempotency key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

fn yes() -> bool {
    true
}

/// `files.organize` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilesOrganizeResult {
    /// True when this response replays a durable mutation receipt.
    #[serde(default)]
    pub replayed: bool,
    /// The plan.
    pub steps: Vec<FileStep>,
    /// Whether the plan was applied.
    pub applied: bool,
    /// The `destructive_op` approval a person must give before anything moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<ApprovalId>,
    /// The stored operation; `files.undo` reverts it once it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

/// `files.convert` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilesConvertParams {
    /// Inputs, relative to a write root.
    pub inputs: Vec<String>,
    /// Target extension without a dot.
    pub target: String,
    /// Plan only. **Default true.**
    #[serde(default = "yes")]
    pub dry_run: bool,
    /// Idempotency key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `files.convert` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilesConvertResult {
    /// True when this response replays a durable mutation receipt.
    #[serde(default)]
    pub replayed: bool,
    /// Planned or performed conversions.
    pub steps: Vec<FileStep>,
    /// Converter name and version used, when applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Whether anything was done.
    pub applied: bool,
}

/// `files.undo` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilesUndoParams {
    /// Identifier of the operation in the undo journal.
    pub operation_id: String,
    /// Idempotency key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `files.undo` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilesUndoResult {
    /// True when this response replays a durable mutation receipt.
    #[serde(default)]
    pub replayed: bool,
    /// The inverse steps, planned now and run after a person approves them.
    pub steps: Vec<FileStep>,
    /// The `destructive_op` approval for the undo (absent when there is nothing to revert).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<ApprovalId>,
    /// The undo's own operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutations_default_to_dry_run() {
        let organize: FilesOrganizeParams =
            serde_json::from_value(serde_json::json!({"dir": "x", "rules": {}})).unwrap();
        assert!(organize.dry_run);
        let convert: FilesConvertParams =
            serde_json::from_value(serde_json::json!({"inputs": ["a.docx"], "target": "pdf"}))
                .unwrap();
        assert!(convert.dry_run);
    }
}
