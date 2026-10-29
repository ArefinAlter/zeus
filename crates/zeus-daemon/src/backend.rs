//! The backend contract: one method per MVP API method.
//!
//! The real implementation adapts `zeus-engine`, `zeus-journal`, the approvals store and the audit log; it is a
//! later task because those crates are being written in `zeus-core`. Every method has a default body returning
//! `api.not_implemented`, so a partial backend is useful and a gap is a typed error, never a panic.

use async_trait::async_trait;
use zeus_api::methods::admin::{
    AuditTailParams, AuditTailResult, AuditVerifyParams, AuditVerifyResult, ClientsAddParams,
    ClientsAddResult, ClientsListParams, ClientsListResult, ClientsRevokeParams,
    ClientsRevokeResult, ConfigGetParams, ConfigGetResult, ConfigSetParams, ConfigSetResult,
    DoctorParams, DoctorResult, LlmAuthCancelParams, LlmAuthFlow, LlmAuthSignOutParams,
    LlmAuthSignOutResult, LlmAuthStartParams, LlmAuthStartResult, LlmAuthStatusParams,
    LlmAuthStatusResult, LlmConfigureParams, LlmConfigureResult, LlmStatusParams, LlmStatusResult,
};
use zeus_api::methods::artifacts::{
    ArtifactsGetParams, ArtifactsGetResult, ArtifactsIngestParams, ArtifactsIngestResult,
    ArtifactsLifecycleApplyParams, ArtifactsLifecycleApplyResult, ArtifactsLifecyclePreviewParams,
    ArtifactsLifecyclePreviewResult, ArtifactsLineageParams, ArtifactsLineageResult,
    ArtifactsListParams, ArtifactsListResult,
};
use zeus_api::methods::cloud::*;
use zeus_api::methods::cloud_oauth::*;
use zeus_api::methods::daemon::{
    KillSwitchParams, KillSwitchResult, ShutdownParams, ShutdownResult, StatusParams, StatusResult,
};
use zeus_api::methods::events::{
    EventsReadParams, EventsReadResult, SubscribeParams, SubscribeResult, UnsubscribeParams,
    UnsubscribeResult,
};
use zeus_api::methods::files::{
    FilesListParams, FilesListResult, FilesOperationGetParams, FilesOperationGetResult,
    FilesOrganizeParams, FilesOrganizeResult, FilesRootsRegisterParams, FilesRootsRegisterResult,
    FilesUndoParams, FilesUndoResult,
};
use zeus_api::methods::inbox::*;
use zeus_api::methods::jobs::{
    EnqueueParams, EnqueueResult, JobsControlParams, JobsControlResult, JobsGetParams,
    JobsGetResult, JobsListParams, JobsListResult, JobsVerifyParams, JobsVerifyResult,
    JobsWaitParams, JobsWaitResult, ProbeParams, ProbeResultView,
};
use zeus_api::methods::queues::{
    QueuesConfigureParams, QueuesConfigureResult, QueuesControlParams, QueuesControlResult,
    QueuesDeleteParams, QueuesDeleteResult, QueuesListParams, QueuesListResult, QueuesPutParams,
    QueuesPutResult,
};
use zeus_api::methods::sessions::{
    ApprovalsDecideParams, ApprovalsDecideResult, ApprovalsGetParams, ApprovalsGetResult,
    ApprovalsListParams, ApprovalsListResult, SessionsListParams, SessionsListResult,
    SessionsRequestParams, SessionsRequestResult, SessionsRevokeParams, SessionsRevokeResult,
};
use zeus_api::methods::sources::*;
use zeus_api::methods::workflows::*;
use zeus_api::{ApiError, ApiResult, CallContext};

