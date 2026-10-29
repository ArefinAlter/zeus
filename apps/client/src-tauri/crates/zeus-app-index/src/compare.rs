//! Bounded recursive directory comparisons. Metadata agreement is never a content claim.
use crate::{
    facts::quarantined_under,
    paths::{cap_failure, shown},
    transfers::Authority,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};
use zeus_app::failure::{Failure, Result};
use zeus_fs::DirectoryCap;
const MAX_ENTRIES: usize = 100_000;
const MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_SECONDS: Duration = Duration::from_secs(120);
fn fail(message: &str) -> Failure {
    Failure::new("app.compare_incomplete", message)
}

/// Explicit recursive comparison selected by the person in the native file manager.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Left source directory inside current read scopes.
    pub left: String,
    /// Right source directory inside current read scopes.
    pub right: String,
    /// Read all regular file bytes and label content equality only after SHA-256 verification.
    #[serde(default)]
    pub content: bool,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Stamp {
    id: zeus_types::FileId,
    bytes: u64,
    directory: bool,
    modified: SystemTime,
}
fn stamp(meta: zeus_fs::FileMeta, os: std::fs::Metadata) -> Result<Stamp> {
    if meta.is_symlink || (!meta.is_dir && !meta.is_regular_file) {
        return Err(fail(
            "The comparison encountered a link, junction or special file. The result is incomplete.",
        ));
    }
    Ok(Stamp {
        id: meta
            .file_id
            .ok_or_else(|| fail("This filesystem cannot provide a stable file identity."))?,
        bytes: if meta.is_dir { 0 } else { meta.len },
        directory: meta.is_dir,
        modified: os.modified()?,
    })
}
#[derive(Clone, Serialize, Deserialize)]
struct Node {
    path: String,
    relative: String,
    parent_id: zeus_types::FileId,
    stamp: Stamp,
    sha256: Option<String>,
}
impl Node {
    fn view(&self) -> Value {
        json!({"path":self.path,"size":self.stamp.bytes,"is_dir":self.stamp.directory,"modified_ms":self.stamp.modified.duration_since(SystemTime::UNIX_EPOCH).map_or(0,|d|d.as_millis().min(i64::MAX as u128) as i64),"sha256":self.sha256})
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Row {
    relative: String,
    status: String,
    left: Option<Node>,
    right: Option<Node>,
}
struct Job {
    id: String,
    left: String,
    right: String,
    content: bool,
    cancel: AtomicBool,
    entries: AtomicU64,
    bytes: AtomicU64,
    state: Mutex<String>,
    error: Mutex<String>,
    rows: Mutex<Vec<Row>>,
    created: Instant,
}
impl Job {
    fn check(&self, authority: &Authority) -> Result<()> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err(fail(
                "Comparison cancelled. Partial results cannot authorize synchronization.",
            ));
        }
        if self.created.elapsed() > MAX_SECONDS {
            return Err(fail(
                "Comparison exceeded its shared two-minute deadline. Choose smaller folders; partial results cannot authorize synchronization.",
            ));
        }
        (authority.check)()
    }
    fn snapshot(&self) -> Value {
        json!({"id":self.id,"left":self.left,"right":self.right,"content":self.content,"state":*self.state.lock().unwrap(),"complete":*self.state.lock().unwrap()=="complete","entries":self.entries.load(Ordering::SeqCst),"hashed_bytes":self.bytes.load(Ordering::SeqCst),"rows":self.rows.lock().unwrap().len(),"error":*self.error.lock().unwrap(),"limits":{"entries":MAX_ENTRIES,"hash_bytes":MAX_BYTES,"seconds":120}})
    }
}
/// At most two live recursive scans and four retained comparisons. Results expire on desktop restart.
#[derive(Default)]
pub struct Comparisons {
    jobs: Mutex<BTreeMap<String, Arc<Job>>>,
}
impl Comparisons {
    /// Start a bounded, cancellable comparison using already retained read authority.
    pub fn start(&self, request: Request, authority: Authority) -> Result<Value> {
        let left = authority.reads.path(&request.left)?;
        let right = authority.reads.path(&request.right)?;
        if left == right {
            return Err(Failure::invalid("Choose two different folders."));
        }
        let mut jobs = self.jobs.lock().unwrap();
        if jobs
            .values()
            .filter(|j| *j.state.lock().unwrap() == "running")
            .count()
            >= 2
        {
            return Err(Failure::new(
                "app.busy",
                "Two recursive comparisons are already running.",
            ));
        }
        if jobs.len() >= 4 {
            let oldest = jobs
                .iter()
                .filter(|(_, j)| *j.state.lock().unwrap() != "running")
                .min_by_key(|(_, j)| j.created)
                .map(|(id, _)| id.clone())
                .ok_or_else(|| fail("Comparison history is busy."))?;
            jobs.remove(&oldest);
        }
        let id = zeus_app::review::random_id();
        let job = Arc::new(Job {
            id: id.clone(),
            left: shown(&left),
            right: shown(&right),
            content: request.content,
            cancel: AtomicBool::new(false),
            entries: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            state: Mutex::new("running".into()),
            error: Mutex::new(String::new()),
            rows: Mutex::new(Vec::new()),
            created: Instant::now(),
        });
        let out = job.snapshot();
        jobs.insert(id, job.clone());
        tokio::task::spawn_blocking(move || match compare(&job, &authority) {
            Ok(rows) => {
                *job.rows.lock().unwrap() = rows;
                *job.state.lock().unwrap() = "complete".into();
            }
            Err(e) => {
                *job.error.lock().unwrap() = e.message;
                *job.state.lock().unwrap() = if job.cancel.load(Ordering::SeqCst) {
                    "cancelled"
                } else {
                    "incomplete"
                }
                .into();
            }
        });
        Ok(out)
    }
    /// Status and cancellation do not read file contents or mutate any files.
    pub fn status(&self, id: &str, cancel: bool) -> Result<Value> {
        let jobs = self.jobs.lock().unwrap();
        let job = jobs.get(id).ok_or_else(|| {
            Failure::invalid("That comparison has expired. Scan the folders again.")
        })?;
        if cancel {
            job.cancel.store(true, Ordering::SeqCst);
        }
        Ok(job.snapshot())
    }
    /// Page the complete stable result with a hard 500 row response bound.
    pub fn page(&self, id: &str, offset: usize, limit: usize) -> Result<Value> {
        let jobs = self.jobs.lock().unwrap();
        let job = jobs
            .get(id)
            .ok_or_else(|| Failure::invalid("That comparison has expired."))?;
        let rows = job.rows.lock().unwrap();
        let limit = limit.clamp(1, 500);
        if offset > rows.len() {
            return Err(Failure::invalid("Invalid comparison cursor."));
        }
        let items=rows.iter().skip(offset).take(limit).map(|r|json!({"relative":r.relative,"status":r.status,"left":r.left.as_ref().map(Node::view),"right":r.right.as_ref().map(Node::view)})).collect::<Vec<_>>();
        Ok(
            json!({"id":id,"complete":*job.state.lock().unwrap()=="complete","items":items,"next":(offset+limit<rows.len()).then_some(offset+limit),"total":rows.len()}),
        )
    }
    /// Stop active comparisons at their next filesystem boundary on desktop shutdown.
    pub fn cancel_all(&self) {
        for job in self.jobs.lock().unwrap().values() {
            job.cancel.store(true, Ordering::SeqCst);
        }
    }
}
impl Drop for Comparisons {
    fn drop(&mut self) {
        self.cancel_all();
    }
}
fn names(cap: &DirectoryCap, remaining: usize) -> Result<Vec<std::ffi::OsString>> {
    cap.entry_names(remaining).map_err(cap_failure)
}
fn scan(job: &Job, authority: &Authority, path: &Path) -> Result<(Stamp, BTreeMap<String, Node>)> {
    job.check(authority)?;
    if quarantined_under(&authority.facts, &shown(path)) {
        return Err(crate::ops::quarantine_refusal("This folder"));
    }
    let root = authority.reads.directory(path)?;
    let root_stamp = stamp(
        root.metadata().map_err(cap_failure)?,
        root.os_metadata().map_err(cap_failure)?,
    )?;
    let mut pending = vec![(PathBuf::new(), root.clone())];
    let mut out = BTreeMap::new();
    while let Some((relative, parent)) = pending.pop() {
        job.check(authority)?;
        let mut children = names(
            &parent,
            MAX_ENTRIES.saturating_sub(job.entries.load(Ordering::SeqCst) as usize),
        )?;
        children.sort();
        for name in children {
            job.check(authority)?;
            if name == ".zeus" || name == ".zeus-partial" {
                return Err(fail(
                    "A compared folder contains Zeus recovery storage. Choose ordinary child folders; recovery data is never silently treated as sync content.",
                ));
            }
            let rel = relative.join(&name);
            if rel.components().count() > 128
                || job.entries.fetch_add(1, Ordering::SeqCst) >= MAX_ENTRIES as u64
            {
                return Err(fail(
                    "Comparison reached its shared 100,000 entry or 128 level bound.",
                ));
            }
            let shown_rel = rel
                .to_str()
                .ok_or_else(|| fail("A filename cannot be represented without loss."))?
                .to_owned();
            let observed = stamp(
                parent.entry_metadata(&name).map_err(cap_failure)?,
                parent.entry_os_metadata(&name).map_err(cap_failure)?,
            )?;
            let mut node = Node {
                path: shown(&path.join(&rel)),
                relative: shown_rel.clone(),
                parent_id: parent.file_id().map_err(cap_failure)?,
                stamp: observed,
                sha256: None,
            };
            if node.stamp.directory {
                let child = parent.open_dir(Path::new(&name)).map_err(cap_failure)?;
                if child.file_id().map_err(cap_failure)? != node.stamp.id {
                    return Err(fail("A directory changed during comparison."));
                }
                pending.push((rel, child));
            } else if job.content {
                let mut file = parent.open_read(&name).map_err(cap_failure)?;
                if stamp(
                    zeus_fs::opened_file_metadata(&file).map_err(cap_failure)?,
                    file.metadata()?,
                )? != node.stamp
                {
                    return Err(fail("A file changed before hashing."));
                }
                let mut digest = Sha256::new();
                let mut remaining = node.stamp.bytes;
                let mut buffer = vec![0; 1024 * 1024];
                while remaining > 0 {
                    job.check(authority)?;
                    let count = remaining.min(buffer.len() as u64) as usize;
                    if job
                        .bytes
                        .fetch_add(count as u64, Ordering::SeqCst)
                        .saturating_add(count as u64)
                        > MAX_BYTES
                    {
                        return Err(fail(
                            "Content comparison reached its shared 8 GiB read bound. Choose smaller folders.",
                        ));
                    }
                    file.read_exact(&mut buffer[..count])?;
                    digest.update(&buffer[..count]);
                    remaining -= count as u64;
                }
                if stamp(
                    zeus_fs::opened_file_metadata(&file).map_err(cap_failure)?,
                    file.metadata()?,
                )? != node.stamp
                {
                    return Err(fail("A file changed during hashing."));
                }
                node.sha256 = Some(hex::encode(digest.finalize()));
            }
            out.insert(shown_rel, node);
        }
    }
    if stamp(
        root.metadata().map_err(cap_failure)?,
        root.os_metadata().map_err(cap_failure)?,
    )? != root_stamp
    {
        return Err(fail("A comparison root changed during the scan."));
    }
    Ok((root_stamp, out))
}
fn recheck(
    job: &Job,
    authority: &Authority,
    root: &str,
    expected: &Stamp,
    nodes: &BTreeMap<String, Node>,
) -> Result<()> {
    let root = authority.reads.directory(Path::new(root))?;
    if stamp(
        root.metadata().map_err(cap_failure)?,
        root.os_metadata().map_err(cap_failure)?,
    )? != *expected
    {
        return Err(fail(
            "A comparison root changed before the result was complete.",
        ));
    }
    for node in nodes.values() {
        job.check(authority)?;
        let (parent, name) = authority.reads.parent(Path::new(&node.path))?;
        if parent.file_id().map_err(cap_failure)? != node.parent_id
            || stamp(
                parent.entry_metadata(&name).map_err(cap_failure)?,
                parent.entry_os_metadata(&name).map_err(cap_failure)?,
            )? != node.stamp
        {
            return Err(fail(
                "A compared entry changed. Scan again before planning synchronization.",
            ));
        }
    }
    Ok(())
}
fn compare(job: &Job, authority: &Authority) -> Result<Vec<Row>> {
    let (left_stamp, left) = scan(job, authority, Path::new(&job.left))?;
    let (right_stamp, right) = scan(job, authority, Path::new(&job.right))?;
    recheck(job, authority, &job.left, &left_stamp, &left)?;
    recheck(job, authority, &job.right, &right_stamp, &right)?;
    let keys: BTreeSet<_> = left.keys().chain(right.keys()).cloned().collect();
    let mut rows = Vec::with_capacity(keys.len());
    for relative in keys {
        job.check(authority)?;
        let l = left.get(&relative);
        let r = right.get(&relative);
        let status = match (l, r) {
            (Some(_), None) => "left_only",
            (None, Some(_)) => "right_only",
            (Some(a), Some(b)) if a.stamp.directory != b.stamp.directory => "type_conflict",
            (Some(a), Some(_)) if a.stamp.directory => "directory",
            (Some(a), Some(b)) if job.content => {
                if a.sha256 == b.sha256 {
                    "content_match"
                } else {
                    "content_different"
                }
            }
            (Some(a), Some(b)) => {
                if a.stamp.bytes == b.stamp.bytes && a.stamp.modified == b.stamp.modified {
                    "metadata_match"
                } else {
                    "metadata_different"
                }
            }
            _ => unreachable!(),
        };
        rows.push(Row {
            relative,
            status: status.into(),
            left: l.cloned(),
            right: r.cloned(),
        });
    }
    if serde_json::to_vec(&rows)?.len() > 64 * 1024 * 1024 {
        return Err(fail(
            "The comparison reached its 64 MiB result bound. Choose smaller folders.",
        ));
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::RootCaps;
    fn fixture(content: bool) -> (tempfile::TempDir, Job, Authority) {
        let temp = tempfile::tempdir().unwrap();
        let left = temp.path().join("left");
        let right = temp.path().join("right");
        std::fs::create_dir_all(left.join("deep/inner")).unwrap();
        std::fs::create_dir_all(right.join("deep/inner")).unwrap();
        let roots = vec![shown(temp.path())];
        let authority = Authority {
            reads: RootCaps::capture(&roots).unwrap(),
            writes: RootCaps::capture(&roots).unwrap(),
            facts: Arc::new(Default::default()),
            check: Arc::new(|| Ok(())),
        };
        let job = Job {
            id: "fixture123".into(),
            left: shown(&left),
            right: shown(&right),
            content,
            cancel: AtomicBool::new(false),
            entries: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            state: Mutex::new("running".into()),
            error: Mutex::new(String::new()),
            rows: Mutex::new(Vec::new()),
            created: Instant::now(),
        };
        (temp, job, authority)
    }
    #[test]
    fn recursive_hash_comparison_distinguishes_metadata_and_content() {
        let (_temp, job, a) = fixture(true);
        std::fs::write(Path::new(&job.left).join("deep/inner/a"), b"same").unwrap();
        std::fs::write(Path::new(&job.right).join("deep/inner/a"), b"same").unwrap();
        std::fs::write(Path::new(&job.left).join("left-only"), b"left").unwrap();
        std::fs::write(Path::new(&job.right).join("right-only"), b"right").unwrap();
        let rows = compare(&job, &a).unwrap();
        assert!(rows.iter().any(
            |r| r.relative.replace('\\', "/") == "deep/inner/a" && r.status == "content_match"
        ));
        assert!(rows.iter().any(|r| r.status == "left_only"));
        assert!(rows.iter().any(|r| r.status == "right_only"));
        assert!(
            rows.iter()
                .filter(|r| r.left.as_ref().is_some_and(|n| n.stamp.directory) && r.right.is_some())
                .all(|r| r.status == "directory")
        );
    }
    #[test]
    fn matching_size_and_timestamp_does_not_claim_equal_content() {
        let (_temp, job, a) = fixture(false);
        let l = Path::new(&job.left).join("file");
        let r = Path::new(&job.right).join("file");
        std::fs::write(&l, b"aaaa").unwrap();
        std::fs::write(&r, b"bbbb").unwrap();
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        for p in [&l, &r] {
            std::fs::OpenOptions::new()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(time)
                .unwrap();
        }
        let rows = compare(&job, &a).unwrap();
        assert!(
            rows.iter()
                .any(|r| r.relative == "file" && r.status == "metadata_match")
        );
        assert!(rows.iter().all(|r| r.status != "content_match"));
    }
    #[test]
    fn cancellation_and_shared_hash_budget_produce_no_complete_plan() {
        let (_temp, job, a) = fixture(true);
        job.cancel.store(true, Ordering::SeqCst);
        assert!(compare(&job, &a).is_err());
        job.cancel.store(false, Ordering::SeqCst);
        std::fs::write(Path::new(&job.left).join("file"), b"bytes").unwrap();
        job.bytes.store(MAX_BYTES, Ordering::SeqCst);
        assert!(compare(&job, &a).is_err());
        assert!(job.rows.lock().unwrap().is_empty());
    }
    #[test]
    fn unknown_recovery_storage_is_not_silently_scanned_as_user_content() {
        let (_temp, job, a) = fixture(false);
        std::fs::create_dir(Path::new(&job.left).join(".zeus-partial")).unwrap();
        assert!(compare(&job, &a).is_err());
    }
}
