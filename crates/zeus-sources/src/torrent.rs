//! Bounded v1 metainfo and explicit-IP peer acquisition. No listener, DHT, LSD,
//! trackers, UPnP, ambient credentials, automatic magnets or payload uploading.
use std::{
    collections::BTreeSet,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use librqbit::storage::{BoxStorageFactory, StorageFactory, StorageFactoryExt, TorrentStorage};
use librqbit::{AddTorrent, AddTorrentOptions, Session, SessionOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use zeus_fs::{DataFile, Filesystem, RealFs};

use crate::{SourceAccountConfig, SourceAuthority, SourceError, SourceResult, SourceTarget};

const MAX_META: usize = 2 * 1024 * 1024;
const MAX_FILES: usize = 4096;
const MAX_PIECES: usize = 65536;

/// One reviewed file. Its untrusted components are validated but never used as SDK paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentFile {
    /// Exact metainfo file index.
    pub index: usize,
    /// Safe relative name retained for explicit destination review.
    pub untrusted_path: Vec<String>,
    /// Exact file length.
    pub bytes: u64,
    /// Position in the concatenated v1 piece-address space.
    pub offset: u64,
}
/// Immutable metadata review. No tracker address or passkey is projected.
#[derive(Clone)]
pub struct TorrentReview {
    /// SHA-256 of original metainfo, independent of its v1 info hash.
    pub metainfo_sha256: String,
    /// BitTorrent v1 info hash; not a publisher authenticity claim.
    pub info_hash: String,
    /// Exact bounded file map.
    pub files: Vec<TorrentFile>,
    /// Piece length; at most16MiB.
    pub piece_length: u64,
    /// Total torrent address-space bytes, including files not selected for publication.
    pub total_bytes: u64,
    piece_hashes: Vec<[u8; 20]>,
    sdk_metainfo: Vec<u8>,
}
impl std::fmt::Debug for TorrentReview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TorrentReview")
            .field("info_hash", &self.info_hash)
            .field("files", &self.files.len())
            .finish_non_exhaustive()
    }
}

fn string<'a>(bytes: &'a [u8], offset: &mut usize) -> SourceResult<&'a [u8]> {
    let start = *offset;
    while bytes.get(*offset).is_some_and(u8::is_ascii_digit) {
        *offset += 1;
        if *offset - start > 8 {
            return Err(SourceError::Invalid);
        }
    }
    if bytes.get(*offset) != Some(&b':') || *offset == start {
        return Err(SourceError::Invalid);
    }
    let count = std::str::from_utf8(&bytes[start..*offset])
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or(SourceError::Invalid)?;
    if *offset - start > 1 && bytes[start] == b'0' {
        return Err(SourceError::Invalid);
    }
    *offset += 1;
    let end = offset
        .checked_add(count)
        .filter(|end| *end <= bytes.len())
        .ok_or(SourceError::Invalid)?;
    let result = &bytes[*offset..end];
    *offset = end;
    Ok(result)
}
fn preflight(
    bytes: &[u8],
    offset: &mut usize,
    depth: usize,
    nodes: &mut usize,
) -> SourceResult<()> {
    *nodes += 1;
    if depth > 32 || *nodes > 50000 {
        return Err(SourceError::Invalid);
    }
    match bytes.get(*offset).copied().ok_or(SourceError::Invalid)? {
        b'0'..=b'9' => {
            string(bytes, offset)?;
        }
        b'i' => {
            *offset += 1;
            let begin = *offset;
            while bytes.get(*offset) != Some(&b'e') {
                let byte = *bytes.get(*offset).ok_or(SourceError::Invalid)?;
                if !byte.is_ascii_digit() && !(byte == b'-' && *offset == begin) {
                    return Err(SourceError::Invalid);
                }
                *offset += 1;
                if *offset - begin > 21 {
                    return Err(SourceError::Invalid);
                }
            }
            if *offset == begin {
                return Err(SourceError::Invalid);
            }
            *offset += 1;
        }
        b'l' => {
            *offset += 1;
            while bytes.get(*offset) != Some(&b'e') {
                preflight(bytes, offset, depth + 1, nodes)?;
            }
            *offset += 1;
        }
        b'd' => {
            *offset += 1;
            let mut previous: Option<&[u8]> = None;
            while bytes.get(*offset) != Some(&b'e') {
                let key = string(bytes, offset)?;
                if previous.is_some_and(|previous| previous >= key) {
                    return Err(SourceError::Invalid);
                }
                previous = Some(key);
                preflight(bytes, offset, depth + 1, nodes)?;
            }
            *offset += 1;
        }
        _ => return Err(SourceError::Invalid),
    }
    Ok(())
}

