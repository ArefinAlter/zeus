//! Diagnostics, the dry-run report, the run report and checkpoints.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeus_types::{ApprovalId, ErrorEnvelope, GrantId, RunId, Timestamp};

use crate::planner::Estimate;

/// How serious a [`Diagnostic`] is.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// For information only.
    Info,
    /// The plan can run but the user should look at this.
    Warning,
    /// The plan cannot be approved or run until this is fixed.
    Error,
}

/// A finding about a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Diagnostic {
    /// How serious.
    pub severity: Severity,
    /// Stable machine code, `plan.<reason>`.
    pub code: String,
    /// JSON Pointer to the offending part of the plan.
    pub path: String,
    /// English explanation. Contains no page content.
    pub message: String,
}

impl Diagnostic {
    /// An error finding.
    pub fn error(code: &str, path: &str, message: impl Into<String>) -> Self {
        Self::new(Severity::Error, code, path, message)
    }

    /// A warning finding.
    pub fn warning(code: &str, path: &str, message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, code, path, message)
    }

    /// An informational finding.
    pub fn info(code: &str, path: &str, message: impl Into<String>) -> Self {
        Self::new(Severity::Info, code, path, message)
    }

    fn new(severity: Severity, code: &str, path: &str, message: impl Into<String>) -> Self {
        Self {
            severity,
            code: code.to_string(),
            path: path.to_string(),
            message: message.into(),
        }
    }
}

/// One item the dry run would download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PlannedItem {
    /// Position in discovery order.
    pub index: u32,
    /// Download URL after redaction (query secrets removed).
    pub url_redacted: String,
    /// Final file name after the rename template and sanitising.
    pub file_name: String,
    /// Declared size when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// Index of an earlier item that gets the same final name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collides_with: Option<u32>,
}

/// Result of `workflows.dry_run`: what would happen, without downloading anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DryRunReport {
    /// Whether sampling finished within the deadline; incomplete reports cannot authorize a run.
    #[serde(default)]
    pub complete: bool,
    /// Hash of the plan this report is about.
    pub plan_hash: String,
    /// When the dry run happened.
    pub generated_at: Timestamp,
    /// Pages actually fetched for the sample.
    pub sampled_pages: u32,
    /// The first N items.
    pub sample: Vec<PlannedItem>,
    /// Number of sampled items whose name collides with another.
    pub name_collisions: u32,
    /// Estimated number of files, with an uncertainty band.
    pub estimate_files: Option<Estimate>,
    /// Estimated total bytes, with an uncertainty band.
    pub estimate_bytes: Option<Estimate>,
    /// Estimated duration in milliseconds at the plan's politeness budget.
    pub estimate_duration_ms: Option<Estimate>,
    /// Warnings: robots.txt findings, terms-of-service reminders, drift.
    pub warnings: Vec<Diagnostic>,
}

/// Overall state of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Waiting for the plan approval.
    AwaitingApproval,
    /// Running.
    Running,
    /// Paused by a client.
    Paused,
    /// Finished normally.
    Completed,
    /// A budget ran out.
    BudgetExhausted,
    /// A stop rule ended pagination.
    Stopped,
    /// Ended with an error.
    Failed,
    /// Canceled by a client.
    Canceled,
}

/// Progress and outcome of a run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunCounts {
    /// Successful document fetches committed to the checkpoint.
    pub pages_fetched: u32,
    /// Failed document attempts committed before pausing.
    pub pages_failed: u32,
    /// Document attempts refused by robots policy.
    pub pages_skipped_robots: u32,
    /// Extracted candidate items.
    pub items_seen: u64,
    /// Repeated URLs or dedupe keys.
    pub items_duplicate: u64,
    /// Items rejected by ordinary filters.
    pub items_filtered: u64,
    /// Items outside the approved domains.
    pub items_out_of_domain: u64,
    /// Items with missing or invalid required fields.
    pub items_invalid: u64,
    /// Durable intents skipped because their destination already exists.
    pub items_skipped_existing: u64,
    /// Jobs admitted to the engine; pending intents are excluded in API projections.
    pub files_enqueued: u32,
    /// Jobs currently completed.
    pub files_completed: u32,
    /// Jobs currently failed.
    pub files_failed: u32,
    /// Jobs currently canceled.
    pub files_canceled: u32,
    /// Jobs currently marked quarantined.
    pub files_quarantined: u32,
    /// Successful conversions; zero while conversion is unsupported.
    pub files_converted: u32,
    /// Failed conversions; zero while conversion is unsupported.
    pub files_convert_failed: u32,
    /// Committed bytes across admitted jobs.
    pub bytes_done: u64,
}

