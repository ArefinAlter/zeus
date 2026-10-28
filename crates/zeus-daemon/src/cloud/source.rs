//! Public-peer source reads and full local verification; streaming never creates a full-file cache.
use super::*;
use futures::TryStreamExt;
use std::{
    io,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, ReadBuf};
use zeus_types::HttpUrl;
/// Source URL syntax policy; every resolved peer is checked independently at connection time.
pub(super) fn validate_url(url: &HttpUrl, https_only: bool) -> ApiResult<()> {
    let parsed = url.url();
    if let Some(host) = parsed.host() {
        let address = match host {
            url::Host::Ipv4(ip) => Some(std::net::IpAddr::V4(ip)),
            url::Host::Ipv6(ip) => Some(std::net::IpAddr::V6(ip)),
            url::Host::Domain(_) => None,
        };
        if address.is_some_and(|ip| !crate::workflow_network::public_ip(ip)) {
            return Err(ApiError::new(
                "policy.origin_denied",
                "source IP is outside public network policy",
            ));
        }
    }
    if parsed.username() != ""
        || parsed.password().is_some()
        || parsed.fragment().is_some()
        || parsed.port_or_known_default().is_none()
        || https_only && parsed.scheme() != "https"
    {
        return Err(invalid(
            "source URL must be public HTTP(S), without userinfo or fragment; remote_import requires HTTPS",
        ));
    }
    if parsed.host_str().is_none_or(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host.ends_with(".localhost")
            || host.ends_with(".local")
    }) {
        return Err(invalid("local source hosts are not allowed"));
    }
    Ok(())
}
async fn response(
    url: &HttpUrl,
    size: u64,
    head: bool,
    cancel: &CancellationToken,
) -> ApiResult<(HttpUrl, reqwest::Response)> {
    response_from(url, size, head, None, cancel).await
}
pub(super) async fn response_from(
    url: &HttpUrl,
    size: u64,
    head: bool,
    resume: Option<(&str, u64)>,
    cancel: &CancellationToken,
) -> ApiResult<(HttpUrl, reqwest::Response)> {
    let mut current = url.clone();
    for redirects in 0..=5 {
        validate_url(&current, head)?;
        let parsed = current.url();
        let host = parsed
            .host_str()
            .ok_or_else(|| invalid("source host missing"))?;
        let port = parsed
            .port_or_known_default()
            .ok_or_else(|| invalid("source port missing"))?;
        let addresses: Vec<SocketAddr> = tokio::select! {_ = cancel.cancelled()=>return Err(invalid("source request canceled")),result=tokio::time::timeout(std::time::Duration::from_secs(10),tokio::net::lookup_host((host,port)))=>result.map_err(|_|ApiError::new("cloud.source_unavailable","source DNS timed out"))?.map_err(|_|ApiError::new("cloud.source_unavailable","source DNS failed"))?.collect()};
        if addresses.is_empty()
            || addresses
                .iter()
                .any(|a| !crate::workflow_network::public_ip(a.ip()))
        {
            return Err(ApiError::new(
                "policy.origin_denied",
                "source resolved outside public network policy",
            ));
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(host, &addresses)
            .connect_timeout(std::time::Duration::from_secs(15))
            .read_timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|_| fail())?;
        let request = if head {
            client.head(current.expose())
        } else {
            client.get(current.expose())
        }
        .header(reqwest::header::ACCEPT_ENCODING, "identity");
        let request = if let Some((etag, offset)) = resume {
            request
                .header(
                    reqwest::header::RANGE,
                    format!("bytes={offset}-{}", size - 1),
                )
                .header(reqwest::header::IF_RANGE, etag)
        } else {
            request
        };
        let response = tokio::select! {_ = cancel.cancelled()=>return Err(invalid("source request canceled")),result=request.send()=>result.map_err(|_|ApiError::new("cloud.source_unavailable","source HTTP request failed"))?};
        if response.remote_addr().is_none_or(|peer| {
            !crate::workflow_network::public_ip(peer.ip())
                || !addresses.iter().any(|a| a.ip() == peer.ip())
        }) {
            return Err(ApiError::new(
                "policy.origin_denied",
                "actual source peer differs from the vetted public address",
            ));
        }
        if response.status().is_redirection() {
            if redirects == 5 {
                return Err(invalid("too many source redirects"));
            }
            let next = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| invalid("invalid source redirect"))?;
            let next = current
                .url()
                .join(next)
                .map_err(|_| invalid("invalid source redirect"))?;
            if current.url().scheme() == "https" && next.scheme() != "https" {
                return Err(ApiError::new(
                    "policy.origin_denied",
                    "HTTPS source cannot redirect to plaintext",
                ));
            }
            current =
                HttpUrl::parse(next.as_str()).map_err(|_| invalid("invalid source redirect"))?;
            continue;
        }
        super::staging::validate_response(response.status(), response.headers(), size, resume)?;
        if response
            .headers()
            .get(reqwest::header::CONTENT_ENCODING)
            .is_some_and(|v| v.as_bytes() != b"identity")
        {
            return Err(invalid("encoded source representations are not allowed"));
        }
        return Ok((current, response));
    }
    Err(fail())
}
pub(super) async fn import_url(
    url: &HttpUrl,
    size: u64,
    cancel: &CancellationToken,
) -> ApiResult<zeus_cloud::SecretString> {
    let (final_url, _) = response(url, size, true, cancel).await?;
    Ok(zeus_cloud::SecretString::new(final_url.expose().to_owned()))
}
struct GuardedReader {
    inner: zeus_cloud::UploadSource,
    service: Arc<CloudService>,
    delivery_id: String,
    cancel: CancellationToken,
}
impl AsyncRead for GuardedReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.cancel.is_cancelled() || self.service.guard(&self.delivery_id).is_err() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cloud delivery authorization ended",
            )));
        }
        Pin::new(&mut self.inner).poll_read(ctx, buf)
    }
}
pub(super) fn guarded(
    service: &Arc<CloudService>,
    row: &Delivery,
    cancel: &CancellationToken,
    inner: zeus_cloud::UploadSource,
) -> zeus_cloud::UploadSource {
    Box::new(GuardedReader {
        inner,
        service: service.clone(),
        delivery_id: row.view.delivery_id.clone(),
        cancel: cancel.clone(),
    })
}
async fn public_reader(
    service: &Arc<CloudService>,
    row: &Delivery,
    url: &HttpUrl,
    cancel: &CancellationToken,
) -> ApiResult<zeus_cloud::UploadSource> {
    service.guard(&row.view.delivery_id)?;
    let (_, response) = response(url, row.view.plan.size_bytes, false, cancel).await?;
    let stream = response
        .bytes_stream()
        .map_err(|_| io::Error::other("public source read failed"));
    Ok(guarded(
        service,
        row,
        cancel,
        Box::new(tokio_util::io::StreamReader::new(stream)),
    ))
}
/// Source remains on one open handle through verification and upload, so path replacement cannot swap it.
pub(super) async fn upload_source(
    service: &Arc<CloudService>,
    row: &Delivery,
    cancel: &CancellationToken,
) -> ApiResult<(zeus_cloud::UploadSource, zeus_cloud::UploadRequest)> {
    if row.view.plan.mode == DeliveryMode::StreamUpload {
        let CloudSource::PublicUrl { url, .. } = &row.source else {
            return Err(invalid("stream_upload requires public_url"));
        };
        return Ok((
            public_reader(service, row, url, cancel).await?,
            row.upload.clone(),
        ));
    }
    let mut file = match &row.source {
        CloudSource::LocalFile { path } => {
            // Snapshot into Zeus-owned staging. The original is never moved or deleted.
            // The existing full verification below checks this snapshot before any remote write.
            let source_path = local_path(service, path.expose())?;
            let mut source = tokio::fs::File::open(source_path)
                .await
                .map_err(|_| invalid("local source is unavailable"))?;
            let path = service
                .staging
                .join(format!("{}.part", row.view.delivery_id));
            if path.is_symlink() {
                return Err(invalid("unexpected staging symlink"));
            }
            let mut output = tokio::fs::OpenOptions::new()
                .write(true)
                .read(true)
                .create(true)
                .truncate(true)
                .open(path)
                .await
                .map_err(|_| fail())?;
            service
                .store
                .mutate_delivery(&row.view.delivery_id, |stored| {
                    stored.view.source_bytes_done = 0;
                    stored.view.source_verified = false;
                    Ok(())
                })?;
            let mut buffer = vec![0; 65536];
            let mut read = 0u64;
            loop {
                service.guard(&row.view.delivery_id)?;
                if cancel.is_cancelled() {
                    return Err(invalid("local source staging canceled"));
                }
                let n = source.read(&mut buffer).await.map_err(|_| fail())?;
                if n == 0 {
                    break;
                }
                read = read.saturating_add(n as u64);
                if read > row.view.plan.size_bytes || read > row.view.plan.max_bytes {
                    return Err(invalid("local source exceeds approved length"));
                }
                output.write_all(&buffer[..n]).await.map_err(|_| fail())?;
                if read.is_multiple_of(4 << 20) {
                    output.sync_all().await.map_err(|_| fail())?;
                    service
                        .store
                        .mutate_delivery(&row.view.delivery_id, |stored| {
                            stored.view.source_bytes_done = read;
                            Ok(())
                        })?;
                }
            }
            if read != row.view.plan.size_bytes {
                return Err(invalid("local source size changed"));
            }
            output.sync_all().await.map_err(|_| fail())?;
            output
                .seek(std::io::SeekFrom::Start(0))
                .await
                .map_err(|_| fail())?;
            output
        }
        CloudSource::CompletedJob { job_id } => {
            let record = service
                .journal
                .job(*job_id)
                .map_err(|_| fail())?
                .ok_or_else(missing)?;
            let result = record.job.result.ok_or_else(missing)?;
            if record.job.state != JobState::Completed
                || record.job.quarantined
                || result.size_bytes != row.view.plan.size_bytes
                || Some(result.sha256) != row.view.plan.expected_sha256
            {
                return Err(ApiError::new(
                    "cloud.source_changed",
                    "completed source identity changed",
                ));
            }
            let path = Path::new(&result.path);
            if path.is_symlink() || !std::fs::metadata(path).map_err(|_| fail())?.is_file() {
                return Err(invalid("source must remain a regular file"));
            }
            tokio::fs::File::open(path).await.map_err(|_| {
                ApiError::new(
                    "cloud.source_unavailable",
                    "completed source cannot be opened",
                )
            })?
        }
        CloudSource::PublicUrl { url, .. } => {
            // This directory and filename are Zeus-owned. An interrupted staged download is discarded,
            // never confused with the user's original completed download.
            let path = service
                .staging
                .join(format!("{}.part", row.view.delivery_id));
            if path.is_symlink() {
                return Err(invalid("unexpected staging symlink"));
            }
            if row.staged_sha256.is_some() || row.checkpoint.is_some() {
                tokio::fs::File::open(&path).await.map_err(|_| {
                    ApiError::new(
                        "cloud.source_unavailable",
                        "verified staged source is missing; remote checkpoint retained",
                    )
                })?
            } else {
                super::staging::download(service, row, url, &path, cancel).await?
            }
        }
    };
    if file.metadata().await.map_err(|_| fail())?.len() != row.view.plan.size_bytes {
        return Err(ApiError::new(
            "cloud.source_changed",
            "source file size changed",
        ));
    }
    let mut hasher = zeus_verify::hash::DualHasher::new();
    let mut blocks = Vec::new();
    let mut buffer = vec![0; 4 * 1024 * 1024];
    let mut left = row.view.plan.size_bytes;
    loop {
        service.guard(&row.view.delivery_id)?;
        if cancel.is_cancelled() {
            return Err(invalid("source verification canceled"));
        }
        let amount = left.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..amount]).await.map_err(|_| {
            ApiError::new("cloud.source_changed", "source changed during verification")
        })?;
        hasher.update(&buffer[..amount]);
        if row.view.plan.provider == ProviderKind::BaiduNetdisk {
            use md5::Digest;
            blocks.push(hex::encode(md5::Md5::digest(&buffer[..amount])));
            if blocks.len() > 8192 {
                return Err(invalid("Baidu block manifest exceeds supported maximum"));
            }
        }
        left -= amount as u64;
        if left == 0 {
            break;
        }
    }
    let mut extra = [0];
    if file.read(&mut extra).await.map_err(|_| fail())? != 0 {
        return Err(ApiError::new(
            "cloud.source_changed",
            "source grew during verification",
        ));
    }
    let digest = hasher.finalize();
    if row
        .view
        .plan
        .expected_sha256
        .is_some_and(|expected| expected != digest.sha256)
    {
        return Err(ApiError::new(
            "cloud.source_changed",
            "source hash differs from approved digest",
        ));
    }
    if row
        .staged_sha256
        .is_some_and(|expected| expected != digest.sha256)
    {
        return Err(ApiError::new(
            "cloud.source_changed",
            "verified staged source changed after its durable binding",
        ));
    }
    let mut request = row.upload.clone();
    request.expected_sha256 = Some(digest.sha256.to_string());
    if row.view.plan.provider == ProviderKind::BaiduNetdisk {
        request.block_md5 = Some(blocks);
    }
    // A restart must not bind a previously allocated remote session to newly changed local bytes.
    if let Some(checkpoint) = &row.checkpoint
        && (checkpoint.request.expected_sha256 != request.expected_sha256
            || checkpoint.request.block_md5 != request.block_md5)
    {
        return Err(ApiError::new(
            "cloud.source_changed",
            "staged source no longer matches the durable upload checkpoint",
        ));
    }
    service.guard(&row.view.delivery_id)?;
    service
        .store
        .mutate_delivery(&row.view.delivery_id, |stored| {
            if stored.stop.is_some() {
                return Err(invalid("delivery stopped"));
            }
            stored.staged_sha256 = Some(digest.sha256);
            stored.view.source_bytes_done = digest.size_bytes;
            stored.view.source_verified = true;
            stored.source_checkpoint = None;
            stored.upload = request.clone();
            Ok(())
        })?;
    file.seek(std::io::SeekFrom::Start(0))
        .await
        .map_err(|_| fail())?;
    Ok((guarded(service, row, cancel, Box::new(file)), request))
}

