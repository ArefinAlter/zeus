//! Durable bounded copies with identity-bound staging and explicit, verified restart resume.
use crate::{
    facts::{FileFacts, quarantined_under},
    ops::quarantine_refusal,
    paths::{RootCaps, cap_failure, shown},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};
use zeus_app::failure::{Failure, Result};
use zeus_fs::DirectoryCap;
mod receipt;
mod worker;
use receipt::{Event, Log, Store};
const CHUNK: usize = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 100_000;
const MAX_JOB_BYTES: u64 = 256 * 1024 * 1024 * 1024;
const MAX_STAGING: u64 = 1024 * 1024 * 1024 * 1024;
const VERIFY_TIME: Duration = Duration::from_secs(120);
fn failed(message: &str) -> Failure {
    Failure::new("app.copy_incomplete", message)
}
fn file_id(file: &std::fs::File) -> Result<zeus_types::FileId> {
    zeus_fs::opened_file_metadata(file)
        .map_err(cap_failure)?
        .file_id
        .ok_or_else(|| failed("A stable file identity is required."))
}
fn reserve(counter: &AtomicU64, amount: u64, maximum: u64) -> std::result::Result<(), ()> {
    let mut current = counter.load(Ordering::SeqCst);
    loop {
        let next = current
            .checked_add(amount)
            .filter(|n| *n <= maximum)
            .ok_or(())?;
        match counter.compare_exchange_weak(current, next, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return Ok(()),
            Err(actual) => current = actual,
        }
    }
}
fn valid_id(id: &str) -> bool {
    id.len() >= 8
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn utf8(path: &Path) -> Result<String> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        failed("This copy contains a filename that cannot be represented without loss.")
    })
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Stamp {
    id: zeus_types::FileId,
    size: u64,
    directory: bool,
    modified: SystemTime,
}
fn stamp(meta: zeus_fs::FileMeta, os: std::fs::Metadata) -> Result<Stamp> {
    if meta.is_symlink || (!meta.is_regular_file && !meta.is_dir) {
        return Err(failed("Copies refuse links, junctions and special files."));
    }
    Ok(Stamp {
        id: meta
            .file_id
            .ok_or_else(|| failed("A stable source identity is required."))?,
        size: if meta.is_dir { 0 } else { meta.len },
        directory: meta.is_dir,
        modified: os.modified()?,
    })
}
fn entry_stamp(parent: &DirectoryCap, name: &std::ffi::OsStr) -> Result<Stamp> {
    // Metadata observations are rechecked against the opened file before content is consumed.
    stamp(
        parent.entry_metadata(name).map_err(cap_failure)?,
        parent.entry_os_metadata(name).map_err(cap_failure)?,
    )
}
fn open_stamp(file: &std::fs::File) -> Result<Stamp> {
    stamp(
        zeus_fs::opened_file_metadata(file).map_err(cap_failure)?,
        file.metadata()?,
    )
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    relative: String,
    stamp: Stamp,
    parent_id: zeus_types::FileId,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Plan {
    id: String,
    source: String,
    destination: String,
    name: String,
    reads: Vec<(String, zeus_types::FileId)>,
    writes: Vec<(String, zeus_types::FileId)>,
    destination_id: zeus_types::FileId,
    entries: Vec<Entry>,
    bytes: u64,
}
impl Plan {
    fn validate(&self) -> Result<()> {
        if !valid_id(&self.id)
            || self.entries.is_empty()
            || self.entries.len() > MAX_ENTRIES
            || self.bytes > MAX_JOB_BYTES
        {
            return Err(failed("The copy manifest is outside its bounds."));
        }
        zeus_fs::directory::validate_filename(self.name.as_ref()).map_err(cap_failure)?;
        if !Path::new(&self.source).is_absolute() || !Path::new(&self.destination).is_absolute() {
            return Err(failed("Copy roots must be absolute."));
        }
        let mut names = std::collections::HashSet::new();
        let mut total = 0u64;
        for (i, entry) in self.entries.iter().enumerate() {
            let p = Path::new(&entry.relative);
            if (i == 0 && !entry.relative.is_empty())
                || (i > 0 && entry.relative.is_empty())
                || p.components().count() > 128
                || p.components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
                || !names.insert(entry.relative.clone())
            {
                return Err(failed("Copy manifest paths are invalid or duplicated."));
            }
            total = total
                .checked_add(entry.stamp.size)
                .ok_or_else(|| failed("Copy size overflow."))?;
        }
        if total != self.bytes {
            return Err(failed("Copy manifest size is invalid."));
        }
        Ok(())
    }
    fn target(&self) -> PathBuf {
        Path::new(&self.destination).join(&self.name)
    }
    fn stage(&self) -> String {
        format!("copy-{}", self.id)
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct Progress {
    id: Option<zeus_types::FileId>,
    chunks: Vec<String>,
    modified: Option<SystemTime>,
}
struct Receipt {
    plan: Plan,
    stage_id: Option<zeus_types::FileId>,
    progress: Vec<Progress>,
    publishing: bool,
    state: String,
    error: String,
    log_id: Option<zeus_types::FileId>,
    valid: u64,
}
impl Receipt {
    fn new(plan: Plan) -> Self {
        let count = plan.entries.len();
        Self {
            plan,
            stage_id: None,
            progress: vec![Progress::default(); count],
            publishing: false,
            state: "copying".into(),
            error: String::new(),
            log_id: None,
            valid: 0,
        }
    }
    fn apply(&mut self, event: Event) -> Result<()> {
        let invalid = || failed("Copy receipt transition is invalid. Preserve its staging data.");
        match event {
            Event::Plan { .. } => return Err(invalid()),
            Event::Stage { identity } if self.stage_id.is_none() => self.stage_id = Some(identity),
            Event::Created { index, identity } if self.stage_id.is_some() && !self.publishing => {
                let p = self.progress.get_mut(index).ok_or_else(invalid)?;
                if p.id.is_some() {
                    return Err(invalid());
                }
                p.id = Some(identity);
            }
            Event::Chunk { index, digest } if !self.publishing => {
                let entry = self.plan.entries.get(index).ok_or_else(invalid)?;
                let p = self.progress.get_mut(index).ok_or_else(invalid)?;
                if entry.stamp.directory
                    || p.id.is_none()
                    || p.modified.is_some()
                    || p.chunks.len() as u64 >= entry.stamp.size.div_ceil(CHUNK as u64)
                    || digest.len() != 64
                    || !digest.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(invalid());
                }
                p.chunks.push(digest);
            }
            Event::Done { index, modified } if !self.publishing => {
                let entry = self.plan.entries.get(index).ok_or_else(invalid)?;
                let p = self.progress.get_mut(index).ok_or_else(invalid)?;
                if p.id.is_none()
                    || p.modified.is_some()
                    || (!entry.stamp.directory
                        && p.chunks.len() as u64 != entry.stamp.size.div_ceil(CHUNK as u64))
                {
                    return Err(invalid());
                }
                p.modified = Some(modified);
            }
            Event::Publishing if self.progress.iter().all(|p| p.modified.is_some()) => {
                self.publishing = true;
                self.state = "publishing".into();
            }
            Event::Complete if self.publishing => self.state = "complete".into(),
            Event::Stopped { state, message }
                if matches!(state.as_str(), "failed" | "cancelled" | "interrupted") =>
            {
                self.state = state;
                self.error = message;
            }
            _ => return Err(invalid()),
        }
        Ok(())
    }
    fn bytes(&self) -> u64 {
        self.progress
            .iter()
            .zip(&self.plan.entries)
            .map(|(p, e)| (p.chunks.len() as u64 * CHUNK as u64).min(e.stamp.size))
            .sum()
    }
    fn snapshot(&self) -> Value {
        json!({"id":self.plan.id,"state":self.state,"source":self.plan.source,"path":shown(&self.plan.target()),"bytes":self.bytes(),"total_bytes":self.plan.bytes,"files":self.progress.iter().zip(&self.plan.entries).filter(|(p,e)| !e.stamp.directory && p.modified.is_some()).count(),"error":self.error,"staging":shown(&Path::new(&self.plan.destination).join(".zeus-partial").join(self.plan.stage())),"can_resume":matches!(self.state.as_str(),"interrupted"|"cancelled"|"failed"),"persistent":true})
    }
}
struct Job {
    receipt: Mutex<Receipt>,
    cancelled: AtomicBool,
    active: AtomicBool,
}
impl Job {
    fn check(&self, authority: &Authority) -> Result<()> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(Failure::new(
                "app.cancelled",
                "Copy stopped. Its private staging and last synced checkpoint are kept for explicit Resume.",
            ));
        }
        (authority.check)()
    }
    fn event(&self, log: &mut Log, event: Event) -> Result<()> {
        let mut receipt = self
            .receipt
            .lock()
            .map_err(|_| failed("Copy receipt is busy."))?;
        log.append(&event)?;
        receipt.apply(event)?;
        log.bind(&mut receipt);
        Ok(())
    }
}
/// Fresh scopes and quarantine facts, plus the desktop's live revocation/stop check.
pub struct Authority {
    /// Retained read roots captured against their enablement identities.
    pub reads: RootCaps,
    /// Retained writable roots captured against their enablement identities.
    pub writes: RootCaps,
    /// Current quarantine facts from the daemon.
    pub facts: Arc<HashMap<String, FileFacts>>,
    /// Called between chunks and before effects; any failure holds the copy.
    pub check: Arc<dyn Fn() -> Result<()> + Send + Sync>,
}
/// Durable copy receipts; at most four workers and 64 retained jobs, with no automatic deletion.
pub struct Transfers {
    workers: Arc<AtomicU64>,
    store: Option<Arc<Store>>,
    jobs: Mutex<BTreeMap<String, Arc<Job>>>,
    load_error: Option<String>,
}
impl Transfers {
    /// Load bounded receipts without opening source paths, resuming workers or publishing data.
    pub fn new(path: PathBuf) -> Self {
        match Store::open(&path) {
            Ok((store, rows)) => Self {
                workers: Arc::new(AtomicU64::new(0)),
                store: Some(store),
                jobs: Mutex::new(
                    rows.into_iter()
                        .map(|r| {
                            (
                                r.plan.id.clone(),
                                Arc::new(Job {
                                    receipt: Mutex::new(r),
                                    cancelled: AtomicBool::new(false),
                                    active: AtomicBool::new(false),
                                }),
                            )
                        })
                        .collect(),
                ),
                load_error: None,
            },
            Err(e) => Self {
                workers: Arc::new(AtomicU64::new(0)),
                store: None,
                jobs: Mutex::new(BTreeMap::new()),
                load_error: Some(e.message),
            },
        }
    }
    fn store(&self) -> Result<Arc<Store>> {
        self.store.clone().ok_or_else(|| {
            failed(
                self.load_error
                    .as_deref()
                    .unwrap_or("Copy history is unavailable."),
            )
        })
    }
    /// Preflight and persist a new job. Caller runs this bounded scan on a blocking worker.
    pub fn start(&self, from: String, to: String, authority: Authority) -> Result<Value> {
        let lease = self.acquire()?;
        let store = self.store()?;
        let plan = worker::plan(&from, &to, &authority)?;
        let mut jobs = self.jobs.lock().unwrap();
        if jobs.len() >= receipt::MAX_JOBS {
            return Err(failed(
                "The 64 retained copy receipts are full. No recovery history was discarded.",
            ));
        }
        self.admit(&jobs, plan.bytes)?;
        let log = store.create(&plan)?;
        let id = plan.id.clone();
        let mut receipt = Receipt::new(plan);
        log.bind(&mut receipt);
        let job = Arc::new(Job {
            receipt: Mutex::new(receipt),
            cancelled: AtomicBool::new(false),
            active: AtomicBool::new(true),
        });
        let out = job.receipt.lock().unwrap().snapshot();
        jobs.insert(id, job.clone());
        Self::launch(job, log, authority, lease);
        Ok(out)
    }
    fn admit(&self, jobs: &BTreeMap<String, Arc<Job>>, added: u64) -> Result<()> {
        if jobs
            .values()
            .filter(|j| j.active.load(Ordering::SeqCst))
            .count()
            >= 4
        {
            return Err(Failure::new(
                "app.busy",
                "Wait for a copy worker to finish or pause it.",
            ));
        }
        let reserved: u64 = jobs
            .values()
            .map(|j| {
                let r = j.receipt.lock().unwrap();
                if r.state == "complete" {
                    0
                } else {
                    r.plan.bytes
                }
            })
            .sum();
        if reserved.checked_add(added).is_none_or(|n| n > MAX_STAGING) {
            return Err(failed(
                "Unfinished copy staging reached its 1 TiB reservation limit. Existing checkpoints are kept.",
            ));
        }
        Ok(())
    }
    /// Resume only after a fresh user action; reopening does not adopt replaced roots or files.
    pub fn resume(&self, id: &str, authority: Authority) -> Result<Value> {
        let lease = self.acquire()?;
        let store = self.store()?;
        let jobs = self.jobs.lock().unwrap();
        self.admit(&jobs, 0)?;
        let job = jobs
            .get(id)
            .ok_or_else(|| Failure::invalid("That copy receipt is unavailable."))?
            .clone();
        if job.active.load(Ordering::SeqCst) {
            return Err(Failure::new("app.busy", "This copy is already running."));
        }
        let mut r = job.receipt.lock().unwrap();
        if r.state == "complete" {
            return Err(Failure::invalid("This copy is already complete."));
        }
        let log = store.resume(&r)?;
        r.state = "verifying".into();
        r.error.clear();
        let out = r.snapshot();
        drop(r);
        job.cancelled.store(false, Ordering::SeqCst);
        job.active.store(true, Ordering::SeqCst);
        Self::launch(job, log, authority, lease);
        Ok(out)
    }
    fn acquire(&self) -> Result<WorkerLease> {
        reserve(&self.workers, 1, 4).map_err(|_| {
            Failure::new(
                "app.busy",
                "Four copy workers are already scanning, verifying or copying.",
            )
        })?;
        Ok(WorkerLease(self.workers.clone()))
    }
    fn launch(job: Arc<Job>, mut log: Log, authority: Authority, lease: WorkerLease) {
        tokio::task::spawn_blocking(move || {
            let _lease = lease;
            if let Err(e) = worker::run(&job, &mut log, &authority) {
                let state = if job.cancelled.load(Ordering::SeqCst) {
                    "cancelled"
                } else {
                    "failed"
                };
                let message = format!(
                    "{} Source and existing destination data were preserved; inspect or resume the private staging copy.",
                    e.message
                );
                // A full disk may prevent this state event; the last valid checkpoint remains usable.
                if job
                    .event(
                        &mut log,
                        Event::Stopped {
                            state: state.into(),
                            message: message.clone(),
                        },
                    )
                    .is_err()
                    && let Ok(mut r) = job.receipt.lock()
                {
                    r.state = state.into();
                    r.error = message;
                }
            }
            job.active.store(false, Ordering::SeqCst);
        });
    }
    /// List durable receipts, including cancelled and interrupted jobs after restart.
    pub fn list(&self) -> Value {
        json!({"jobs":self.jobs.lock().unwrap().values().map(|j|j.receipt.lock().unwrap().snapshot()).collect::<Vec<_>>(),"load_error":self.load_error})
    }
    /// Stop active workers at their next checkpoint, preserving all receipts and staging bytes.
    pub fn cancel_all(&self) {
        for j in self.jobs.lock().unwrap().values() {
            j.cancelled.store(true, Ordering::SeqCst);
        }
    }
    /// Observe or cancel an existing receipt; neither action deletes data.
    pub fn get(&self, id: &str, cancel: bool) -> Result<Value> {
        let jobs = self.jobs.lock().unwrap();
        let j = jobs
            .get(id)
            .ok_or_else(|| Failure::invalid("That copy job is unavailable."))?;
        if cancel {
            j.cancelled.store(true, Ordering::SeqCst);
        }
        Ok(j.receipt.lock().unwrap().snapshot())
    }
}
impl Drop for Transfers {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

struct WorkerLease(Arc<AtomicU64>);
impl Drop for WorkerLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests;
