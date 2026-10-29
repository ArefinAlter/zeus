//! Tool dispatch: from an MCP tool call to daemon API calls and back.
//!
//! [`ZeusTools`] is transport-agnostic (any [`zeus_api::Client`]), so it is unit-tested with a fake client and bound
//! to `rmcp` separately ([`crate::transport`]). Conventions (ARCHITECTURE section 6):
//!
//! * **Tool execution errors, not protocol errors, for bad input.** Invalid arguments and every daemon error come
//!   back as a tool result with `isError: true`, the 00 error envelope in `structuredContent`, a one-line
//!   `"<code>: <message>"` text and a recovery hint, so the model can correct itself. Only an unknown tool name is a
//!   protocol error.
//! * **Nothing here decides, grants or reveals.** The tools map to daemon methods an `agent` credential may
//!   call; the daemon enforces that again.
//! * **Long work is a job.** `zeus_wait` takes at most 60 seconds.

use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use zeus_api::methods::jobs::{
    ControlAction, EnqueueParams, JobsControlParams, JobsGetParams, JobsListParams,
    JobsVerifyParams, JobsWaitParams, NewJob, ProbeParams, WaitUntil,
};
use zeus_api::methods::sessions::{SessionsListParams, SessionsRequestParams};
use zeus_api::methods::{
    FilesList, JobsControl, JobsEnqueue, JobsGet, JobsList, JobsProbe, JobsVerify, JobsWait,
    RpcMethod, SessionsList, SessionsRequest,
};
use zeus_api::{ApiError, Client, PageRequest, call};
use zeus_types::job::GrantScopeHint;
use zeus_types::{ErrorEnvelope, Hex256, HttpUrl, JobId, Origin, Priority};

use crate::args::*;
use crate::output::*;

/// Longest `zeus_wait` timeout in seconds.
pub const MAX_WAIT_S: u32 = 60;
/// Most URLs in one `zeus_enqueue`.
pub const MAX_URLS: usize = 50;

/// The unknown-tool protocol error (JSON-RPC `-32602` in MCP).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown tool `{0}`")]
pub struct UnknownTool(pub String);

/// The result of a tool call, ready to become an MCP `CallToolResult`.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutcome {
    /// Success: `structuredContent`.
    Ok(Value),
    /// Failure that the model can act on: `isError: true`.
    Error {
        /// The 00 error envelope.
        envelope: ErrorEnvelope,
        /// Recovery advice.
        next: NextStep,
    },
}

impl ToolOutcome {
    /// `isError`.
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error { .. })
    }

    /// One-line text for `content[0].text` (00 section 9.1: `"<code>: <message>"` on errors).
    pub fn text(&self) -> String {
        match self {
            Self::Ok(value) => serde_json::to_string(value).unwrap_or_default(),
            Self::Error { envelope, .. } => format!("{}: {}", envelope.code, envelope.message),
        }
    }

    /// `structuredContent`: the output, or the envelope plus the hint.
    pub fn structured(&self) -> Value {
        match self {
            Self::Ok(value) => value.clone(),
            Self::Error { envelope, next } => {
                let mut value = serde_json::to_value(envelope).unwrap_or(Value::Null);
                if let Some(object) = value.as_object_mut() {
                    object.insert(
                        "next".into(),
                        serde_json::to_value(next).unwrap_or(Value::Null),
                    );
                }
                value
            }
        }
    }

    pub(crate) fn from_api(error: &ApiError) -> Self {
        let envelope = error.envelope().clone();
        let next = next_step_for_error(envelope.code.as_str(), envelope.class);
        Self::Error { envelope, next }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::from_api(&ApiError::invalid_params(message))
    }
}

type ToolResult<T> = Result<T, ToolOutcome>;

/// The tool implementation over a daemon client.
pub struct ZeusTools<C: Client> {
    client: Arc<C>,
}

impl<C: Client> ZeusTools<C> {
    /// Wrap a client (an `agent`-kind daemon connection).
    pub fn new(client: Arc<C>) -> Self {
        Self { client }
    }

    /// Run the tool `name` with JSON `arguments`.
    pub async fn call(&self, name: &str, arguments: Value) -> Result<ToolOutcome, UnknownTool> {
        let outcome = match name {
            "zeus_inbox" => {
                self.run(arguments, |a: crate::inbox::InboxArgs| {
                    crate::inbox::dispatch(self.client.as_ref(), a)
                })
                .await
            }
            "zeus_workflow" => {
                self.run(arguments, |a: crate::workspace::WorkflowArgs| {
                    crate::workspace::workflow(self.client.as_ref(), a)
                })
                .await
            }
            "zeus_files" => {
                self.run(arguments, |a: crate::workspace::FilesArgs| {
                    crate::workspace::files(self.client.as_ref(), a)
                })
                .await
            }
            "zeus_artifacts" => {
                self.run(arguments, |a: crate::workspace::ArtifactsArgs| {
                    crate::workspace::artifacts(self.client.as_ref(), a)
                })
                .await
            }
            "zeus_sources" => {
                self.run(arguments, |a: crate::sources::SourcesArgs| {
                    crate::sources::dispatch(self.client.as_ref(), a)
                })
                .await
            }
            "zeus_cloud" => {
                self.run(arguments, |a: crate::cloud::CloudArgs| {
                    crate::cloud::dispatch(self.client.as_ref(), a)
                })
                .await
            }
            "zeus_probe" => self.run(arguments, |a: ProbeArgs| self.probe(a)).await,
            "zeus_enqueue" => self.run(arguments, |a: EnqueueArgs| self.enqueue(a)).await,
            "zeus_status" => self.run(arguments, |a: StatusArgs| self.status(a)).await,
            "zeus_wait" => self.run(arguments, |a: WaitArgs| self.wait(a)).await,
            "zeus_control" => self.run(arguments, |a: ControlArgs| self.control(a)).await,
            "zeus_verify" => self.run(arguments, |a: VerifyArgs| self.verify(a)).await,
            "zeus_session_list" => {
                self.run(arguments, |a: SessionListArgs| self.session_list(a))
                    .await
            }
            "zeus_session_request" => {
                self.run(arguments, |a: SessionRequestArgs| self.session_request(a))
                    .await
            }
            "zeus_files_list" => {
                self.run(arguments, |a: FilesListArgs| self.files_list(a))
                    .await
            }
            other => return Err(UnknownTool(other.chars().take(64).collect())),
        };
        Ok(outcome)
    }

