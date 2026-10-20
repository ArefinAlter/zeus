//! Daemon process lifecycle and exclusive state ownership.
use anyhow::{Context, Result};
use std::sync::Arc;
use zeus_api::endpoint::{DiscoveryRecord, ZeusPaths};
use zeus_api::server::{ServerContext, serve_listener};
use zeus_types::Timestamp;

/// Run until shutdown is requested over the authenticated API or by the OS.
pub async fn run(paths: ZeusPaths) -> Result<()> {
    crate::provision::provision(&paths, Timestamp::now())?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(paths.lock_file())
        .context("cannot open daemon lock")?;
    lock.try_lock()
        .context("another daemon owns this state directory")?;
    let endpoint = paths.endpoint();
    #[cfg(unix)]
    if let zeus_api::endpoint::Endpoint::UnixSocket(path) = &endpoint {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        // Only the exclusive state owner may reclaim its own stale socket, never a symlink/file.
        if let Ok(metadata) = std::fs::symlink_metadata(path) {
            if !metadata.file_type().is_socket()
                || metadata.uid().to_string() != zeus_api::transport::user_identity()?
            {
                anyhow::bail!("refusing to replace unexpected IPC endpoint");
            }
            if zeus_api::transport::connect(&endpoint).await.is_ok() {
                anyhow::bail!("another daemon is listening on this endpoint");
            }
            std::fs::remove_file(path).context("cannot remove stale socket")?;
        }
    }
    let listener = zeus_api::transport::bind(&endpoint)?;
    let config = crate::config::DaemonConfig::load(&paths.config_file())?;
    let backend = crate::runtime::RuntimeBackend::open(paths.state_dir.clone(), config).await?;
    backend.set_config_path(paths.config_file())?;
    let cleanup = backend.clone();
    let native = backend.clone();
    let classify = backend.clone();
    let credentials = backend.credentials();
    let rates = Arc::new(zeus_api::rate::RateLimiter::per_caller(
        backend
            .config()
            .api
            .rate_limits
            .per_caller(credentials.clone()),
    ));
    backend.share_rate_limiter(rates.clone());
    let router = crate::build_router(backend.clone())
        .with_shutdown(backend.shutdown.clone())
        .with_revocations(credentials.revocations())
        .with_rate_limiter(rates)
        .on_authenticate(move |ctx| classify.classify_client(ctx))
        .on_disconnect(move |ctx| {
            let cleanup = cleanup.clone();
            async move {
                cleanup.disconnect(&ctx).await;
            }
        })
        .native_messaging(move |ctx, frame| {
            let native = native.clone();
            async move { native.native_frame(ctx, frame).await }
        });
    let context = Arc::new(ServerContext {
        secret: Arc::new(crate::provision::load_ipc_secret(&paths)?),
        // One store for handshakes and handlers, so credential changes reach live connections.
        credentials,
        daemon: backend.info(),
        router: Arc::new(router),
    });
    let discovery = DiscoveryRecord {
        pid: std::process::id(),
        instance_id: backend.info().instance_id,
        version: env!("CARGO_PKG_VERSION").into(),
        protocol_versions: vec![1],
        endpoint: endpoint.to_string(),
        started_at: Timestamp::now(),
    };
    discovery.write(&paths.discovery_file())?;
    tracing::info!("daemon ready");
    let maintenance_backend = backend.clone();
    let workflows = backend.workflows.clone();
    let cloud = backend.cloud.clone();
    let cloud_runner = tokio::spawn(async move {
        loop {
            if let Err(error) = cloud.tick().await {
                tracing::warn!(code=%error.envelope().code,"cloud execution needs attention");
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
    let workflow_runner = tokio::spawn(async move {
        loop {
            if let Err(e) = workflows.tick().await {
                tracing::warn!(code = %e.envelope().code, "workflow execution needs attention");
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
    let maintenance = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            interval.tick().await;
            if let Err(e) = maintenance_backend.maintain().await {
                tracing::warn!(code = %e.envelope().code, "daemon maintenance needs attention");
            }
        }
    });
    let mut serving = tokio::spawn(serve_listener(context, listener));
    let mut serving_finished = false;
    let result = tokio::select! {
        result = &mut serving => {
            serving_finished = true;
            let announced = backend.engine.begin_shutdown(zeus_types::event::StopReason::FatalError).map_err(anyhow::Error::from);
            backend.shutdown.request(0,"fatal_error",None);
            result.map_err(anyhow::Error::from).and_then(|result|result.map_err(anyhow::Error::from)).and(announced)
        },
        _ = backend.shutdown.stopping() => Ok(()),
        result = termination_signal() => {
            let announced = backend.engine.begin_shutdown(zeus_types::event::StopReason::Signal).map_err(anyhow::Error::from);
            backend.shutdown.request(10_000,"signal",None);
            result.and(announced)
        },
    };
    let notice = backend.shutdown.stopping().await;
    maintenance.abort();
    let _ = maintenance.await;
    cloud_runner.abort();
    let _ = cloud_runner.await;
    backend.cloud.shutdown().await;
    backend.workflows.shutdown();
    workflow_runner.abort();
    let _ = workflow_runner.await;
    let draining = backend.engine.shutdown();
    tokio::pin!(draining);
    let drained = match tokio::time::timeout_at(notice.deadline, &mut draining).await {
        Ok(result) => result,
        Err(_) => {
            // IPC still closes at its deadline. Do not drop the state lock or interrupt a file
            // fsync/journal commit merely to meet a wall-clock target: doing so risks durable data.
            tracing::warn!(
                "shutdown grace expired; waiting for storage writers to reach a safe point"
            );
            draining.await
        }
    };
    let served = if !serving_finished {
        serving
            .await
            .context("IPC shutdown task failed")
            .and_then(|result| result.map_err(anyhow::Error::from))
    } else {
        Ok(())
    };
    loop {
        let meta = backend.journal.meta()?;
        if meta.manifest_cursor == meta.last_event_seq
            && meta.artifact_cursor == meta.last_event_seq
        {
            break;
        }
        backend.flush_outbox().await?;
    }
    DiscoveryRecord::remove(&paths.discovery_file())?;
    drop(lock);
    result.and(drained.map_err(anyhow::Error::from)).and(served)
}

async fn termination_signal() -> Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.map_err(anyhow::Error::from),
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.map_err(anyhow::Error::from)
    }
}
