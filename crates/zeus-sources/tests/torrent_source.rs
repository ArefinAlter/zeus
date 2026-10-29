//! Real loopback v1 peers; no tracker, Internet, user index or other program.
#![cfg(feature = "torrent")]
use sha2::Digest;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use zeus_sources::{
    SourceAccountConfig, SourceAuthority, SourceError, SourceResult, SourceTarget, torrent,
};

struct Authority {
    received: AtomicU64,
    limit: u64,
    denied: AtomicBool,
    deadline: Instant,
}
impl SourceAuthority for Authority {
    fn remaining(&self) -> SourceResult<Duration> {
        if self.denied.load(Ordering::Acquire) {
            return Err(SourceError::Denied);
        }
        self.deadline
            .checked_duration_since(Instant::now())
            .ok_or(SourceError::Budget)
    }
    fn charge(&self, bytes: u64) -> SourceResult<()> {
        self.remaining()?;
        // try_update replaces this only after the supported Rust 1.90 baseline.
        #[allow(deprecated)]
        if self
            .received
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                old.checked_add(bytes).filter(|new| *new <= self.limit)
            })
            .is_err()
        {
            self.denied.store(true, Ordering::Release);
            return Err(SourceError::Budget);
        }
        Ok(())
    }
}
fn authority(limit: u64) -> Arc<Authority> {
    Arc::new(Authority {
        received: AtomicU64::new(0),
        limit,
        denied: AtomicBool::new(false),
        deadline: Instant::now() + Duration::from_secs(30),
    })
}
fn metainfo(data: &[u8], name: &[u8]) -> Vec<u8> {
    let pieces = data
        .chunks(16384)
        .flat_map(|chunk| sha1::Sha1::digest(chunk).to_vec())
        .collect::<Vec<_>>();
    let mut bytes = format!("d4:infod6:lengthi{}e4:name{}:", data.len(), name.len()).into_bytes();
    bytes.extend_from_slice(name);
    bytes.extend_from_slice(format!("12:piece lengthi16384e6:pieces{}:", pieces.len()).as_bytes());
    bytes.extend_from_slice(&pieces);
    bytes.extend_from_slice(b"ee");
    bytes
}
struct Peer {
    address: std::net::SocketAddr,
    requests: Arc<AtomicU64>,
    uploads: Arc<AtomicU64>,
    upload_requests: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.task.abort()
    }
}
async fn peer(data: Vec<u8>, info_hash: &str, corrupt_first: bool, stall: bool) -> Peer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let info_hash = hex::decode(info_hash).unwrap();
    let requests = Arc::new(AtomicU64::new(0));
    let uploads = Arc::new(AtomicU64::new(0));
    let upload_requests = Arc::new(AtomicU64::new(0));
    let (seen, sent, attempts) = (requests.clone(), uploads.clone(), upload_requests.clone());
    let data = Arc::new(data);
    let corrupt = Arc::new(AtomicBool::new(corrupt_first));
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let (data, seen, sent, attempts, corrupt, hash) = (
                data.clone(),
                seen.clone(),
                sent.clone(),
                attempts.clone(),
                corrupt.clone(),
                info_hash.clone(),
            );
            tokio::spawn(async move {
                let mut handshake = [0u8; 68];
                if socket.read_exact(&mut handshake).await.is_err() {
                    return;
                }
                assert_eq!(&handshake[28..48], hash.as_slice());
                let mut answer = b"\x13BitTorrent protocol".to_vec();
                answer.extend_from_slice(&[0; 8]);
                answer.extend_from_slice(&hash);
                answer.extend_from_slice(b"-ZSTEST-000000000000");
                if socket.write_all(&answer).await.is_err() {
                    return;
                }
                let pieces = data.len().div_ceil(16384);
                let mut bitfield = vec![0u8; pieces.div_ceil(8)];
                for index in 0..pieces {
                    bitfield[index / 8] |= 0x80 >> (index % 8)
                }
                let mut field = ((bitfield.len() + 1) as u32).to_be_bytes().to_vec();
                field.push(5);
                field.extend_from_slice(&bitfield);
                field.extend_from_slice(&[0, 0, 0, 1, 1]);
                if socket.write_all(&field).await.is_err() {
                    return;
                }
                let mut served = std::collections::BTreeSet::new();
                let mut served_bytes = 0;
                let mut asked_upload = false;
                loop {
                    let Ok(length) = socket.read_u32().await else {
                        return;
                    };
                    if length == 0 {
                        continue;
                    }
                    if length > 1 << 20 {
                        return;
                    }
                    let mut message = vec![0; length as usize];
                    if socket.read_exact(&mut message).await.is_err() {
                        return;
                    }
                    if message[0] == 7 {
                        sent.fetch_add((message.len().saturating_sub(9)) as u64, Ordering::AcqRel);
                        continue;
                    }
                    if message[0] != 6 || message.len() != 13 {
                        continue;
                    }
                    seen.fetch_add(1, Ordering::AcqRel);
                    if stall {
                        continue;
                    }
                    let index = u32::from_be_bytes(message[1..5].try_into().unwrap()) as usize;
                    let begin = u32::from_be_bytes(message[5..9].try_into().unwrap()) as usize;
                    let count = u32::from_be_bytes(message[9..13].try_into().unwrap()) as usize;
                    let offset = index * 16384 + begin;
                    if offset.checked_add(count).is_none_or(|end| end > data.len()) {
                        return;
                    }
                    let mut answer = ((count + 9) as u32).to_be_bytes().to_vec();
                    answer.push(7);
                    answer.extend_from_slice(&message[1..9]);
                    answer.extend_from_slice(&data[offset..offset + count]);
                    if corrupt.load(Ordering::Acquire) && count > 0 {
                        *answer.last_mut().unwrap() ^= 1
                    }
                    if socket.write_all(&answer).await.is_err() {
                        return;
                    }
                    if served.insert((index, begin, count)) {
                        served_bytes += count;
                    }
                    // Upload refusal terminates a misbehaving peer connection.
                    // Deliver the valid fixture first, so that this probe does
                    // not repeatedly disconnect its only source mid-download.
                    if !asked_upload && served_bytes >= data.len() {
                        asked_upload = true;
                        let mut upload_request = vec![0, 0, 0, 13, 6];
                        upload_request.extend_from_slice(&(index as u32).to_be_bytes());
                        upload_request.extend_from_slice(&0u32.to_be_bytes());
                        upload_request.extend_from_slice(&1u32.to_be_bytes());
                        if socket.write_all(&upload_request).await.is_ok() {
                            attempts.fetch_add(1, Ordering::AcqRel);
                        }
                    }
                }
            });
        }
    });
    Peer {
        address,
        requests,
        uploads,
        upload_requests,
        task,
    }
}
fn scope(review: &torrent::TorrentReview, peer: &Peer) -> (SourceAccountConfig, SourceTarget) {
    (
        SourceAccountConfig::Torrent {
            max_peers: 1,
            max_download_bytes_per_second: 4 * 1024 * 1024,
            allowed_peer_ips: vec!["127.0.0.1".into()],
        },
        SourceTarget::Torrent {
            metainfo_sha256: review.metainfo_sha256.clone(),
            file_indices: vec![0],
            initial_peers: vec![peer.address.to_string()],
        },
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_scoped_peer_download_uploads_no_payload() {
    let data = (0..48000)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let bytes = metainfo(&data, b"fixture.bin");
    let review = torrent::inspect(&bytes).unwrap();
    let peer = peer(data.clone(), &review.info_hash, false, false).await;
    let (config, target) = scope(&review, &peer);
    let directory = tempfile::tempdir().unwrap();
    let authority = authority(1024 * 1024);
    let receipt = torrent::acquire(
        &config,
        &target,
        review,
        directory.path(),
        authority.clone(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        receipt.files[0].sha256,
        hex::encode(sha2::Sha256::digest(&data))
    );
    assert_eq!(
        std::fs::read(torrent::staging_path(directory.path(), 0)).unwrap(),
        data
    );
    assert!(
        authority.received.load(Ordering::Acquire) >= data.len() as u64,
        "all received piece bytes must be charged"
    );
    assert_eq!(peer.uploads.load(Ordering::Acquire), 0);
    assert!(peer.upload_requests.load(Ordering::Acquire) > 0);
    assert_eq!(receipt.payload_uploaded_bytes, 0);
    let names = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert!(!names.iter().any(|name| name == "fixture.bin"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupted_peer_pieces_never_produce_a_verified_receipt() {
    let data = vec![71; 48000];
    let review = torrent::inspect(&metainfo(&data, b"fixture.bin")).unwrap();
    let peer = peer(data, &review.info_hash, true, false).await;
    let (config, target) = scope(&review, &peer);
    let directory = tempfile::tempdir().unwrap();
    let authority = Arc::new(Authority {
        received: AtomicU64::new(0),
        limit: 1024 * 1024,
        denied: AtomicBool::new(false),
        deadline: Instant::now() + Duration::from_secs(3),
    });
    // A sole malicious peer may be banned after its first bad piece. There is no
    // promise to retry that peer: the reviewed deadline still bounds the attempt.
    let result = torrent::acquire(
        &config,
        &target,
        review,
        directory.path(),
        authority.clone(),
        CancellationToken::new(),
    )
    .await;
    assert!(
        result.is_err(),
        "corrupt content must never return a verified receipt"
    );
    assert!(
        authority.received.load(Ordering::Acquire) > 0,
        "the fixture must actually deliver corrupt piece payload"
    );
    assert!(peer.requests.load(Ordering::Acquire) > 0);
    assert_eq!(peer.uploads.load(Ordering::Acquire), 0);
    assert!(!directory.path().join("fixture.bin").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peer_cancellation_and_revocation_stop_without_publishing() {
    let data = vec![29; 48000];
    let bytes = metainfo(&data, b"fixture.bin");
    let review = torrent::inspect(&bytes).unwrap();
    let peer = peer(data, &review.info_hash, false, true).await;
    let (config, target) = scope(&review, &peer);
    let directory = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let stopped = cancel.clone();
    let observed = peer.requests.clone();
    tokio::spawn(async move {
        while observed.load(Ordering::Acquire) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await
        }
        stopped.cancel()
    });
    assert!(matches!(
        torrent::acquire(
            &config,
            &target,
            review,
            directory.path(),
            authority(1024 * 1024),
            cancel
        )
        .await,
        Err(SourceError::Canceled)
    ));
    assert_eq!(peer.uploads.load(Ordering::Acquire), 0);
}

#[test]
fn metainfo_paths_sizes_and_peer_scope_are_checked_before_network() {
    for name in [b"..".as_slice(), b"/outside", b"C:escape", b"CON", b"a\\b"] {
        assert!(torrent::inspect(&metainfo(b"data", name)).is_err())
    }
    assert!(torrent::inspect(&[b'l'; 2 * 1024 * 1024 + 1]).is_err());
    let bytes = metainfo(b"data", b"valid.bin");
    let review = torrent::inspect(&bytes).unwrap();
    let config = SourceAccountConfig::Torrent {
        max_peers: 1,
        max_download_bytes_per_second: 1024,
        allowed_peer_ips: vec!["127.0.0.1".into()],
    };
    let target = SourceTarget::Torrent {
        metainfo_sha256: review.metainfo_sha256.clone(),
        file_indices: vec![0],
        initial_peers: vec!["127.0.0.2:1234".into()],
    };
    assert_eq!(
        torrent::peer_scope(&config, &target, &review),
        Err(SourceError::Denied)
    );
    let deep = [vec![b'l'; 40], vec![b'e'; 40]].concat();
    assert!(torrent::inspect(&deep).is_err());
}