    async fn run<A, O, F, Fut>(&self, arguments: Value, f: F) -> ToolOutcome
    where
        A: DeserializeOwned,
        O: Serialize,
        F: FnOnce(A) -> Fut,
        Fut: std::future::Future<Output = ToolResult<O>>,
    {
        let arguments = if arguments.is_null() {
            Value::Object(Default::default())
        } else {
            arguments
        };
        let args: A = match serde_json::from_value(arguments) {
            Ok(args) => args,
            Err(error) => return ToolOutcome::invalid(argument_error(&error)),
        };
        match f(args).await {
            Ok(output) => ToolOutcome::Ok(serde_json::to_value(output).unwrap_or(Value::Null)),
            Err(outcome) => outcome,
        }
    }

    async fn rpc<M: RpcMethod>(&self, params: M::Params) -> ToolResult<M::Result> {
        call::<M, C>(&self.client, params)
            .await
            .map_err(|e| ToolOutcome::from_api(&e))
    }

    async fn probe(&self, args: ProbeArgs) -> ToolResult<ProbeOutput> {
        let params = ProbeParams {
            url: parse_url(&args.url)?,
            session_origin: parse_origin_opt(args.session_origin.as_deref())?,
        };
        let result = self.rpc::<JobsProbe>(params).await?;
        let probe = result.probe;
        Ok(ProbeOutput {
            status: probe.status,
            range_support: wire_name(&probe.range_support),
            size_bytes: probe.total_bytes,
            requires_session: probe.requires_session,
            interstitial_suspected: probe.interstitial_suspected,
            approval_needed: result.approval_needed,
            untrusted_content_type: probe.content_type,
            untrusted_filename: Some(probe.filename.value),
            etag: probe.validators.etag,
        })
    }

    async fn enqueue(&self, args: EnqueueArgs) -> ToolResult<EnqueueOutput> {
        check_key(&args.idempotency_key)?;
        if args.urls.is_empty() || args.urls.len() > MAX_URLS {
            return Err(ToolOutcome::invalid(format!(
                "urls must hold 1 to {MAX_URLS} entries"
            )));
        }
        if args.urls.len() > 1 && (args.filename.is_some() || args.expected_sha256.is_some()) {
            return Err(ToolOutcome::invalid(
                "filename and expected_sha256 need exactly one url",
            ));
        }
        let session_origin = parse_origin_opt(args.session_origin.as_deref())?;
        let expected_sha256 = args
            .expected_sha256
            .as_deref()
            .map(|h| {
                Hex256::from_hex(h)
                    .map_err(|_| ToolOutcome::invalid("expected_sha256 must be 64 hex digits"))
            })
            .transpose()?;
        let priority = args.priority.map(|p| match p {
            PriorityArg::Low => Priority::Low,
            PriorityArg::Normal => Priority::Normal,
            PriorityArg::High => Priority::High,
        });
        let mut jobs = Vec::with_capacity(args.urls.len());
        for url in &args.urls {
            jobs.push(NewJob {
                url: parse_url(url)?,
                hint_url: None,
                referrer: None,
                dest_dir: args.dest_dir.clone(),
                filename: args.filename.clone(),
                expected_sha256,
                expected_size: None,
                priority,
                session_origin: session_origin.clone(),
                grant_scope_hint: None,
                extra_headers: Default::default(),
                overwrite: None,
                max_connections: None,
                transport_hint: None,
                labels: Default::default(),
                run_id: None,
            });
        }
        let result = self
            .rpc::<JobsEnqueue>(EnqueueParams {
                jobs,
                dry_run: args.dry_run,
                idempotency_key: Some(args.idempotency_key),
            })
            .await?;
        Ok(EnqueueOutput {
            dry_run: result.dry_run,
            jobs: result
                .jobs
                .into_iter()
                .map(|j| EnqueuedEntry {
                    job_id: j.job_id.map(|id| id.to_string()),
                    state: j.state,
                    approval_id: j.approval_id.map(|id| id.to_string()),
                    deduplicated: result.replayed,
                    untrusted_planned_path: j.plan.and_then(|p| p.dest_path),
                    error_code: j.error.map(|e| e.code.to_string()),
                })
                .collect(),
            note: if result.dry_run {
                "Nothing was created. Repeat without dry_run to start."
            } else {
                "Started. Call zeus_wait with the job ids; entries with an approval_id wait for the user."
            },
        })
    }

    async fn status(&self, args: StatusArgs) -> ToolResult<JobsOutput> {
        if let Some(id) = &args.job_id {
            let job_id = parse_job_id(id)?;
            let result = self
                .rpc::<JobsGet>(JobsGetParams {
                    job_id,
                    reveal_urls: false,
                })
                .await?;
            return Ok(JobsOutput {
                jobs: vec![JobBrief::from_view(&result.job)],
                timed_out: None,
                cursor: None,
                all_settled: None,
            });
        }
        let limit = args.limit.unwrap_or(10).clamp(1, 50);
        let page = self
            .rpc::<JobsList>(JobsListParams {
                page: PageRequest {
                    limit: Some(limit),
                    cursor: None,
                },
                ..Default::default()
            })
            .await?;
        Ok(JobsOutput {
            jobs: page.items.iter().map(JobBrief::from_summary).collect(),
            timed_out: None,
            cursor: None,
            all_settled: None,
        })
    }

    async fn wait(&self, args: WaitArgs) -> ToolResult<JobsOutput> {
        if args.job_ids.is_empty() || args.job_ids.len() > 20 {
            return Err(ToolOutcome::invalid("job_ids must hold 1 to 20 entries"));
        }
        let job_ids = args
            .job_ids
            .iter()
            .map(|id| parse_job_id(id))
            .collect::<ToolResult<Vec<_>>>()?;
        let seconds = args.timeout_s.unwrap_or(30).clamp(1, MAX_WAIT_S);
        let since_seq = args
            .since
            .as_deref()
            .map(|cursor| {
                cursor
                    .strip_prefix("ev_")
                    .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                    .and_then(|s| s.parse::<u64>().ok())
                    .ok_or_else(|| {
                        ToolOutcome::invalid("since must be a durable ev_<number> cursor")
                    })
            })
            .transpose()?;
        let until = match args.until {
            WaitUntilArg::SettledOrBlocked => WaitUntil::SettledOrBlocked,
            WaitUntilArg::AllFinal => WaitUntil::AllTerminal,
            WaitUntilArg::AnyChange => WaitUntil::AnyChange,
        };
        let result = self
            .rpc::<JobsWait>(JobsWaitParams {
                job_ids,
                until,
                timeout_ms: u64::from(seconds) * 1000,
                since_seq,
                progress_bytes: None,
            })
            .await?;
        Ok(JobsOutput {
            jobs: result.jobs.iter().map(JobBrief::from_summary).collect(),
            timed_out: Some(result.timed_out),
            cursor: Some(format!("ev_{}", result.head_seq)),
            all_settled: Some(result.jobs.iter().all(|j| {
                matches!(
                    j.state,
                    zeus_types::JobState::Completed
                        | zeus_types::JobState::Failed
                        | zeus_types::JobState::Canceled
                )
            })),
        })
    }