/// Parse finite v1 metainfo without network. A bounded preflight precedes SDK parsing.
pub fn inspect(bytes: &[u8]) -> SourceResult<TorrentReview> {
    if bytes.is_empty() || bytes.len() > MAX_META {
        return Err(SourceError::Invalid);
    }
    let mut offset = 0;
    preflight(bytes, &mut offset, 0, &mut 0)?;
    if offset != bytes.len() {
        return Err(SourceError::Invalid);
    }
    let parsed = librqbit::torrent_from_bytes(bytes).map_err(|_| SourceError::Invalid)?;
    let raw_info = parsed.info.raw_bytes.as_ref();
    let mut sdk_metainfo = b"d4:info".to_vec();
    sdk_metainfo.extend_from_slice(raw_info);
    sdk_metainfo.push(b'e');
    let info_hash = parsed.info_hash.as_string();
    let info = parsed
        .info
        .data
        .validate()
        .map_err(|_| SourceError::Invalid)?;
    let piece_length = u64::from(info.info().piece_length);
    if !(16384..=16 * 1024 * 1024).contains(&piece_length) || !piece_length.is_power_of_two() {
        return Err(SourceError::Unsupported);
    }
    let piece_bytes: &[u8] = info.info().pieces.as_ref();
    let (piece_hashes, trailing) = piece_bytes.as_chunks::<20>();
    if piece_hashes.is_empty() || !trailing.is_empty() || piece_hashes.len() > MAX_PIECES {
        return Err(SourceError::Invalid);
    }
    let piece_hashes = piece_hashes.to_vec();
    let mut files = Vec::new();
    let mut total_bytes = 0u64;
    for (index, details) in info.iter_file_details().enumerate() {
        if index >= MAX_FILES
            || details.attrs().symlink
            || details.symlink_path.is_some()
            || details.attrs().padding
        {
            return Err(SourceError::Unsupported);
        }
        let mut parts = Vec::new();
        for bytes in details.filename.iter_components_bytes() {
            let part = std::str::from_utf8(bytes).map_err(|_| SourceError::Invalid)?;
            if part.is_empty()
                || part.len() > 200
                || part == "."
                || part == ".."
                || !zeus_fs::sanitize_filename(part, &Default::default())
                    .is_ok_and(|safe| safe.name == part)
            {
                return Err(SourceError::Invalid);
            }
            parts.push(part.to_owned());
            if parts.len() > 32 {
                return Err(SourceError::Invalid);
            }
        }
        if parts.is_empty() {
            return Err(SourceError::Invalid);
        }
        files.push(TorrentFile {
            index,
            untrusted_path: parts,
            bytes: details.len,
            offset: total_bytes,
        });
        total_bytes = total_bytes
            .checked_add(details.len)
            .filter(|total| *total <= 1 << 40)
            .ok_or(SourceError::Invalid)?;
    }
    if files.is_empty()
        || total_bytes == 0
        || total_bytes.div_ceil(piece_length) != piece_hashes.len() as u64
    {
        return Err(SourceError::Invalid);
    }
    Ok(TorrentReview {
        metainfo_sha256: hex::encode(Sha256::digest(bytes)),
        info_hash,
        files,
        piece_length,
        total_bytes,
        piece_hashes,
        sdk_metainfo,
    })
}

/// Validate exact IP-level peer scope without DNS, trackers or private discovery.
pub fn peer_scope(
    config: &SourceAccountConfig,
    target: &SourceTarget,
    review: &TorrentReview,
) -> SourceResult<Vec<SocketAddr>> {
    let SourceAccountConfig::Torrent {
        max_peers,
        max_download_bytes_per_second,
        allowed_peer_ips,
    } = config
    else {
        return Err(SourceError::Invalid);
    };
    let SourceTarget::Torrent {
        metainfo_sha256,
        file_indices,
        initial_peers,
    } = target
    else {
        return Err(SourceError::Invalid);
    };
    if !(1..=32).contains(max_peers)
        || *max_download_bytes_per_second == 0
        || *max_download_bytes_per_second > u32::MAX as u64
        || allowed_peer_ips.is_empty()
        || allowed_peer_ips.len() > 256
        || initial_peers.is_empty()
        || initial_peers.len() > 256
        || *metainfo_sha256 != review.metainfo_sha256
        || file_indices.is_empty()
        || file_indices.len() > MAX_FILES
        || file_indices
            .iter()
            .any(|index| *index >= review.files.len())
        || file_indices.iter().copied().collect::<BTreeSet<_>>().len() != file_indices.len()
    {
        return Err(SourceError::Invalid);
    }
    let ips = allowed_peer_ips
        .iter()
        .map(|value| value.parse::<IpAddr>().map_err(|_| SourceError::Invalid))
        .collect::<SourceResult<BTreeSet<_>>>()?;
    if ips
        .iter()
        .any(|ip| ip.is_unspecified() || ip.is_multicast())
    {
        return Err(SourceError::Invalid);
    }
    initial_peers
        .iter()
        .map(|value| {
            let peer = value
                .parse::<SocketAddr>()
                .map_err(|_| SourceError::Invalid)?;
            if peer.port() == 0 || !ips.contains(&peer.ip()) {
                return Err(SourceError::Denied);
            }
            Ok(peer)
        })
        .collect()
}

