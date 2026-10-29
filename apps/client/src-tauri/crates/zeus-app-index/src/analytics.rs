//! Scoped disk-size summaries and bounded duplicate verification. Equal sizes are candidates;
//! only independently read, unchanged SHA-256 content is reported as a verified duplicate group.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zeus_app::failure::{Failure, Result};

use crate::{
    config::norm,
    facts::{FileFacts, quarantined_under},
    paths::{RootCaps, shown},
    read::{OpenedRead, changed},
    search::in_scope,
    store::Index,
};

/// Produce the largest 50 directories/extensions and 100 duplicate-size candidates.
pub fn summarize(index: &Index, roots: &[String]) -> Value {
    let scopes: Vec<_> = roots.iter().map(|r| norm(r)).collect();
    let mut directories: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    let mut extensions: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    let mut sizes: BTreeMap<u64, (usize, Vec<String>)> = BTreeMap::new();
    let mut candidate_objects = HashSet::new();
    let mut count = 0usize;
    let mut bytes = 0u64;
    for (path, entry) in &index.entries {
        if entry.dir || !in_scope(path, &scopes) {
            continue;
        }
        count += 1;
        bytes = bytes.saturating_add(entry.size);
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let extension = name
            .rsplit_once('.')
            .map_or("(none)", |(_, ext)| ext)
            .to_lowercase();
        let ex = extensions.entry(extension).or_default();
        ex.0 += 1;
        ex.1 = ex.1.saturating_add(entry.size);
        let mut parent = std::path::Path::new(path).parent();
        for _ in 0..128 {
            let Some(p) = parent else { break };
            let key = shown(p);
            if !in_scope(&key, &scopes) {
                break;
            }
            let dir = directories.entry(key).or_default();
            dir.0 += 1;
            dir.1 = dir.1.saturating_add(entry.size);
            parent = p.parent();
        }
        // Tiny and empty files are not a useful space-recovery candidate.
        let distinct = entry
            .storage
            .as_ref()
            .is_none_or(|facts| candidate_objects.insert(facts.file_id.clone()));
        if entry.size >= 1024 && distinct {
            let group = sizes.entry(entry.size).or_default();
            group.0 += 1;
            if group.1.len() < 32 {
                group.1.push(path.clone());
            }
        }
    }
    let top = |map: BTreeMap<String, (usize, u64)>| {
        let mut rows: Vec<_> = map.into_iter().collect();
        rows.sort_by(|a, b| b.1.1.cmp(&a.1.1).then(a.0.cmp(&b.0)));
        rows.into_iter()
            .take(50)
            .map(|(path, (files, bytes))| json!({"path":path,"files":files,"bytes":bytes}))
            .collect::<Vec<_>>()
    };
    let mut duplicate_sizes: Vec<_> = sizes.into_iter().filter(|(_, x)| x.0 > 1).collect();
    duplicate_sizes.sort_by_key(|(size, (count, _))| {
        std::cmp::Reverse(size.saturating_mul((*count - 1) as u64))
    });
    json!({"generation":index.generation,"files":count,"logical_bytes":bytes,"measurement":"logical_file_size","storage":storage_summary(index,&scopes),"directories":top(directories),"extensions":top(extensions),
        "duplicate_candidates":duplicate_sizes.into_iter().take(100).map(|(size,(count,paths))|json!({"bytes":size,"files":count,"paths":paths,"truncated":count>32,"verified":false})).collect::<Vec<_>>()})
}

