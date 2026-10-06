//! `jobs.*`: probe, enqueue, list, get, wait, control, verify.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeus_types::job::GrantScopeHint;
use zeus_types::{
    ApprovalId, GrantId, Hex256, HttpUrl, JobId, JobState, JobSummary, Origin, Overwrite, Priority,
    ProbeResult, QuarantineReason, RedactedUrl, RunId, SourceKind, TransportHint,
};

use crate::paging::{Page, PageRequest};
use crate::view::JobView;

/// `jobs.probe` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProbeParams {
    /// The URL to probe (http or https).
    pub url: HttpUrl,
    /// Probe with this origin's session when an effective grant exists. Never raises an approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_origin: Option<Origin>,
}

/// `jobs.probe` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProbeResultView {
    /// What the server answered (URLs redacted).
    pub probe: ProbeResult,
    /// The grant the probe used, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_grant: Option<GrantId>,
    /// True when an enqueue of this URL would need a human approval first.
    pub approval_needed: bool,
}

/// One download to enqueue. Mirrors the caller-settable fields of a `JobSpec` (01 section 3.2); the daemon fills
/// in the source from the authenticated connection, never from the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewJob {
    /// The stable address of the resource.
    pub url: HttpUrl,
    /// Last known resolved URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint_url: Option<HttpUrl>,
    /// Referrer, sent only for same-origin requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referrer: Option<HttpUrl>,
    /// Destination directory inside a write root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_dir: Option<String>,
    /// File name hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Expected SHA-256.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_sha256: Option<Hex256>,
    /// Expected size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_size: Option<u64>,
    /// Priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    /// Origin whose session the job needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_origin: Option<Origin>,
    /// Scope suggestion for the approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_scope_hint: Option<GrantScopeHint>,
    /// Extra request headers from the allow-list (01 section 3.2).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra_headers: BTreeMap<String, String>,
    /// Overwrite policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overwrite: Option<Overwrite>,
    /// Upper bound on connections, 1 to 32.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_connections: Option<u8>,
    /// Transport hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_hint: Option<TransportHint>,
    /// Free-form tags. Untrusted data.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// Workflow run this job belongs to (workflow runner only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
}

/// `jobs.enqueue` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EnqueueParams {
    /// One to 100 downloads.
    pub jobs: Vec<NewJob>,
    /// Plan only: resolve names and destinations, create nothing.
    #[serde(default)]
    pub dry_run: bool,
    /// Client-chosen key making the call safe to repeat (00 section 5.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// What a dry run would do for one job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PlannedJob {
    /// Redacted URL.
    pub url_redacted: RedactedUrl,
    /// File name after sanitising and collision handling, when known without a probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_filename: Option<String>,
    /// Full destination path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_path: Option<String>,
    /// Whether an approval would be raised.
    pub approval_needed: bool,
    /// Findings such as `policy.path_outside_roots`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// One entry of the enqueue result, in request order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EnqueuedJob {
    /// The new job. Absent on a dry run or when this entry was rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<JobId>,
    /// Its state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<JobState>,
    /// Approval the job waits for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<ApprovalId>,
    /// True when an identical request (same idempotency key) already created this job.
    #[serde(default)]
    pub deduplicated: bool,
    /// Present on a dry run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlannedJob>,
    /// Why this entry was rejected (the others may still succeed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<zeus_types::ErrorEnvelope>,
}

/// `jobs.enqueue` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EnqueueResult {
    /// One entry per requested job.
    pub jobs: Vec<EnqueuedJob>,
    /// Echo of the request flag.
    pub dry_run: bool,
}

/// `jobs.list` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JobsListParams {
    /// Paging.
    #[serde(default)]
    pub page: PageRequest,
    /// Only these states.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<JobState>,
    /// Only this source kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<SourceKind>,
    /// Only this workflow run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// Only jobs for this origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
}

/// `jobs.list` result.
pub type JobsListResult = Page<JobSummary>;

/// `jobs.get` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JobsGetParams {
    /// The job.
    pub job_id: JobId,
    /// Also return the raw URLs. **Human-only and audited**: the policy table rejects it for agent clients.
    #[serde(default)]
    pub reveal_urls: bool,
}

