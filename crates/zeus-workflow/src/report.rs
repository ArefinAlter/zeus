//! Diagnostics, the dry-run report, the run report and checkpoints.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeus_types::{RunId, Timestamp};

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunReport {
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
    pub started_at: Timestamp,
    /// When the run ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
    /// Why pagination stopped, for example `no_new_items` or `budget_pages`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Drift findings (a selector that stopped matching, a layout change).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
