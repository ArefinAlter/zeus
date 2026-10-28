//! Native ZIP operations with reviewed manifests, bounded reads/expansion, durable receipts,
//! cancellation and no-replace publication. Archive content never launches a helper or executable.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zeus_app::failure::{Failure, Result};
use zeus_fs::{Filesystem, RealFs};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::{
    facts::{FileFacts, quarantined_under},
    paths::{RootIdentity, check_name, resolve_inside, shown},
};

const MAX_ENTRIES: usize = 10_000;
const MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_ZIP_BYTES: u64 = 1024 * 1024 * 1024;

fn failed(message: &str) -> Failure {
    Failure::new("app.archive", message)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Stamp {
    file_id: zeus_types::FileId,
    bytes: u64,
    mtime_ms: i64,
    directory: bool,
}
fn stamp(path: &Path) -> Result<Stamp> {
    let m = RealFs
        .metadata(path)
        .map_err(|_| failed("The source identity could not be read."))?
        .ok_or_else(|| failed("The source is missing."))?;
    if m.is_symlink || (!m.is_regular_file && !m.is_dir) {
        return Err(failed(
            "Archives do not follow links, junctions or special files.",
        ));
    }
    Ok(Stamp {file_id:m.file_id.ok_or_else(||failed("This filesystem cannot provide the stable identities needed for archive operations."))?,bytes:if m.is_dir{0}else{m.len},mtime_ms:m.mtime_ms,directory:m.is_dir})
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    name: String,
    stamp: Option<Stamp>,
    bytes: u64,
    directory: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Plan {
    id: String,
    kind: String,
    source: String,
    source_stamp: Stamp,
    target: String,
    entries: Vec<Entry>,
    bytes: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Receipt {
    plan: Plan,
    state: String,
    error: String,
    staging: Option<String>,
    staging_id: Option<zeus_types::FileId>,
    sha256: Option<String>,
}
struct Job {
    receipt: Mutex<Receipt>,
    bytes: AtomicU64,
    files: AtomicU64,
    cancel: AtomicBool,
}
impl Job {
    fn check(&self, start: Instant) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(Failure::new(
                "app.cancelled",
                "Archive operation cancelled. Published source files are unchanged.",
            ));
        }
        if start.elapsed() > Duration::from_secs(600) {
            return Err(failed(
                "The archive operation exceeded its ten-minute execution limit.",
            ));
        }
        Ok(())
    }
    fn snapshot(&self) -> Value {
        let r = self.receipt.lock().unwrap();
        let complete = r.state == "complete";
        let bytes = if complete {
            r.plan.bytes
        } else {
            self.bytes.load(Ordering::Relaxed)
        };
        let entries = if complete {
            r.plan.entries.len() as u64
        } else {
            self.files.load(Ordering::Relaxed)
        };
        json!({"id":r.plan.id,"kind":r.plan.kind,"state":r.state,"source":r.plan.source,"target":r.plan.target,"total_bytes":r.plan.bytes,"bytes":bytes,"total_entries":r.plan.entries.len(),"entries":entries,"error":r.error,"staging":r.staging,"sha256":r.sha256,"digest_kind":if r.plan.kind=="create"{"zip_bytes_sha256"}else{"tree_manifest_sha256"}})
    }
}

/// Native archive-manifest input supplied by the person's file-manager action.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prepare {
    /// Create a ZIP or extract an ordinary ZIP.
    pub kind: String,
    /// Existing source in an included read root.
    pub source: String,
    /// Existing destination in an explicitly writable root.
    pub directory: String,
    /// New archive filename or destination folder name.
    pub name: String,
}