    async fn control(&self, args: ControlArgs) -> ToolResult<ControlOutput> {
        check_key(&args.idempotency_key)?;
        let action = match args.action {
            ControlActionArg::Pause => ControlAction::Pause,
            ControlActionArg::Resume => ControlAction::Resume,
            ControlActionArg::Cancel => ControlAction::Cancel,
            ControlActionArg::Retry => ControlAction::Retry,
            ControlActionArg::RetryNow => ControlAction::RetryNow,
            ControlActionArg::Reprioritize => ControlAction::Reprioritize,
        };
        if (action == ControlAction::Reprioritize) != args.priority.is_some() {
            return Err(ToolOutcome::invalid(
                "priority is required for reprioritize and not allowed otherwise",
            ));
        }
        let priority = args.priority.map(|p| match p {
            PriorityArg::Low => Priority::Low,
            PriorityArg::Normal => Priority::Normal,
            PriorityArg::High => Priority::High,
        });
        let result = self
            .rpc::<JobsControl>(JobsControlParams {
                job_id: parse_job_id(&args.job_id)?,
                action,
                priority,
                idempotency_key: Some(args.idempotency_key),
            })
            .await?;
        Ok(ControlOutput {
            job: JobBrief::from_summary(&result.job),
        })
    }

    async fn verify(&self, args: VerifyArgs) -> ToolResult<VerifyOutput> {
        let expected = args
            .expected_sha256
            .as_deref()
            .map(|h| {
                Hex256::from_hex(h)
                    .map_err(|_| ToolOutcome::invalid("expected_sha256 must be 64 hex digits"))
            })
            .transpose()?;
        let result = self
            .rpc::<JobsVerify>(JobsVerifyParams {
                job_id: parse_job_id(&args.job_id)?,
                expected_sha256: expected,
            })
            .await?;
        Ok(VerifyOutput {
            sha256: result.sha256.to_hex(),
            size_bytes: result.size_bytes,
            matches_recorded: result.matches_recorded,
            matches_expected: result.matches_expected,
            untrusted_detected_type: result.detected_mime,
            quarantined: result.quarantined,
            quarantine_reasons: result.quarantine_reasons.iter().map(wire_name).collect(),
        })
    }

    async fn session_list(&self, args: SessionListArgs) -> ToolResult<SessionListOutput> {
        let page = self
            .rpc::<SessionsList>(SessionsListParams {
                page: PageRequest {
                    limit: Some(50),
                    cursor: None,
                },
                origin: parse_origin_opt(args.origin.as_deref())?,
                include_inactive: args.include_inactive,
            })
            .await?;
        Ok(SessionListOutput {
            grants: page
                .items
                .iter()
                .map(|g| GrantBrief {
                    grant_id: g.id.to_string(),
                    origin: g.origin.to_string(),
                    scope: wire_name(&g.scope),
                    state: wire_name(&g.state),
                    expires_at: g.expires_at.map(|t| t.to_rfc3339()),
                    uses: g.uses,
                })
                .collect(),
        })
    }

    async fn session_request(&self, args: SessionRequestArgs) -> ToolResult<SessionRequestOutput> {
        check_key(&args.idempotency_key)?;
        if args.reason.trim().is_empty() || args.reason.chars().count() > 280 {
            return Err(ToolOutcome::invalid("reason must be 1 to 280 characters"));
        }
        let scope = match args.scope {
            ScopeArg::Once => GrantScopeHint::Once,
            ScopeArg::Site => GrantScopeHint::Site,
            ScopeArg::Workflow => GrantScopeHint::Workflow,
        };
        let result = self
            .rpc::<SessionsRequest>(SessionsRequestParams {
                origin: parse_origin(&args.origin)?,
                scope,
                reason: args.reason,
                job_id: args.job_id.as_deref().map(parse_job_id).transpose()?,
                run_id: None,
                idempotency_key: Some(args.idempotency_key),
            })
            .await?;
        let status = wire_name(&result.status);
        let note = match status.as_str() {
            "already_granted" => "A grant already covers this site. Go ahead with zeus_enqueue.",
            "pending_user_approval" => {
                "A person must approve this in their browser or terminal. You cannot. Check with zeus_status or zeus_wait."
            }
            _ => {
                "The user has blocked this site. Do not ask again; use open_in_browser guidance instead."
            }
        };
        Ok(SessionRequestOutput {
            status,
            grant_id: result.grant_id.map(|g| g.to_string()),
            approval_id: result.approval_id.map(|a| a.to_string()),
            expires_at: result.expires_at.map(|t| t.to_rfc3339()),
            note,
        })
    }

    async fn files_list(&self, args: FilesListArgs) -> ToolResult<FilesOutput> {
        let limit = args.limit.unwrap_or(50).clamp(1, 100);
        let page = self
            .rpc::<FilesList>(zeus_api::methods::files::FilesListParams {
                page: PageRequest {
                    limit: Some(limit),
                    cursor: None,
                },
                dir: args.dir,
                recursive: args.recursive,
                include_partial: false,
            })
            .await?;
        Ok(FilesOutput {
            truncated: page.next_cursor.is_some(),
            files: page
                .items
                .into_iter()
                .map(|f| FileBrief {
                    untrusted_path: f.path,
                    size_bytes: f.size_bytes,
                    sha256: f.sha256.map(|h| h.to_hex()),
                    quarantined: f.quarantined,
                    job_id: f.job_id.map(|j| j.to_string()),
                    untrusted_source: f.source_url_redacted.map(|u| u.to_string()),
                })
                .collect(),
        })
    }
}

