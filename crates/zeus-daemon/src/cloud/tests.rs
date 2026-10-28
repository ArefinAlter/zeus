//! Hermetic cloud authorization, receipt, interruption and recovery regressions.
use super::*;
use std::sync::atomic::AtomicUsize;
use tokio::io::AsyncReadExt;
use zeus_api::ClientRole;
use zeus_api::auth::{AuthKey, Credential, CredentialId, MemoryCredentialStore};
use zeus_api::methods::sessions::{ApprovalsDecideParams, ConfirmationLevel, DecideOutcome};
use zeus_cloud::{
    CheckpointPhase, CheckpointSink, CloudError, CloudResult, ImportStatus, PrivateCheckpoint,
    RemoteArtifact, UploadRequest, UploadSource, VerificationEvidence,
};
use zeus_types::{HttpUrl, IdGenerator, Job, JobResult, JobSpec, Source, SourceKind, Surface};
#[derive(Default)]
struct Secrets(std::sync::Mutex<BTreeMap<String, zeus_cloud::SecretString>>);
impl CredentialStore for Secrets {
    fn get(&self, key: &str) -> CloudResult<zeus_cloud::SecretString> {
        self.0
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or(CloudError::CredentialUnavailable)
    }
    fn put(&self, key: &str, value: &zeus_cloud::SecretString) -> CloudResult<()> {
        self.0.lock().unwrap().insert(key.into(), value.clone());
        Ok(())
    }
    fn delete(&self, key: &str) -> CloudResult<()> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}
