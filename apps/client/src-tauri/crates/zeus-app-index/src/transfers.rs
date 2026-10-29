//! Bounded background file and folder copies. Existing destinations are never replaced.
use crate::{
    facts::{FileFacts, quarantined_under},
    ops::quarantine_refusal,
    paths::{RootCaps, cap_failure, shown},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use zeus_app::failure::{Failure, Result};

#[derive(Default)]
struct Job {
    cancelled: AtomicBool,
    bytes: AtomicU64,
    total: AtomicU64,
    files: AtomicU64,
    state: Mutex<String>,
    target: Mutex<String>,
    error: Mutex<String>,
}
impl Job {
    fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Relaxed) {
            Err(Failure::new(
                "app.cancelled",
                "Copy cancelled. Completed files in the destination are kept; the source is unchanged.",
            ))
        } else {
            Ok(())
        }
    }
    fn snapshot(&self, id: &str) -> Value {
        json!({"id":id,"state":self.state.lock().unwrap().clone(),"path":self.target.lock().unwrap().clone(),"bytes":self.bytes.load(Ordering::Relaxed),"total_bytes":self.total.load(Ordering::Relaxed),"files":self.files.load(Ordering::Relaxed),"error":self.error.lock().unwrap().clone()})
    }
}

/// Copy jobs owned by this desktop session; at most four run concurrently.
#[derive(Default)]
pub struct Transfers {
    jobs: Mutex<HashMap<String, Arc<Job>>>,
}
impl Drop for Transfers {
    fn drop(&mut self) {
        for job in self.jobs.get_mut().unwrap().values() {
            job.cancelled.store(true, Ordering::Relaxed);
        }
    }
}
impl Transfers {
    /// Start a copy after the app has obtained a fresh quarantine table.
    pub fn start(
        &self,
        from: String,
        to: String,
        reads: RootCaps,
        writes: RootCaps,
        facts: Arc<HashMap<String, FileFacts>>,
    ) -> Result<Value> {
        let mut jobs = self.jobs.lock().unwrap();
        let running =
            |j: &&Arc<Job>| matches!(j.state.lock().unwrap().as_str(), "scanning" | "copying");
        if jobs.values().filter(running).count() >= 4 {
            return Err(Failure::new(
                "app.busy",
                "Wait for a copy to finish, or cancel one.",
            ));
        }
        if jobs.len() >= 32 {
            jobs.retain(|_, j| matches!(j.state.lock().unwrap().as_str(), "scanning" | "copying"));
        }
        let id = zeus_app::review::random_id();
        let job = Arc::new(Job::default());
        *job.state.lock().unwrap() = "scanning".into();
        jobs.insert(id.clone(), job.clone());
        let out = job.snapshot(&id);
        tokio::task::spawn_blocking(move || {
            match copy_tree_pinned(&from, &to, &reads, &writes, &facts, &job) {
                Ok(()) => *job.state.lock().unwrap() = "complete".into(),
                Err(e) => {
                    *job.error.lock().unwrap() = if job.target.lock().unwrap().is_empty() {
                        e.message
                    } else {
                        format!(
                            "{} Completed destination files are kept. The source is unchanged.",
                            e.message
                        )
                    };
                    *job.state.lock().unwrap() = if job.cancelled.load(Ordering::Relaxed) {
                        "cancelled"
                    } else {
                        "failed"
                    }
                    .into();
                }
            }
        });
        Ok(out)
    }
    /// List this desktop session's bounded copy history.
    pub fn list(&self) -> Value {
        json!({"jobs": self.jobs.lock().unwrap().iter().map(|(id,job)|job.snapshot(id)).collect::<Vec<_>>()})
    }
    /// Cancel all active copies when the desktop shuts down.
    pub fn cancel_all(&self) {
        for job in self.jobs.lock().unwrap().values() {
            job.cancelled.store(true, Ordering::Relaxed);
        }
    }
    /// Return a job's progress, or request cancellation of that same local job.
    pub fn get(&self, id: &str, cancel: bool) -> Result<Value> {
        let jobs = self.jobs.lock().unwrap();
        let job = jobs
            .get(id)
            .ok_or_else(|| Failure::invalid("That copy job is no longer available."))?;
        if cancel {
            job.cancelled.store(true, Ordering::Relaxed);
        }
        Ok(job.snapshot(id))
    }
}

