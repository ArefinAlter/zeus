use super::*;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use zeus_api::methods::daemon::{AuthenticateResult, ConnectionLimits};
use zeus_api::{ApiError, ApiResult, Client};
use zeus_types::{
    HttpUrl, IdGenerator, Job, JobId, JobSpec, JobState, Source, SourceKind, Timestamp,
};

struct Fake {
    session: AuthenticateResult,
    events: tokio::sync::broadcast::Sender<zeus_api::jsonrpc::Notification>,
    jobs: Mutex<HashMap<JobId, Job>>,
    keys: Mutex<HashMap<String, JobId>>,
    calls: AtomicUsize,
    lose_response: AtomicBool,
}
impl Fake {
    fn new() -> Self {
        Self {
            session:serde_json::from_value(json!({"client_id":"cli_test","kind":"agent","credential_kind":"agent","downgraded":false,"role":"a2a","permissions":[],"limits":ConnectionLimits::STANDARD,"connection_id":IdGenerator::new().connection_id()})).unwrap(),
            events:tokio::sync::broadcast::channel(8).0,jobs:Mutex::new(HashMap::new()),keys:Mutex::new(HashMap::new()),calls:AtomicUsize::new(0),lose_response:AtomicBool::new(false),
        }
    }
}
#[async_trait::async_trait]
impl Client for Fake {
    fn session(&self) -> &AuthenticateResult {
        &self.session
    }
    fn notifications(&self) -> tokio::sync::broadcast::Receiver<zeus_api::jsonrpc::Notification> {
        self.events.subscribe()
    }
    async fn call_raw(&self, method: &str, params: Value) -> ApiResult<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match method {
            "jobs.enqueue" => {
                let key = params["idempotency_key"].as_str().unwrap();
                let mut keys = self.keys.lock().unwrap();
                let duplicate = keys.contains_key(key);
                let id = *keys.entry(key.into()).or_insert_with(|| {
                    let id = IdGenerator::new().job_id();
                    let spec = JobSpec::new(
                        HttpUrl::parse(params["jobs"][0]["url"].as_str().unwrap()).unwrap(),
                        Source::new(SourceKind::Agent),
                    );
                    self.jobs
                        .lock()
                        .unwrap()
                        .insert(id, Job::new(id, spec, JobState::Queued, Timestamp::now()));
                    id
                });
                if self.lose_response.swap(false, Ordering::SeqCst) {
                    return Err(ApiError::internal("ZEUS-CANARY backend secret"));
                }
                Ok(
                    json!({"jobs":[{"job_id":id,"state":"queued","deduplicated":duplicate}],"dry_run":false}),
                )
            }
            "jobs.get" => {
                assert_eq!(params["reveal_urls"], false);
                let id: JobId = serde_json::from_value(params["job_id"].clone()).unwrap();
                let jobs = self.jobs.lock().unwrap();
                let job = jobs.get(&id).unwrap();
                Ok(json!({"job":zeus_api::view::JobView::from(job)}))
            }
            "jobs.control" => {
                assert_eq!(params["action"], "cancel");
                let id: JobId = serde_json::from_value(params["job_id"].clone()).unwrap();
                let mut jobs = self.jobs.lock().unwrap();
                let job = jobs.get_mut(&id).unwrap();
                job.state = JobState::Canceled;
                Ok(json!({"job":job.summary()}))
            }
            _ => panic!("unexpected backend method {method}"),
        }
    }
}
fn config(dir: &std::path::Path) -> Config {
    Config {
        bind: "127.0.0.1:4750".parse().unwrap(),
        peers: vec![
            Peer {
                id: "alpha".into(),
                token: "aa".repeat(32),
            },
            Peer {
                id: "beta".into(),
                token: "bb".repeat(32),
            },
        ],
        allowed_origins: Default::default(),
        index_path: dir.join("index.redb"),
    }
}
fn request(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
}
fn submit(key: &str) -> Value {
    request(
        "SendMessage",
        json!({"message":{"messageId":format!("message-{key}"),"role":"ROLE_USER","parts":[{"data":{"skill":"fetch_file","job":{"url":"https://example.org/file?token=ZEUS-CANARY-source"}}}]},"configuration":{"returnImmediately":true},"metadata":{"idempotencyKey":key}}),
    )
}

