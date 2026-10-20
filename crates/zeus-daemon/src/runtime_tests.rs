use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeus_api::server::Notifier;
use zeus_api::{ClientIdentity, ClientKind, ClientRole, auth::CredentialStore};
use zeus_types::{Actor, HttpUrl};

fn contract(def: &str, value: &impl serde::Serialize) {
    static SCHEMAS: std::sync::OnceLock<zeus_types::spec_testing::SpecSchemas> =
        std::sync::OnceLock::new();
    SCHEMAS
        .get_or_init(zeus_types::spec_testing::SpecSchemas::load)
        .check(
            true,
            "daemon-api.v1",
            def,
            &serde_json::to_value(value).unwrap(),
        )
        .unwrap();
}
fn paths(root: &Path) -> zeus_api::endpoint::ZeusPaths {
    zeus_api::endpoint::ZeusPaths {
        state_dir: root.join("state"),
        config_dir: root.join("config"),
        runtime_dir: root.join("runtime"),
        user_identity: "test".into(),
        profile: None,
    }
}
async fn setup(root: &Path) -> Arc<RuntimeBackend> {
    let paths = paths(root);
    crate::provision::provision(&paths, Timestamp::now()).unwrap();
    RuntimeBackend::open(
        paths.state_dir,
        DaemonConfig {
            write_roots: vec![root.join("downloads")],
            ..Default::default()
        },
    )
    .await
    .unwrap()
}
fn context(
    root: &Path,
    name: &str,
) -> (
    CallContext,
    tokio::sync::mpsc::Receiver<zeus_api::jsonrpc::Message>,
) {
    let paths = paths(root);
    let store =
        crate::provision::FileCredentialStore::load(&paths.state_dir.join("credentials.json"))
            .unwrap();
    let credential = store
        .get(
            &crate::provision::load_client_credential(&paths, name)
                .unwrap()
                .id,
        )
        .unwrap();
    let (notifier, receiver) = Notifier::channel(256);
    (
        CallContext {
            reply: Default::default(),
            peer: Default::default(),
            attach: None,
            identity: ClientIdentity {
                credential_id: credential.id,
                kind: credential.kind,
                role: credential.role,
                permissions: credential.permissions,
                client_name: "untrusted test name".into(),
            },
            connection_id: zeus_types::IdGenerator::new().connection_id(),
            notifier,
            limits: zeus_api::methods::daemon::ConnectionLimits::STANDARD,
        },
        receiver,
    )
}
async fn http(data: Vec<u8>) -> (HttpUrl, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = HttpUrl::parse(&format!(
        "http://{}/fixture.bin",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let data = Arc::new(data);
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let data = data.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0; 1024];
                while !request.ends_with(b"\r\n\r\n") && request.len() < 16384 {
                    let Ok(n) = stream.read(&mut buf).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..n]);
                }
                let request = String::from_utf8_lossy(&request);
                let range = request.lines().find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("range: bytes=")
                        .map(str::to_owned)
                });
                let (start, end) = range
                    .as_deref()
                    .and_then(|s| s.split_once('-'))
                    .map(|(a, b)| {
                        (
                            a.trim().parse::<usize>().unwrap(),
                            b.trim().parse::<usize>().unwrap_or(data.len() - 1),
                        )
                    })
                    .unwrap_or((0, data.len() - 1));
                let end = end.min(data.len() - 1);
                if start > end {
                    return;
                }
                let header = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\nETag: \"fixture-1\"\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                    data.len(),
                    end - start + 1
                );
                if stream.write_all(header.as_bytes()).await.is_ok() {
                    let _ = stream.write_all(&data[start..=end]).await;
                }
            });
        }
    });
    (url, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_api_download_survives_restart_with_receipts_and_provenance() {
    use zeus_api::{
        client::{Client, ClientConfig, IpcClient, call},
        methods::*,
        server::{ServerContext, serve_connection},
    };
    let dir = tempfile::tempdir().unwrap();
    let backend = setup(dir.path()).await;
    let paths = paths(dir.path());
    let data = (0..300_000).map(|n| (n % 251) as u8).collect::<Vec<_>>();
    let (url, http) = http(data.clone()).await;
    let secret = Arc::new(crate::provision::load_ipc_secret(&paths).unwrap());
    let server = Arc::new(ServerContext {
        secret: secret.clone(),
        credentials: Arc::new(
            crate::provision::FileCredentialStore::load(&paths.state_dir.join("credentials.json"))
                .unwrap(),
        ),
        daemon: backend.info(),
        router: Arc::new(crate::build_router(backend.clone())),
    });
    let (a, b) = tokio::io::duplex(256 * 1024);
    let serving = tokio::spawn(serve_connection(b, server));
    let client = IpcClient::from_stream(
        a,
        crate::provision::load_client_credential(&paths, "agent").unwrap(),
        &secret,
        ClientConfig::new(ClientInfo {
            name: "synthetic agent".into(),
            version: "0".into(),
            role: ClientRole::Mcp,
        }),
    )
    .await
    .unwrap();
    contract("DaemonAuthenticateResult", client.session());
    assert_eq!(client.session().kind, ClientKind::Agent);
    let request: EnqueueParams = serde_json::from_value(serde_json::json!({"jobs":[{"url":url,"filename":"verified.bin","expected_sha256":zeus_verify::hash_bytes(&data).sha256}],"idempotency_key":"batch-one"})).unwrap();
    let first = call::<JobsEnqueue, _>(&client, request.clone())
        .await
        .unwrap();
    contract("JobsEnqueueResult", &first);
    let id = first.jobs[0].job_id.unwrap();
    let repeated = call::<JobsEnqueue, _>(&client, request.clone())
        .await
        .unwrap();
    assert_eq!(first.jobs, repeated.jobs);
    assert!(repeated.replayed);
    let mut changed = request.clone();
    changed.jobs[0].filename = Some("different.bin".into());
    assert_eq!(
        call::<JobsEnqueue, _>(&client, changed)
            .await
            .unwrap_err()
            .envelope()
            .code
            .as_str(),
        "api.idempotency_conflict"
    );
    let completed = call::<JobsWait, _>(
        &client,
        JobsWaitParams {
            job_ids: vec![id],
            until: WaitUntil::AllTerminal,
            timeout_ms: 10000,
            since_seq: None,
            progress_bytes: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        completed.jobs[0].state,
        JobState::Completed,
        "{:?}",
        completed.jobs[0].last_error
    );
    contract("JobsWaitResult", &completed);
    assert!(completed.jobs[0].state_seq.is_some());
    assert_eq!(
        std::fs::read(dir.path().join("downloads/verified.bin")).unwrap(),
        data
    );
    let quiet = call::<JobsWait, _>(
        &client,
        JobsWaitParams {
            job_ids: vec![id],
            until: WaitUntil::SettledOrBlocked,
            timeout_ms: 20,
            since_seq: Some(completed.head_seq),
            progress_bytes: None,
        },
    )
    .await
    .unwrap();
    assert!(quiet.timed_out);
    backend.flush_outbox().await.unwrap();
    let audit = call::<AuditVerify, _>(&client, AuditVerifyParams::default())
        .await
        .unwrap();
    assert!(audit.ok);
    let manifest =
        std::fs::read_to_string(dir.path().join("downloads/.zeus/manifest.jsonl")).unwrap();
    assert_eq!(manifest.lines().count(), 2);
    assert!(manifest.contains(&id.to_string()));
    let meta = backend.journal.meta().unwrap();
    assert_eq!(meta.artifact_cursor, meta.last_event_seq);
    drop(client);
    serving.abort();
    let _ = serving.await;
    backend.engine.shutdown().await.unwrap();
    drop(backend);
    let second = setup(dir.path()).await;
    let (ctx, _rx) = context(dir.path(), "agent");
    let replay = second.jobs_enqueue(&ctx, request).await.unwrap();
    assert_eq!(replay.jobs, first.jobs);
    assert!(replay.replayed);
    second.flush_outbox().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("downloads/.zeus/manifest.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    second.engine.shutdown().await.unwrap();
    http.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_clients_are_confined_to_their_roots_jobs_and_records() {
    let dir = tempfile::tempdir().unwrap();
    let backend = setup(dir.path()).await;
    let (admin, _rx) = context(dir.path(), "cli");
    let actor = Actor::human(Surface::CliInteractive);
    let add = |name: &str, kind| ClientsAddParams {
        name: name.into(),
        kind,
        role: None,
        permissions: None,
        idempotency_key: None,
    };
    // Unverified terminals, agents and bad requests cannot create credentials.
    assert_eq!(
        backend
            .clients_add(&admin, add("ogun", ClientKind::Service))
            .await
            .unwrap_err()
            .envelope()
            .code
            .as_str(),
        "api.human_required"
    );
    for (params, code) in [
        (add("agent", ClientKind::Agent), "api.invalid_params"),
        (add("Ogun", ClientKind::Service), "api.invalid_params"),
        (add("person", ClientKind::Human), "api.invalid_params"),
        (
            ClientsAddParams {
                role: Some(ClientRole::Cli),
                ..add("disguised", ClientKind::Agent)
            },
            "api.invalid_params",
        ),
        (
            ClientsAddParams {
                permissions: Some([zeus_api::Permission::KillSwitch].into()),
                ..add("widened", ClientKind::Agent)
            },
            "api.invalid_params",
        ),
    ] {
        let error = backend
            .add_client_as(actor.clone(), &admin, params)
            .await
            .unwrap_err();
        assert_eq!(error.envelope().code.as_str(), code);
    }
    let created = backend
        .add_client_as(
            actor.clone(),
            &admin,
            ClientsAddParams {
                idempotency_key: Some("link-ogun".into()),
                ..add("ogun", ClientKind::Service)
            },
        )
        .await
        .unwrap();
    contract("ClientsAddResult", &created);
    assert_eq!(created.client.role, ClientRole::Service);
    assert!(Path::new(&created.credential_file).is_file());
    assert!(
        !serde_json::to_string(&created).unwrap().contains("zt1_"),
        "the token never leaves its file"
    );
    let replay = backend
        .add_client_as(
            actor.clone(),
            &admin,
            ClientsAddParams {
                idempotency_key: Some("link-ogun".into()),
                ..add("ogun", ClientKind::Service)
            },
        )
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.client, created.client);
    assert_eq!(
        backend
            .add_client_as(actor.clone(), &admin, add("ogun", ClientKind::Agent))
            .await
            .unwrap_err()
            .envelope()
            .code
            .as_str(),
        "api.conflict"
    );
    let listed = backend
        .clients_list(&admin, Default::default())
        .await
        .unwrap();
    contract("ClientsListResult", &listed);
    assert!(listed.clients.iter().any(|c| c.name == "ogun"));
    assert!(listed.clients.iter().any(|c| c.name == "cli"));

    // The service registers a root inside the user's write root, never the root itself or outside it.
    let (service, _rx2) = context(dir.path(), "ogun");
    assert_eq!(service.identity.kind, ClientKind::Service);
    let root_path = dir.path().join("downloads").join("ogun");
    let register = |path: &Path| FilesRootsRegisterParams {
        path: path.display().to_string(),
        label: Some("ogun datasets".into()),
        idempotency_key: None,
    };
    let root = backend
        .files_roots_register(&service, register(&root_path))
        .await
        .unwrap();
    contract("FilesRootsRegisterResult", &root);
    assert!(root.created && root_path.is_dir());
    assert!(
        !backend
            .files_roots_register(&service, register(&root_path))
            .await
            .unwrap()
            .created
    );
    for outside in [dir.path().join("downloads"), dir.path().join("elsewhere")] {
        assert_eq!(
            backend
                .files_roots_register(&service, register(&outside))
                .await
                .unwrap_err()
                .envelope()
                .code
                .as_str(),
            "policy.path_outside_roots"
        );
    }

    // Its jobs land in its root; a destination elsewhere is refused before any job exists.
    let data = (0..50_000).map(|n| (n % 241) as u8).collect::<Vec<_>>();
    let (url, http) = http(data.clone()).await;
    let request = |dest: Option<&Path>| -> EnqueueParams {
        let mut job = serde_json::json!({"url":url,"filename":"set.bin"});
        if let Some(dest) = dest {
            job["dest_dir"] = serde_json::json!(dest.display().to_string());
        }
        serde_json::from_value(serde_json::json!({ "jobs": [job] })).unwrap()
    };
    assert_eq!(
        backend
            .jobs_enqueue(&service, request(Some(&dir.path().join("downloads"))))
            .await
            .unwrap_err()
            .envelope()
            .code
            .as_str(),
        "policy.path_outside_roots"
    );
    let own = backend.jobs_enqueue(&service, request(None)).await.unwrap();
    let own_id = own.jobs[0].job_id.unwrap();
    let (agent, _rx3) = context(dir.path(), "agent");
    let theirs = backend.jobs_enqueue(&agent, request(None)).await.unwrap();
    let their_id = theirs.jobs[0].job_id.unwrap();
    let done = backend
        .jobs_wait(
            &agent,
            JobsWaitParams {
                job_ids: vec![own_id, their_id],
                until: WaitUntil::AllTerminal,
                timeout_ms: 10_000,
                since_seq: None,
                progress_bytes: None,
            },
        )
        .await
        .unwrap();
    assert!(done.jobs.iter().all(|j| j.state == JobState::Completed));
    assert_eq!(std::fs::read(root_path.join("set.bin")).unwrap(), data);

    // A service sees its own jobs, files and audit records; another client's job is "not found".
    let visible = backend
        .jobs_list(&service, Default::default())
        .await
        .unwrap();
    assert_eq!(
        visible.items.iter().map(|j| j.id).collect::<Vec<_>>(),
        [own_id]
    );
    assert_eq!(
        backend
            .jobs_get(
                &service,
                JobsGetParams {
                    job_id: their_id,
                    reveal_urls: false
                }
            )
            .await
            .unwrap_err()
            .envelope()
            .code
            .as_str(),
        "api.not_found"
    );
    assert_eq!(
        backend
            .jobs_list(&agent, Default::default())
            .await
            .unwrap()
            .items
            .len(),
        2
    );
    let files = backend
        .files_list(&service, Default::default())
        .await
        .unwrap();
    assert_eq!(
        files
            .items
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>(),
        ["set.bin"]
    );
    let records = backend
        .audit_tail(
            &service,
            AuditTailParams {
                limit: Some(100),
                after_seq: None,
            },
        )
        .await
        .unwrap();
    assert!(!records.records.is_empty());
    assert!(
        records
            .records
            .iter()
            .all(|r| r["actor"]["client_id"] == service.identity.credential_id.as_str())
    );

    // Revocation withdraws its pending requests, deactivates its roots and ends authentication.
    let pending = backend
        .sessions_request(
            &service,
            serde_json::from_value(
                serde_json::json!({"origin":"https://portal.example.org","scope":"site","reason":"datasets"}),
            )
            .unwrap(),
        )
        .await
        .unwrap()
        .approval_id
        .unwrap();
    assert_eq!(
        backend
            .revoke_client_as(
                actor.clone(),
                &admin,
                ClientsRevokeParams {
                    credential_id: admin.identity.credential_id.to_string(),
                    idempotency_key: None
                }
            )
            .await
            .unwrap_err()
            .envelope()
            .code
            .as_str(),
        "api.invalid_params"
    );
    let revoked = backend
        .revoke_client_as(
            actor.clone(),
            &admin,
            ClientsRevokeParams {
                credential_id: service.identity.credential_id.to_string(),
                idempotency_key: None,
            },
        )
        .await
        .unwrap();
    contract("ClientsRevokeResult", &revoked);
    assert!(revoked.client.revoked && revoked.client.revoked_at.is_some());
    assert!(!Path::new(&created.credential_file).exists());
    assert!(
        backend
            .credentials
            .get(&service.identity.credential_id)
            .unwrap()
            .revoked
    );
    assert!(
        backend
            .credentials
            .roots(&service.identity.credential_id)
            .is_empty()
    );
    let withdrawn = backend
        .journal
        .read(|tx| tx.approval(pending))
        .unwrap()
        .unwrap();
    assert_eq!(withdrawn.state, ApprovalState::Canceled);
    let again = backend
        .revoke_client_as(
            actor,
            &admin,
            ClientsRevokeParams {
                credential_id: service.identity.credential_id.to_string(),
                idempotency_key: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(again.connections_closed, 0);
    // Its downloaded file and job continue to exist for the person.
    assert!(root_path.join("set.bin").is_file());
    backend.engine.shutdown().await.unwrap();
    http.abort();
}

#[tokio::test]
async fn one_decision_reaches_every_job_of_a_batched_approval() {
    use zeus_api::methods::sessions::{ApprovalView, ConfirmationLevel, DecideOutcome};
    let dir = tempfile::tempdir().unwrap();
    let backend = setup(dir.path()).await;
    let (agent, _rx) = context(dir.path(), "agent");
    let (human, _rx2) = context(dir.path(), "cli");
    let request: EnqueueParams = serde_json::from_value(serde_json::json!({"jobs":[
        {"url":"https://portal.example.org/a.pdf","session_origin":"https://portal.example.org"},
        {"url":"https://portal.example.org/b.pdf","session_origin":"https://portal.example.org"},
        {"url":"https://portal.example.org/c.pdf","session_origin":"https://portal.example.org"}
    ]}))
    .unwrap();
    let enqueued = backend.jobs_enqueue(&agent, request).await.unwrap();
    let approvals: std::collections::BTreeSet<_> = enqueued
        .jobs
        .iter()
        .map(|j| j.approval_id.unwrap())
        .collect();
    assert_eq!(approvals.len(), 1, "{enqueued:?}");
    let id = *approvals.iter().next().unwrap();
    let view = ApprovalView::try_from(backend.consent.get(&human.identity, id).unwrap()).unwrap();
    assert_eq!(view.approval.revision, Some(3));
    let p = ApprovalsDecideParams {
        approval_id: id,
        request_hash: view.request_hash,
        outcome: DecideOutcome::Deny,
        scope: None,
        channel: Surface::CliInteractive,
        confirmation: ConfirmationLevel::Gesture,
        idempotency_key: None,
    };
    let evidence = serde_json::json!({"client_id":human.identity.credential_id.as_str(),"surface":"cli_interactive",
        "exe_verified":true,"credential_kind":human.identity.kind,"effective_kind":human.identity.kind,
        "role":human.identity.role,"tty":"verified","confirmation":"gesture","peer_pid":1});
    let (approval, grant) = backend
        .consent
        .decide(&human.identity, &p, evidence)
        .await
        .unwrap();
    backend.apply_decision(&approval, &grant).await.unwrap();
    for job in &enqueued.jobs {
        let job = backend.engine.get(job.job_id.unwrap()).await.unwrap();
        assert_eq!(
            job.state,
            JobState::Failed,
            "every covered job follows the decision"
        );
    }
    backend.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn emergency_stop_persists_and_unverified_clients_cannot_release_it() {
    let dir = tempfile::tempdir().unwrap();
    let backend = setup(dir.path()).await;
    backend
        .change_kill_switch(Actor::human(Surface::CliInteractive), true, None)
        .await
        .unwrap();
    let (agent, _rx) = context(dir.path(), "agent");
    let (human, _rx2) = context(dir.path(), "cli");
    for ctx in [&agent, &human] {
        assert!(
            backend
                .daemon_kill_switch(
                    ctx,
                    KillSwitchParams {
                        action: KillSwitchAction::Release,
                        surface: Some(Surface::CliInteractive),
                        reason: None
                    }
                )
                .await
                .is_err()
        );
    }
    let request =
        serde_json::from_value(serde_json::json!({"jobs":[{"url":"https://example.org/file"}]}))
            .unwrap();
    assert_eq!(
        backend
            .jobs_enqueue(&agent, request)
            .await
            .unwrap_err()
            .envelope()
            .code
            .as_str(),
        "policy.kill_switch_engaged"
    );
    backend.engine.shutdown().await.unwrap();
    drop(backend);
    let backend = setup(dir.path()).await;
    assert!(backend.journal.meta().unwrap().kill_switch_engaged());
    backend
        .change_kill_switch(Actor::human(Surface::CliInteractive), false, None)
        .await
        .unwrap();
    backend.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn emergency_stop_without_audit_is_recorded_once_the_log_recovers() {
    use std::sync::atomic::Ordering;
    fn kill_switch_records(backend: &RuntimeBackend) -> Vec<serde_json::Value> {
        let mut records = Vec::new();
        backend
            .audit
            .visit_records(|r| {
                // Outbox observations of the journal event carry `journal_seq`; only direct records count.
                if (r.action == "daemon.kill_switch" || r.action == "audit.error")
                    && !r.data.contains_key("journal_seq")
                {
                    records.push(serde_json::to_value(r).unwrap());
                }
                Ok(())
            })
            .unwrap();
        records
    }
    let dir = tempfile::tempdir().unwrap();
    let backend = setup(dir.path()).await;
    backend
        .fail_kill_switch_audit
        .store(true, Ordering::Release);
    let result = backend
        .change_kill_switch(Actor::human(Surface::Tui), true, Some("stop now"))
        .await
        .unwrap();
    assert!(result.engaged && result.changed);
    assert!(backend.journal.meta().unwrap().kill_switch_engaged());
    assert!(backend.deferred_audit.is_pending());
    assert!(kill_switch_records(&backend).is_empty());
    // Release is a widening action and fails closed while the log is unavailable.
    assert!(
        backend
            .change_kill_switch(Actor::human(Surface::Tui), false, None)
            .await
            .is_err()
    );
    assert!(backend.journal.meta().unwrap().kill_switch_engaged());
    backend
        .fail_kill_switch_audit
        .store(false, Ordering::Release);
    backend.maintain().await.unwrap();
    assert!(!backend.deferred_audit.is_pending());
    let records = kill_switch_records(&backend);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["action"], "audit.error");
    assert_eq!(records[0]["data"]["reason"], "write_ahead_deferred");
    assert_eq!(records[1]["data"]["deferred"], true);
    assert_eq!(records[1]["data"]["action"], "engage");
    assert_eq!(
        records[1]["data"]["reason_blake3"],
        blake3::hash(b"stop now").to_hex().to_string()
    );
    assert!(!records[1].to_string().contains("stop now"));
    backend.engine.shutdown().await.unwrap();
    drop(backend);
    let backend = setup(dir.path()).await;
    assert_eq!(kill_switch_records(&backend).len(), 2);
    backend.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn unverified_admin_is_downgraded_before_authentication_reply() {
    use zeus_api::{
        client::{Client, ClientConfig, IpcClient, call},
        methods::*,
        server::{ServerContext, serve_connection},
    };
    let dir = tempfile::tempdir().unwrap();
    let backend = setup(dir.path()).await;
    let paths = paths(dir.path());
    let classify = backend.clone();
    let router = crate::build_router(backend.clone())
        .on_authenticate(move |ctx| classify.classify_client(ctx));
    let secret = Arc::new(crate::provision::load_ipc_secret(&paths).unwrap());
    let server = Arc::new(ServerContext {
        secret: secret.clone(),
        credentials: Arc::new(
            crate::provision::FileCredentialStore::load(&paths.state_dir.join("credentials.json"))
                .unwrap(),
        ),
        daemon: backend.info(),
        router: Arc::new(router),
    });
    let (a, b) = tokio::io::duplex(64 * 1024);
    let task = tokio::spawn(serve_connection(b, server));
    let client = IpcClient::from_stream(
        a,
        crate::provision::load_client_credential(&paths, "cli").unwrap(),
        &secret,
        ClientConfig::new(ClientInfo {
            name: "forged trusted terminal".into(),
            version: "0".into(),
            role: ClientRole::Cli,
        }),
    )
    .await
    .unwrap();
    assert_eq!(client.session().credential_kind, ClientKind::Admin);
    contract("DaemonAuthenticateResult", client.session());
    assert_eq!(client.session().kind, ClientKind::Agent);
    assert!(client.session().downgraded);
    assert!(
        !client
            .session()
            .permissions
            .contains(&zeus_api::Permission::ApprovalsDecide)
    );
    assert!(
        call::<DaemonShutdown, _>(&client, ShutdownParams { grace_ms: None })
            .await
            .is_err()
    );
    assert!(
        call::<DaemonStatus, _>(&client, StatusParams {})
            .await
            .is_ok()
    );
    drop(client);
    task.abort();
    let _ = task.await;
    backend.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_handoff_cannot_launder_browser_security_or_cancel_decisions() {
    use zeus_session::protocol as nm;
    let dir = tempfile::tempdir().unwrap();
    let backend = setup(dir.path()).await;
    let (mut ctx, _rx) = context(dir.path(), "host");
    let extension = "abcdefghijklmnopabcdefghijklmnop";
    ctx.attach = Some(zeus_api::methods::daemon::AttachInfo {
        browser: zeus_types::BrowserName::Chrome,
        attested_extension: extension.into(),
        host_version: "0.1.0".into(),
        parent_verified: true,
    });
    let hello=serde_json::from_value(serde_json::json!({"client":"zeus-extension","extension_id":extension,"extension_version":"0.1.0","browser":{"name":"chrome","version":"130"},"protocol_versions":[1],"capabilities":["handoff"],"profile_id":"11111111111111111111111111111111","sw_instance_id":"2222222222222222"})).unwrap();
    let connection = backend.browser.attach(ctx, hello, true).unwrap();
    for reason in [
        Some("FILE_VIRUS_INFECTED"),
        Some("FILE_BLOCKED"),
        Some("FILE_SECURITY_CHECK_FAILED"),
        Some("SERVER_CERT_PROBLEM"),
        Some("USER_CANCELED"),
        None,
    ] {
        let body=serde_json::from_value(serde_json::json!({"offer_key":"synthetic","trigger":"interrupted","download":{"browser_download_id":1,"url":"https://example.org/file","final_url":"https://example.org/file","referrer":null,"filename":"file.bin","mime":null,"bytes_received":0,"total_bytes":null,"start_time":"2026-10-16T00:00:00.000Z","incognito":false,"can_resume":false,"interrupt_reason":reason}})).unwrap();
        let result = backend
            .native_request(
                &connection,
                &nm::MessageId::new("e1").unwrap(),
                nm::ExtensionBody::HandoffOffer(body),
            )
            .await;
        assert_eq!(
            result.unwrap_err().envelope().code.as_str(),
            "api.permission_denied"
        );
    }
    assert!(backend.journal.list_incomplete().unwrap().is_empty());
    backend.browser.disconnect(connection.ctx.connection_id);
    backend.engine.shutdown().await.unwrap();
}
