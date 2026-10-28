//! Native ZIP operations with reviewed manifests, bounded reads/expansion, durable receipts,
//! cancellation and no-replace publication. Archive content never launches a helper or executable.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Seek, Write};
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
use zeus_fs::DirectoryCap;
#[cfg(test)]
use zeus_fs::{Filesystem, RealFs};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::{
    facts::{FileFacts, quarantined_under},
    paths::{RootCaps, cap_failure, check_name, shown},
};

mod io;
use io::{ArchiveSource, cleanup_stage, entry_stamp, file_stamp, stage_name};

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
#[cfg(test)]
fn stamp(path: &Path) -> Result<Stamp> {
    let m = RealFs
        .metadata(path)
        .map_err(|_| failed("The source identity could not be read."))?
        .ok_or_else(|| failed("The source is missing."))?;
    stamp_meta(m)
}
fn stamp_meta(m: zeus_fs::FileMeta) -> Result<Stamp> {
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
    #[serde(default)]
    target_parent_id: Option<zeus_types::FileId>,
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
        self.prepare_pinned(
            request,
            &RootCaps::capture(reads)?,
            &RootCaps::capture(writes)?,
            facts,
        )
    }
    /// Preview with the root identities retained by the native settings service.
    pub fn prepare_pinned(
        &self,
        request: &Prepare,
        reads: &RootCaps,
        writes: &RootCaps,
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
        let source = reads.path(source)?;
        if quarantined_under(facts, &shown(&source)) {
            return Err(crate::ops::quarantine_refusal("This item"));
        }
        let directory = writes.path(directory)?;
        let destination = writes.directory(&directory)?;
        if directory.starts_with(&source) {
            return Err(failed(
                "Choose an existing writable destination outside the source.",
            ));
        }
        let target = directory.join(name);
        if destination
            .entry_exists(std::ffi::OsStr::new(name))
            .map_err(cap_failure)?
        {
            return Err(Failure::new(
                "app.exists",
                "The destination exists. Archives never replace it.",
            ));
        }
        let mut input = ArchiveSource::open(&source, reads)?;
        let source_stamp = input.stamp()?;
        let (entries, bytes) = if kind == "create" {
            input.manifest(&source, facts)?
        } else {
            zip_manifest_open(input.zip_file()?)?
        };
        if input.stamp()? != source_stamp {
            return Err(failed("The source changed during preview."));
        }
        let plan = Plan {
            id: format!("farc_{}", zeus_app::review::random_id()),
            kind: kind.into(),
            source: shown(&source),
            source_stamp,
            target: shown(&target),
            target_parent_id: Some(destination.file_id().map_err(cap_failure)?),
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
        self.start_pinned(
            id,
            RootCaps::capture(&reads)?,
            RootCaps::capture(&writes)?,
            facts,
        )
    }
    /// Execute using retained current read and managed-write root identities.
    pub fn start_pinned(
        self: &Arc<Self>,
        id: &str,
        reads: RootCaps,
        writes: RootCaps,
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
            let result = execute_pinned(&manager, &job, &reads, &writes, &facts);
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
        self.cleanup_pinned(id, &RootCaps::capture(writes)?)
    }
    /// Remove only the expected staging object through retained directory authority.
    pub fn cleanup_pinned(&self, id: &str, writes: &RootCaps) -> Result<Value> {
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
            let (parent, target_name) = destination(&r.plan, writes, false)?;
            let name = stage_name(&r.plan, stage, writes)?;
            if !parent.entry_exists(&name).map_err(cap_failure)? {
                if parent.entry_exists(&target_name).map_err(cap_failure)? {
                    return Err(failed(
                        "The target exists and staging is absent. Reconcile publication before cleanup.",
                    ));
                }
            } else {
                let expected = r.staging_id.as_ref().ok_or_else(|| failed("The staging identity was not saved. Inspect it manually; cleanup was refused."))?;
                cleanup_stage(&parent, &name, expected)?;
            }
            r.staging = None;
            r.staging_id = None;
            if r.state == "uncertain" && !parent.entry_exists(&target_name).map_err(cap_failure)? {
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
        self.reconcile_pinned(id, &RootCaps::capture(writes)?, facts)
    }
    /// Inspect publication with held roots, directories and files; never replay an interrupted write.
    pub fn reconcile_pinned(
        &self,
        id: &str,
        writes: &RootCaps,
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
        let target = writes.path(&r.plan.target)?;
        let (parent, target_name) = destination(&r.plan, writes, false)?;
        if quarantined_under(facts, &shown(&target)) {
            return Err(crate::ops::quarantine_refusal("This output"));
        }
        let actual = entry_stamp(&parent, &target_name)?;
        if Some(&actual.file_id) != r.staging_id.as_ref() || r.sha256.is_none() {
            return Err(failed(
                "The output identity is not established by this receipt; review the target manually.",
            ));
        }
        if let Some(stage) = &r.staging {
            let name = stage_name(&r.plan, stage, writes)?;
            if parent.entry_exists(&name).map_err(cap_failure)? {
                return Err(failed(
                    "Both staging and target exist; automatic reconciliation is ambiguous.",
                ));
            }
        }
        let digest = published_digest(&r.plan, &parent, &target_name, &actual)?;
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
fn validate_zip_container(input: &mut std::fs::File) -> Result<()> {
    use std::io::SeekFrom;
    input.rewind()?;
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
#[cfg(test)]
fn zip_manifest(source: &Path) -> Result<(Vec<Entry>, u64)> {
    let roots = RootCaps::capture(&[shown(source.parent().unwrap())])?;
    let mut input = ArchiveSource::open(source, &roots)?;
    zip_manifest_open(input.zip_file()?)
}
fn zip_manifest_open(source: &mut std::fs::File) -> Result<(Vec<Entry>, u64)> {
    if source.metadata()?.len() > MAX_ZIP_BYTES {
        return Err(failed(
            "Inspect ZIP inputs up to 1 GiB. Expanded data is limited to 4 GiB.",
        ));
    }
    validate_zip_container(source)?;
    let mut zip =
        ZipArchive::new(source).map_err(|_| failed("This is not a supported ZIP archive."))?;
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
    let mut folded = HashMap::new();
    for path in paths.keys() {
        let key = path
            .components()
            .map(|p| p.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
            .to_lowercase();
        if folded
            .insert(key, path)
            .is_some_and(|previous| previous != path)
        {
            return Err(failed(
                "ZIP parent folders collide when compared without case.",
            ));
        }
    }
    Ok(paths)
}
fn published_digest(
    plan: &Plan,
    parent: &DirectoryCap,
    name: &std::ffi::OsStr,
    expected_root: &Stamp,
) -> Result<String> {
    let started = Instant::now();
    if plan.kind == "create" {
        let mut input = parent.open_read(name).map_err(cap_failure)?;
        let before = file_stamp(&input)?;
        if &before != expected_root {
            return Err(failed("The published ZIP changed before reconciliation."));
        }
        let mut hash = Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            io::time_bound(started)?;
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
        if bytes != before.bytes
            || file_stamp(&input)? != before
            || entry_stamp(parent, name)? != before
        {
            return Err(failed("The published ZIP changed during reconciliation."));
        }
        return Ok(hex::encode(hash.finalize()));
    }
    let root = parent.open_dir(Path::new(name)).map_err(cap_failure)?;
    let before = stamp_meta(root.metadata().map_err(cap_failure)?)?;
    if &before != expected_root {
        return Err(failed(
            "The published directory changed before reconciliation.",
        ));
    }
    let expected = expanded_paths(&plan.entries)?;
    let mut pending = vec![PathBuf::new()];
    let mut observed = Vec::new();
    while let Some(relative) = pending.pop() {
        io::time_bound(started)?;
        let dir = root.open_dir(&relative).map_err(cap_failure)?;
        for child in dir
            .entry_names(MAX_ENTRIES - observed.len())
            .map_err(cap_failure)?
        {
            let path = relative.join(&child);
            let meta = entry_stamp(&dir, &child)?;
            if expected.get(&path) != Some(&meta.directory) {
                return Err(failed(
                    "The published tree contains an unexpected or changed item.",
                ));
            }
            if meta.directory {
                pending.push(path.clone());
            }
            observed.push((path, meta));
        }
    }
    if observed.len() != expected.len() {
        return Err(failed("The published tree is incomplete."));
    }
    let mut hash = Sha256::new();
    for entry in plan.entries.iter().filter(|e| !e.directory) {
        let relative = relative_name(&entry.name, false)?;
        let dir = root
            .open_dir(relative.parent().unwrap())
            .map_err(cap_failure)?;
        let mut input = dir
            .open_read(relative.file_name().unwrap())
            .map_err(cap_failure)?;
        let before = file_stamp(&input)?;
        if before.bytes != entry.bytes {
            return Err(failed("A published file changed length."));
        }
        let mut file_hash = Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            io::time_bound(started)?;
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
        if bytes != entry.bytes
            || file_stamp(&input)? != before
            || entry_stamp(&dir, relative.file_name().unwrap())? != before
        {
            return Err(failed("A published file changed during reconciliation."));
        }
        hash.update((entry.name.len() as u64).to_le_bytes());
        hash.update(entry.name.as_bytes());
        hash.update(entry.bytes.to_le_bytes());
        hash.update(file_hash.finalize());
    }
    for (path, before) in observed {
        io::time_bound(started)?;
        let dir = root.open_dir(path.parent().unwrap()).map_err(cap_failure)?;
        if entry_stamp(&dir, path.file_name().unwrap())? != before {
            return Err(failed("The published tree changed during reconciliation."));
        }
    }
    if stamp_meta(root.metadata().map_err(cap_failure)?)? != before
        || entry_stamp(parent, name)? != before
    {
        return Err(failed(
            "The published directory changed during reconciliation.",
        ));
    }
    Ok(hex::encode(hash.finalize()))
}
fn destination(
    plan: &Plan,
    writes: &RootCaps,
    require_preview_identity: bool,
) -> Result<(DirectoryCap, std::ffi::OsString)> {
    let target = writes.path(&plan.target)?;
    let (parent, name) = writes.parent(&target)?;
    check_name(
        name.to_str()
            .ok_or_else(|| failed("The destination name is not valid text."))?,
    )?;
    match plan.target_parent_id.as_ref() {
        Some(expected) if parent.file_id().map_err(cap_failure)?.eq(expected) => (),
        None if !require_preview_identity => (),
        None => {
            return Err(failed(
                "Preview this older archive plan again before starting it.",
            ));
        }
        Some(_) => {
            return Err(failed(
                "The destination folder changed after preview. Choose it again.",
            ));
        }
    }
    Ok((parent, name))
}
fn validate_plan(
    plan: &Plan,
    reads: &RootCaps,
    writes: &RootCaps,
    facts: &HashMap<String, FileFacts>,
) -> Result<(ArchiveSource, DirectoryCap, std::ffi::OsString)> {
    if !valid_id(&plan.id)
        || !["create", "extract"].contains(&plan.kind.as_str())
        || plan.entries.len() > MAX_ENTRIES
        || plan.bytes > MAX_BYTES
    {
        return Err(failed("The saved archive plan is invalid."));
    }
    let path = reads.path(&plan.source)?;
    let mut source = ArchiveSource::open(&path, reads)?;
    if source.stamp()? != plan.source_stamp {
        return Err(failed(
            "The source changed after preview. Prepare a new manifest.",
        ));
    }
    if quarantined_under(facts, &shown(&path)) {
        return Err(crate::ops::quarantine_refusal("This item"));
    }
    let target = writes.path(&plan.target)?;
    if target.parent().is_none_or(|p| p.starts_with(&path)) {
        return Err(failed("Choose a destination outside the source."));
    }
    let (parent, name) = destination(plan, writes, true)?;
    if parent.entry_exists(&name).map_err(cap_failure)? {
        return Err(Failure::new(
            "app.exists",
            "The destination already exists. No files were replaced.",
        ));
    }
    let (current, bytes) = if plan.kind == "create" {
        source.manifest(&path, facts)?
    } else {
        zip_manifest_open(source.zip_file()?)?
    };
    if bytes != plan.bytes
        || serde_json::to_vec(&current)? != serde_json::to_vec(&plan.entries)?
        || source.stamp()? != plan.source_stamp
    {
        return Err(failed(
            "The source manifest changed. Preview again before starting.",
        ));
    }
    Ok((source, parent, name))
}
#[cfg(test)]
fn execute(
    manager: &Archives,
    job: &Job,
    reads: &[String],
    writes: &[String],
    facts: &HashMap<String, FileFacts>,
) -> Result<String> {
    execute_pinned(
        manager,
        job,
        &RootCaps::capture(reads)?,
        &RootCaps::capture(writes)?,
        facts,
    )
}
fn execute_pinned(
    manager: &Archives,
    job: &Job,
    reads: &RootCaps,
    writes: &RootCaps,
    facts: &HashMap<String, FileFacts>,
) -> Result<String> {
    execute_steps(manager, job, reads, writes, facts, || {})
}
// The private callback marks the durable publication boundary for deterministic crash/cancel/
// collision tests. Production supplies an empty callback; it grants no new external authority.
fn execute_steps(
    manager: &Archives,
    job: &Job,
    reads: &RootCaps,
    writes: &RootCaps,
    facts: &HashMap<String, FileFacts>,
    before_publish: impl FnOnce(),
) -> Result<String> {
    let started = Instant::now();
    let plan = job.receipt.lock().unwrap().plan.clone();
    job.check(started)?;
    let (mut source, parent, target_name) = validate_plan(&plan, reads, writes, facts)?;
    let stage_name = format!(
        ".zeus-archive-{}-{}",
        plan.id,
        zeus_app::review::random_id()
    );
    let stage = Path::new(&plan.target).parent().unwrap().join(&stage_name);
    {
        let mut r = job.receipt.lock().unwrap();
        r.staging = Some(shown(&stage));
        r.staging_id = None;
        r.sha256 = None;
        manager.save(&r)?;
    }
    let stage_name = std::ffi::OsStr::new(&stage_name);
    let (digest, staging_id) = if plan.kind == "create" {
        let output = parent.create_new(stage_name).map_err(cap_failure)?;
        let id = file_stamp(&output)?.file_id;
        {
            let mut r = job.receipt.lock().unwrap();
            r.staging_id = Some(id.clone());
            manager.save(&r)?;
        }
        (create_zip(&plan, output, &source, job, started)?, id)
    } else {
        parent.create_dir(stage_name).map_err(cap_failure)?;
        let stage = parent
            .open_dir(Path::new(stage_name))
            .map_err(cap_failure)?;
        let id = stage.file_id().map_err(cap_failure)?;
        {
            let mut r = job.receipt.lock().unwrap();
            r.staging_id = Some(id.clone());
            manager.save(&r)?;
        }
        (
            extract_zip(&plan, &stage, source.zip_file()?, job, started)?,
            id,
        )
    };
    job.check(started)?;
    if source.stamp()? != plan.source_stamp {
        return Err(failed("The archive source changed during execution."));
    }
    // Close staging data handles, then pin and compare the publication object. Windows moves the
    // exact retained object; Unix retains the shared primitive's explicit compare/rename limit.
    let publication = parent.prepare_rename(stage_name).map_err(cap_failure)?;
    if publication
        .metadata()
        .map_err(cap_failure)?
        .file_id
        .as_ref()
        != Some(&staging_id)
    {
        return Err(failed("The staging object changed before publication."));
    }
    {
        let mut r = job.receipt.lock().unwrap();
        r.state = "publishing".into();
        r.sha256 = Some(digest.clone());
        manager.save(&r)?;
    }
    before_publish();
    if let Err(error) = job.check(started) {
        job.receipt.lock().unwrap().state = if job.cancel.load(Ordering::Relaxed) {
            "cancelled"
        } else {
            "failed"
        }
        .into();
        return Err(error);
    }
    if let Err(error) = publication.rename_noreplace(&parent, &target_name) {
        // A reported no-replace collision is a refused publication, not a completed output.
        if matches!(error, zeus_fs::FsError::AlreadyExists) {
            job.receipt.lock().unwrap().state = "failed".into();
        }
        return Err(cap_failure(error));
    }
    parent.sync().map_err(|_| failed("The output was published but directory durability could not be confirmed. Review the target."))?;
    Ok(digest)
}
fn create_zip(
    plan: &Plan,
    output: std::fs::File,
    source: &ArchiveSource,
    job: &Job,
    start: Instant,
) -> Result<String> {
    let mut output = ZipWriter::new(output);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);
    for entry in &plan.entries {
        job.check(start)?;
        if Some(source.member_stamp(&entry.name, entry.directory, Path::new(&plan.source))?)
            != entry.stamp
        {
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
            let mut input = source.open_member(&entry.name, Path::new(&plan.source))?;
            if Some(file_stamp(&input.file)?) != entry.stamp {
                return Err(failed("A source changed before opening it."));
            }
            input.file.rewind()?;
            let mut count = 0u64;
            let mut buffer = [0u8; 65536];
            loop {
                job.check(start)?;
                let n = input.file.read(&mut buffer)?;
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
            if count != entry.bytes
                || Some(file_stamp(&input.file)?) != entry.stamp
                || Some(source.member_stamp(&entry.name, false, Path::new(&plan.source))?)
                    != entry.stamp
            {
                return Err(failed("A source changed while creating the ZIP."));
            }
        }
        job.files.fetch_add(1, Ordering::Relaxed);
    }
    let mut file = output
        .finish()
        .map_err(|_| failed("The ZIP archive could not be finalized."))?;
    file.sync_all()?;
    file.rewind()?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        job.check(start)?;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}
fn extract_zip(
    plan: &Plan,
    stage: &DirectoryCap,
    source: &mut std::fs::File,
    job: &Job,
    start: Instant,
) -> Result<String> {
    let mut zip = ZipArchive::new(source).map_err(|_| failed("The ZIP source is invalid."))?;
    let mut tree_hash = Sha256::new();
    for (i, entry) in plan.entries.iter().enumerate() {
        job.check(start)?;
        let relative = relative_name(&entry.name, entry.directory)?;
        if entry.directory {
            stage.create_dir_all(&relative).map_err(cap_failure)?;
        } else {
            let parent = stage
                .create_dir_all(
                    relative
                        .parent()
                        .ok_or_else(|| failed("A ZIP entry has no parent."))?,
                )
                .map_err(cap_failure)?;
            let mut output = parent
                .create_new(relative.file_name().unwrap())
                .map_err(cap_failure)?;
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
    stage.sync().map_err(cap_failure)?;
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
    fn prepared_file(
        root: &Path,
        dest: &Path,
        db: &Path,
    ) -> (Archives, String, Vec<String>, Arc<Job>) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::create_dir_all(dest).unwrap();
        std::fs::write(root.join("source.txt"), b"source bytes").unwrap();
        let roots = vec![shown(root)];
        let manager = Archives::new(db.to_path_buf());
        let plan = manager
            .prepare(
                &Prepare {
                    kind: "create".into(),
                    source: shown(&root.join("source.txt")),
                    directory: shown(dest),
                    name: "out.zip".into(),
                },
                &roots,
                &roots,
                &HashMap::new(),
            )
            .unwrap();
        let id = plan["id"].as_str().unwrap().to_owned();
        let job = manager.jobs.lock().unwrap()[&id].clone();
        (manager, id, roots, job)
    }
    fn directory_link(target: &Path, link: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
        #[cfg(windows)]
        assert!(
            std::process::Command::new("cmd.exe")
                .current_dir(link.parent().unwrap())
                .args(["/d", "/c", "mklink", "/j"])
                .arg(link)
                .arg(target)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        );
    }
    #[test]
    fn preview_binds_destination_parent_and_legacy_ready_plans_require_preview() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let dest = root.join("dest");
        let (manager, id, roots, job) = prepared_file(&root, &dest, &temp.path().join("jobs"));
        std::fs::rename(&dest, root.join("old-dest")).unwrap();
        std::fs::create_dir(&dest).unwrap();
        assert!(execute(&manager, &job, &roots, &roots, &HashMap::new()).is_err());
        assert!(job.receipt.lock().unwrap().staging.is_none());
        assert!(!dest.join("out.zip").exists());
        // A receipt written by the previous alpha still loads, but cannot start with an unbound destination.
        let file = temp.path().join("jobs").join(format!("{id}.json"));
        let mut old: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        old["plan"]
            .as_object_mut()
            .unwrap()
            .remove("target_parent_id");
        std::fs::write(&file, serde_json::to_vec(&old).unwrap()).unwrap();
        let restored = Archives::new(temp.path().join("jobs"));
        let job = restored.jobs.lock().unwrap()[&id].clone();
        assert!(
            execute(&restored, &job, &roots, &roots, &HashMap::new())
                .unwrap_err()
                .message
                .contains("older")
        );
        assert!(job.receipt.lock().unwrap().staging.is_none());
    }
    #[test]
    fn publication_collision_and_late_cancellation_keep_owned_staging_for_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        for cancel in [false, true] {
            let root = temp
                .path()
                .join(if cancel { "cancel" } else { "collision" });
            let (manager, id, roots, job) = prepared_file(&root, &root, &root.join("state"));
            let caps = RootCaps::capture(&roots).unwrap();
            let outcome = execute_steps(&manager, &job, &caps, &caps, &HashMap::new(), || {
                if cancel {
                    job.cancel.store(true, Ordering::Relaxed);
                } else {
                    std::fs::write(root.join("out.zip"), b"foreign output").unwrap();
                }
            });
            assert!(outcome.is_err());
            assert_eq!(
                job.receipt.lock().unwrap().state,
                if cancel { "cancelled" } else { "failed" }
            );
            let stage = job.receipt.lock().unwrap().staging.clone().unwrap();
            assert!(Path::new(&stage).is_file());
            if cancel {
                assert!(!root.join("out.zip").exists());
            } else {
                assert_eq!(
                    std::fs::read(root.join("out.zip")).unwrap(),
                    b"foreign output"
                );
            }
            manager.cleanup_pinned(&id, &caps).unwrap();
            assert!(!Path::new(&stage).exists());
            assert!(job.receipt.lock().unwrap().staging.is_none());
            assert_eq!(
                std::fs::read(root.join("source.txt")).unwrap(),
                b"source bytes"
            );
        }
    }
    #[test]
    fn cleanup_refuses_a_replaced_staging_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let (manager, id, roots, job) = prepared_file(&root, &root, &temp.path().join("jobs"));
        let stage = root.join(format!(".zeus-archive-{id}-{}", "a".repeat(32)));
        std::fs::write(&stage, b"owned output").unwrap();
        {
            let mut receipt = job.receipt.lock().unwrap();
            receipt.staging = Some(shown(&stage));
            receipt.staging_id = Some(stamp(&stage).unwrap().file_id);
            manager.save(&receipt).unwrap();
        }
        std::fs::rename(&stage, root.join("original-stage")).unwrap();
        std::fs::write(&stage, b"foreign replacement").unwrap();
        assert!(manager.cleanup(&id, &roots).is_err());
        assert_eq!(std::fs::read(&stage).unwrap(), b"foreign replacement");
        assert!(job.receipt.lock().unwrap().staging.is_some());
    }
    #[test]
    fn cleanup_preflights_the_entire_tree_before_removing_anything() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("canary"), b"outside unchanged").unwrap();
        let root = temp.path().join("root");
        let (manager, id, roots, job) = prepared_file(&root, &root, &temp.path().join("jobs"));
        let stage = root.join(format!(".zeus-archive-{id}-{}", "b".repeat(32)));
        std::fs::create_dir_all(stage.join("nested")).unwrap();
        std::fs::write(stage.join("nested/owned"), b"partial").unwrap();
        directory_link(outside.path(), &stage.join("junction"));
        {
            let mut r = job.receipt.lock().unwrap();
            r.staging = Some(shown(&stage));
            r.staging_id = Some(stamp(&stage).unwrap().file_id);
            manager.save(&r).unwrap();
        }
        assert!(manager.cleanup(&id, &roots).is_err());
        assert_eq!(
            std::fs::read(stage.join("nested/owned")).unwrap(),
            b"partial"
        );
        assert_eq!(
            std::fs::read(outside.path().join("canary")).unwrap(),
            b"outside unchanged"
        );
        #[cfg(windows)]
        std::fs::remove_dir(stage.join("junction")).unwrap();
        #[cfg(unix)]
        std::fs::remove_file(stage.join("junction")).unwrap();
        manager.cleanup(&id, &roots).unwrap();
        assert!(!stage.exists());
        assert_eq!(
            std::fs::read(outside.path().join("canary")).unwrap(),
            b"outside unchanged"
        );
    }
    #[test]
    fn a_whole_read_root_can_be_archived_without_authority_over_its_parent() {
        let temp = tempfile::tempdir().unwrap();
        let read = temp.path().join("read");
        let write = temp.path().join("write");
        std::fs::create_dir_all(read.join("inner")).unwrap();
        std::fs::create_dir(&write).unwrap();
        std::fs::write(read.join("inner/data.txt"), b"root contents").unwrap();
        let reads = vec![shown(&read)];
        let writes = vec![shown(&write)];
        let manager = Archives::new(temp.path().join("jobs"));
        let plan = manager
            .prepare(
                &Prepare {
                    kind: "create".into(),
                    source: shown(&read),
                    directory: shown(&write),
                    name: "all.zip".into(),
                },
                &reads,
                &writes,
                &HashMap::new(),
            )
            .unwrap();
        let job = manager.jobs.lock().unwrap()[plan["id"].as_str().unwrap()].clone();
        execute(&manager, &job, &reads, &writes, &HashMap::new()).unwrap();
        let mut zip = ZipArchive::new(std::fs::File::open(write.join("all.zip")).unwrap()).unwrap();
        let mut bytes = Vec::new();
        zip.by_name("read/inner/data.txt")
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"root contents");
    }
    #[test]
    fn implicit_parent_case_collisions_and_device_aliases_fail_preview() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source.zip");
        for names in [
            ["A/first", "a/second"],
            ["CONIN$/first", "second"],
            ["parent/COM¹.txt", "second"],
        ] {
            make_zip(&path, &[(names[0], b"one"), (names[1], b"two")]);
            assert!(zip_manifest(&path).is_err(), "{names:?}");
        }
    }

    #[test]
    fn extracted_tree_reconciliation_rejects_content_changes_and_linked_children() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("canary"), b"outside unchanged").unwrap();
        let zip = root.join("source.zip");
        make_zip(
            &zip,
            &[("nested/data.txt", b"original"), ("other.txt", b"two")],
        );
        let roots = vec![shown(&root)];
        let db = temp.path().join("jobs");
        let manager = Archives::new(db.clone());
        let plan = manager
            .prepare(
                &Prepare {
                    kind: "extract".into(),
                    source: shown(&zip),
                    directory: shown(&root),
                    name: "output".into(),
                },
                &roots,
                &roots,
                &HashMap::new(),
            )
            .unwrap();
        let id = plan["id"].as_str().unwrap();
        let job = manager.jobs.lock().unwrap()[id].clone();
        // Publication succeeded; simulate a restart before the worker saves its final receipt.
        execute(&manager, &job, &roots, &roots, &HashMap::new()).unwrap();
        drop(job);
        drop(manager);
        let restored = Archives::new(db);
        let output = root.join("output");
        std::fs::write(output.join("nested/data.txt"), b"modified").unwrap();
        assert!(restored.reconcile(id, &roots, &HashMap::new()).is_err());
        assert_eq!(
            restored.jobs.lock().unwrap()[id]
                .receipt
                .lock()
                .unwrap()
                .state,
            "interrupted"
        );
        std::fs::write(output.join("nested/data.txt"), b"original").unwrap();
        std::fs::rename(output.join("nested"), root.join("saved-nested")).unwrap();
        directory_link(&outside, &output.join("nested"));
        assert!(restored.reconcile(id, &roots, &HashMap::new()).is_err());
        assert_eq!(
            std::fs::read(outside.join("canary")).unwrap(),
            b"outside unchanged"
        );
        #[cfg(windows)]
        std::fs::remove_dir(output.join("nested")).unwrap();
        #[cfg(unix)]
        std::fs::remove_file(output.join("nested")).unwrap();
        std::fs::rename(root.join("saved-nested"), output.join("nested")).unwrap();
        restored.reconcile(id, &roots, &HashMap::new()).unwrap();
        assert_eq!(
            restored.jobs.lock().unwrap()[id]
                .receipt
                .lock()
                .unwrap()
                .state,
            "complete"
        );
        assert_eq!(
            std::fs::read(output.join("nested/data.txt")).unwrap(),
            b"original"
        );
    }
}
