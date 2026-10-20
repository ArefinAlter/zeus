use crate::*;
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Sink(Mutex<Vec<PrivateCheckpoint>>);
#[async_trait]
impl CheckpointSink for Sink {
    async fn save(&self, cp: &PrivateCheckpoint) -> CloudResult<()> {
        self.0.lock().unwrap().push(cp.clone());
        Ok(())
    }
}

#[derive(Debug)]
struct Request {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}
struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}
impl Response {
    fn json(status: u16, body: serde_json::Value) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: serde_json::to_vec(&body).unwrap(),
        }
    }
    fn empty(status: u16) -> Self {
        Self {
            status,
            headers: vec![],
            body: vec![],
        }
    }
    fn header(mut self, key: &str, value: impl ToString) -> Self {
        self.headers.push((key.into(), value.to_string()));
        self
    }
}

async fn server(
    handler: impl Fn(Request, &str) -> Response + Send + Sync + 'static,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let returned = origin.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            let mut byte = [0u8];
            while !head.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).await.unwrap() == 0 {
                    break;
                }
                head.push(byte[0]);
                assert!(head.len() < 32 * 1024);
            }
            if head.is_empty() {
                continue;
            }
            let head = String::from_utf8(head).unwrap();
            let mut lines = head.split("\r\n");
            let first: Vec<_> = lines.next().unwrap().split_whitespace().collect();
            let headers: BTreeMap<_, _> = lines
                .filter_map(|v| v.split_once(':'))
                .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
                .collect();
            let length: usize = headers
                .get("content-length")
                .map(|v| v.parse().unwrap())
                .unwrap_or(0);
            assert!(length <= 2 * 1024 * 1024);
            let mut body = vec![0; length];
            if stream.read_exact(&mut body).await.is_err() {
                continue;
            }
            let response = handler(
                Request {
                    method: first[0].into(),
                    path: first[1].into(),
                    headers,
                    body,
                },
                &origin,
            );
            let mut head = format!(
                "HTTP/1.1 {} Test\r\nContent-Length: {}\r\nConnection: close\r\n",
                response.status,
                response.body.len()
            );
            for (key, value) in response.headers {
                head.push_str(&format!("{key}: {value}\r\n"));
            }
            head.push_str("\r\n");
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(&response.body).await;
            let _ = stream.shutdown().await;
        }
    });
    (returned, task)
}

fn upload(bytes: &[u8], mode: DeliveryMode) -> UploadRequest {
    UploadRequest {
        operation_id: "01Jcloudnative000000000001".into(),
        name: "archive.bin".into(),
        size: bytes.len() as u64,
        mode,
        expected_sha256: Some(hex::encode(Sha256::digest(bytes))),
        block_md5: None,
    }
}

fn drive_metadata(request: &UploadRequest) -> serde_json::Value {
    serde_json::json!({ "id":"generated_file_id", "size":request.size.to_string(), "sha256Checksum":request.expected_sha256,
        "appProperties":{"zeus_operation":request.operation_id} })
}

