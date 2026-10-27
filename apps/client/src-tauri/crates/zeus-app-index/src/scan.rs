//! Walking the folders into the index.
//!
//! A scan visits one folder at a time (no recursion, so a deep tree cannot overflow the stack), skips what the
//! exclusions skip, never follows a link out of the tree, and publishes each folder's entries as it goes so the first
//! results appear before a big scan ends. Entries the scan did not see are removed at the end of a root.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use crate::config::Excluder;
use crate::store::Index;

/// Counters a scan updates and the status route reads.
#[derive(Debug, Default)]
pub struct Progress {
    /// Folders read so far in the current root.
    pub dirs_done: AtomicU64,
    /// Folders found and not yet read in the current root.
    pub dirs_queued: AtomicU64,
    /// Entries seen in this run.
    pub entries: AtomicU64,
}

impl Progress {
    /// Fraction of the current root that is done, from the folders read against the folders known.
    pub fn fraction(&self) -> f64 {
        let done = self.dirs_done.load(Ordering::Relaxed) as f64;
        let queued = self.dirs_queued.load(Ordering::Relaxed) as f64;
        if done + queued == 0.0 {
            0.0
        } else {
            done / (done + queued)
        }
    }

    fn reset(&self) {
        self.dirs_done.store(0, Ordering::Relaxed);
        self.dirs_queued.store(0, Ordering::Relaxed);
    }
}

/// Milliseconds since the epoch of a file time, 0 when unknown.
pub fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64)
}

