//! Fixed, typed operations exposed to the bundled webview.
//!
//! There is deliberately no method-name parameter, credential response, session response,
//! URL reveal, arbitrary file opening, process launching or approval-decision command.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, MutexGuard};
use zeus_api::auth::{ClientCredential, IpcSecret};
use zeus_api::client::{ClientConfig, call};
use zeus_api::endpoint::ZeusPaths;
use zeus_api::methods::artifacts::ArtifactsListParams;
use zeus_api::methods::daemon::{ClientInfo, StatusParams};
use zeus_api::methods::files::FilesListParams;
use zeus_api::methods::jobs::{
    ControlAction, EnqueueParams, JobsControlParams, JobsListParams, NewJob,
};
use zeus_api::methods::sessions::ApprovalsListParams;
use zeus_api::methods::workflows::WorkflowsListParams;
use zeus_api::methods::{
    ApprovalsList, ArtifactsList, DaemonStatus, FilesList, JobsControl, JobsEnqueue, JobsList,
    WorkflowsList,
};
use zeus_api::paging::PageRequest;
use zeus_api::{ApiError, Client, ClientKind, ClientRole, IpcClient, Permission};
use zeus_types::{HttpUrl, JobId, SecretString};

const FRESH_FOR: Duration = Duration::from_secs(20);
const REVIEW_FOR: Duration = Duration::from_secs(600);
const MAX_INPUT_BYTES: usize = 64 * 1024;

/// An error safe to present without including daemon text, paths or request URLs.
#[derive(Debug, Serialize)]
pub struct BridgeError {
    code: String,
    message: String,
}

impl BridgeError {
    fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    fn daemon(error: ApiError) -> Self {
        Self::new(
            error.envelope().code.as_str(),
            "The daemon could not complete this operation. Refresh the workspace and review details in the Zeus CLI.",
        )
    }

    fn reconnect() -> Self {
        Self::new(
            "desktop.refresh_required",
            "Refresh the connection before changing work.",
        )
    }
}

/// A closed set of readable workspace pages.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    /// Redacted download summaries.
    Jobs,
    /// Workflow progress and counts.
    Runs,
    /// Files inside the configured default root.
    Files,
    /// Artifact identity and integrity facts.
    Artifacts,
    /// Approvals visible to this restricted GUI identity.
    Approvals,
}

/// The four job mutations this client implements.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobAction {
    /// Pause a job.
    Pause,
    /// Resume a paused job.
    Resume,
    /// Cancel unfinished work.
    Cancel,
    /// Retry a failed job.
    Retry,
}

impl From<JobAction> for ControlAction {
    fn from(value: JobAction) -> Self {
        match value {
            JobAction::Pause => Self::Pause,
            JobAction::Resume => Self::Resume,
            JobAction::Cancel => Self::Cancel,
            JobAction::Retry => Self::Retry,
        }
    }
}

/// Fresh authority and daemon health, without the authenticated session or credential.
#[derive(Debug, Serialize)]
pub struct Snapshot {
    generation: u64,
    daemon_version: String,
    capabilities: Vec<String>,
    permissions: Vec<String>,
    head_seq: u64,
    pending_approvals: u32,
    extension_connected: bool,
    kill_switch_engaged: bool,
    journal_ok: bool,
}

/// One bounded page of explicit display projections.
#[derive(Debug, Serialize)]
pub struct PageView {
    section: Section,
    generation: u64,
    items: Vec<Value>,
    next_cursor: Option<String>,
}

#[derive(Default)]
struct MutationGate {
    refreshed: Option<Instant>,
}

impl MutationGate {
    fn check(&self, expected: u64, actual: u64, now: Instant) -> Result<(), BridgeError> {
        if expected != actual
            || self
                .refreshed
                .is_none_or(|at| now.saturating_duration_since(at) > FRESH_FOR)
        {
            return Err(BridgeError::reconnect());
        }
        Ok(())
    }
}

struct Review {
    id: u64,
    created: Instant,
    jobs: Vec<NewJob>,
}

struct Connection {
    client: IpcClient,
    generation: u64,
    gate: MutationGate,
    reviews: Vec<Review>,
}