/// The snake_case wire spelling of a serde enum value.
fn wire_name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn argument_error(error: &serde_json::Error) -> String {
    // serde messages for values can echo the offending value; only field-name messages are passed on.
    let text = error.to_string();
    if text.starts_with("missing field") || text.starts_with("unknown field") {
        text.split(" at line")
            .next()
            .unwrap_or("invalid arguments")
            .to_string()
    } else {
        "arguments do not match the tool's input schema".to_string()
    }
}

fn check_key(key: &str) -> ToolResult<()> {
    if key.is_empty() || key.chars().count() > 128 {
        Err(ToolOutcome::invalid(
            "idempotency_key must be 1 to 128 characters",
        ))
    } else {
        Ok(())
    }
}

fn parse_url(text: &str) -> ToolResult<HttpUrl> {
    HttpUrl::parse(text).map_err(|_| ToolOutcome::invalid("url must be an absolute http(s) URL"))
}

fn parse_origin(text: &str) -> ToolResult<Origin> {
    Origin::parse(text).map_err(|_| {
        ToolOutcome::invalid(
            "origin must look like https://host (no path, query or trailing slash)",
        )
    })
}

fn parse_origin_opt(text: Option<&str>) -> ToolResult<Option<Origin>> {
    text.map(parse_origin).transpose()
}

