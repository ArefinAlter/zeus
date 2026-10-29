//! The in-memory index and its snapshot file.
//!
//! The whole index lives in a sorted map (path to size, date and kind): a folder's subtree is one range, a delta is a
//! slice of the change log, and nothing here talks to the disk except the snapshot. The snapshot is a compact binary
//! file written atomically after scans and (debounced) after watcher changes, and read at start so the first search
//! needs no scan. One writer holds the lock at a time; every critical section is short.

use std::collections::{BTreeMap, VecDeque};
use std::ops::Bound;
use std::path::Path;

use serde::Serialize;

/// Most entries the index keeps; past it the scan stops adding and says so.
pub const CAP: usize = 500_000;
/// Changes remembered for deltas; a client further behind gets the whole list.
const LOG_CAP: usize = 200_000;

const MAGIC: &[u8; 4] = b"ZIX1";

/// One file or folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// Size in bytes (0 for folders).
    pub size: u64,
    /// Modified time, epoch milliseconds.
    pub mtime: i64,
    /// A folder.
    pub dir: bool,
    /// The scan that last saw it (not persisted).
    pub scan: u32,
    /// Lossy trigram mask for fast negative filename checks, rebuilt from paths after loading.
    pub(crate) name_mask: u128,
    /// Lossy trigram mask for fast negative full-path checks.
    pub(crate) path_mask: u128,
}

/// One row as the page reads it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Row {
    /// Absolute path with the platform separator.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// Modified time, epoch milliseconds.
    pub mtime: i64,
    /// A folder.
    pub is_dir: bool,
}

/// The sorted index with its change log.
#[derive(Debug, Default)]
pub struct Index {
    /// Every entry by path.
    pub entries: BTreeMap<String, Entry>,
    /// Counts up with every batch of changes.
    pub generation: u64,
    /// When the last full scan finished, epoch milliseconds.
    pub last_scan: Option<i64>,
    /// The cap stopped a scan from adding more.
    pub capped: bool,
    log: VecDeque<(u64, String)>,
    /// The generation before the oldest change still in the log.
    log_base: u64,
}

/// What the page needs to catch up.
#[derive(Debug, Serialize)]
pub struct Delta {
    /// The generation the answer is current at.
    pub generation: u64,
    /// The page's copy is too old (or empty): replace it with `files`.
    pub reset: bool,
    /// The whole list, when `reset`.
    pub files: Vec<Row>,
    /// Entries to add or replace.
    pub upserts: Vec<Row>,
    /// Paths to drop.
    pub removes: Vec<String>,
}

fn row(path: &str, e: &Entry) -> Row {
    Row {
        path: path.to_owned(),
        size: e.size,
        mtime: e.mtime,
        is_dir: e.dir,
    }
}

fn sep() -> char {
    std::path::MAIN_SEPARATOR
}

/// The exclusive upper bound of `prefix + sep` keys: the separator's successor.
fn after_sep() -> char {
    char::from_u32(sep() as u32 + 1).unwrap_or('\u{10FFFF}')
}

