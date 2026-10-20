//! Shutdown grace validation and durable announcement ordering without running a daemon process.
use zeus_api::methods::daemon::{ConnectionLimits, ShutdownParams};
use zeus_api::server::{CallContext, Notifier};
use zeus_api::{ClientIdentity, ClientKind, ClientRole};
use zeus_daemon::{Backend, DaemonConfig, runtime::RuntimeBackend};
use zeus_types::Timestamp;

#[tokio::test]
async fn shutdown_defaults_validates_before_effects_and_persists_one_announcement() {
    let dir = tempfile::tempdir().unwrap();
    let paths = zeus_api::endpoint::ZeusPaths {
        state_dir: dir.path().join("state"),
        config_dir: dir.path().join("config"),
        runtime_dir: dir.path().join("runtime"),
        user_identity: "test".into(),
        profile: None,
    };
    zeus_daemon::provision::provision(&paths, Timestamp::now()).unwrap();
    let backend = RuntimeBackend::open(
        paths.state_dir,
        DaemonConfig {
            write_roots: vec![dir.path().join("downloads")],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (notifier, _receiver) = Notifier::channel(10);
    let ctx = CallContext {
        reply: Default::default(),
        peer: Default::default(),
        attach: None,
        identity: ClientIdentity {
            credential_id: zeus_api::auth::CredentialId::generate(),
            kind: ClientKind::Admin,
            role: ClientRole::Cli,
            permissions: ClientKind::Admin.default_permissions(),
            client_name: "shutdown regression".into(),
        },
        connection_id: zeus_types::IdGenerator::new().connection_id(),
        notifier,
        limits: ConnectionLimits::STANDARD,
        capabilities: zeus_api::capabilities::all(),
    };
    let before = backend.journal.meta().unwrap().last_event_seq;
    assert!(
        backend
            .daemon_shutdown(
                &ctx,
                ShutdownParams {
                    grace_ms: Some(60_001)
                }
            )
            .await
            .is_err()
    );
    assert_eq!(backend.journal.meta().unwrap().last_event_seq, before);
    assert!(backend.shutdown.notice().is_none());
    let result = backend
        .daemon_shutdown(&ctx, ShutdownParams::default())
        .await
        .unwrap();
    assert!(result.accepted);
    assert_eq!(result.grace_ms, 10_000);
    zeus_types::spec_testing::SpecSchemas::load()
        .check(
            true,
            "daemon-api.v1",
            "DaemonShutdownResult",
            &serde_json::to_value(&result).unwrap(),
        )
        .unwrap();
    assert_eq!(backend.journal.meta().unwrap().last_event_seq, before + 1);
    assert!(
        backend.shutdown.notice().is_none(),
        "notification must wait for the reply fence"
    );
    ctx.reply.release();
    let notice = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        backend.shutdown.stopping(),
    )
    .await
    .unwrap();
    assert_eq!(notice.grace_ms, 10_000);
    assert_eq!(
        backend
            .daemon_shutdown(
                &ctx,
                ShutdownParams {
                    grace_ms: Some(60_000)
                }
            )
            .await
            .unwrap()
            .grace_ms,
        10_000
    );
    backend.engine.shutdown().await.unwrap();
    assert_eq!(
        backend.journal.meta().unwrap().last_event_seq,
        before + 1,
        "draining must not append a second stop event"
    );
}
