//! Durable media segment receipts and bounded, cancellable assembly.
//!
//! A receipt is durable only after the segment bytes are flushed. Recovery
//! re-hashes the local file and revalidates a strong origin ETag; absent strong
//! validators cause a fresh download. A manifest plan must be reviewed again
//! after process restart because signed resource addresses are never persisted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use reqwest::Method;
use reqwest::header::{
    CONTENT_LENGTH, CONTENT_RANGE, ETAG, HeaderMap, HeaderValue, IF_MATCH, RANGE,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeus_app::failure::{Failure, Result};
use zeus_fs::provenance::{NativeMarker, ProvenanceMarker, ProvenanceMarks};
use zeus_fs::{DataFile, Filesystem, RealFs, SafeRoot};

use super::http::{Network, canceled, cancelled, status};
use super::manifest::{Segment, Track};

/// Maximum size of one segmented-media resource. Direct media uses the reviewed
/// overall byte ceiling instead.
const SEGMENT_BYTES: u64 = 512 * 1024 * 1024;

/// One completed and flushed segment, stored privately without its URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    /// Exact segment bytes on disk.
    pub bytes: u64,
    /// Digest of those bytes, independently rechecked before assembly.
    pub sha256: String,
    /// Strong origin validator, when available; never returned to the webview.
    pub etag: Option<String>,
}

/// Private resumable state. Resource URLs, headers and credentials are excluded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Opaque local task identity.
    pub task_id: String,
    /// Digest of the exact reviewed, expanded resource list.
    pub fingerprint: String,
    /// Untrusted descriptive label.
    pub label: String,
    /// Configured root index selected by the person.
    pub root_index: usize,
    /// Canonical root bound at review, checked again on resume.
    pub root: PathBuf,
    /// Safe, single-component output name.
    pub filename: String,
    /// Lifecycle state.
    pub state: String,
    /// Acquisition or an explicitly reviewed local mux job.
    #[serde(default = "acquisition_kind")]
    pub kind: String,
    /// Local task lineage for a mux/repackage operation.
    #[serde(default)]
    pub source_task_ids: Vec<String>,
    /// Pinned helper identity used for conversion, absent for acquisition.
    #[serde(default)]
    pub helper_sha256: Option<String>,
    /// Reviewed lifetime receive ceiling, including retries.
    pub max_bytes: u64,
    /// Reviewed parallel resource limit.
    pub connections: usize,
    /// Reviewed helper deadline, absent for HTTP acquisition.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Assembly was durably announced before finalization; allows recovery of
    /// an identical output after a crash between rename and checkpoint commit.
    pub assembly_started: bool,
    /// Total resource count.
    pub segment_count: usize,
    /// Durable segment receipts.
    pub receipts: BTreeMap<usize, Receipt>,
    /// Bytes received across attempts, including interrupted work.
    pub network_bytes: u64,
    /// Complete output bytes, after assembly.
    pub output_bytes: Option<u64>,
    /// Digest of the verified complete output.
    pub sha256: Option<String>,
    /// Redacted failure code.
    pub error_code: Option<String>,
    /// Redacted failure text.
    pub error_message: Option<String>,
}

/// Mutable in-memory progress of an acquisition.
pub struct Progress {
    /// Durable state and live lifecycle facts.
    pub checkpoint: Mutex<Checkpoint>,
    /// Current network byte count, including in-flight resources.
    pub received: AtomicU64,
    /// Cooperative stop flag.
    pub cancel: Arc<AtomicBool>,
}

fn acquisition_kind() -> String {
    "acquisition".into()
}