#[tokio::test]
async fn ownership_and_restart_idempotency_are_durable() {
    let dir = tempfile::tempdir().unwrap();
    let client = Arc::new(Fake::new());
    let bridge = Bridge::open(config(dir.path()), client.clone()).unwrap();
    let first = bridge.rpc("alpha", submit("one")).await;
    let id = first["result"]["task"]["id"].as_str().unwrap().to_string();
    assert!(!first.to_string().contains("ZEUS-CANARY"));
    let before = client.calls.load(Ordering::SeqCst);
    for method in ["GetTask", "CancelTask"] {
        assert_eq!(
            bridge.rpc("beta", request(method, json!({"id":id}))).await["error"]["code"],
            -32001
        );
    }
    assert_eq!(client.calls.load(Ordering::SeqCst), before);
    assert_eq!(
        bridge.rpc("beta", request("ListTasks", json!({}))).await["result"]["totalSize"],
        0
    );
    drop(bridge);
    let bridge = Bridge::open(config(dir.path()), client.clone()).unwrap();
    assert_eq!(
        bridge.rpc("alpha", submit("one")).await["result"]["task"]["id"],
        id
    );
    assert_eq!(client.jobs.lock().unwrap().len(), 1);
    let mut conflict = submit("one");
    conflict["params"]["message"]["parts"][0]["data"]["job"]["url"] =
        json!("https://example.org/changed");
    assert_eq!(bridge.rpc("alpha", conflict).await["error"]["code"], -32602);
    let canceled = bridge
        .rpc("alpha", request("CancelTask", json!({"id":id})))
        .await;
    assert_eq!(canceled["result"]["status"]["state"], "TASK_STATE_CANCELED");
    assert_eq!(
        bridge
            .rpc("alpha", request("CancelTask", json!({"id":id})))
            .await["result"]["status"]["state"],
        "TASK_STATE_CANCELED"
    );
}

