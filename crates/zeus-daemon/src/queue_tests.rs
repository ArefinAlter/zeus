//! Queues against the real engine and a loopback server: hold while stopped, release up to `max_concurrent`, the
//! queue's speed limit, a stop that pauses, and a restart that keeps queues, membership and held state.
use super::tests::{context, setup};
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeus_api::methods::queues::{OnFinish, QueueDef};

/// A server that answers any path with `data`, honouring one `Range` (always as 206, as the engine expects).
async fn server(data: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
    versioned_server(Arc::new(std::sync::Mutex::new(data))).await
}

async fn versioned_server(
    data: Arc<std::sync::Mutex<Vec<u8>>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let data = data.lock().unwrap().clone();
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
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\nETag: \"{}\"\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                    data.len(),
                    end - start + 1,
                    blake3::hash(&data).to_hex()
                );
                if stream.write_all(header.as_bytes()).await.is_ok() {
                    let _ = stream.write_all(&data[start..=end]).await;
                }
            });
        }
    });
    (base, task)
}

fn job(url: &str, name: &str, queue: &str) -> NewJob {
    serde_json::from_value(serde_json::json!({
        "url": url,
        "filename": name,
        "labels": { QUEUE_LABEL: queue }
    }))
    .unwrap()
}

fn state_of(backend: &RuntimeBackend, id: zeus_types::JobId) -> JobState {
    backend.journal.job(id).unwrap().unwrap().job.state
}

fn occupying(state: JobState) -> bool {
    matches!(
        state,
        JobState::Queued
            | JobState::Probing
            | JobState::Downloading
            | JobState::Verifying
            | JobState::Finalizing
            | JobState::RetryScheduled
    )
}

