use super::*;
use crate::{DeliveryMode, VerificationEvidence};
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::IntoResponse,
};
use sha2::Sha256;
use std::{
    collections::HashMap,
    io::Cursor,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct Fake {
    base: String,
    path: String,
    total: u64,
    hashes: Vec<String>,
    parts: HashMap<usize, Vec<u8>>,
    object: Option<Vec<u8>>,
    object_id: u64,
    uploaded: Vec<usize>,
    creates: usize,
    requests: usize,
    fail_part: Option<usize>,
    lost_create_reply: bool,
    bad_part_hash: bool,
    bad_host: bool,
    bad_metadata_path: bool,
}

async fn handle(
    State(state): State<Arc<Mutex<Fake>>>,
    request: Request<Body>,
) -> impl IntoResponse {
    let url = Url::parse(&format!("http://localhost{}", request.uri())).unwrap();
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(
        query.get("access_token").map(String::as_str),
        Some("ZEUS-CANARY-baidu")
    );
    assert!(!request.headers().contains_key(http::header::AUTHORIZATION));
    let boundary = request
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split("boundary=").nth(1))
        .map(str::to_owned);
    let body = axum::body::to_bytes(request.into_body(), 5 * 1024 * 1024)
        .await
        .unwrap();
    let form: HashMap<String, String> = url::form_urlencoded::parse(&body).into_owned().collect();
    let mut s = state.lock().unwrap();
    s.requests += 1;
    let mut status = StatusCode::OK;
    let value = match query["method"].as_str() {
        "meta" => {
            if let Some(object) = &s.object {
                json!({"errno":0,"list":[{"path":if s.bad_metadata_path { "/apps/other/file" } else { &query["path"] },
                    "size":object.len(),"isdir":0,"fs_id":s.object_id,"md5":"opaque-cloud-md5"}]})
            } else {
                json!({"errno":-9})
            }
        }
        "precreate" => {
            assert!(!form.contains_key("rtype") || form["rtype"] == "0");
            s.path = form["path"].clone();
            s.total = form["size"].parse().unwrap();
            s.hashes = serde_json::from_str(&form["block_list"]).unwrap();
            json!({"errno":0,"path":s.path,"uploadid":"opaque-upload-id",
                "block_list":(0..s.hashes.len()).filter(|i| !s.parts.contains_key(i)).collect::<Vec<_>>()})
        }
        "locateupload" => {
            assert_eq!(query["uploadid"], "opaque-upload-id");
            assert_eq!(query["path"], s.path);
            json!({"error_code":0,"servers":[{"server":if s.bad_host { "https://attacker.invalid/" } else { &s.base }}]})
        }
        "upload" => {
            let index: usize = query["partseq"].parse().unwrap();
            assert_eq!(query["path"], s.path);
            assert_eq!(query["uploadid"], "opaque-upload-id");
            let boundary = boundary.unwrap();
            let start = body.windows(4).position(|v| v == b"\r\n\r\n").unwrap() + 4;
            let end = body.len() - format!("\r\n--{boundary}--\r\n").len();
            let bytes = body[start..end].to_vec();
            let hash = hex::encode(Md5::digest(&bytes));
            assert_eq!(hash, s.hashes[index]);
            if s.fail_part == Some(index) {
                s.fail_part = None;
                status = StatusCode::SERVICE_UNAVAILABLE;
                json!({"error":"do-not-surface-provider-body"})
            } else {
                s.uploaded.push(index);
                s.parts.insert(index, bytes);
                json!({"md5":if s.bad_part_hash { "00000000000000000000000000000000".to_owned() } else { hash }})
            }
        }
        "create" => {
            assert_eq!(form["rtype"], "0");
            assert_eq!(form["uploadid"], "opaque-upload-id");
            assert_eq!(
                serde_json::from_str::<Vec<String>>(&form["block_list"]).unwrap(),
                s.hashes
            );
            s.creates += 1;
            let bytes = (0..s.hashes.len())
                .flat_map(|i| s.parts.get(&i).unwrap().clone())
                .collect::<Vec<_>>();
            assert_eq!(bytes.len() as u64, s.total);
            s.object = Some(bytes);
            s.object_id = 42;
            if s.lost_create_reply {
                s.lost_create_reply = false;
                status = StatusCode::SERVICE_UNAVAILABLE;
                json!({"error":"the-object-was-created-but-response-was-lost"})
            } else {
                json!({"errno":0,"path":s.path,"size":s.total,"fs_id":42})
            }
        }
        "filemanager" => {
            assert_eq!(query["opera"], "delete");
            assert_eq!(form["async"], "0");
            assert_eq!(
                serde_json::from_str::<Vec<String>>(&form["filelist"]).unwrap(),
                vec![s.path.clone()]
            );
            s.object = None;
            json!({"errno":0,"info":[{"errno":0,"path":s.path}]})
        }
        _ => panic!("unexpected operation"),
    };
    (status, axum::Json(value))
}

