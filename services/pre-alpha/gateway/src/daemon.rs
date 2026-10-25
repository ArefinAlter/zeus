//! The one authenticated connection to zeusd, and the typed calls the bridge makes over it.
//!
//! The credential is the installation's dedicated `gui.token` (kind `agent`, role `gui`): seven permissions, none of
//! which decides an approval, changes a grant, writes files or reveals a job's original address. The connection is
//! refused unless the daemon confirms exactly that identity, as in apps/desktop.

use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;
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
use zeus_api::paging::{Page, PageRequest};
use zeus_api::{ApiError, Client, ClientKind, ClientRole, IpcClient, Permission};
use zeus_types::{ApprovalState, HttpUrl, JobId, SecretString};

/// A failure safe to show a visitor: a stable code and a fixed sentence, never daemon text, paths or addresses.
#[derive(Debug, Clone, Serialize)]
pub struct Failure {
    /// Stable code, from the daemon's envelope or the bridge's own `bridge.*` set.
    pub code: String,
    /// A fixed explanation.
    pub message: String,
}

impl Failure {
    /// A bridge-defined failure.
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    fn daemon(error: &ApiError) -> Self {
        let code = error.envelope().code.as_str();
        let message = match code {
            "api.invalid_transition" => "That download cannot do this in its current state.",
            "api.rate_limited" => "zeusd is busy. Wait a moment and try again.",
            "api.permission_denied" => "The bridge credential does not have permission for that.",
            c if c.starts_with("policy.") => "zeusd's policy refused this. Nothing was changed.",
            _ => "zeusd could not complete this operation. Refresh and try again.",
        };
        Self::new(code, message)
    }
}

/// Health and authority of the connection.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    /// zeusd's version.
    pub daemon_version: String,
    /// Capabilities the daemon negotiated.
    pub capabilities: Vec<String>,
    /// Permissions of the bridge's credential.
    pub permissions: Vec<String>,
    /// Durable head of the event log.
    pub head_seq: u64,
    /// Pending approvals daemon-wide.
    pub pending_approvals: u32,
    /// The global stop.
    pub kill_switch_engaged: bool,
    /// Journal health.
    pub journal_ok: bool,
    /// Daemon uptime.
    pub uptime_ms: u64,
}

/// One job to create, already validated by the caller.
#[derive(Debug, Clone)]
pub struct Wanted {
    /// The exact address (kept private; only the redacted form is ever returned).
    pub url: HttpUrl,
    /// Absolute destination directory, inside a write root.
    pub dest_dir: Option<String>,
}

/// The four controls the bridge offers, as in apps/desktop.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Pause.
    Pause,
    /// Resume.
    Resume,
    /// Cancel.
    Cancel,
    /// Retry a failed job.
    Retry,
}

