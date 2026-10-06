//! End to end over an in-memory pipe: real handshake, real authorisation, real framing.

use std::sync::Arc;

use zeus_api::auth::{
    ClientCredential, Credential, CredentialId, IpcSecret, MemoryCredentialStore, Token,
};
use zeus_api::client::{Client, ClientConfig, IpcClient, call};
use zeus_api::error::codes;
use zeus_api::methods::daemon::{ClientInfo, DaemonInfo, PingParams, PingResult};
use zeus_api::methods::{ApprovalsDecide, DaemonPing, JobsList};
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
    IpcClient::from_stream(client_end, credential, &h.secret, ClientConfig::new(info)).await
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
