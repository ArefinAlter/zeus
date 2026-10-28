//! Optional Streamable HTTP: separate loopback listener, dedicated bearer, exact Host/Origin and
//! bounded stateless requests. Daemon permissions and human-only decisions remain authoritative.
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use futures::StreamExt;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde::Deserialize;
use std::{
    io::Read,
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    sync::Arc,
    time::Duration,
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;
use zeus_api::{ApiError, ApiResult, Client};
const TOKEN_PREFIX: &str = "zmcp1_";
const MAX_BODY: usize = 65_536;
const REQUEST_TIME: Duration = Duration::from_secs(90);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenFile {
    token: zeus_types::SecretString,
}
fn invalid(message: &str) -> ApiError {
    ApiError::invalid_params(message)
}
fn valid_token(token: &str) -> bool {
    token.strip_prefix(TOKEN_PREFIX).is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
/// Load a dedicated MCP bearer file. IPC/client credential formats are refused. Secret bytes never
/// appear in the return value, diagnostics or endpoint URL; the listener receives only their BLAKE3.
pub fn token_file_hash(path: &Path) -> ApiResult<[u8; 32]> {
    zeus_api::transport::secure_private_file(path)
        .map_err(|_| invalid("MCP token file must be an owner-only regular file"))?;
    let file = std::fs::File::open(path).map_err(|_| invalid("MCP token file cannot be opened"))?;
    if file
        .metadata()
        .map_err(|_| invalid("MCP token file cannot be inspected"))?
        .len()
        > 4096
    {
        return Err(invalid("MCP token file exceeds 4096 bytes"));
    }
    let mut bytes = Zeroizing::new(vec![]);
    file.take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("MCP token file cannot be read"))?;
    if bytes.len() > 4096 {
        return Err(invalid("MCP token file exceeds 4096 bytes"));
    }
    let token: TokenFile = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("MCP token file must contain only a dedicated token"))?;
    if !valid_token(token.token.expose()) {
        return Err(invalid(
            "MCP token must be zmcp1_ followed by 64 lowercase hexadecimal digits",
        ));
    }
    Ok(*blake3::hash(token.token.expose().as_bytes()).as_bytes())
}
#[derive(Clone)]
struct Guard {
    hash: blake3::Hash,
    authority: String,
    origin: String,
    requests: Arc<Semaphore>,
    request_time: Duration,
}
fn rejection(status: StatusCode, message: &'static str) -> Response {
    let mut response = (status, message).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            header::HeaderValue::from_static("Bearer realm=\"Zeus MCP\""),
        );
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, header::HeaderValue::from_static("1"));
    }
    response
}
async fn protect(State(state): State<Guard>, request: Request, next: Next) -> Response {
    if request.uri().query().is_some() {
        return rejection(
            StatusCode::BAD_REQUEST,
            "MCP endpoint does not accept URL parameters",
        );
    }
    let headers = request.headers();
    if headers.get_all(header::HOST).iter().count() != 1
        || headers.get(header::HOST).and_then(|v| v.to_str().ok()) != Some(state.authority.as_str())
    {
        return rejection(
            StatusCode::FORBIDDEN,
            "MCP Host is not the configured loopback authority",
        );
    }
    if headers.get_all(header::ORIGIN).iter().count() > 1
        || headers
            .get(header::ORIGIN)
            .is_some_and(|v| v.to_str().ok() != Some(state.origin.as_str()))
    {
        return rejection(StatusCode::FORBIDDEN, "MCP Origin is not allowed");
    }
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token);
    if headers.get_all(header::AUTHORIZATION).iter().count() != 1
        || bearer
            .is_none_or(|token| !valid_token(token) || blake3::hash(token.as_bytes()) != state.hash)
    {
        return rejection(
            StatusCode::UNAUTHORIZED,
            "MCP bearer authentication required",
        );
    }
    let Ok(permit) = state.requests.clone().try_acquire_owned() else {
        return rejection(
            StatusCode::TOO_MANY_REQUESTS,
            "MCP request capacity reached",
        );
    };
    let deadline = tokio::time::Instant::now() + state.request_time;
    let response = match tokio::time::timeout_at(deadline, next.run(request)).await {
        Ok(response) => response,
        Err(_) => return rejection(StatusCode::REQUEST_TIMEOUT, "MCP request deadline elapsed"),
    };
    // SDK text errors can include deserializer input. Keep the HTTP status and Allow header,
    // but do not echo private request bytes; structured JSON-RPC errors retain their protocol.
    let response = if response.status().is_client_error()
        && !response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|v| v.to_str().is_ok_and(|s| s.starts_with("application/json")))
    {
        let mut sanitized = rejection(response.status(), "MCP HTTP request rejected");
        if let Some(allow) = response.headers().get(header::ALLOW) {
            sanitized.headers_mut().insert(header::ALLOW, allow.clone());
        }
        sanitized
    } else {
        response
    };
    let (mut parts, body) = response.into_parts();
    parts.headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    parts.headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    // An SSE/body stream retains its slot until EOF or disconnect, not merely until headers arrive.
    let body = futures::stream::unfold(
        (body.into_data_stream(), permit, deadline),
        |(mut stream, permit, deadline)| async move {
            tokio::select! {
                _=tokio::time::sleep_until(deadline)=>None,
                next=stream.next()=>next.map(|chunk|(chunk,(stream,permit,deadline))),
            }
        },
    );
    Response::from_parts(parts, Body::from_stream(body))
}
fn router<C: Client + 'static>(
    client: Arc<C>,
    address: SocketAddr,
    hash: [u8; 32],
    cancel: CancellationToken,
) -> Router {
    router_with_deadline(client, address, hash, cancel, REQUEST_TIME)
}
fn router_with_deadline<C: Client + 'static>(
    client: Arc<C>,
    address: SocketAddr,
    hash: [u8; 32],
    cancel: CancellationToken,
    request_time: Duration,
) -> Router {
    let authority = address.to_string();
    let origin = format!("http://{authority}");
    let mut settings = StreamableHttpServerConfig::default().enforce_origin_validation();
    settings.allowed_hosts = vec![authority.clone()];
    settings.allowed_origins = vec![origin.clone()];
    settings.max_request_body_bytes = MAX_BODY;
    settings.legacy_session_mode = false;
    settings.json_response = true;
    settings.cancellation_token = cancel;
    let service = StreamableHttpService::new(
        move || {
            Ok(crate::transport::McpServer {
                client: client.clone(),
            })
        },
        Arc::new(LocalSessionManager::default()),
        settings,
    );
    Router::new()
        .route_service("/mcp", service)
        .layer(middleware::from_fn_with_state(
            Guard {
                hash: blake3::Hash::from(hash),
                authority,
                origin,
                requests: Arc::new(Semaphore::new(32)),
                request_time,
            },
            protect,
        ))
}
/// Serve on an already-bound loopback socket. Useful for embedding/tests without a port race;
/// `cancel` drains the HTTP service and does not change daemon grants or pending approvals.
pub async fn serve_on<C: Client + 'static>(
    client: Arc<C>,
    listener: tokio::net::TcpListener,
    hash: [u8; 32],
    cancel: CancellationToken,
) -> ApiResult<()> {
    let address = listener
        .local_addr()
        .map_err(|_| invalid("MCP listener address unavailable"))?;
    if address.ip() != std::net::IpAddr::V4(Ipv4Addr::LOCALHOST) {
        return Err(invalid("MCP HTTP binds only to 127.0.0.1"));
    }
    let app = router(client, address, hash, cancel.clone());
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { cancel.cancelled().await })
        .await
        .map_err(|_| ApiError::internal("MCP HTTP service stopped unexpectedly"))
}
/// Run an optional MCP process listener until Ctrl-C. This never adds a listener to the daemon.
pub async fn serve<C: Client + 'static>(
    client: Arc<C>,
    port: u16,
    hash: [u8; 32],
) -> ApiResult<()> {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|_| ApiError::internal("MCP loopback port could not be bound"))?;
    let address = listener
        .local_addr()
        .map_err(|_| ApiError::internal("MCP loopback address unavailable"))?;
    tracing::info!(%address,"Authenticated MCP HTTP endpoint is /mcp");
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let signal = tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        stop.cancel();
    });
    let result = serve_on(client, listener, hash, cancel).await;
    signal.abort();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use zeus_api::{
        ClientKind, ClientRole,
        methods::daemon::{AuthenticateResult, ConnectionLimits},
    };
    struct Fake {
        session: AuthenticateResult,
        tx: tokio::sync::broadcast::Sender<zeus_api::jsonrpc::Notification>,
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait::async_trait]
    impl Client for Fake {
        async fn call_raw(&self, _: &str, _: Value) -> ApiResult<Value> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(ApiError::invalid_params(
                "fixture daemon operation unavailable",
            ))
        }
        fn notifications(
            &self,
        ) -> tokio::sync::broadcast::Receiver<zeus_api::jsonrpc::Notification> {
            self.tx.subscribe()
        }
        fn session(&self) -> &AuthenticateResult {
            &self.session
        }
    }
    fn fake() -> Arc<Fake> {
        let (tx, _) = tokio::sync::broadcast::channel(4);
        Arc::new(Fake {
            session: AuthenticateResult {
                client_id: "http-fixture".into(),
                kind: ClientKind::Agent,
                credential_kind: ClientKind::Agent,
                downgraded: false,
                role: ClientRole::Mcp,
                permissions: vec![],
                limits: ConnectionLimits::STANDARD,
                connection_id: zeus_types::IdGenerator::new().connection_id(),
            },
            tx,
            calls: std::sync::atomic::AtomicUsize::new(0),
        })
    }
    struct Fixture {
        url: String,
        token: String,
        cancel: CancellationToken,
        task: tokio::task::JoinHandle<()>,
        fake: Arc<Fake>,
        client: reqwest::Client,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.cancel.cancel();
            self.task.abort();
        }
    }
    impl Fixture {
        async fn new(deadline: Duration) -> Self {
            let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            let address = listener.local_addr().unwrap();
            let fake = fake();
            let token = format!("zmcp1_{}", "a".repeat(64));
            let cancel = CancellationToken::new();
            let shutdown = cancel.clone();
            let app = router_with_deadline(
                fake.clone(),
                address,
                *blake3::hash(token.as_bytes()).as_bytes(),
                cancel.clone(),
                deadline,
            );
            let task = tokio::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(async move { shutdown.cancelled().await })
                    .await
                    .unwrap();
            });
            Self {
                url: format!("http://{address}/mcp"),
                token,
                cancel,
                task,
                fake,
                client: reqwest::Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .unwrap(),
            }
        }
        fn post(&self) -> reqwest::RequestBuilder {
            self.client
                .post(&self.url)
                .bearer_auth(&self.token)
                .header("Accept", "application/json, text/event-stream")
                .header("MCP-Protocol-Version", "2025-11-25")
                .header("Content-Type", "application/json")
        }
    }
    fn init() -> Value {
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}})
    }
    #[tokio::test]
    async fn authenticated_stateless_http_uses_real_sdk_and_exposes_only_existing_tools() {
        let f = Fixture::new(REQUEST_TIME).await;
        let response = f.post().json(&init()).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get("mcp-session-id").is_none());
        let result: Value = response.json().await.unwrap();
        assert_eq!(result["result"]["serverInfo"]["name"], "zeus");
        let response = f
            .post()
            .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let result: Value = response.json().await.unwrap();
        assert_eq!(
            result["result"]["tools"].as_array().unwrap().len(),
            crate::catalogue::catalogue().len()
        );
        assert!(
            !result["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "approvals.decide")
        );
        let response = f
            .post()
            .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert!(response.bytes().await.unwrap().is_empty());
        let response = f
            .client
            .get(&f.url)
            .bearer_auth(&f.token)
            .header("Accept", "text/event-stream")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(f.fake.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn current_protocol_metadata_is_stateless_and_header_mismatch_is_rejected() {
        let f = Fixture::new(REQUEST_TIME).await;
        let message = json!({"jsonrpc":"2.0","id":11,"method":"tools/list","params":{"_meta":{
            "io.modelcontextprotocol/protocolVersion":"2026-07-28",
            "io.modelcontextprotocol/clientCapabilities":{},
            "io.modelcontextprotocol/clientInfo":{"name":"current-fixture","version":"1"}
        }}});
        let response = f
            .client
            .post(&f.url)
            .bearer_auth(&f.token)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2026-07-28")
            .header("Mcp-Method", "tools/list")
            .json(&message)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get("mcp-session-id").is_none());
        let value: Value = response.json().await.unwrap();
        assert_eq!(value["result"]["resultType"], "complete", "{value}");
        assert_eq!(
            value["result"]["tools"].as_array().unwrap().len(),
            crate::catalogue::catalogue().len()
        );
        let response = f
            .client
            .post(&f.url)
            .bearer_auth(&f.token)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2026-07-28")
            .header("Mcp-Method", "tools/call")
            .json(&message)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let value: Value = response.json().await.unwrap();
        assert_eq!(value["error"]["code"], -32020);
        assert_eq!(f.fake.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn authentication_origin_host_and_body_failures_never_reach_the_daemon() {
        let f = Fixture::new(REQUEST_TIME).await;
        let response = f.client.post(&f.url).json(&init()).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = f
            .client
            .post(&f.url)
            .bearer_auth(format!("zmcp1_{}", "b".repeat(64)))
            .json(&init())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = f
            .post()
            .header("Origin", "https://evil.invalid")
            .json(&init())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let response = f
            .post()
            .header("Host", "evil.invalid")
            .json(&init())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let response = f
            .post()
            .header("Origin", "null")
            .json(&init())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let response = f
            .client
            .post(format!("{}?token=URL-CANARY", f.url))
            .bearer_auth(&f.token)
            .json(&init())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!response.text().await.unwrap().contains("URL-CANARY"));
        let response = f
            .post()
            .header("Content-Type", "application/json")
            .body("x".repeat(MAX_BODY + 1))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let response = f
            .post()
            .json(&json!([init(), init()]))
            .send()
            .await
            .unwrap();
        // The pinned SDK accepts a single JSON-RPC message and rejects arrays with HTTP 415.
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        let response = f.post().body("BODY-CANARY-private").send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(!response.text().await.unwrap().contains("BODY-CANARY"));
        assert_eq!(f.fake.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn incomplete_bodies_have_a_deadline_and_inflight_requests_are_bounded() {
        let f = Fixture::new(Duration::from_secs(1)).await;
        let mut pending = vec![];
        for _ in 0..33 {
            let client = f.client.clone();
            let url = f.url.clone();
            let token = f.token.clone();
            pending.push(tokio::spawn(async move {
                let body = futures::stream::once(async { Ok::<_, std::io::Error>("{") })
                    .chain(futures::stream::pending());
                client
                    .post(url)
                    .bearer_auth(token)
                    .header("Accept", "application/json, text/event-stream")
                    .header("Content-Type", "application/json")
                    .body(reqwest::Body::wrap_stream(body))
                    .send()
                    .await
                    .unwrap()
                    .status()
            }));
        }
        let mut limited = 0;
        let mut timed_out = 0;
        for task in pending {
            match task.await.unwrap() {
                StatusCode::TOO_MANY_REQUESTS => limited += 1,
                StatusCode::REQUEST_TIMEOUT => timed_out += 1,
                other => panic!("unexpected HTTP result {other}"),
            }
        }
        assert!(limited >= 1);
        assert!(timed_out >= 1);
        assert_eq!(f.fake.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
    #[test]
    fn malformed_or_ipc_token_files_are_rejected_without_echoing_secrets() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("token.json");
        for value in [
            json!({"token":"zt1_IPC-CANARY"}),
            json!({"token":format!("zmcp1_{}","a".repeat(64)),"extra":true}),
            json!({"token":"SHORT-CANARY"}),
        ] {
            std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            let error = token_file_hash(&path).unwrap_err();
            assert!(!error.to_string().contains("CANARY"));
        }
    }
}