/// Resolve a file against the desktop user's explicit read scopes. No raw path is returned to callers.
pub(super) fn local_path(service: &CloudService, raw: &str) -> ApiResult<PathBuf> {
    let path = Path::new(raw);
    if !path.is_absolute()
        || raw.contains('\0')
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(invalid("local source path is invalid"));
    }
    if service.local_roots_file.is_symlink() {
        return Err(invalid("local upload scopes are invalid"));
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(
            std::fs::File::open(&service.local_roots_file)
                .map_err(|_| invalid("include the local folder in desktop Files settings first"))?,
            65537,
        ),
        &mut bytes,
    )
    .map_err(|_| invalid("local upload scopes are invalid"))?;
    if bytes.len() > 65536 {
        return Err(invalid("local upload scopes are invalid"));
    }
    let roots: Vec<String> =
        serde_json::from_slice(&bytes).map_err(|_| invalid("local upload scopes are invalid"))?;
    let path = std::fs::canonicalize(path).map_err(|_| invalid("local source is unavailable"))?;
    if !roots
        .iter()
        .filter_map(|r| std::fs::canonicalize(r).ok())
        .any(|r| path.starts_with(r))
    {
        return Err(invalid("local source is outside included folders"));
    }
    if !std::fs::metadata(&path).map_err(|_| fail())?.is_file() {
        return Err(invalid("local source must be a regular file"));
    }
    // A local-file spelling must not bypass the quarantine gate on completed jobs.
    // Page the journal rather than materializing every retained job at once.
    let mut after = None;
    loop {
        let rows = service
            .journal
            .read(|tx| {
                tx.jobs(&zeus_journal::JobFilter {
                    after,
                    limit: Some(256),
                    ..Default::default()
                })
            })
            .map_err(|_| fail())?;
        if rows.is_empty() {
            break;
        }
        after = rows.last().map(|row| row.job.id);
        for row in rows.into_iter().filter(|row| row.job.quarantined) {
            let paths = [
                Some(row.internal.dest_path.as_str()),
                Some(row.internal.zpart_path.as_str()),
                row.job.result.as_ref().map(|r| r.path.as_str()),
            ];
            if paths
                .into_iter()
                .flatten()
                .any(|raw| std::fs::canonicalize(raw).is_ok_and(|candidate| candidate == path))
            {
                return Err(ApiError::new(
                    "policy.quarantine_blocked",
                    "quarantined files cannot be uploaded through a local path",
                ));
            }
        }
    }
    Ok(path)
}

/// Compute a bounded source digest before a human is asked to approve its exact bytes.
pub(super) async fn local_digest(path: &Path, max_bytes: u64) -> ApiResult<(u64, Hex256)> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| invalid("local source is unavailable"))?;
    let size = file.metadata().await.map_err(|_| fail())?.len();
    if size > max_bytes {
        return Err(invalid("local source exceeds the approved byte ceiling"));
    }
    let mut hash = zeus_verify::hash::DualHasher::new();
    let mut buffer = vec![0; 65536];
    let mut read = 0u64;
    loop {
        let n = file.read(&mut buffer).await.map_err(|_| fail())?;
        if n == 0 {
            break;
        }
        read = read.saturating_add(n as u64);
        if read > size || read > max_bytes {
            return Err(invalid("local source changed while planning"));
        }
        hash.update(&buffer[..n]);
    }
    if read != size {
        return Err(invalid("local source changed while planning"));
    }
    Ok((size, hash.finalize().sha256))
}