/// Last-indexed allocation, counting each known file identity once across the selected roots.
/// This does not claim live free-space savings or disjoint allocation for reflink/shared extents.
fn storage_summary(index: &Index, scopes: &[String]) -> Value {
    #[derive(Clone)]
    struct Object {
        logical: u64,
        allocation: Option<u64>,
        links: Option<u64>,
        aliases: u64,
        consistent: bool,
    }
    let mut objects: HashMap<zeus_types::FileId, Object> = HashMap::new();
    let mut unknown_paths = 0u64;
    for (path, entry) in &index.entries {
        if entry.dir || !in_scope(path, scopes) {
            continue;
        }
        let Some(facts) = &entry.storage else {
            unknown_paths += 1;
            continue;
        };
        objects
            .entry(facts.file_id.clone())
            .and_modify(|o| {
                o.aliases += 1;
                o.consistent &= o.logical == entry.size
                    && o.allocation == facts.allocated_bytes
                    && o.links == facts.hard_links;
            })
            .or_insert(Object {
                logical: entry.size,
                allocation: facts.allocated_bytes,
                links: facts.hard_links,
                aliases: 1,
                consistent: true,
            });
    }
    let mut logical = 0u64;
    let mut allocated = 0u64;
    let mut unknown_allocation = 0u64;
    let mut aliases = 0u64;
    let mut unseen_aliases = 0u64;
    let mut inconsistent = 0u64;
    for object in objects.values() {
        aliases += object.aliases.saturating_sub(1);
        if !object.consistent {
            inconsistent += 1;
            unknown_allocation += 1;
            continue;
        }
        logical = logical.saturating_add(object.logical);
        if let Some(bytes) = object.allocation {
            allocated = allocated.saturating_add(bytes)
        } else {
            unknown_allocation += 1
        }
        unseen_aliases = unseen_aliases.saturating_add(
            object
                .links
                .unwrap_or(object.aliases)
                .saturating_sub(object.aliases),
        );
    }
    json!({"measurement":"last_indexed_filesystem_allocation","known_objects":objects.len(),
        "observed_aliases":aliases,"unobserved_aliases":unseen_aliases,"unknown_identity_paths":unknown_paths,
        "inconsistent_objects":inconsistent,"unique_logical_bytes_known":logical,"allocated_bytes_known":allocated,
        "unknown_allocation_objects":unknown_allocation,"complete":unknown_paths==0&&unknown_allocation==0&&inconsistent==0,
        "reclaimable_bytes":null,"shared_extents_deduplicated":false})
}

/// Hash 2–32 explicitly selected regular files, up to 1 GiB of reads, and recheck source metadata.
pub fn verify_duplicates(
    paths: &[String],
    roots: &[String],
    facts: &HashMap<String, FileFacts>,
) -> Result<Value> {
    verify_duplicates_pinned(paths, &RootCaps::capture(roots)?, facts)
}