struct Node {
    path: PathBuf,
    meta: zeus_fs::FileMeta,
}
fn checked(path: &Path, roots: &RootCaps) -> Result<zeus_fs::FileMeta> {
    let (parent, name) = roots.parent(path)?;
    parent.entry_metadata(&name).map_err(cap_failure)
}
#[cfg(test)]
fn copy_tree(
    from: &str,
    dest: &str,
    reads: &[String],
    writes: &[String],
    facts: &HashMap<String, FileFacts>,
    job: &Job,
) -> Result<()> {
    copy_tree_pinned(
        from,
        dest,
        &RootCaps::capture(reads)?,
        &RootCaps::capture(writes)?,
        facts,
        job,
    )
}
fn copy_tree_pinned(
    from: &str,
    dest: &str,
    reads: &RootCaps,
    writes: &RootCaps,
    facts: &HashMap<String, FileFacts>,
    job: &Job,
) -> Result<()> {
    let source = reads.path(from)?;
    let dest = writes.path(dest)?;
    let dest_cap = writes.directory(&dest)?;
    if dest.starts_with(&source) {
        return Err(Failure::invalid(
            "Choose an existing destination outside the source folder.",
        ));
    }
    if quarantined_under(facts, &shown(&source)) {
        return Err(quarantine_refusal("This item"));
    }
    let name = source
        .file_name()
        .ok_or_else(|| Failure::invalid("Copy a file or folder, rather than a drive root."))?;
    let target = dest.join(name);
    // Preflight the whole bounded tree before creating output: no silently skipped links or hidden items.
    let mut pending = vec![source.clone()];
    let mut nodes = Vec::new();
    let mut total = 0u64;
    while let Some(path) = pending.pop() {
        job.check()?;
        let meta = checked(&path, reads)?;
        if nodes.len() + pending.len() >= 100_000
            || path.strip_prefix(&source).unwrap().components().count() > 128
        {
            return Err(Failure::invalid(
                "Copy at most 100,000 entries and 128 folder levels at a time.",
            ));
        }
        if meta.is_dir {
            for name in reads
                .directory(&path)?
                .entry_names(100_000 - nodes.len() - pending.len())
                .map_err(cap_failure)?
            {
                pending.push(path.join(name));
                if nodes.len() + pending.len() >= 100_000 {
                    return Err(Failure::invalid("Copy at most 100,000 entries at a time."));
                }
            }
        } else {
            total = total
                .checked_add(meta.len)
                .ok_or_else(|| Failure::invalid("The copy is too large."))?;
        }
        nodes.push(Node { path, meta });
    }
    job.total.store(total, Ordering::Relaxed);
    *job.state.lock().unwrap() = "copying".into();
    *job.target.lock().unwrap() = shown(&target);
    for node in nodes {
        job.check()?;
        let (source_parent, source_name) = reads.parent(&node.path)?;
        if source_parent
            .entry_metadata(&source_name)
            .map_err(cap_failure)?
            != node.meta
        {
            return Err(Failure::invalid(
                "The source changed while copying. Choose it again.",
            ));
        }
        let relative = if node.path == source {
            PathBuf::from(name)
        } else {
            Path::new(name).join(node.path.strip_prefix(&source).unwrap())
        };
        let parent = dest_cap
            .open_dir(relative.parent().unwrap())
            .map_err(cap_failure)?;
        let output_name = relative.file_name().unwrap();
        if node.meta.is_dir {
            parent.create_dir(output_name).map_err(cap_failure)?;
            continue;
        }
        let mut input = source_parent.open_read(&source_name).map_err(cap_failure)?;
        // The open source, not a later path lookup, is what the byte-copy loop consumes.
        let id = zeus_fs::opened_file_metadata(&input).map_err(cap_failure)?;
        if id != node.meta {
            return Err(Failure::invalid("A source file changed while copying."));
        }
        let mut file = parent.create_new(output_name).map_err(cap_failure)?;
        let result = (|| -> Result<()> {
            use std::io::{Read, Write};
            let mut buffer = [0u8; 65536];
            let mut copied = 0u64;
            loop {
                job.check()?;
                let n = input.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                copied += n as u64;
                if copied > node.meta.len {
                    return Err(Failure::invalid("A source file changed while copying."));
                }
                file.write_all(&buffer[..n])?;
                job.bytes.fetch_add(n as u64, Ordering::Relaxed);
            }
            if copied != node.meta.len {
                return Err(Failure::invalid("A source file changed while copying."));
            }
            if zeus_fs::opened_file_metadata(&input).map_err(cap_failure)? != node.meta {
                return Err(Failure::invalid("A source file changed while copying."));
            }
            file.sync_all()?;
            Ok(())
        })();
        if let Err(e) = result {
            if parent.discard_created(output_name, file).is_err() {
                return Err(Failure::new(
                    "app.copy_incomplete",
                    format!(
                        "{} Its incomplete destination could not be removed; inspect it before retrying.",
                        e.message
                    ),
                ));
            }
            return Err(e);
        }
        job.files.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copies_whole_tree_including_hidden_files_and_never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("source");
        let dest = d.path().join("dest");
        std::fs::create_dir_all(src.join("inner")).unwrap();
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(src.join("inner/.hidden"), b"contents").unwrap();
        let root = vec![shown(d.path())];
        let job = Job::default();
        copy_tree(
            src.to_str().unwrap(),
            dest.to_str().unwrap(),
            &root,
            &root,
            &HashMap::new(),
            &job,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(dest.join("source/inner/.hidden")).unwrap(),
            b"contents"
        );
        assert_eq!(job.bytes.load(Ordering::Relaxed), 8);
        assert!(
            copy_tree(
                src.to_str().unwrap(),
                dest.to_str().unwrap(),
                &root,
                &root,
                &HashMap::new(),
                &job
            )
            .is_err()
        );
        assert!(src.join("inner/.hidden").exists());
        assert!(
            copy_tree(
                src.to_str().unwrap(),
                src.join("inner").to_str().unwrap(),
                &root,
                &root,
                &HashMap::new(),
                &job
            )
            .is_err()
        );
    }
    #[test]
    fn cancellation_and_quarantine_prevent_output() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("file");
        let dest = d.path().join("dest");
        std::fs::write(&src, b"keep").unwrap();
        std::fs::create_dir(&dest).unwrap();
        let root = vec![shown(d.path())];
        let job = Job::default();
        job.cancelled.store(true, Ordering::Relaxed);
        assert!(
            copy_tree(
                src.to_str().unwrap(),
                dest.to_str().unwrap(),
                &root,
                &root,
                &HashMap::new(),
                &job
            )
            .is_err()
        );
        assert!(!dest.join("file").exists());
        job.cancelled.store(false, Ordering::Relaxed);
        let mut facts = HashMap::new();
        facts.insert(
            crate::config::norm(&shown(&src.canonicalize().unwrap())),
            FileFacts {
                quarantined: true,
                ..Default::default()
            },
        );
        assert!(
            copy_tree(
                src.to_str().unwrap(),
                dest.to_str().unwrap(),
                &root,
                &root,
                &facts,
                &job
            )
            .is_err()
        );
        assert!(!dest.join("file").exists());
    }
}