impl Progress {
    /// Start or resume from a verified private checkpoint.
    pub fn new(checkpoint: Checkpoint) -> Self {
        Self {
            received: AtomicU64::new(checkpoint.network_bytes),
            checkpoint: Mutex::new(checkpoint),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Obtain a checkpoint without retaining its lock across I/O.
    pub fn snapshot(&self) -> Result<Checkpoint> {
        let mut checkpoint = self
            .checkpoint
            .lock()
            .map_err(|_| Failure::new("media.state", "The media task state is unavailable."))?
            .clone();
        checkpoint.network_bytes = self.received.load(Ordering::Acquire);
        Ok(checkpoint)
    }
}

fn fs_error(_: zeus_fs::FsError) -> Failure {
    Failure::new(
        "media.filesystem",
        "The media file operation failed. Check the selected folder and free disk space.",
    )
}

fn segment_path(dir: &Path, index: usize) -> PathBuf {
    dir.join(format!("segment-{index:04}.bin"))
}

/// Atomically persist a checkpoint with exclusive, private temporary files.
pub fn save(dir: &Path, progress: &Progress) -> Result<()> {
    let bytes = serde_json::to_vec(&progress.snapshot()?)?;
    let temporary = dir.join(format!("checkpoint-{}.tmp", zeus_app::review::random_id()));
    let file = RealFs.create_new(&temporary).map_err(fs_error)?;
    file.write_all_at(0, &bytes).map_err(fs_error)?;
    file.sync_all().map_err(fs_error)?;
    drop(file);
    std::fs::rename(&temporary, dir.join("checkpoint.json"))?;
    RealFs.sync_dir(dir).map_err(fs_error)?;
    Ok(())
}

/// Re-hash a regular, non-link segment using one open file handle.
pub fn verify_local(path: &Path, receipt: &Receipt) -> Result<bool> {
    let file = match RealFs.open_read(path) {
        Ok(file) => file,
        Err(_) => return Ok(false),
    };
    verify_opened(file.as_ref(), receipt, None, None)
}

/// Read-back verification using the same already-open file object. Cancellation
/// and an optional helper deadline are checked at each bounded read.
pub(crate) fn verify_opened(
    file: &dyn DataFile,
    receipt: &Receipt,
    cancel: Option<&AtomicBool>,
    deadline: Option<std::time::Instant>,
) -> Result<bool> {
    if file.len().map_err(fs_error)? != receipt.bytes {
        return Ok(false);
    }
    let mut digest = Sha256::new();
    let mut offset = 0;
    let mut buffer = vec![0; 64 * 1024];
    while offset < receipt.bytes {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Acquire)) {
            return Err(canceled());
        }
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return Err(Failure::new(
                "media.helper_timeout",
                "Media read-back verification reached the reviewed time limit.",
            ));
        }
        let count = buffer.len().min((receipt.bytes - offset) as usize);
        file.read_exact_at(offset, &mut buffer[..count])
            .map_err(fs_error)?;
        digest.update(&buffer[..count]);
        offset += count as u64;
    }
    Ok(hex::encode(digest.finalize()) == receipt.sha256)
}

async fn reusable(
    network: &Network,
    segment: &Segment,
    receipt: &Receipt,
    path: PathBuf,
    cancel: Arc<AtomicBool>,
) -> Result<bool> {
    let Some(etag) = &receipt.etag else {
        return Ok(false);
    };
    let local = receipt.clone();
    if !tokio::task::spawn_blocking(move || verify_local(&path, &local))
        .await
        .map_err(|_| Failure::new("media.filesystem", "The media verification task stopped."))??
    {
        return Ok(false);
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        IF_MATCH,
        HeaderValue::from_str(etag)
            .map_err(|_| super::invalid("A stored media validator is invalid."))?,
    );
    let response = network
        .request(segment.url.clone(), Method::HEAD, headers, cancel)
        .await?;
    if response.status() == reqwest::StatusCode::PRECONDITION_FAILED {
        return Err(Failure::new(
            "media.source_changed",
            "A retained media segment changed at its origin. Start a new acquisition to avoid mixing versions.",
        ));
    }
    if matches!(
        response.status(),
        reqwest::StatusCode::METHOD_NOT_ALLOWED | reqwest::StatusCode::NOT_IMPLEMENTED
    ) {
        return Ok(false);
    }
    status(response.status())?;
    super::http::identity(response.headers())?;
    Ok(response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        == Some(etag))
}

fn reserve(progress: &Progress, bytes: u64, ceiling: u64) -> Result<()> {
    let mut old = progress.received.load(Ordering::Acquire);
    loop {
        let next = old.checked_add(bytes).filter(|total| *total <= ceiling)
            .ok_or_else(|| Failure::new("media.byte_limit", "The reviewed media byte ceiling was reached. Verified segments remain available for a reviewed resume."))?;
        match progress.received.compare_exchange_weak(
            old,
            next,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return Ok(()),
            Err(actual) => old = actual,
        }
    }
}