/// True for a link or a Windows junction or mount point: never followed.
pub fn is_link(meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.is_dir() && meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Display form of a path: no `\\?\` prefix.
pub fn display(p: &Path) -> String {
    let s = p.display().to_string();
    let s = s.strip_prefix(r"\\?\").map_or(s.clone(), str::to_owned);
    if cfg!(windows) {
        s.replace('/', "\\")
    } else {
        s
    }
}

/// What one folder holds, as the scan reads it.
struct Listing {
    files: Vec<(String, u64, i64, bool)>,
    subdirs: Vec<PathBuf>,
}

fn list_dir(dir: &Path, ex: &Excluder) -> Listing {
    let mut out = Listing {
        files: Vec::new(),
        subdirs: Vec::new(),
    };
    let Ok(read) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if ex.skips(&name, &path) || ex.skips_attributes(&name, &meta) {
            continue;
        }
        if meta.is_dir() {
            if is_link(&meta) {
                continue;
            }
            out.files.push((display(&path), 0, mtime_ms(&meta), true));
            out.subdirs.push(path);
        } else if meta.is_file() || meta.file_type().is_symlink() {
            out.files
                .push((display(&path), meta.len(), mtime_ms(&meta), false));
        }
    }
    out
}

/// Read `dir` (one level) into the index in the batch `gen_`; returns the sub-folders found.
pub fn publish_dir(
    index: &Mutex<Index>,
    dir: &Path,
    ex: &Excluder,
    scan: u32,
) -> (Vec<PathBuf>, usize) {
    let l = list_dir(dir, ex);
    let count = l.files.len();
    if let Ok(mut ix) = index.lock() {
        let g = ix.begin();
        for (path, size, mtime, is_dir) in &l.files {
            if !ix.upsert(g, path, *size, *mtime, *is_dir, scan) {
                break;
            }
        }
    }
    (l.subdirs, count)
}

/// Scan `root` completely: everything below it is read, updated, and what vanished is removed. Returns the number of
/// entries seen. `cancel` ends the scan early without pruning.
pub fn scan_root(
    index: &Mutex<Index>,
    root: &Path,
    ex: &Excluder,
    scan: u32,
    progress: &Progress,
    cancel: &AtomicBool,
) -> usize {
    progress.reset();
    if !root.is_dir() {
        return 0;
    }
    let mut total = 0usize;
    let mut stack = vec![root.to_path_buf()];
    progress.dirs_queued.store(1, Ordering::Relaxed);
    while let Some(dir) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return total;
        }
        let (subs, n) = publish_dir(index, &dir, ex, scan);
        total += n;
        progress.entries.fetch_add(n as u64, Ordering::Relaxed);
        progress.dirs_done.fetch_add(1, Ordering::Relaxed);
        progress.dirs_queued.fetch_sub(1, Ordering::Relaxed);
        progress
            .dirs_queued
            .fetch_add(subs.len() as u64, Ordering::Relaxed);
        stack.extend(subs);
    }
    if let Ok(mut ix) = index.lock() {
        let g = ix.begin();
        ix.prune_unseen(g, &display(root), scan);
    }
    total
}

/// Bring one folder up to date after the watcher saw a change in it: read its direct children, drop the ones that
/// are gone, and scan any sub-folder that is new. A folder that is gone is removed with its subtree.
pub fn refresh_dir(index: &Mutex<Index>, dir: &Path, ex: &Excluder, scan: u32) {
    let key = display(dir);
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        if let Ok(mut ix) = index.lock() {
            let g = ix.begin();
            ix.remove_tree(g, &key);
        }
        return;
    };
    if !meta.is_dir() || is_link(&meta) {
        return;
    }
    let known: std::collections::HashSet<String> = index
        .lock()
        .map(|ix| ix.children(&key).into_iter().collect())
        .unwrap_or_default();
    let l = list_dir(dir, ex);
    let present: std::collections::HashSet<&str> = l.files.iter().map(|f| f.0.as_str()).collect();
    if let Ok(mut ix) = index.lock() {
        let g = ix.begin();
        for gone in known.iter().filter(|k| !present.contains(k.as_str())) {
            ix.remove_tree(g, gone);
        }
        for (path, size, mtime, is_dir) in &l.files {
            if !ix.upsert(g, path, *size, *mtime, *is_dir, scan) {
                break;
            }
        }
    }
    let progress = Progress::default();
    let cancel = AtomicBool::new(false);
    for sub in l.subdirs {
        if !known.contains(&display(&sub)) {
            scan_subtree(index, &sub, ex, scan, &progress, &cancel);
        }
    }
}

/// Scan a new sub-folder fully without pruning anything outside it.
fn scan_subtree(
    index: &Mutex<Index>,
    dir: &Path,
    ex: &Excluder,
    scan: u32,
    progress: &Progress,
    cancel: &AtomicBool,
) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let (subs, _) = publish_dir(index, &d, ex, scan);
        progress.dirs_done.fetch_add(1, Ordering::Relaxed);
        stack.extend(subs);
    }
}

/// Bring one path up to date: a file or folder that exists is stored, one that does not is removed. A folder is
/// refreshed one level deep.
pub fn refresh_path(index: &Mutex<Index>, path: &Path, ex: &Excluder, scan: u32) {
    let key = display(path);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match std::fs::symlink_metadata(path) {
        Err(_) => {
            if let Ok(mut ix) = index.lock() {
                let g = ix.begin();
                ix.remove_tree(g, &key);
            }
        }
        Ok(meta) => {
            if ex.skips(&name, path)
                || ex.skips_attributes(&name, &meta)
                || is_link(&meta) && meta.is_dir()
            {
                if let Ok(mut ix) = index.lock() {
                    let g = ix.begin();
                    ix.remove_tree(g, &key);
                }
                return;
            }
            if meta.is_dir() {
                if let Ok(mut ix) = index.lock() {
                    let g = ix.begin();
                    ix.upsert(g, &key, 0, mtime_ms(&meta), true, scan);
                }
                refresh_dir(index, path, ex, scan);
            } else if let Ok(mut ix) = index.lock() {
                let g = ix.begin();
                ix.upsert(g, &key, meta.len(), mtime_ms(&meta), false, scan);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::IndexConfig;

    fn ex() -> Excluder {
        Excluder::new(&IndexConfig::default())
    }

    fn put(p: &Path, text: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn a_scan_reads_the_tree_skips_exclusions_and_removes_what_vanished() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        put(&root.join("a.txt"), "hello");
        put(&root.join("sub/b.png"), "png");
        put(&root.join("sub/deep/c.zip"), "zip");
        put(&root.join("movie.zpart"), "partial");
        put(&root.join(".zeus/trash.bin"), "x");
        put(&root.join("node_modules/pkg/index.js"), "x");
        let index = Mutex::new(Index::default());
        let progress = Progress::default();
        let cancel = AtomicBool::new(false);
        scan_root(&index, root, &ex(), 1, &progress, &cancel);
        let names: Vec<String> = index
            .lock()
            .unwrap()
            .entries
            .keys()
            .map(|k| k.rsplit(['\\', '/']).next().unwrap().to_owned())
            .collect();
        assert_eq!(names.iter().filter(|n| n.as_str() == "a.txt").count(), 1);
        assert!(names.contains(&"c.zip".to_owned()) && names.contains(&"deep".to_owned()));
        assert!(!names.iter().any(|n| n.ends_with(".zpart")
            || n == ".zeus"
            || n == "node_modules"
            || n == "index.js"));
        assert!(progress.fraction() > 0.99);

        std::fs::remove_file(root.join("a.txt")).unwrap();
        std::fs::remove_dir_all(root.join("sub/deep")).unwrap();
        put(&root.join("new.md"), "# hi");
        scan_root(&index, root, &ex(), 2, &progress, &cancel);
        let ix = index.lock().unwrap();
        let keys: Vec<&String> = ix.entries.keys().collect();
        assert!(
            !keys
                .iter()
                .any(|k| k.ends_with("a.txt") || k.ends_with("c.zip"))
        );
        assert!(keys.iter().any(|k| k.ends_with("new.md")));
    }

    #[test]
    fn a_path_refresh_adds_updates_and_removes_without_a_full_scan() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        put(&root.join("x/one.txt"), "1");
        let index = Mutex::new(Index::default());
        scan_root(
            &index,
            root,
            &ex(),
            1,
            &Progress::default(),
            &AtomicBool::new(false),
        );
        let before = index.lock().unwrap().generation;
        put(&root.join("x/two.txt"), "22");
        refresh_path(&index, &root.join("x/two.txt"), &ex(), 2);
        assert!(
            index
                .lock()
                .unwrap()
                .entries
                .keys()
                .any(|k| k.ends_with("two.txt"))
        );
        // A new folder with files arrives (a moved-in directory): its subtree is scanned.
        put(&root.join("moved/in/deep.bin"), "d");
        refresh_path(&index, &root.join("moved"), &ex(), 3);
        assert!(
            index
                .lock()
                .unwrap()
                .entries
                .keys()
                .any(|k| k.ends_with("deep.bin"))
        );
        // A folder disappears with everything in it.
        std::fs::remove_dir_all(root.join("x")).unwrap();
        refresh_path(&index, &root.join("x"), &ex(), 4);
        let ix = index.lock().unwrap();
        assert!(
            !ix.entries
                .keys()
                .any(|k| k.ends_with("one.txt") || k.ends_with("two.txt"))
        );
        assert!(ix.generation > before);
        let d = ix.delta(before);
        assert!(!d.reset && !d.removes.is_empty() && !d.upserts.is_empty());
    }

    #[test]
    fn a_cancelled_scan_does_not_prune() {
        let dir = tempfile::tempdir().unwrap();
        put(&dir.path().join("keep.txt"), "k");
        let index = Mutex::new(Index::default());
        scan_root(
            &index,
            dir.path(),
            &ex(),
            1,
            &Progress::default(),
            &AtomicBool::new(false),
        );
        let stop = AtomicBool::new(true);
        scan_root(&index, dir.path(), &ex(), 2, &Progress::default(), &stop);
        assert_eq!(index.lock().unwrap().entries.len(), 1);
    }
}