macro_rules! backend {
    ($( $(#[$meta:meta])* $name:ident ( $params:ty ) -> $result:ty; )*) => {
        /// What the daemon's request handlers call. Authorisation has already happened in the router; the
        /// backend still derives the actor and the source from `ctx.identity`, never from request parameters.
        #[async_trait]
        pub trait Backend: Send + Sync + 'static {
            /// Additional installation-wide restrictions before any new backend effect.
            fn team_admit(&self, method: &str, params: &serde_json::Value) -> ApiResult<()> { let _ = (method, params); Ok(()) }
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
    /// Immutable source operation: sources.accounts.list.
    sources_accounts(SourcesReadParams) -> SourcesAccountsResult;
    /// Immutable source operation: sources.accounts.configure.
    sources_configure(SourcesAccountConfigureParams) -> SourcesAccountMutationResult;
    /// Immutable source operation: sources.accounts.revoke.
    sources_revoke(SourcesAccountRevokeParams) -> SourcesAccountMutationResult;
    /// Immutable source operation: sources.inspect.
    sources_inspect(SourcesInspectParams) -> SourcePlanView;
    /// Immutable source operation: sources.submit.
    sources_submit(SourcesSubmitParams) -> SourceOperationView;
    /// Immutable source operation: sources.list.
    sources_list(SourcesReadParams) -> SourcesListResult;
    /// Immutable source operation: sources.get.
    sources_get(SourcesGetParams) -> SourceOperationView;
    /// Immutable source operation: sources.control.
    sources_control(SourcesControlParams) -> SourceOperationView;

    /// Durable owner-scoped `inbox.package.put`.
    inbox_package_put(InboxPackagePutParams) -> InboxMutationResult;
    /// Durable owner-scoped `inbox.packages.list`.
    inbox_packages_list(InboxPackagesListParams) -> InboxPackagesListResult;
    /// Durable owner-scoped `inbox.package.get`.
    inbox_package_get(InboxPackageGetParams) -> InboxPackageGetResult;
    /// Durable owner-scoped `inbox.package.archive`.
    inbox_package_archive(InboxPackageArchiveParams) -> InboxMutationResult;
    /// Durable owner-scoped `inbox.entries.add`.
    inbox_entries_add(InboxEntriesAddParams) -> InboxEntriesAddResult;
    /// Durable owner-scoped `inbox.entries.remove`.
    inbox_entries_remove(InboxEntriesRemoveParams) -> InboxMutationResult;
    /// Durable owner-scoped `inbox.preview`.
    inbox_preview(InboxPreviewParams) -> InboxPreviewResult;
    /// Durable owner-scoped `inbox.submit`.
    inbox_submit(InboxSubmitParams) -> InboxSubmitResult;
    /// Durable owner-scoped `inbox.submission.get`.
    inbox_submission_get(InboxSubmissionGetParams) -> InboxSubmissionView;
    /// Durable owner-scoped `inbox.submission.retry`.
    inbox_submission_retry(InboxSubmissionRetryParams) -> InboxSubmitResult;
    /// Human-only `cloud.oauth.begin`.
    cloud_oauth_begin(CloudOAuthBeginParams) -> CloudOAuthBeginResult;
    /// Human-only `cloud.oauth.finish`.
    cloud_oauth_finish(CloudOAuthFinishParams) -> CloudOAuthStatus;
    /// Human-only `cloud.oauth.status`.
    cloud_oauth_status(CloudOAuthStatusParams) -> CloudOAuthStatus;
    /// Human-only `cloud.oauth.cancel`.
    cloud_oauth_cancel(CloudOAuthCancelParams) -> CloudOAuthStatus;
    /// Human-only `cloud.oauth.refresh`.
    cloud_oauth_refresh(CloudOAuthRefreshParams) -> CloudOAuthStatus;
    /// Human-only `cloud.oauth.disconnect`.
    cloud_oauth_disconnect(CloudOAuthDisconnectParams) -> CloudOAuthDisconnectResult;
    /// `cloud.accounts.list`.
    cloud_accounts_list(CloudAccountsListParams) -> CloudAccountsListResult;
    /// `cloud.accounts.configure`.
    cloud_accounts_configure(CloudAccountsConfigureParams) -> CloudAccountMutationResult;
    /// `cloud.accounts.revoke`.
    cloud_accounts_revoke(CloudAccountsRevokeParams) -> CloudAccountMutationResult;
    /// `cloud.plan`.
    cloud_plan(CloudPlanParams) -> CloudPlanResult;
    /// `cloud.submit`.
    cloud_submit(CloudSubmitParams) -> CloudMutationResult;
    /// `cloud.list`.
    cloud_list(CloudListParams) -> CloudListResult;
    /// `cloud.get`.
    cloud_get(CloudGetParams) -> CloudDeliveryView;
    /// `cloud.control`.
    cloud_control(CloudControlParams) -> CloudMutationResult;
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
    /// `queues.list`.
    queues_list(QueuesListParams) -> QueuesListResult;
    /// `queues.put`.
    queues_put(QueuesPutParams) -> QueuesPutResult;
    /// `queues.delete`.
    queues_delete(QueuesDeleteParams) -> QueuesDeleteResult;
    /// `queues.control`.
    queues_control(QueuesControlParams) -> QueuesControlResult;
    /// `queues.configure`.
    queues_configure(QueuesConfigureParams) -> QueuesConfigureResult;
    /// `events.subscribe`: replays durable events, then pushes through `ctx.notifier`.
    events_subscribe(SubscribeParams) -> SubscribeResult;
    /// `events.read`: bounded replay under the same entity visibility rules.
    events_read(EventsReadParams) -> EventsReadResult;
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
    /// `files.organize`: dry run by default; applying raises a `destructive_op` approval.
    files_organize(FilesOrganizeParams) -> FilesOrganizeResult;
    /// `files.operation.get`: owner-scoped durable organization status.
    files_operation_get(FilesOperationGetParams) -> FilesOperationGetResult;
    /// `files.undo`: plans the inverse of an applied operation behind a new approval.
    files_undo(FilesUndoParams) -> FilesUndoResult;
    /// `files.roots.register` (role `service` only).
    files_roots_register(FilesRootsRegisterParams) -> FilesRootsRegisterResult;
    /// `artifacts.get`.
    artifacts_get(ArtifactsGetParams) -> ArtifactsGetResult;
    /// `artifacts.list`.
    artifacts_list(ArtifactsListParams) -> ArtifactsListResult;
    /// `artifacts.lineage`.
    artifacts_lineage(ArtifactsLineageParams) -> ArtifactsLineageResult;
    /// `artifacts.ingest`.
    artifacts_ingest(ArtifactsIngestParams) -> ArtifactsIngestResult;
    /// `artifacts.lifecycle.preview`.
    artifacts_lifecycle_preview(ArtifactsLifecyclePreviewParams) -> ArtifactsLifecyclePreviewResult;
    /// `artifacts.lifecycle.apply`.
    artifacts_lifecycle_apply(ArtifactsLifecycleApplyParams) -> ArtifactsLifecycleApplyResult;
    /// `clients.list` (admin, human-only).
    clients_list(ClientsListParams) -> ClientsListResult;
    /// `clients.add` (admin, human-only, `cli_interactive` evidence).
    clients_add(ClientsAddParams) -> ClientsAddResult;
    /// `clients.revoke` (admin, human-only, `cli_interactive` evidence).
    clients_revoke(ClientsRevokeParams) -> ClientsRevokeResult;
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
