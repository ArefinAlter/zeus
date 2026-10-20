//! Real subprocess tests using an offline rclone command fixture.
#![cfg(feature = "test-fixtures")]
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;
use zeus_cloud::*;

#[derive(Default)]
struct Sink(Mutex<Vec<PrivateCheckpoint>>);
#[async_trait]
impl CheckpointSink for Sink {
    async fn save(&self, cp: &PrivateCheckpoint) -> CloudResult<()> {
        self.0.lock().unwrap().push(cp.clone());
        Ok(())
    }
}

fn configuration(dir: &std::path::Path, mode: &str) -> RcloneConfig {
    let config_file = dir.join("rclone.conf");
    std::fs::write(&config_file, mode).unwrap();
    RcloneConfig {
        executable: env!("CARGO_BIN_EXE_zeus-cloud-rclone-fixture").into(),
        config_file,
        remote: "fixture".into(),
        root_path: "bucket".into(),
        config_sha256: None,
        executable_sha256: None,
    }
}

fn request(bytes: &[u8], mode: DeliveryMode) -> UploadRequest {
    UploadRequest {
        operation_id: "01Jcloudfixture000000000001".into(),
        name: "file.bin".into(),
        size: bytes.len() as u64,
        mode,
        expected_sha256: Some(hex::encode(Sha256::digest(bytes))),
        block_md5: None,
    }
}

#[tokio::test]
async fn rcat_streams_known_size_reconciles_and_cleans_only_unique_object() {
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let provider = Rclone::connect(configuration(dir.path(), ""), &cancel)
        .await
        .unwrap();
    let sink = Sink::default();
    let bytes = vec![42u8; 200_001];
    let request = request(&bytes, DeliveryMode::StreamUpload);
    let receipt = provider
        .upload(
            &request,
            Box::new(std::io::Cursor::new(bytes.clone())),
            None,
            &sink,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        receipt.verification,
        VerificationEvidence::LocalAndRemoteSha256
    );
    assert!(receipt.object_path.contains(".zeus-"));
    let checkpoint = sink.0.lock().unwrap().last().unwrap().clone();
    assert!(
        provider
            .reconcile(&checkpoint, &cancel)
            .await
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        provider
            .upload(
                &request,
                Box::new(std::io::Cursor::new(bytes)),
                None,
                &sink,
                &cancel
            )
            .await,
        Err(CloudError::AlreadyExists)
    ));
    provider.cleanup(&checkpoint, &cancel).await.unwrap();
    assert!(
        provider
            .reconcile(&checkpoint, &cancel)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn adapter_rejects_resume_and_remote_fetch_without_spawning_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let provider = Rclone::connect(configuration(dir.path(), ""), &cancel)
        .await
        .unwrap();
    assert!(matches!(
        provider
            .capabilities()
            .plan(DeliveryMode::StreamUpload, None, None, false),
        Err(CloudError::Unsupported(CapabilityReason::KnownSizeRequired))
    ));
    assert!(matches!(
        provider.capabilities().plan(
            DeliveryMode::StagedUpload,
            Some(4),
            Some(&"a".repeat(64)),
            true
        ),
        Err(CloudError::Unsupported(CapabilityReason::ResumeUnavailable))
    ));
    let req = ImportRequest {
        upload: request(b"body", DeliveryMode::RemoteImport),
        source_url: SecretString::new("https://example.invalid/?ZEUS-CANARY-url".into()),
    };
    assert!(matches!(
        provider.import_url(&req, &Sink::default(), &cancel).await,
        Err(CloudError::Unsupported(
            CapabilityReason::RemoteImportUnavailable
        ))
    ));
}

#[tokio::test]
async fn subprocess_output_is_bounded_and_secret_errors_are_not_exposed() {
    for (mode, large) in [("flood", true), ("fail", false)] {
        let dir = tempfile::tempdir().unwrap();
        let error = Rclone::connect(configuration(dir.path(), mode), &CancellationToken::new())
            .await
            .unwrap_err();
        if large {
            assert!(matches!(error, CloudError::ResponseTooLarge));
        }
        assert!(!format!("{error:?} {error}").contains("ZEUS-CANARY"));
    }
}

#[tokio::test]
async fn cancelled_reader_kills_subprocess_and_retains_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let provider = Rclone::connect(configuration(dir.path(), ""), &cancel)
        .await
        .unwrap();
    let (writer, reader) = tokio::io::duplex(8);
    let request = request(b"body", DeliveryMode::StreamUpload);
    let sink = Sink::default();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        trigger.cancel();
        drop(writer);
    });
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        provider.upload(&request, Box::new(reader), None, &sink, &cancel),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(CloudError::Cancelled)));
    assert!(!sink.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn changed_rclone_config_is_rejected_before_every_subprocess_launch() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = configuration(dir.path(), "approved");
    config.config_sha256 = Some(hex::encode(Sha256::digest(b"approved")));
    let provider = Rclone::connect(config.clone(), &CancellationToken::new())
        .await
        .unwrap();
    std::fs::write(&config.config_file, "changed remote").unwrap();
    let result = provider
        .upload(
            &request(b"data", DeliveryMode::StreamUpload),
            Box::new(std::io::Cursor::new(b"data")),
            None,
            &Sink::default(),
            &CancellationToken::new(),
        )
        .await;
    assert!(matches!(result, Err(CloudError::CheckpointMismatch)));
}

#[tokio::test]
async fn cleanup_refuses_an_externally_replaced_operation_object() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Rclone::connect(configuration(dir.path(), ""), &CancellationToken::new())
        .await
        .unwrap();
    let request = request(b"data", DeliveryMode::StreamUpload);
    let sink = Sink::default();
    let cancel = CancellationToken::new();
    provider
        .upload(
            &request,
            Box::new(std::io::Cursor::new(b"data")),
            None,
            &sink,
            &cancel,
        )
        .await
        .unwrap();
    let cp = sink.0.lock().unwrap().last().unwrap().clone();
    std::fs::write(dir.path().join(request.unique_name()), b"evil").unwrap();
    assert!(matches!(
        provider.cleanup(&cp, &cancel).await,
        Err(CloudError::ReconciliationRequired)
    ));
    assert_eq!(
        std::fs::read(dir.path().join(request.unique_name())).unwrap(),
        b"evil"
    );
}