#[tokio::test]
async fn lost_enqueue_response_recovers_with_the_same_daemon_key() {
    let dir = tempfile::tempdir().unwrap();
    let client = Arc::new(Fake::new());
    client.lose_response.store(true, Ordering::SeqCst);
    let bridge = Bridge::open(config(dir.path()), client.clone()).unwrap();
    let failed = bridge.rpc("alpha", submit("lost")).await;
    assert!(failed.get("error").is_some());
    assert!(!failed.to_string().contains("ZEUS-CANARY"));
    drop(bridge);
    let bridge = Bridge::open(config(dir.path()), client.clone()).unwrap();
    assert!(bridge.rpc("alpha", submit("lost")).await["result"]["task"]["id"].is_string());
    assert_eq!(client.jobs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn human_approval_cannot_be_supplied_and_completed_artifacts_are_metadata_only() {
    let dir = tempfile::tempdir().unwrap();
    let client = Arc::new(Fake::new());
    let bridge = Bridge::open(config(dir.path()), client.clone()).unwrap();
    let sent = bridge.rpc("alpha", submit("safe")).await;
    let id = sent["result"]["task"]["id"].clone();
    {
        let mut jobs = client.jobs.lock().unwrap();
        let job = jobs.values_mut().next().unwrap();
        job.state = JobState::AwaitingApproval;
        job.approval_id = Some(IdGenerator::new().approval_id());
    }
    let waiting = bridge
        .rpc("alpha", request("GetTask", json!({"id":id})))
        .await;
    assert_eq!(
        waiting["result"]["status"]["state"],
        "TASK_STATE_INPUT_REQUIRED"
    );
    assert_eq!(waiting["result"]["status"]["message"]["role"], "ROLE_AGENT");
    assert_eq!(
        bridge
            .rpc(
                "alpha",
                request(
                    "approvals.decide",
                    json!({"approval_id":"x","decision":"approve"})
                )
            )
            .await["error"]["code"],
        -32601
    );
    let mut injected = submit("injected");
    injected["params"]["message"]["parts"][0]["data"]["job"]["extra_headers"] =
        json!({"Cookie":"ZEUS-CANARY"});
    assert_eq!(bridge.rpc("alpha", injected).await["error"]["code"], -32602);
    {
        let mut jobs = client.jobs.lock().unwrap();
        let job = jobs.values_mut().next().unwrap();
        job.state = JobState::Completed;
        job.result = Some(zeus_types::JobResult {
            path: "C:/ZEUS-CANARY-private/file".into(),
            size_bytes: 4,
            sha256: zeus_types::Hex256::from_hex(&"11".repeat(32)).unwrap(),
            blake3: zeus_types::Hex256::from_hex(&"22".repeat(32)).unwrap(),
            detected_mime: None,
            completed_at: Timestamp::now(),
        });
    }
    let completed = bridge
        .rpc("alpha", request("GetTask", json!({"id":id})))
        .await;
    assert!(!completed.to_string().contains("ZEUS-CANARY"));
    assert!(!completed.to_string().contains("file:///"));
    assert_eq!(
        completed["result"]["artifacts"][0]["parts"][0]["data"]["verified"],
        true
    );
    assert_eq!(
        bridge
            .rpc("alpha", request("CancelTask", json!({"id":id})))
            .await["error"]["code"],
        -32002
    );
}

async fn http(bridge: &Bridge, text: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (server, mut client) = tokio::io::duplex(128 * 1024);
    let exchange = async {
        client.write_all(text.as_bytes()).await.unwrap();
        client.shutdown().await.unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).await.unwrap();
        response
    };
    let (_, response) = tokio::join!(crate::transport::connection(server, bridge), exchange);
    response
}
#[tokio::test]
async fn http_auth_host_origin_and_bounds_are_checked_before_daemon_calls() {
    let dir = tempfile::tempdir().unwrap();
    let client = Arc::new(Fake::new());
    let bridge = Bridge::open(config(dir.path()), client.clone()).unwrap();
    let auth = format!("Authorization: Bearer {}\r\n", "aa".repeat(32));
    for (headers, status) in [
        ("Host: 127.0.0.1:4750\r\n".to_string(), 401),
        (format!("Host: evil.example\r\n{auth}"), 403),
        (
            format!("Host: 127.0.0.1:4750\r\nOrigin: null\r\n{auth}"),
            403,
        ),
        (
            format!("Host: 127.0.0.1:4750\r\nOrigin: https://evil.example\r\n{auth}"),
            403,
        ),
        (
            format!("Host: 127.0.0.1:4750\r\nHost: 127.0.0.1:4750\r\n{auth}"),
            400,
        ),
        (
            format!("Host: 127.0.0.1:4750\r\nTransfer-Encoding: chunked\r\n{auth}"),
            400,
        ),
    ] {
        let response=http(&bridge,&format!("POST /a2a HTTP/1.1\r\n{headers}Content-Length: 100000\r\nContent-Type: application/json\r\n\r\n")).await;
        assert!(
            response.starts_with(&format!("HTTP/1.1 {status}")),
            "{response}"
        );
    }
    let response=http(&bridge,&format!("POST /a2a HTTP/1.1\r\nHost: 127.0.0.1:4750\r\n{auth}Content-Length: 1048577\r\nContent-Type: application/json\r\n\r\n")).await;
    assert!(response.starts_with("HTTP/1.1 413"));
    assert_eq!(client.calls.load(Ordering::SeqCst), 0);
    let card = http(
        &bridge,
        "GET /.well-known/agent-card.json HTTP/1.1\r\nHost: 127.0.0.1:4750\r\n\r\n",
    )
    .await;
    assert!(card.starts_with("HTTP/1.1 200"));
    assert!(card.contains("fetch_file"));
    for unsupported in [
        "rescue_download",
        "organize_files",
        "convert_files",
        "ZEUS-CANARY",
    ] {
        assert!(!card.contains(unsupported));
    }
    let body = submit("over-http").to_string();
    let result=http(&bridge,&format!("POST /a2a HTTP/1.1\r\nHost: 127.0.0.1:4750\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len())).await;
    assert!(result.starts_with("HTTP/1.1 200"));
    assert!(result.contains("TASK_STATE_SUBMITTED"));
    assert!(!result.contains("ZEUS-CANARY"));
}

#[test]
fn non_loopback_config_and_duplicate_tokens_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = config(dir.path());
    c.bind = "0.0.0.0:4750".parse().unwrap();
    assert!(c.validate().is_err());
    c.bind = "127.0.0.1:4750".parse().unwrap();
    c.peers[1].token = c.peers[0].token.clone();
    assert!(c.validate().is_err());
}