/// Tick the scheduler until `done` holds, sampling how many of `ids` hold a slot at the same time.
async fn drive(
    backend: &RuntimeBackend,
    ids: &[zeus_types::JobId],
    timeout: Duration,
    mut done: impl FnMut() -> bool,
) -> usize {
    let started = Instant::now();
    let mut most = 0;
    while !done() {
        assert!(started.elapsed() < timeout, "timed out");
        backend.queues.tick().await.unwrap();
        let busy = ids
            .iter()
            .filter(|id| occupying(state_of(backend, **id)))
            .count();
        most = most.max(busy);
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    most
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queues_hold_release_limit_stop_and_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let backend = setup(dir.path()).await;
    let (ctx, _rx) = context(dir.path(), "cli");
    let (base, http) = server((0..256 * 1024).map(|n| (n % 251) as u8).collect()).await;

    // A queue of two at a time, limited to 512 KiB/s for all of it, that notifies when it is done.
    backend
        .queues_put(
            &ctx,
            QueuesPutParams {
                queue: QueueDef {
                    id: "night".into(),
                    name: "Night".into(),
                    max_concurrent: 2,
                    on_finish: OnFinish::Notify,
                    limit_kib_s: Some(512),
                    ..serde_json::from_value(serde_json::json!({"id": "x", "name": "x"})).unwrap()
                },
                idempotency_key: Some("put-night".into()),
            },
        )
        .await
        .unwrap();

    // A label naming no queue is refused before anything is created.
    let err = backend
        .jobs_enqueue(
            &ctx,
            EnqueueParams {
                jobs: vec![job(&format!("{base}/x.bin"), "x.bin", "ghost")],
                dry_run: false,
                idempotency_key: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.envelope().code.as_str(), "api.invalid_params");

    let enqueued = backend
        .jobs_enqueue(
            &ctx,
            EnqueueParams {
                jobs: (0..4)
                    .map(|n| job(&format!("{base}/f{n}.bin"), &format!("f{n}.bin"), "night"))
                    .collect(),
                dry_run: false,
                idempotency_key: Some("four".into()),
            },
        )
        .await
        .unwrap();
    let ids: Vec<_> = enqueued.jobs.iter().map(|j| j.job_id.unwrap()).collect();
    assert_eq!(ids.len(), 4);

    // Held while the queue is stopped: paused by the daemon, reported as waiting.
    tokio::time::sleep(Duration::from_millis(400)).await;
    backend.queues.tick().await.unwrap();
    for id in &ids {
        assert_eq!(state_of(&backend, *id), JobState::Paused, "{id} is held");
        assert_eq!(
            backend
                .journal
                .job(*id)
                .unwrap()
                .unwrap()
                .job
                .progress
                .bytes_done,
            0
        );
    }
    let listed = backend
        .queues_list(&ctx, QueuesListParams {})
        .await
        .unwrap();
    let night = listed.queues.iter().find(|q| q.def.id == "night").unwrap();
    assert!(!night.running);
    assert_eq!((night.waiting, night.active), (4, 0));
    assert_eq!(night.order, ids);
    assert_eq!(
        listed.queues[0].def.id, MAIN_QUEUE,
        "the built-in queue comes first"
    );

    // A retried call with the same key replays the stored answer.
    let replay = backend
        .queues_put(
            &ctx,
            QueuesPutParams {
                queue: night.def.clone(),
                idempotency_key: Some("put-night".into()),
            },
        )
        .await
        .unwrap();
    assert!(replay.replayed);

    // Restart: queues, membership and held state come back and the scheduler carries on.
    backend.engine.shutdown().await.unwrap();
    drop(backend);
    let backend = setup(dir.path()).await;
    let listed = backend
        .queues_list(&ctx, QueuesListParams {})
        .await
        .unwrap();
    let night = listed.queues.iter().find(|q| q.def.id == "night").unwrap();
    assert_eq!(night.order, ids);
    assert_eq!(night.held_job_ids, ids);
    assert_eq!(night.def.limit_kib_s, Some(512));

    // Starting releases two; the other two wait for a slot. 1 MiB at 512 KiB/s takes about two seconds.
    let started = Instant::now();
    backend
        .queues_control(
            &ctx,
            QueuesControlParams {
                action: QueueAction::Start { id: "night".into() },
                idempotency_key: None,
            },
        )
        .await
        .unwrap();
    let most = drive(&backend, &ids, Duration::from_secs(40), || {
        ids.iter()
            .all(|id| state_of(&backend, *id) == JobState::Completed)
    })
    .await;
    let took = started.elapsed();
    assert!(most <= 2, "at most two at a time, saw {most}");
    assert!(
        took >= Duration::from_millis(1800),
        "1 MiB under a 512 KiB/s queue limit cannot finish in {took:?}"
    );
    println!("1 MiB through the night queue limited to 512 KiB/s: {took:?}");
    for id in &ids {
        let job = backend.journal.job(*id).unwrap().unwrap().job;
        assert_eq!(job.progress.bytes_done, 256 * 1024);
    }

    // The queue ran out of work: it stopped itself and waits for its on-finish action to be acknowledged.
    backend.queues.tick().await.unwrap();
    let listed = backend
        .queues_list(&ctx, QueuesListParams {})
        .await
        .unwrap();
    let night = listed.queues.iter().find(|q| q.def.id == "night").unwrap();
    assert!(!night.running && night.order.is_empty());
    assert!(night.finished_at_ms.is_some() && night.finish_action_pending);
    let acknowledged = backend
        .queues_control(
            &ctx,
            QueuesControlParams {
                action: QueueAction::AckFinish { id: "night".into() },
                idempotency_key: None,
            },
        )
        .await
        .unwrap();
    assert!(!acknowledged.queue.unwrap().finish_action_pending);

    // Stopping a running queue pauses what is downloading.
    let slow_limit = QueueDef {
        limit_kib_s: Some(64),
        ..night.def.clone()
    };
    backend
        .queues_put(
            &ctx,
            QueuesPutParams {
                queue: slow_limit,
                idempotency_key: None,
            },
        )
        .await
        .unwrap();
    backend
        .queues_control(
            &ctx,
            QueuesControlParams {
                action: QueueAction::Start { id: "night".into() },
                idempotency_key: None,
            },
        )
        .await
        .unwrap();
    let more = backend
        .jobs_enqueue(
            &ctx,
            EnqueueParams {
                jobs: vec![job(&format!("{base}/slow.bin"), "slow.bin", "night")],
                dry_run: false,
                idempotency_key: None,
            },
        )
        .await
        .unwrap();
    let slow = more.jobs[0].job_id.unwrap();
    let begun = Instant::now();
    while !matches!(state_of(&backend, slow), JobState::Downloading) {
        assert!(
            begun.elapsed() < Duration::from_secs(10),
            "the download never began"
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    backend
        .queues_control(
            &ctx,
            QueuesControlParams {
                action: QueueAction::Stop { id: "night".into() },
                idempotency_key: None,
            },
        )
        .await
        .unwrap();
    let stopped = Instant::now();
    while state_of(&backend, slow) != JobState::Paused {
        assert!(
            stopped.elapsed() < Duration::from_secs(3),
            "a stop pauses promptly, even while the limiter is making the download wait"
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    let listed = backend
        .queues_list(&ctx, QueuesListParams {})
        .await
        .unwrap();
    let night = listed.queues.iter().find(|q| q.def.id == "night").unwrap();
    assert!(!night.running);
    assert_eq!(night.held_job_ids, vec![slow]);

    backend.engine.shutdown().await.unwrap();
    http.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_service_sees_only_its_own_jobs_in_queues_and_cannot_change_them() {
    use zeus_api::ClientKind;
    use zeus_types::{Actor, Surface};
    let dir = tempfile::tempdir().unwrap();
    let backend = setup(dir.path()).await;
    let (admin, _rx) = context(dir.path(), "cli");
    backend
        .add_client_as(
            Actor::human(Surface::CliInteractive),
            &admin,
            ClientsAddParams {
                name: "ogun".into(),
                kind: ClientKind::Service,
                role: None,
                permissions: None,
                idempotency_key: None,
            },
        )
        .await
        .unwrap();
    let (service, _rx2) = context(dir.path(), "ogun");
    let (base, http) = server(vec![1; 4096]).await;
    backend
        .queues_put(
            &admin,
            QueuesPutParams {
                queue: serde_json::from_value(serde_json::json!({"id": "night", "name": "Night"}))
                    .unwrap(),
                idempotency_key: None,
            },
        )
        .await
        .unwrap();
    let enqueued = backend
        .jobs_enqueue(
            &admin,
            EnqueueParams {
                jobs: vec![job(&format!("{base}/a.bin"), "a.bin", "night")],
                dry_run: false,
                idempotency_key: None,
            },
        )
        .await
        .unwrap();
    let theirs = enqueued.jobs[0].job_id.unwrap();

    // The service reads queues but sees none of the person's jobs in them.
    let listed = backend
        .queues_list(&service, QueuesListParams {})
        .await
        .unwrap();
    let night = listed.queues.iter().find(|q| q.def.id == "night").unwrap();
    assert!(night.order.is_empty() && night.held_job_ids.is_empty());
    let all = backend
        .queues_list(&admin, QueuesListParams {})
        .await
        .unwrap();
    assert_eq!(
        all.queues
            .iter()
            .find(|q| q.def.id == "night")
            .unwrap()
            .order,
        vec![theirs]
    );

    // It cannot name them, and it cannot change queues, the limiter or the defaults.
    let refused = backend
        .queues_control(
            &service,
            QueuesControlParams {
                action: QueueAction::Unassign {
                    job_ids: vec![theirs],
                },
                idempotency_key: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(refused.envelope().code.as_str(), "api.not_found");
    for params in [
        QueueAction::Start { id: "night".into() },
        QueueAction::Stop { id: "night".into() },
        QueueAction::AckFinish { id: "night".into() },
    ] {
        let e = backend
            .queues_control(
                &service,
                QueuesControlParams {
                    action: params,
                    idempotency_key: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(e.envelope().code.as_str(), "api.permission_denied");
    }
    assert!(
        backend
            .queues_put(
                &service,
                QueuesPutParams {
                    queue: serde_json::from_value(
                        serde_json::json!({"id": "mine", "name": "Mine"})
                    )
                    .unwrap(),
                    idempotency_key: None,
                },
            )
            .await
            .is_err()
    );
    assert!(
        backend
            .queues_configure(
                &service,
                QueuesConfigureParams {
                    limiter: Some(LimiterDef {
                        enabled: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .is_err()
    );
    assert!(
        backend
            .queues_delete(
                &service,
                QueuesDeleteParams {
                    id: "night".into(),
                    idempotency_key: None,
                },
            )
            .await
            .is_err()
    );
    backend.engine.shutdown().await.unwrap();
    http.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn synchronization_downloads_changed_bytes_beside_the_original_file() {
    use zeus_api::methods::queues::{NetworkMode, QueueDefaults, QueueKind, SyncStatus};
    let dir = tempfile::tempdir().unwrap();
    let backend = setup(dir.path()).await;
    let (ctx, _rx) = context(dir.path(), "cli");
    backend
        .queues
        .configure(
            None,
            Some(QueueDefaults {
                network_mode: NetworkMode::Unmetered,
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    let data = Arc::new(std::sync::Mutex::new(vec![17_u8; 32 * 1024]));
    let (base, http) = versioned_server(data.clone()).await;
    backend
        .queues
        .put(QueueDef {
            id: "versions".into(),
            name: "Versions".into(),
            kind: QueueKind::Sync,
            ..serde_json::from_value(serde_json::json!({"id":"x","name":"x"})).unwrap()
        })
        .await
        .unwrap();
    backend
        .queues
        .control(
            QueueAction::Start {
                id: "versions".into(),
            },
            &|_| true,
        )
        .await
        .unwrap();
    let enqueued = backend
        .jobs_enqueue(
            &ctx,
            EnqueueParams {
                jobs: vec![job(
                    &format!("{base}/version.bin"),
                    "version.bin",
                    "versions",
                )],
                dry_run: false,
                idempotency_key: None,
            },
        )
        .await
        .unwrap();
    let old = enqueued.jobs[0].job_id.unwrap();
    drive(&backend, &[old], Duration::from_secs(15), || {
        state_of(&backend, old) == JobState::Completed
    })
    .await;
    let old_result = backend
        .journal
        .job(old)
        .unwrap()
        .unwrap()
        .job
        .result
        .unwrap();
    backend.queues.tick().await.unwrap(); // Register the first completed version.
    *data.lock().unwrap() = vec![29_u8; 48 * 1024];
    backend.queues.tick().await.unwrap(); // Probe and enqueue the changed representation.
    let watch = backend
        .queues
        .queue_view("versions", &|_| true)
        .await
        .unwrap()
        .sync_files[0]
        .clone();
    let new = watch
        .pending_job_id
        .expect("changed validators must create a new job");
    drive(&backend, &[new], Duration::from_secs(15), || {
        state_of(&backend, new) == JobState::Completed
    })
    .await;
    backend.queues.tick().await.unwrap();
    let new_result = backend
        .journal
        .job(new)
        .unwrap()
        .unwrap()
        .job
        .result
        .unwrap();
    assert_ne!(old_result.path, new_result.path);
    assert_eq!(
        std::path::Path::new(&old_result.path).parent(),
        std::path::Path::new(&new_result.path).parent()
    );
    assert_eq!(
        std::fs::read(&old_result.path).unwrap(),
        vec![17_u8; 32 * 1024]
    );
    assert_eq!(
        std::fs::read(&new_result.path).unwrap(),
        vec![29_u8; 48 * 1024]
    );
    let watch = backend
        .queues
        .queue_view("versions", &|_| true)
        .await
        .unwrap()
        .sync_files[0]
        .clone();
    assert_eq!(watch.job_id, new);
    assert_eq!(watch.status, SyncStatus::Updated);
    assert!(watch.pending_job_id.is_none());
    backend
        .credentials
        .revoke(
            &dir.path().join("state/credentials"),
            &ctx.identity.credential_id,
            Timestamp::now(),
        )
        .unwrap();
    backend
        .queues
        .control(
            QueueAction::Start {
                id: "versions".into(),
            },
            &|_| true,
        )
        .await
        .unwrap();
    backend.queues.tick().await.unwrap();
    let denied = backend
        .queues
        .queue_view("versions", &|_| true)
        .await
        .unwrap()
        .sync_files[0]
        .clone();
    assert_eq!(denied.status, SyncStatus::PolicyBlocked);
    assert!(denied.pending_job_id.is_none());
    backend.engine.shutdown().await.unwrap();
    http.abort();
}