#[tokio::test]
async fn google_upload_is_chunked_and_saves_identity_before_mutations() {
    let bytes = vec![27u8; 1024 * 1024 + 17];
    let request = upload(&bytes, DeliveryMode::StagedUpload);
    let sink = Arc::new(Sink::default());
    let state = Arc::clone(&sink);
    let wanted = request.clone();
    let received = Arc::new(Mutex::new(Vec::new()));
    let data = Arc::clone(&received);
    let (endpoint, task) = server(move |req, origin| {
        assert_eq!(
            req.headers.get("authorization").map(String::as_str),
            Some("Bearer ZEUS-CANARY-token")
        );
        if req.path.starts_with("/drive/v3/files/generateIds") {
            return Response::json(200, serde_json::json!({"ids":["generated_file_id"]}));
        }
        assert_eq!(
            state.0.lock().unwrap().last().unwrap().object_id.as_deref(),
            Some("generated_file_id")
        );
        if req.method == "POST" {
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&req.body).unwrap()["id"],
                "generated_file_id"
            );
            return Response::empty(200).header(
                "Location",
                format!("{origin}upload/drive/v3/files?upload_id=ZEUS-CANARY-session"),
            );
        }
        assert_eq!(req.method, "PUT");
        let mut data = data.lock().unwrap();
        if data.is_empty() {
            assert_eq!(
                req.headers["content-range"],
                format!("bytes 0-1048575/{}", wanted.size)
            );
        }
        data.extend(req.body);
        if data.len() < wanted.size as usize {
            Response::empty(308).header("Range", "bytes=0-1048575")
        } else {
            Response::json(200, drive_metadata(&wanted))
        }
    })
    .await;
    let provider = GoogleDrive::test_endpoint(&endpoint);
    let result = provider
        .upload(
            &request,
            Box::new(std::io::Cursor::new(bytes.clone())),
            None,
            sink.as_ref(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(*received.lock().unwrap(), bytes);
    assert_eq!(
        result.verification,
        VerificationEvidence::LocalAndRemoteSha256
    );
    let checkpoint = sink.0.lock().unwrap().last().unwrap().clone();
    assert_eq!(checkpoint.phase, CheckpointPhase::Complete);
    assert!(!format!("{checkpoint:?} {provider:?} {result:?}").contains("ZEUS-CANARY"));
    task.abort();
}

#[tokio::test]
async fn google_resume_queries_provider_offset_and_rehashes_original_source() {
    let bytes = vec![13u8; 1024 * 1024 + 9];
    let request = upload(&bytes, DeliveryMode::StagedUpload);
    let expected = request.clone();
    let calls = Arc::new(Mutex::new(0));
    let copy = Arc::clone(&calls);
    let (endpoint, task) = server(move |req, _| {
        assert_eq!(req.method, "PUT");
        let mut count = copy.lock().unwrap();
        *count += 1;
        if *count == 1 {
            assert!(req.body.is_empty());
            assert_eq!(
                req.headers["content-range"],
                format!("bytes */{}", expected.size)
            );
            Response::empty(308).header("Range", "bytes=0-1048575")
        } else {
            assert_eq!(
                req.headers["content-range"],
                format!("bytes 1048576-1048584/{}", expected.size)
            );
            assert_eq!(req.body, vec![13u8; 9]);
            Response::json(200, drive_metadata(&expected))
        }
    })
    .await;
    let cp = PrivateCheckpoint {
        provider: ProviderKind::GoogleDrive,
        request: request.clone(),
        object_path: format!("root/{}", request.unique_name()),
        object_id: Some("generated_file_id".into()),
        session_url: Some(format!("{endpoint}upload/drive/v3/files?upload_id=private")),
        committed_bytes: 0,
        phase: CheckpointPhase::Uploading,
        provider_state: serde_json::Value::Null,
    };
    let result = GoogleDrive::test_endpoint(&endpoint)
        .upload(
            &request,
            Box::new(std::io::Cursor::new(bytes)),
            Some(cp),
            &Sink::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        result.verification,
        VerificationEvidence::LocalAndRemoteSha256
    );
    assert_eq!(*calls.lock().unwrap(), 2);
    task.abort();
}

#[tokio::test]
async fn provider_upload_locations_cannot_redirect_credentials_to_another_origin() {
    let (endpoint, task) = server(|req, _| {
        if req.path.starts_with("/drive/") {
            Response::json(200, serde_json::json!({"ids":["generated_file_id"]}))
        } else {
            Response::empty(200).header(
                "Location",
                "https://evil.invalid/upload/drive/v3/files?ZEUS-CANARY-private",
            )
        }
    })
    .await;
    let request = upload(b"body", DeliveryMode::StreamUpload);
    let result = GoogleDrive::test_endpoint(&endpoint)
        .upload(
            &request,
            Box::new(std::io::Cursor::new(b"body")),
            None,
            &Sink::default(),
            &CancellationToken::new(),
        )
        .await;
    assert!(matches!(result, Err(CloudError::UntrustedLocation)));
    task.abort();
}

#[tokio::test]
async fn yandex_stream_upload_checks_size_hash_and_omits_oauth_on_upload_target() {
    let bytes = vec![9u8; 160_001];
    let request = upload(&bytes, DeliveryMode::StreamUpload);
    let expected = request.clone();
    let (endpoint,task)=server(move |req,origin| {
        if req.path.starts_with("/upload-target/") {
            assert_eq!(req.method,"PUT");
            assert!(!req.headers.contains_key("authorization"));
            assert_eq!(req.body,vec![9u8;160_001]);
            Response::empty(201)
        } else if req.path.starts_with("/v1/disk/resources/upload") {
            assert_eq!(req.method,"GET");
            assert!(req.path.contains("overwrite=false"));
            Response::json(200,serde_json::json!({"method":"PUT","templated":false,"href":format!("{origin}upload-target/ZEUS-CANARY-session")}))
        } else {
            assert_eq!(req.headers["authorization"],"OAuth ZEUS-CANARY-token");
            Response::json(200,serde_json::json!({"path":format!("disk:/Zeus/{}",expected.unique_name()),"type":"file","size":expected.size,"sha256":expected.expected_sha256}))
        }
    }).await;
    let result = YandexDisk::test_endpoint(&endpoint)
        .upload(
            &request,
            Box::new(std::io::Cursor::new(bytes)),
            None,
            &Sink::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        result.verification,
        VerificationEvidence::LocalAndRemoteSha256
    );
    task.abort();
}

#[tokio::test]
async fn yandex_import_never_reads_source_and_polls_until_metadata_verifies() {
    let request = upload(b"data", DeliveryMode::RemoteImport);
    let expected = request.clone();
    let polls = Arc::new(Mutex::new(0));
    let pollcount = Arc::clone(&polls);
    let (endpoint,task)=server(move |req,origin| {
        if req.path.starts_with("/v1/disk/resources/upload") {
            assert_eq!(req.method,"POST");
            assert!(req.path.contains("disable_redirects=true"));
            assert!(req.path.contains("overwrite=false"));
            assert!(req.path.contains("ZEUS-CANARY-signed"));
            assert!(req.body.is_empty());
            Response::json(202,serde_json::json!({"method":"GET","href":format!("{origin}v1/disk/operations/operation1")}))
        } else if req.path.starts_with("/v1/disk/operations/") {
            let mut polls=pollcount.lock().unwrap();*polls+=1;
            Response::json(200,serde_json::json!({"status":if *polls==1 {"in-progress"} else {"success"}}))
        } else {
            Response::json(200,serde_json::json!({"path":format!("disk:/Zeus/{}",expected.unique_name()),"type":"file","size":4,"sha256":expected.expected_sha256}))
        }
    }).await;
    let provider = YandexDisk::test_endpoint(&endpoint);
    let cancel = CancellationToken::new();
    let sink = Sink::default();
    let checkpoint = provider
        .import_url(
            &ImportRequest {
                upload: request,
                source_url: SecretString::new(
                    "https://source.invalid/file?key=ZEUS-CANARY-signed".into(),
                ),
            },
            &sink,
            &cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        provider.poll_import(&checkpoint, &cancel).await.unwrap(),
        ImportStatus::Pending
    ));
    let ImportStatus::Complete { artifact } =
        provider.poll_import(&checkpoint, &cancel).await.unwrap()
    else {
        panic!("complete")
    };
    assert!(artifact.local_sha256.is_none());
    assert_eq!(
        artifact.verification,
        VerificationEvidence::ExpectedAndRemoteSha256
    );
    assert!(matches!(
        provider.cancel_import(&checkpoint, &cancel).await,
        Err(CloudError::Unsupported(
            CapabilityReason::RemoteCancellationUnavailable
        ))
    ));
    assert!(!format!("{checkpoint:?} {artifact:?}").contains("ZEUS-CANARY"));
    task.abort();
}

