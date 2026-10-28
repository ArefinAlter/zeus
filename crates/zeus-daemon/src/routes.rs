//! Registers a [`Backend`] on a [`zeus_api::Router`].
//!
//! `daemon.ping` is answered here; every other implemented method is forwarded to the backend. Methods that are
//! catalogued but not built remain unregistered: the router answers `api.not_implemented` for them while the policy
//! table still enforces who may call them.

use std::sync::Arc;

use zeus_api::Router;
use zeus_api::methods::daemon::{PingParams, PingResult};
use zeus_api::methods::{
    ApprovalsDecide, ApprovalsGet, ApprovalsList, ArtifactsGet, ArtifactsIngest,
    ArtifactsLifecycleApply, ArtifactsLifecyclePreview, ArtifactsLineage, ArtifactsList, AuditTail,
    AuditVerify, ClientsAdd, ClientsList, ClientsRevoke, ConfigGet, ConfigSet, DaemonKillSwitch,
    DaemonPing, DaemonShutdown, DaemonStatus, DoctorRun, EventsSubscribe, EventsUnsubscribe,
    FilesList, FilesOrganize, FilesRootsRegister, FilesUndo, JobsControl, JobsEnqueue, JobsGet,
    JobsList, JobsProbe, JobsVerify, JobsWait, LlmAuthCancel, LlmAuthSignOut, LlmAuthStart,
    LlmAuthStatus, LlmConfigure, LlmStatus, QueuesConfigure, QueuesControl, QueuesDelete,
    QueuesList, QueuesPut, SessionsList, SessionsRequest, SessionsRevoke, WorkflowsControl,
    WorkflowsDryRun, WorkflowsGet, WorkflowsList, WorkflowsManifest, WorkflowsPlan, WorkflowsRun,
};
use zeus_api::methods::{
    CloudAccountsConfigure, CloudAccountsList, CloudAccountsRevoke, CloudControl, CloudGet,
    CloudList, CloudPlan, CloudSubmit,
};
use zeus_api::methods::{
    CloudOAuthBegin, CloudOAuthCancel, CloudOAuthDisconnect, CloudOAuthFinish, CloudOAuthRefresh,
    CloudOAuthStatusMethod,
};
use zeus_api::methods::{
    InboxEntriesAdd, InboxEntriesRemove, InboxPackageArchive, InboxPackageGet, InboxPackagePut,
    InboxPackagesList, InboxPreview, InboxSubmissionGet, InboxSubmissionRetry, InboxSubmit,
};
use zeus_types::Timestamp;

use crate::backend::Backend;

macro_rules! forward {
    ($router:expr, $backend:expr, $marker:ty, $method:ident) => {{
        let backend = Arc::clone(&$backend);
        $router.route::<$marker, _, _>(move |ctx, params| {
            let backend = Arc::clone(&backend);
            async move {
                crate::consent::REQUESTER_KIND
                    .scope(ctx.identity.kind, backend.$method(&ctx, params))
                    .await
            }
        })
    }};
}

