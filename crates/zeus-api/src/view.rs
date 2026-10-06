//! Redacted projections of core entities.
//!
//! `zeus_types::Job` carries the raw job URL (it must: resuming needs it) and the journal stores it. The API never
//! returns a `Job` as is. [`JobView`] replaces every URL with its redacted form (00 section 8.2, mode `log`) and
//! lists extra header **names** only. Revealing the raw URL is a separate, human-only, audited parameter of
//! `jobs.get`.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeus_types::job::GrantScopeHint;
use zeus_types::{
    ApprovalId, ErrorEnvelope, GrantId, Hex256, Job, JobId, JobResult, JobSpec, JobState, Origin,
    Overwrite, Priority, ProbeResult, Progress, QuarantineReason, RedactMode, RedactedUrl,
    RetryState, Source, Timestamp, Transport, TransportHint,
};

/// A [`JobSpec`] with URLs redacted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JobSpecView {
    /// Redacted `spec.url`.
    pub url_redacted: RedactedUrl,
    /// Redacted `spec.hint_url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint_url_redacted: Option<RedactedUrl>,
    /// Redacted `spec.referrer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referrer_redacted: Option<RedactedUrl>,
    /// Destination directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_dir: Option<String>,
    /// File name hint. Untrusted data (01 section 12.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Expected digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_sha256: Option<Hex256>,
    /// Expected size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_size: Option<u64>,
    /// Priority.
    pub priority: Priority,
    /// Origin whose session the job needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_origin: Option<Origin>,
    /// Scope hint shown on approvals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_scope_hint: Option<GrantScopeHint>,
    /// Names of extra request headers (values are not returned).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_header_names: Vec<String>,
    /// What to do when the final path exists.
    pub overwrite: Overwrite,
    /// Upper bound on connections.
    pub max_connections: u8,
    /// Transport hint.
    pub transport_hint: TransportHint,
    /// Caller labels. Untrusted data.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// Who asked and how.
    pub source: Source,
}

/// A [`Job`] safe to return to any client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JobView {
    /// Identifier.
    pub id: JobId,
    /// State.
    pub state: JobState,
    /// Zeus's own quarantine flag; only ever true when completed.
    pub quarantined: bool,
    /// Why it was quarantined.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quarantine_reasons: Vec<QuarantineReason>,
    /// The request, redacted.
    pub spec: JobSpecView,
    /// Creation instant.
    pub created_at: Timestamp,
    /// Last change.
    pub updated_at: Timestamp,
    /// Last state change.
    pub state_since: Timestamp,
    /// Latest probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<ProbeResult>,
    /// Committed progress.
    pub progress: Progress,
    /// Retry counters.
    pub retry: RetryState,
    /// Most recent error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorEnvelope>,
    /// Outcome of a completed job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<JobResult>,
    /// How bytes flow.
    pub transport: Transport,
    /// Effective grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<GrantId>,
    /// Pending approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<ApprovalId>,
}

impl JobSpecView {
    /// Project a spec.
    pub fn from_spec(spec: &JobSpec) -> Self {
        Self {
            url_redacted: spec.url.redact(RedactMode::Log),
            hint_url_redacted: spec.hint_url.as_ref().map(|u| u.redact(RedactMode::Log)),
            referrer_redacted: spec.referrer.as_ref().map(|u| u.redact(RedactMode::Log)),
            dest_dir: spec.dest_dir.clone(),
            filename: spec.filename.clone(),
            expected_sha256: spec.expected_sha256,
            expected_size: spec.expected_size,
            priority: spec.priority(),
            session_origin: spec.session_origin.clone(),
            grant_scope_hint: spec.grant_scope_hint,
            extra_header_names: spec.extra_headers.keys().cloned().collect(),
            overwrite: spec.overwrite(),
            max_connections: spec.max_connections(),
            transport_hint: spec.transport_hint(),
            labels: spec.labels.clone(),
            source: spec.source.clone(),
        }
    }
}

impl From<&Job> for JobView {
    fn from(job: &Job) -> Self {
        Self {
            id: job.id,
            state: job.state,
            quarantined: job.quarantined,
            quarantine_reasons: job.quarantine_reasons.clone(),
            spec: JobSpecView::from_spec(&job.spec),
            created_at: job.created_at,
            updated_at: job.updated_at,
            state_since: job.state_since,
            probe: job.probe.clone(),
            progress: job.progress.clone(),
            retry: job.retry.clone(),
            error: job.error.clone(),
            result: job.result.clone(),
            transport: job.transport,
            grant_id: job.grant_id,
            approval_id: job.approval_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeus_types::{HttpUrl, SourceKind};

    fn job_with_secret_url() -> Job {
        let url =
            HttpUrl::parse("https://files.example.com/a/b.zip?X-Amz-Signature=ZEUS-CANARY-sig&x=1")
                .unwrap();
        let mut spec = JobSpec::new(url, Source::new(SourceKind::Agent));
        spec.hint_url =
            Some(HttpUrl::parse("https://cdn.example.com/b.zip?token=ZEUS-CANARY-tok").unwrap());
        spec.extra_headers
            .insert("Accept".into(), "ZEUS-CANARY-header-value".into());
        Job::new(
            zeus_types::IdGenerator::new().job_id(),
            spec,
            JobState::Queued,
            Timestamp::now(),
        )
    }

    #[test]
    fn a_job_view_never_contains_the_raw_url_or_header_values() {
        let view = JobView::from(&job_with_secret_url());
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("ZEUS-CANARY"), "{json}");
        assert!(json.contains("files.example.com"));
        assert_eq!(view.spec.extra_header_names, ["Accept"]);
    }

    #[test]
    fn the_raw_job_would_leak_which_is_why_the_view_exists() {
        let json = serde_json::to_string(&job_with_secret_url()).unwrap();
        assert!(json.contains("ZEUS-CANARY-sig"));
    }
}