#[tokio::test]
async fn yandex_rejects_foreign_operation_url_before_polling() {
    let (endpoint,task)=server(|_,_|Response::json(202,serde_json::json!({"method":"GET","href":"https://evil.invalid/v1/disk/operations/ZEUS-CANARY-secret"}))).await;
    let request = ImportRequest {
        upload: upload(b"data", DeliveryMode::RemoteImport),
        source_url: SecretString::new("https://source.invalid/file".into()),
    };
    let sink = Sink::default();
    let result = YandexDisk::test_endpoint(&endpoint)
        .import_url(&request, &sink, &CancellationToken::new())
        .await;
    assert!(matches!(result, Err(CloudError::UntrustedLocation)));
    assert_eq!(sink.0.lock().unwrap().len(), 1); // intent remains for ambiguous remote mutation
    task.abort();
}

#[tokio::test]
async fn mismatched_and_long_sources_are_rejected_before_final_upload() {
    for (bytes, expected_error) in [
        (b"wrong".as_slice(), "hash"),
        (b"toolong".as_slice(), "size"),
    ] {
        let (endpoint, task) = server(|req, origin| {
            if req.path.starts_with("/drive/") {
                Response::json(200, serde_json::json!({"ids":["generated_file_id"]}))
            } else {
                assert_eq!(req.method, "POST");
                Response::empty(200).header(
                    "Location",
                    format!("{origin}upload/drive/v3/files?upload_id=private"),
                )
            }
        })
        .await;
        let request = upload(b"right", DeliveryMode::StreamUpload);
        let result = GoogleDrive::test_endpoint(&endpoint)
            .upload(
                &request,
                Box::new(std::io::Cursor::new(bytes.to_vec())),
                None,
                &Sink::default(),
                &CancellationToken::new(),
            )
            .await;
        match expected_error {
            "hash" => assert!(matches!(result, Err(CloudError::HashMismatch))),
            _ => assert!(matches!(result, Err(CloudError::SizeMismatch))),
        }
        task.abort();
    }
}