struct Harness {
    provider: BaiduNetdisk,
    state: Arc<Mutex<Fake>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Harness {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(Fake {
            base: base.clone(),
            ..Default::default()
        }));
        let router = Router::new().fallback(handle).with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let token = SecretString::new("ZEUS-CANARY-baidu".into());
        let provider = BaiduNetdisk {
            http: ProviderHttp::new(&base, token.clone(), false).unwrap(),
            locate: Url::parse(&base).unwrap(),
            token,
            root: "/apps/Zeus/deliveries".into(),
        };
        Self {
            provider,
            state,
            task,
        }
    }
}

#[derive(Default)]
struct Sink(Mutex<Option<PrivateCheckpoint>>);
#[async_trait]
impl CheckpointSink for Sink {
    async fn save(&self, cp: &PrivateCheckpoint) -> CloudResult<()> {
        *self.0.lock().unwrap() = Some(cp.clone());
        Ok(())
    }
}
fn source(bytes: &[u8]) -> UploadSource {
    Box::new(Cursor::new(bytes.to_vec()))
}
fn request(bytes: &[u8], mode: DeliveryMode) -> UploadRequest {
    UploadRequest {
        operation_id: "cld_01J00000000000000000000000".into(),
        mode,
        name: "sample.bin".into(),
        size: bytes.len() as u64,
        expected_sha256: Some(hex::encode(Sha256::digest(bytes))),
        block_md5: Some(if bytes.is_empty() {
            vec![hex::encode(Md5::digest([]))]
        } else {
            bytes
                .chunks(BAIDU_BLOCK_BYTES as usize)
                .map(|c| hex::encode(Md5::digest(c)))
                .collect()
        }),
    }
}

#[tokio::test]
async fn staged_multipart_commits_exact_bytes_and_owned_cleanup() {
    let h = Harness::new().await;
    let bytes = vec![0x43; BAIDU_BLOCK_BYTES as usize + 17];
    let request = request(&bytes, DeliveryMode::StagedUpload);
    let sink = Sink::default();
    let artifact = h
        .provider
        .upload(
            &request,
            source(&bytes),
            None,
            &sink,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(h.state.lock().unwrap().object.as_ref().unwrap(), &bytes);
    assert_eq!(
        artifact.verification,
        VerificationEvidence::LocalSha256RemoteSize
    );
    assert!(artifact.remote_sha256.is_none());
    assert_eq!(artifact.local_sha256, request.expected_sha256);
    let cp = sink.0.lock().unwrap().clone().unwrap();
    h.provider
        .cleanup(&cp, &CancellationToken::new())
        .await
        .unwrap();
    assert!(h.state.lock().unwrap().object.is_none());
}

#[tokio::test]
async fn stream_with_manifest_never_requires_a_seekable_source() {
    let h = Harness::new().await;
    let bytes = vec![7; 100_000];
    let request = request(&bytes, DeliveryMode::StreamUpload);
    let (mut writer, reader) = tokio::io::duplex(4096);
    let copy = bytes.clone();
    let task = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        writer.write_all(&copy).await.unwrap();
    });
    h.provider
        .upload(
            &request,
            Box::new(reader),
            None,
            &Sink::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    task.await.unwrap();
    assert_eq!(h.state.lock().unwrap().object.as_ref().unwrap(), &bytes);
}

#[tokio::test]
async fn missing_manifest_fails_before_network() {
    let h = Harness::new().await;
    let mut request = request(b"hello", DeliveryMode::StreamUpload);
    request.block_md5 = None;
    let error = h
        .provider
        .upload(
            &request,
            source(b"hello"),
            None,
            &Sink::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        CloudError::Unsupported(CapabilityReason::KnownBlockHashesRequired)
    ));
    assert_eq!(h.state.lock().unwrap().requests, 0);
}

