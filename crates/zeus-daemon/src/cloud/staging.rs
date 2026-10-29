//! Durable, strongly identified partial URL staging. The final full-file hash still gates upload.
use super::*;
use reqwest::{
    StatusCode,
    header::{CONTENT_LENGTH, CONTENT_RANGE, ETAG, HeaderMap},
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use zeus_types::HttpUrl;
const BLOCK: u64 = 4 << 20;
// A bounded private record covers the first 32 GiB; larger tails can be re-read after interruption.
const MAX_BLOCKS: usize = 8192;
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct SourceCheckpoint {
    version: u8,
    source_hash: Hex256,
    resolved_hash: Hex256,
    size: u64,
    etag: String,
    blocks: Vec<Hex256>,
}
fn changed(message: &str) -> ApiError {
    ApiError::new("cloud.source_changed", message)
}
fn strong_etag(headers: &HeaderMap) -> Option<String> {
    let tag = headers.get(ETAG)?.to_str().ok()?;
    (tag.len() >= 2
        && tag.len() <= 512
        && tag.starts_with('"')
        && tag.ends_with('"')
        && tag.as_bytes()[1..tag.len() - 1]
            .iter()
            .all(|b| *b == 0x21 || (0x23..=0x7e).contains(b)))
    .then(|| tag.to_owned())
}
pub(super) fn validate_response(
    status: StatusCode,
    headers: &HeaderMap,
    size: u64,
    resume: Option<(&str, u64)>,
) -> ApiResult<()> {
    if !matches!(status, StatusCode::OK | StatusCode::PARTIAL_CONTENT) {
        return Err(ApiError::new(
            "cloud.source_unavailable",
            "source returned an unsupported HTTP status; retained bytes preserved",
        ));
    }
    let (expected_status, length) = if let Some((etag, offset)) = resume {
        if offset >= size {
            return Err(changed("invalid source resume offset"));
        }
        if status == StatusCode::OK {
            // The caller discards the old prefix before accepting a full replacement response.
            (StatusCode::OK, size)
        } else {
            if strong_etag(headers).as_deref() != Some(etag) {
                return Err(changed("source validator changed during range recovery"));
            }
            let expected = format!("bytes {offset}-{}/{size}", size - 1);
            if headers.get(CONTENT_RANGE).and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
                return Err(changed(
                    "source resume Content-Range differs from the requested bytes",
                ));
            }
            (StatusCode::PARTIAL_CONTENT, size - offset)
        }
    } else {
        (StatusCode::OK, size)
    };
    if status != expected_status {
        return Err(ApiError::new(
            "cloud.source_unavailable",
            "source returned an unsupported HTTP status",
        ));
    }
    if headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        != Some(length)
    {
        return Err(changed(
            "source content length differs from the exact approved size",
        ));
    }
    Ok(())
}
fn url_hash(url: &HttpUrl) -> Hex256 {
    zeus_verify::hash_bytes(url.expose().as_bytes()).sha256
}
async fn verify_prefix(file: &mut tokio::fs::File, cp: &SourceCheckpoint) -> ApiResult<u64> {
    if cp.version != 1 || cp.blocks.len() > MAX_BLOCKS {
        return Err(changed("invalid retained staging checkpoint"));
    }
    let durable = (cp.blocks.len() as u64)
        .checked_mul(BLOCK)
        .ok_or_else(fail)?;
    if durable >= cp.size || file.metadata().await.map_err(|_| fail())?.len() < durable {
        return Err(changed(
            "retained staging prefix is missing or has an invalid length",
        ));
    }
    file.seek(std::io::SeekFrom::Start(0))
        .await
        .map_err(|_| fail())?;
    let mut block = vec![0; BLOCK as usize];
    for expected in &cp.blocks {
        file.read_exact(&mut block)
            .await
            .map_err(|_| changed("retained staging prefix is unreadable"))?;
        if zeus_verify::hash_bytes(&block).sha256 != *expected {
            return Err(changed(
                "retained staging prefix failed integrity verification",
            ));
        }
    }
    // Writes acknowledged only by the file, not by the checkpoint, cannot be reused.
    file.set_len(durable).await.map_err(|_| fail())?;
    file.seek(std::io::SeekFrom::Start(durable))
        .await
        .map_err(|_| fail())?;
    Ok(durable)
}
#[async_trait]
pub(super) trait SourceNetwork: Send + Sync {
    async fn response(
        &self,
        url: &HttpUrl,
        size: u64,
        resume: Option<(String, u64)>,
        cancel: &CancellationToken,
    ) -> ApiResult<(HttpUrl, reqwest::Response)>;
}
struct PublicNetwork;
#[async_trait]
impl SourceNetwork for PublicNetwork {
    async fn response(
        &self,
        url: &HttpUrl,
        size: u64,
        resume: Option<(String, u64)>,
        cancel: &CancellationToken,
    ) -> ApiResult<(HttpUrl, reqwest::Response)> {
        super::source::response_from(
            url,
            size,
            false,
            resume.as_ref().map(|(e, n)| (e.as_str(), *n)),
            cancel,
        )
        .await
    }
}
pub(super) async fn download(
    service: &Arc<CloudService>,
    row: &Delivery,
    url: &HttpUrl,
    path: &Path,
    cancel: &CancellationToken,
) -> ApiResult<tokio::fs::File> {
    download_using(service, row, url, path, cancel, &PublicNetwork).await
}
pub(super) async fn download_using(
    service: &Arc<CloudService>,
    row: &Delivery,
    url: &HttpUrl,
    path: &Path,
    cancel: &CancellationToken,
    network: &dyn SourceNetwork,
) -> ApiResult<tokio::fs::File> {
    service.guard(&row.view.delivery_id)?;
    let size = row.view.plan.size_bytes;
    let source_hash = url_hash(url);
    let mut cp = row
        .source_checkpoint
        .clone()
        .filter(|c| !c.blocks.is_empty());
    let mut file = if let Some(checkpoint) = &cp {
        if checkpoint.source_hash != source_hash || checkpoint.size != size {
            return Err(changed(
                "retained staging checkpoint belongs to a different source",
            ));
        }
        let meta = tokio::fs::symlink_metadata(path)
            .await
            .map_err(|_| changed("retained staging file is missing"))?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(changed("retained staging is not a regular file"));
        }
        tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .await
            .map_err(|_| fail())?
    } else {
        if path.exists() {
            tokio::fs::remove_file(path).await.map_err(|_| fail())?;
        }
        tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .await
            .map_err(|_| fail())?
    };
    let mut offset = if let Some(checkpoint) = &cp {
        verify_prefix(&mut file, checkpoint).await?
    } else {
        0
    };
    service
        .store
        .mutate_delivery(&row.view.delivery_id, |stored| {
            stored.view.source_bytes_done = offset;
            stored.view.source_verified = false;
            Ok(())
        })?;
    let resume = cp
        .as_ref()
        .filter(|_| offset > 0)
        .map(|c| (c.etag.clone(), offset));
    let (resolved, mut response) = network.response(url, size, resume, cancel).await?;
    validate_response(
        response.status(),
        response.headers(),
        size,
        cp.as_ref()
            .filter(|_| offset > 0)
            .map(|c| (c.etag.as_str(), offset)),
    )?;
    if response
        .headers()
        .get(reqwest::header::CONTENT_ENCODING)
        .is_some_and(|v| v.as_bytes() != b"identity")
    {
        return Err(changed("encoded staging responses are not supported"));
    }
    if response.status() == StatusCode::PARTIAL_CONTENT {
        if cp
            .as_ref()
            .is_none_or(|c| c.resolved_hash != url_hash(&resolved))
        {
            return Err(changed("source redirect target changed during recovery"));
        }
    } else {
        offset = 0;
        file.set_len(0).await.map_err(|_| fail())?;
        file.seek(std::io::SeekFrom::Start(0))
            .await
            .map_err(|_| fail())?;
        cp = strong_etag(response.headers()).map(|etag| SourceCheckpoint {
            version: 1,
            source_hash,
            resolved_hash: url_hash(&resolved),
            size,
            etag,
            blocks: vec![],
        });
        service
            .store
            .mutate_delivery(&row.view.delivery_id, |stored| {
                stored.source_checkpoint = cp.clone();
                stored.view.source_bytes_done = 0;
                stored.view.source_verified = false;
                Ok(())
            })?;
    }
    let mut block = Vec::with_capacity(BLOCK as usize);
    loop {
        service.guard(&row.view.delivery_id)?;
        let chunk = tokio::select! {
            _ = cancel.cancelled() => return Err(ApiError::new("cloud.source_unavailable", "source staging paused or canceled")),
            chunk = response.chunk() => chunk.map_err(|_| ApiError::new("cloud.source_unavailable", "source download interrupted; durable prefix retained"))?,
        };
        let Some(mut chunk) = chunk else {
            break;
        };
        if offset.saturating_add(chunk.len() as u64) > size {
            return Err(changed("source exceeds the approved length"));
        }
        while !chunk.is_empty() {
            service.guard(&row.view.delivery_id)?;
            if cancel.is_cancelled() {
                return Err(ApiError::new(
                    "cloud.source_unavailable",
                    "source staging paused or canceled",
                ));
            }
            let n = chunk.len().min(64 * 1024).min(BLOCK as usize - block.len());
            let part = chunk.split_to(n);
            file.write_all(&part).await.map_err(|_| fail())?;
            offset += n as u64;
            block.extend_from_slice(&part);
            if block.len() as u64 == BLOCK {
                file.sync_all().await.map_err(|_| fail())?;
                if let Some(checkpoint) = &mut cp
                    && checkpoint.blocks.len() < MAX_BLOCKS
                    && offset < size
                {
                    checkpoint
                        .blocks
                        .push(zeus_verify::hash_bytes(&block).sha256);
                }
                service
                    .store
                    .mutate_delivery(&row.view.delivery_id, |stored| {
                        stored.source_checkpoint = cp.clone();
                        stored.view.source_bytes_done = offset;
                        stored.view.source_verified = false;
                        Ok(())
                    })?;
                block.clear();
            }
        }
    }
    if offset != size {
        return Err(ApiError::new(
            "cloud.source_unavailable",
            "source ended before the approved length; durable prefix retained",
        ));
    }
    file.sync_all().await.map_err(|_| fail())?;
    service
        .store
        .mutate_delivery(&row.view.delivery_id, |stored| {
            stored.view.source_bytes_done = size;
            Ok(())
        })?;
    file.seek(std::io::SeekFrom::Start(0))
        .await
        .map_err(|_| fail())?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn headers(etag: &str, range: Option<&str>, length: u64) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(ETAG, etag.parse().unwrap());
        h.insert(CONTENT_LENGTH, length.to_string().parse().unwrap());
        if let Some(r) = range {
            h.insert(CONTENT_RANGE, r.parse().unwrap());
        }
        h
    }
    #[test]
    fn only_exact_strongly_bound_ranges_can_reuse_a_prefix() {
        let h = headers("\"v1\"", Some("bytes 4-9/10"), 6);
        assert!(
            validate_response(StatusCode::PARTIAL_CONTENT, &h, 10, Some(("\"v1\"", 4))).is_ok()
        );
        for h in [
            headers("W/\"v1\"", Some("bytes 4-9/10"), 6),
            headers("\"v2\"", Some("bytes 4-9/10"), 6),
            headers("\"v1\"", Some("bytes 3-8/10"), 6),
            headers("\"v1\"", Some("bytes 4-9/11"), 6),
            headers("\"v1\"", Some("bytes 4-9/10"), 7),
        ] {
            assert!(
                validate_response(StatusCode::PARTIAL_CONTENT, &h, 10, Some(("\"v1\"", 4)))
                    .is_err()
            );
        }
        assert!(
            validate_response(
                StatusCode::OK,
                &headers("\"v2\"", None, 10),
                10,
                Some(("\"v1\"", 4))
            )
            .is_ok()
        );
    }
    #[tokio::test]
    async fn recovery_hashes_acknowledged_prefix_and_discards_unacknowledged_tail() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("part");
        let bytes = vec![19; BLOCK as usize];
        tokio::fs::write(&path, [bytes.clone(), vec![99; 1234]].concat())
            .await
            .unwrap();
        let url = HttpUrl::parse("https://example.org/file").unwrap();
        let cp = SourceCheckpoint {
            version: 1,
            source_hash: url_hash(&url),
            resolved_hash: url_hash(&url),
            size: BLOCK * 3,
            etag: "\"v1\"".into(),
            blocks: vec![zeus_verify::hash_bytes(&bytes).sha256],
        };
        let mut f = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .await
            .unwrap();
        assert_eq!(verify_prefix(&mut f, &cp).await.unwrap(), BLOCK);
        assert_eq!(f.metadata().await.unwrap().len(), BLOCK);
        f.seek(std::io::SeekFrom::Start(12)).await.unwrap();
        f.write_all(&[98]).await.unwrap();
        f.sync_all().await.unwrap();
        assert!(verify_prefix(&mut f, &cp).await.is_err());
    }
}
