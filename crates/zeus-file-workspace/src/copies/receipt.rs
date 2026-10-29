//! Checksummed append-only copy receipts. Data sync precedes the checkpoint that describes it.
use super::*;
use std::io::{Read, Seek, SeekFrom, Write};

pub(super) const MAX_JOBS: usize = 64;
const MAX_FRAME: usize = 64 * 1024 * 1024;
const MAX_LOG: u64 = 128 * 1024 * 1024;
const MAX_STORE: u64 = 512 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"ZEUSCP01";

#[derive(Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(super) enum Event {
    Plan {
        plan: Plan,
    },
    Stage {
        identity: zeus_types::FileId,
    },
    Created {
        index: usize,
        identity: zeus_types::FileId,
    },
    Chunk {
        index: usize,
        digest: String,
    },
    Done {
        index: usize,
        modified: SystemTime,
    },
    Publishing,
    Complete,
    Stopped {
        state: String,
        message: String,
    },
}

pub(super) struct Store {
    directory: DirectoryCap,
    bytes: AtomicU64,
    count: AtomicU64,
    _lease: std::fs::File,
}
pub(super) struct Log {
    file: std::fs::File,
    identity: zeus_types::FileId,
    valid: u64,
    store: Arc<Store>,
}
impl Store {
    pub(super) fn open(path: &Path) -> Result<(Arc<Self>, Vec<Receipt>)> {
        std::fs::create_dir_all(path)?;
        let directory = DirectoryCap::open_root(path).map_err(cap_failure)?;
        let lease = if directory
            .entry_exists("store.lock".as_ref())
            .map_err(cap_failure)?
        {
            directory
                .open_existing("store.lock".as_ref())
                .map_err(cap_failure)?
        } else {
            directory
                .create_new("store.lock".as_ref())
                .map_err(cap_failure)?
        };
        lease.try_lock().map_err(|_| {
            failed("Another desktop owns copy history. Close it before resuming copies.")
        })?;
        let names = directory.entry_names(MAX_JOBS + 1).map_err(cap_failure)?;
        let mut bytes = 0u64;
        let mut records = Vec::new();
        let started = Instant::now();
        for name in names {
            if started.elapsed() > VERIFY_TIME {
                return Err(failed(
                    "Copy history needs manual recovery: its load deadline was exceeded.",
                ));
            }
            let name = name
                .to_str()
                .ok_or_else(|| failed("Copy history contains an invalid filename."))?;
            if name == "store.lock" {
                continue;
            }
            let id = name
                .strip_suffix(".receipt")
                .filter(|s| valid_id(s))
                .ok_or_else(|| {
                    failed("Copy history contains an unknown entry. Preserve it for recovery.")
                })?;
            let mut file = directory.open_read(name.as_ref()).map_err(cap_failure)?;
            let len = file.metadata()?.len();
            bytes = bytes
                .checked_add(len)
                .filter(|b| *b <= MAX_STORE)
                .ok_or_else(|| {
                    failed("Copy history exceeds its storage bound. Preserve it for recovery.")
                })?;
            if len > MAX_LOG {
                return Err(failed("A copy receipt exceeds its storage bound."));
            }
            let (mut receipt, valid) = read(&mut file)?;
            if receipt.plan.id != id {
                return Err(failed(
                    "A copy receipt identity does not match its filename.",
                ));
            }
            receipt.log_id = Some(file_id(&file)?);
            receipt.valid = valid;
            if receipt.state != "complete" {
                receipt.state = "interrupted".into();
                receipt.error = "Zeus stopped with an unfinished copy. Resume explicitly after reviewing its source and destination.".into();
            }
            records.push(receipt);
        }
        Ok((
            Arc::new(Self {
                directory,
                bytes: AtomicU64::new(bytes),
                count: AtomicU64::new(records.len() as u64),
                _lease: lease,
            }),
            records,
        ))
    }
    #[cfg(test)]
    pub(super) fn exhaust_budget(&self) {
        self.bytes.store(MAX_STORE, Ordering::SeqCst);
    }