struct StorageInner {
    review: TorrentReview,
    files: Vec<Arc<dyn DataFile>>,
    authority: Arc<dyn SourceAuthority>,
}
#[derive(Clone)]
struct ConfinedFactory {
    inner: Arc<StorageInner>,
}
struct ConfinedStorage {
    inner: Arc<StorageInner>,
    active: AtomicBool,
}
fn sdk_error() -> anyhow::Error {
    anyhow::anyhow!("confined source storage refused an operation")
}
impl ConfinedStorage {
    fn check(&self, file: usize, offset: u64, length: usize) -> anyhow::Result<&Arc<dyn DataFile>> {
        if !self.active.load(Ordering::Acquire) || self.inner.authority.remaining().is_err() {
            return Err(sdk_error());
        }
        let metadata = self.inner.review.files.get(file).ok_or_else(sdk_error)?;
        if offset
            .checked_add(length as u64)
            .is_none_or(|end| end > metadata.bytes)
        {
            return Err(sdk_error());
        }
        self.inner.files.get(file).ok_or_else(sdk_error)
    }
}
impl StorageFactory for ConfinedFactory {
    type Storage = ConfinedStorage;
    fn create(
        &self,
        _: &librqbit::ManagedTorrentShared,
        _: &librqbit::TorrentMetadata,
    ) -> anyhow::Result<Self::Storage> {
        Ok(ConfinedStorage {
            inner: self.inner.clone(),
            active: AtomicBool::new(true),
        })
    }
    fn clone_box(&self) -> BoxStorageFactory {
        self.clone().boxed()
    }
}
impl TorrentStorage for ConfinedStorage {
    fn init(
        &mut self,
        shared: &librqbit::ManagedTorrentShared,
        metadata: &librqbit::TorrentMetadata,
    ) -> anyhow::Result<()> {
        if shared.info_hash.as_string() != self.inner.review.info_hash
            || metadata.file_infos.len() != self.inner.files.len()
            || metadata
                .file_infos
                .iter()
                .zip(&self.inner.review.files)
                .any(|(actual, expected)| {
                    actual.len != expected.bytes || actual.offset_in_torrent != expected.offset
                })
        {
            return Err(sdk_error());
        }
        Ok(())
    }
    fn pread_exact(&self, file: usize, offset: u64, buffer: &mut [u8]) -> anyhow::Result<()> {
        self.check(file, offset, buffer.len())?
            .read_exact_at(offset, buffer)
            .map_err(|_| sdk_error())
    }
    fn pwrite_all(&self, file: usize, offset: u64, buffer: &[u8]) -> anyhow::Result<()> {
        let target = self.check(file, offset, buffer.len())?;
        self.inner
            .authority
            .charge(buffer.len() as u64)
            .map_err(|_| sdk_error())?;
        target.write_all_at(offset, buffer).map_err(|_| sdk_error())
    }
    fn ensure_file_length(&self, file: usize, length: u64) -> anyhow::Result<()> {
        if self
            .inner
            .review
            .files
            .get(file)
            .is_none_or(|metadata| metadata.bytes != length)
        {
            return Err(sdk_error());
        }
        self.check(file, 0, 0)?
            .set_len(length)
            .map_err(|_| sdk_error())
    }
    fn remove_file(&self, _: usize, _: &Path) -> anyhow::Result<()> {
        Err(sdk_error())
    }
    fn remove_directory_if_empty(&self, _: &Path) -> anyhow::Result<()> {
        Err(sdk_error())
    }
    fn take(&self) -> anyhow::Result<Box<dyn TorrentStorage>> {
        self.active.store(false, Ordering::Release);
        Ok(Box::new(Self {
            inner: self.inner.clone(),
            active: AtomicBool::new(true),
        }))
    }
}

