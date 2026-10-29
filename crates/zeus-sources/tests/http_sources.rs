//! Real loopback HTTP behind a test-only route adapter. Production URLs/scopes
//! remain canonical; only the injected socket transport maps them to127.0.0.1.
#![cfg(all(feature = "s3", feature = "hugging-face"))]
use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Method, Response, StatusCode},
};
use futures::StreamExt;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;
use zeus_http::{HttpRequest, HttpResponse, HttpTransport, TransportError};
use zeus_sources::*;

#[derive(Default)]
struct Fixture {
    requests: Mutex<Vec<(String, String, HeaderMap)>>,
    changed: AtomicBool,
}
async fn serve(
    State(state): State<Arc<Fixture>>,
    method: Method,
    uri: axum::http::Uri,
    headers: HeaderMap,
) -> Response<Body> {
    let host = headers
        .get("x-test-target-host")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    state
        .requests
        .lock()
        .unwrap()
        .push((method.to_string(), host.clone(), headers));
    let mut response = Response::builder();
    if host == "huggingface.co" {
        return response
            .status(StatusCode::FOUND)
            .header(
                "location",
                "https://cas-bridge.xethub.hf.co/blob?token=ZEUS-CANARY-signed",
            )
            .header("x-repo-commit", "a".repeat(40))
            .header("x-linked-size", "7")
            .header(
                "x-linked-etag",
                hex::encode(sha2::Sha256::digest(b"payload")),
            )
            .body(Body::empty())
            .unwrap();
    }
    if host.ends_with("amazonaws.com") {
        assert!(uri.path().ends_with("/file.bin"));
        response = response
            .header(
                "x-amz-version-id",
                if state.changed.load(Ordering::Acquire) {
                    "changed"
                } else {
                    "version-1"
                },
            )
            .header("etag", "\"v1\"");
    } else {
        assert_eq!(host, "cas-bridge.xethub.hf.co");
    }
    response
        .status(StatusCode::OK)
        .header("content-length", "7")
        .header("accept-ranges", "bytes")
        .body(if method == Method::HEAD {
            Body::empty()
        } else {
            Body::from("payload")
        })
        .unwrap()
}
struct Route {
    base: url::Url,
    inner: zeus_http::ReqwestTransport,
}
#[async_trait]
impl HttpTransport for Route {
    async fn send(&self, mut request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let host = request.url.host_str().unwrap().to_owned();
        request
            .headers
            .insert("x-test-target-host", host.parse().unwrap());
        request.url = self.base.join(request.url.path()).unwrap();
        self.inner.send(request).await
    }
}
struct Authority {
    allowed: AtomicBool,
    charged: AtomicU64,
    maximum: u64,
}
impl SourceAuthority for Authority {
    fn remaining(&self) -> SourceResult<Duration> {
        if self.allowed.load(Ordering::Acquire) {
            Ok(Duration::from_secs(20))
        } else {
            Err(SourceError::Denied)
        }
    }
    fn charge(&self, bytes: u64) -> SourceResult<()> {
        self.remaining()?;
        // Retain compatibility with Rust 1.90, before AtomicU64::try_update.
        #[allow(deprecated)]
        self.charged
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|next| *next <= self.maximum)
            })
            .map_err(|_| SourceError::Budget)?;
        Ok(())
    }
}
async fn fixture() -> (
    Arc<Fixture>,
    Arc<dyn HttpTransport>,
    tokio::task::JoinHandle<()>,
) {
    let fixture = Arc::new(Fixture::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let router = Router::new().fallback(serve).with_state(fixture.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let inner = zeus_http::ReqwestTransport::new(&Default::default()).unwrap();
    (fixture, Arc::new(Route { base, inner }), server)
}
fn authority(maximum: u64) -> Arc<Authority> {
    Arc::new(Authority {
        allowed: AtomicBool::new(true),
        charged: AtomicU64::new(0),
        maximum,
    })
}
fn virtual_url() -> url::Url {
    url::Url::parse("https://sources.zeus.invalid/sop_01?binding=private").unwrap()
}
fn request() -> HttpRequest {
    HttpRequest {
        method: Method::GET,
        url: virtual_url(),
        headers: HeaderMap::new(),
        head_timeout: Duration::from_secs(10),
        idle_timeout: Duration::from_secs(10),
    }
}
use sha2::Digest;

#[tokio::test]
async fn s3_native_head_and_get_are_separately_signed_and_version_changes_fail_closed() {
    let (fixture, inner, server) = fixture().await;
    let authority = authority(100);
    let config = SourceAccountConfig::S3 {
        region: "us-east-1".into(),
        bucket: "examplebucket".into(),
        key_prefix: "allowed/".into(),
    };
    let target = SourceTarget::S3 {
        bucket: "examplebucket".into(),
        key: "allowed/file.bin".into(),
        version_id: Some("version-1".into()),
    };
    let secret = SourceSecret::new(
        r#"{"access_key_id":"ZEUS-CANARY-key","secret_access_key":"ZEUS-CANARY-secret"}"#.into(),
    );
    let prepare = || BoundSource::prepare(&config, &target, Some(&secret)).unwrap();
    let inspect =
        SourceTransport::new(prepare(), virtual_url(), inner.clone(), authority.clone()).unwrap();
    let metadata = inspect.inspect().await.unwrap();
    assert_eq!(metadata.bytes, 7);
    assert!(metadata.sha256.is_none());
    let transport = SourceTransport::new(
        prepare().pin(metadata).unwrap(),
        virtual_url(),
        inner,
        authority.clone(),
    )
    .unwrap();
    let mut response = transport.send(request()).await.unwrap();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.body.next().await {
        bytes.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(bytes, b"payload");
    assert_eq!(authority.charged.load(Ordering::Acquire), 7);
    let requests = fixture.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].0, "HEAD");
    assert_eq!(requests[1].0, "GET");
    assert_ne!(
        requests[0].2["authorization"],
        requests[1].2["authorization"]
    );
    assert_eq!(requests[1].2["if-match"], "\"v1\"");
    fixture.changed.store(true, Ordering::Release);
    let refusal = transport.send(request()).await.unwrap_err();
    assert_eq!(refusal.kind, zeus_http::TransportFailure::PolicyDenied);
    assert!(!format!("{transport_error:?}", transport_error = refusal).contains("ZEUS-CANARY"));
    server.abort();
}

#[tokio::test]
async fn hub_commit_pinning_strips_tokens_from_cdn_and_never_returns_signed_addresses() {
    let (fixture, inner, server) = fixture().await;
    let authority = authority(100);
    let config = SourceAccountConfig::HuggingFace {
        repository_type: HubRepositoryType::Model,
        repository: "owner/model".into(),
    };
    let target = SourceTarget::HuggingFace {
        repository_type: HubRepositoryType::Model,
        repository: "owner/model".into(),
        commit: "a".repeat(40),
        path: "file.bin".into(),
    };
    let secret = SourceSecret::new("ZEUS-CANARY-token".into());
    let prepare = || BoundSource::prepare(&config, &target, Some(&secret)).unwrap();
    let inspector =
        SourceTransport::new(prepare(), virtual_url(), inner.clone(), authority.clone()).unwrap();
    let metadata = inspector.inspect().await.unwrap();
    assert_eq!(
        metadata.sha256,
        Some(hex::encode(sha2::Sha256::digest(b"payload")))
    );
    let transport = SourceTransport::new(
        prepare().pin(metadata).unwrap(),
        virtual_url(),
        inner,
        authority,
    )
    .unwrap();
    let mut response = transport.send(request()).await.unwrap();
    assert_eq!(response.url, virtual_url());
    assert!(!response.headers.contains_key("location"));
    assert!(!format!("{response:?}").contains("ZEUS-CANARY"));
    while let Some(chunk) = response.body.next().await {
        chunk.unwrap();
    }
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    for (_, host, headers) in requests.iter() {
        if host == "huggingface.co" {
            assert_eq!(headers["authorization"], "Bearer ZEUS-CANARY-token")
        } else {
            assert!(!headers.contains_key("authorization"));
            assert!(!headers.contains_key("cookie"));
        }
    }
    server.abort();
}

#[tokio::test]
async fn durable_charge_refusal_and_live_revocation_prevent_body_release() {
    let (_, inner, server) = fixture().await;
    let authority = authority(6);
    let config = SourceAccountConfig::S3 {
        region: "us-east-1".into(),
        bucket: "examplebucket".into(),
        key_prefix: "allowed/".into(),
    };
    let target = SourceTarget::S3 {
        bucket: "examplebucket".into(),
        key: "allowed/file.bin".into(),
        version_id: Some("version-1".into()),
    };
    let secret = SourceSecret::new(
        r#"{"access_key_id":"test-key","secret_access_key":"test-secret"}"#.into(),
    );
    let source = BoundSource::prepare(&config, &target, Some(&secret))
        .unwrap()
        .pin(ObjectMetadata {
            bytes: 7,
            strong_etag: Some("\"v1\"".into()),
            sha256: None,
            immutable_revision: "version-1".into(),
            integrity_kind: "version".into(),
        })
        .unwrap();
    let transport = SourceTransport::new(source, virtual_url(), inner, authority.clone()).unwrap();
    let mut response = transport.send(request()).await.unwrap();
    assert!(response.body.next().await.unwrap().is_err());
    assert_eq!(authority.charged.load(Ordering::Acquire), 0);
    authority.allowed.store(false, Ordering::Release);
    assert!(transport.send(request()).await.is_err());
    server.abort();
}
