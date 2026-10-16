//! Hermetic HTTP protocol, privacy, and durable budget tests; no vendor accounts are used.
#![cfg(feature = "http")]
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeus_llm::{
    http_provider::{HttpDialect, HttpProvider, HttpProviderConfig, Pricing},
    runtime::{ModelRuntime, RequestAuthorization, RuntimeSettings, RuntimeStore},
    secrets::SecretKey,
    *,
};

fn request() -> LlmRequest {
    LlmRequest{task:TaskKind::SuggestName,messages:vec![Message{role:Role::User,content:"name.zip https://user:pass@files.example/item?token=secret\nAuthorization: Bearer secret".into()}],response_schema:None,max_output_tokens:32,privacy:PrivacyTier::MetadataOnly}
}
async fn mock(
    status: u16,
    body: Value,
) -> (
    url::Url,
    tokio::sync::oneshot::Receiver<String>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint =
        url::Url::parse(&format!("http://{}/test", listener.local_addr().unwrap())).unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        let (mut socket, _) =
            tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0; 4096];
        loop {
            let n = socket.read(&mut chunk).await.unwrap();
            assert_ne!(n, 0);
            bytes.extend_from_slice(&chunk[..n]);
            if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|n| n.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        tx.send(String::from_utf8(bytes).unwrap()).unwrap();
        let body = body.to_string();
        let response = format!(
            "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });
    (endpoint, rx, handle)
}
fn config(kind: ProviderKind, dialect: HttpDialect, endpoint: url::Url) -> HttpProviderConfig {
    HttpProviderConfig {
        kind,
        dialect,
        endpoint,
        model: "explicit-model".into(),
        pricing: Some(Pricing {
            input_micro_usd_per_million: 1_000_000,
            output_micro_usd_per_million: 1_000_000,
            max_input_tokens: 4096,
        }),
        timeout_seconds: 5,
    }
}
fn credentials(kind: ProviderKind) -> Arc<MemorySecretStore> {
    let store = Arc::new(MemorySecretStore::default());
    store
        .set(
            &SecretKey {
                provider: kind,
                purpose: "api_key".into(),
            },
            "ZEUS-CANARY-key",
        )
        .unwrap();
    store
}

#[tokio::test]
async fn responses_aggregates_text_after_reasoning_and_disables_storage() {
    let (endpoint,rx,server)=mock(200,json!({"status":"completed","output":[{"type":"reasoning","summary":[]},{"type":"message","content":[{"type":"output_text","text":"renamed"},{"type":"output_text","text":".zip"}]}],"usage":{"input_tokens":12,"output_tokens":3}})).await;
    let provider = HttpProvider::new(
        config(
            ProviderKind::ApiKeyOpenaiCompatible,
            HttpDialect::OpenAiResponses,
            endpoint,
        ),
        credentials(ProviderKind::ApiKeyOpenaiCompatible),
    )
    .unwrap();
    let response = provider.complete(request()).await.unwrap();
    assert_eq!(response.content, LlmContent::Text("renamed.zip".into()));
    let wire = rx.await.unwrap();
    server.await.unwrap();
    let body: Value = serde_json::from_str(wire.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], false);
    assert_eq!(body["model"], "explicit-model");
    assert!(!body.to_string().contains("token=secret"));
    assert!(!body.to_string().contains("user:pass"));
    assert_eq!(response.usage.cost_micro_usd, Some(15));
    assert!(
        wire.to_ascii_lowercase()
            .contains("authorization: bearer zeus-canary-key")
    );
}
#[tokio::test]
async fn anthropic_uses_api_key_system_and_local_schema_validation() {
    let (endpoint,rx,server)=mock(200,json!({"stop_reason":"end_turn","content":[{"type":"text","text":"{\"name\":\"report.pdf\"}"}],"usage":{"input_tokens":20,"output_tokens":8}})).await;
    let provider = HttpProvider::new(
        config(
            ProviderKind::ApiKeyAnthropic,
            HttpDialect::AnthropicMessages,
            endpoint,
        ),
        credentials(ProviderKind::ApiKeyAnthropic),
    )
    .unwrap();
    let mut req = request();
    req.messages.insert(
        0,
        Message {
            role: Role::System,
            content: "Suggest a safe filename".into(),
        },
    );
    req.response_schema = Some(
        json!({"type":"object","required":["name"],"properties":{"name":{"type":"string"}},"additionalProperties":false}),
    );
    let response = provider.complete(req).await.unwrap();
    assert_eq!(
        response.content,
        LlmContent::Json(json!({"name":"report.pdf"}))
    );
    let wire = rx.await.unwrap();
    server.await.unwrap();
    assert!(
        wire.to_ascii_lowercase()
            .contains("x-api-key: zeus-canary-key")
    );
    assert!(wire.contains("anthropic-version: 2023-06-01"));
    let body: Value = serde_json::from_str(wire.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert!(body["system"].as_str().unwrap().contains("safe filename"));
}
#[tokio::test]
async fn ollama_is_loopback_only_and_honors_token_bound() {
    let (endpoint,rx,server)=mock(200,json!({"done":true,"done_reason":"stop","message":{"content":"local.txt"},"prompt_eval_count":2,"eval_count":2})).await;
    let mut cfg = config(ProviderKind::LocalOllama, HttpDialect::Ollama, endpoint);
    cfg.pricing = None;
    let provider = HttpProvider::new(cfg.clone(), Arc::new(MemorySecretStore::default())).unwrap();
    let mut req = request();
    req.privacy = PrivacyTier::LocalOnly;
    assert_eq!(provider.quote(&req).unwrap(), 0);
    provider.complete(req).await.unwrap();
    let wire = rx.await.unwrap();
    server.await.unwrap();
    assert!(wire.contains("\"num_predict\":32"));
    assert!(
        !wire
            .split("\r\n\r\n")
            .next()
            .unwrap()
            .to_lowercase()
            .contains("authorization:")
    );
    cfg.endpoint = url::Url::parse("https://models.example/api/chat").unwrap();
    assert!(HttpProvider::new(cfg, Arc::new(MemorySecretStore::default())).is_err());
}
#[tokio::test]
async fn auth_error_is_opaque_and_invalid_json_schema_does_not_trigger_external_fetch() {
    let (endpoint, rx, server) = mock(401, json!({"error":"ZEUS-CANARY-server-secret"})).await;
    let provider = HttpProvider::new(
        config(
            ProviderKind::ApiKeyOpenaiCompatible,
            HttpDialect::ChatCompletions,
            endpoint,
        ),
        credentials(ProviderKind::ApiKeyOpenaiCompatible),
    )
    .unwrap();
    let error = provider.complete(request()).await.unwrap_err();
    let _ = rx.await;
    server.await.unwrap();
    assert!(matches!(error, LlmError::AuthFailed));
    assert!(!error.to_string().contains("ZEUS-CANARY"));
    assert!(
        zeus_llm::runtime::validate_schema(&json!({"$ref":"http://127.0.0.1:1/secret"})).is_err()
    );
    assert!(zeus_llm::runtime::check_output(&json!({"type":"integer"}), &json!("wrong")).is_err());
}

struct Stub {
    calls: AtomicUsize,
    seen: Mutex<Vec<LlmRequest>>,
    fail: bool,
    cost: u64,
}
#[async_trait]
impl LlmProvider for Stub {
    fn kind(&self) -> ProviderKind {
        ProviderKind::ApiKeyOpenaiCompatible
    }
    fn is_configured(&self) -> bool {
        true
    }
    fn cost_ceiling(&self, _: &LlmRequest) -> Result<u64, LlmError> {
        Ok(60)
    }
    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(request);
        if self.fail {
            return Err(LlmError::ProviderUnavailable("timeout".into()));
        }
        Ok(LlmResponse {
            content: LlmContent::Text("safe.txt".into()),
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
                cost_micro_usd: Some(self.cost),
            },
            provider: self.kind(),
            model: "mock".into(),
        })
    }
}
fn authorization() -> RequestAuthorization {
    RequestAuthorization {
        max_cost_micro_usd: 100,
        ..Default::default()
    }
}
#[tokio::test]
async fn caps_reserve_before_send_refund_known_cost_and_persist_on_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.redb");
    let caps = SpendCaps {
        daily_micro_usd: 100,
        monthly_micro_usd: 100,
    };
    let store = Arc::new(RuntimeStore::open(&path, caps, time::OffsetDateTime::now_utc()).unwrap());
    let provider = Arc::new(Stub {
        calls: AtomicUsize::new(0),
        seen: Mutex::new(Vec::new()),
        fail: false,
        cost: 40,
    });
    let runtime = ModelRuntime::new(
        RuntimeSettings {
            enabled: true,
            ..Default::default()
        },
        store.clone(),
        TrustedKeys::pinned(),
        vec![provider.clone()],
    )
    .unwrap();
    runtime.complete(request(), authorization()).await.unwrap();
    runtime.complete(request(), authorization()).await.unwrap();
    assert!(matches!(
        runtime.complete(request(), authorization()).await,
        Err(LlmError::BudgetExceeded { .. })
    ));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(store.spend().unwrap().day_spent(), 80);
    assert!(
        !provider.seen.lock().unwrap()[0].messages[0]
            .content
            .contains("Bearer secret")
    );
    drop(runtime);
    drop(store);
    let reopened =
        RuntimeStore::open(&path, SpendCaps::default(), time::OffsetDateTime::now_utc()).unwrap();
    assert_eq!(reopened.spend().unwrap().day_spent(), 80);
    assert_eq!(reopened.spend().unwrap().caps(), caps);
}
#[tokio::test]
async fn content_consent_and_default_off_prevent_model_calls_and_ambiguous_failures_stay_charged() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(
        RuntimeStore::open(
            &temp.path().join("state"),
            SpendCaps {
                daily_micro_usd: 100,
                monthly_micro_usd: 100,
            },
            time::OffsetDateTime::now_utc(),
        )
        .unwrap(),
    );
    let provider = Arc::new(Stub {
        calls: AtomicUsize::new(0),
        seen: Mutex::new(Vec::new()),
        fail: true,
        cost: 0,
    });
    let off = ModelRuntime::new(
        RuntimeSettings::default(),
        store.clone(),
        TrustedKeys::pinned(),
        vec![provider.clone()],
    )
    .unwrap();
    assert!(matches!(
        off.complete(request(), authorization()).await,
        Err(LlmError::NotConfigured)
    ));
    let on = ModelRuntime::new(
        RuntimeSettings {
            enabled: true,
            ..Default::default()
        },
        store.clone(),
        TrustedKeys::pinned(),
        vec![provider.clone()],
    )
    .unwrap();
    let auth = RequestAuthorization {
        contains_content: true,
        ..authorization()
    };
    assert!(matches!(
        on.complete(request(), auth).await,
        Err(LlmError::PrivacyViolation { .. })
    ));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert!(on.complete(request(), authorization()).await.is_err());
    assert_eq!(store.spend().unwrap().day_spent(), 60);
    assert!(matches!(
        on.complete(request(), authorization()).await,
        Err(LlmError::BudgetExceeded { .. })
    ));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn inaccurate_quote_blocks_further_paid_requests() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(
        RuntimeStore::open(
            &temp.path().join("state"),
            SpendCaps::default(),
            time::OffsetDateTime::now_utc(),
        )
        .unwrap(),
    );
    let provider = Arc::new(Stub {
        calls: AtomicUsize::new(0),
        seen: Mutex::new(Vec::new()),
        fail: false,
        cost: 70,
    });
    let runtime = ModelRuntime::new(
        RuntimeSettings {
            enabled: true,
            ..Default::default()
        },
        store.clone(),
        TrustedKeys::pinned(),
        vec![provider.clone()],
    )
    .unwrap();
    assert!(matches!(
        runtime.complete(request(), authorization()).await,
        Err(LlmError::DisabledByPolicy(_))
    ));
    assert!(runtime.complete(request(), authorization()).await.is_err());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(store.spend().unwrap().day_spent(), 70);
    store
        .configure(serde_json::json!({"enabled":true}), None)
        .unwrap();
    assert!(
        runtime
            .status()
            .unwrap()
            .providers
            .iter()
            .all(|p| p.status == ProviderStatus::Disabled)
    );
    store
        .configure_reviewed(serde_json::json!({"pricing_reviewed":true}), None, true)
        .unwrap();
    assert_eq!(store.spend().unwrap().day_spent(), 70);
    assert!(
        runtime
            .status()
            .unwrap()
            .providers
            .iter()
            .any(|p| p.status != ProviderStatus::Disabled)
    );
}
#[test]
fn redaction_covers_all_bearers_case_insensitive_urls_and_local_only_excludes_host() {
    let redacted =
        redact_text("Bearer first, BEARER second https://u:p@x/p?q=third HTTPS://u:p@x/p?q=fourth");
    for secret in ["first", "second", "third", "fourth", "u:p"] {
        assert!(!redacted.contains(secret), "{redacted}")
    }
    assert!(
        PrivacyTier::LocalOnly
            .check_provider(ProviderKind::HostAgent)
            .is_err()
    );
}

#[tokio::test]
async fn unfinished_chatgpt_stream_never_becomes_success() {
    // This fixture exercises the public stream parser through a test in its owning module;
    // provider-level truncated non-streaming responses are rejected here as well.
    let (endpoint, rx, server) = mock(
        200,
        json!({"status":"incomplete","output":[],"usage":{"input_tokens":2,"output_tokens":1}}),
    )
    .await;
    let provider = HttpProvider::new(
        config(
            ProviderKind::ApiKeyOpenaiCompatible,
            HttpDialect::OpenAiResponses,
            endpoint,
        ),
        credentials(ProviderKind::ApiKeyOpenaiCompatible),
    )
    .unwrap();
    assert!(provider.complete(request()).await.is_err());
    let _ = rx.await;
    server.await.unwrap();
}