async fn fetch(
    network: Network,
    segment: Segment,
    index: usize,
    dir: PathBuf,
    progress: Arc<Progress>,
    ceiling: u64,
    resource_ceiling: u64,
) -> Result<Receipt> {
    let mut headers = HeaderMap::new();
    if let Some(range) = &segment.range {
        headers.insert(
            RANGE,
            HeaderValue::from_str(&format!("bytes={}-{}", range.start, range.end - 1))
                .map_err(|_| super::invalid("A media range cannot be represented."))?,
        );
    }
    let mut response = network
        .request(segment.url, Method::GET, headers, progress.cancel.clone())
        .await?;
    status(response.status())?;
    super::http::identity(response.headers())?;
    let media_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(
        media_type.as_str(),
        "text/html"
            | "application/json"
            | "application/dash+xml"
            | "application/vnd.apple.mpegurl"
            | "application/x-mpegurl"
    ) {
        return Err(Failure::new(
            "media.unexpected_content",
            "A media resource returned a page, error document or nested manifest instead of media bytes.",
        ));
    }
    let expected = match segment.range {
        Some(range) => {
            if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
                return Err(Failure::new(
                    "media.range_ignored",
                    "The media host ignored the exact byte range. Zeus did not append the full response.",
                ));
            }
            let value = response
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("");
            let expected = format!("bytes {}-{}/", range.start, range.end - 1);
            let total = value
                .strip_prefix(&expected)
                .and_then(|s| s.parse::<u64>().ok());
            if !total.is_some_and(|n| n >= range.end) {
                return Err(Failure::new(
                    "media.range_mismatch",
                    "The media host returned a different byte interval.",
                ));
            }
            Some(range.end - range.start)
        }
        None => {
            if response.status() != reqwest::StatusCode::OK {
                return Err(Failure::new(
                    "media.unexpected_partial",
                    "The media host returned an unrequested partial response.",
                ));
            }
            response
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|h| h.to_str().ok())
                .and_then(|n| n.parse().ok())
        }
    };
    if expected.is_some_and(|n| n > resource_ceiling) {
        return Err(Failure::new(
            "media.resource_limit",
            "A media resource exceeds the bounded segment size or reviewed byte ceiling.",
        ));
    }
    let etag = response
        .headers()
        .get(ETAG)
        .and_then(|h| h.to_str().ok())
        .filter(|h| h.starts_with('"') && h.ends_with('"') && h.len() <= 512)
        .map(str::to_owned);
    let temporary = dir.join(format!(
        "segment-{index:04}-{}.part",
        zeus_app::review::random_id()
    ));
    let file: Arc<dyn DataFile> = Arc::from(RealFs.create_new(&temporary).map_err(fs_error)?);
    let result = async {
        let mut offset = 0u64;
        let mut digest = Sha256::new();
        loop {
            let chunk = tokio::select! {
                biased;
                _ = cancelled(progress.cancel.clone()) => return Err(canceled()),
                result = response.chunk() => result.map_err(|_| Failure::new("media.network", "A media segment ended unexpectedly."))?,
            };
            let Some(chunk) = chunk else { break };
            if chunk.len() as u64 > resource_ceiling.saturating_sub(offset)
                || expected.is_some_and(|n| chunk.len() as u64 > n.saturating_sub(offset)) {
                return Err(Failure::new("media.resource_overrun", "A media response exceeded its declared range or size ceiling."));
            }
            reserve(&progress, chunk.len() as u64, ceiling)?;
            digest.update(&chunk);
            let count = chunk.len() as u64;
            let (writer, data) = (file.clone(), chunk);
            tokio::task::spawn_blocking(move || writer.write_all_at(offset, &data)).await
                .map_err(|_| Failure::new("media.filesystem", "The media writer stopped."))?.map_err(fs_error)?;
            offset += count;
        }
        if expected.is_some_and(|n| n != offset) || offset == 0 {
            return Err(Failure::new("media.premature_eof", "A media resource ended before its declared size or was empty."));
        }
        let writer = file.clone();
        tokio::task::spawn_blocking(move || writer.sync_all()).await
            .map_err(|_| Failure::new("media.filesystem", "The media flush stopped."))?.map_err(fs_error)?;
        Ok(Receipt { bytes: offset, sha256: hex::encode(digest.finalize()), etag })
    }.await;
    drop(file);
    match result {
        Ok(receipt) => {
            let target = segment_path(&dir, index);
            RealFs.remove_file(&target).map_err(fs_error)?;
            RealFs
                .rename_noreplace(&temporary, &target)
                .map_err(fs_error)?;
            RealFs.sync_dir(&dir).map_err(fs_error)?;
            Ok(receipt)
        }
        Err(error) => {
            let _ = RealFs.remove_file(&temporary);
            Err(error)
        }
    }
}

