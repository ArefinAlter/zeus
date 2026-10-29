use super::*;
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use zeus_api::ClientRole;
use zeus_api::auth::CredentialId;
use zeus_engine::JobTransportFactory;
use zeus_http::{HttpRequest, HttpResponse, HttpTransport, TransportError};
#[derive(Default)]
struct Secrets(std::sync::Mutex<BTreeMap<String, zeus_cloud::SecretString>>);
impl CredentialStore for Secrets {
    fn get(&self, key: &str) -> zeus_cloud::CloudResult<zeus_cloud::SecretString> {
        self.0
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or(zeus_cloud::CloudError::CredentialUnavailable)
    }
    fn put(&self, key: &str, value: &zeus_cloud::SecretString) -> zeus_cloud::CloudResult<()> {
        self.0.lock().unwrap().insert(key.into(), value.clone());
        Ok(())
    }
    fn delete(&self, key: &str) -> zeus_cloud::CloudResult<()> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}
struct Wire {
    bytes: Vec<u8>,
    requests: AtomicUsize,
    changed: AtomicBool,
}
#[async_trait::async_trait]
impl HttpTransport for Wire {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.url.host_str(), Some("huggingface.co"));
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "x-repo-commit",
            if self.changed.load(Ordering::SeqCst) {
                "b".repeat(40)
            } else {
                "a".repeat(40)
            }
            .parse()
            .unwrap(),
        );
        headers.insert(
            "etag",
            format!("\"{}\"", zeus_verify::hash_bytes(&self.bytes).sha256)
                .parse()
                .unwrap(),
        );
        headers.insert(
            "content-disposition",
            "attachment; filename=unexpected.bin".parse().unwrap(),
        );
        headers.insert("accept-ranges", "bytes".parse().unwrap());
        let (status, bytes) = if request.method == http::Method::HEAD {
            headers.insert(
                "content-length",
                self.bytes.len().to_string().parse().unwrap(),
            );
            (http::StatusCode::OK, vec![])
        } else if let Some(range) = request.headers.get("range") {
            let raw = range.to_str().unwrap().strip_prefix("bytes=").unwrap();
            let (start, end) = raw.split_once('-').unwrap();
            let start: usize = start.parse().unwrap();
            let end: usize = if end.is_empty() {
                self.bytes.len() - 1
            } else {
                end.parse().unwrap()
            };
            let end = end.min(self.bytes.len() - 1);
            headers.insert(
                "content-range",
                format!("bytes {start}-{end}/{}", self.bytes.len())
                    .parse()
                    .unwrap(),
            );
            headers.insert(
                "content-length",
                (end - start + 1).to_string().parse().unwrap(),
            );
            (
                http::StatusCode::PARTIAL_CONTENT,
                self.bytes[start..=end].to_vec(),
            )
        } else {
            headers.insert(
                "content-length",
                self.bytes.len().to_string().parse().unwrap(),
            );
            (http::StatusCode::OK, self.bytes.clone())
        };
        Ok(HttpResponse {
            status,
            version: http::Version::HTTP_11,
            headers,
            url: request.url,
            body: Box::pin(futures::stream::iter(if bytes.is_empty() {
                vec![]
            } else {
                vec![Ok(bytes::Bytes::from(bytes))]
            })),
        })
    }
}
struct Next;
impl JobTransportFactory for Next {
    fn for_job(
        &self,
        _: &zeus_types::JobSpec,
    ) -> Result<Option<Arc<dyn HttpTransport>>, zeus_types::ErrorEnvelope> {
        Ok(None)
    }
}
fn identity(kind: ClientKind) -> ClientIdentity {
    ClientIdentity {
        credential_id: CredentialId::generate(),
        kind,
        role: if kind == ClientKind::Agent {
            ClientRole::Mcp
        } else {
            ClientRole::Cli
        },
        permissions: kind.default_permissions(),
        client_name: "source-test".into(),
    }
}
struct Fixture {
    dir: tempfile::TempDir,
    service: Arc<SourceService>,
    wire: Arc<Wire>,
    human: ClientIdentity,
    agent: ClientIdentity,
    valid: Arc<AtomicBool>,
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("outputs");
        std::fs::create_dir(&root).unwrap();
        let journal = zeus_journal::Journal::open(
            &dir.path().join("journal.redb"),
            &Default::default(),
            Arc::new(zeus_control::SystemClock::new()),
        )
        .unwrap();
        let wire = Arc::new(Wire {
            bytes: vec![0x41; 512 * 1024],
            requests: AtomicUsize::new(0),
            changed: AtomicBool::new(false),
        });
        let valid = Arc::new(AtomicBool::new(true));
        let flag = valid.clone();
        let roots = vec![root.clone()];
        let scoped = roots.clone();
        let service = SourceService::open(
            dir.path(),
            journal.clone(),
            Arc::new(Secrets::default()),
            wire.clone(),
            roots,
            Arc::new(move |_| flag.load(Ordering::SeqCst)),
            Arc::new(move |_| scoped.clone()),
        )
        .unwrap();
        let engine = zeus_engine::EngineBuilder::new(zeus_engine::EngineConfig::new(
            vec![root.clone()],
            root,
            dir.path().to_owned(),
        ))
        .journal(journal)
        .job_transports(service.transport_factory(Arc::new(Next)))
        .build()
        .await
        .unwrap();
        service.attach_engine(engine).unwrap();
        let fixture = Self {
            dir,
            service,
            wire,
            human: identity(ClientKind::Human),
            agent: identity(ClientKind::Agent),
            valid,
        };
        fixture
            .service
            .configure(
                &fixture.human,
                Actor::human(zeus_types::Surface::CliInteractive),
                fixture.configure(),
            )
            .await
            .unwrap();
        fixture
    }
    fn configure(&self) -> SourcesAccountConfigureParams {
        SourcesAccountConfigureParams {
            account_id: "hub".into(),
            untrusted_label: "Fixture Hub".into(),
            config: SourceAccountConfig::HuggingFace {
                repository_type: zeus_sources::HubRepositoryType::Model,
                repository: "owner/model".into(),
            },
            allowed_credential_ids: vec![self.agent.credential_id.to_string()],
            budget_ceiling: SourceBudget {
                max_received_bytes: 4 * 1024 * 1024,
                timeout_ms: 60_000,
            },
            expected_revision: None,
            credential: Some(zeus_types::SecretString::new(
                "ZEUS-CANARY-source-token".into(),
            )),
            idempotency_key: "configure".into(),
        }
    }
    fn inspect(&self) -> SourcesInspectParams {
        SourcesInspectParams {
            account_id: "hub".into(),
            account_revision: 1,
            target: SourceTarget::HuggingFace {
                repository_type: zeus_sources::HubRepositoryType::Model,
                repository: "owner/model".into(),
                commit: "a".repeat(40),
                path: "file.bin".into(),
            },
            budget: SourceBudget {
                max_received_bytes: 2 * 1024 * 1024,
                timeout_ms: 30_000,
            },
            metainfo_base64: None,
            idempotency_key: "inspect".into(),
        }
    }
    fn submit(plan: &SourcePlanView) -> SourcesSubmitParams {
        SourcesSubmitParams {
            plan_id: plan.plan_id.clone(),
            dest_dir: None,
            filename: "file.bin".into(),
            max_connections: 2,
            torrent_filenames: Default::default(),
            idempotency_key: "submit".into(),
        }
    }
    async fn close(&self) {
        self.service.engine.get().unwrap().shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn immutable_review_held_admission_and_verified_publication() {
    let fixture = Fixture::new().await;
    let plan = fixture
        .service
        .inspect(&fixture.agent, fixture.inspect())
        .await
        .unwrap();
    assert_eq!(plan.metadata.bytes, fixture.wire.bytes.len() as u64);
    assert!(!serde_json::to_string(&plan).unwrap().contains("CANARY"));
    let operation = fixture
        .service
        .submit(&fixture.agent, Fixture::submit(&plan))
        .await
        .unwrap();
    assert!(operation.job_id.is_some());
    let final_view = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let view = fixture
                .service
                .get(
                    &fixture.agent,
                    SourcesGetParams {
                        operation_id: operation.operation_id.clone(),
                    },
                )
                .unwrap();
            if matches!(view.state.as_str(), "completed" | "failed") {
                break view;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let record = fixture
        .service
        .journal
        .job(operation.job_id.unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        final_view.state, "completed",
        "{final_view:?}; engine error={:?}; spec={:?}",
        record.job.error, record.job.spec
    );
    assert_eq!(final_view.output_sha256, plan.metadata.sha256);
    assert!(final_view.received_bytes >= plan.metadata.bytes);
    assert_eq!(
        std::fs::read(fixture.dir.path().join("outputs/file.bin")).unwrap(),
        fixture.wire.bytes
    );
    let replay = fixture
        .service
        .submit(&fixture.agent, Fixture::submit(&plan))
        .await
        .unwrap();
    assert_eq!(replay.job_id, operation.job_id);
    assert_eq!(replay.state, "completed");
    assert!(
        fixture
            .service
            .get(
                &fixture.human,
                SourcesGetParams {
                    operation_id: operation.operation_id
                }
            )
            .is_err()
    );
    fixture.close().await;
}
#[tokio::test]
async fn revisions_owner_and_destination_scope_cannot_be_bypassed() {
    let fixture = Fixture::new().await;
    let mut request = fixture.inspect();
    request.account_revision = 2;
    assert!(
        fixture
            .service
            .inspect(&fixture.agent, request)
            .await
            .is_err()
    );
    assert!(
        fixture
            .service
            .inspect(&fixture.human, fixture.inspect())
            .await
            .is_err()
    );
    let plan = fixture
        .service
        .inspect(&fixture.agent, fixture.inspect())
        .await
        .unwrap();
    let mut submit = Fixture::submit(&plan);
    submit.dest_dir = Some(fixture.dir.path().to_string_lossy().into_owned());
    assert!(
        fixture
            .service
            .submit(&fixture.agent, submit)
            .await
            .is_err()
    );
    fixture
        .service
        .revoke(
            &fixture.human,
            Actor::human(zeus_types::Surface::CliInteractive),
            SourcesAccountRevokeParams {
                account_id: "hub".into(),
                expected_revision: 1,
                idempotency_key: "revoke".into(),
            },
        )
        .await
        .unwrap();
    assert!(
        fixture
            .service
            .submit(&fixture.agent, Fixture::submit(&plan))
            .await
            .is_err()
    );
    fixture.close().await;
}
#[tokio::test]
async fn forged_private_urls_and_labels_never_create_source_authority() {
    let fixture = Fixture::new().await;
    let factory = fixture.service.transport_factory(Arc::new(Next));
    let mut spec = zeus_types::JobSpec::new(
        zeus_types::HttpUrl::parse("https://sources.zeus.invalid/srcop_forged").unwrap(),
        zeus_types::Source::new(zeus_types::SourceKind::Agent),
    );
    spec.labels.insert("source_account".into(), "hub".into());
    assert!(factory.for_job(&spec).is_err());
    assert!(
        factory
            .for_job_id(zeus_types::IdGenerator::new().job_id(), &spec)
            .is_err()
    );
    assert_eq!(fixture.wire.requests.load(Ordering::SeqCst), 0);
    fixture.valid.store(false, Ordering::SeqCst);
    assert!(
        fixture
            .service
            .inspect(&fixture.agent, fixture.inspect())
            .await
            .is_err()
    );
    fixture.close().await;
}
#[tokio::test]
async fn account_retry_payload_conflicts_and_machine_clients_cannot_configure() {
    let fixture = Fixture::new().await;
    let replay = fixture
        .service
        .configure(
            &fixture.human,
            Actor::human(zeus_types::Surface::CliInteractive),
            fixture.configure(),
        )
        .await
        .unwrap();
    assert!(replay.replayed);
    let mut request = fixture.configure();
    request.untrusted_label = "different".into();
    assert!(
        fixture
            .service
            .configure(
                &fixture.human,
                Actor::human(zeus_types::Surface::CliInteractive),
                request
            )
            .await
            .is_err()
    );
    assert!(
        fixture
            .service
            .configure(
                &fixture.agent,
                fixture.agent.action_actor(),
                fixture.configure()
            )
            .await
            .is_err()
    );
    fixture.close().await;
}

fn operation_row(fixture: &Fixture, plan: &SourcePlanView) -> Operation {
    let request = Fixture::submit(plan);
    let now = fixture.service.now();
    let job = zeus_types::IdGenerator::new().job_id();
    let operation_id = id("srcop");
    Operation {
        owner: fixture.agent.credential_id.to_string(),
        view: SourceOperationView {
            operation_id: operation_id.clone(),
            plan_id: plan.plan_id.clone(),
            account_id: "hub".into(),
            account_revision: 1,
            job_id: Some(job),
            state: "running".into(),
            untrusted_label: "test".into(),
            received_bytes: 0,
            payload_uploaded_bytes: 0,
            budget: SourceBudget {
                max_received_bytes: 500,
                timeout_ms: 30_000,
            },
            metadata: plan.metadata.clone(),
            output_sha256: None,
            files: vec![],
            error_code: None,
        },
        target: fixture.inspect().target,
        deadline: now.saturating_add_ms(30_000),
        created_at: now,
        dest_file_id: zeus_fs::DirectoryCap::open_root(&fixture.dir.path().join("outputs"))
            .unwrap()
            .file_id()
            .unwrap(),
        dest_dir: fixture
            .dir
            .path()
            .join("outputs")
            .to_string_lossy()
            .into_owned(),
        filename: "bound.bin".into(),
        metainfo: None,
        request,
        actor_kind: zeus_types::ActorKind::Agent,
        source: fixture
            .agent
            .role
            .job_source(fixture.agent.credential_id.as_str()),
        publications: vec![],
    }
}

#[tokio::test]
async fn cumulative_budget_reservations_are_atomic_and_revocation_stops_commits() {
    let fixture = Fixture::new().await;
    let plan = fixture
        .service
        .inspect(&fixture.agent, fixture.inspect())
        .await
        .unwrap();
    let row = operation_row(&fixture, &plan);
    let operation_id = row.view.operation_id.clone();
    let job = row.view.job_id.unwrap();
    fixture
        .service
        .store
        .save(&format!("operation:{operation_id}"), &row)
        .unwrap();
    let authority = transport::authority(&fixture.service, &operation_id);
    let attempts = (0..20)
        .map(|_| {
            let authority = authority.clone();
            tokio::task::spawn_blocking(move || authority.charge(100))
        })
        .collect::<Vec<_>>();
    let results = futures::future::join_all(attempts).await;
    assert_eq!(
        results
            .into_iter()
            .filter(|r| r.as_ref().is_ok_and(|r| r.is_ok()))
            .count(),
        5
    );
    assert_eq!(
        fixture
            .service
            .store
            .get::<Operation>(&format!("operation:{operation_id}"))
            .unwrap()
            .unwrap()
            .view
            .received_bytes,
        500
    );
    let mut spec = zeus_types::JobSpec::new(
        zeus_types::HttpUrl::parse(transport::virtual_url(&operation_id).unwrap().as_str())
            .unwrap(),
        zeus_types::Source::new(zeus_types::SourceKind::Agent),
    );
    spec.source = row.source.clone();
    spec.dest_dir = Some(row.dest_dir.clone());
    spec.filename = Some(row.filename.clone());
    spec.max_connections = Some(row.request.max_connections);
    spec.overwrite = Some(zeus_types::Overwrite::Fail);
    spec.expected_size = Some(plan.metadata.bytes);
    spec.expected_sha256 = plan.metadata.sha256.as_ref().map(|s| s.parse().unwrap());
    let factory = fixture.service.transport_factory(Arc::new(Next));
    assert!(factory.for_job_id(job, &spec).is_ok());
    let mut redirected = spec.clone();
    redirected.dest_dir = Some(fixture.dir.path().to_string_lossy().into_owned());
    assert!(factory.for_job_id(job, &redirected).is_err());
    let mut renamed = spec.clone();
    renamed.filename = Some("unreviewed.bin".into());
    assert!(factory.for_job_id(job, &renamed).is_err());
    assert!(
        factory
            .for_job_id(zeus_types::IdGenerator::new().job_id(), &spec)
            .is_err()
    );
    fixture
        .service
        .revoke(
            &fixture.human,
            Actor::human(zeus_types::Surface::CliInteractive),
            SourcesAccountRevokeParams {
                account_id: "hub".into(),
                expected_revision: 1,
                idempotency_key: "revoke-budget".into(),
            },
        )
        .await
        .unwrap();
    assert!(authority.remaining().is_err());
    assert!(factory.for_job_id(job, &spec).is_err());
    fixture.close().await;
}

#[tokio::test]
async fn restart_keeps_budget_pauses_authority_and_preserves_control_receipts() {
    let fixture = Fixture::new().await;
    let plan = fixture
        .service
        .inspect(&fixture.agent, fixture.inspect())
        .await
        .unwrap();
    let mut row = operation_row(&fixture, &plan);
    row.view.job_id = None;
    row.view.received_bytes = 321;
    let key = format!("operation:{}", row.view.operation_id);
    fixture.service.store.save(&key, &row).unwrap();
    fixture.close().await;
    let Fixture {
        dir,
        service,
        wire,
        human: _,
        agent,
        valid,
    } = fixture;
    let journal = service.journal.clone();
    let credentials = service.credentials.clone();
    drop(service);
    let root = dir.path().join("outputs");
    let roots = vec![root.clone()];
    let reopened = SourceService::open(
        dir.path(),
        journal,
        credentials,
        wire.clone(),
        roots.clone(),
        Arc::new(move |_| valid.load(Ordering::SeqCst)),
        Arc::new(move |_| roots.clone()),
    )
    .unwrap();
    let recovered = reopened.store.get::<Operation>(&key).unwrap().unwrap();
    assert_eq!(recovered.view.state, "paused");
    assert_eq!(recovered.view.received_bytes, 321);
    assert_eq!(recovered.deadline, row.deadline);
    assert_eq!(
        recovered.view.error_code.as_deref(),
        Some("sources.restart_review_required")
    );
    assert!(
        transport::authority(&reopened, &row.view.operation_id)
            .remaining()
            .is_err()
    );
    let stopped = reopened
        .control(
            &agent,
            SourcesControlParams {
                operation_id: row.view.operation_id.clone(),
                action: SourceControlAction::Cancel,
                idempotency_key: "cancel-after-restart".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(stopped.state, "canceled");
    assert_eq!(stopped.received_bytes, 321);
    assert_eq!(
        wire.requests.load(Ordering::SeqCst),
        1,
        "restart/cancel must not start network work"
    );
    assert!(
        reopened
            .control(
                &agent,
                SourcesControlParams {
                    operation_id: row.view.operation_id,
                    action: SourceControlAction::Resume,
                    idempotency_key: "cannot-resume-canceled".into()
                }
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn replacing_a_reviewed_destination_revokes_operation_authority() {
    let fixture = Fixture::new().await;
    let plan = fixture
        .service
        .inspect(&fixture.agent, fixture.inspect())
        .await
        .unwrap();
    let mut row = operation_row(&fixture, &plan);
    let original = fixture.dir.path().join("outputs/selected");
    std::fs::create_dir(&original).unwrap();
    row.dest_dir = original.to_string_lossy().into_owned();
    row.dest_file_id = zeus_fs::DirectoryCap::open_root(&original)
        .unwrap()
        .file_id()
        .unwrap();
    fixture
        .service
        .store
        .save(&format!("operation:{}", row.view.operation_id), &row)
        .unwrap();
    let authority = transport::authority(&fixture.service, &row.view.operation_id);
    assert!(authority.remaining().is_ok());
    std::fs::rename(&original, fixture.dir.path().join("outputs/moved")).unwrap();
    std::fs::create_dir(&original).unwrap();
    assert!(authority.remaining().is_err());
    assert!(authority.charge(1).is_err());
    fixture.close().await;
}

#[tokio::test]
async fn torrent_publication_verifies_receipts_recovers_rename_and_never_overwrites() {
    let fixture = Fixture::new().await;
    let plan = fixture
        .service
        .inspect(&fixture.agent, fixture.inspect())
        .await
        .unwrap();
    let mut row = operation_row(&fixture, &plan);
    row.view.job_id = None;
    row.filename = "verified-torrent.bin".into();
    let key = format!("operation:{}", row.view.operation_id);
    fixture.service.store.save(&key, &row).unwrap();
    let staging = fixture.dir.path().join("torrent-fixture");
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(staging.join("data-0000.bin"), &fixture.wire.bytes).unwrap();
    let receipt = zeus_sources::torrent::TorrentReceipt {
        payload_uploaded_bytes: 0,
        files: vec![zeus_sources::torrent::TorrentFileReceipt {
            index: 0,
            bytes: fixture.wire.bytes.len() as u64,
            sha256: zeus_verify::hash_bytes(&fixture.wire.bytes)
                .sha256
                .to_string(),
        }],
    };
    fixture
        .service
        .publish_torrent(
            &row,
            &staging,
            &receipt,
            transport::authority(&fixture.service, &row.view.operation_id),
        )
        .unwrap();
    let published = fixture
        .service
        .store
        .get::<Operation>(&key)
        .unwrap()
        .unwrap();
    assert_eq!(published.view.state, "completed");
    assert_eq!(published.view.files.len(), 1);
    assert_eq!(published.view.files[0].sha256, receipt.files[0].sha256);
    let output = fixture.dir.path().join("outputs/verified-torrent.bin");
    assert_eq!(std::fs::read(&output).unwrap(), fixture.wire.bytes);
    // Emulate a crash after rename but before the published bit became durable.
    fixture
        .service
        .store
        .mutate::<Operation, _>(&key, |op| {
            op.view.state = "running".into();
            op.publications[0].published = false;
            Ok(())
        })
        .unwrap();
    fixture
        .service
        .publish_torrent(
            &row,
            &staging,
            &receipt,
            transport::authority(&fixture.service, &row.view.operation_id),
        )
        .unwrap();
    assert_eq!(
        fixture
            .service
            .store
            .get::<Operation>(&key)
            .unwrap()
            .unwrap()
            .view
            .state,
        "completed"
    );
    let mut collision = operation_row(&fixture, &plan);
    collision.filename = row.filename.clone();
    collision.view.job_id = None;
    fixture
        .service
        .store
        .save(
            &format!("operation:{}", collision.view.operation_id),
            &collision,
        )
        .unwrap();
    assert!(
        fixture
            .service
            .publish_torrent(
                &collision,
                &staging,
                &receipt,
                transport::authority(&fixture.service, &collision.view.operation_id)
            )
            .is_err()
    );
    assert_eq!(std::fs::read(output).unwrap(), fixture.wire.bytes);
    fixture.close().await;
}