struct Provider {
    uploads: AtomicUsize,
    polls: AtomicUsize,
    blocked: AtomicBool,
    entered: tokio::sync::Notify,
    artifact: std::sync::Mutex<Option<RemoteArtifact>>,
}
impl Default for Provider {
    fn default() -> Self {
        Self {
            uploads: AtomicUsize::new(0),
            polls: AtomicUsize::new(0),
            blocked: AtomicBool::new(false),
            entered: tokio::sync::Notify::new(),
            artifact: Default::default(),
        }
    }
}
fn checkpoint(request: &UploadRequest, phase: CheckpointPhase) -> PrivateCheckpoint {
    PrivateCheckpoint {
        provider: ProviderKind::GoogleDrive,
        request: request.clone(),
        object_path: request.unique_name(),
        object_id: Some("private-id".into()),
        session_url: Some("https://provider.invalid/upload?secret=CHECKPOINT-CANARY".into()),
        committed_bytes: 0,
        phase,
        provider_state: serde_json::Value::Null,
    }
}
fn artifact(request: &UploadRequest) -> RemoteArtifact {
    RemoteArtifact {
        provider: ProviderKind::GoogleDrive,
        operation_id: request.operation_id.clone(),
        object_id: "remote-object".into(),
        object_path: request.unique_name(),
        size: request.size,
        local_sha256: request.expected_sha256.clone(),
        remote_sha256: request.expected_sha256.clone(),
        verification: VerificationEvidence::LocalAndRemoteSha256,
    }
}
#[async_trait]
impl CloudProvider for Provider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider: ProviderKind::GoogleDrive,
            staged_upload: true,
            stream_upload: true,
            remote_import: true,
            resumable_upload: true,
            remote_cancel: false,
            remote_sha256: true,
            remote_import_hard_byte_limit: false,
        }
    }
    async fn upload(
        &self,
        request: &UploadRequest,
        mut source: UploadSource,
        _checkpoint: Option<PrivateCheckpoint>,
        sink: &dyn CheckpointSink,
        _cancel: &CancellationToken,
    ) -> CloudResult<RemoteArtifact> {
        sink.save(&checkpoint(request, CheckpointPhase::Uploading))
            .await?;
        self.uploads.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        if self.blocked.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        let mut bytes = Vec::new();
        source
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| CloudError::Source)?;
        let hash = zeus_verify::hash::hash_bytes(&bytes);
        if hash.size_bytes != request.size
            || Some(hash.sha256.to_string()) != request.expected_sha256
        {
            return Err(CloudError::HashMismatch);
        }
        let result = artifact(request);
        *self.artifact.lock().unwrap() = Some(result.clone());
        let mut cp = checkpoint(request, CheckpointPhase::Complete);
        cp.committed_bytes = request.size;
        sink.save(&cp).await?;
        Ok(result)
    }
    async fn poll_import(
        &self,
        cp: &PrivateCheckpoint,
        _cancel: &CancellationToken,
    ) -> CloudResult<ImportStatus> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        Ok(match self.artifact.lock().unwrap().clone() {
            Some(a) if a.operation_id == cp.request.operation_id => {
                ImportStatus::Complete { artifact: a }
            }
            _ => ImportStatus::Pending,
        })
    }
    async fn reconcile(
        &self,
        cp: &PrivateCheckpoint,
        _cancel: &CancellationToken,
    ) -> CloudResult<Option<RemoteArtifact>> {
        Ok(self
            .artifact
            .lock()
            .unwrap()
            .clone()
            .filter(|a| a.operation_id == cp.request.operation_id))
    }
    async fn cleanup(
        &self,
        _cp: &PrivateCheckpoint,
        _cancel: &CancellationToken,
    ) -> CloudResult<()> {
        self.artifact.lock().unwrap().take();
        Ok(())
    }
}
struct Factory(Arc<Provider>);
#[async_trait]
impl ProviderFactory for Factory {
    async fn connect(
        &self,
        _: &ProviderConfig,
        _: Option<zeus_cloud::SecretString>,
        _: &CancellationToken,
    ) -> CloudResult<Arc<dyn CloudProvider>> {
        Ok(self.0.clone())
    }
}
struct Fixture {
    dir: tempfile::TempDir,
    service: Arc<CloudService>,
    provider: Arc<Provider>,
    agent: ClientIdentity,
    human: ClientIdentity,
    credentials: Arc<MemoryCredentialStore>,
}
fn identity(kind: ClientKind, credentials: &MemoryCredentialStore) -> ClientIdentity {
    let id = CredentialId::generate();
    let role = if kind == ClientKind::Agent {
        ClientRole::Mcp
    } else {
        ClientRole::Cli
    };
    credentials.insert(
        Credential::new(
            id.clone(),
            kind,
            role,
            AuthKey::from_bytes([4; 32]),
            Timestamp::now(),
        )
        .unwrap(),
    );
    ClientIdentity {
        credential_id: id,
        kind,
        role,
        permissions: kind.default_permissions(),
        client_name: "test".into(),
    }
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(
            &dir.path().join("journal.redb"),
            &Default::default(),
            Arc::new(zeus_control::SystemClock::new()),
        )
        .unwrap();
        let audit = Arc::new(zeus_verify::MemoryAuditLog::new(
            IdGenerator::new().chain_id(),
            IdGenerator::new().store_id(),
            "test",
            Timestamp::now(),
            None,
        ));
        let credentials = Arc::new(MemoryCredentialStore::default());
        let agent = identity(ClientKind::Agent, &credentials);
        let human = identity(ClientKind::Human, &credentials);
        let consent = Arc::new(crate::consent::ConsentManager::new(
            journal.clone(),
            audit.clone(),
            credentials.clone(),
        ));
        let provider = Arc::new(Provider::default());
        let service = CloudService::open_with_factory(
            dir.path(),
            journal,
            consent,
            audit,
            Arc::new(Secrets::default()),
            Arc::new(Factory(provider.clone())),
        )
        .unwrap();
        service
            .configure(
                &human,
                Actor::human(Surface::CliInteractive),
                CloudAccountsConfigureParams {
                    account_id: "drive".into(),
                    label: "Team drive".into(),
                    config: ProviderConfig::GoogleDrive {
                        parent_id: "root".into(),
                    },
                    max_bytes_per_delivery: 1000000,
                    expected_revision: None,
                    credential: Some(zeus_types::SecretString::from("CREDENTIAL-CANARY")),
                    idempotency_key: "configure".into(),
                },
            )
            .await
            .unwrap();
        Self {
            dir,
            service,
            provider,
            agent,
            human,
            credentials,
        }
    }
    fn source_job(&self, bytes: &[u8]) -> zeus_types::JobId {
        let id = IdGenerator::new().job_id();
        let path = self.dir.path().join(format!("{id}.bin"));
        std::fs::write(&path, bytes).unwrap();
        let hashes = zeus_verify::hash::hash_bytes(bytes);
        let now = Timestamp::now();
        let mut source = Source::new(SourceKind::Agent);
        source.client_id = Some(self.agent.credential_id.to_string());
        let spec = JobSpec::new(
            HttpUrl::parse("https://example.org/source").unwrap(),
            source,
        );
        let mut job = Job::new(id, spec, JobState::Completed, now);
        job.result = Some(JobResult {
            path: path.to_string_lossy().into_owned(),
            size_bytes: bytes.len() as u64,
            sha256: hashes.sha256,
            blake3: hashes.blake3,
            detected_mime: None,
            completed_at: now,
        });
        let internal=serde_json::from_value(serde_json::json!({"epoch":1,"dest_path":path,"zpart_path":path.with_extension("zpart"),"zpart_file_id":null,"total_bytes":bytes.len(),"data_complete":true,"bound_validators":{},"range_support":"verified"})).unwrap();
        self.service
            .journal
            .transact(|tx| tx.put_job(&zeus_journal::JobRecord::new(job, internal)))
            .unwrap();
        id
    }
    async fn submit(
        &self,
        source: CloudSource,
        mode: DeliveryMode,
        key: &str,
    ) -> CloudDeliveryView {
        let plan = self
            .service
            .plan(
                &self.agent,
                CloudPlanParams {
                    account_id: "drive".into(),
                    mode,
                    source,
                    remote_name: "saved.bin".into(),
                    max_bytes: 1000000,
                    max_seconds: 3600,
                    allow_unbounded_remote: mode == DeliveryMode::RemoteImport,
                    idempotency_key: format!("plan-{key}"),
                },
            )
            .await
            .unwrap()
            .plan;
        self.service
            .submit(
                &self.agent,
                CloudSubmitParams {
                    plan_id: plan.plan_id,
                    plan_hash: plan.plan_hash,
                    idempotency_key: format!("submit-{key}"),
                },
            )
            .await
            .unwrap()
            .delivery
    }
    async fn approve(&self, view: &CloudDeliveryView) {
        let a = self
            .service
            .consent
            .get(&self.human, view.approval_id.unwrap())
            .unwrap();
        let p = ApprovalsDecideParams {
            approval_id: a.id,
            request_hash: a.request_hash.unwrap(),
            outcome: DecideOutcome::Approve,
            scope: None,
            channel: Surface::CliInteractive,
            confirmation: ConfirmationLevel::TypedCode,
            idempotency_key: None,
        };
        self.service.consent.decide(&self.human,&p,serde_json::json!({"client_id":self.human.credential_id.as_str(),"surface":"cli_interactive","exe_verified":true})).await.unwrap();
    }
    fn get(&self, id: &str) -> CloudDeliveryView {
        self.service
            .get(
                &self.agent,
                CloudGetParams {
                    delivery_id: id.into(),
                },
            )
            .unwrap()
    }
    async fn settle(&self) {
        for _ in 0..1000 {
            if self
                .service
                .workers
                .lock()
                .unwrap()
                .values()
                .all(|w| w.task.is_finished())
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        panic!("worker did not settle");
    }
    fn age(&self, id: &str) {
        let mut row: Delivery = self
            .service
            .store
            .get(&format!("delivery:{id}"))
            .unwrap()
            .unwrap();
        row.view.updated_at = Timestamp::UNIX_EPOCH;
        self.service
            .store
            .save(&format!("delivery:{id}"), &row)
            .unwrap();
    }
}
#[tokio::test]
async fn a_failing_retention_never_stops_deliveries() {
    let f = Fixture::new().await;
    f.service.fail_retention.store(true, Ordering::Release);
    let id = f.source_job(b"retention fails");
    let delivery = f
        .submit(
            CloudSource::CompletedJob { job_id: id },
            DeliveryMode::StagedUpload,
            "retention",
        )
        .await;
    f.service.tick().await.unwrap();
    f.approve(&delivery).await;
    f.service.tick().await.unwrap();
    f.settle().await;
    assert_eq!(
        f.get(&delivery.delivery_id).state,
        CloudDeliveryState::Completed
    );
}
#[tokio::test]
async fn approval_is_required_upload_verified_once_and_original_file_preserved() {
    let f = Fixture::new().await;
    let id = f.source_job(b"verified original");
    let delivery = f
        .submit(
            CloudSource::CompletedJob { job_id: id },
            DeliveryMode::StagedUpload,
            "one",
        )
        .await;
    f.service.tick().await.unwrap();
    assert_eq!(f.provider.uploads.load(Ordering::SeqCst), 0);
    assert_eq!(
        f.get(&delivery.delivery_id).state,
        CloudDeliveryState::AwaitingApproval
    );
    f.approve(&delivery).await;
    f.service.tick().await.unwrap();
    f.settle().await;
    let result = f.get(&delivery.delivery_id);
    assert_eq!(result.state, CloudDeliveryState::Completed);
    assert!(result.artifact.is_some());
    let replay = f
        .service
        .submit(
            &f.agent,
            CloudSubmitParams {
                plan_id: delivery.plan.plan_id.clone(),
                plan_hash: delivery.plan.plan_hash,
                idempotency_key: "submit-one".into(),
            },
        )
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.delivery.delivery_id, delivery.delivery_id);
    f.service.tick().await.unwrap();
    assert_eq!(f.provider.uploads.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read(
            f.service
                .journal
                .job(id)
                .unwrap()
                .unwrap()
                .job
                .result
                .unwrap()
                .path
        )
        .unwrap(),
        b"verified original"
    );
    let public = serde_json::to_string(&result).unwrap();
    assert!(!public.contains("CANARY"));
    assert!(!public.contains("session_url"));
    f.service.shutdown().await;
}
#[tokio::test]
async fn source_queries_and_checkpoints_are_private_and_other_agents_cannot_approve_or_read() {
    let f = Fixture::new().await;
    let delivery = f
        .submit(
            CloudSource::PublicUrl {
                url: HttpUrl::parse("https://example.org/private/path?token=URL-CANARY").unwrap(),
                size_bytes: 8,
                sha256: None,
                block_md5: None,
            },
            DeliveryMode::RemoteImport,
            "private",
        )
        .await;
    let other = identity(ClientKind::Agent, &f.credentials);
    assert!(
        f.service
            .get(
                &other,
                CloudGetParams {
                    delivery_id: delivery.delivery_id.clone()
                }
            )
            .is_err()
    );
    assert!(
        f.service
            .list(&other, Default::default())
            .unwrap()
            .deliveries
            .is_empty()
    );
    let a = f
        .service
        .consent
        .get(&f.human, delivery.approval_id.unwrap())
        .unwrap();
    let public = serde_json::to_string(&a).unwrap();
    assert!(!public.contains("URL-CANARY"));
    assert!(!public.contains("private/path"));
    let p = ApprovalsDecideParams {
        approval_id: a.id,
        request_hash: a.request_hash.unwrap(),
        outcome: DecideOutcome::Approve,
        scope: None,
        channel: Surface::CliInteractive,
        confirmation: ConfirmationLevel::TypedCode,
        idempotency_key: None,
    };
    assert!(f.service.consent.decide(&f.agent,&p,serde_json::json!({"client_id":f.agent.credential_id.as_str(),"surface":"cli_interactive","exe_verified":true})).await.is_err());
    let mut browser = p;
    browser.channel = Surface::ExtensionPopup;
    assert!(f.service.consent.decide(&f.human,&browser,serde_json::json!({"client_id":f.human.credential_id.as_str(),"surface":"extension_popup","exe_verified":true})).await.is_err());
    let configure = CloudAccountsConfigureParams {
        account_id: "evil".into(),
        label: "evil".into(),
        config: ProviderConfig::GoogleDrive {
            parent_id: "root".into(),
        },
        max_bytes_per_delivery: 10,
        expected_revision: None,
        credential: Some("secret".into()),
        idempotency_key: "x".into(),
    };
    assert!(
        f.service
            .configure(&f.agent, Actor::human(Surface::CliInteractive), configure)
            .await
            .is_err()
    );
    f.service.shutdown().await;
}
#[tokio::test]
async fn killed_or_revoked_scope_stops_inflight_bytes_and_cannot_resume() {
    let f = Fixture::new().await;
    let id = f.source_job(b"bounded bytes");
    let delivery = f
        .submit(
            CloudSource::CompletedJob { job_id: id },
            DeliveryMode::StagedUpload,
            "kill",
        )
        .await;
    f.approve(&delivery).await;
    f.provider.blocked.store(true, Ordering::SeqCst);
    f.service.tick().await.unwrap();
    f.provider.entered.notified().await;
    f.service
        .journal
        .transact(|tx| tx.set_kill_switch(true))
        .unwrap();
    f.service.pause_all_for_kill_switch().await.unwrap();
    assert!(
        f.service
            .configure(
                &f.human,
                Actor::human(Surface::CliInteractive),
                CloudAccountsConfigureParams {
                    account_id: "drive".into(),
                    label: "Changed during stop".into(),
                    config: ProviderConfig::GoogleDrive {
                        parent_id: "root".into()
                    },
                    max_bytes_per_delivery: 1_000_000,
                    expected_revision: Some(1),
                    credential: Some("replacement-canary".into()),
                    idempotency_key: "configure-while-stopped".into(),
                }
            )
            .await
            .is_err()
    );
    assert_eq!(f.service.account("drive").unwrap().view.revision, 1);
    f.settle().await;
    assert_eq!(
        f.get(&delivery.delivery_id).state,
        CloudDeliveryState::Paused
    );
    assert!(f.service.guard(&delivery.delivery_id).is_err());
    f.service
        .journal
        .transact(|tx| tx.set_kill_switch(false))
        .unwrap();
    let revoked_credential_key = f.service.account("drive").unwrap().credential_key.unwrap();
    let revoked = f
        .service
        .revoke(
            &f.human,
            Actor::human(Surface::CliInteractive),
            CloudAccountsRevokeParams {
                account_id: "drive".into(),
                expected_revision: 1,
                idempotency_key: "revoke".into(),
            },
        )
        .await
        .unwrap();
    assert!(revoked.account.revoked);
    assert!(
        f.service
            .control(
                &f.agent,
                CloudControlParams {
                    delivery_id: delivery.delivery_id.clone(),
                    action: CloudControlAction::Resume,
                    idempotency_key: "resume".into()
                }
            )
            .await
            .is_err()
    );
    assert!(f.service.credentials.get(&revoked_credential_key).is_err());
    assert!(f.get(&delivery.delivery_id).artifact.is_none());
    f.service.shutdown().await;
}
#[tokio::test]
async fn idempotency_binds_exact_plan_and_control_receipts_survive_reopen() {
    let f = Fixture::new().await;
    let id = f.source_job(b"input");
    let delivery = f
        .submit(
            CloudSource::CompletedJob { job_id: id },
            DeliveryMode::StagedUpload,
            "idem",
        )
        .await;
    let mut params = CloudSubmitParams {
        plan_id: delivery.plan.plan_id.clone(),
        plan_hash: delivery.plan.plan_hash,
        idempotency_key: "submit-idem".into(),
    };
    params.plan_hash = Hex256::from_bytes([7; 32]);
    assert!(f.service.submit(&f.agent, params).await.is_err());
    let control = CloudControlParams {
        delivery_id: delivery.delivery_id.clone(),
        action: CloudControlAction::Cancel,
        idempotency_key: "cancel".into(),
    };
    let first = f.service.control(&f.agent, control.clone()).await.unwrap();
    assert_eq!(first.delivery.state, CloudDeliveryState::Canceled);
    let replay = f.service.control(&f.agent, control).await.unwrap();
    assert!(replay.replayed);
    let journal = f.service.journal.clone();
    let consent = f.service.consent.clone();
    let audit = f.service.audit.clone();
    let secrets = f.service.credentials.clone();
    let factory = f.service.factory.clone();
    f.service.shutdown().await;
    drop(f.service);
    let reopened =
        CloudService::open_with_factory(f.dir.path(), journal, consent, audit, secrets, factory)
            .unwrap();
    let again = reopened
        .control(
            &f.agent,
            CloudControlParams {
                delivery_id: delivery.delivery_id,
                action: CloudControlAction::Cancel,
                idempotency_key: "cancel".into(),
            },
        )
        .await
        .unwrap();
    assert!(again.replayed);
    assert_eq!(again.delivery.state, CloudDeliveryState::Canceled);
    reopened.shutdown().await;
}
#[tokio::test]
async fn interrupted_completed_remote_object_is_reconciled_without_duplicate_upload() {
    let f = Fixture::new().await;
    let id = f.source_job(b"reconciled");
    let delivery = f
        .submit(
            CloudSource::CompletedJob { job_id: id },
            DeliveryMode::StagedUpload,
            "recover",
        )
        .await;
    f.approve(&delivery).await;
    f.service
        .store
        .mutate_delivery(&delivery.delivery_id, |r| {
            r.view.state = CloudDeliveryState::Running;
            r.checkpoint = Some(checkpoint(&r.upload, CheckpointPhase::Uploading));
            *f.provider.artifact.lock().unwrap() = Some(artifact(&r.upload));
            Ok(())
        })
        .unwrap();
    let journal = f.service.journal.clone();
    let consent = f.service.consent.clone();
    let audit = f.service.audit.clone();
    let secrets = f.service.credentials.clone();
    let factory = f.service.factory.clone();
    f.service.shutdown().await;
    drop(f.service);
    let reopened =
        CloudService::open_with_factory(f.dir.path(), journal, consent, audit, secrets, factory)
            .unwrap();
    let mut row: Delivery = reopened
        .store
        .get(&format!("delivery:{}", delivery.delivery_id))
        .unwrap()
        .unwrap();
    assert_eq!(row.view.state, CloudDeliveryState::Reconciling);
    row.view.updated_at = Timestamp::UNIX_EPOCH;
    reopened
        .store
        .save(&format!("delivery:{}", delivery.delivery_id), &row)
        .unwrap();
    reopened.tick().await.unwrap();
    for _ in 0..1000 {
        if reopened
            .workers
            .lock()
            .unwrap()
            .values()
            .all(|w| w.task.is_finished())
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    assert_eq!(
        reopened
            .get(
                &f.agent,
                CloudGetParams {
                    delivery_id: delivery.delivery_id
                }
            )
            .unwrap()
            .state,
        CloudDeliveryState::Completed
    );
    assert_eq!(f.provider.uploads.load(Ordering::SeqCst), 0);
    reopened.shutdown().await;
}
#[tokio::test]
async fn accepted_remote_import_is_not_complete_and_uncancelable_receipt_is_retained() {
    let f = Fixture::new().await;
    let delivery = f
        .submit(
            CloudSource::PublicUrl {
                url: HttpUrl::parse("https://example.org/file?token=URL-CANARY").unwrap(),
                size_bytes: 8,
                sha256: None,
                block_md5: None,
            },
            DeliveryMode::RemoteImport,
            "remote",
        )
        .await;
    f.approve(&delivery).await;
    f.service
        .store
        .mutate_delivery(&delivery.delivery_id, |r| {
            r.view.state = CloudDeliveryState::RemotePending;
            r.checkpoint = Some(checkpoint(&r.upload, CheckpointPhase::ImportPending));
            Ok(())
        })
        .unwrap();
    f.age(&delivery.delivery_id);
    f.service.tick().await.unwrap();
    f.settle().await;
    assert_eq!(
        f.get(&delivery.delivery_id).state,
        CloudDeliveryState::RemotePending
    );
    assert!(f.get(&delivery.delivery_id).artifact.is_none());
    let canceled = f
        .service
        .control(
            &f.agent,
            CloudControlParams {
                delivery_id: delivery.delivery_id.clone(),
                action: CloudControlAction::Cancel,
                idempotency_key: "cancel-remote".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        canceled.delivery.state,
        CloudDeliveryState::RemoteCancellationUnavailable
    );
    let row: Delivery = f
        .service
        .store
        .get(&format!("delivery:{}", delivery.delivery_id))
        .unwrap()
        .unwrap();
    assert!(row.checkpoint.is_some());
    f.age(&delivery.delivery_id);
    f.service.tick().await.unwrap();
    f.settle().await;
    assert_eq!(
        f.get(&delivery.delivery_id).state,
        CloudDeliveryState::RemoteCancellationUnavailable
    );
    assert!(f.provider.polls.load(Ordering::SeqCst) >= 2);
    f.service.shutdown().await;
}
#[test]
fn identifiers_are_canonical_and_private_source_networks_are_denied() {
    let valid = id("cld");
    assert!(validate_id(&valid, "cld").is_ok());
    assert!(validate_id(&valid.to_ascii_lowercase(), "cld").is_err());
    assert!(validate_id("cld_80000000000000000000000000", "cld").is_err());
    assert!(
        source::validate_url(&HttpUrl::parse("http://localhost/file").unwrap(), false).is_err()
    );
    assert!(
        source::validate_url(
            &HttpUrl::parse("https://user:secret@example.org/file").unwrap(),
            false
        )
        .is_err()
    );
    assert!(
        source::validate_url(&HttpUrl::parse("http://example.org/file").unwrap(), true).is_err()
    );
}

#[tokio::test]
async fn forward_only_stream_reader_stops_after_kill_without_staging_any_bytes() {
    let f = Fixture::new().await;
    let delivery = f
        .submit(
            CloudSource::PublicUrl {
                url: HttpUrl::parse("https://example.org/stream").unwrap(),
                size_bytes: 12,
                sha256: None,
                block_md5: None,
            },
            DeliveryMode::StreamUpload,
            "stream",
        )
        .await;
    f.approve(&delivery).await;
    f.service
        .store
        .mutate_delivery(&delivery.delivery_id, |row| {
            row.view.state = CloudDeliveryState::Running;
            Ok(())
        })
        .unwrap();
    let row: Delivery = f
        .service
        .store
        .get(&format!("delivery:{}", delivery.delivery_id))
        .unwrap()
        .unwrap();
    let cancel = CancellationToken::new();
    let mut reader = source::guarded(
        &f.service,
        &row,
        &cancel,
        Box::new(std::io::Cursor::new(b"stream bytes".to_vec())),
    );
    let mut part = [0; 6];
    reader.read_exact(&mut part).await.unwrap();
    assert_eq!(&part, b"stream");
    f.service
        .journal
        .transact(|tx| tx.set_kill_switch(true))
        .unwrap();
    assert!(reader.read(&mut part).await.is_err());
    assert_eq!(std::fs::read_dir(&f.service.staging).unwrap().count(), 0);
    f.service.shutdown().await;
}

#[tokio::test]
async fn verified_staging_survives_restart_and_never_refetches_an_expired_source_url() {
    let f = Fixture::new().await;
    let bytes = b"verified staged payload";
    let digest = zeus_verify::hash::hash_bytes(bytes).sha256;
    let delivery = f
        .submit(
            CloudSource::PublicUrl {
                url: HttpUrl::parse("https://expired.invalid/file?signature=EXPIRED-CANARY")
                    .unwrap(),
                size_bytes: bytes.len() as u64,
                sha256: Some(digest),
                block_md5: None,
            },
            DeliveryMode::StagedUpload,
            "spool",
        )
        .await;
    f.approve(&delivery).await;
    let path = f
        .service
        .staging
        .join(format!("{}.part", delivery.delivery_id));
    std::fs::write(&path, bytes).unwrap();
    f.service
        .store
        .mutate_delivery(&delivery.delivery_id, |row| {
            row.view.state = CloudDeliveryState::Running;
            row.staged_sha256 = Some(digest);
            row.checkpoint = Some(checkpoint(&row.upload, CheckpointPhase::Uploading));
            Ok(())
        })
        .unwrap();
    let row: Delivery = f
        .service
        .store
        .get(&format!("delivery:{}", delivery.delivery_id))
        .unwrap()
        .unwrap();
    let (mut reader, request) = source::upload_source(&f.service, &row, &CancellationToken::new())
        .await
        .unwrap();
    let mut actual = Vec::new();
    reader.read_to_end(&mut actual).await.unwrap();
    assert_eq!(actual, bytes);
    assert_eq!(request.expected_sha256, Some(digest.to_string()));
    drop(reader);
    std::fs::write(&path, vec![0; bytes.len()]).unwrap();
    assert!(
        source::upload_source(&f.service, &row, &CancellationToken::new())
            .await
            .is_err()
    );
    assert!(path.exists());
    f.service.shutdown().await;
}

#[tokio::test]
async fn uncertain_remote_dispatch_is_never_automatically_repeated() {
    let f = Fixture::new().await;
    let delivery = f
        .submit(
            CloudSource::PublicUrl {
                url: HttpUrl::parse("https://example.org/file").unwrap(),
                size_bytes: 8,
                sha256: None,
                block_md5: None,
            },
            DeliveryMode::RemoteImport,
            "uncertain",
        )
        .await;
    f.approve(&delivery).await;
    f.service
        .store
        .mutate_delivery(&delivery.delivery_id, |row| {
            row.view.state = CloudDeliveryState::Reconciling;
            row.checkpoint = Some(checkpoint(&row.upload, CheckpointPhase::Prepared));
            Ok(())
        })
        .unwrap();
    f.age(&delivery.delivery_id);
    f.service.tick().await.unwrap();
    f.settle().await;
    let view = f.get(&delivery.delivery_id);
    assert_eq!(view.state, CloudDeliveryState::Reconciling);
    assert_eq!(view.reason.as_deref(), Some("cloud.dispatch_uncertain"));
    assert_eq!(f.provider.uploads.load(Ordering::SeqCst), 0);
    assert_eq!(f.provider.polls.load(Ordering::SeqCst), 0);
    assert!(view.artifact.is_none());
    f.service.shutdown().await;
}

#[tokio::test]
async fn kill_switch_blocks_even_metadata_polling_of_accepted_remote_imports() {
    let f = Fixture::new().await;
    let delivery = f
        .submit(
            CloudSource::PublicUrl {
                url: HttpUrl::parse("https://example.org/source").unwrap(),
                size_bytes: 1,
                sha256: None,
                block_md5: None,
            },
            DeliveryMode::RemoteImport,
            "kill-poll",
        )
        .await;
    f.approve(&delivery).await;
    f.service
        .store
        .mutate_delivery(&delivery.delivery_id, |row| {
            row.view.state = CloudDeliveryState::RemotePending;
            row.checkpoint = Some(checkpoint(&row.upload, CheckpointPhase::ImportPending));
            Ok(())
        })
        .unwrap();
    f.service
        .journal
        .transact(|tx| tx.set_kill_switch(true))
        .unwrap();
    f.service.pause_all_for_kill_switch().await.unwrap();
    f.age(&delivery.delivery_id);
    f.service.tick().await.unwrap();
    f.settle().await;
    assert_eq!(f.provider.polls.load(Ordering::SeqCst), 0);
    assert_eq!(
        f.get(&delivery.delivery_id).state,
        CloudDeliveryState::RemoteCancellationUnavailable
    );
    f.service
        .journal
        .transact(|tx| tx.set_kill_switch(false))
        .unwrap();
    f.service.tick().await.unwrap();
    f.settle().await;
    assert_eq!(f.provider.polls.load(Ordering::SeqCst), 1);
    assert_eq!(
        f.get(&delivery.delivery_id).state,
        CloudDeliveryState::RemoteCancellationUnavailable
    );
    f.service.shutdown().await;
}

#[test]
fn oversized_private_record_rolls_back_entire_atomic_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("cloud.redb")).unwrap();
    store
        .save("receipt:existing", &serde_json::json!({"saved":true}))
        .unwrap();
    let result = store.save_many(&[
        ("receipt:new".into(), b"{}".to_vec()),
        ("plan:oversized".into(), vec![b'x'; 2 * 1024 * 1024 + 1]),
    ]);
    assert!(result.is_err());
    assert!(
        store
            .get::<serde_json::Value>("receipt:new")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .get::<serde_json::Value>("receipt:existing")
            .unwrap()
            .unwrap()["saved"],
        true
    );
    drop(store);
    assert!(
        Store::open(&dir.path().join("cloud.redb"))
            .unwrap()
            .get::<serde_json::Value>("receipt:new")
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn staging_cleanup_failure_is_visible_and_retried_without_losing_remote_receipt() {
    let f = Fixture::new().await;
    let delivery = f
        .submit(
            CloudSource::PublicUrl {
                url: HttpUrl::parse("https://example.org/source").unwrap(),
                size_bytes: 1,
                sha256: None,
                block_md5: None,
            },
            DeliveryMode::StagedUpload,
            "cleanup",
        )
        .await;
    let path = f
        .service
        .staging
        .join(format!("{}.part", delivery.delivery_id));
    std::fs::create_dir(&path).unwrap();
    f.service
        .store
        .mutate_delivery(&delivery.delivery_id, |row| {
            row.view.state = CloudDeliveryState::Completed;
            row.view.cleanup_pending = true;
            row.view.artifact = Some(artifact(&row.upload));
            Ok(())
        })
        .unwrap();
    f.service.tick().await.unwrap();
    let view = f.get(&delivery.delivery_id);
    assert!(view.cleanup_pending);
    assert_eq!(view.reason.as_deref(), Some("cloud.local_cleanup_failed"));
    assert!(view.artifact.is_some());
    assert_eq!(view.state, CloudDeliveryState::Completed);
    std::fs::remove_dir(path).unwrap();
    f.service.tick().await.unwrap();
    let view = f.get(&delivery.delivery_id);
    assert!(!view.cleanup_pending);
    assert!(view.artifact.is_some());
    f.service.shutdown().await;
}

#[tokio::test]
async fn retention_ages_out_finished_history_but_never_live_evidence_or_a_replay() {
    let f = Fixture::new().await;
    let finished = f
        .submit(
            CloudSource::CompletedJob {
                job_id: f.source_job(b"finished upload"),
            },
            DeliveryMode::StagedUpload,
            "done",
        )
        .await;
    f.approve(&finished).await;
    f.service.tick().await.unwrap();
    f.settle().await;
    assert_eq!(
        f.get(&finished.delivery_id).state,
        CloudDeliveryState::Completed
    );
    let waiting = f
        .submit(
            CloudSource::CompletedJob {
                job_id: f.source_job(b"still waiting"),
            },
            DeliveryMode::StagedUpload,
            "waiting",
        )
        .await;
    let orphan = f
        .service
        .plan(
            &f.agent,
            CloudPlanParams {
                account_id: "drive".into(),
                mode: DeliveryMode::StagedUpload,
                source: CloudSource::CompletedJob {
                    job_id: f.source_job(b"never submitted"),
                },
                remote_name: "never.bin".into(),
                max_bytes: 1000000,
                max_seconds: 3600,
                allow_unbounded_remote: false,
                idempotency_key: "plan-orphan".into(),
            },
        )
        .await
        .unwrap()
        .plan;
    // A receipt from before receipts carried a time is stamped first, so it ages from now.
    f.service
        .store
        .save(
            "receipt:legacy",
            &Receipt {
                hash: "h".into(),
                result: serde_json::json!({}),
                created_at: None,
            },
        )
        .unwrap();
    let later = Timestamp::now().saturating_add_ms(31 * 86_400_000);
    let first = f
        .service
        .store
        .prune(
            later,
            TERMINAL_RETENTION_MS,
            RECEIPT_RETENTION_MS,
            &f.service.staging,
        )
        .unwrap();
    assert_eq!(first.deliveries, 1, "{first:?}");
    assert_eq!(first.plans, 2, "the finished and the never-submitted plan");
    assert_eq!(first.stamped, 1);
    let store = &f.service.store;
    assert!(
        store
            .get::<Delivery>(&format!("delivery:{}", finished.delivery_id))
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get::<Plan>(&format!("plan:{}", orphan.plan_id))
            .unwrap()
            .is_none()
    );
    // The waiting delivery, its plan and its receipts are live evidence and stay.
    assert!(
        store
            .get::<Delivery>(&format!("delivery:{}", waiting.delivery_id))
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get::<Plan>(&format!("plan:{}", waiting.plan.plan_id))
            .unwrap()
            .is_some()
    );
    let receipts = store.keys("receipt:").unwrap();
    assert!(receipts.contains(&"receipt:legacy".to_owned()));
    assert_eq!(
        receipts.len(),
        3,
        "the waiting plan and submit receipts, and the stamped legacy one"
    );
    // A replay of the pruned submission cannot run it again: its plan is gone.
    assert!(
        f.service
            .submit(
                &f.agent,
                CloudSubmitParams {
                    plan_id: finished.plan.plan_id.clone(),
                    plan_hash: finished.plan.plan_hash,
                    idempotency_key: "submit-done".into(),
                },
            )
            .await
            .is_err()
    );
    f.service.tick().await.unwrap();
    assert_eq!(f.provider.uploads.load(Ordering::SeqCst), 1);
    let second = f
        .service
        .store
        .prune(
            later.saturating_add_ms(8 * 86_400_000),
            TERMINAL_RETENTION_MS,
            RECEIPT_RETENTION_MS,
            &f.service.staging,
        )
        .unwrap();
    assert_eq!(
        second.receipts, 1,
        "the stamped legacy receipt ages out a week later"
    );
    f.service.shutdown().await;
}

#[tokio::test]
async fn local_file_upload_requires_approval_preserves_original_and_cleans_its_snapshot() {
    let f = Fixture::new().await;
    let folder = f.dir.path().join("local");
    std::fs::create_dir(&folder).unwrap();
    let path = folder.join("original.txt");
    std::fs::write(&path, b"local original").unwrap();
    std::fs::write(
        &f.service.local_roots_file,
        serde_json::to_vec(&vec![folder.to_string_lossy()]).unwrap(),
    )
    .unwrap();
    let delivery = f
        .submit(
            CloudSource::LocalFile {
                path: LocalFilePath::new(path.to_string_lossy().into_owned()),
            },
            DeliveryMode::StagedUpload,
            "local",
        )
        .await;
    f.service.tick().await.unwrap();
    assert_eq!(f.provider.uploads.load(Ordering::SeqCst), 0);
    assert!(
        !serde_json::to_string(&delivery)
            .unwrap()
            .contains(f.dir.path().to_str().unwrap())
    );
    f.approve(&delivery).await;
    f.service.tick().await.unwrap();
    f.settle().await;
    assert_eq!(
        f.get(&delivery.delivery_id).state,
        CloudDeliveryState::Completed
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"local original");
    assert!(
        !f.service
            .staging
            .join(format!("{}.part", delivery.delivery_id))
            .exists()
    );
}

#[tokio::test]
async fn local_file_changes_after_planning_never_reach_the_remote_provider() {
    let f = Fixture::new().await;
    let path = f.dir.path().join("change.txt");
    std::fs::write(&path, b"original").unwrap();
    std::fs::write(
        &f.service.local_roots_file,
        serde_json::to_vec(&vec![f.dir.path().to_string_lossy()]).unwrap(),
    )
    .unwrap();
    let delivery = f
        .submit(
            CloudSource::LocalFile {
                path: LocalFilePath::new(path.to_string_lossy().into_owned()),
            },
            DeliveryMode::StagedUpload,
            "changed",
        )
        .await;
    f.service.tick().await.unwrap();
    f.approve(&delivery).await;
    std::fs::write(&path, b"modified").unwrap();
    f.service.tick().await.unwrap();
    f.settle().await;
    assert_eq!(f.provider.uploads.load(Ordering::SeqCst), 0);
    assert_ne!(
        f.get(&delivery.delivery_id).state,
        CloudDeliveryState::Completed
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"modified");
}

#[tokio::test]
async fn local_scope_revocation_and_outside_paths_fail_closed() {
    let f = Fixture::new().await;
    let scope = f.dir.path().join("scope");
    std::fs::create_dir(&scope).unwrap();
    let outside = f.dir.path().join("outside");
    std::fs::write(&outside, b"private").unwrap();
    std::fs::write(
        &f.service.local_roots_file,
        serde_json::to_vec(&vec![scope.to_string_lossy()]).unwrap(),
    )
    .unwrap();
    assert!(source::local_path(&f.service, outside.to_str().unwrap()).is_err());
    let inside = scope.join("inside");
    std::fs::write(&inside, b"inside").unwrap();
    assert!(source::local_path(&f.service, inside.to_str().unwrap()).is_ok());
    std::fs::remove_file(&f.service.local_roots_file).unwrap();
    assert!(source::local_path(&f.service, inside.to_str().unwrap()).is_err());
}

#[tokio::test]
async fn local_file_cannot_bypass_job_quarantine_even_after_approval() {
    let f = Fixture::new().await;
    let id = f.source_job(b"quarantine regression");
    let mut record = f.service.journal.job(id).unwrap().unwrap();
    let path = record.job.result.as_ref().unwrap().path.clone();
    std::fs::write(
        &f.service.local_roots_file,
        serde_json::to_vec(&vec![f.dir.path().to_string_lossy()]).unwrap(),
    )
    .unwrap();
    let delivery = f
        .submit(
            CloudSource::LocalFile {
                path: LocalFilePath::new(&path),
            },
            DeliveryMode::StagedUpload,
            "quarantine-local",
        )
        .await;
    f.approve(&delivery).await;
    record.job.quarantined = true;
    f.service
        .journal
        .transact(|tx| tx.put_job(&record))
        .unwrap();
    assert_eq!(
        source::local_path(&f.service, &path)
            .unwrap_err()
            .0
            .code
            .as_str(),
        "policy.quarantine_blocked"
    );
    f.service.tick().await.unwrap();
    f.settle().await;
    assert_eq!(f.provider.uploads.load(Ordering::SeqCst), 0);
    assert_ne!(
        f.get(&delivery.delivery_id).state,
        CloudDeliveryState::Completed
    );
    assert_eq!(std::fs::read(path).unwrap(), b"quarantine regression");
}

/// Explicit test port maps an approved public source onto a loopback fixture. Production always
/// constructs PublicNetwork and retains source::response_from DNS/peer/redirect checks.
struct StagingWire {
    endpoint: String,
}
#[async_trait]
impl staging::SourceNetwork for StagingWire {
    async fn response(
        &self,
        url: &HttpUrl,
        _size: u64,
        resume: Option<(String, u64)>,
        cancel: &CancellationToken,
    ) -> ApiResult<(HttpUrl, reqwest::Response)> {
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut request = client.get(&self.endpoint);
        if let Some((etag, offset)) = resume {
            request = request
                .header("Range", format!("bytes={offset}-"))
                .header("If-Range", etag);
        }
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err(invalid("fixture canceled")),
            response = request.send() => response.map_err(|_| fail())?,
        };
        Ok((url.clone(), response))
    }
}
async fn staged_wire_fixture(bytes: &[u8]) -> (Fixture, Delivery, HttpUrl) {
    let f = Fixture::new().await;
    f.service
        .configure(
            &f.human,
            Actor::human(Surface::CliInteractive),
            CloudAccountsConfigureParams {
                account_id: "drive".into(),
                label: "Fixture".into(),
                config: ProviderConfig::GoogleDrive {
                    parent_id: "root".into(),
                },
                max_bytes_per_delivery: 32 << 20,
                expected_revision: Some(1),
                credential: None,
                idempotency_key: "larger-fixture".into(),
            },
        )
        .await
        .unwrap();
    let url = HttpUrl::parse("https://example.org/stage?signature=SOURCE-STAGING-CANARY").unwrap();
    let plan = f
        .service
        .plan(
            &f.agent,
            CloudPlanParams {
                account_id: "drive".into(),
                mode: DeliveryMode::StagedUpload,
                source: CloudSource::PublicUrl {
                    url: url.clone(),
                    size_bytes: bytes.len() as u64,
                    sha256: Some(zeus_verify::hash_bytes(bytes).sha256),
                    block_md5: None,
                },
                remote_name: "fixture.bin".into(),
                max_bytes: 32 << 20,
                max_seconds: 3600,
                allow_unbounded_remote: false,
                idempotency_key: "range-plan".into(),
            },
        )
        .await
        .unwrap()
        .plan;
    let view = f
        .service
        .submit(
            &f.agent,
            CloudSubmitParams {
                plan_id: plan.plan_id,
                plan_hash: plan.plan_hash,
                idempotency_key: "range-submit".into(),
            },
        )
        .await
        .unwrap()
        .delivery;
    f.approve(&view).await;
    f.service
        .store
        .mutate_delivery(&view.delivery_id, |row| {
            row.view.state = CloudDeliveryState::Running;
            Ok(())
        })
        .unwrap();
    let row = f
        .service
        .store
        .get(&format!("delivery:{}", view.delivery_id))
        .unwrap()
        .unwrap();
    (f, row, url)
}
async fn staging_server(
    bytes: Arc<Vec<u8>>,
    ignore_range: bool,
) -> (
    StagingWire,
    Arc<std::sync::Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/fixture", listener.local_addr().unwrap());
    let requests = Arc::new(std::sync::Mutex::new(vec![]));
    let observed = requests.clone();
    let task = tokio::spawn(async move {
        let mut ordinal = 0;
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let mut head = vec![];
            while !head.ends_with(b"\r\n\r\n") && head.len() < 8192 {
                let mut one = [0];
                if stream.read(&mut one).await.unwrap_or(0) == 0 {
                    break;
                }
                head.push(one[0]);
            }
            let head = String::from_utf8(head).unwrap().to_ascii_lowercase();
            observed.lock().unwrap().push(head.clone());
            let requested = head
                .lines()
                .find_map(|s| s.strip_prefix("range: bytes="))
                .map(|s| s.split('-').next().unwrap().parse::<usize>().unwrap());
            let start = if ignore_range {
                0
            } else {
                requested.unwrap_or(0)
            };
            let range = if requested.is_some() && !ignore_range {
                format!(
                    "Content-Range: bytes {}-{}/{}\r\n",
                    start,
                    bytes.len() - 1,
                    bytes.len()
                )
            } else {
                String::new()
            };
            let status = if requested.is_some() && !ignore_range {
                "206 Partial Content"
            } else {
                "200 OK"
            };
            let headers = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nETag: \"stage-v1\"\r\n{range}Connection: close\r\n\r\n",
                bytes.len() - start
            );
            stream.write_all(headers.as_bytes()).await.unwrap();
            if ordinal == 0 {
                let _ = stream.write_all(&bytes[..5 << 20]).await;
            } else {
                let _ = stream.write_all(&bytes[start..]).await;
            }
            ordinal += 1;
        }
    });
    (StagingWire { endpoint }, requests, task)
}
#[tokio::test]
async fn partial_url_staging_recovers_over_http_and_ignored_ranges_restart_without_append() {
    for ignore_range in [false, true] {
        let bytes = Arc::new(
            (0..(9 << 20) + 123)
                .map(|i| (i % 251) as u8)
                .collect::<Vec<_>>(),
        );
        let (f, row, url) = staged_wire_fixture(&bytes).await;
        let path = f
            .service
            .staging
            .join(format!("{}.part", row.view.delivery_id));
        let (network, requests, server) = staging_server(bytes.clone(), ignore_range).await;
        assert!(
            staging::download_using(
                &f.service,
                &row,
                &url,
                &path,
                &CancellationToken::new(),
                &network
            )
            .await
            .is_err()
        );
        let resumed: Delivery = f
            .service
            .store
            .get(&format!("delivery:{}", row.view.delivery_id))
            .unwrap()
            .unwrap();
        assert_eq!(resumed.view.source_bytes_done, 4 << 20);
        assert!(!resumed.view.source_verified);
        assert!(resumed.source_checkpoint.is_some());
        assert!(
            !serde_json::to_string(&resumed.view)
                .unwrap()
                .contains("SOURCE-STAGING-CANARY")
        );
        let mut file = staging::download_using(
            &f.service,
            &resumed,
            &url,
            &path,
            &CancellationToken::new(),
            &network,
        )
        .await
        .unwrap();
        let mut actual = vec![];
        file.read_to_end(&mut actual).await.unwrap();
        assert_eq!(
            zeus_verify::hash_bytes(&actual).sha256,
            zeus_verify::hash_bytes(&bytes).sha256
        );
        assert_eq!(actual, *bytes);
        let seen = requests.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        assert!(
            seen[1].contains("range: bytes=4194304-\r\n"),
            "{:?}",
            seen[1]
        );
        assert!(seen[1].contains("if-range: \"stage-v1\""));
        server.abort();
        f.service.shutdown().await;
    }
}

