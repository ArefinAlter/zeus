use super::*;
use std::io::{Read, Seek, Write};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    source: PathBuf,
    destination: PathBuf,
    receipts: PathBuf,
}
impl Fixture {
    fn new(tree: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let source = root.join("source");
        let destination = root.join("destination");
        let receipts = root.join("receipts");
        std::fs::create_dir(&destination).unwrap();
        if tree {
            std::fs::create_dir_all(source.join("nested")).unwrap();
            std::fs::write(source.join("nested/.hidden"), b"hidden").unwrap();
            std::fs::write(source.join("empty"), b"").unwrap();
        } else {
            std::fs::write(&source, vec![0x35; CHUNK * 2 + 731]).unwrap();
        }
        Self {
            _temp: temp,
            root,
            source,
            destination,
            receipts,
        }
    }
    fn authority(&self) -> Authority {
        let roots = vec![shown(&self.root)];
        Authority {
            reads: RootCaps::capture(&roots).unwrap(),
            writes: RootCaps::capture(&roots).unwrap(),
            path_check: Arc::new(|_| Ok(())),
            check: Arc::new(|| Ok(())),
        }
    }
    fn prepare(&self, authority: &Authority) -> (Arc<Store>, Arc<Job>, Log) {
        let (store, rows) = Store::open(&self.receipts).unwrap();
        assert!(rows.is_empty());
        let plan =
            worker::plan(&shown(&self.source), &shown(&self.destination), authority).unwrap();
        let log = store.create(&plan).unwrap();
        let mut r = Receipt::new(plan);
        log.bind(&mut r);
        (
            store,
            Arc::new(Job {
                receipt: Mutex::new(r),
                cancelled: AtomicBool::new(false),
                active: AtomicBool::new(false),
            }),
            log,
        )
    }
    fn interrupted(&self) -> String {
        let authority = self.authority();
        let (store, job, mut log) = self.prepare(&authority);
        let id = job.receipt.lock().unwrap().plan.id.clone();
        let output = self
            .destination
            .join(".zeus-partial")
            .join(format!("copy-{id}"))
            .join("payload");
        let authority = Authority {
            check: Arc::new(move || {
                if std::fs::metadata(&output).is_ok_and(|m| m.len() >= CHUNK as u64) {
                    Err(failed(
                        "Simulated scope revocation after the first synced chunk.",
                    ))
                } else {
                    Ok(())
                }
            }),
            ..authority
        };
        assert!(worker::run(&job, &mut log, &authority).is_err());
        assert_eq!(job.receipt.lock().unwrap().bytes(), CHUNK as u64);
        assert!(!self.destination.join("source").exists());
        drop(log);
        drop(store);
        drop(job);
        id
    }
    fn resume(&self, id: &str) -> Result<()> {
        let (store, mut rows) = Store::open(&self.receipts)?;
        assert_eq!(rows.len(), 1);
        let r = rows.remove(0);
        assert_eq!(r.plan.id, id);
        assert_eq!(r.state, "interrupted");
        let mut log = store.resume(&r)?;
        let job = Job {
            receipt: Mutex::new(r),
            cancelled: AtomicBool::new(false),
            active: AtomicBool::new(false),
        };
        worker::run(&job, &mut log, &self.authority())
    }
}
#[test]
fn whole_tree_is_published_once_with_hidden_and_empty_files() {
    let f = Fixture::new(true);
    let a = f.authority();
    let (_store, job, mut log) = f.prepare(&a);
    worker::run(&job, &mut log, &a).unwrap();
    assert_eq!(job.receipt.lock().unwrap().state, "complete");
    assert_eq!(
        std::fs::read(f.destination.join("source/nested/.hidden")).unwrap(),
        b"hidden"
    );
    assert!(f.destination.join("source/empty").exists());
    assert!(worker::plan(&shown(&f.source), &shown(&f.destination), &a).is_err());
    assert!(f.source.join("nested/.hidden").exists());
}
#[test]
fn restart_does_not_resume_and_explicit_resume_verifies_committed_prefix() {
    let f = Fixture::new(false);
    let id = f.interrupted();
    {
        let transfers = Transfers::new(f.receipts.clone());
        assert_eq!(transfers.list()["jobs"][0]["state"], "interrupted");
        assert!(!f.destination.join("source").exists());
    }
    f.resume(&id).unwrap();
    assert_eq!(
        std::fs::read(&f.source).unwrap(),
        std::fs::read(f.destination.join("source")).unwrap()
    );
}
#[test]
fn same_identity_same_mtime_source_edit_is_detected_by_checkpoint_digest() {
    let f = Fixture::new(false);
    let id = f.interrupted();
    let modified = std::fs::metadata(&f.source).unwrap().modified().unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&f.source)
        .unwrap();
    file.write_all(b"changed").unwrap();
    file.sync_all().unwrap();
    file.set_modified(modified).unwrap();
    drop(file);
    assert!(f.resume(&id).is_err());
    assert!(!f.destination.join("source").exists());
    assert!(f.source.exists());
}
#[test]
fn changed_original_mtime_cannot_be_adopted_even_when_bytes_match() {
    let f = Fixture::new(false);
    let id = f.interrupted();
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&f.source)
        .unwrap();
    file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1))
        .unwrap();
    drop(file);
    assert!(f.resume(&id).is_err());
    assert!(!f.destination.join("source").exists());
}
#[test]
fn tampered_staging_or_replaced_destination_is_preserved_and_refused() {
    let f = Fixture::new(false);
    let id = f.interrupted();
    let stage = f
        .destination
        .join(".zeus-partial")
        .join(format!("copy-{id}"))
        .join("payload");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&stage)
        .unwrap();
    file.write_all(b"bad").unwrap();
    file.sync_all().unwrap();
    drop(file);
    assert!(f.resume(&id).is_err());
    assert!(stage.exists());
    let f = Fixture::new(false);
    let id = f.interrupted();
    let original = f.root.join("old-destination");
    std::fs::rename(&f.destination, &original).unwrap();
    std::fs::create_dir(&f.destination).unwrap();
    assert!(f.resume(&id).is_err());
    assert!(original.join(".zeus-partial").exists());
    assert!(!f.destination.join("source").exists());
}
#[test]
fn torn_last_frame_and_uncommitted_data_tail_are_recovered_only_on_resume() {
    let f = Fixture::new(false);
    let id = f.interrupted();
    let receipt = f.receipts.join(format!("{id}.receipt"));
    let mut log = std::fs::OpenOptions::new()
        .append(true)
        .open(&receipt)
        .unwrap();
    log.write_all(&[1, 2, 3, 4, 5]).unwrap();
    log.sync_all().unwrap();
    drop(log);
    let stage = f
        .destination
        .join(".zeus-partial")
        .join(format!("copy-{id}"))
        .join("payload");
    let mut output = std::fs::OpenOptions::new()
        .append(true)
        .open(stage)
        .unwrap();
    output.write_all(b"uncommitted").unwrap();
    output.sync_all().unwrap();
    drop(output);
    {
        let transfers = Transfers::new(f.receipts.clone());
        assert!(transfers.list()["load_error"].is_null());
        assert!(!f.destination.join("source").exists());
    }
    f.resume(&id).unwrap();
    assert_eq!(
        std::fs::read(&f.source).unwrap(),
        std::fs::read(f.destination.join("source")).unwrap()
    );
}
#[test]
fn crash_after_publication_is_reconciled_by_identity_and_content() {
    let f = Fixture::new(true);
    let a = f.authority();
    let (store, job, mut log) = f.prepare(&a);
    let id = job.receipt.lock().unwrap().plan.id.clone();
    worker::run(&job, &mut log, &a).unwrap();
    drop(log);
    drop(job);
    drop(store);
    let path = f.receipts.join(format!("{id}.receipt"));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    file.seek(std::io::SeekFrom::Start(8)).unwrap();
    let len = file.metadata().unwrap().len();
    let mut offset = 8;
    while offset < len {
        let mut header = [0; 36];
        file.read_exact(&mut header).unwrap();
        let size = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
        let mut bytes = vec![0; size];
        file.read_exact(&mut bytes).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        if value["event"] == "complete" {
            file.set_len(offset).unwrap();
            break;
        }
        offset += 36 + size as u64;
    }
    file.sync_all().unwrap();
    drop(file);
    f.resume(&id).unwrap();
    assert_eq!(
        std::fs::read(f.destination.join("source/nested/.hidden")).unwrap(),
        b"hidden"
    );
}
#[test]
fn unexpected_staging_entry_never_reaches_the_final_destination() {
    let f = Fixture::new(false);
    let id = f.interrupted();
    let extra = f
        .destination
        .join(".zeus-partial")
        .join(format!("copy-{id}"))
        .join("extra");
    std::fs::write(&extra, b"keep").unwrap();
    assert!(f.resume(&id).is_err());
    assert!(extra.exists());
    assert!(!f.destination.join("source").exists());
}
#[test]
fn one_store_owner_and_four_worker_slots_bound_scans_as_well_as_copying() {
    let f = Fixture::new(false);
    let transfers = Transfers::new(f.receipts.clone());
    let second = Transfers::new(f.receipts.clone());
    assert!(!second.list()["load_error"].is_null());
    let leases = (0..4)
        .map(|_| transfers.acquire().unwrap())
        .collect::<Vec<_>>();
    assert!(transfers.acquire().is_err());
    drop(leases);
    assert!(transfers.acquire().is_ok());
}