/// Build the router for `backend`.
pub fn build_router<B: Backend>(backend: Arc<B>) -> Router {
    let router = Router::new().route::<DaemonPing, _, _>(|_ctx, _params: PingParams| async {
        Ok(PingResult {
            server_time: Timestamp::now(),
        })
    });
    let router = forward!(router, backend, InboxPackagePut, inbox_package_put);
    let router = forward!(router, backend, InboxPackagesList, inbox_packages_list);
    let router = forward!(router, backend, InboxPackageGet, inbox_package_get);
    let router = forward!(router, backend, InboxPackageArchive, inbox_package_archive);
    let router = forward!(router, backend, InboxEntriesAdd, inbox_entries_add);
    let router = forward!(router, backend, InboxEntriesRemove, inbox_entries_remove);
    let router = forward!(router, backend, InboxPreview, inbox_preview);
    let router = forward!(router, backend, InboxSubmit, inbox_submit);
    let router = forward!(router, backend, InboxSubmissionGet, inbox_submission_get);
    let router = forward!(
        router,
        backend,
        InboxSubmissionRetry,
        inbox_submission_retry
    );
    let router = forward!(router, backend, CloudOAuthBegin, cloud_oauth_begin);
    let router = forward!(router, backend, CloudOAuthFinish, cloud_oauth_finish);
    let router = forward!(router, backend, CloudOAuthStatusMethod, cloud_oauth_status);
    let router = forward!(router, backend, CloudOAuthCancel, cloud_oauth_cancel);
    let router = forward!(router, backend, CloudOAuthRefresh, cloud_oauth_refresh);
    let router = forward!(
        router,
        backend,
        CloudOAuthDisconnect,
        cloud_oauth_disconnect
    );
    let router = forward!(router, backend, CloudAccountsList, cloud_accounts_list);
    let router = forward!(
        router,
        backend,
        CloudAccountsConfigure,
        cloud_accounts_configure
    );
    let router = forward!(router, backend, CloudAccountsRevoke, cloud_accounts_revoke);
    let router = forward!(router, backend, CloudPlan, cloud_plan);
    let router = forward!(router, backend, CloudSubmit, cloud_submit);
    let router = forward!(router, backend, CloudList, cloud_list);
    let router = forward!(router, backend, CloudGet, cloud_get);
    let router = forward!(router, backend, CloudControl, cloud_control);
    let router = forward!(router, backend, DaemonStatus, daemon_status);
    let router = forward!(router, backend, DaemonShutdown, daemon_shutdown);
    let router = forward!(router, backend, DaemonKillSwitch, daemon_kill_switch);
    let router = forward!(router, backend, JobsProbe, jobs_probe);
    let router = forward!(router, backend, JobsEnqueue, jobs_enqueue);
    let router = forward!(router, backend, JobsList, jobs_list);
    let router = forward!(router, backend, JobsGet, jobs_get);
    let router = forward!(router, backend, JobsWait, jobs_wait);
    let router = forward!(router, backend, JobsControl, jobs_control);
    let router = forward!(router, backend, JobsVerify, jobs_verify);
    let router = forward!(router, backend, QueuesList, queues_list);
    let router = forward!(router, backend, QueuesPut, queues_put);
    let router = forward!(router, backend, QueuesDelete, queues_delete);
    let router = forward!(router, backend, QueuesControl, queues_control);
    let router = forward!(router, backend, QueuesConfigure, queues_configure);
    let router = forward!(router, backend, EventsSubscribe, events_subscribe);
    let router = forward!(router, backend, EventsUnsubscribe, events_unsubscribe);
    let router = forward!(router, backend, SessionsList, sessions_list);
    let router = forward!(router, backend, SessionsRequest, sessions_request);
    let router = forward!(router, backend, SessionsRevoke, sessions_revoke);
    let router = forward!(router, backend, ApprovalsList, approvals_list);
    let router = forward!(router, backend, ApprovalsGet, approvals_get);
    let router = forward!(router, backend, ApprovalsDecide, approvals_decide);
    let router = forward!(router, backend, FilesList, files_list);
    let router = forward!(router, backend, FilesRootsRegister, files_roots_register);
    let router = forward!(router, backend, FilesOrganize, files_organize);
    let router = forward!(router, backend, FilesUndo, files_undo);
    let router = forward!(router, backend, ArtifactsGet, artifacts_get);
    let router = forward!(router, backend, ArtifactsList, artifacts_list);
    let router = forward!(router, backend, ArtifactsLineage, artifacts_lineage);
    let router = forward!(router, backend, ArtifactsIngest, artifacts_ingest);
    let router = forward!(
        router,
        backend,
        ArtifactsLifecyclePreview,
        artifacts_lifecycle_preview
    );
    let router = forward!(
        router,
        backend,
        ArtifactsLifecycleApply,
        artifacts_lifecycle_apply
    );
    let router = forward!(router, backend, ClientsList, clients_list);
    let router = forward!(router, backend, ClientsAdd, clients_add);
    let router = forward!(router, backend, ClientsRevoke, clients_revoke);
    let router = forward!(router, backend, AuditTail, audit_tail);
    let router = forward!(router, backend, AuditVerify, audit_verify);
    let router = forward!(router, backend, ConfigGet, config_get);
    let router = forward!(router, backend, ConfigSet, config_set);
    let router = forward!(router, backend, LlmAuthStart, llm_auth_start);
    let router = forward!(router, backend, LlmAuthStatus, llm_auth_status);
    let router = forward!(router, backend, LlmAuthCancel, llm_auth_cancel);
    let router = forward!(router, backend, LlmAuthSignOut, llm_auth_sign_out);
    let router = forward!(router, backend, LlmStatus, llm_status);
    let router = forward!(router, backend, LlmConfigure, llm_configure);
    let router = forward!(router, backend, WorkflowsPlan, workflows_plan);
    let router = forward!(router, backend, WorkflowsDryRun, workflows_dry_run);
    let router = forward!(router, backend, WorkflowsRun, workflows_run);
    let router = forward!(router, backend, WorkflowsGet, workflows_get);
    let router = forward!(router, backend, WorkflowsList, workflows_list);
    let router = forward!(router, backend, WorkflowsControl, workflows_control);
    let router = forward!(router, backend, WorkflowsManifest, workflows_manifest);
    forward!(router, backend, DoctorRun, doctor_run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::NotImplementedBackend;
    use zeus_api::auth::CredentialId;
    use zeus_api::error::codes;
    use zeus_api::jsonrpc::{Request, RequestId};
    use zeus_api::methods::daemon::ConnectionLimits;
    use zeus_api::policy::{METHODS, Stage};
    use zeus_api::server::Notifier;
    use zeus_api::{CallContext, ClientIdentity, ClientKind, ClientRole};

    fn ctx(kind: ClientKind, role: ClientRole) -> CallContext {
        let (notifier, _rx) = Notifier::channel(8);
        CallContext {
            reply: Default::default(),
            peer: Default::default(),
            attach: None,
            identity: ClientIdentity {
                credential_id: CredentialId::generate(),
                kind,
                role,
                permissions: kind.default_permissions(),
                client_name: "t".into(),
            },
            connection_id: zeus_types::IdGenerator::new().connection_id(),
            notifier,
            limits: ConnectionLimits::STANDARD,
            capabilities: zeus_api::capabilities::all(),
        }
    }

    #[test]
    fn every_mvp_method_has_a_route_except_the_handshake() {
        let router = build_router(Arc::new(NotImplementedBackend));
        let registered: std::collections::BTreeSet<_> = router.registered().into_iter().collect();
        for method in METHODS
            .iter()
            .filter(|m| m.stage == Stage::Mvp && !m.is_pre_auth())
        {
            assert!(
                registered.contains(method.name),
                "{} has no route",
                method.name
            );
        }
        for method in METHODS.iter().filter(|m| {
            m.stage == Stage::Later
                && !matches!(
                    m.name,
                    "llm.status"
                        | "llm.configure"
                        | "llm.auth.start"
                        | "llm.auth.status"
                        | "llm.auth.cancel"
                        | "llm.auth.sign_out"
                )
                && !m.name.starts_with("workflows.")
                && !matches!(
                    m.name,
                    "files.organize"
                        | "files.undo"
                        | "artifacts.get"
                        | "artifacts.list"
                        | "artifacts.lineage"
                        | "artifacts.ingest"
                        | "artifacts.lifecycle.preview"
                        | "artifacts.lifecycle.apply"
                )
        }) {
            assert!(
                !registered.contains(method.name),
                "{} is later but routed",
                method.name
            );
        }
    }

    /// 05 section 8.2: a capability is advertised only when every method it gates is implemented.
    #[test]
    fn every_advertised_capability_is_fully_routed() {
        let router = build_router(Arc::new(NotImplementedBackend));
        let registered: std::collections::BTreeSet<_> = router.registered().into_iter().collect();
        for method in METHODS.iter().filter(|m| {
            m.capability
                .is_some_and(|c| zeus_api::capabilities::ALL.contains(&c))
        }) {
            assert!(
                registered.contains(method.name),
                "{} is gated by an advertised capability but has no route",
                method.name
            );
        }
    }

    #[tokio::test]
    async fn ping_works_and_unimplemented_backend_methods_say_so() {
        let router = build_router(Arc::new(NotImplementedBackend));
        let ctx = ctx(ClientKind::Human, ClientRole::Cli);
        let pong = router
            .dispatch(&ctx, Request::new(1, "daemon.ping", None))
            .await;
        assert!(pong.error.is_none());
        let status = router
            .dispatch(&ctx, Request::new(2, "daemon.status", None))
            .await;
        let error = status.error.unwrap();
        assert_eq!(error.data.unwrap().code.as_str(), codes::NOT_IMPLEMENTED);
        assert_eq!(status.id, Some(RequestId::Number(2)));
    }

    #[tokio::test]
    async fn the_router_checks_permissions_before_the_backend_runs() {
        let router = build_router(Arc::new(NotImplementedBackend));
        let agent = ctx(ClientKind::Agent, ClientRole::Mcp);
        for method in [
            "approvals.decide",
            "sessions.revoke",
            "daemon.kill_switch",
            "config.set",
            "daemon.shutdown",
        ] {
            let response = router
                .dispatch(&agent, Request::new(1, method, Some(serde_json::json!({}))))
                .await;
            let code = response.error.unwrap().data.unwrap().code;
            assert!(
                code.as_str() == codes::PERMISSION_DENIED || code.as_str() == codes::HUMAN_REQUIRED,
                "{method}: {code}"
            );
        }
    }

    #[tokio::test]
    async fn unknown_methods_and_bad_params_are_reported_precisely() {
        let router = build_router(Arc::new(NotImplementedBackend));
        let human = ctx(ClientKind::Human, ClientRole::Cli);
        let unknown = router
            .dispatch(&human, Request::new(1, "jobs.nope", None))
            .await;
        assert_eq!(unknown.error.unwrap().code, -32601);
        let bad = router
            .dispatch(
                &human,
                Request::new(
                    2,
                    "jobs.get",
                    Some(serde_json::json!({ "job_id": "ZEUS-CANARY-not-an-id" })),
                ),
            )
            .await;
        let error = bad.error.unwrap();
        assert_eq!(error.code, -32602);
        assert!(
            !error.message.contains("ZEUS-CANARY"),
            "parameter values must not be echoed"
        );
        let not_object = router
            .dispatch(
                &human,
                Request::new(3, "jobs.list", Some(serde_json::json!([1]))),
            )
            .await;
        assert_eq!(not_object.error.unwrap().code, -32602);
    }
}