fn parse_job_id(text: &str) -> ToolResult<JobId> {
    text.parse::<JobId>()
        .map_err(|_| ToolOutcome::invalid("job_id must look like job_<26 characters>"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::json;
    use tokio::sync::broadcast;
    use zeus_api::ApiResult;
    use zeus_api::error::codes;
    use zeus_api::jsonrpc::Notification;
    use zeus_api::kind::{ClientKind, ClientRole};
    use zeus_api::methods::daemon::{AuthenticateResult, ConnectionLimits};

    /// Records calls and answers from a script keyed by method name.
    struct Fake {
        calls: Mutex<Vec<(String, Value)>>,
        answers: Mutex<std::collections::HashMap<String, ApiResult<Value>>>,
        session: AuthenticateResult,
        tx: broadcast::Sender<Notification>,
    }

    impl Fake {
        fn new() -> Arc<Self> {
            let (tx, _) = broadcast::channel(4);
            Arc::new(Self {
                calls: Mutex::default(),
                answers: Mutex::default(),
                session: AuthenticateResult {
                    client_id: "cli_x".into(),
                    kind: ClientKind::Agent,
                    credential_kind: ClientKind::Agent,
                    downgraded: false,
                    role: ClientRole::Mcp,
                    permissions: vec![],
                    limits: ConnectionLimits::STANDARD,
                    connection_id: zeus_types::IdGenerator::new().connection_id(),
                },
                tx,
            })
        }

        fn answer(&self, method: &str, result: ApiResult<Value>) {
            self.answers.lock().unwrap().insert(method.into(), result);
        }

        fn last(&self) -> (String, Value) {
            self.calls
                .lock()
                .unwrap()
                .last()
                .cloned()
                .expect("a call was made")
        }
    }

    #[async_trait]
    impl Client for Fake {
        async fn call_raw(&self, method: &str, params: Value) -> ApiResult<Value> {
            self.calls.lock().unwrap().push((method.into(), params));
            self.answers
                .lock()
                .unwrap()
                .get(method)
                .cloned()
                .unwrap_or_else(|| Err(ApiError::not_implemented("fake")))
        }

        fn notifications(&self) -> broadcast::Receiver<Notification> {
            self.tx.subscribe()
        }

        fn session(&self) -> &AuthenticateResult {
            &self.session
        }
    }

    const JOB: &str = "job_01M45NHJ2AHWMXT8JVJ7BEEJX7";
    const KEY: &str = "intent-1";

    #[tokio::test]
    async fn maintenance_rejects_approval_and_arbitrary_rpc_before_dispatch() {
        let fake = Fake::new();
        let tools = ZeusTools::new(Arc::clone(&fake));
        for request in [
            json!({"action":"maintenance","phase":"approve","request":{}}),
            json!({"action":"maintenance","phase":"apply","request":{"method":"approvals.decide","approved":true}}),
            json!({"action":"maintenance","phase":"preview","request":{"operation":{"kind":"import","path":"x","grant":true}}}),
            json!({"action":"maintenance","phase":"events","request":{"after_seq":0,"secret":"ZEUS-CANARY-injected"}}),
        ] {
            let result = tools
                .call(
                    "zeus_artifacts",
                    json!({"idempotency_key":KEY,"request":request}),
                )
                .await
                .unwrap();
            assert!(result.is_error());
            assert!(!result.text().contains("ZEUS-CANARY-"));
        }
        assert!(fake.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn workspace_requests_reject_authority_fields_and_invalid_limits_before_rpc() {
        let fake = Fake::new();
        let tools = ZeusTools::new(Arc::clone(&fake));
        for (name, request) in [
            ("zeus_workflow", json!({"action":"decide","approved":true})),
            (
                "zeus_workflow",
                json!({"action":"plan","goal":"x","start_urls":["https://example.org"],"draft":{}}),
            ),
            ("zeus_workflow", json!({"action":"list","limit":0})),
            (
                "zeus_files",
                json!({"action":"organize","dir":"x","rules":{},"approval_id":"ap_x"}),
            ),
            ("zeus_files", json!({"action":"convert","inputs":["x"]})),
            (
                "zeus_artifacts",
                json!({"action":"delete","artifact_id":"art_x"}),
            ),
            ("zeus_artifacts", json!({"action":"list","limit":101})),
            (
                "zeus_artifacts",
                json!({"action":"get","artifact_id":"ZEUS-CANARY-invalid"}),
            ),
        ] {
            let result = tools
                .call(name, json!({"idempotency_key":KEY,"request":request}))
                .await
                .unwrap();
            assert!(result.is_error(), "{name}: {}", result.text());
            assert!(!result.text().contains("ZEUS-CANARY-"));
        }
        assert!(fake.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn workspace_mutations_forward_retry_keys_and_never_decide() {
        let fake = Fake::new();
        let tools = ZeusTools::new(Arc::clone(&fake));
        fake.answer(
            "workflows.run",
            Ok(json!({
                "run_id":"run_01M45NHJ2AHWMXT8JVJ7BEEJX7","status":"awaiting_approval",
                "approval_id":"ap_01M45NHJ2AHWMXT8JVJ7BEEJX7"
            })),
        );
        let hash = "a".repeat(64);
        let result = tools
            .call(
                "zeus_workflow",
                json!({
                    "idempotency_key":KEY,"request":{"action":"run",
                        "plan_id":"plan_01M45NHJ2AHWMXT8JVJ7BEEJX7","plan_hash":hash}
                }),
            )
            .await
            .unwrap();
        assert!(!result.is_error(), "{}", result.text());
        assert_eq!(result.structured()["next"]["action"], "ask_user");
        assert_eq!(fake.last().0, "workflows.run");
        assert_eq!(fake.last().1["idempotency_key"], KEY);
        assert_eq!(fake.last().1["plan_hash"], hash);
        fake.answer("files.organize", Ok(json!({"steps":[],"applied":false})));
        let result = tools.call("zeus_files", json!({
            "idempotency_key":KEY,"request":{"action":"organize","dir":"x","rules":{"rules":[]}}
        })).await.unwrap();
        assert!(!result.is_error(), "{}", result.text());
        assert_eq!(fake.last().1["dry_run"], true);
        assert_eq!(fake.last().1["idempotency_key"], KEY);
        assert_eq!(result.structured()["next"]["action"], "review_plan");
        assert_eq!(fake.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn plan_control_and_manifest_keys_survive_typed_daemon_parameters() {
        let fake = Fake::new();
        let tools = ZeusTools::new(Arc::clone(&fake));
        for (method, request) in [
            (
                "workflows.plan",
                json!({"action":"plan","goal":"Files","start_urls":["https://example.org/"]}),
            ),
            (
                "workflows.control",
                json!({"action":"control","run_id":"run_01M45NHJ2AHWMXT8JVJ7BEEJX7","operation":"pause"}),
            ),
            (
                "workflows.manifest",
                json!({"action":"manifest","run_id":"run_01M45NHJ2AHWMXT8JVJ7BEEJX7","format":"jsonl"}),
            ),
        ] {
            fake.answer(method, Err(ApiError::not_implemented("fixture")));
            tools
                .call(
                    "zeus_workflow",
                    json!({"idempotency_key":KEY,"request":request}),
                )
                .await
                .unwrap();
            let (called, params) = fake.last();
            assert_eq!(called, method);
            let key =
                match method {
                    "workflows.plan" => {
                        serde_json::from_value::<zeus_api::methods::workflows::WorkflowsPlanParams>(
                            params,
                        )
                        .unwrap()
                        .idempotency_key
                    }
                    "workflows.control" => serde_json::from_value::<
                        zeus_api::methods::workflows::WorkflowsControlParams,
                    >(params)
                    .unwrap()
                    .idempotency_key,
                    _ => {
                        serde_json::from_value::<
                            zeus_api::methods::workflows::WorkflowsManifestParams,
                        >(params)
                        .unwrap()
                        .idempotency_key
                    }
                };
            assert_eq!(key.as_deref(), Some(KEY));
        }
    }

    #[tokio::test]
    async fn browser_workflow_mode_is_explicit_and_pending_review_cannot_create_authority() {
        let fake = Fake::new();
        let approval = zeus_types::IdGenerator::new().approval_id().to_string();
        fake.answer(
            "workflows.plan",
            Ok(json!({"plan_id":"plan_01M45NHJ2AHWMXT8JVJ7BEEJX7",
            "browser_review":approval,"samples":[],"plan_schema":{},"diagnostics":[]})),
        );
        let tools = ZeusTools::new(Arc::clone(&fake));
        for origin in [None, Some("https://example.org")] {
            let result = tools.call("zeus_workflow", json!({"idempotency_key":KEY,
                "request":{"action":"plan","goal":"read documents","start_urls":["https://example.org/list"],"browser_origin":origin}})).await.unwrap();
            assert!(!result.is_error(), "{}", result.text());
            assert_eq!(fake.last().1["session_origin"], json!(origin));
            assert_eq!(
                fake.last().1["browser_document_mode"],
                json!(origin.map(|_| "browser_origin"))
            );
            assert_eq!(
                result.structured()["untrusted_data"]["browser_review"],
                approval
            );
        }
        let result = tools.call("zeus_workflow", json!({"idempotency_key":KEY,
            "request":{"action":"plan","goal":"read documents","start_urls":["https://example.org/list"],"browser_origin":"https://example.org","profile_id":"a".repeat(32),"approved":true}})).await.unwrap();
        assert!(result.is_error());
        assert_eq!(fake.calls.lock().unwrap().len(), 2);
        assert!(
            fake.calls
                .lock()
                .unwrap()
                .iter()
                .all(|(method, _)| method == "workflows.plan")
        );
    }

    #[tokio::test]
    async fn workflow_samples_are_bounded_and_schema_is_available_on_demand() {
        let fake = Fake::new();
        fake.answer(
            "workflows.plan",
            Ok(json!({
                "plan_id":"plan_01M45NHJ2AHWMXT8JVJ7BEEJX7",
                "samples":[{"url_redacted":"https://example.org/","body":"文".repeat(100_000)}],
                "plan_schema":schemars::schema_for!(zeus_workflow::Plan),
                "diagnostics":[]
            })),
        );
        let result = ZeusTools::new(Arc::clone(&fake)).call("zeus_workflow", json!({
            "idempotency_key":KEY,"request":{"action":"plan","goal":"get files","start_urls":["https://example.org"]}
        })).await.unwrap();
        assert!(!result.is_error(), "{}", result.text());
        let value = result.structured();
        assert_eq!(value["truncated"], true);
        assert!(value["untrusted_data"].get("plan_schema").is_none());
        assert_eq!(
            value["untrusted_data"]["plan_schema_resource"],
            "zeus://workflow/plan-schema"
        );
        assert!(
            value["untrusted_data"]["samples"][0]["body"]
                .as_str()
                .unwrap()
                .len()
                <= 2048
        );
        assert_eq!(fake.last().1["idempotency_key"], KEY);
        let schema =
            crate::resources::read_resource(&fake, &crate::resources::ResourceUri::PlanSchema)
                .await
                .unwrap();
        assert!(
            jsonschema::validator_for(&serde_json::from_str::<Value>(&schema).unwrap()).is_ok()
        );
    }

    #[tokio::test]
    async fn artifact_pagination_and_ingestion_preserve_public_receipts() {
        let fake = Fake::new();
        fake.answer(
            "artifacts.list",
            Ok(json!({"items":[{"name":"ZEUS-CANARY-untrusted"}],"next_cursor":"page2"})),
        );
        fake.answer(
            "artifacts.ingest",
            Ok(json!({"artifact":{"name":"data"},"created":true,"replayed":false})),
        );
        let tools = ZeusTools::new(Arc::clone(&fake));
        let result = tools
            .call(
                "zeus_artifacts",
                json!({
                    "idempotency_key":KEY,"request":{"action":"list","limit":7,"cursor":"page1"}
                }),
            )
            .await
            .unwrap();
        assert!(!result.is_error(), "{}", result.text());
        assert_eq!(fake.last().1["cursor"], "page1");
        assert!(fake.last().1.get("idempotency_key").is_none());
        assert_eq!(
            result.structured()["untrusted_data"]["next_cursor"],
            "page2"
        );
        assert_eq!(result.structured()["next"]["action"], "none");
        let result = tools.call("zeus_artifacts", json!({
            "idempotency_key":KEY,"request":{"action":"ingest","path":"/root/data","name":"data"}
        })).await.unwrap();
        assert!(!result.is_error(), "{}", result.text());
        assert_eq!(fake.last().0, "artifacts.ingest");
        assert_eq!(fake.last().1["idempotency_key"], KEY);
    }

    #[tokio::test]
    async fn artifact_lifecycle_preserves_exact_preview_and_cannot_supply_approval_decisions() {
        let fake = Fake::new();
        let ids = zeus_types::IdGenerator::new();
        let artifact = ids.artifact_id();
        let preview = json!({"plan_hash":"a".repeat(64),"plan":{
            "version":1,"artifact_id":artifact,"expected_record_blake3":"b".repeat(64),
            "blake3":"c".repeat(64),"sha256":"d".repeat(64),"size_bytes":7,
            "quarantined":false,"current_retention":"keep","current_retain_until":null,
            "operation":{"kind":"delete"},"requires_approval":true,
            "expires_at":zeus_types::Timestamp::now().saturating_add_ms(60_000)
        }});
        fake.answer("artifacts.lifecycle.preview", Ok(preview.clone()));
        fake.answer(
            "artifacts.lifecycle.apply",
            Ok(json!({"applied":false,"approval_id":ids.approval_id(),"replayed":false})),
        );
        let tools = ZeusTools::new(Arc::clone(&fake));
        let result=tools.call("zeus_artifacts",json!({"idempotency_key":KEY,"request":{
            "action":"lifecycle_preview","artifact_id":artifact,"operation":{"kind":"delete"}
        }})).await.unwrap();
        assert!(!result.is_error(), "{}", result.text());
        assert_eq!(result.structured()["next"]["action"], "review_plan");
        assert_eq!(result.structured()["untrusted_data"], preview);
        let result = tools
            .call(
                "zeus_artifacts",
                json!({"idempotency_key":KEY,"request":{
                    "action":"lifecycle_apply","preview":preview
                }}),
            )
            .await
            .unwrap();
        assert!(!result.is_error(), "{}", result.text());
        assert_eq!(result.structured()["next"]["action"], "ask_user");
        let (method, params) = fake.last();
        assert_eq!(method, "artifacts.lifecycle.apply");
        assert_eq!(params["idempotency_key"], KEY);
        assert_eq!(params["plan_hash"], preview["plan_hash"]);
        assert_eq!(params["plan"], preview["plan"]);
        let count = fake.calls.lock().unwrap().len();
        for key in ["approved", "idempotency_key", "decision"] {
            let mut altered = preview.clone();
            altered[key] = json!("MCP-CANARY-forged");
            let result = tools
                .call(
                    "zeus_artifacts",
                    json!({"idempotency_key":KEY,"request":{
                        "action":"lifecycle_apply","preview":altered
                    }}),
                )
                .await
                .unwrap();
            assert!(result.is_error());
            assert!(!result.text().contains("MCP-CANARY-forged"));
        }
        assert_eq!(fake.calls.lock().unwrap().len(), count);
        assert!(
            !fake
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|(method, _)| method == "approvals.decide")
        );
    }

    #[tokio::test]
    async fn inbox_preserves_owner_boundary_reviews_and_exact_retry_keys() {
        use zeus_api::methods::inbox::{
            InboxBatchId, InboxEntryId, InboxPackageId, InboxPreviewId,
        };
        let fake = Fake::new();
        let ids = zeus_types::IdGenerator::new();
        let package = InboxPackageId::from_job_id(ids.job_id());
        let preview = InboxPreviewId::from_job_id(ids.job_id());
        let batch = InboxBatchId::from_job_id(ids.job_id());
        let entry = InboxEntryId::from_job_id(ids.job_id());
        fake.answer(
            "inbox.packages.list",
            Ok(json!({"items":[],"next_cursor":"owner-page2"})),
        );
        fake.answer("inbox.preview",Ok(json!({"preview_id":preview,"package_id":package,"package_revision":4,"expires_at":zeus_types::Timestamp::now().saturating_add_ms(60_000),"entries":[],"ready_entries":0,"remaining_pending":75,"replayed":false})));
        fake.answer("inbox.submit",Ok(json!({"batch_id":batch,"package_id":package,"package_revision":5,"entries":[{"entry_id":entry,"result":{"job_id":ids.job_id(),"state":"awaiting_approval","approval_id":ids.approval_id()}}],"remaining_pending":74,"replayed":false})));
        let tools = ZeusTools::new(fake.clone());
        let result = tools
            .call(
                "zeus_inbox",
                json!({"idempotency_key":KEY,"request":{"action":"packages_list"}}),
            )
            .await
            .unwrap();
        assert!(!result.is_error(), "{}", result.text());
        assert_eq!(fake.last().1["limit"], 10);
        assert!(fake.last().1.get("owner_credential_id").is_none());
        assert!(fake.last().1.get("idempotency_key").is_none());
        assert_eq!(
            result.structured()["untrusted_data"]["next_cursor"],
            "owner-page2"
        );
        let result=tools.call("zeus_inbox",json!({"idempotency_key":KEY,"request":{"action":"preview","parameters":{"package_id":package,"expected_revision":4}}})).await.unwrap();
        assert!(!result.is_error(), "{}", result.text());
        assert_eq!(result.structured()["next"]["action"], "review_plan");
        assert_eq!(fake.last().1["idempotency_key"], KEY);
        let result=tools.call("zeus_inbox",json!({"idempotency_key":KEY,"request":{"action":"submit","parameters":{"package_id":package,"expected_revision":4,"preview_id":preview}}})).await.unwrap();
        assert!(!result.is_error(), "{}", result.text());
        assert_eq!(result.structured()["next"]["action"], "ask_user");
        assert_eq!(fake.last().1["preview_id"], json!(preview));
        let before = fake.calls.lock().unwrap().len();
        for request in [
            json!({"action":"packages_list","parameters":{"owner_credential_id":null}}),
            json!({"action":"packages_list","parameters":{"owner_credential_id":"cli_MCP-CANARY-other"}}),
            json!({"action":"packages_list","parameters":{"idempotency_key":"MCP-CANARY-inner"}}),
            json!({"action":"submit","parameters":{"package_id":package,"expected_revision":4,"preview_id":preview,"approved":true}}),
            json!({"action":"decide","parameters":{}}),
        ] {
            let result = tools
                .call(
                    "zeus_inbox",
                    json!({"idempotency_key":KEY,"request":request}),
                )
                .await
                .unwrap();
            assert!(result.is_error());
            assert!(!result.text().contains("MCP-CANARY"));
        }
        assert_eq!(fake.calls.lock().unwrap().len(), before);
        let resource = crate::inbox::read(fake.as_ref(), &crate::inbox::InboxResource::Schema)
            .await
            .unwrap();
        assert!(resource.contains("package_put"));
        assert!(!resource.contains("owner_credential_id"));
        assert_eq!(fake.calls.lock().unwrap().len(), before);
    }

    #[tokio::test]
    async fn source_tool_preserves_review_identity_and_cannot_configure_credentials() {
        let fake = Fake::new();
        fake.answer("sources.accounts.list", Ok(json!({"accounts":[]})));
        let tools = ZeusTools::new(Arc::clone(&fake));
        let listed = tools
            .call(
                "zeus_sources",
                json!({"idempotency_key":"read","request":{"action":"accounts"}}),
            )
            .await
            .unwrap();
        assert!(!listed.is_error());
        assert_eq!(fake.last().0, "sources.accounts.list");
        let count = fake.calls.lock().unwrap().len();
        for request in [
            json!({"action":"configure","credential":"SOURCE-CANARY"}),
            json!({"action":"revoke","account_id":"hub"}),
            json!({"action":"accounts","owner":"another"}),
        ] {
            let result = tools
                .call(
                    "zeus_sources",
                    json!({"idempotency_key":"intent","request":request}),
                )
                .await
                .unwrap();
            assert!(result.is_error());
            assert!(!result.text().contains("SOURCE-CANARY"));
        }
        assert_eq!(fake.calls.lock().unwrap().len(), count);
        let result=tools.call("zeus_sources",json!({"idempotency_key":"source-intent","request":{"action":"submit","plan_id":"srcplan_fixture","filename":"file.bin","max_connections":2}})).await.unwrap();
        assert!(
            result.is_error(),
            "a failing backend must not claim publication"
        );
        let (method, params) = fake.last();
        assert_eq!(method, "sources.submit");
        assert_eq!(params["plan_id"], "srcplan_fixture");
        assert_eq!(params["idempotency_key"], "source-intent");
    }

    #[tokio::test]
    async fn cloud_dispatch_is_bounded_and_never_maps_to_human_methods() {
        let fake = Fake::new();
        fake.answer("cloud.accounts.list", Ok(json!({"accounts":[]})));
        let tools = ZeusTools::new(Arc::clone(&fake));
        let result = tools
            .call(
                "zeus_cloud",
                json!({"idempotency_key":"read-1","request":{"action":"accounts"}}),
            )
            .await
            .unwrap();
        assert!(!result.is_error(), "{}", result.text());
        assert_eq!(fake.last().0, "cloud.accounts.list");
        assert_eq!(result.structured()["untrusted_data"]["accounts"], json!([]));
        let count = fake.calls.lock().unwrap().len();
        for request in [
            json!({"action":"configure","credential":"CANARY-secret"}),
            json!({"action":"control","delivery_id":"cld_example","operation":"delete","args":["--force"]}),
            json!({"action":"accounts","secret":"CANARY-secret"}),
        ] {
            let result = tools
                .call(
                    "zeus_cloud",
                    json!({"idempotency_key":"intent","request":request}),
                )
                .await
                .unwrap();
            assert!(result.is_error());
            assert!(!result.text().contains("CANARY-secret"));
        }
        assert_eq!(fake.calls.lock().unwrap().len(), count);
    }

    #[tokio::test]
    async fn cloud_submit_preserves_plan_binding_and_retry_key() {
        let fake = Fake::new();
        let tools = ZeusTools::new(Arc::clone(&fake));
        let hash = "f".repeat(64);
        let outcome=tools.call("zeus_cloud",json!({"idempotency_key":"cloud-intent","request":{"action":"submit","plan_id":"clp_example","plan_hash":hash}})).await.unwrap();
        assert!(
            outcome.is_error(),
            "Fake backend must not claim a delivery was created"
        );
        let (method, params) = fake.last();
        assert_eq!(method, "cloud.submit");
        assert_eq!(params["plan_hash"], hash);
        assert_eq!(params["idempotency_key"], "cloud-intent");
        assert_eq!(fake.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unknown_tools_are_protocol_errors_and_bad_arguments_are_tool_errors() {
        let tools = ZeusTools::new(Fake::new());
        assert!(tools.call("zeus_nope", json!({})).await.is_err());
        let outcome = tools
            .call("zeus_enqueue", json!({ "urls": ["https://a.example/x"] }))
            .await
            .unwrap();
        assert!(outcome.is_error());
        assert!(outcome.text().starts_with("api.invalid_params: "));
        assert!(outcome.text().contains("idempotency_key"));
        let outcome = tools
            .call("zeus_wait", json!({ "job_ids": ["ZEUS-CANARY-bad"] }))
            .await
            .unwrap();
        assert!(!outcome.text().contains("ZEUS-CANARY"));
    }

    #[tokio::test]
    async fn enqueue_maps_arguments_and_never_sends_a_source() {
        let fake = Fake::new();
        fake.answer(
            "jobs.enqueue",
            Ok(json!({ "dry_run": false, "jobs": [{ "job_id": JOB, "state": "queued", "deduplicated": false }] })),
        );
        let tools = ZeusTools::new(Arc::clone(&fake));
        let outcome = tools
            .call("zeus_enqueue", json!({ "urls": ["https://files.example.com/a.zip"], "idempotency_key": KEY, "priority": "high" }))
            .await
            .unwrap();
        assert!(!outcome.is_error(), "{}", outcome.text());
        let (method, params) = fake.last();
        assert_eq!(method, "jobs.enqueue");
        assert_eq!(params["idempotency_key"], KEY);
        assert_eq!(params["jobs"][0]["priority"], "high");
        assert!(params["jobs"][0].get("source").is_none());
        let structured = outcome.structured();
        assert_eq!(structured["jobs"][0]["job_id"], JOB);
    }

    #[tokio::test]
    async fn enqueue_validates_before_calling_the_daemon() {
        let fake = Fake::new();
        let tools = ZeusTools::new(Arc::clone(&fake));
        for bad in [
            json!({ "urls": [], "idempotency_key": KEY }),
            json!({ "urls": ["ftp://x/y"], "idempotency_key": KEY }),
            json!({ "urls": ["https://a/1", "https://a/2"], "filename": "x", "idempotency_key": KEY }),
            json!({ "urls": ["https://a/1"], "expected_sha256": "nothex", "idempotency_key": KEY }),
            json!({ "urls": ["https://a/1"], "idempotency_key": "" }),
        ] {
            assert!(tools.call("zeus_enqueue", bad).await.unwrap().is_error());
        }
        assert!(
            fake.calls.lock().unwrap().is_empty(),
            "invalid input must not reach the daemon"
        );
    }

    #[tokio::test]
    async fn wait_clamps_the_timeout_to_sixty_seconds() {
        let fake = Fake::new();
        fake.answer(
            "jobs.wait",
            Ok(json!({ "jobs": [], "timed_out": true, "head_seq": 0 })),
        );
        let tools = ZeusTools::new(Arc::clone(&fake));
        let outcome = tools
            .call("zeus_wait", json!({ "job_ids": [JOB], "timeout_s": 3600 }))
            .await
            .unwrap();
        assert!(!outcome.is_error(), "{}", outcome.text());
        assert_eq!(fake.last().1["timeout_ms"], 60_000);
        assert_eq!(outcome.structured()["timed_out"], true);
        tools
            .call("zeus_wait", json!({ "job_ids": [JOB], "timeout_s": 0 }))
            .await
            .unwrap();
        assert_eq!(fake.last().1["timeout_ms"], 1000);
    }

    #[tokio::test]
    async fn wait_preserves_the_durable_cursor_and_rejects_malformed_cursors() {
        let fake = Fake::new();
        fake.answer(
            "jobs.wait",
            Ok(json!({"jobs":[],"timed_out":true,"head_seq":43})),
        );
        let tools = ZeusTools::new(Arc::clone(&fake));
        let result = tools
            .call(
                "zeus_wait",
                json!({"job_ids":[JOB],"since":"ev_42","until":"any_change"}),
            )
            .await
            .unwrap();
        assert!(!result.is_error(), "{}", result.text());
        assert_eq!(fake.last().1["since_seq"], 42);
        assert_eq!(fake.last().1["until"], "any_change");
        assert_eq!(result.structured()["cursor"], "ev_43");
        for cursor in ["42", "ev_", "ev_-1", "ev_1.0", "ev_18446744073709551616"] {
            assert!(
                tools
                    .call("zeus_wait", json!({"job_ids":[JOB],"since":cursor}))
                    .await
                    .unwrap()
                    .is_error()
            );
        }
        assert_eq!(
            fake.calls.lock().unwrap().len(),
            1,
            "invalid cursors never reach the daemon"
        );
    }

    #[tokio::test]
    async fn daemon_errors_become_tool_errors_with_recovery_hints() {
        let fake = Fake::new();
        let envelope =
            ErrorEnvelope::from_code(zeus_types::codes::POLICY_ORIGIN_NEVER, "origin is blocked");
        fake.answer("sessions.request", Err(ApiError::from(envelope)));
        let tools = ZeusTools::new(Arc::clone(&fake));
        let outcome = tools
            .call(
                "zeus_session_request",
                json!({ "origin": "https://portal.example.org", "scope": "site", "reason": "needs the export", "idempotency_key": KEY }),
            )
            .await
            .unwrap();
        assert!(outcome.is_error());
        assert_eq!(outcome.text(), "policy.origin_never: origin is blocked");
        assert_eq!(outcome.structured()["next"]["action"], "ask_user");
        assert_eq!(outcome.structured()["class"], "policy_denied");
    }

    #[tokio::test]
    async fn session_request_cannot_ask_for_never_and_says_only_a_person_can_approve() {
        let fake = Fake::new();
        fake.answer("sessions.request", Ok(json!({ "status": "pending_user_approval", "approval_id": "ap_01M45NHJ3XS0QXG9T42HHN2YR8" })));
        let tools = ZeusTools::new(Arc::clone(&fake));
        let never = tools
            .call("zeus_session_request", json!({ "origin": "https://p.example", "scope": "never", "reason": "x", "idempotency_key": KEY }))
            .await
            .unwrap();
        assert!(never.is_error());
        let ok = tools
            .call("zeus_session_request", json!({ "origin": "https://p.example", "scope": "once", "reason": "x", "idempotency_key": KEY }))
            .await
            .unwrap();
        assert_eq!(ok.structured()["status"], "pending_user_approval");
        assert!(
            ok.structured()["note"]
                .as_str()
                .unwrap()
                .contains("You cannot")
        );
        assert_eq!(fake.last().1["scope"], "once");
    }

    #[tokio::test]
    async fn status_without_an_id_lists_recent_jobs_with_a_small_page() {
        let fake = Fake::new();
        fake.answer("jobs.list", Ok(json!({ "items": [], "next_cursor": null })));
        let tools = ZeusTools::new(Arc::clone(&fake));
        tools.call("zeus_status", json!({})).await.unwrap();
        let (method, params) = fake.last();
        assert_eq!(method, "jobs.list");
        assert_eq!(params["limit"], 10);
        tools
            .call("zeus_status", json!({ "limit": 500 }))
            .await
            .unwrap();
        assert_eq!(fake.last().1["limit"], 50);
    }

    #[tokio::test]
    async fn reveal_urls_is_never_requested() {
        let fake = Fake::new();
        fake.answer("jobs.get", Err(ApiError::not_implemented("fake")));
        let tools = ZeusTools::new(Arc::clone(&fake));
        tools
            .call("zeus_status", json!({ "job_id": JOB }))
            .await
            .unwrap();
        assert_eq!(fake.last().1["reveal_urls"], false);
        let _ = codes::NOT_IMPLEMENTED;
    }
}