fn assemble(dir: &Path, progress: &Progress, recover_existing: bool) -> Result<(u64, String)> {
    let checkpoint = progress.snapshot()?;
    let root = SafeRoot::open(&checkpoint.root).map_err(fs_error)?;
    let target = root
        .join_checked(Path::new(&checkpoint.filename))
        .map_err(fs_error)?;
    root.check_existing(&target).map_err(fs_error)?;
    let existing = RealFs.metadata(&target).map_err(fs_error)?;
    let temporary = root
        .join_checked(Path::new(&format!(
            ".zeus-media-{}.part",
            checkpoint.task_id
        )))
        .map_err(fs_error)?;
    // An earlier interrupted assembly is never trusted. Only segment receipts
    // establish reusable data; remove the task's own exclusive temporary name.
    if RealFs.metadata(&temporary).map_err(fs_error)?.is_some() {
        RealFs.remove_file(&temporary).map_err(fs_error)?;
    }
    let output = RealFs.create_new(&temporary).map_err(fs_error)?;
    let result = (|| {
        let mut offset = 0u64;
        let mut digest = Sha256::new();
        let mut buffer = vec![0; 64 * 1024];
        for index in 0..checkpoint.segment_count {
            let receipt = checkpoint.receipts.get(&index).ok_or_else(|| {
                Failure::new(
                    "media.receipt_missing",
                    "A media segment has no durable receipt.",
                )
            })?;
            let input = RealFs
                .open_read(&segment_path(dir, index))
                .map_err(fs_error)?;
            if input.len().map_err(fs_error)? != receipt.bytes {
                return Err(Failure::new(
                    "media.segment_changed",
                    "A retained media segment changed on disk.",
                ));
            }
            let mut segment_digest = Sha256::new();
            let mut segment_offset = 0;
            while segment_offset < receipt.bytes {
                if progress.cancel.load(Ordering::Acquire) {
                    return Err(canceled());
                }
                let count = buffer.len().min((receipt.bytes - segment_offset) as usize);
                input
                    .read_exact_at(segment_offset, &mut buffer[..count])
                    .map_err(fs_error)?;
                segment_digest.update(&buffer[..count]);
                digest.update(&buffer[..count]);
                output
                    .write_all_at(offset, &buffer[..count])
                    .map_err(fs_error)?;
                segment_offset += count as u64;
                offset += count as u64;
            }
            if hex::encode(segment_digest.finalize()) != receipt.sha256 {
                return Err(Failure::new(
                    "media.segment_changed",
                    "A retained media segment failed SHA-256 verification.",
                ));
            }
        }
        output.sync_all().map_err(fs_error)?;
        let sha256 = hex::encode(digest.finalize());
        let expected = Receipt {
            bytes: offset,
            sha256: sha256.clone(),
            etag: None,
        };
        if !verify_opened(output.as_ref(), &expected, Some(&progress.cancel), None)? {
            return Err(Failure::new(
                "media.output_changed",
                "The assembled output failed read-back SHA-256 verification.",
            ));
        }
        if existing.is_some() {
            let receipt = Receipt {
                bytes: offset,
                sha256: sha256.clone(),
                etag: None,
            };
            if recover_existing && verify_local(&target, &receipt)? {
                RealFs.remove_file(&temporary).map_err(fs_error)?;
                return Ok((offset, sha256));
            }
            return Err(Failure::new(
                "media.destination_exists",
                "An existing output differs from this task. Media assembly never overwrites it.",
            ));
        }
        let marks = ProvenanceMarks {
            origin_url: None,
            referrer_url: None,
            retrieved_at_unix_s: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |n| n.as_secs() as i64),
        };
        NativeMarker
            .apply(&temporary, &marks, zeus_types::OsMetadataMode::MarkOnly)
            .map_err(fs_error)?;
        root.check_existing(&target).map_err(fs_error)?;
        RealFs
            .rename_noreplace(&temporary, &target)
            .map_err(fs_error)?;
        RealFs.sync_dir(root.path()).map_err(fs_error)?;
        Ok((offset, sha256))
    })();
    drop(output);
    if result.is_err() {
        let _ = RealFs.remove_file(&temporary);
    }
    result
}

