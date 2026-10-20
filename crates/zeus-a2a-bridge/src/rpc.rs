use crate::{
    Config,
    config::bad,
    store::{Entry, Store, digest, missing},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;
use zeus_api::client::call;
use zeus_api::methods::jobs::{
    ControlAction, EnqueueParams, JobsControlParams, JobsGetParams, NewJob,
};
use zeus_api::methods::{JobsControl, JobsEnqueue, JobsGet};
use zeus_api::view::JobView;
use zeus_api::{ApiError, ApiResult, Client, ClientKind, ClientRole};
use zeus_types::{JobState, Timestamp};

/// An authenticated bridge and its durable index. The daemon remains the source of task status.
pub struct Bridge {
    pub(crate) config: Config,
    client: Arc<dyn Client>,
    store: Store,
    submit: Mutex<()>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    jsonrpc: String,
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Send {
    message: Message,
    #[serde(default)]
    configuration: Configuration,
    metadata: Metadata,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Message {
    message_id: String,
    role: String,
    parts: Vec<DataPart>,
    context_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DataPart {
    data: Fetch,
    #[serde(default, rename = "mediaType")]
    media_type: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fetch {
    skill: String,
    job: Value,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Configuration {
    #[serde(default)]
    return_immediately: bool,
    #[serde(default)]
    accepted_output_modes: Vec<String>,
    history_length: Option<u32>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Metadata {
    idempotency_key: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Get {
    id: String,
    history_length: Option<u32>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cancel {
    id: String,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct List {
    context_id: Option<String>,
    status: Option<zeus_a2a::TaskState>,
    page_size: Option<usize>,
    page_token: Option<String>,
    history_length: Option<u32>,
    #[serde(default)]
    include_artifacts: bool,
}

fn decode<T: serde::de::DeserializeOwned>(value: Value) -> ApiResult<T> {
    serde_json::from_value(value).map_err(|_| bad("Unsupported or malformed A2A request fields"))
}
fn valid_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 128
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}

impl Bridge {
    /// Open the index only after confirming the IPC credential has agent/A2A authority.
    pub fn open(config: Config, client: Arc<dyn Client>) -> ApiResult<Self> {
        config.validate()?;
        if client.session().kind != ClientKind::Agent || client.session().role != ClientRole::A2a {
            return Err(ApiError::new(
                "api.permission_denied",
                "A2A requires its dedicated agent credential",
            ));
        }
        let store = Store::open(&config.index_path)?;
        Ok(Self {
            config,
            client,
            store,
            submit: Mutex::new(()),
        })
    }

    /// Public card. Only the executable structured-data skill is advertised.
    pub fn card(&self) -> Value {
        json!({"name":"Zeus", "description":"Local downloads with human-controlled permissions and verification.",
            "version":env!("CARGO_PKG_VERSION"),
            "supportedInterfaces":[{"url":format!("http://{}/a2a",self.config.bind),"protocolBinding":"JSONRPC","protocolVersion":"1.0"}],
            "capabilities":{"streaming":false,"pushNotifications":false,"extendedAgentCard":false},
            "securitySchemes":{"zeusBearer":{"httpAuthSecurityScheme":{"scheme":"Bearer","bearerFormat":"opaque","description":"Dedicated local peer token"}}},
            "securityRequirements":[{"schemes":{"zeusBearer":{"list":[]}}}],
            "defaultInputModes":["application/json"],"defaultOutputModes":["application/json"],
            "skills":[{"id":"fetch_file","name":"Fetch a file","description":"One validated public HTTP(S) download. Supply one data part with skill fetch_file and a Zeus NewJob in job. Human permission remains outside A2A.","tags":["download","verify"],"inputModes":["application/json"],"outputModes":["application/json"]}]})
    }

    pub(crate) async fn rpc(&self, peer: &str, raw: Value) -> Value {
        let request: Request = match decode(raw) {
            Ok(r) => r,
            Err(_) => return rpc_error(Value::Null, -32600, "Invalid request"),
        };
        let id = request.id;
        if request.jsonrpc != "2.0" || !(id.is_i64() || id.as_str().is_some_and(|s| s.len() <= 128))
        {
            return rpc_error(Value::Null, -32600, "Invalid request");
        }
        let result = match request.method.as_str() {
            "SendMessage" => self.send(peer, request.params).await,
            "GetTask" => self.get(peer, request.params).await,
            "CancelTask" => self.cancel(peer, request.params).await,
            "ListTasks" => self.list(peer, request.params).await,
            "SendStreamingMessage"
            | "SubscribeToTask"
            | "GetExtendedAgentCard"
            | "CreateTaskPushNotificationConfig"
            | "GetTaskPushNotificationConfig"
            | "ListTaskPushNotificationConfigs"
            | "DeleteTaskPushNotificationConfig" => {
                return rpc_error(id, -32004, "Unsupported operation");
            }
            _ => return rpc_error(id, -32601, "Method not found"),
        };
        match result {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(error) => {
                // Backend messages may contain paths or remote diagnostic text. Never forward them.
                let (code, message) = match error.envelope().code.as_str() {
                    "api.not_found" | "api.permission_denied" => (-32001, "Task not found"),
                    "api.invalid_transition" => {
                        (-32002, "Task cannot be canceled in its current state")
                    }
                    "api.invalid_params" | "api.idempotency_conflict" => {
                        (-32602, "Invalid parameters or idempotency conflict")
                    }
                    "api.not_implemented" => (-32004, "Unsupported operation"),
                    _ => (
                        -32603,
                        "The daemon could not complete this operation; retry with the same message and idempotency key",
                    ),
                };
                rpc_error(id, code, message)
            }
        }
    }

    async fn send(&self, peer: &str, params: Value) -> ApiResult<Value> {
        let request: Send = decode(params)?;
        if request.message.role != "ROLE_USER"
            || !valid_id(&request.message.message_id)
            || !valid_id(&request.metadata.idempotency_key)
            || request.message.parts.len() != 1
            || request
                .configuration
                .history_length
                .is_some_and(|n| n > 100)
            || (!request.configuration.accepted_output_modes.is_empty()
                && !request
                    .configuration
                    .accepted_output_modes
                    .iter()
                    .any(|s| s == "application/json"))
        {
            return Err(bad("Invalid A2A structured submission"));
        }
        let part = &request.message.parts[0];
        if part.data.skill != "fetch_file"
            || part
                .media_type
                .as_ref()
                .is_some_and(|s| s != "application/json")
        {
            return Err(ApiError::not_implemented("A2A skill"));
        }
        let object = part
            .data
            .job
            .as_object()
            .ok_or_else(|| bad("A download object is required"))?;
        const FIELDS: &[&str] = &[
            "url",
            "dest_dir",
            "filename",
            "expected_sha256",
            "expected_size",
            "priority",
            "overwrite",
            "max_connections",
        ];
        if object.keys().any(|key| !FIELDS.contains(&key.as_str())) {
            return Err(bad(
                "Unsupported download fields; session access and authority cannot be supplied",
            ));
        }
        let job: NewJob = decode(part.data.job.clone())?;
        let normalized = serde_json::to_value(&job).map_err(|_| bad("Invalid download"))?;
        let context = if let Some(id) = &request.message.context_id {
            if !valid_id(id) || !self.store.list(peer)?.iter().any(|e| &e.context_id == id) {
                return Err(missing());
            }
            id.clone()
        } else {
            format!("ctx_{}", ulid::Ulid::generate())
        };
        let key = digest(&json!([
            "zeus.a2a.key.v1",
            peer,
            request.metadata.idempotency_key
        ]))?;
        let message_key = digest(&json!([
            "zeus.a2a.message.v1",
            peer,
            request.message.message_id
        ]))?;
        let request_hash = digest(&json!([
            part.data.skill,
            normalized,
            request.message.message_id,
            request.message.context_id
        ]))?;
        let mut entry;
        {
            let _serial = self.submit.lock().await;
            entry = self.store.reserve(Entry {
                task_id: format!("task_{}", ulid::Ulid::generate()),
                context_id: context,
                peer: peer.into(),
                request_hash,
                key,
                message_key,
                job_id: None,
                created_ms: Timestamp::now().unix_millis() as u64,
            })?;
            if entry.job_id.is_none() {
                let result = call::<JobsEnqueue, _>(
                    &*self.client,
                    EnqueueParams {
                        jobs: vec![job],
                        dry_run: false,
                        idempotency_key: Some(format!("a2a:{}", entry.key)),
                    },
                )
                .await?;
                let first = result
                    .jobs
                    .first()
                    .ok_or_else(|| ApiError::internal("Missing enqueue result"))?;
                let id = first
                    .job_id
                    .ok_or_else(|| ApiError::new("api.unavailable", "Download was not accepted"))?;
                self.store.attach(&mut entry, id)?;
            }
        }
        let mut task = self.task(&entry).await?;
        if !request.configuration.return_immediately {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
            while matches!(
                task["status"]["state"].as_str(),
                Some("TASK_STATE_SUBMITTED" | "TASK_STATE_WORKING")
            ) {
                if tokio::time::Instant::now() >= deadline {
                    return Err(ApiError::new(
                        "api.unavailable",
                        "Task still running; repeat the same message with returnImmediately",
                    ));
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
                task = self.task(&entry).await?;
            }
        }
        Ok(json!({"task":task}))
    }

    async fn get(&self, peer: &str, params: Value) -> ApiResult<Value> {
        let params: Get = decode(params)?;
        if params.history_length.is_some_and(|n| n > 100) {
            return Err(bad("Invalid history length"));
        }
        self.task(&self.store.get(peer, &params.id)?).await
    }
    async fn cancel(&self, peer: &str, params: Value) -> ApiResult<Value> {
        let params: Cancel = decode(params)?;
        let entry = self.store.get(peer, &params.id)?;
        let task = self.task(&entry).await?;
        if task["status"]["state"] == "TASK_STATE_CANCELED" {
            return Ok(task);
        }
        if matches!(
            task["status"]["state"].as_str(),
            Some("TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_REJECTED")
        ) {
            return Err(ApiError::new("api.invalid_transition", "Task is terminal"));
        }
        call::<JobsControl, _>(
            &*self.client,
            JobsControlParams {
                job_id: entry.job_id.ok_or_else(missing)?,
                action: ControlAction::Cancel,
                priority: None,
                idempotency_key: Some(format!("a2a-cancel:{}", entry.task_id)),
            },
        )
        .await?;
        self.task(&entry).await
    }
    async fn list(&self, peer: &str, params: Value) -> ApiResult<Value> {
        let params: List = decode(if params.is_null() { json!({}) } else { params })?;
        let size = params.page_size.unwrap_or(50);
        if !(1..=100).contains(&size) || params.history_length.is_some_and(|n| n > 100) {
            return Err(bad("Invalid task page"));
        }
        let filter = digest(&json!([
            peer,
            params.context_id,
            params.status,
            params.include_artifacts,
            size
        ]))?;
        let offset = if let Some(token) = params.page_token.filter(|s| !s.is_empty()) {
            let (binding, offset) = token
                .split_once(':')
                .ok_or_else(|| bad("Invalid task cursor"))?;
            if binding != filter {
                return Err(bad("Task cursor does not match this peer and filter"));
            }
            offset
                .parse::<usize>()
                .map_err(|_| bad("Invalid task cursor"))?
        } else {
            0
        };
        let mut tasks = Vec::new();
        for entry in self.store.list(peer)? {
            if entry.job_id.is_none()
                || params
                    .context_id
                    .as_ref()
                    .is_some_and(|c| c != &entry.context_id)
            {
                continue;
            }
            let mut task = self.task(&entry).await?;
            if params.status.is_some_and(|s| {
                serde_json::to_value(s).ok().as_ref() != Some(&task["status"]["state"])
            }) {
                continue;
            }
            if !params.include_artifacts {
                task.as_object_mut()
                    .expect("task object")
                    .remove("artifacts");
            }
            tasks.push(task);
        }
        let total = tasks.len();
        if offset > total {
            return Err(bad("Task cursor exceeds results"));
        }
        let end = offset.saturating_add(size).min(total);
        let next = if end < total {
            format!("{filter}:{end}")
        } else {
            String::new()
        };
        Ok(
            json!({"tasks":&tasks[offset..end],"nextPageToken":next,"pageSize":size,"totalSize":total}),
        )
    }
    async fn task(&self, entry: &Entry) -> ApiResult<Value> {
        let response = call::<JobsGet, _>(
            &*self.client,
            JobsGetParams {
                job_id: entry.job_id.ok_or_else(missing)?,
                reveal_urls: false,
            },
        )
        .await?;
        Ok(project(entry, &response.job))
    }
}

fn project(entry: &Entry, job: &JobView) -> Value {
    let state = zeus_a2a::task_state_for(job.state, job.error.as_ref().map(|e| e.class));
    let mut task = json!({"id":entry.task_id,"contextId":entry.context_id,"status":{"state":state,"timestamp":job.updated_at},"metadata":{"zeusProfile":"1","bytesSaved":job.progress.bytes_done,"quarantined":job.quarantined}});
    let note = match job.state {
        JobState::AwaitingApproval => Some(
            "A person must review this request in the Zeus terminal or browser extension. Agent messages cannot approve it.",
        ),
        JobState::AwaitingSession => Some(
            "Waiting for the user's browser; website credentials cannot be supplied through A2A.",
        ),
        JobState::Paused => Some("Paused. A person can resume the download in Zeus."),
        JobState::Failed => Some(
            "The download failed. Inspect it locally in Zeus for the diagnostic and next step.",
        ),
        _ => None,
    };
    if let Some(note) = note {
        task["status"]["message"] = json!({"messageId":format!("status:{}:{}",entry.task_id,job.updated_at.unix_millis()),"contextId":entry.context_id,"taskId":entry.task_id,"role":"ROLE_AGENT","parts":[{"text":note}]});
    }
    if let Some(id) = &job.approval_id {
        task["metadata"]["approvalId"] = json!(id);
    }
    if job.state == JobState::Completed
        && let Some(result) = &job.result
    {
        task["artifacts"] = json!([{"artifactId":format!("file:{}",job.id),"parts":[{"data":{"jobId":job.id,"sha256":result.sha256,"blake3":result.blake3,"sizeBytes":result.size_bytes,"verified":true,"quarantined":job.quarantined}}]}]);
    }
    task
}
pub(crate) fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