impl Index {
    /// Start a batch of changes: the generation moves once, so everything in the batch shares it.
    pub fn begin(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    fn record(&mut self, gen_: u64, path: &str) {
        self.log.push_back((gen_, path.to_owned()));
        if self.log.len() > LOG_CAP
            && let Some((g, _)) = self.log.pop_front()
        {
            self.log_base = self.log_base.max(g);
        }
    }

    /// Insert or update one entry in batch `gen_`; false when the cap refused a new entry.
    pub fn upsert(
        &mut self,
        gen_: u64,
        path: &str,
        size: u64,
        mtime: i64,
        dir: bool,
        scan: u32,
    ) -> bool {
        match self.entries.get_mut(path) {
            Some(e) if e.size == size && e.mtime == mtime && e.dir == dir => {
                e.scan = scan;
                true
            }
            Some(e) => {
                *e = Entry {
                    size,
                    mtime,
                    dir,
                    scan,
                    name_mask: trigram_mask(path.rsplit(['/', '\\']).next().unwrap_or(path)),
                    path_mask: trigram_mask(path),
                };
                self.record(gen_, path);
                true
            }
            None => {
                if self.entries.len() >= CAP {
                    self.capped = true;
                    return false;
                }
                self.entries.insert(
                    path.to_owned(),
                    Entry {
                        size,
                        mtime,
                        dir,
                        scan,
                        name_mask: trigram_mask(path.rsplit(['/', '\\']).next().unwrap_or(path)),
                        path_mask: trigram_mask(path),
                    },
                );
                self.record(gen_, path);
                true
            }
        }
    }

    /// Remove `path` and everything below it in batch `gen_`; returns how many entries went.
    pub fn remove_tree(&mut self, gen_: u64, path: &str) -> usize {
        let mut gone: Vec<String> = Vec::new();
        if self.entries.contains_key(path) {
            gone.push(path.to_owned());
        }
        let lo = format!("{path}{}", sep());
        let hi = format!("{path}{}", after_sep());
        gone.extend(
            self.entries
                .range::<str, _>((Bound::Included(lo.as_str()), Bound::Excluded(hi.as_str())))
                .map(|(k, _)| k.clone()),
        );
        for k in &gone {
            self.entries.remove(k);
            self.record(gen_, k);
        }
        gone.len()
    }

    /// Remove entries under `root` that the scan `scan` did not see.
    pub fn prune_unseen(&mut self, gen_: u64, root: &str, scan: u32) -> usize {
        let lo = format!("{root}{}", sep());
        let hi = format!("{root}{}", after_sep());
        let stale: Vec<String> = self
            .entries
            .range::<str, _>((Bound::Included(lo.as_str()), Bound::Excluded(hi.as_str())))
            .filter(|(_, e)| e.scan != scan)
            .map(|(k, _)| k.clone())
            .collect();
        for k in &stale {
            self.entries.remove(k);
            self.record(gen_, k);
        }
        stale.len()
    }

    /// Direct children of `dir` (one level), as paths.
    pub fn children(&self, dir: &str) -> Vec<String> {
        let lo = format!("{dir}{}", sep());
        let hi = format!("{dir}{}", after_sep());
        self.entries
            .range::<str, _>((Bound::Included(lo.as_str()), Bound::Excluded(hi.as_str())))
            .map(|(k, _)| k)
            .filter(|k| !k[lo.len()..].contains(sep()))
            .cloned()
            .collect()
    }

    /// Entries (files and folders) below `dir`, and the bytes of the files.
    pub fn under(&self, dir: &str) -> (usize, u64) {
        let lo = format!("{dir}{}", sep());
        let hi = format!("{dir}{}", after_sep());
        self.entries
            .range::<str, _>((Bound::Included(lo.as_str()), Bound::Excluded(hi.as_str())))
            .fold((0, 0), |(n, b), (_, e)| (n + 1, b + e.size))
    }

    /// Drop every entry that is not below one of `roots`.
    pub fn keep_only(&mut self, gen_: u64, roots: &[String]) -> usize {
        let outside: Vec<String> = self
            .entries
            .keys()
            .filter(|k| {
                !roots.iter().any(|r| {
                    *k == r || (k.starts_with(r.as_str()) && k[r.len()..].starts_with(sep()))
                })
            })
            .cloned()
            .collect();
        for k in &outside {
            self.entries.remove(k);
            self.record(gen_, k);
        }
        outside.len()
    }

    /// The whole list.
    pub fn all(&self) -> Vec<Row> {
        self.entries.iter().map(|(k, e)| row(k, e)).collect()
    }

    /// What changed since generation `since`.
    pub fn delta(&self, since: u64) -> Delta {
        if since == 0 || since < self.log_base || since > self.generation {
            return Delta {
                generation: self.generation,
                reset: true,
                files: self.all(),
                upserts: Vec::new(),
                removes: Vec::new(),
            };
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut upserts = Vec::new();
        let mut removes = Vec::new();
        for (g, p) in self.log.iter().rev() {
            if *g <= since {
                break;
            }
            if !seen.insert(p.as_str()) {
                continue;
            }
            match self.entries.get(p) {
                Some(e) => upserts.push(row(p, e)),
                None => removes.push(p.clone()),
            }
        }
        Delta {
            generation: self.generation,
            reset: false,
            files: Vec::new(),
            upserts,
            removes,
        }
    }

    // ------------------------------------------------------------ snapshot

    /// Serialize for the snapshot file.
    pub fn snapshot(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.entries.len() * 64 + 64);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.generation.to_le_bytes());
        out.extend_from_slice(&self.last_scan.unwrap_or(-1).to_le_bytes());
        out.extend_from_slice(&(self.entries.len() as u64).to_le_bytes());
        for (k, e) in &self.entries {
            out.extend_from_slice(&(k.len() as u32).to_le_bytes());
            out.extend_from_slice(k.as_bytes());
            out.extend_from_slice(&e.size.to_le_bytes());
            out.extend_from_slice(&e.mtime.to_le_bytes());
            out.push(u8::from(e.dir));
        }
        let sum = fnv(&out);
        out.extend_from_slice(&sum.to_le_bytes());
        out
    }