/// Execute one reviewed finite track. Parallel requests are capped at eight;
/// successful workers are drained before any error is returned or resumed.
pub async fn execute(
    network: Network,
    track: Track,
    dir: PathBuf,
    progress: Arc<Progress>,
    ceiling: u64,
    concurrency: usize,
) -> Result<()> {
    let mut pending = Vec::new();
    let snapshot = progress.snapshot()?;
    for (index, segment) in track.segments.iter().enumerate() {
        if progress.cancel.load(Ordering::Acquire) {
            return Err(canceled());
        }
        network.check(&segment.url)?;
        let kept = match snapshot.receipts.get(&index) {
            Some(receipt) => {
                reusable(
                    &network,
                    segment,
                    receipt,
                    segment_path(&dir, index),
                    progress.cancel.clone(),
                )
                .await?
            }
            None => false,
        };
        if !kept {
            pending.push(index);
        }
    }
    let mut pending = pending.into_iter();
    let mut workers = tokio::task::JoinSet::new();
    let mut failure = None;
    loop {
        while failure.is_none() && workers.len() < concurrency.clamp(1, 8) {
            let Some(index) = pending.next() else { break };
            let (network, segment, dir, progress) = (
                network.clone(),
                track.segments[index].clone(),
                dir.clone(),
                progress.clone(),
            );
            let resource_ceiling = if track.segments.len() == 1 {
                ceiling
            } else {
                SEGMENT_BYTES.min(ceiling)
            };
            workers.spawn(async move {
                (
                    index,
                    fetch(
                        network,
                        segment,
                        index,
                        dir,
                        progress,
                        ceiling,
                        resource_ceiling,
                    )
                    .await,
                )
            });
        }
        let Some(outcome) = workers.join_next().await else {
            break;
        };
        match outcome {
            Ok((index, Ok(receipt))) => {
                let stored = match progress.checkpoint.lock() {
                    Ok(mut checkpoint) => {
                        checkpoint.receipts.insert(index, receipt);
                        Ok(())
                    }
                    Err(_) => Err(Failure::new(
                        "media.state",
                        "The media task state is unavailable.",
                    )),
                }
                .and_then(|()| save(&dir, &progress));
                if let Err(error) = stored
                    && failure.is_none()
                {
                    failure = Some(error);
                    progress.cancel.store(true, Ordering::Release);
                }
            }
            Ok((_, Err(error))) if failure.is_none() => {
                failure = Some(error);
                progress.cancel.store(true, Ordering::Release);
            }
            Err(_) if failure.is_none() => {
                failure = Some(Failure::new(
                    "media.worker",
                    "A media worker stopped unexpectedly.",
                ));
                progress.cancel.store(true, Ordering::Release);
            }
            _ => {}
        }
    }
    if let Some(error) = failure {
        return Err(error);
    }
    let recover_existing = progress.snapshot()?.assembly_started;
    {
        let mut checkpoint = progress
            .checkpoint
            .lock()
            .map_err(|_| Failure::new("media.state", "The media task state is unavailable."))?;
        checkpoint.state = "assembling".into();
        checkpoint.assembly_started = true;
    }
    save(&dir, &progress)?;
    let (assembly_dir, assembly_progress) = (dir.clone(), progress.clone());
    let (bytes, sha256) = tokio::task::spawn_blocking(move || {
        assemble(&assembly_dir, &assembly_progress, recover_existing)
    })
    .await
    .map_err(|_| Failure::new("media.assembly", "The media assembly task stopped."))??;
    {
        let mut state = progress
            .checkpoint
            .lock()
            .map_err(|_| Failure::new("media.state", "The media task state is unavailable."))?;
        state.state = "complete".into();
        state.output_bytes = Some(bytes);
        state.sha256 = Some(sha256);
        state.error_code = None;
        state.error_message = None;
    }
    save(&dir, &progress)?;
    Ok(())
}

/// Rebuild or recognize an output from a complete durable set of local segment
/// receipts. This path performs no network requests and no codec conversion.
pub async fn recover(dir: PathBuf, progress: Arc<Progress>) -> Result<()> {
    let checkpoint = progress.snapshot()?;
    if checkpoint.kind != "acquisition" || checkpoint.receipts.len() != checkpoint.segment_count {
        return Err(Failure::new(
            "media.recovery_incomplete",
            "Only an acquisition with every verified segment can be recovered without its source.",
        ));
    }
    {
        let mut state = progress
            .checkpoint
            .lock()
            .map_err(|_| Failure::new("media.state", "The media state is unavailable."))?;
        state.state = "assembling".into();
        state.assembly_started = true;
    }
    save(&dir, &progress)?;
    let (assembly_dir, assembly_progress) = (dir.clone(), progress.clone());
    let (bytes, sha256) = tokio::task::spawn_blocking(move || {
        assemble(
            &assembly_dir,
            &assembly_progress,
            checkpoint.assembly_started,
        )
    })
    .await
    .map_err(|_| Failure::new("media.assembly", "The media recovery task stopped."))??;
    {
        let mut state = progress
            .checkpoint
            .lock()
            .map_err(|_| Failure::new("media.state", "The media state is unavailable."))?;
        state.state = "complete".into();
        state.output_bytes = Some(bytes);
        state.sha256 = Some(sha256);
        state.error_code = None;
        state.error_message = None;
    }
    save(&dir, &progress)?;
    Ok(())
}
