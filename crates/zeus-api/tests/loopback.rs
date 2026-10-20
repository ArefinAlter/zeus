//! End to end over an in-memory pipe: real handshake, real authorisation, real framing.

use std::sync::Arc;

use zeus_api::auth::{
    ClientCredential, Credential, CredentialId, IpcSecret, MemoryCredentialStore, Token,
};
use zeus_api::client::{Client, ClientConfig, IpcClient, call};
use zeus_api::error::codes;
use zeus_api::methods::daemon::{ClientInfo, DaemonInfo, PingParams, PingResult};
use zeus_api::methods::{ApprovalsDecide, DaemonPing, JobsList, JobsWait};
use zeus_api::server::{Router, ServerContext, serve_connection};
use zeus_api::{ClientKind, ClientRole};
use zeus_types::Timestamp;

struct Harness {
    secret: Arc<IpcSecret>,
    store: Arc<MemoryCredentialStore>,
    router: Arc<Router>,
}

fn harness() -> Harness {
    let router = Router::new()
        .route::<DaemonPing, _, _>(|_ctx, _params: PingParams| async {
            Ok(PingResult {
                server_time: Timestamp::now(),
            })
        })
        .route::<ApprovalsDecide, _, _>(|_ctx, _params| async {
            Err(zeus_api::ApiError::not_implemented("decide"))
        })
        .route::<JobsWait, _, _>(|_ctx, _params| async {
            Err(zeus_api::ApiError::not_implemented("wait"))
        })
        .route::<JobsList, _, _>(|_ctx, params| async move {
            assert!(params.page.effective_limit() <= 100);
            Ok(zeus_api::Page::last(Vec::new()))
        });
    Harness {
        secret: Arc::new(IpcSecret::generate()),
        store: Arc::new(MemoryCredentialStore::default()),
        router: Arc::new(router),
    }
}

fn add_credential(h: &Harness, kind: ClientKind, role: ClientRole) -> ClientCredential {
    let token = Token::generate();
    let id = CredentialId::generate();
    h.store.insert(
        Credential::new(id.clone(), kind, role, token.auth_key(), Timestamp::now()).unwrap(),
    );
    ClientCredential { id, token }
}

async fn connect(
    h: &Harness,
    credential: ClientCredential,
    role: ClientRole,
) -> Result<IpcClient, zeus_api::ApiError> {
    connect_offering(h, credential, role, None).await
}

/// Connect, offering only `capabilities` in `daemon.hello` when given.
async fn connect_offering(
    h: &Harness,
    credential: ClientCredential,
    role: ClientRole,
    capabilities: Option<Vec<String>>,
) -> Result<IpcClient, zeus_api::ApiError> {
    let (client_end, server_end) = tokio::io::duplex(64 * 1024);
    let ctx = Arc::new(ServerContext {
        secret: Arc::clone(&h.secret),
        credentials: h.store.clone(),
        daemon: DaemonInfo {
            version: "0.1.0".into(),
            instance_id: "3f9a6c2d1b7e4a08c5d2e9f1a0b3c4d5".into(),
        },
        router: Arc::clone(&h.router),
    });
    tokio::spawn(async move {
        let _ = serve_connection(server_end, ctx).await;
    });
    let info = ClientInfo {
        name: "loopback-test".into(),
        version: "0".into(),
        role,
    };
    let mut config = ClientConfig::new(info);
    if let Some(capabilities) = capabilities {
        config.capabilities = capabilities;
    }
    IpcClient::from_stream(client_end, credential, &h.secret, config).await
}