impl Connection {
    fn require(&self, permission: Permission, capability: Option<&str>) -> Result<(), BridgeError> {
        if !self.client.session().permissions.contains(&permission) {
            return Err(BridgeError::new(
                "api.permission_denied",
                "This desktop identity does not have permission for that operation.",
            ));
        }
        if capability.is_some_and(|name| !self.client.hello().capabilities.contains(name)) {
            return Err(BridgeError::new(
                "api.capability_required",
                "This daemon does not offer the required feature.",
            ));
        }
        Ok(())
    }

    fn mutation(&self, generation: u64, permission: Permission) -> Result<(), BridgeError> {
        self.gate
            .check(generation, self.generation, Instant::now())?;
        self.require(permission, Some(zeus_api::capabilities::IDEMPOTENCY))
    }

    fn failed(&mut self, error: ApiError) -> BridgeError {
        self.gate.refreshed = None;
        BridgeError::daemon(error)
    }
}

/// Rust-owned authenticated connection and private link-review inputs.
#[derive(Default)]
pub struct Bridge {
    connection: Mutex<Option<Connection>>,
    next_generation: AtomicU64,
    next_review: AtomicU64,
}

fn read_private(path: &std::path::Path) -> Result<SecretString, BridgeError> {
    let missing = || {
        BridgeError::new(
            "desktop.not_installed",
            "Install or update the local daemon with `zeus daemon install`, then start it and reconnect. A dedicated gui.token is required.",
        )
    };
    zeus_api::transport::secure_private_file(path).map_err(|_| missing())?;
    let mut data = String::new();
    let read = std::fs::File::open(path)
        .map_err(|_| missing())?
        .take(4097)
        .read_to_string(&mut data);
    let data = SecretString::new(data);
    read.map_err(|_| missing())?;
    if data.len() > 4096 {
        return Err(missing());
    }
    Ok(data)
}