    pub(super) fn create(self: &Arc<Self>, plan: &Plan) -> Result<Log> {
        reserve(&self.count, 1, MAX_JOBS as u64)
            .map_err(|_| failed("Copy history reached its 64 receipt bound."))?;
        let name = format!("{}.receipt", plan.id);
        let mut file = self
            .directory
            .create_new(name.as_ref())
            .map_err(cap_failure)?;
        file.write_all(MAGIC)?;
        file.sync_all()?;
        self.directory.sync().map_err(cap_failure)?;
        let identity = file_id(&file)?;
        let mut log = Log {
            file,
            identity,
            valid: 8,
            store: self.clone(),
        };
        self.bytes.fetch_add(8, Ordering::SeqCst);
        log.append(&Event::Plan { plan: plan.clone() })?;
        Ok(log)
    }
    pub(super) fn resume(self: &Arc<Self>, receipt: &Receipt) -> Result<Log> {
        let name = format!("{}.receipt", receipt.plan.id);
        let mut file = self
            .directory
            .open_existing(name.as_ref())
            .map_err(cap_failure)?;
        let identity = file_id(&file)?;
        if receipt.log_id.as_ref() != Some(&identity) {
            return Err(failed(
                "The copy receipt was replaced. Preserve it for manual recovery.",
            ));
        }
        // Reparse the opened receipt: a concurrent edit cannot authorize a stale in-memory prefix.
        let (disk, valid) = read(&mut file)?;
        if serde_json::to_vec(&disk.plan)? != serde_json::to_vec(&receipt.plan)?
            || valid != receipt.valid
        {
            return Err(failed(
                "Copy history changed since it was loaded. Restart and review it.",
            ));
        }
        if disk.progress != receipt.progress
            || disk.stage_id != receipt.stage_id
            || disk.publishing != receipt.publishing
        {
            return Err(failed("Copy checkpoints changed since they were loaded."));
        }
        // Only an incomplete final frame is discarded, on the exact opened receipt.
        file.set_len(valid)?;
        file.sync_all()?;
        file.seek(SeekFrom::Start(valid))?;
        Ok(Log {
            file,
            identity,
            valid,
            store: self.clone(),
        })
    }
}
impl Log {
    pub(super) fn bind(&self, receipt: &mut Receipt) {
        receipt.log_id = Some(self.identity.clone());
        receipt.valid = self.valid;
    }
    pub(super) fn append(&mut self, event: &Event) -> Result<()> {
        let bytes = serde_json::to_vec(event)?;
        if bytes.len() > MAX_FRAME {
            return Err(failed("The copy manifest exceeds its 64 MiB bound."));
        }
        let length = bytes.len() as u64 + 36;
        if self.valid.saturating_add(length) > MAX_LOG {
            return Err(failed(
                "This copy reached its 128 MiB receipt bound. Its checkpoint is preserved.",
            ));
        }
        reserve(&self.store.bytes, length, MAX_STORE).map_err(|_| failed("Copy receipts reached their 512 MiB aggregate bound. Existing checkpoints are preserved."))?;
        self.file.set_len(self.valid)?;
        self.file.seek(SeekFrom::Start(self.valid))?;
        self.file.write_all(&(bytes.len() as u32).to_le_bytes())?;
        self.file.write_all(&Sha256::digest(&bytes))?;
        self.file.write_all(&bytes)?;
        self.file.sync_all()?;
        self.valid += length;
        Ok(())
    }
}
fn read(file: &mut std::fs::File) -> Result<(Receipt, u64)> {
    file.seek(SeekFrom::Start(0))?;
    let len = file.metadata()?.len();
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(failed(
            "Copy receipt format is invalid. Preserve it for recovery.",
        ));
    }
    let mut valid = 8u64;
    let mut receipt: Option<Receipt> = None;
    while valid < len {
        if len - valid < 36 {
            break;
        }
        let mut header = [0u8; 36];
        file.read_exact(&mut header)?;
        let size = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
        if size > MAX_FRAME {
            return Err(failed("Copy receipt frame exceeds its bound."));
        }
        if len - valid - 36 < size as u64 {
            break;
        }
        let mut bytes = vec![0; size];
        file.read_exact(&mut bytes)?;
        if Sha256::digest(&bytes).as_slice() != &header[4..] {
            return Err(failed(
                "Copy receipt checksum failed. Preserve it and its staging data.",
            ));
        }
        let event: Event = serde_json::from_slice(&bytes)?;
        match (&mut receipt, event) {
            (None, Event::Plan { plan }) => {
                plan.validate()?;
                receipt = Some(Receipt::new(plan));
            }
            (Some(r), event) => r.apply(event)?,
            _ => return Err(failed("Copy receipt event order is invalid.")),
        }
        valid += size as u64 + 36;
    }
    Ok((
        receipt.ok_or_else(|| {
            failed("Copy receipt has no complete manifest. Preserve its staging data for recovery.")
        })?,
        valid,
    ))
}
