//! Live, paged folder listings, independent of search exclusions and index limits.
use crate::{paths, scan, store::Row};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use zeus_app::failure::Result;

/// Read one page in stable path order, retaining at most `limit + 1` entries in memory.
/// Links and reparse points are omitted; opening a folder always rechecks the read grant.
pub fn list(path: &str, after: &str, limit: usize, roots: &[String]) -> Result<Value> {
    list_pinned(path, after, limit, &paths::RootCaps::capture(roots)?)
}

/// Read one bounded page through the settings service's retained read roots.
pub fn list_pinned(
    path: &str,
    after: &str,
    limit: usize,
    roots: &paths::RootCaps,
) -> Result<Value> {
    list_filtered_pinned(path, after, limit, roots, false)
}

/// List directories separately for a lazily expanded folder tree. File rows never consume its page.
pub fn list_filtered_pinned(
    path: &str,
    after: &str,
    limit: usize,
    roots: &paths::RootCaps,
    directories_only: bool,
) -> Result<Value> {
    let folder = roots.path(path)?;
    let directory = roots.directory(&folder)?;
    let limit = limit.clamp(1, 500);
    let mut rows = BTreeMap::new();
    let mut skipped = 0usize;
    for entry in directory.iter_names().map_err(paths::cap_failure)? {
        let name = entry.map_err(paths::cap_failure)?;
        let p = folder.join(&name);
        let key = paths::shown(&p);
        if key.as_str() <= after {
            continue;
        }
        let meta = match directory.entry_os_metadata(&name) {
            Ok(m) if !scan::is_link(&m) && (m.is_file() || m.is_dir()) => m,
            _ => {
                skipped += 1;
                continue;
            }
        };
        if directories_only && !meta.is_dir() {
            continue;
        }
        rows.insert(
            key.clone(),
            Row {
                path: key,
                size: if meta.is_dir() { 0 } else { meta.len() },
                mtime: scan::mtime_ms(&meta),
                is_dir: meta.is_dir(),
            },
        );
        if rows.len() > limit + 1 {
            rows.pop_last();
        }
    }
    let more = rows.len() > limit;
    if more {
        rows.pop_last();
    }
    let next = if more {
        rows.last_key_value().map(|(k, _)| k.clone())
    } else {
        None
    };
    Ok(
        json!({"path": paths::shown(&folder), "files": rows.into_values().collect::<Vec<_>>(), "next":next, "skipped":skipped}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_listing_pages_excluded_and_new_files_without_an_index() {
        let d = tempfile::tempdir().unwrap();
        for n in [".hidden", "a.txt", "b.txt", "new"] {
            std::fs::write(d.path().join(n), n).unwrap();
        }
        let roots = vec![d.path().to_string_lossy().into_owned()];
        let a = list(&roots[0], "", 2, &roots).unwrap();
        let b = list(&roots[0], a["next"].as_str().unwrap(), 2, &roots).unwrap();
        assert_eq!(a["files"].as_array().unwrap().len(), 2);
        assert_eq!(b["files"].as_array().unwrap().len(), 2);
        assert!(b["next"].is_null());
        assert!(a["files"][0]["path"].as_str().unwrap().ends_with(".hidden"));
        let outside = tempfile::tempdir().unwrap();
        assert!(list(outside.path().to_str().unwrap(), "", 2, &roots).is_err());
    }
    #[test]
    fn directory_pages_are_not_crowded_out_by_files() {
        let d = tempfile::tempdir().unwrap();
        for n in 0..510 {
            std::fs::write(d.path().join(format!("a{n:04}.txt")), b"file").unwrap();
        }
        for name in ["x-empty", "y-empty", "z-empty"] {
            std::fs::create_dir(d.path().join(name)).unwrap();
        }
        let root = d.path().to_string_lossy().into_owned();
        let caps = paths::RootCaps::capture(std::slice::from_ref(&root)).unwrap();
        let first = list_filtered_pinned(&root, "", 2, &caps, true).unwrap();
        let rows = first["files"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r["is_dir"] == true));
        assert!(rows[0]["path"].as_str().unwrap().ends_with("x-empty"));
        let second =
            list_filtered_pinned(&root, first["next"].as_str().unwrap(), 2, &caps, true).unwrap();
        assert_eq!(second["files"].as_array().unwrap().len(), 1);
        assert!(
            second["files"][0]["path"]
                .as_str()
                .unwrap()
                .ends_with("z-empty")
        );
        assert!(second["next"].is_null());
        let outside = tempfile::tempdir().unwrap();
        assert!(
            list_filtered_pinned(outside.path().to_str().unwrap(), "", 2, &caps, true).is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn links_do_not_reveal_an_outside_folder() {
        let d = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/", d.path().join("escape")).unwrap();
        let roots = vec![d.path().to_string_lossy().into_owned()];
        let out = list(&roots[0], "", 2, &roots).unwrap();
        assert!(out["files"].as_array().unwrap().is_empty());
        assert_eq!(out["skipped"], 1);
    }
}