fn read_private(path: &std::path::Path) -> Result<SecretString, Failure> {
    let missing = || {
        Failure::new(
            "bridge.not_installed",
            "zeusd is not installed for this user, or its gui.token is missing. Run `zeus daemon install`, start zeusd, then restart the bridge.",
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

/// The lazily opened connection. A failed call drops it; the next call reconnects.
#[derive(Default)]
pub struct Daemon {
    connection: Mutex<Option<IpcClient>>,
}

fn page(cursor: Option<String>) -> PageRequest {
    PageRequest {
        limit: Some(100),
        cursor,
    }
}

fn to_values<T: Serialize>(page: Page<T>) -> (Vec<Value>, Option<String>) {
    let items = page
        .items
        .into_iter()
        .filter_map(|item| serde_json::to_value(item).ok())
        .collect();
    (items, page.next_cursor)
}

impl Daemon {
    async fn open() -> Result<IpcClient, Failure> {
        let paths = ZeusPaths::discover().map_err(|e| Failure::daemon(&e))?;
        let secret = IpcSecret::from_file_text(read_private(&paths.ipc_secret_file())?.expose())
            .map_err(|e| Failure::daemon(&e))?;
        let credential =
            ClientCredential::from_file_text(read_private(&paths.credential_file("gui"))?.expose())
                .map_err(|e| Failure::daemon(&e))?;
        let mut config = ClientConfig::new(ClientInfo {
            name: "zeus-web-bridge".into(),
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
            .map_err(|_| {
                Failure::new(
                    "bridge.daemon_unreachable",
                    "zeusd is not running or refused the connection.",
                )
            })?;
        if client.session().role != ClientRole::Gui
            || client.session().credential_kind != ClientKind::Agent
        {
            return Err(Failure::new(
                "bridge.identity_mismatch",
                "The bridge requires the restricted gui credential. Update the daemon installation.",
            ));
        }
        Ok(client)
    }

    /// Run `f` on the connection, reconnecting first if needed; any error drops the connection.
    async fn with<T, F, Fut>(&self, permission: Permission, f: F) -> Result<T, Failure>
    where
        F: FnOnce(IpcClient) -> Fut,
        Fut: std::future::Future<Output = Result<(T, IpcClient), (ApiError, IpcClient)>>,
    {
        let mut guard = self.connection.lock().await;
        let client = match guard.take() {
            Some(client) => client,
            None => Self::open().await?,
        };
        if !client.session().permissions.contains(&permission) {
            *guard = Some(client);
            return Err(Failure::new(
                "api.permission_denied",
                "The bridge credential does not have that permission.",
            ));
        }
        match f(client).await {
            Ok((value, client)) => {
                *guard = Some(client);
                Ok(value)
            }
            Err((error, client)) => {
                // Ordinary refusals keep the connection; transport-level failures drop it.
                let code = error.envelope().code.as_str().to_owned();
                let transport = matches!(
                    code.as_str(),
                    "api.unavailable"
                        | "api.timeout"
                        | "api.not_authenticated"
                        | "api.slow_consumer"
                        | "internal"
                ) || code.starts_with("internal");
                if !transport {
                    *guard = Some(client);
                }
                Err(Failure::daemon(&error))
            }
        }
    }

    /// Health and authority.
    pub async fn status(&self) -> Result<Status, Failure> {
        self.with(Permission::DaemonRead, |c| async move {
            match call::<DaemonStatus, _>(&c, StatusParams {}).await {
                Ok(s) => {
                    let status = Status {
                        daemon_version: s.daemon.version,
                        capabilities: c.hello().capabilities.iter().cloned().collect(),
                        permissions: c
                            .session()
                            .permissions
                            .iter()
                            .map(|p| p.as_str().to_owned())
                            .collect(),
                        head_seq: s.head_seq,
                        pending_approvals: s.pending_approvals,
                        kill_switch_engaged: s.kill_switch_engaged,
                        journal_ok: s.journal_ok,
                        uptime_ms: s.uptime_ms,
                    };
                    Ok((status, c))
                }
                Err(e) => Err((e, c)),
            }
        })
        .await
    }

    /// Every job summary visible to the bridge, up to `max_pages` pages of 100.
    pub async fn jobs(&self, max_pages: usize) -> Result<Vec<Value>, Failure> {
        self.with(Permission::JobsRead, |c| async move {
            let mut all = Vec::new();
            let mut cursor = None;
            for _ in 0..max_pages {
                match call::<JobsList, _>(
                    &c,
                    JobsListParams {
                        page: page(cursor.take()),
                        ..Default::default()
                    },
                )
                .await
                {
                    Ok(p) => {
                        let (items, next) = to_values(p);
                        all.extend(items);
                        match next {
                            Some(next) => cursor = Some(next),
                            None => break,
                        }
                    }
                    Err(e) => return Err((e, c)),
                }
            }
            Ok((all, c))
        })
        .await
    }

    /// Files under `dir` (absolute, inside a write root), recursively, partial files included. `None`: the default root.
    pub async fn files(
        &self,
        dir: Option<String>,
        max_pages: usize,
    ) -> Result<Vec<Value>, Failure> {
        self.with(Permission::FilesRead, |c| async move {
            let mut all = Vec::new();
            let mut cursor = None;
            for _ in 0..max_pages {
                let params = FilesListParams {
                    page: page(cursor.take()),
                    dir: dir.clone(),
                    recursive: true,
                    include_partial: true,
                };
                match call::<FilesList, _>(&c, params).await {
                    Ok(p) => {
                        let (items, next) = to_values(p);
                        all.extend(items);
                        match next {
                            Some(next) => cursor = Some(next),
                            None => break,
                        }
                    }
                    Err(e) => return Err((e, c)),
                }
            }
            Ok((all, c))
        })
        .await
    }

    /// Artifact records, newest first.
    pub async fn artifacts(&self, max_pages: usize) -> Result<Vec<Value>, Failure> {
        self.with(Permission::FilesRead, |c| async move {
            if !c
                .hello()
                .capabilities
                .contains(zeus_api::capabilities::ARTIFACTS)
            {
                return Ok((Vec::new(), c));
            }
            let mut all = Vec::new();
            let mut cursor = None;
            for _ in 0..max_pages {
                match call::<ArtifactsList, _>(
                    &c,
                    ArtifactsListParams {
                        page: page(cursor.take()),
                        ..Default::default()
                    },
                )
                .await
                {
                    Ok(p) => {
                        let (items, next) = to_values(p);
                        all.extend(items);
                        match next {
                            Some(next) => cursor = Some(next),
                            None => break,
                        }
                    }
                    Err(e) => return Err((e, c)),
                }
            }
            Ok((all, c))
        })
        .await
    }

    /// Approvals visible to this agent identity (the ones it caused), in the given states.
    pub async fn approvals(&self, states: &[ApprovalState]) -> Result<Vec<Value>, Failure> {
        let states = states.to_vec();
        self.with(Permission::ApprovalsRead, |c| async move {
            let mut all = Vec::new();
            for state in states {
                match call::<ApprovalsList, _>(
                    &c,
                    ApprovalsListParams {
                        page: page(None),
                        state: Some(state),
                        kind: None,
                    },
                )
                .await
                {
                    Ok(p) => all.extend(to_values(p).0),
                    Err(e) => return Err((e, c)),
                }
            }
            Ok((all, c))
        })
        .await
    }

    /// Workflow runs and their report counters.
    pub async fn runs(&self) -> Result<Vec<Value>, Failure> {
        self.with(Permission::WorkflowsPlan, |c| async move {
            if !c
                .hello()
                .capabilities
                .contains(zeus_api::capabilities::WORKFLOWS)
            {
                return Ok((Vec::new(), c));
            }
            match call::<WorkflowsList, _>(&c, WorkflowsListParams { page: page(None) }).await {
                Ok(p) => Ok((to_values(p).0, c)),
                Err(e) => Err((e, c)),
            }
        })
        .await
    }

    /// Enqueue (or, with `dry_run`, only resolve) a batch. Returns the daemon's result as JSON.
    pub async fn enqueue(
        &self,
        wanted: &[Wanted],
        dry_run: bool,
        key: Option<String>,
    ) -> Result<Value, Failure> {
        let jobs: Vec<NewJob> = wanted.iter().map(new_job).collect();
        self.with(Permission::JobsWrite, |c| async move {
            if !dry_run
                && !c
                    .hello()
                    .capabilities
                    .contains(zeus_api::capabilities::IDEMPOTENCY)
            {
                return Err((ApiError::new("api.capability_required", "idempotency"), c));
            }
            match call::<JobsEnqueue, _>(
                &c,
                EnqueueParams {
                    jobs,
                    dry_run,
                    idempotency_key: key,
                },
            )
            .await
            {
                Ok(r) => Ok((serde_json::to_value(r).unwrap_or(Value::Null), c)),
                Err(e) => Err((e, c)),
            }
        })
        .await
    }

    /// One control through the daemon's ordinary transition checks.
    pub async fn control(&self, job: JobId, action: Action, key: String) -> Result<Value, Failure> {
        self.with(Permission::JobsControl, |c| async move {
            let action = match action {
                Action::Pause => ControlAction::Pause,
                Action::Resume => ControlAction::Resume,
                Action::Cancel => ControlAction::Cancel,
                Action::Retry => ControlAction::Retry,
            };
            let params = JobsControlParams { job_id: job, action, priority: None, idempotency_key: Some(key) };
            match call::<JobsControl, _>(&c, params).await {
                Ok(r) => Ok((serde_json::json!({"replayed": r.replayed, "job_id": r.job.id, "state": r.job.state}), c)),
                Err(e) => Err((e, c)),
            }
        })
        .await
    }
}

fn new_job(w: &Wanted) -> NewJob {
    NewJob {
        url: w.url.clone(),
        hint_url: None,
        referrer: None,
        dest_dir: w.dest_dir.clone(),
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
    }
}