impl Bridge {
    async fn connected(&self) -> Result<MutexGuard<'_, Option<Connection>>, BridgeError> {
        let mut state = self.connection.lock().await;
        if state.is_none() {
            let paths = ZeusPaths::discover().map_err(BridgeError::daemon)?;
            let secret =
                IpcSecret::from_file_text(read_private(&paths.ipc_secret_file())?.expose())
                    .map_err(BridgeError::daemon)?;
            let credential = ClientCredential::from_file_text(
                read_private(&paths.credential_file("gui"))?.expose(),
            )
            .map_err(BridgeError::daemon)?;
            let mut config = ClientConfig::new(ClientInfo {
                name: "zeus-desktop".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                role: ClientRole::Gui,
            });
            config.call_timeout = Duration::from_secs(15);
            config.capabilities = [
                zeus_api::capabilities::IDEMPOTENCY,
                zeus_api::capabilities::ARTIFACTS,
                zeus_api::capabilities::WORKFLOWS,
            ]
            .into_iter()
            .map(str::to_owned)
            .collect();
            let client = IpcClient::connect(&paths.endpoint(), credential, &secret, config)
                .await
                .map_err(BridgeError::daemon)?;
            if client.session().role != ClientRole::Gui
                || client.session().credential_kind != ClientKind::Agent
            {
                return Err(BridgeError::new(
                    "desktop.identity_mismatch",
                    "This client requires its dedicated restricted GUI credential. Update the daemon installation.",
                ));
            }
            *state = Some(Connection {
                client,
                generation: self.next_generation.fetch_add(1, Ordering::Relaxed) + 1,
                gate: MutationGate::default(),
                reviews: Vec::new(),
            });
        }
        Ok(state)
    }

    /// Refresh health and authority. A failed refresh drops the connection and all private reviews.
    pub async fn snapshot(&self) -> Result<Snapshot, BridgeError> {
        let mut state = self.connected().await?;
        let connection = state.as_mut().ok_or_else(BridgeError::reconnect)?;
        let status = match call::<DaemonStatus, _>(&connection.client, StatusParams {}).await {
            Ok(value) => value,
            Err(error) => {
                state.take();
                return Err(BridgeError::daemon(error));
            }
        };
        connection.gate.refreshed = Some(Instant::now());
        Ok(Snapshot {
            generation: connection.generation,
            daemon_version: status.daemon.version,
            capabilities: connection
                .client
                .hello()
                .capabilities
                .iter()
                .cloned()
                .collect(),
            permissions: connection
                .client
                .session()
                .permissions
                .iter()
                .map(|p| p.as_str().to_owned())
                .collect(),
            head_seq: status.head_seq,
            pending_approvals: status.pending_approvals,
            extension_connected: status.extension_connected,
            kill_switch_engaged: status.kill_switch_engaged,
            journal_ok: status.journal_ok,
        })
    }

    /// Read a page using a typed method and an explicit metadata projection.
    pub async fn page(
        &self,
        section: Section,
        cursor: Option<String>,
    ) -> Result<PageView, BridgeError> {
        let page = PageRequest {
            limit: Some(100),
            cursor,
        };
        page.validate().map_err(BridgeError::daemon)?;
        let mut state = self.connected().await?;
        let connection = state.as_mut().ok_or_else(BridgeError::reconnect)?;
        let (permission, capability) = match section {
            Section::Jobs => (Permission::JobsRead, None),
            Section::Runs => (
                Permission::WorkflowsPlan,
                Some(zeus_api::capabilities::WORKFLOWS),
            ),
            Section::Files => (Permission::FilesRead, None),
            Section::Artifacts => (
                Permission::FilesRead,
                Some(zeus_api::capabilities::ARTIFACTS),
            ),
            Section::Approvals => (Permission::ApprovalsRead, None),
        };
        connection.require(permission, capability)?;
        let result = match section {
            Section::Jobs => call::<JobsList, _>(
                &connection.client,
                JobsListParams {
                    page,
                    ..Default::default()
                },
            )
            .await
            .and_then(|result| {
                project_page(
                    result,
                    &[
                        "id",
                        "state",
                        "filename",
                        "url_redacted",
                        "bytes_done",
                        "bytes_total",
                        "rate_ewma",
                        "eta_s",
                        "quarantined",
                        "source_kind",
                        "updated_at",
                    ],
                )
            }),
            Section::Runs => {
                call::<WorkflowsList, _>(&connection.client, WorkflowsListParams { page })
                    .await
                    .and_then(|result| {
                        project_page(
                            result,
                            &[
                                "run_id",
                                "status",
                                "plan_hash",
                                "pages_fetched",
                                "files_enqueued",
                                "files_completed",
                                "files_failed",
                                "bytes_done",
                                "started_at",
                                "finished_at",
                                "report_version",
                                "plan_id",
                                "created_at",
                                "updated_at",
                                "elapsed_ms",
                                "counts",
                                "pause_reason",
                                "possibly_incomplete",
                            ],
                        )
                    })
            }
            Section::Files => call::<FilesList, _>(
                &connection.client,
                FilesListParams {
                    page,
                    ..Default::default()
                },
            )
            .await
            .and_then(|result| {
                project_page(
                    result,
                    &[
                        "root",
                        "path",
                        "size_bytes",
                        "modified_at",
                        "sha256",
                        "quarantined",
                        "partial",
                        "job_id",
                    ],
                )
            }),
            Section::Artifacts => call::<ArtifactsList, _>(
                &connection.client,
                ArtifactsListParams {
                    page,
                    ..Default::default()
                },
            )
            .await
            .and_then(|result| {
                project_page(
                    result,
                    &[
                        "id",
                        "state",
                        "name",
                        "size_bytes",
                        "sha256",
                        "blake3",
                        "media_type",
                        "quarantined",
                        "created_at",
                        "job_id",
                        "run_id",
                    ],
                )
            }),
            Section::Approvals => call::<ApprovalsList, _>(
                &connection.client,
                ApprovalsListParams {
                    page,
                    ..Default::default()
                },
            )
            .await
            .and_then(|result| {
                project_page(
                    result,
                    &[
                        "id",
                        "kind",
                        "state",
                        "created_at",
                        "expires_at",
                        "request_hash",
                    ],
                )
            }),
        };
        let (items, next_cursor) = result.map_err(|error| connection.failed(error))?;
        Ok(PageView {
            section,
            generation: connection.generation,
            items,
            next_cursor,
        })
    }

    /// Resolve a bounded batch through the daemon without creating jobs, and retain exact inputs privately.
    pub async fn preview_links(
        &self,
        urls: Vec<String>,
        destination: Option<String>,
        generation: u64,
    ) -> Result<Value, BridgeError> {
        let jobs = new_jobs(urls, destination)?;
        let mut state = self.connected().await?;
        let connection = state.as_mut().ok_or_else(BridgeError::reconnect)?;
        connection.mutation(generation, Permission::JobsWrite)?;
        let result = call::<JobsEnqueue, _>(
            &connection.client,
            EnqueueParams {
                jobs: jobs.clone(),
                dry_run: true,
                idempotency_key: None,
            },
        )
        .await
        .map_err(|error| connection.failed(error))?;
        let review_id = self.next_review.fetch_add(1, Ordering::Relaxed) + 1;
        let items = result.jobs.iter().map(|entry| match &entry.plan {
            Some(plan) => json!({"url_redacted":plan.url_redacted, "filename":plan.resolved_filename, "destination":plan.dest_path, "approval_needed":plan.approval_needed, "warnings":plan.warnings}),
            None => json!({"error_code":entry.error.as_ref().map(|error| error.code.as_str())}),
        }).collect::<Vec<_>>();
        if result
            .jobs
            .iter()
            .any(|entry| entry.error.is_some() || entry.plan.is_none())
        {
            return Ok(json!({"review_id":null,"items":items,"ready":false}));
        }
        if connection.reviews.len() >= 4 {
            connection.reviews.remove(0);
        }
        connection.reviews.push(Review {
            id: review_id,
            created: Instant::now(),
            jobs,
        });
        Ok(json!({"review_id":review_id,"items":items,"ready":true}))
    }

    /// Enqueue exactly a still-current private review, preserving the caller's retry key.
    pub async fn enqueue_review(
        &self,
        review_id: u64,
        idempotency_key: String,
        generation: u64,
    ) -> Result<Value, BridgeError> {
        validate_key(&idempotency_key)?;
        let mut state = self.connected().await?;
        let connection = state.as_mut().ok_or_else(BridgeError::reconnect)?;
        connection.mutation(generation, Permission::JobsWrite)?;
        let review = connection
            .reviews
            .iter()
            .find(|review| review.id == review_id && review.created.elapsed() <= REVIEW_FOR)
            .ok_or_else(|| {
                BridgeError::new(
                    "desktop.review_expired",
                    "Preview the links again before starting them.",
                )
            })?;
        let result = call::<JobsEnqueue, _>(
            &connection.client,
            EnqueueParams {
                jobs: review.jobs.clone(),
                dry_run: false,
                idempotency_key: Some(idempotency_key),
            },
        )
        .await
        .map_err(|error| connection.failed(error))?;
        Ok(
            json!({"replayed":result.replayed,"jobs":result.jobs.iter().map(|entry| json!({
            "job_id":entry.job_id,"state":entry.state,"approval_id":entry.approval_id,
            "error_code":entry.error.as_ref().map(|error| error.code.as_str()),
        })).collect::<Vec<_>>()}),
        )
    }

    /// Apply one of the four implemented controls through the daemon's ordinary transition checks.
    pub async fn control_job(
        &self,
        job_id: String,
        action: JobAction,
        idempotency_key: String,
        generation: u64,
    ) -> Result<Value, BridgeError> {
        validate_key(&idempotency_key)?;
        let job_id = job_id
            .parse::<JobId>()
            .map_err(|_| BridgeError::new("api.invalid_params", "Select a valid download."))?;
        let mut state = self.connected().await?;
        let connection = state.as_mut().ok_or_else(BridgeError::reconnect)?;
        connection.mutation(generation, Permission::JobsControl)?;
        let result = call::<JobsControl, _>(
            &connection.client,
            JobsControlParams {
                job_id,
                action: action.into(),
                priority: None,
                idempotency_key: Some(idempotency_key),
            },
        )
        .await
        .map_err(|error| connection.failed(error))?;
        Ok(json!({"replayed":result.replayed,"job_id":result.job.id,"state":result.job.state}))
    }
}