#[tokio::test]
async fn a_human_client_authenticates_and_calls_methods() {
    let h = harness();
    let credential = add_credential(&h, ClientKind::Human, ClientRole::Cli);
    let client = connect(&h, credential, ClientRole::Cli).await.unwrap();
    assert_eq!(client.session().kind, ClientKind::Human);
    assert_eq!(client.hello().protocol_version, 1);
    let pong: PingResult = call::<DaemonPing, _>(&client, PingParams::default())
        .await
        .unwrap();
    assert!(pong.server_time.to_rfc3339().ends_with('Z'));
    // A registered handler that fails is reported through the envelope, not by dropping the connection.
    let error = call::<ApprovalsDecide, _>(&client, decide_params())
        .await
        .unwrap_err();
    assert_eq!(error.envelope().code.as_str(), codes::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn an_agent_client_can_never_reach_the_decide_handler() {
    let h = harness();
    let credential = add_credential(&h, ClientKind::Agent, ClientRole::Mcp);
    let client = connect(&h, credential, ClientRole::Mcp).await.unwrap();
    let error = call::<ApprovalsDecide, _>(&client, decide_params())
        .await
        .unwrap_err();
    assert_eq!(error.envelope().code.as_str(), codes::PERMISSION_DENIED);
    // The same connection keeps working for permitted methods.
    let page = call::<JobsList, _>(&client, Default::default())
        .await
        .unwrap();
    assert!(page.items.is_empty());
}

#[tokio::test]
async fn an_unknown_credential_is_refused_and_the_connection_ends() {
    let h = harness();
    let stranger = ClientCredential {
        id: CredentialId::generate(),
        token: Token::generate(),
    };
    let error = connect(&h, stranger, ClientRole::Cli).await.err().unwrap();
    assert_eq!(error.envelope().code.as_str(), codes::BAD_CREDENTIAL);
}

#[tokio::test]
async fn a_client_does_not_hand_credentials_to_a_server_with_the_wrong_secret() {
    let h = harness();
    let credential = add_credential(&h, ClientKind::Human, ClientRole::Cli);
    let (client_end, server_end) = tokio::io::duplex(64 * 1024);
    let squatter = Arc::new(ServerContext {
        secret: Arc::new(IpcSecret::generate()),
        credentials: h.store.clone(),
        daemon: DaemonInfo {
            version: "x".into(),
            instance_id: "y".into(),
        },
        router: Arc::clone(&h.router),
    });
    tokio::spawn(async move {
        let _ = serve_connection(server_end, squatter).await;
    });
    let info = ClientInfo {
        name: "t".into(),
        version: "0".into(),
        role: ClientRole::Cli,
    };
    let error = IpcClient::from_stream(client_end, credential, &h.secret, ClientConfig::new(info))
        .await
        .err()
        .unwrap();
    assert_eq!(error.envelope().code.as_str(), codes::SERVER_PROOF_INVALID);
}

#[tokio::test]
async fn requests_before_authentication_are_refused() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let h = harness();
    let (mut client_end, server_end) = tokio::io::duplex(64 * 1024);
    let ctx = Arc::new(ServerContext {
        secret: Arc::clone(&h.secret),
        credentials: h.store.clone(),
        daemon: DaemonInfo {
            version: "0.1.0".into(),
            instance_id: "z".into(),
        },
        router: Arc::clone(&h.router),
    });
    let server = tokio::spawn(serve_connection(server_end, ctx));
    client_end
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"jobs.list\",\"params\":{}}\n")
        .await
        .unwrap();
    let mut lines = BufReader::new(&mut client_end).lines();
    let line = lines.next_line().await.unwrap().unwrap();
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["error"]["data"]["code"], "api.not_authenticated");
    assert!(server.await.unwrap().is_err());
}

#[tokio::test]
async fn oversized_lines_end_the_connection_with_payload_too_large() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let h = harness();
    let (mut client_end, server_end) = tokio::io::duplex(2 * 1024 * 1024);
    let ctx = Arc::new(ServerContext {
        secret: Arc::clone(&h.secret),
        credentials: h.store.clone(),
        daemon: DaemonInfo {
            version: "0.1.0".into(),
            instance_id: "z".into(),
        },
        router: Arc::clone(&h.router),
    });
    let server = tokio::spawn(serve_connection(server_end, ctx));
    let huge = vec![b'x'; 1024 * 1024 + 10];
    let writer = tokio::spawn(async move {
        let _ = client_end.write_all(&huge).await;
        client_end
    });
    let mut client_end = writer.await.unwrap();
    let mut lines = BufReader::new(&mut client_end).lines();
    let line = lines.next_line().await.unwrap().unwrap();
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["error"]["data"]["code"], "api.payload_too_large");
    assert!(server.await.unwrap().is_err());
}

fn decide_params() -> zeus_api::methods::sessions::ApprovalsDecideParams {
    use zeus_api::methods::sessions::ApprovalsDecideParams;
    serde_json::from_value::<ApprovalsDecideParams>(serde_json::json!({
        "approval_id": "ap_01M45NHJ3XS0QXG9T42HHN2YR8",
        "request_hash": "1e0f07a3c0beca17e28b2d889c3e78a743e9c931e3d5efbff76921fe028ce231",
        "outcome": "approve",
        "scope": "site",
        "channel": "cli_interactive"
    }))
    .unwrap()
}

