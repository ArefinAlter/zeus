//! Job to A2A task mapping.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeus_types::{ErrorClass, Job, JobState};

/// A2A task states (specification 1.0.0, spelled as in the protobuf enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum TaskState {
    /// `TASK_STATE_UNSPECIFIED`.
    #[serde(rename = "TASK_STATE_UNSPECIFIED")]
    Unspecified,
    /// `TASK_STATE_SUBMITTED`.
    #[serde(rename = "TASK_STATE_SUBMITTED")]
    Submitted,
    /// `TASK_STATE_WORKING`.
    #[serde(rename = "TASK_STATE_WORKING")]
    Working,
    /// `TASK_STATE_COMPLETED`.
    #[serde(rename = "TASK_STATE_COMPLETED")]
    Completed,
    /// `TASK_STATE_FAILED`.
    #[serde(rename = "TASK_STATE_FAILED")]
    Failed,
    /// `TASK_STATE_CANCELED`.
    #[serde(rename = "TASK_STATE_CANCELED")]
    Canceled,
    /// `TASK_STATE_INPUT_REQUIRED`.
    #[serde(rename = "TASK_STATE_INPUT_REQUIRED")]
    InputRequired,
    /// `TASK_STATE_REJECTED`.
    #[serde(rename = "TASK_STATE_REJECTED")]
    Rejected,
    /// `TASK_STATE_AUTH_REQUIRED`.
    #[serde(rename = "TASK_STATE_AUTH_REQUIRED")]
    AuthRequired,
}

impl TaskState {
    /// True for states after which the task never changes.
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Canceled | Self::Rejected
        )
    }
}

/// Map a Zeus job state to the A2A task state (ARCHITECTURE section 10).
///
/// `awaiting_approval` is `INPUT_REQUIRED`: a human must decide, which the caller cannot do. `awaiting_session` is
/// `AUTH_REQUIRED`. A failure whose class is `policy_denied` is `REJECTED`; every other failure is `FAILED`.
/// `paused` and `retry_scheduled` stay `WORKING` because the task is still alive and will continue without the
/// caller's input.
pub const fn task_state_for(state: JobState, error_class: Option<ErrorClass>) -> TaskState {
    match state {
        JobState::Queued | JobState::Probing => TaskState::Submitted,
        JobState::Downloading
        | JobState::Verifying
        | JobState::Finalizing
        | JobState::RetryScheduled
        | JobState::Paused => TaskState::Working,
        JobState::AwaitingApproval => TaskState::InputRequired,
        JobState::AwaitingSession => TaskState::AuthRequired,
        JobState::Completed => TaskState::Completed,
        JobState::Failed => match error_class {
            Some(ErrorClass::PolicyDenied) => TaskState::Rejected,
            _ => TaskState::Failed,
        },
        JobState::Canceled => TaskState::Canceled,
    }
}

/// One content part. Exactly one of `text`, `raw`, `url`, `data` is set. Zeus never inlines file bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    /// Plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// A URI the peer can fetch or recognise (Zeus uses `file:` URIs for local peers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Structured data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// File name hint (untrusted, sanitised).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Media type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// Extra metadata: Zeus puts `sha256` and `size_bytes` here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// A task output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    /// Identifier within the task.
    pub artifact_id: String,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Content.
    pub parts: Vec<Part>,
}

/// State of a task with an optional human-readable note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatus {
    /// The state.
    pub state: TaskState,
    /// Explanation for states such as `INPUT_REQUIRED`. Daemon-written; never contains secrets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// RFC 3339 UTC instant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// An A2A task backed by a Zeus job. The task id is the job id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    /// The job id (`job_...`).
    pub id: String,
    /// Conversation context; Zeus uses the run id for workflow jobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    /// Current status.
    pub status: TaskStatus,
    /// Outputs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,
}

/// The status of the task for `job`.
pub fn task_status_for(job: &Job) -> TaskStatus {
    let class = job.error.as_ref().map(|e| e.class);
    let state = task_state_for(job.state, class);
    let message = match job.state {
        JobState::AwaitingApproval => {
            Some("A person must approve this on the user's machine.".to_string())
        }
        JobState::AwaitingSession => {
            Some("Waiting for the user's browser to provide access.".to_string())
        }
        JobState::Paused => Some("Paused by a client.".to_string()),
        JobState::Failed | JobState::RetryScheduled => {
            job.error.as_ref().map(|e| e.code.to_string())
        }
        _ => None,
    };
    TaskStatus {
        state,
        message,
        timestamp: Some(job.updated_at.to_rfc3339()),
    }
}

/// The artifacts of a completed job: one file part with the SHA-256 in metadata, never inline bytes. A quarantined
/// file is still reported but flagged in the metadata so peers do not open it.
pub fn artifacts_for(job: &Job) -> Vec<Artifact> {
    let Some(result) = &job.result else {
        return Vec::new();
    };
    let file_name = std::path::Path::new(&result.path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    let part = Part {
        url: Some(format!(
            "file:///{}",
            result.path.replace('\\', "/").trim_start_matches('/')
        )),
        filename: file_name.clone(),
        media_type: result.detected_mime.clone(),
        metadata: Some(serde_json::json!({
            "sha256": result.sha256.to_hex(),
            "size_bytes": result.size_bytes,
            "quarantined": job.quarantined,
        })),
        ..Part::default()
    };
    vec![Artifact {
        artifact_id: "file".to_string(),
        name: file_name,
        parts: vec![part],
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_job_state_has_a_task_state() {
        for state in JobState::ALL {
            let mapped = task_state_for(state, None);
            assert_ne!(mapped, TaskState::Unspecified, "{state:?}");
        }
    }

    #[test]
    fn terminal_job_states_map_to_terminal_task_states_and_back() {
        for state in JobState::ALL {
            if state.is_terminal() {
                assert!(task_state_for(state, None).is_terminal(), "{state:?}");
            }
        }
        assert!(task_state_for(JobState::Failed, None).is_terminal());
        assert!(!task_state_for(JobState::Downloading, None).is_terminal());
    }

    #[test]
    fn human_waits_are_input_and_auth_required() {
        assert_eq!(
            task_state_for(JobState::AwaitingApproval, None),
            TaskState::InputRequired
        );
        assert_eq!(
            task_state_for(JobState::AwaitingSession, None),
            TaskState::AuthRequired
        );
    }

    #[test]
    fn policy_denied_failures_are_rejected() {
        assert_eq!(
            task_state_for(JobState::Failed, Some(ErrorClass::PolicyDenied)),
            TaskState::Rejected
        );
        assert_eq!(
            task_state_for(JobState::Failed, Some(ErrorClass::Permanent)),
            TaskState::Failed
        );
        assert_eq!(task_state_for(JobState::Failed, None), TaskState::Failed);
    }

    #[test]
    fn task_state_uses_the_protobuf_names() {
        assert_eq!(
            serde_json::to_string(&TaskState::InputRequired).unwrap(),
            "\"TASK_STATE_INPUT_REQUIRED\""
        );
        let back: TaskState = serde_json::from_str("\"TASK_STATE_AUTH_REQUIRED\"").unwrap();
        assert_eq!(back, TaskState::AuthRequired);
    }
}