/// Hard bound for one requested item export, separate from downloaded file bytes.
pub const ITEM_SINK_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Bound for redacted item metadata reserved by one atomic discovery document.
pub const ITEM_METADATA_MAX_BYTES: usize = 8 * 1024 * 1024;
/// Progress of an explicitly requested item export. Extracted field values are never returned here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ItemSinkReport {
    /// File inside the run's reviewed destination directory.
    pub file_name: String,
    /// Durably appended JSONL records.
    pub records: u64,
    /// Durably appended bytes.
    pub bytes: u64,
    /// Resolved outcomes waiting for an append receipt.
    pub pending_records: u32,
    /// Maximum export bytes for this version.
    pub max_bytes: u64,
}

/// Progress and outcome of a run, including compatibility fields for pre-alpha clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunReport {
    /// One for reports whose discovery counters were recorded from run creation; zero for legacy checkpoints.
    #[serde(default)]
    pub report_version: u8,
    /// Stored plan identity when known.
    #[serde(default)]
    pub plan_id: Option<crate::PlanId>,
    /// Approval that authorized this run.
    #[serde(default)]
    pub approval_id: Option<ApprovalId>,
    /// Effective workflow session grant; null for public-page runs.
    #[serde(default)]
    pub grant_id: Option<GrantId>,
    /// The reviewed browser document mode; its workflow approval must be decided in that browser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_document_mode: Option<crate::SessionDocumentMode>,
    /// Item sink progress; absent when the reviewed plan requests no item export.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_sink: Option<ItemSinkReport>,
    /// Reservation time; legacy checkpoints may not retain it separately.
    #[serde(default)]
    pub created_at: Option<Timestamp>,
    /// Most recent durable workflow update.
    #[serde(default)]
    pub updated_at: Option<Timestamp>,
    /// Active running time, excluding approval waits and pauses.
    #[serde(default)]
    pub elapsed_ms: u64,
    /// Discovery and job counters; null means the old checkpoint did not retain complete counters.
    #[serde(default)]
    pub counts: Option<RunCounts>,
    /// Reason a run is paused, using the spec 08 vocabulary.
    #[serde(default)]
    pub pause_reason: Option<String>,
    /// True when budgets, cancellation, errors or drift may have left sources unexplored.
    #[serde(default)]
    pub possibly_incomplete: bool,
    /// Trusted error code/class/message only; never page content or a full URL.
    #[serde(default)]
    pub last_error: Option<ErrorEnvelope>,
    /// The run.
    pub run_id: RunId,
    /// Hash of the approved plan.
    pub plan_hash: String,
    /// State.
    pub status: RunStatus,
    /// Pages fetched so far.
    pub pages_fetched: u32,
    /// Files handed to the engine.
    pub files_enqueued: u32,
    /// Files finished and verified.
    pub files_completed: u32,
    /// Files that failed.
    pub files_failed: u32,
    /// Bytes committed.
    pub bytes_done: u64,
    /// When the run started.
    #[serde(default)]
    pub started_at: Option<Timestamp>,
    /// When the run ended.
    #[serde(default)]
    pub finished_at: Option<Timestamp>,
    /// Why pagination stopped, for example `no_new_items` or `budget_pages`.
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// Drift findings (a selector that stopped matching, a layout change).
    #[serde(default)]
    pub drift: Vec<Diagnostic>,
}

/// What a run needs to resume after a restart. Stored in the journal by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Checkpoint {
    /// The run.
    pub run_id: RunId,
    /// Hash of the plan the run is executing; a different plan cannot resume this checkpoint.
    pub plan_hash: String,
    /// Opaque position of the next page (URL, cursor or offset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_page: Option<String>,
    /// Pages fetched so far.
    pub pages_fetched: u32,
    /// Hex BLAKE3 over the sorted dedupe keys seen so far, to detect a lost key set.
    pub seen_keys_digest: String,
    /// Consecutive pages without a new item.
    pub empty_pages: u32,
    /// Last update.
    pub updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_orders_by_gravity() {
        assert!(Severity::Error > Severity::Warning && Severity::Warning > Severity::Info);
    }

    #[test]
    fn diagnostic_serialises_with_stable_code() {
        let d = Diagnostic::error("plan.budget_invalid", "/budgets/max_pages", "too large");
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["code"], "plan.budget_invalid");
        assert_eq!(v["severity"], "error");
    }
}