/// Hash selected files through enablement-bound read roots, retaining every opened file until the
/// final identity/metadata recheck. Distinct names of one file object are not duplicate copies.
pub fn verify_duplicates_pinned(
    paths: &[String],
    roots: &RootCaps,
    facts: &HashMap<String, FileFacts>,
) -> Result<Value> {
    verify_steps(paths, roots, facts, || {})
}
fn verify_steps(
    paths: &[String],
    roots: &RootCaps,
    facts: &HashMap<String, FileFacts>,
    after_hashes: impl FnOnce(),
) -> Result<Value> {
    if !(2..=32).contains(&paths.len()) {
        return Err(Failure::invalid("Verify 2 to 32 files at a time."));
    }
    let mut prepared = Vec::new();
    let mut total = 0u64;
    let mut seen = std::collections::HashSet::new();
    for path in paths {
        let path = roots.path(path)?;
        if quarantined_under(facts, &shown(&path)) {
            return Err(crate::ops::quarantine_refusal("This file"));
        }
        let input = OpenedRead::open(roots, &path)?;
        if !seen.insert(input.before.file_id.clone().ok_or_else(changed)?) {
            continue;
        }
        total = total
            .checked_add(input.before.len)
            .ok_or_else(|| Failure::invalid("The files are too large."))?;
        if total > 1024 * 1024 * 1024 {
            return Err(Failure::invalid(
                "Verify at most 1 GiB of file content per request.",
            ));
        }
        prepared.push((path, input));
    }
    if prepared.len() < 2 {
        return Err(Failure::invalid(
            "Choose at least two distinct file objects.",
        ));
    }
    let started = std::time::Instant::now();
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (path, input) in &mut prepared {
        input.check_unchanged()?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        let mut bytes = 0u64;
        loop {
            if started.elapsed() > std::time::Duration::from_secs(120) {
                return Err(Failure::new(
                    "app.io",
                    "Duplicate verification exceeded two minutes.",
                ));
            }
            let n = input.file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            bytes += n as u64;
            if bytes > input.before.len {
                return Err(changed());
            }
            hash.update(&buffer[..n]);
        }
        if bytes != input.before.len {
            return Err(changed());
        }
        input.check_unchanged()?;
        groups
            .entry(hex::encode(hash.finalize()))
            .or_default()
            .push(shown(path));
    }
    // A deterministic test boundary also verifies an earlier file cannot change while later files
    // are hashed. Production supplies no callback and no external mutation authority.
    after_hashes();
    for (_, input) in &prepared {
        input.check_unchanged()?;
    }
    Ok(
        json!({"algorithm":"sha256","bytes_read":total,"groups":groups.into_iter().filter(|(_,p)|p.len()>1).map(|(sha256,paths)|json!({"sha256":sha256,"paths":paths,"verified":true})).collect::<Vec<_>>(),"deletes_files":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hard_links_count_once_and_unknown_or_conflicting_measurements_stay_explicit() {
        let mut ix = Index::default();
        let g = ix.begin();
        let path = |s: &str| s.replace('/', std::path::MAIN_SEPARATOR_STR);
        let root = path("/allowed");
        let storage = zeus_fs::storage::FileStorage {
            file_id: zeus_types::FileId::unix(1, 10),
            allocated_bytes: Some(4096),
            hard_links: Some(3),
        };
        for p in ["/allowed/a.bin", "/allowed/alias.bin", "/revoked/alias.bin"] {
            ix.upsert_observed(g, &path(p), (8192, 1, false), 1, Some(storage.clone()));
        }
        let summary = summarize(&ix, std::slice::from_ref(&root));
        assert_eq!(summary["logical_bytes"], 16384);
        assert_eq!(summary["storage"]["allocated_bytes_known"], 4096);
        assert_eq!(summary["storage"]["unique_logical_bytes_known"], 8192);
        assert_eq!(summary["storage"]["observed_aliases"], 1);
        assert_eq!(summary["storage"]["unobserved_aliases"], 1);
        assert_eq!(summary["storage"]["complete"], true);
        assert!(
            summary["duplicate_candidates"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        ix.upsert(g, &path("/allowed/unknown.bin"), 8192, 1, false, 1);
        assert_eq!(
            summarize(&ix, std::slice::from_ref(&root))["storage"]["unknown_identity_paths"],
            1
        );
        let mut conflicting = storage;
        conflicting.allocated_bytes = Some(8192);
        ix.upsert_observed(
            g,
            &path("/allowed/alias.bin"),
            (8192, 1, false),
            1,
            Some(conflicting),
        );
        let summary = summarize(&ix, &[root]);
        assert_eq!(summary["storage"]["allocated_bytes_known"], 0);
        assert_eq!(summary["storage"]["inconsistent_objects"], 1);
        assert_eq!(summary["storage"]["complete"], false);
        assert!(summary["storage"]["reclaimable_bytes"].is_null());
    }
    #[test]
    fn native_scan_refresh_and_restart_retain_alias_accounting() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.bin");
        let alias = tmp.path().join("alias.bin");
        std::fs::write(&a, vec![42u8; 8192]).unwrap();
        std::fs::hard_link(&a, &alias).unwrap();
        let index = std::sync::Mutex::new(Index::default());
        let cfg = crate::config::IndexConfig::default();
        let ex = crate::config::Excluder::new(&cfg);
        let progress = crate::scan::Progress::default();
        crate::scan::scan_root(
            &index,
            tmp.path(),
            &ex,
            1,
            &progress,
            &std::sync::atomic::AtomicBool::new(false),
        );
        let roots = vec![shown(tmp.path())];
        let summary = summarize(&index.lock().unwrap(), &roots);
        assert_eq!(summary["storage"]["known_objects"], 1);
        assert_eq!(summary["storage"]["observed_aliases"], 1);
        let snapshot = index.lock().unwrap().snapshot();
        let loaded = Index::from_snapshot(&snapshot).unwrap();
        assert_eq!(summarize(&loaded, &roots)["storage"], summary["storage"]);
        std::fs::remove_file(&alias).unwrap();
        crate::scan::refresh_dir(&index, tmp.path(), &ex, 2);
        let after = summarize(&index.lock().unwrap(), &roots);
        assert_eq!(after["storage"]["observed_aliases"], 0);
        assert_eq!(after["storage"]["unobserved_aliases"], 0);
    }

    #[test]
    fn same_size_different_content_is_not_reported_as_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        let c = dir.path().join("c");
        std::fs::write(&a, b"same").unwrap();
        std::fs::write(&b, b"same").unwrap();
        std::fs::write(&c, b"else").unwrap();
        let roots = vec![shown(dir.path())];
        let paths = vec![shown(&a), shown(&b), shown(&c)];
        let v = verify_duplicates(&paths, &roots, &HashMap::new()).unwrap();
        assert_eq!(v["groups"].as_array().unwrap().len(), 1);
        assert_eq!(v["groups"][0]["paths"].as_array().unwrap().len(), 2);
        assert!(verify_duplicates(&paths, &[], &HashMap::new()).is_err());
        assert_eq!(std::fs::read(&c).unwrap(), b"else");
    }
    #[test]
    fn summary_never_counts_revoked_roots_or_labels_candidates_verified() {
        let mut ix = Index::default();
        let g = ix.begin();
        for path in ["/allowed/a.bin", "/allowed/b.bin", "/revoked/c.bin"] {
            ix.upsert(
                g,
                &path.replace('/', std::path::MAIN_SEPARATOR_STR),
                1024,
                0,
                false,
                1,
            );
        }
        let v = summarize(
            &ix,
            &["/allowed".replace('/', std::path::MAIN_SEPARATOR_STR)],
        );
        assert_eq!(v["files"], 2);
        assert_eq!(v["duplicate_candidates"][0]["verified"], false);
        assert_eq!(v["logical_bytes"], 2048);
    }
    #[test]
    fn aliases_count_once_and_late_changes_invalidate_all_duplicate_results() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        let alias = dir.path().join("alias");
        std::fs::write(&a, b"same").unwrap();
        std::fs::write(&b, b"same").unwrap();
        std::fs::hard_link(&a, &alias).unwrap();
        let roots = RootCaps::capture(&[shown(dir.path())]).unwrap();
        let paths = vec![shown(&a), shown(&alias), shown(&b)];
        let result = verify_duplicates_pinned(&paths, &roots, &HashMap::new()).unwrap();
        assert_eq!(result["bytes_read"], 8);
        assert_eq!(result["groups"][0]["paths"].as_array().unwrap().len(), 2);
        assert!(verify_duplicates_pinned(&paths[..2], &roots, &HashMap::new()).is_err());
        assert!(
            verify_steps(&paths, &roots, &HashMap::new(), || {
                std::fs::write(&a, b"changed after hashing").unwrap();
            })
            .is_err()
        );
        let facts = HashMap::from([(
            norm(&shown(&b)),
            FileFacts {
                quarantined: true,
                ..FileFacts::default()
            },
        )]);
        assert!(verify_duplicates_pinned(&paths, &roots, &facts).is_err());
    }
}
