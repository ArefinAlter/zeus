//! The backend contract: one method per MVP API method.
//!
//! The real implementation adapts `zeus-engine`, `zeus-journal`, the approvals store and the audit log; it is a
//! later task because those crates are being written in `zeus-core`. Every method has a default body returning
//! `api.not_implemented`, so a partial backend is useful and a gap is a typed error, never a panic.

use async_trait::async_trait;
use zeus_api::methods::admin::{
    AuditTailParams, AuditTailResult, AuditVerifyParams, AuditVerifyResult, ConfigGetParams,
    ConfigGetResult, ConfigSetParams, ConfigSetResult, DoctorParams, DoctorResult,
    LlmAuthCancelParams, LlmAuthFlow, LlmAuthSignOutParams, LlmAuthSignOutResult,
    LlmAuthStartParams, LlmAuthStartResult, LlmAuthStatusParams, LlmAuthStatusResult,
    LlmConfigureParams, LlmConfigureResult, LlmStatusParams, LlmStatusResult,
};
use zeus_api::methods::daemon::{
    KillSwitchParams, KillSwitchResult, ShutdownParams, ShutdownResult, StatusParams, StatusResult,
};
use zeus_api::methods::events::{
    SubscribeParams, SubscribeResult, UnsubscribeParams, UnsubscribeResult,
};
use zeus_api::methods::files::{FilesListParams, FilesListResult};
use zeus_api::methods::jobs::{
    EnqueueParams, EnqueueResult, JobsControlParams, JobsControlResult, JobsGetParams,
    JobsGetResult, JobsListParams, JobsListResult, JobsVerifyParams, JobsVerifyResult,
    JobsWaitParams, JobsWaitResult, ProbeParams, ProbeResultView,
};
use zeus_api::methods::sessions::{
    ApprovalsDecideParams, ApprovalsDecideResult, ApprovalsGetParams, ApprovalsGetResult,
    ApprovalsListParams, ApprovalsListResult, SessionsListParams, SessionsListResult,
    SessionsRequestParams, SessionsRequestResult, SessionsRevokeParams, SessionsRevokeResult,
};
use zeus_api::methods::workflows::*;
use zeus_api::{ApiError, ApiResult, CallContext};

macro_rules! backend {
    ($( $(#[$meta:meta])* $name:ident ( $params:ty ) -> $result:ty; )*) => {
        /// What the daemon's request handlers call. Authorisation has already happened in the router; the
        /// backend still derives the actor and the source from `ctx.identity`, never from request parameters.
        #[async_trait]
        pub trait Backend: Send + Sync + 'static {
            $(
                $(#[$meta])*
                async fn $name(&self, ctx: &CallContext, params: $params) -> ApiResult<$result> {
                    let _ = (ctx, params);
                    Err(ApiError::not_implemented(concat!("backend::", stringify!($name))))
                }
            )*
        }
    };
}

backend! {
    /// `daemon.status`.
    daemon_status(StatusParams) -> StatusResult;
    /// `daemon.shutdown` (admin).
    daemon_shutdown(ShutdownParams) -> ShutdownResult;
    /// `daemon.kill_switch` (human): revoke every grant and pause every job in one transaction.
    daemon_kill_switch(KillSwitchParams) -> KillSwitchResult;
    /// `jobs.probe`.
    jobs_probe(ProbeParams) -> ProbeResultView;
    /// `jobs.enqueue`: honours `dry_run`, idempotency keys (00 section 5.4) and the write roots.
    jobs_enqueue(EnqueueParams) -> EnqueueResult;
    /// `jobs.list`.
    jobs_list(JobsListParams) -> JobsListResult;
    /// `jobs.get`: returns raw URLs only when `reveal_urls` is set (the router already required a human).
    jobs_get(JobsGetParams) -> JobsGetResult;
    /// `jobs.wait`: at most 60 s.
    jobs_wait(JobsWaitParams) -> JobsWaitResult;
    /// `jobs.control`.
    jobs_control(JobsControlParams) -> JobsControlResult;
    /// `jobs.verify`.
    jobs_verify(JobsVerifyParams) -> JobsVerifyResult;
    /// `events.subscribe`: replays durable events, then pushes through `ctx.notifier`.
    events_subscribe(SubscribeParams) -> SubscribeResult;
    /// `events.unsubscribe`.
    events_unsubscribe(UnsubscribeParams) -> UnsubscribeResult;
    /// `sessions.list`.
    sessions_list(SessionsListParams) -> SessionsListResult;
    /// `sessions.request`: raises an approval, never grants.
    sessions_request(SessionsRequestParams) -> SessionsRequestResult;
    /// `sessions.revoke` (human).
    sessions_revoke(SessionsRevokeParams) -> SessionsRevokeResult;
    /// `approvals.list`: agents see only the approvals they caused.
    approvals_list(ApprovalsListParams) -> ApprovalsListResult;
    /// `approvals.get`.
    approvals_get(ApprovalsGetParams) -> ApprovalsGetResult;
    /// `approvals.decide` (human): validate with [`crate::approvals::validate_decision`], then apply.
    approvals_decide(ApprovalsDecideParams) -> ApprovalsDecideResult;
    /// `files.list`.
    files_list(FilesListParams) -> FilesListResult;
    /// `audit.tail`.
    audit_tail(AuditTailParams) -> AuditTailResult;
    /// `audit.verify`.
    audit_verify(AuditVerifyParams) -> AuditVerifyResult;
    /// `config.get`: secrets withheld.
    config_get(ConfigGetParams) -> ConfigGetResult;
    /// `config.set` (human).
    config_set(ConfigSetParams) -> ConfigSetResult;
    /// `llm.auth.start` (verified human terminal).
    llm_auth_start(LlmAuthStartParams) -> LlmAuthStartResult;
    /// `llm.auth.status` (verified human terminal).
    llm_auth_status(LlmAuthStatusParams) -> LlmAuthStatusResult;
    /// `llm.auth.cancel` (verified human terminal).
    llm_auth_cancel(LlmAuthCancelParams) -> LlmAuthFlow;
    /// `llm.auth.sign_out` (verified human terminal).
    llm_auth_sign_out(LlmAuthSignOutParams) -> LlmAuthSignOutResult;
    /// `llm.status`.
    llm_status(LlmStatusParams) -> LlmStatusResult;
    /// `llm.configure` (verified human terminal).
    llm_configure(LlmConfigureParams) -> LlmConfigureResult;
    /// `workflows.plan`.
    workflows_plan(WorkflowsPlanParams) -> WorkflowsPlanResult;
    /// `workflows.dry_run`.
    workflows_dry_run(WorkflowsDryRunParams) -> WorkflowsDryRunResult;
    /// `workflows.run`.
    workflows_run(WorkflowsRunParams) -> WorkflowsRunResult;
    /// `workflows.get`.
    workflows_get(WorkflowsGetParams) -> WorkflowsGetResult;
    /// `workflows.list`.
    workflows_list(WorkflowsListParams) -> WorkflowsListResult;
    /// `workflows.control`.
    workflows_control(WorkflowsControlParams) -> WorkflowsControlResult;
    /// `workflows.manifest`.
    workflows_manifest(WorkflowsManifestParams) -> WorkflowsManifestResult;
    /// `doctor.run`.
    doctor_run(DoctorParams) -> DoctorResult;
}

/// A backend with no behaviour: every method answers `api.not_implemented`.
#[derive(Debug, Clone, Copy, Default)]
pub struct NotImplementedBackend;

impl Backend for NotImplementedBackend {}