#[test]
fn checkpoint_storage_failure_keeps_the_last_synced_prefix_and_source() {
    let f = Fixture::new(false);
    let authority = f.authority();
    let (store, job, mut log) = f.prepare(&authority);
    let id = job.receipt.lock().unwrap().plan.id.clone();
    let output = f
        .destination
        .join(".zeus-partial")
        .join(format!("copy-{id}"))
        .join("payload");
    let quota = store.clone();
    let authority = Authority {
        check: Arc::new(move || {
            if std::fs::metadata(&output).is_ok_and(|m| m.len() >= CHUNK as u64) {
                quota.exhaust_budget();
            }
            Ok(())
        }),
        ..authority
    };
    assert!(worker::run(&job, &mut log, &authority).is_err());
    assert_eq!(job.receipt.lock().unwrap().bytes(), CHUNK as u64);
    assert!(f.source.exists());
    assert!(!f.destination.join("source").exists());
    drop(authority);
    drop(log);
    drop(job);
    drop(store);
    f.resume(&id).unwrap();
    assert_eq!(
        std::fs::read(&f.source).unwrap(),
        std::fs::read(f.destination.join("source")).unwrap()
    );
}
#[test]
fn changed_source_parent_and_replaced_staging_identity_refuse_resume() {
    let f = Fixture::new(false);
    let id = f.interrupted();
    let original = f.root.join("original");
    std::fs::rename(&f.source, &original).unwrap();
    std::fs::copy(&original, &f.source).unwrap();
    assert!(f.resume(&id).is_err());
    assert!(original.exists());
    let f = Fixture::new(false);
    let id = f.interrupted();
    let stage = f
        .destination
        .join(".zeus-partial")
        .join(format!("copy-{id}"));
    std::fs::rename(&stage, stage.with_extension("original")).unwrap();
    std::fs::create_dir(&stage).unwrap();
    assert!(f.resume(&id).is_err());
    assert!(stage.with_extension("original").exists());
}
#[test]
fn elapsed_shared_validation_deadline_refuses_before_publication() {
    let f = Fixture::new(false);
    let a = f.authority();
    let (_store, job, _log) = f.prepare(&a);
    let plan = job.receipt.lock().unwrap().plan.clone();
    assert!(
        worker::verify_sources(
            &job,
            &a,
            &plan,
            Instant::now() - VERIFY_TIME - Duration::from_secs(1)
        )
        .is_err()
    );
    assert!(!f.destination.join("source").exists());
}