#[tokio::test]
async fn shutdown_notifies_all_clients_drains_existing_calls_and_refuses_new_work() {
    use zeus_api::methods::{
        DaemonShutdown,
        daemon::{ShutdownParams, ShutdownResult},
    };
    use zeus_api::server::ShutdownController;
    let controller = ShutdownController::default();
    let entered = Arc::new(tokio::sync::Notify::new());
    let finish = Arc::new(tokio::sync::Notify::new());
    let entered_handler = entered.clone();
    let finish_handler = finish.clone();
    let stop = controller.clone();
    let mut h = harness();
    h.router = Arc::new(
        Router::new()
            .with_shutdown(controller.clone())
            .route::<DaemonPing, _, _>(move |_ctx, _params| {
                let entered = entered_handler.clone();
                let finish = finish_handler.clone();
                async move {
                    entered.notify_one();
                    finish.notified().await;
                    Ok(PingResult {
                        server_time: Timestamp::now(),
                    })
                }
            })
            .route::<DaemonShutdown, _, _>(move |ctx, params| {
                let stop = stop.clone();
                async move {
                    let grace_ms = stop.request(
                        params.grace_ms.unwrap_or(10_000),
                        "shutdown_requested",
                        Some(ctx.reply),
                    );
                    Ok(ShutdownResult {
                        accepted: true,
                        replayed: false,
                        grace_ms,
                    })
                }
            }),
    );
    let admin = connect(
        &h,
        add_credential(&h, ClientKind::Admin, ClientRole::Cli),
        ClientRole::Cli,
    )
    .await
    .unwrap();
    let human = Arc::new(
        connect(
            &h,
            add_credential(&h, ClientKind::Human, ClientRole::Cli),
            ClientRole::Cli,
        )
        .await
        .unwrap(),
    );
    let mut admin_notes = admin.notifications();
    let mut human_notes = human.notifications();
    let running = human.clone();
    let pending = tokio::spawn(async move {
        call::<DaemonPing, _>(running.as_ref(), PingParams::default()).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
        .await
        .unwrap();
    let result = call::<DaemonShutdown, _>(
        &admin,
        ShutdownParams {
            grace_ms: Some(300),
        },
    )
    .await
    .unwrap();
    assert!(result.accepted);
    assert_eq!(result.grace_ms, 300);
    for receiver in [&mut admin_notes, &mut human_notes] {
        let note = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(note.method, "daemon.stopping");
        assert_eq!(note.params.unwrap()["grace_ms"], 300);
    }
    finish.notify_one();
    assert!(
        pending.await.unwrap().is_ok(),
        "a previously accepted request must complete during grace"
    );
    let rejected = call::<DaemonPing, _>(&admin, PingParams::default())
        .await
        .unwrap_err();
    assert_eq!(rejected.envelope().code.as_str(), "api.unavailable");
    assert_eq!(
        serde_json::to_value(rejected.envelope()).unwrap()["details"]["reason"],
        "stopping"
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), admin.closed())
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), human.closed())
        .await
        .unwrap();
}

#[tokio::test]
async fn gated_methods_and_parameters_need_a_negotiated_capability() {
    let h = harness();
    let credential = add_credential(&h, ClientKind::Human, ClientRole::Cli);
    let client = connect_offering(&h, credential, ClientRole::Cli, Some(vec!["events".into()]))
        .await
        .unwrap();
    assert_eq!(
        client.hello().capabilities.iter().collect::<Vec<_>>(),
        ["events"]
    );
    let wait: zeus_api::methods::jobs::JobsWaitParams =
        serde_json::from_value(serde_json::json!({"job_ids": ["job_01M45NHJ2AHWMXT8JVJ7BEEJX7"], "until": "all_terminal", "timeout_ms": 10}))
            .unwrap();
    let missing = |error: zeus_api::ApiError| {
        let envelope = error.envelope().clone();
        assert_eq!(envelope.code.as_str(), codes::CAPABILITY_MISSING);
        envelope.details.unwrap()["capability"].clone()
    };
    assert_eq!(
        missing(
            call::<JobsWait, _>(&client, wait.clone())
                .await
                .unwrap_err()
        ),
        "jobs.wait"
    );
    // A gated parameter: `idempotency_key` on a mutating method needs `idempotency`.
    let mut keyed = decide_params();
    keyed.idempotency_key = Some("retry-1".into());
    assert_eq!(
        missing(
            call::<ApprovalsDecide, _>(&client, keyed)
                .await
                .unwrap_err()
        ),
        "idempotency"
    );
    // Without the parameter the same method reaches its handler; ungated methods are always available.
    let error = call::<ApprovalsDecide, _>(&client, decide_params())
        .await
        .unwrap_err();
    assert_eq!(error.envelope().code.as_str(), codes::NOT_IMPLEMENTED);
    call::<DaemonPing, _>(&client, PingParams::default())
        .await
        .unwrap();

    // A client offering every capability reaches the handler.
    let credential = add_credential(&h, ClientKind::Human, ClientRole::Cli);
    let full = connect(&h, credential, ClientRole::Cli).await.unwrap();
    let error = call::<JobsWait, _>(&full, wait).await.unwrap_err();
    assert_eq!(error.envelope().code.as_str(), codes::NOT_IMPLEMENTED);
}