/// Hash-verified file receipt. Private staging uses numeric IDs, never metainfo paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentFileReceipt {
    /// Reviewed file index.
    pub index: usize,
    /// Exact complete bytes independently read back.
    pub bytes: u64,
    /// SHA-256 after selected v1 pieces have independently passed SHA-1 checks.
    pub sha256: String,
}
/// A completed piece-verified staging result, before safe destination publication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentReceipt {
    /// Exact selected files in reviewed index order.
    pub files: Vec<TorrentFileReceipt>,
    /// Must remain zero; control/handshake packets still leave the device.
    pub payload_uploaded_bytes: u64,
}

fn verify(inner: &StorageInner, indices: &[usize]) -> SourceResult<TorrentReceipt> {
    for file in &inner.files {
        file.sync_all().map_err(|_| SourceError::Storage)?;
    }
    let mut pieces = BTreeSet::new();
    for &index in indices {
        let file = inner.review.files.get(index).ok_or(SourceError::Invalid)?;
        if file.bytes > 0 {
            pieces.extend(
                file.offset / inner.review.piece_length
                    ..(file.offset + file.bytes).div_ceil(inner.review.piece_length),
            );
        }
    }
    let mut buffer = vec![0; 64 * 1024];
    for piece in pieces {
        let mut digest = sha1::Sha1::new();
        let mut at = piece * inner.review.piece_length;
        let end = (at + inner.review.piece_length).min(inner.review.total_bytes);
        while at < end {
            inner.authority.remaining()?;
            let file = inner
                .review
                .files
                .iter()
                .find(|file| file.offset <= at && at < file.offset + file.bytes)
                .ok_or(SourceError::Integrity)?;
            let n = buffer
                .len()
                .min((end - at).min(file.offset + file.bytes - at) as usize);
            inner.files[file.index]
                .read_exact_at(at - file.offset, &mut buffer[..n])
                .map_err(|_| SourceError::Storage)?;
            digest.update(&buffer[..n]);
            at += n as u64;
        }
        let actual: [u8; 20] = digest.finalize().into();
        if actual != inner.review.piece_hashes[piece as usize] {
            return Err(SourceError::Integrity);
        }
    }
    let mut files = Vec::new();
    for &index in indices {
        let file = &inner.review.files[index];
        let mut at = 0;
        let mut digest = Sha256::new();
        while at < file.bytes {
            inner.authority.remaining()?;
            let n = buffer.len().min((file.bytes - at) as usize);
            inner.files[index]
                .read_exact_at(at, &mut buffer[..n])
                .map_err(|_| SourceError::Storage)?;
            digest.update(&buffer[..n]);
            at += n as u64;
        }
        files.push(TorrentFileReceipt {
            index,
            bytes: file.bytes,
            sha256: hex::encode(digest.finalize()),
        });
    }
    Ok(TorrentReceipt {
        files,
        payload_uploaded_bytes: 0,
    })
}

/// Numeric private staging filename for a previously reviewed file index.
pub fn staging_path(directory: &Path, index: usize) -> PathBuf {
    directory.join(format!("data-{index:04}.bin"))
}

