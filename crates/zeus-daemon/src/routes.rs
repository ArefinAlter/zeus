//! Registers a [`Backend`] on a [`zeus_api::Router`].
//!
//! `daemon.ping` is answered here; every other MVP method is forwarded to the backend. Later-stage methods
//! (workflows, files organize/convert/undo, llm, clients) are deliberately not registered: the router answers
//! `api.not_implemented` for them while the policy table still enforces who may call them.

use std::sync::Arc;

use zeus_api::Router;
use zeus_api::methods::daemon::{PingParams, PingResult};
use zeus_api::methods::{
    ApprovalsDecide, ApprovalsGet, ApprovalsList, AuditTail, AuditVerify, ConfigGet, ConfigSet,
    DaemonKillSwitch, DaemonPing, DaemonShutdown, DaemonStatus, DoctorRun, EventsSubscribe,
    EventsUnsubscribe, FilesList, JobsControl, JobsEnqueue, JobsGet, JobsList, JobsProbe,
    JobsVerify, JobsWait, SessionsList, SessionsRequest, SessionsRevoke,
};
use zeus_types::Timestamp;

use crate::backend::Backend;

macro_rules! forward {
    ($router:expr, $backend:expr, $marker:ty, $method:ident) => {{
        let backend = Arc::clone(&$backend);
        $router.route::<$marker, _, _>(move |ctx, params| {
            let backend = Arc::clone(&backend);
            async move { backend.$method(&ctx, params).await }
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
    let router = forward!(router, backend, EventsSubscribe, events_subscribe);
    let router = forward!(router, backend, EventsUnsubscribe, events_unsubscribe);
    let router = forward!(router, backend, SessionsList, sessions_list);
    let router = forward!(router, backend, SessionsRequest, sessions_request);
    let router = forward!(router, backend, SessionsRevoke, sessions_revoke);
    let router = forward!(router, backend, ApprovalsList, approvals_list);
    let router = forward!(router, backend, ApprovalsGet, approvals_get);
    let router = forward!(router, backend, ApprovalsDecide, approvals_decide);
    let router = forward!(router, backend, FilesList, files_list);
    let router = forward!(router, backend, AuditTail, audit_tail);
    let router = forward!(router, backend, AuditVerify, audit_verify);
    let router = forward!(router, backend, ConfigGet, config_get);
    let router = forward!(router, backend, ConfigSet, config_set);
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
        for method in METHODS.iter().filter(|m| m.stage == Stage::Later) {
            assert!(
                !registered.contains(method.name),
                "{} is later but routed",
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