    /// Read a snapshot; `None` when it is damaged or from another format.
    pub fn from_snapshot(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 4 + 8 + 8 + 8 + 8 || &bytes[..4] != MAGIC {
            return None;
        }
        let (body, tail) = bytes.split_at(bytes.len() - 8);
        if fnv(body).to_le_bytes() != tail {
            return None;
        }
        let mut at = 4;
        let take = |n: usize, at: &mut usize| -> Option<&[u8]> {
            let s = body.get(*at..at.checked_add(n)?)?;
            *at += n;
            Some(s)
        };
        let generation = u64::from_le_bytes(take(8, &mut at)?.try_into().ok()?);
        let last = i64::from_le_bytes(take(8, &mut at)?.try_into().ok()?);
        let count = u64::from_le_bytes(take(8, &mut at)?.try_into().ok()?);
        if count > CAP as u64 {
            return None;
        }
        let mut entries = BTreeMap::new();
        for _ in 0..count {
            let len = u32::from_le_bytes(take(4, &mut at)?.try_into().ok()?) as usize;
            if len == 0 || len > 32768 {
                return None;
            }
            let key = std::str::from_utf8(take(len, &mut at)?).ok()?.to_owned();
            if key.contains('\0') || entries.contains_key(&key) {
                return None;
            }
            let size = u64::from_le_bytes(take(8, &mut at)?.try_into().ok()?);
            let mtime = i64::from_le_bytes(take(8, &mut at)?.try_into().ok()?);
            let dir = match take(1, &mut at)?[0] {
                0 => false,
                1 => true,
                _ => return None,
            };
            entries.insert(
                key.clone(),
                Entry {
                    size,
                    mtime,
                    dir,
                    scan: 0,
                    name_mask: trigram_mask(key.rsplit(['/', '\\']).next().unwrap_or(&key)),
                    path_mask: trigram_mask(&key),
                },
            );
        }
        if at != body.len() {
            return None;
        }
        Some(Self {
            entries,
            generation,
            last_scan: (last >= 0).then_some(last),
            capped: false,
            log: VecDeque::new(),
            log_base: generation,
        })
    }

    /// Write the snapshot to `path` atomically.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        zeus_app::engine_config::write_atomic(path, &self.snapshot())
    }

    /// Read the snapshot at `path`.
    pub fn load(path: &Path) -> Option<Self> {
        if std::fs::metadata(path).ok()?.len() > 512 * 1024 * 1024 {
            return None;
        }
        Self::from_snapshot(&std::fs::read(path).ok()?)
    }
}