/// Raw URLs, returned only to a human who asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RevealedUrls {
    /// `spec.url` as received.
    pub url: HttpUrl,
    /// `spec.hint_url` as received.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint_url: Option<HttpUrl>,
}

/// `jobs.get` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JobsGetResult {
    /// The job with URLs redacted.
    pub job: JobView,
    /// Raw URLs, only when `reveal_urls` was set by a human.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revealed: Option<RevealedUrls>,
}

/// What `jobs.wait` waits for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WaitUntil {
    /// Any watched job reaches `completed`, `failed` or `canceled`, or needs a human or the browser.
    #[default]
    SettledOrBlocked,
    /// Every watched job reaches a terminal state or `failed`.
    AllTerminal,
    /// Any state change at all.
    AnyChange,
}

/// Longest `jobs.wait` timeout in milliseconds.
pub const MAX_WAIT_MS: u64 = 60_000;

/// `jobs.wait` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JobsWaitParams {
    /// One to 100 jobs.
    pub job_ids: Vec<JobId>,
    /// What to wait for.
    #[serde(default)]
    pub until: WaitUntil,
    /// Maximum wait, 1 to 60 000 ms (the MCP tool maps `timeout_s` to this).
    pub timeout_ms: u64,
}

/// `jobs.wait` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JobsWaitResult {
    /// Current summaries of the watched jobs.
    pub jobs: Vec<JobSummary>,
    /// True when the timeout expired before the condition held.
    pub timed_out: bool,
    /// Newest durable event sequence at the time of the answer.
    pub head_seq: u64,
}

/// A control action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ControlAction {
    /// Pause (01 transitions S05, S15, S24, S61).
    Pause,
    /// Resume a paused job (S50).
    Resume,
    /// Cancel; partial data is deleted (S06 and friends).
    Cancel,
    /// Retry a failed job (S90).
    Retry,
    /// Retry a job in `retry_scheduled` now (S60).
    RetryNow,
    /// Change priority.
    Reprioritize,
}

/// `jobs.control` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JobsControlParams {
    /// The job.
    pub job_id: JobId,
    /// What to do.
    pub action: ControlAction,
    /// New priority, required for `reprioritize`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    /// Idempotency key (00 section 5.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `jobs.control` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JobsControlResult {
    /// The job after the action. Illegal transitions fail with `api.invalid_transition` instead.
    pub job: JobSummary,
}

/// `jobs.verify` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JobsVerifyParams {
    /// A completed job.
    pub job_id: JobId,
    /// Compare against this digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_sha256: Option<Hex256>,
}

/// `jobs.verify` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JobsVerifyResult {
    /// SHA-256 of the file as it is now.
    pub sha256: Hex256,
    /// BLAKE3 of the file as it is now.
    pub blake3: Hex256,
    /// Size on disk.
    pub size_bytes: u64,
    /// Type from magic bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected_mime: Option<String>,
    /// Whether the digest matches the one recorded at completion.
    pub matches_recorded: bool,
    /// Whether it matches `expected_sha256`, when given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matches_expected: Option<bool>,
    /// Zeus's quarantine flag.
    pub quarantined: bool,
    /// Why, if so.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quarantine_reasons: Vec<QuarantineReason>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enqueue_params_round_trip_with_defaults() {
        let json = serde_json::json!({
            "jobs": [{ "url": "https://files.example.com/a.zip" }],
            "idempotency_key": "k1"
        });
        let params: EnqueueParams = serde_json::from_value(json).unwrap();
        assert!(!params.dry_run);
        assert_eq!(params.jobs.len(), 1);
        assert_eq!(
            serde_json::to_value(&params).unwrap()["jobs"][0]["url"],
            "https://files.example.com/a.zip"
        );
    }

    #[test]
    fn new_job_cannot_name_a_source() {
        // The source comes from the connection: an extra `source` member is ignored, never trusted.
        let json = serde_json::json!({
            "url": "https://files.example.com/a.zip",
            "source": { "kind": "browser_handoff" }
        });
        let job: NewJob = serde_json::from_value(json).unwrap();
        let back = serde_json::to_value(&job).unwrap();
        assert!(back.get("source").is_none());
    }

    #[test]
    fn wait_timeout_constant_matches_the_spec() {
        assert_eq!(MAX_WAIT_MS, 60_000);
    }
}