/// Durable, bounded native archive jobs, separate from engine download and upload lifecycles.
pub struct Archives {
    directory: PathBuf,
    jobs: Mutex<BTreeMap<String, Arc<Job>>>,
}
impl Archives {
    /// Restore at most 32 local receipts; interrupted operations never auto-publish on restart.
    pub fn new(directory: PathBuf) -> Self {
        let mut jobs = BTreeMap::new();
        if let Ok(items) = std::fs::read_dir(&directory) {
            for item in items.flatten().take(64) {
                let path = item.path();
                if path.extension().is_none_or(|x| x != "json")
                    || item.metadata().map_or(true, |m| m.len() > 16 * 1024 * 1024)
                {
                    continue;
                }
                if let Ok(bytes) = std::fs::read(&path)
                    && let Ok(mut receipt) = serde_json::from_slice::<Receipt>(&bytes)
                {
                    let id = receipt.plan.id.clone();
                    if !valid_id(&id)
                        || path.file_stem().is_none_or(|x| x != id.as_str())
                        || receipt.plan.entries.len() > MAX_ENTRIES
                    {
                        continue;
                    }
                    if matches!(receipt.state.as_str(), "running" | "publishing") {
                        receipt.state = "interrupted".into();
                        receipt.error="Zeus stopped during this operation. Review or clean its private staging output before restarting.".into();
                    }
                    jobs.insert(
                        id,
                        Arc::new(Job {
                            receipt: Mutex::new(receipt),
                            bytes: AtomicU64::new(0),
                            files: AtomicU64::new(0),
                            cancel: AtomicBool::new(false),
                        }),
                    );
                    if jobs.len() == 32 {
                        break;
                    }
                }
            }
        }
        Self {
            directory,
            jobs: Mutex::new(jobs),
        }
    }
    fn save(&self, receipt: &Receipt) -> Result<()> {
        std::fs::create_dir_all(&self.directory)?;
        let bytes = serde_json::to_vec(receipt)?;
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(failed(
                "The archive manifest exceeds its 16 MiB receipt bound.",
            ));
        }
        zeus_app::engine_config::write_atomic(
            &self.directory.join(format!("{}.json", receipt.plan.id)),
            &bytes,
        )?;
        Ok(())
    }
    /// List bounded local archive receipts and progress.
    pub fn list(&self) -> Value {
        json!({"jobs":self.jobs.lock().unwrap().values().map(|j|j.snapshot()).collect::<Vec<_>>()})
    }
    /// Preview an exact bounded manifest, persist it and return an opaque plan identifier.
    pub fn prepare(
        &self,
        request: &Prepare,
        reads: &[String],
        writes: &[String],
        facts: &HashMap<String, FileFacts>,
    ) -> Result<Value> {
        let kind = request.kind.as_str();
        let source = request.source.as_str();
        let directory = request.directory.as_str();
        let name = request.name.as_str();
        check_name(name)?;
        if !["create", "extract"].contains(&kind) {
            return Err(failed("Choose Create ZIP or Extract ZIP."));
        }
        if kind == "create" && !name.to_lowercase().ends_with(".zip") {
            return Err(failed("A ZIP archive name must end in .zip."));
        }
        let source = resolve_inside(source, reads)?;
        if quarantined_under(facts, &shown(&source)) {
            return Err(crate::ops::quarantine_refusal("This item"));
        }
        let directory = resolve_inside(directory, writes)?;
        if !directory.is_dir() || directory.starts_with(&source) {
            return Err(failed(
                "Choose an existing writable destination outside the source.",
            ));
        }
        let target = directory.join(name);
        if std::fs::symlink_metadata(&target).is_ok() {
            return Err(Failure::new(
                "app.exists",
                "The destination exists. Archives never replace it.",
            ));
        }
        let source_stamp = stamp(&source)?;
        let (entries, bytes) = if kind == "create" {
            manifest(&source, reads, facts)?
        } else {
            zip_manifest(&source)?
        };
        let plan = Plan {
            id: format!("farc_{}", zeus_app::review::random_id()),
            kind: kind.into(),
            source: shown(&source),
            source_stamp,
            target: shown(&target),
            entries,
            bytes,
        };
        let out = json!({"id":plan.id,"kind":plan.kind,"source":plan.source,"target":plan.target,"total_bytes":plan.bytes,"total_entries":plan.entries.len(),"entries":plan.entries.iter().take(100).map(|e|json!({"name":e.name,"bytes":e.bytes,"directory":e.directory})).collect::<Vec<_>>(),"truncated":plan.entries.len()>100,"state":"ready"});
        let receipt = Receipt {
            plan,
            state: "ready".into(),
            error: String::new(),
            staging: None,
            staging_id: None,
            sha256: None,
        };
        let mut jobs = self.jobs.lock().unwrap();
        if jobs.len() >= 32 {
            return Err(failed(
                "Archive history contains 32 receipts. Dismiss a completed job before preparing another.",
            ));
        }
        self.save(&receipt)?;
        jobs.insert(
            receipt.plan.id.clone(),
            Arc::new(Job {
                receipt: Mutex::new(receipt),
                bytes: AtomicU64::new(0),
                files: AtomicU64::new(0),
                cancel: AtomicBool::new(false),
            }),
        );
        Ok(out)
    }
    /// Start exactly the prepared manifest after rechecking current scopes and quarantine facts.
    pub fn start(
        self: &Arc<Self>,
        id: &str,
        reads: Vec<String>,
        writes: Vec<String>,
        facts: Arc<HashMap<String, FileFacts>>,
    ) -> Result<Value> {
        let job = {
            let jobs = self.jobs.lock().unwrap();
            let job = jobs
                .get(id)
                .cloned()
                .ok_or_else(|| failed("That archive plan is unavailable."))?;
            if jobs
                .values()
                .filter(|j| {
                    matches!(
                        j.receipt.lock().unwrap().state.as_str(),
                        "running" | "publishing"
                    )
                })
                .count()
                >= 2
            {
                return Err(failed("Wait for an archive job to finish or cancel it."));
            }
            {
                let mut r = job.receipt.lock().unwrap();
                if !["ready", "failed", "cancelled", "interrupted"].contains(&r.state.as_str())
                    || r.staging.is_some()
                {
                    return Err(failed(
                        "Review this receipt and clean its private staging output before restarting.",
                    ));
                }
                r.state = "running".into();
                r.error.clear();
                self.save(&r)?;
            }
            job
        };
        job.cancel.store(false, Ordering::Relaxed);
        job.bytes.store(0, Ordering::Relaxed);
        job.files.store(0, Ordering::Relaxed);
        let manager = self.clone();
        let out = job.snapshot();
        tokio::task::spawn_blocking(move || {
            let result = execute(&manager, &job, &reads, &writes, &facts);
            let mut r = job.receipt.lock().unwrap();
            match result {
                Ok(digest) => {
                    r.state = "complete".into();
                    r.staging = None;
                    r.sha256 = Some(digest);
                    r.error.clear();
                }
                Err(e) => {
                    r.state = if r.state == "publishing" {
                        "uncertain"
                    } else if job.cancel.load(Ordering::Relaxed) {
                        "cancelled"
                    } else {
                        "failed"
                    }
                    .into();
                    r.error = e.message;
                }
            }
            if manager.save(&r).is_err() {
                r.state = "uncertain".into();
                r.error =
                    "The result receipt could not be saved. Review its target before retrying."
                        .into();
            }
        });
        Ok(out)
    }
    /// Cancel a running local archive job; publication checks cancellation too.
    pub fn cancel(&self, id: &str) -> Result<Value> {
        let jobs = self.jobs.lock().unwrap();
        let job = jobs.get(id).ok_or_else(|| failed("Unknown archive job."))?;
        job.cancel.store(true, Ordering::Relaxed);
        Ok(job.snapshot())
    }
    /// Remove only a recorded private staging object whose stable identity still matches.
    pub fn cleanup(&self, id: &str, writes: &[String]) -> Result<Value> {
        let job = self
            .jobs
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| failed("Unknown archive job."))?;
        let mut r = job.receipt.lock().unwrap();
        if matches!(r.state.as_str(), "running" | "publishing") {
            return Err(failed("Cancel the running job and wait before cleanup."));
        }
        if let Some(stage) = r.staging.as_ref() {
            if std::fs::symlink_metadata(stage)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                if std::fs::symlink_metadata(&r.plan.target).is_ok() {
                    return Err(failed(
                        "The target exists and staging is absent. Reconcile publication before cleanup.",
                    ));
                }
                r.staging = None;
                r.staging_id = None;
                if r.state == "uncertain" {
                    r.state = "failed".into();
                    r.sha256 = None;
                }
                self.save(&r)?;
                return Ok(json!({"ok":true}));
            }
            let p = resolve_inside(stage, writes)?;
            let m = RealFs
                .metadata(&p)
                .map_err(|_| failed("The staging identity could not be checked."))?
                .ok_or_else(|| failed("The staging output is missing; review the receipt."))?;
            if m.file_id != r.staging_id
                || m.is_symlink
                || !p.file_name().is_some_and(|n| {
                    n.to_string_lossy()
                        .starts_with(&format!(".zeus-archive-{}-", r.plan.id))
                })
            {
                return Err(failed(
                    "The staging identity changed; automatic cleanup was refused.",
                ));
            }
            if m.is_dir {
                std::fs::remove_dir_all(&p)?;
            } else {
                std::fs::remove_file(&p)?;
            }
            r.staging = None;
            r.staging_id = None;
            if r.state == "uncertain"
                && std::fs::symlink_metadata(&r.plan.target)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                r.state = "failed".into();
                r.sha256 = None;
            }
            self.save(&r)?;
        }
        Ok(json!({"ok":true}))
    }
    /// Reconcile a crash at publication using the retained staging identity and content digest.
    pub fn reconcile(
        &self,
        id: &str,
        writes: &[String],
        facts: &HashMap<String, FileFacts>,
    ) -> Result<Value> {
        let job = self
            .jobs
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| failed("Unknown archive job."))?;
        let mut r = job.receipt.lock().unwrap();
        if !["uncertain", "interrupted", "failed"].contains(&r.state.as_str()) {
            return Err(failed(
                "This archive receipt does not need publication reconciliation.",
            ));
        }
        let target = resolve_inside(&r.plan.target, writes)?;
        if quarantined_under(facts, &shown(&target)) {
            return Err(crate::ops::quarantine_refusal("This output"));
        }
        let actual = stamp(&target)?;
        if Some(&actual.file_id) != r.staging_id.as_ref() || r.sha256.is_none() {
            return Err(failed(
                "The output identity is not established by this receipt; review the target manually.",
            ));
        }
        if r.staging
            .as_ref()
            .is_some_and(|p| std::fs::symlink_metadata(p).is_ok())
        {
            return Err(failed(
                "Both staging and target exist; automatic reconciliation is ambiguous.",
            ));
        }
        let digest = published_digest(&r.plan, &target)?;
        if r.sha256.as_deref() != Some(digest.as_str()) {
            return Err(failed(
                "The published output content changed; completion was not asserted.",
            ));
        }
        r.state = "complete".into();
        r.staging = None;
        r.error.clear();
        self.save(&r)?;
        Ok(json!({"ok":true,"sha256":digest}))
    }
    /// Dismiss a completed or unused receipt; uncertain staging is retained for recovery.
    pub fn dismiss(&self, id: &str) -> Result<Value> {
        let mut jobs = self.jobs.lock().unwrap();
        let job = jobs.get(id).ok_or_else(|| failed("Unknown archive job."))?;
        let r = job.receipt.lock().unwrap();
        if matches!(r.state.as_str(), "running" | "publishing")
            || r.staging.is_some()
            || r.state == "uncertain"
        {
            return Err(failed(
                "Resolve this operation's receipt before dismissing it.",
            ));
        }
        std::fs::remove_file(self.directory.join(format!("{id}.json")))?;
        drop(r);
        jobs.remove(id);
        Ok(json!({"ok":true}))
    }
    /// Request cancellation of every running archive job on app shutdown.
    pub fn cancel_all(&self) {
        for job in self.jobs.lock().unwrap().values() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
}
fn valid_id(id: &str) -> bool {
    id.strip_prefix("farc_")
        .is_some_and(|x| x.len() == 32 && x.bytes().all(|b| b.is_ascii_hexdigit()))
}
fn relative_name(name: &str, directory: bool) -> Result<PathBuf> {
    let value = name.trim_end_matches('/');
    if value.is_empty() || name.starts_with(['/', '\\']) || name.contains('\\') {
        return Err(failed("ZIP entries must use ordinary relative paths."));
    }
    let mut p = PathBuf::new();
    for part in value.split('/') {
        check_name(part)?;
        p.push(part);
    }
    if p.components().count() > 128 || (!directory && name.ends_with('/')) {
        return Err(failed(
            "ZIP entry paths are too deep or have conflicting types.",
        ));
    }
    Ok(p)
}
fn manifest(
    source: &Path,
    reads: &[String],
    facts: &HashMap<String, FileFacts>,
) -> Result<(Vec<Entry>, u64)> {
    let base = source
        .parent()
        .ok_or_else(|| failed("Choose a named file or folder, rather than a volume root."))?;
    let mut pending = vec![source.to_path_buf()];
    let mut entries = Vec::new();
    let mut bytes = 0u64;
    let mut names = HashSet::new();
    let mut name_bytes = 0usize;
    while let Some(path) = pending.pop() {
        if entries.len() + pending.len() >= MAX_ENTRIES {
            return Err(failed("Archive at most 10,000 entries at a time."));
        }
        if resolve_inside(&shown(&path), reads)? != path || quarantined_under(facts, &shown(&path))
        {
            return Err(failed(
                "A source is outside current read scopes or quarantined.",
            ));
        }
        let st = stamp(&path)?;
        let name = path
            .strip_prefix(base)
            .map_err(|_| failed("The archive source changed."))?
            .components()
            .map(|x| x.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        relative_name(&name, st.directory)?;
        name_bytes = name_bytes.saturating_add(name.len());
        if name_bytes > 8 * 1024 * 1024 {
            return Err(failed("Archive member paths exceed their 8 MiB bound."));
        }
        if !names.insert(name.to_lowercase()) {
            return Err(failed(
                "Source names collide when compared without case; choose a portable subset.",
            ));
        }
        bytes = bytes
            .checked_add(st.bytes)
            .ok_or_else(|| failed("The archive size exceeds its limit."))?;
        if bytes > MAX_BYTES {
            return Err(failed(
                "Archive at most 4 GiB of source data per operation.",
            ));
        }
        if st.directory {
            for item in std::fs::read_dir(&path)? {
                pending.push(item?.path());
                if pending.len() + entries.len() >= MAX_ENTRIES {
                    return Err(failed("Archive at most 10,000 entries."));
                }
            }
        }
        entries.push(Entry {
            name,
            bytes: st.bytes,
            directory: st.directory,
            stamp: Some(st),
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok((entries, bytes))
}
fn validate_zip_container(path: &Path) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    let mut input = std::fs::File::open(path)?;
    let size = input.metadata()?.len();
    if !(22..=MAX_ZIP_BYTES).contains(&size) {
        return Err(failed("Inspect ordinary ZIP files up to 1 GiB."));
    }
    let mut magic = [0u8; 4];
    input.read_exact(&mut magic)?;
    if ![b"PK\x03\x04", b"PK\x05\x06"].contains(&&magic) {
        return Err(failed(
            "Self-extracting and prefixed ZIP containers are unsupported.",
        ));
    }
    let count = (size.min(65557)) as usize;
    input.seek(SeekFrom::End(-(count as i64)))?;
    let mut tail = vec![0u8; count];
    input.read_exact(&mut tail)?;
    let at = (0..=count - 22)
        .rev()
        .find(|at| {
            &tail[*at..*at + 4] == b"PK\x05\x06"
                && *at + 22 + u16::from_le_bytes([tail[*at + 20], tail[*at + 21]]) as usize == count
        })
        .ok_or_else(|| failed("The ZIP directory footer is invalid."))?;
    let u16_at = |i| u16::from_le_bytes([tail[at + i], tail[at + i + 1]]);
    let u32_at = |i| {
        u32::from_le_bytes([
            tail[at + i],
            tail[at + i + 1],
            tail[at + i + 2],
            tail[at + i + 3],
        ])
    };
    if u16_at(4) != 0
        || u16_at(6) != 0
        || u16_at(8) != u16_at(10)
        || usize::from(u16_at(10)) > MAX_ENTRIES
        || u32_at(12) > 16 * 1024 * 1024
        || u64::from(u32_at(12)) + u64::from(u32_at(16)) > size
    {
        return Err(failed(
            "Split/ZIP64 archives or directories above 10,000 entries/16 MiB are unsupported.",
        ));
    }
    Ok(())
}
fn zip_manifest(source: &Path) -> Result<(Vec<Entry>, u64)> {
    if std::fs::metadata(source)?.len() > MAX_ZIP_BYTES {
        return Err(failed(
            "Inspect ZIP inputs up to 1 GiB. Expanded data is limited to 4 GiB.",
        ));
    }
    validate_zip_container(source)?;
    let mut zip = ZipArchive::new(std::fs::File::open(source)?)
        .map_err(|_| failed("This is not a supported ZIP archive."))?;
    if zip.len() > MAX_ENTRIES {
        return Err(failed("Extract at most 10,000 ZIP entries."));
    }
    let mut names = HashSet::new();
    let mut entries = Vec::new();
    let mut bytes = 0u64;
    let mut name_bytes = 0usize;
    for i in 0..zip.len() {
        let item = zip.by_index(i).map_err(|_| {
            failed("A ZIP entry could not be inspected; encrypted ZIPs are unavailable.")
        })?;
        let mode = item.unix_mode().map(|x| x & 0o170000).unwrap_or(0);
        if item.encrypted()
            || item.is_symlink()
            || ![0, 0o040000, 0o100000].contains(&mode)
            || ![CompressionMethod::Stored, CompressionMethod::Deflated]
                .contains(&item.compression())
        {
            return Err(failed(
                "Extract ordinary Stored/Deflate ZIP files only; encryption, links and special files are unsupported.",
            ));
        }
        let name = item
            .name()
            .map_err(|_| failed("The ZIP entry name could not be decoded."))?
            .trim_end_matches('/')
            .to_owned();
        relative_name(&name, item.is_dir())?;
        name_bytes = name_bytes.saturating_add(name.len());
        if name_bytes > 8 * 1024 * 1024 {
            return Err(failed("ZIP member paths exceed their 8 MiB bound."));
        }
        if item.is_dir() && item.size() != 0
            || mode == 0o040000 && !item.is_dir()
            || mode == 0o100000 && item.is_dir()
        {
            return Err(failed(
                "ZIP entry types or directory content lengths are inconsistent.",
            ));
        }
        if !names.insert(name.to_lowercase()) {
            return Err(failed(
                "ZIP entry names collide, including case-only collisions.",
            ));
        }
        bytes = bytes
            .checked_add(item.size())
            .ok_or_else(|| failed("Expanded size exceeds the limit."))?;
        if bytes > MAX_BYTES
            || (item.size() > 1024 * 1024
                && item.size() > item.compressed_size().max(1).saturating_mul(1000))
        {
            return Err(failed(
                "ZIP expansion exceeds 4 GiB or the supported compression ratio.",
            ));
        }
        entries.push(Entry {
            name,
            stamp: None,
            bytes: item.size(),
            directory: item.is_dir(),
        });
    }
    expanded_paths(&entries)?;
    // A regular file must never be the parent of another entry.
    let files: HashSet<_> = entries
        .iter()
        .filter(|x| !x.directory)
        .map(|x| x.name.to_lowercase())
        .collect();
    for e in &entries {
        let p = relative_name(&e.name, e.directory)?;
        let mut parent = p.parent();
        while let Some(p) = parent {
            if files.contains(
                &p.components()
                    .map(|x| x.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/")
                    .to_lowercase(),
            ) {
                return Err(failed(
                    "A ZIP file conflicts with an entry's parent folder.",
                ));
            }
            parent = p.parent();
        }
    }
    Ok((entries, bytes))
}
fn expanded_paths(entries: &[Entry]) -> Result<HashMap<PathBuf, bool>> {
    let mut paths = HashMap::new();
    for e in entries {
        let path = relative_name(&e.name, e.directory)?;
        if paths
            .insert(path.clone(), e.directory)
            .is_some_and(|directory| directory != e.directory)
        {
            return Err(failed(
                "A ZIP file conflicts with an implicit parent directory.",
            ));
        }
        let mut parent = path.parent();
        while let Some(p) = parent {
            if p.as_os_str().is_empty() {
                break;
            }
            if paths.insert(p.to_path_buf(), true) == Some(false) {
                return Err(failed(
                    "A ZIP file conflicts with an implicit parent directory.",
                ));
            }
            parent = p.parent();
        }
        if paths.len() > MAX_ENTRIES {
            return Err(failed(
                "Expanded ZIP files and implicit directories exceed 10,000 entries.",
            ));
        }
    }
    Ok(paths)
}
fn published_digest(plan: &Plan, target: &Path) -> Result<String> {
    let started = Instant::now();
    let before = stamp(target)?;
    if plan.kind == "create" {
        if before.directory {
            return Err(failed("The published ZIP changed type."));
        }
        let mut input = std::fs::File::open(target)?;
        let mut hash = Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            if started.elapsed() > Duration::from_secs(600) {
                return Err(failed("Reconciliation exceeded ten minutes."));
            }
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            bytes += n as u64;
            if bytes > MAX_BYTES + 512 * 1024 * 1024 {
                return Err(failed("The published ZIP exceeded its expected bound."));
            }
            hash.update(&buffer[..n]);
        }
        if bytes != before.bytes || stamp(target)? != before {
            return Err(failed("The published ZIP changed during reconciliation."));
        }
        return Ok(hex::encode(hash.finalize()));
    }
    if !before.directory {
        return Err(failed("The published output changed type."));
    }
    let expected = expanded_paths(&plan.entries)?;
    let mut pending = vec![target.to_path_buf()];
    let mut actual = 0usize;
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            let relative = path
                .strip_prefix(target)
                .map_err(|_| failed("The published tree escaped its root."))?;
            let meta = stamp(&path)?;
            if expected.get(relative) != Some(&meta.directory) {
                return Err(failed(
                    "The published tree contains an unexpected or changed item.",
                ));
            }
            actual += 1;
            if actual > MAX_ENTRIES {
                return Err(failed("The published tree exceeded its entry bound."));
            }
            if meta.directory {
                pending.push(path);
            }
        }
    }
    if actual != expected.len() {
        return Err(failed("The published tree is incomplete."));
    }
    let mut hash = Sha256::new();
    for entry in plan.entries.iter().filter(|e| !e.directory) {
        let path = target.join(relative_name(&entry.name, false)?);
        let file_before = stamp(&path)?;
        if file_before.directory || file_before.bytes != entry.bytes {
            return Err(failed("A published file changed type or length."));
        }
        let mut input = std::fs::File::open(&path)?;
        let mut file_hash = Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            if started.elapsed() > Duration::from_secs(600) {
                return Err(failed("Reconciliation exceeded ten minutes."));
            }
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            bytes += n as u64;
            if bytes > entry.bytes {
                return Err(failed("A published file grew during reconciliation."));
            }
            file_hash.update(&buffer[..n]);
        }
        if bytes != entry.bytes || stamp(&path)? != file_before {
            return Err(failed("A published file changed during reconciliation."));
        }
        hash.update((entry.name.len() as u64).to_le_bytes());
        hash.update(entry.name.as_bytes());
        hash.update(entry.bytes.to_le_bytes());
        hash.update(file_hash.finalize());
    }
    if stamp(target)? != before {
        return Err(failed(
            "The published directory changed during reconciliation.",
        ));
    }
    Ok(hex::encode(hash.finalize()))
}
fn validate_plan(
    plan: &Plan,
    reads: &[String],
    writes: &[String],
    facts: &HashMap<String, FileFacts>,
) -> Result<()> {
    if !valid_id(&plan.id)
        || !["create", "extract"].contains(&plan.kind.as_str())
        || plan.entries.len() > MAX_ENTRIES
        || plan.bytes > MAX_BYTES
    {
        return Err(failed("The saved archive plan is invalid."));
    }
    let source = resolve_inside(&plan.source, reads)?;
    if stamp(&source)? != plan.source_stamp {
        return Err(failed(
            "The source changed after preview. Prepare a new manifest.",
        ));
    }
    if quarantined_under(facts, &shown(&source)) {
        return Err(crate::ops::quarantine_refusal("This item"));
    }
    let target = Path::new(&plan.target);
    let parent = target
        .parent()
        .ok_or_else(|| failed("The archive destination is invalid."))?;
    if resolve_inside(&shown(parent), writes)? != parent || parent.starts_with(&source) {
        return Err(failed(
            "The destination is outside current writable roots or inside the source.",
        ));
    }
    check_name(
        target
            .file_name()
            .ok_or_else(|| failed("The destination has no name."))?
            .to_str()
            .ok_or_else(|| failed("The destination name is not valid text."))?,
    )?;
    if std::fs::symlink_metadata(target).is_ok() {
        return Err(Failure::new(
            "app.exists",
            "The destination already exists. No files were replaced.",
        ));
    }
    let (current, bytes) = if plan.kind == "create" {
        manifest(&source, reads, facts)?
    } else {
        zip_manifest(&source)?
    };
    if bytes != plan.bytes || serde_json::to_vec(&current)? != serde_json::to_vec(&plan.entries)? {
        return Err(failed(
            "The source manifest changed. Preview again before starting.",
        ));
    }
    Ok(())
}
fn execute(
    manager: &Archives,
    job: &Job,
    reads: &[String],
    writes: &[String],
    facts: &HashMap<String, FileFacts>,
) -> Result<String> {
    let started = Instant::now();
    let plan = job.receipt.lock().unwrap().plan.clone();
    validate_plan(&plan, reads, writes, facts)?;
    let target = Path::new(&plan.target);
    let parent = target
        .parent()
        .ok_or_else(|| failed("The target has no parent."))?;
    let parent_pin = RootIdentity::capture(parent)
        .ok_or_else(|| failed("The destination parent cannot be pinned."))?;
    let stage = parent.join(format!(
        ".zeus-archive-{}-{}",
        plan.id,
        zeus_app::review::random_id()
    ));
    {
        let mut r = job.receipt.lock().unwrap();
        r.staging = Some(shown(&stage));
        manager.save(&r)?;
    }
    if !parent_pin.current() {
        return Err(failed("The destination parent changed before staging."));
    }
    if plan.kind == "create" {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage)?;
    } else {
        std::fs::create_dir(&stage)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    {
        let mut r = job.receipt.lock().unwrap();
        r.staging_id = Some(stamp(&stage)?.file_id);
        manager.save(&r)?;
    }
    let digest = if plan.kind == "create" {
        create_zip(&plan, &stage, job, started, reads)?
    } else {
        extract_zip(&plan, &stage, job, started, reads)?
    };
    job.check(started)?;
    if !parent_pin.current() || resolve_inside(&shown(parent), writes)? != parent {
        return Err(failed("The destination changed before publication."));
    }
    {
        let mut r = job.receipt.lock().unwrap();
        r.state = "publishing".into();
        r.sha256 = Some(digest.clone());
        manager.save(&r)?;
    }
    job.check(started)?;
    if !parent_pin.current() {
        return Err(failed("The destination changed before publication."));
    }
    crate::ops::move_noreplace(&stage, target)?;
    if let Some(parent) = target.parent() {
        RealFs.sync_dir(parent).map_err(|_|failed("The output was published but directory durability could not be confirmed. Review the target."))?;
    }
    Ok(digest)
}
fn create_zip(
    plan: &Plan,
    stage: &Path,
    job: &Job,
    start: Instant,
    reads: &[String],
) -> Result<String> {
    let source = Path::new(&plan.source);
    let base = source
        .parent()
        .ok_or_else(|| failed("The source has no parent."))?;
    let mut output = ZipWriter::new(std::fs::OpenOptions::new().write(true).open(stage)?);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);
    for entry in &plan.entries {
        job.check(start)?;
        let relative = relative_name(&entry.name, entry.directory)?;
        let path = base.join(relative);
        if resolve_inside(&shown(&path), reads)? != path || Some(stamp(&path)?) != entry.stamp {
            return Err(failed("An archive source changed after preview."));
        }
        if entry.directory {
            output
                .add_directory(format!("{}/", entry.name), options.unix_permissions(0o755))
                .map_err(|_| failed("A ZIP directory could not be written."))?;
        } else {
            output
                .start_file(&entry.name, options)
                .map_err(|_| failed("A ZIP file header could not be written."))?;
            let mut input = std::fs::File::open(&path)?;
            let mut count = 0u64;
            let mut buffer = [0u8; 65536];
            loop {
                job.check(start)?;
                let n = input.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                count += n as u64;
                if count > entry.bytes {
                    return Err(failed("A source grew while creating the ZIP."));
                }
                output.write_all(&buffer[..n])?;
                job.bytes.fetch_add(n as u64, Ordering::Relaxed);
            }
            if count != entry.bytes || Some(stamp(&path)?) != entry.stamp {
                return Err(failed("A source changed while creating the ZIP."));
            }
        }
        job.files.fetch_add(1, Ordering::Relaxed);
    }
    let file = output
        .finish()
        .map_err(|_| failed("The ZIP archive could not be finalized."))?;
    file.sync_all()?;
    let mut input = std::fs::File::open(stage)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        job.check(start)?;
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}
fn extract_zip(
    plan: &Plan,
    stage: &Path,
    job: &Job,
    start: Instant,
    reads: &[String],
) -> Result<String> {
    let source = resolve_inside(&plan.source, reads)?;
    let mut zip = ZipArchive::new(std::fs::File::open(&source)?)
        .map_err(|_| failed("The ZIP source is invalid."))?;
    let stage_pin = RootIdentity::capture(stage)
        .ok_or_else(|| failed("The staging directory cannot be pinned."))?;
    let stage_root = zeus_fs::SafeRoot::open(stage)
        .map_err(|_| failed("The staging directory cannot be scoped."))?;
    let mut tree_hash = Sha256::new();
    for (i, entry) in plan.entries.iter().enumerate() {
        job.check(start)?;
        let relative = relative_name(&entry.name, entry.directory)?;
        let to = stage.join(relative);
        if !stage_pin.current() {
            return Err(failed("The staging directory changed."));
        }
        if crate::config::norm(&shown(
            &stage_root
                .check_existing(&to)
                .map_err(|_| failed("A ZIP destination escaped private staging."))?,
        )) != crate::config::norm(&shown(&to))
        {
            return Err(failed("A ZIP destination changed."));
        }
        if entry.directory {
            std::fs::create_dir_all(&to)?;
        } else {
            std::fs::create_dir_all(
                to.parent()
                    .ok_or_else(|| failed("A ZIP entry has no parent."))?,
            )?;
            if crate::config::norm(&shown(
                &stage_root
                    .check_existing(&to)
                    .map_err(|_| failed("A ZIP destination escaped private staging."))?,
            )) != crate::config::norm(&shown(&to))
            {
                return Err(failed("A ZIP destination changed."));
            }
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&to)?;
            let mut item = zip
                .by_index(i)
                .map_err(|_| failed("A ZIP entry could not be opened."))?;
            if item
                .name()
                .map_err(|_| failed("The ZIP entry name could not be decoded."))?
                .trim_end_matches('/')
                != entry.name
                || item.size() != entry.bytes
            {
                return Err(failed("A ZIP entry changed after preview."));
            }
            let mut count = 0u64;
            let mut buffer = [0u8; 65536];
            let mut hash = Sha256::new();
            loop {
                job.check(start)?;
                let n = item
                    .read(&mut buffer)
                    .map_err(|_| failed("A ZIP entry failed decompression or CRC verification."))?;
                if n == 0 {
                    break;
                }
                count += n as u64;
                if count > entry.bytes {
                    return Err(failed("A ZIP entry exceeded its declared expanded length."));
                }
                output.write_all(&buffer[..n])?;
                hash.update(&buffer[..n]);
                job.bytes.fetch_add(n as u64, Ordering::Relaxed);
            }
            if count != entry.bytes {
                return Err(failed("A ZIP entry ended early."));
            }
            output.sync_all()?;
            tree_hash.update((entry.name.len() as u64).to_le_bytes());
            tree_hash.update(entry.name.as_bytes());
            tree_hash.update(entry.bytes.to_le_bytes());
            tree_hash.update(hash.finalize());
        }
        job.files.fetch_add(1, Ordering::Relaxed);
    }
    if stamp(&source)? != plan.source_stamp {
        return Err(failed("The ZIP source changed while extracting."));
    }
    Ok(hex::encode(tree_hash.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archive_paths_reject_traversal_devices_ads_and_case_collisions() {
        for name in [
            "../escape",
            "/abs",
            "a/../../b",
            "a\\b",
            "a:b",
            "CON",
            "a//b",
            "a/./b",
            "a/NUL.txt",
        ] {
            assert!(relative_name(name, false).is_err(), "{name}");
        }
        assert_eq!(
            relative_name("plain/folder/file.txt", false)
                .unwrap()
                .components()
                .count(),
            3
        );
    }
    #[tokio::test]
    async fn create_extract_rejects_collisions_and_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let folder = root.join("source");
        std::fs::create_dir_all(folder.join("nested")).unwrap();
        std::fs::write(folder.join("nested/data.txt"), b"content").unwrap();
        let scopes = vec![shown(&root)];
        let state = dir.path().join("jobs");
        let manager = Arc::new(Archives::new(state.clone()));
        let facts = Arc::new(HashMap::new());
        let plan = manager
            .prepare(
                &Prepare {
                    kind: "create".into(),
                    source: shown(&folder),
                    directory: shown(&root),
                    name: "bundle.zip".into(),
                },
                &scopes,
                &scopes,
                &facts,
            )
            .unwrap();
        let id = plan["id"].as_str().unwrap();
        manager
            .start(id, scopes.clone(), scopes.clone(), facts.clone())
            .unwrap();
        for _ in 0..200 {
            if !matches!(
                manager.jobs.lock().unwrap()[id]
                    .receipt
                    .lock()
                    .unwrap()
                    .state
                    .as_str(),
                "running" | "publishing"
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            manager.jobs.lock().unwrap()[id]
                .receipt
                .lock()
                .unwrap()
                .state,
            "complete",
            "{}",
            manager.list()
        );
        assert!(root.join("bundle.zip").is_file());
        assert_eq!(
            std::fs::read(folder.join("nested/data.txt")).unwrap(),
            b"content"
        );
        drop(manager);
        let manager = Arc::new(Archives::new(state));
        assert_eq!(manager.list()["jobs"].as_array().unwrap().len(), 1);
        let plan = manager
            .prepare(
                &Prepare {
                    kind: "extract".into(),
                    source: shown(&root.join("bundle.zip")),
                    directory: shown(&root),
                    name: "unpacked".into(),
                },
                &scopes,
                &scopes,
                &facts,
            )
            .unwrap();
        let id = plan["id"].as_str().unwrap();
        manager
            .start(id, scopes.clone(), scopes.clone(), facts.clone())
            .unwrap();
        for _ in 0..200 {
            if !matches!(
                manager.jobs.lock().unwrap()[id]
                    .receipt
                    .lock()
                    .unwrap()
                    .state
                    .as_str(),
                "running" | "publishing"
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            manager.jobs.lock().unwrap()[id]
                .receipt
                .lock()
                .unwrap()
                .state,
            "complete",
            "{}",
            manager.list()
        );
        assert_eq!(
            std::fs::read(root.join("unpacked/source/nested/data.txt")).unwrap(),
            b"content"
        );
        assert!(
            manager
                .prepare(
                    &Prepare {
                        kind: "extract".into(),
                        source: shown(&root.join("bundle.zip")),
                        directory: shown(&root),
                        name: "unpacked".into()
                    },
                    &scopes,
                    &scopes,
                    &facts
                )
                .is_err()
        );
    }
    fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = ZipWriter::new(std::fs::File::create(path).unwrap());
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (name, bytes) in entries {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }
    #[test]
    fn hostile_zip_manifest_rejects_case_collisions_and_parent_type_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("hostile.zip");
        make_zip(&zip, &[("../outside", b"bad")]);
        assert!(zip_manifest(&zip).is_err());
        make_zip(&zip, &[("same.txt", b"a"), ("SAME.txt", b"b")]);
        assert!(zip_manifest(&zip).is_err());
        make_zip(&zip, &[("parent", b"file"), ("parent/child.txt", b"child")]);
        assert!(zip_manifest(&zip).is_err());
    }
    #[test]
    fn failed_crc_leaves_only_identity_checked_private_staging() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let source = root.join("source.zip");
        make_zip(&source, &[("one.txt", b"payload-canary")]);
        let mut bytes = std::fs::read(&source).unwrap();
        let at = bytes
            .windows(14)
            .position(|v| v == b"payload-canary")
            .unwrap();
        bytes[at] ^= 1;
        std::fs::write(&source, &bytes).unwrap();
        let scopes = vec![shown(&root)];
        let manager = Archives::new(dir.path().join("jobs"));
        let facts = HashMap::new();
        let p = manager
            .prepare(
                &Prepare {
                    kind: "extract".into(),
                    source: shown(&source),
                    directory: shown(&root),
                    name: "out".into(),
                },
                &scopes,
                &scopes,
                &facts,
            )
            .unwrap();
        let id = p["id"].as_str().unwrap();
        let job = manager.jobs.lock().unwrap()[id].clone();
        assert!(execute(&manager, &job, &scopes, &scopes, &facts).is_err());
        assert!(!root.join("out").exists());
        manager.cleanup(id, &scopes).unwrap();
        assert!(job.receipt.lock().unwrap().staging.is_none());
        assert_eq!(std::fs::read(source).unwrap(), bytes);
    }
    #[test]
    fn crash_after_publish_requires_explicit_identity_and_digest_reconciliation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let source = root.join("source.txt");
        std::fs::write(&source, b"recover publication").unwrap();
        let scopes = vec![shown(&root)];
        let db = dir.path().join("jobs");
        let manager = Archives::new(db.clone());
        let facts = HashMap::new();
        let plan = manager
            .prepare(
                &Prepare {
                    kind: "create".into(),
                    source: shown(&source),
                    directory: shown(&root),
                    name: "out.zip".into(),
                },
                &scopes,
                &scopes,
                &facts,
            )
            .unwrap();
        let id = plan["id"].as_str().unwrap();
        let job = manager.jobs.lock().unwrap()[id].clone();
        // execute publishes but the normal worker's final complete receipt has not been saved.
        execute(&manager, &job, &scopes, &scopes, &facts).unwrap();
        assert_eq!(job.receipt.lock().unwrap().state, "publishing");
        drop(job);
        drop(manager);
        let restored = Archives::new(db);
        assert_eq!(
            restored.jobs.lock().unwrap()[id]
                .receipt
                .lock()
                .unwrap()
                .state,
            "interrupted"
        );
        assert!(restored.cleanup(id, &scopes).is_err());
        restored.reconcile(id, &scopes, &facts).unwrap();
        assert_eq!(
            restored.jobs.lock().unwrap()[id]
                .receipt
                .lock()
                .unwrap()
                .state,
            "complete"
        );
        assert!(root.join("out.zip").is_file());
        assert_eq!(std::fs::read(source).unwrap(), b"recover publication");
    }
}
