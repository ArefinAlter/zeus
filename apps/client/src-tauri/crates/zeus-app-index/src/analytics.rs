//! Scoped disk-size summaries and bounded duplicate verification. Equal sizes are candidates;
//! only independently read, unchanged SHA-256 content is reported as a verified duplicate group.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zeus_app::failure::{Failure, Result};

use crate::{
    config::norm,
    facts::{FileFacts, quarantined_under},
    paths::{resolve_inside, shown},
    scan,
    search::in_scope,
    store::Index,
};

/// Produce the largest 50 directories/extensions and 100 duplicate-size candidates.
pub fn summarize(index: &Index, roots: &[String]) -> Value {
    let scopes: Vec<_> = roots.iter().map(|r| norm(r)).collect();
    let mut directories: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    let mut extensions: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    let mut sizes: BTreeMap<u64, (usize, Vec<String>)> = BTreeMap::new();
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
        if entry.size >= 1024 {
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
    json!({"generation":index.generation,"files":count,"logical_bytes":bytes,"measurement":"logical_file_size","directories":top(directories),"extensions":top(extensions),
        "duplicate_candidates":duplicate_sizes.into_iter().take(100).map(|(size,(count,paths))|json!({"bytes":size,"files":count,"paths":paths,"truncated":count>32,"verified":false})).collect::<Vec<_>>()})
}

/// Hash 2–32 explicitly selected regular files, up to 1 GiB of reads, and recheck source metadata.
pub fn verify_duplicates(
    paths: &[String],
    roots: &[String],
    facts: &HashMap<String, FileFacts>,
) -> Result<Value> {
    if !(2..=32).contains(&paths.len()) {
        return Err(Failure::invalid("Verify 2 to 32 files at a time."));
    }
    let mut prepared = Vec::new();
    let mut total = 0u64;
    let mut seen = std::collections::HashSet::new();
    for path in paths {
        if scan::is_link(&std::fs::symlink_metadata(path)?) {
            return Err(Failure::invalid(
                "Duplicate verification does not follow links or junctions.",
            ));
        }
        let p = resolve_inside(path, roots)?;
        if !seen.insert(p.clone()) {
            continue;
        }
        let meta = std::fs::symlink_metadata(&p)?;
        if !meta.is_file() || scan::is_link(&meta) {
            return Err(Failure::invalid("Verify regular files only."));
        }
        if quarantined_under(facts, &shown(&p)) {
            return Err(crate::ops::quarantine_refusal("This file"));
        }
        total = total
            .checked_add(meta.len())
            .ok_or_else(|| Failure::invalid("The files are too large."))?;
        if total > 1024 * 1024 * 1024 {
            return Err(Failure::invalid(
                "Verify at most 1 GiB of file content per request.",
            ));
        }
        prepared.push((p, meta));
    }
    if prepared.len() < 2 {
        return Err(Failure::invalid("Choose at least two distinct paths."));
    }
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (path, before) in prepared {
        let mut input = std::fs::File::open(&path)?;
        let opened = input.metadata()?;
        if opened.len() != before.len() || opened.modified().ok() != before.modified().ok() {
            return Err(Failure::new(
                "app.file_changed",
                "A file changed before verification.",
            ));
        }
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        let mut bytes = 0u64;
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            bytes += n as u64;
            if bytes > before.len() {
                return Err(Failure::new(
                    "app.file_changed",
                    "A file grew during verification.",
                ));
            }
            hash.update(&buffer[..n]);
        }
        let after = std::fs::symlink_metadata(&path)?;
        if resolve_inside(&shown(&path), roots)? != path
            || scan::is_link(&after)
            || bytes != before.len()
            || after.len() != before.len()
            || after.modified().ok() != before.modified().ok()
        {
            return Err(Failure::new(
                "app.file_changed",
                "A file changed during verification.",
            ));
        }
        groups
            .entry(hex::encode(hash.finalize()))
            .or_default()
            .push(shown(&path));
    }
    Ok(
        json!({"algorithm":"sha256","bytes_read":total,"groups":groups.into_iter().filter(|(_,p)|p.len()>1).map(|(sha256,paths)|json!({"sha256":sha256,"paths":paths,"verified":true})).collect::<Vec<_>>(),"deletes_files":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