/// A two-bit trigram Bloom mask. A missing bit proves absence; collisions only cost an exact check.
pub(crate) fn trigram_mask(text: &str) -> u128 {
    let folded = text.to_lowercase();
    let mut mask = 0u128;
    for part in folded.as_bytes().windows(3) {
        let value = (u32::from(part[0]) << 16) | (u32::from(part[1]) << 8) | u32::from(part[2]);
        let mixed = value.wrapping_mul(0x9e37_79b1).rotate_left(13);
        mask |= 1u128 << (mixed & 127);
        mask |= 1u128 << ((mixed >> 7) & 127);
    }
    mask
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> String {
        s.replace('/', &sep().to_string())
    }

    #[test]
    fn deltas_carry_only_what_changed_and_reset_when_too_old() {
        let mut ix = Index::default();
        let g = ix.begin();
        ix.upsert(g, &p("/r/a"), 1, 10, false, 1);
        ix.upsert(g, &p("/r/b"), 2, 10, false, 1);
        let first = ix.delta(0);
        assert!(first.reset && first.files.len() == 2);
        let seen = ix.generation;
        let g = ix.begin();
        ix.upsert(g, &p("/r/a"), 5, 11, false, 2);
        ix.remove_tree(g, &p("/r/b"));
        ix.upsert(g, &p("/r/c"), 3, 11, false, 2);
        let d = ix.delta(seen);
        assert!(!d.reset);
        assert_eq!(d.upserts.len(), 2);
        assert_eq!(d.removes, vec![p("/r/b")]);
        assert!(ix.delta(ix.generation).upserts.is_empty());
        assert!(ix.delta(ix.generation + 5).reset);
    }

    #[test]
    fn a_folder_removal_takes_its_subtree_and_nothing_next_to_it() {
        let mut ix = Index::default();
        let g = ix.begin();
        for path in ["/r/d", "/r/d/x", "/r/d/y/z", "/r/dd", "/r/d.txt"] {
            ix.upsert(g, &p(path), 1, 1, path == "/r/d" || path == "/r/d/y", 1);
        }
        let g = ix.begin();
        assert_eq!(ix.remove_tree(g, &p("/r/d")), 3);
        let left: Vec<_> = ix.entries.keys().cloned().collect();
        assert_eq!(left, vec![p("/r/d.txt"), p("/r/dd")]);
    }

    #[test]
    fn prune_unseen_and_children_and_keep_only() {
        let mut ix = Index::default();
        let g = ix.begin();
        ix.upsert(g, &p("/r/a"), 1, 1, false, 1);
        ix.upsert(g, &p("/r/sub"), 0, 1, true, 1);
        ix.upsert(g, &p("/r/sub/b"), 1, 1, false, 1);
        ix.upsert(g, &p("/other/c"), 1, 1, false, 1);
        let g = ix.begin();
        ix.upsert(g, &p("/r/a"), 1, 1, false, 2);
        assert_eq!(ix.prune_unseen(g, &p("/r"), 2), 2);
        assert_eq!(ix.children(&p("/r")), vec![p("/r/a")]);
        assert_eq!(ix.keep_only(g, &[p("/r")]), 1);
        assert_eq!(ix.entries.len(), 1);
    }

    #[test]
    fn the_snapshot_round_trips_and_detects_damage() {
        let mut ix = Index::default();
        let g = ix.begin();
        ix.upsert(g, &p("/r/é ü.txt"), 42, 1_700_000_000_000, false, 1);
        ix.upsert(g, &p("/r/dir"), 0, 5, true, 1);
        ix.last_scan = Some(99);
        let bytes = ix.snapshot();
        let back = Index::from_snapshot(&bytes).unwrap();
        assert_eq!(back.entries.len(), 2);
        assert_eq!(back.last_scan, Some(99));
        assert_eq!(back.entries[&p("/r/é ü.txt")].size, 42);
        let mut bad = bytes.clone();
        bad[20] ^= 0xff;
        assert!(Index::from_snapshot(&bad).is_none());
        assert!(Index::from_snapshot(&bytes[..10]).is_none());
    }

    #[test]
    fn the_cap_refuses_new_entries_and_says_so() {
        let mut ix = Index::default();
        let g = ix.begin();
        for i in 0..CAP + 3 {
            let _ = ix.upsert(g, &format!("/r/{i}"), 1, 1, false, 1);
        }
        assert_eq!(ix.entries.len(), CAP);
        assert!(ix.capped);
    }
}