fn project(value: Value, fields: &[&str]) -> Value {
    let mut output = serde_json::Map::new();
    for field in fields {
        if let Some(value) = value.get(*field) {
            output.insert((*field).into(), value.clone());
        }
    }
    Value::Object(output)
}

fn project_page<T: Serialize>(
    page: zeus_api::paging::Page<T>,
    fields: &[&str],
) -> Result<(Vec<Value>, Option<String>), ApiError> {
    let items = page
        .items
        .into_iter()
        .map(|item| {
            serde_json::to_value(item)
                .map(|value| project(value, fields))
                .map_err(|_| ApiError::internal("desktop view could not be encoded"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((items, page.next_cursor))
}

fn validate_key(key: &str) -> Result<(), BridgeError> {
    if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
        return Err(BridgeError::new(
            "api.invalid_params",
            "The retry key must have 1 to 128 bytes and no control characters.",
        ));
    }
    Ok(())
}

fn new_jobs(urls: Vec<String>, destination: Option<String>) -> Result<Vec<NewJob>, BridgeError> {
    let invalid = || {
        BridgeError::new(
            "api.invalid_params",
            "Use 1 to 50 unique HTTP or HTTPS links, with at most 64 KiB of input. Every link must be valid before preview.",
        )
    };
    if urls.is_empty()
        || urls.len() > 1000
        || urls.iter().map(String::len).sum::<usize>() > MAX_INPUT_BYTES
        || destination
            .as_ref()
            .is_some_and(|value| value.len() > 4096 || value.chars().any(char::is_control))
    {
        return Err(invalid());
    }
    let mut seen = BTreeSet::new();
    let mut jobs = Vec::new();
    for input in urls {
        let input = input.trim();
        if input.is_empty() || !seen.insert(input.to_owned()) {
            continue;
        }
        let url = HttpUrl::parse(input).map_err(|_| invalid())?;
        jobs.push(NewJob {
            url,
            hint_url: None,
            referrer: None,
            dest_dir: destination.clone().filter(|value| !value.is_empty()),
            filename: None,
            expected_sha256: None,
            expected_size: None,
            priority: None,
            session_origin: None,
            grant_scope_hint: None,
            extra_headers: BTreeMap::new(),
            overwrite: None,
            max_connections: None,
            transport_hint: None,
            labels: BTreeMap::new(),
            run_id: None,
        });
        if jobs.len() > 50 {
            return Err(invalid());
        }
    }
    if jobs.is_empty() {
        return Err(invalid());
    }
    Ok(jobs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_snapshots_and_connection_generations_cannot_mutate() {
        let now = Instant::now();
        let mut gate = MutationGate::default();
        assert!(gate.check(1, 1, now).is_err());
        gate.refreshed = Some(now);
        assert!(gate.check(1, 1, now).is_ok());
        assert!(gate.check(1, 2, now).is_err());
        assert!(
            gate.check(1, 1, now + FRESH_FOR + Duration::from_millis(1))
                .is_err()
        );
        gate.refreshed = None;
        assert!(gate.check(1, 1, now).is_err());
    }

    #[test]
    fn display_projection_omits_untrusted_metadata_and_session_fields() {
        let output = project(
            json!({"id":"art_test","sha256":"abc","metadata":{"token":"ZEUS-CANARY-PRIVATE"},"session_origin":"ZEUS-CANARY-ORIGIN","origin":{"url":"https://example.org/?secret=ZEUS-CANARY-URL"}}),
            &["id", "sha256"],
        );
        let encoded = output.to_string();
        assert_eq!(output, json!({"id":"art_test","sha256":"abc"}));
        assert!(!encoded.contains("ZEUS-CANARY-"));
    }

    #[test]
    fn private_batch_preserves_signed_queries_and_validates_every_input() {
        let jobs = new_jobs(
            vec![
                "https://example.org/a?sig=one".into(),
                "https://example.org/a?sig=two".into(),
                "https://example.org/a?sig=one".into(),
            ],
            None,
        )
        .unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[1].url.expose(), "https://example.org/a?sig=two");
        let error = new_jobs(
            vec![
                "https://example.org/a".into(),
                "file:///ZEUS-CANARY-BAD".into(),
            ],
            None,
        )
        .unwrap_err();
        assert!(
            !serde_json::to_string(&error)
                .unwrap()
                .contains("ZEUS-CANARY")
        );
        assert!(
            new_jobs(
                (0..51)
                    .map(|n| format!("https://example.org/{n}"))
                    .collect(),
                None
            )
            .is_err()
        );
        assert!(new_jobs(vec![" ".into()], None).is_err());
    }

    #[test]
    fn bridge_error_never_forwards_daemon_or_url_text() {
        let error = BridgeError::daemon(ApiError::invalid_params(
            "ZEUS-CANARY-SECRET https://example.org/?token=private",
        ));
        let encoded = serde_json::to_string(&error).unwrap();
        assert!(!encoded.contains("ZEUS-CANARY"));
        assert!(!encoded.contains("token="));
        assert!(validate_key("retry-1").is_ok());
        assert!(validate_key("").is_err());
        assert!(validate_key("line\nbreak").is_err());
    }
}