#[test]
fn reviewed_manifest_rejects_changed_uncopied_content_even_with_original_metadata() {
    let f = Fixture::new(false);
    let a = f.authority();
    let prepared = prepare_verified(&shown(&f.source), &shown(&f.destination), false, &a).unwrap();
    let modified = std::fs::metadata(&f.source).unwrap().modified().unwrap();
    let mut changed = std::fs::OpenOptions::new()
        .write(true)
        .open(&f.source)
        .unwrap();
    changed
        .seek(std::io::SeekFrom::Start(CHUNK as u64 + 17))
        .unwrap();
    changed.write_all(b"modified after exact review").unwrap();
    changed.sync_all().unwrap();
    changed.set_modified(modified).unwrap();
    drop(changed);
    let (store, _) = Store::open(&f.receipts).unwrap();
    let mut log = store.create(&prepared.plan).unwrap();
    let mut receipt = Receipt::new(prepared.plan);
    log.bind(&mut receipt);
    let job = Job {
        receipt: Mutex::new(receipt),
        cancelled: AtomicBool::new(false),
        active: AtomicBool::new(false),
    };
    assert!(worker::run(&job, &mut log, &a).is_err());
    assert!(!f.destination.join("source").exists());
    assert!(f.source.exists());
}
#[tokio::test]
async fn approved_copy_replay_observes_one_receipt_and_never_replaces_a_target() {
    let f = Fixture::new(true);
    let prepared = prepare_verified(
        &shown(&f.source),
        &shown(&f.destination),
        false,
        &f.authority(),
    )
    .unwrap();
    let transfers = Transfers::new(f.receipts.clone());
    let first = transfers
        .start_prepared(prepared.clone(), f.authority())
        .unwrap();
    let second = transfers
        .start_prepared(prepared.clone(), f.authority())
        .unwrap();
    assert_eq!(first["id"], second["id"]);
    for _ in 0..300 {
        let row = transfers.get(prepared.id(), false).unwrap();
        if row["state"] == "complete" {
            break;
        }
        assert_ne!(row["state"], "failed", "{row}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        transfers.get(prepared.id(), false).unwrap()["state"],
        "complete"
    );
    let replay = transfers
        .start_prepared(prepared.clone(), f.authority())
        .unwrap();
    assert_eq!(replay["state"], "complete");
    assert_eq!(transfers.list()["jobs"].as_array().unwrap().len(), 1);
}
#[test]
fn host_path_policy_is_checked_before_any_copy_output() {
    let f = Fixture::new(false);
    let a = Authority {
        path_check: Arc::new(|_| Err(failed("Host quarantined this source."))),
        ..f.authority()
    };
    assert!(worker::plan(&shown(&f.source), &shown(&f.destination), &a).is_err());
    assert!(!f.destination.join(".zeus-partial").exists());
}
#[test]
fn synchronization_copy_keeps_the_comparison_digest_and_refuses_changed_source_metadata() {
    let f=Fixture::new(true);let root=f.root.join("source-root");std::fs::create_dir(&root).unwrap();std::fs::rename(&f.source,root.join("source")).unwrap();
    let pair=crate::compare::snapshot_pair(&crate::compare::Request {left:shown(&root),right:shown(&f.destination),content:true},&f.authority()).unwrap();
    let plan=prepare_from_snapshot(&pair.left,"source",&shown(&f.destination),&f.authority()).unwrap();assert_eq!(plan.entries(),4);
    let source=root.join("source/nested/.hidden");std::fs::write(&source,b"modified").unwrap();
    assert!(prepare_from_snapshot(&pair.left,"source",&shown(&f.destination),&f.authority()).is_err());
}