#[path = "oauth_tests.rs"]
mod oauth_tests;

#[tokio::test]
async fn omitted_credential_cannot_cross_native_provider_boundaries() {
    let f = Fixture::new().await;
    let params = CloudAccountsConfigureParams {
        account_id: "drive".into(),
        label: "Provider change".into(),
        config: ProviderConfig::YandexDisk {
            root_path: "disk:/Zeus".into(),
        },
        max_bytes_per_delivery: 1_000_000,
        expected_revision: Some(1),
        credential: None,
        idempotency_key: "different-provider".into(),
    };
    let error = f
        .service
        .configure(
            &f.human,
            Actor::human(Surface::CliInteractive),
            params.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.envelope().code.as_str(), "api.invalid_params");
    let unchanged = f.service.account("drive").unwrap();
    assert_eq!(unchanged.view.revision, 1);
    assert_eq!(unchanged.view.provider, ProviderKind::GoogleDrive);
    let mut replacement = params;
    replacement.credential = Some("EXPLICIT-NEW-PROVIDER-CREDENTIAL".into());
    replacement.idempotency_key = "explicit-different-provider".into();
    let changed = f
        .service
        .configure(&f.human, Actor::human(Surface::CliInteractive), replacement)
        .await
        .unwrap();
    assert_eq!(changed.account.revision, 2);
    assert_eq!(changed.account.provider, ProviderKind::YandexDisk);
    let mut same_provider = CloudAccountsConfigureParams {
        account_id: "drive".into(),
        label: "Same provider edit".into(),
        config: ProviderConfig::YandexDisk {
            root_path: "disk:/Zeus".into(),
        },
        max_bytes_per_delivery: 1_000_000,
        expected_revision: Some(2),
        credential: None,
        idempotency_key: "same-provider".into(),
    };
    let same = f
        .service
        .configure(
            &f.human,
            Actor::human(Surface::CliInteractive),
            same_provider.clone(),
        )
        .await
        .unwrap();
    assert_eq!(same.account.revision, 3);
    same_provider.config = ProviderConfig::GoogleDrive {
        parent_id: "root".into(),
    };
    same_provider.expected_revision = Some(3);
    same_provider.idempotency_key = "provider-switch-back".into();
    assert!(
        f.service
            .configure(
                &f.human,
                Actor::human(Surface::CliInteractive),
                same_provider
            )
            .await
            .is_err()
    );
}