#[tokio::test]
async fn cancellation_interrupts_a_stalled_source_with_private_checkpoint_intact() {
    let (endpoint, task) = server(|req, origin| {
        if req.path.starts_with("/drive/") {
            Response::json(200, serde_json::json!({"ids":["generated_file_id"]}))
        } else {
            Response::empty(200).header(
                "Location",
                format!("{origin}upload/drive/v3/files?upload_id=ZEUS-CANARY-secret"),
            )
        }
    })
    .await;
    let request = upload(b"body", DeliveryMode::StreamUpload);
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let (writer, reader) = tokio::io::duplex(4);
    let sink = Sink::default();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        trigger.cancel();
        drop(writer);
    });
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        GoogleDrive::test_endpoint(&endpoint).upload(
            &request,
            Box::new(reader),
            None,
            &sink,
            &cancel,
        ),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(CloudError::Cancelled)));
    assert!(sink.0.lock().unwrap().last().unwrap().session_url.is_some());
    task.abort();
}

#[test]
fn unknown_size_resume_and_invalid_destinations_fail_with_capability_reason() {
    let provider = GoogleDrive::new(SecretString::new("token".into()), "root".into()).unwrap();
    assert!(matches!(
        provider
            .capabilities()
            .plan(DeliveryMode::StreamUpload, None, None, false),
        Err(CloudError::Unsupported(CapabilityReason::KnownSizeRequired))
    ));
    assert!(matches!(
        provider
            .capabilities()
            .plan(DeliveryMode::StreamUpload, Some(4), None, true),
        Err(CloudError::Unsupported(CapabilityReason::ResumeUnavailable))
    ));
    let mut request = upload(b"data", DeliveryMode::StreamUpload);
    request.name = "../secret".into();
    assert!(request.validate().is_err());
}

#[tokio::test]
async fn yandex_cleanup_refuses_replaced_or_unverifiable_operation_object() {
    for remote_hash in [Some("d".repeat(64)), None] {
        let request = upload(b"data", DeliveryMode::StreamUpload);
        let expected = request.clone();
        let (endpoint, task) = server(move |req, _| {
            assert_eq!(
                req.method, "GET",
                "cleanup must not delete a replaced object"
            );
            Response::json(
                200,
                serde_json::json!({
                    "path": format!("disk:/Zeus/{}", expected.unique_name()),
                    "type": "file", "size": expected.size, "sha256": remote_hash
                }),
            )
        })
        .await;
        let local_hash = request.expected_sha256.clone();
        let mut cp = PrivateCheckpoint {
            provider: ProviderKind::YandexDisk,
            object_path: format!("disk:/Zeus/{}", request.unique_name()),
            request,
            object_id: None,
            session_url: None,
            committed_bytes: 4,
            phase: CheckpointPhase::Complete,
            provider_state: serde_json::json!({"local_sha256": local_hash}),
        };
        // A stream may not have had an expected digest, but its observed digest still binds cleanup.
        cp.request.expected_sha256 = None;
        let result = YandexDisk::test_endpoint(&endpoint)
            .cleanup(&cp, &CancellationToken::new())
            .await;
        assert!(matches!(result, Err(CloudError::ReconciliationRequired)));
        task.abort();
    }
}