/// Acquire selected pieces through explicit IP-scoped TCP peers. The callback's
/// authority persists payload-byte charges before each SDK write, including bad
/// pieces/retries. Rate/time/peer bounds also limit protocol overhead; packet or
/// handshake bytes are not claimed as piece payload. Restart rehashes retained
/// files, with SDK persistence/fastresume disabled and no automatic network resume.
pub async fn acquire(
    config: &SourceAccountConfig,
    target: &SourceTarget,
    review: TorrentReview,
    directory: &Path,
    authority: Arc<dyn SourceAuthority>,
    cancel: CancellationToken,
) -> SourceResult<TorrentReceipt> {
    let peers = peer_scope(config, target, &review)?;
    let SourceAccountConfig::Torrent {
        max_peers,
        max_download_bytes_per_second,
        allowed_peer_ips,
    } = config
    else {
        return Err(SourceError::Invalid);
    };
    let SourceTarget::Torrent { file_indices, .. } = target else {
        return Err(SourceError::Invalid);
    };
    authority.remaining()?;
    if std::fs::symlink_metadata(directory)
        .is_ok_and(|metadata| !metadata.is_dir() || metadata.file_type().is_symlink())
    {
        return Err(SourceError::Storage);
    }
    RealFs
        .create_dir_all(directory)
        .map_err(|_| SourceError::Storage)?;
    let mut files = Vec::new();
    for file in &review.files {
        let path = staging_path(directory, file.index);
        let existing = RealFs
            .metadata(&path)
            .map_err(|_| SourceError::Storage)?
            .is_some();
        let handle = if existing {
            RealFs.open_existing(&path)
        } else {
            RealFs.create_new(&path)
        }
        .map_err(|_| SourceError::Storage)?;
        handle
            .set_len(file.bytes)
            .map_err(|_| SourceError::Storage)?;
        files.push(Arc::from(handle));
    }
    let allowlist = directory.join("peer-allowlist.txt");
    let contents = allowed_peer_ips
        .iter()
        .map(|ip| format!("zeus:{ip}-{ip}\n"))
        .collect::<String>();
    let allowfile = if RealFs
        .metadata(&allowlist)
        .map_err(|_| SourceError::Storage)?
        .is_some()
    {
        RealFs.open_existing(&allowlist)
    } else {
        RealFs.create_new(&allowlist)
    }
    .map_err(|_| SourceError::Storage)?;
    allowfile
        .set_len(contents.len() as u64)
        .map_err(|_| SourceError::Storage)?;
    allowfile
        .write_all_at(0, contents.as_bytes())
        .and_then(|()| allowfile.sync_all())
        .map_err(|_| SourceError::Storage)?;
    drop(allowfile);
    let inner = Arc::new(StorageInner {
        review,
        files,
        authority: authority.clone(),
    });
    let session_cancel = CancellationToken::new();
    let options = SessionOptions {
        dht: None,
        listen: None,
        persistence: None,
        fastresume: false,
        disable_trackers: true,
        disable_local_service_discovery: true,
        disable_upload: true,
        connect: Some(librqbit::ConnectionOptions {
            enable_tcp: true,
            proxy_url: None,
            peer_opts: Some(librqbit::PeerConnectionOptions {
                connect_timeout: Some(std::time::Duration::from_secs(10)),
                read_write_timeout: Some(std::time::Duration::from_secs(20)),
                ..Default::default()
            }),
        }),
        default_storage_factory: Some(
            ConfinedFactory {
                inner: inner.clone(),
            }
            .boxed(),
        ),
        cancellation_token: Some(session_cancel.clone()),
        concurrent_init_limit: Some(1),
        runtime_worker_threads: Some(2),
        peer_limit: Some(usize::from(*max_peers)),
        allowlist_url: Some(
            url::Url::from_file_path(&allowlist)
                .map_err(|_| SourceError::Storage)?
                .to_string(),
        ),
        ratelimits: librqbit::limits::LimitsConfig {
            download_bps: std::num::NonZeroU32::new(*max_download_bytes_per_second as u32),
            upload_bps: std::num::NonZeroU32::new(1),
        },
        client_name_and_version: Some("Zeus/0.1 scoped-peer source".into()),
        ..Default::default()
    };
    let session = Session::new_with_opts(directory.to_path_buf(), options)
        .await
        .map_err(|_| SourceError::Network)?;
    let added = session
        .add_torrent(
            AddTorrent::from_bytes(inner.review.sdk_metainfo.clone()),
            Some(AddTorrentOptions {
                only_files: Some(file_indices.clone()),
                overwrite: true,
                initial_peers: Some(peers),
                ..Default::default()
            }),
        )
        .await;
    let outcome = async {
        let handle = added
            .map_err(|_| SourceError::Protocol)?
            .into_handle()
            .ok_or(SourceError::Protocol)?;
        let complete = handle.wait_until_completed();
        tokio::pin!(complete);
        loop {
            let remaining = authority.remaining()?;
            if handle.stats().uploaded_bytes != 0 {
                return Err(SourceError::Integrity);
            }
            tokio::select! {
                _=cancel.cancelled()=>return Err(SourceError::Canceled),
                _=tokio::time::sleep(remaining.min(std::time::Duration::from_millis(100)))=>{},
                result=&mut complete=>{result.map_err(|_|SourceError::Network)?;break},
            }
        }
        if handle.stats().uploaded_bytes != 0 {
            return Err(SourceError::Integrity);
        }
        Ok(())
    }
    .await;
    session_cancel.cancel();
    session.stop().await;
    outcome?;
    let selected = file_indices.clone();
    tokio::task::spawn_blocking(move || verify(&inner, &selected))
        .await
        .map_err(|_| SourceError::Storage)?
}