#[tokio::test]
async fn changed_source_or_bad_provider_chunk_hash_never_finalizes() {
    for provider_mismatch in [false, true] {
        let h = Harness::new().await;
        h.state.lock().unwrap().bad_part_hash = provider_mismatch;
        let mut request = request(b"hello", DeliveryMode::StreamUpload);
        if !provider_mismatch {
            request.block_md5 = Some(vec!["0".repeat(32)]);
        }
        assert!(matches!(
            h.provider
                .upload(
                    &request,
                    source(b"hello"),
                    None,
                    &Sink::default(),
                    &CancellationToken::new()
                )
                .await,
            Err(CloudError::HashMismatch)
        ));
        assert_eq!(h.state.lock().unwrap().creates, 0);
    }
}

#[tokio::test]
async fn staged_resume_only_sends_provider_missing_parts() {
    let h = Harness::new().await;
    let bytes = vec![0x55; BAIDU_BLOCK_BYTES as usize + 1];
    let request = request(&bytes, DeliveryMode::StagedUpload);
    h.state.lock().unwrap().fail_part = Some(1);
    let sink = Sink::default();
    assert!(matches!(
        h.provider
            .upload(
                &request,
                source(&bytes),
                None,
                &sink,
                &CancellationToken::new()
            )
            .await,
        Err(CloudError::Http(503))
    ));
    let cp = sink.0.lock().unwrap().clone().unwrap();
    assert_eq!(cp.committed_bytes, BAIDU_BLOCK_BYTES);
    h.provider
        .upload(
            &request,
            source(&bytes),
            Some(cp),
            &sink,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(h.state.lock().unwrap().uploaded, vec![0, 1]);
}

#[tokio::test]
async fn lost_create_response_reconciles_without_duplicate_create() {
    let h = Harness::new().await;
    h.state.lock().unwrap().lost_create_reply = true;
    let request = request(b"hello", DeliveryMode::StagedUpload);
    let sink = Sink::default();
    assert!(matches!(
        h.provider
            .upload(
                &request,
                source(b"hello"),
                None,
                &sink,
                &CancellationToken::new()
            )
            .await,
        Err(CloudError::Http(503))
    ));
    let cp = sink.0.lock().unwrap().clone().unwrap();
    let receipt = h
        .provider
        .reconcile(&cp, &CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.local_sha256, request.expected_sha256);
    h.provider
        .upload(
            &request,
            source(b"hello"),
            Some(cp),
            &sink,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(h.state.lock().unwrap().creates, 1);
}

#[tokio::test]
async fn hostile_upload_host_never_receives_bytes_or_token() {
    let h = Harness::new().await;
    h.state.lock().unwrap().bad_host = true;
    assert!(matches!(
        h.provider
            .upload(
                &request(b"hi", DeliveryMode::StagedUpload),
                source(b"hi"),
                None,
                &Sink::default(),
                &CancellationToken::new()
            )
            .await,
        Err(CloudError::UntrustedLocation)
    ));
    assert!(h.state.lock().unwrap().uploaded.is_empty());
}

#[tokio::test]
async fn existing_file_is_not_replaced_and_replaced_id_is_not_deleted() {
    let h = Harness::new().await;
    let request = request(b"hello", DeliveryMode::StagedUpload);
    let sink = Sink::default();
    h.provider
        .upload(
            &request,
            source(b"hello"),
            None,
            &sink,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(matches!(
        h.provider
            .upload(
                &request,
                source(b"hello"),
                None,
                &sink,
                &CancellationToken::new()
            )
            .await,
        Err(CloudError::AlreadyExists)
    ));
    h.state.lock().unwrap().object_id = 999;
    let cp = sink.0.lock().unwrap().clone().unwrap();
    assert!(matches!(
        h.provider.cleanup(&cp, &CancellationToken::new()).await,
        Err(CloudError::CheckpointMismatch)
    ));
    assert!(h.state.lock().unwrap().object.is_some());
}

#[test]
fn roots_manifests_and_error_redaction_fail_closed() {
    for root in [
        "/",
        "/apps",
        "/apps/",
        "/apps//foo",
        "/apps/Zeus/../other",
        "/apps/Zeus\\other",
    ] {
        assert!(BaiduNetdisk::validate_root(root).is_err(), "{root}");
    }
    assert!(BaiduNetdisk::validate_root("/apps/Zeus/资料").is_ok());
    let mut r = request(b"hello", DeliveryMode::StreamUpload);
    r.block_md5.as_mut().unwrap().push("0".repeat(32));
    assert!(BaiduNetdisk::validate_manifest(&r).is_err());
    let err =
        BaiduNetdisk::check_json(json!({"errno":31024,"errmsg":"ZEUS-CANARY-secret"})).unwrap_err();
    assert!(!format!("{err:?} {err}").contains("ZEUS-CANARY"));
}
