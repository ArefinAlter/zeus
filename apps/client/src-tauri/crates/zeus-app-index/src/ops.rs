//! File operations on the person's click: rename, move and delete inside the download and explicitly managed folders.
//!
//! These are the app acting for the person at the window. The daemon's credential for this app has no file write
//! permission, and an agent has no way to reach these routes. Rules: only inside download or explicitly managed folders (search-only
//! folders are read-only), never replace an existing file, never touch a file the daemon quarantined (that needs
//! `zeus files organize` and an approval at a terminal), deletes go to the recycle bin or trash.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use zeus_app::failure::{Failure, Result};

use crate::facts::{FileFacts, quarantined_under};
use crate::paths::{RootCaps, cap_failure, check_name, resolve_inside, resolve_new_inside, shown};

use std::collections::HashMap;

/// The sentence for a refused quarantined file.
pub fn quarantine_refusal(what: &str) -> Failure {
    Failure::new(
        "policy.quarantined",
        format!(
            "{what} is quarantined. Zeus does not move, rename or delete a quarantined file from here: use `zeus files organize` at a terminal (it asks you first), or show it in its folder and decide there."
        ),
    )
}

/// Move `from` to `to` without ever replacing something at `to`.
pub fn move_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use zeus_fs::{Filesystem, FsError, RealFs};
    RealFs.rename_noreplace(from, to).map_err(|e| match e {
        FsError::AlreadyExists => std::io::Error::from(std::io::ErrorKind::AlreadyExists),
        FsError::Io { source, .. } => source,
        _ => std::io::Error::other("Atomic no-replace move is unavailable."),
    })
}

fn already(name: &str) -> Failure {
    Failure::new(
        "app.exists",
        format!("{name} already exists there. Zeus never replaces a file; choose another name."),
    )
}

/// Rename the file or folder at `path` to `name` (a single name) in the same folder.
pub fn rename(
    path: &str,
    name: &str,
    write_roots: &[String],
    facts: &HashMap<String, FileFacts>,
) -> Result<Value> {
    check_name(name)?;
    let from = resolve_inside(path, write_roots)?;
    if quarantined_under(facts, &shown(&from)) {
        return Err(quarantine_refusal("This file"));
    }
    let parent = from
        .parent()
        .ok_or_else(|| Failure::invalid("That path has no folder."))?;
    let to = parent.join(name);
    move_noreplace(&from, &to).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => already(name),
        _ => Failure::new(
            "app.io",
            format!("The file could not be renamed ({:?}).", e.kind()),
        ),
    })?;
    Ok(json!({"ok": true, "from": shown(&from), "path": shown(&to)}))
}

/// Move the file or folder at `path` into the folder `dir` (created if needed, inside the download folders).
pub fn move_to(
    path: &str,
    dir: &str,
    write_roots: &[String],
    facts: &HashMap<String, FileFacts>,
) -> Result<Value> {
    let from = resolve_inside(path, write_roots)?;
    if quarantined_under(facts, &shown(&from)) {
        return Err(quarantine_refusal("This file"));
    }
    let dest = resolve_new_inside(dir, write_roots)?;
    let name = from
        .file_name()
        .ok_or_else(|| Failure::invalid("That path has no name."))?
        .to_string_lossy()
        .into_owned();
    if dest == from || dest.starts_with(&from) {
        return Err(Failure::invalid("A folder cannot be moved into itself."));
    }
    std::fs::create_dir_all(&dest)?;
    let to = dest.join(&name);
    if to == from {
        return Err(Failure::invalid("It is in that folder already."));
    }
    move_noreplace(&from, &to).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => already(&name),
        _ => Failure::new(
            "app.io",
            format!("The file could not be moved ({:?}).", e.kind()),
        ),
    })?;
    Ok(json!({"ok": true, "from": shown(&from), "path": shown(&to)}))
}

/// Delete using fresh explicitly supplied roots for direct local library callers. Desktop routes
/// use `delete_pinned` with the service's previously recorded enablement identities.
pub fn delete(
    paths: &[String],
    permanent: bool,
    write_roots: &[String],
    facts: &HashMap<String, FileFacts>,
) -> Result<Value> {
    delete_pinned(
        paths,
        permanent,
        &RootCaps::capture(write_roots)?,
        facts,
        None,
    )
}

/// Delete through retained roots. Reversible trash is a same-volume move with the existing durable
/// operation receipt; it can be restored with guarded Undo. Platform recycle APIs accept only
/// ambient paths, so this route never hands an approved item back to such a path-based deleter.
pub fn delete_pinned(
    paths: &[String],
    permanent: bool,
    roots: &RootCaps,
    facts: &HashMap<String, FileFacts>,
    history: Option<&crate::history::Journal>,
) -> Result<Value> {
    if paths.is_empty() || paths.len() > 500 {
        return Err(Failure::invalid("Delete 1 to 500 items at a time."));
    }
    let mut targets = Vec::new();
    for path in paths {
        let target = roots.path(path)?;
        let scopes = roots.paths();
        if scopes
            .iter()
            .any(|r| crate::config::norm(r) == crate::config::norm(&shown(&target)))
        {
            return Err(Failure::invalid(
                "An enabled folder itself cannot be deleted here.",
            ));
        }
        if quarantined_under(facts, &shown(&target)) {
            return Err(quarantine_refusal("This item holds a quarantined file and"));
        }
        let (parent, name) = roots.parent(&target)?;
        let meta = parent.entry_metadata(&name).map_err(cap_failure)?;
        targets.push((target, parent, name, meta));
    }
    let mut trashed = Vec::new();
    let mut deleted = Vec::new();
    let mut failed = Vec::new();
    for (path, parent, name, meta) in targets {
        let label = shown(&path);
        let result = if permanent {
            remove_tree(&parent, &name, &meta)
        } else if let Some(journal) = history {
            (|| {
                let scope = roots
                    .scopes()
                    .into_iter()
                    .map(PathBuf::from)
                    .filter(|p| path.starts_with(p))
                    .max_by_key(|p| p.components().count())
                    .ok_or_else(|| Failure::invalid("The managed root is unavailable."))?;
                // The source may never be the private trash ancestor itself.
                let trash = scope.join(".zeus/trash");
                if trash.starts_with(&path) || path.starts_with(&trash) {
                    return Err(Failure::invalid(
                        "Restore items through Storage and history before deleting them again.",
                    ));
                }
                let id = zeus_types::IdGenerator::new().next_ulid().to_string();
                let relative = Path::new(".zeus").join("trash").join(&id);
                let directory = roots
                    .directory(&scope)?
                    .create_dir_all(&relative)
                    .map_err(cap_failure)?;
                // Retain the destination chain until the journal completes publication.
                let target = scope.join(relative).join(&name);
                if parent.entry_metadata(&name).map_err(cap_failure)?.file_id != meta.file_id {
                    return Err(Failure::new(
                        "app.path_changed",
                        "The item changed before trashing.",
                    ));
                }
                let result = journal.relocate_pinned(&label, &shown(&target), roots, facts);
                drop(directory);
                result.map(|_| ())
            })()
        } else {
            Err(Failure::new(
                "app.trash_unavailable",
                "A durable operation journal is required for reversible trash.",
            ))
        };
        match result {
            Ok(()) if permanent => deleted.push(label),
            Ok(()) => trashed.push(label),
            Err(e) => failed.push(json!({"path":label,"code":e.code,"message":e.message})),
        }
    }
    Ok(json!({"ok":failed.is_empty(),"trashed":trashed,"deleted":deleted,"failed":failed}))
}

// Full bounded preflight precedes deletion. Every child retains its parent until its own removal;
// deepest-first removal drops descendant handles before attempting the now-empty directory.
fn remove_tree(
    parent: &zeus_fs::DirectoryCap,
    name: &std::ffi::OsStr,
    expected: &zeus_fs::FileMeta,
) -> Result<()> {
    let expected_id = expected
        .file_id
        .as_ref()
        .ok_or_else(|| Failure::invalid("The item has no stable identity."))?;
    let mut entries = Vec::new();
    let started = std::time::Instant::now();
    let bound = || {
        if started.elapsed() > std::time::Duration::from_secs(120) {
            Err(Failure::new(
                "app.io",
                "Deletion exceeded its time bound; review the remaining items.",
            ))
        } else {
            Ok(())
        }
    };
    if expected.is_dir {
        let root = parent.open_dir(Path::new(name)).map_err(cap_failure)?;
        if root.file_id().map_err(cap_failure)? != *expected_id {
            return Err(Failure::invalid("The folder identity changed."));
        }
        let mut pending = vec![(root, 0usize)];
        while let Some((dir, depth)) = pending.pop() {
            bound()?;
            if depth > 128 {
                return Err(Failure::invalid(
                    "The folder exceeds the deletion depth bound.",
                ));
            }
            for name in dir
                .entry_names(20_000usize.saturating_sub(entries.len()))
                .map_err(cap_failure)?
            {
                let meta = dir.entry_metadata(&name).map_err(cap_failure)?;
                let id = meta
                    .file_id
                    .ok_or_else(|| Failure::invalid("A child has no stable identity."))?;
                if meta.is_dir {
                    pending.push((
                        dir.open_dir(Path::new(&name)).map_err(cap_failure)?,
                        depth + 1,
                    ));
                }
                entries.push((depth + 1, dir.clone(), name, id));
            }
        }
    }
    entries.sort_by_key(|(depth, _, _, _)| *depth);
    while let Some((_, dir, name, id)) = entries.pop() {
        bound()?;
        dir.prepare_rename(&name)
            .map_err(cap_failure)?
            .remove(&id)
            .map_err(cap_failure)?;
    }
    parent
        .prepare_rename(name)
        .map_err(cap_failure)?
        .remove(expected_id)
        .map_err(cap_failure)?;
    parent.sync().map_err(cap_failure)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, String, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("dl");
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::write(root.join("a/one.txt"), "1").unwrap();
        std::fs::write(root.join("a/two.txt"), "2").unwrap();
        let r = root.display().to_string();
        (dir, r.clone(), vec![r])
    }

    #[test]
    fn rename_never_replaces_and_checks_the_name() {
        let (_d, root, roots) = setup();
        let none = HashMap::new();
        let one = format!("{root}/a/one.txt");
        assert!(rename(&one, "two.txt", &roots, &none).unwrap_err().code == "app.exists");
        assert!(rename(&one, "x/y.txt", &roots, &none).is_err());
        let r = rename(&one, "uno.txt", &roots, &none).unwrap();
        assert!(r["path"].as_str().unwrap().ends_with("uno.txt"));
        assert!(!Path::new(&one).exists());
        assert!(rename(&format!("{root}/../elsewhere.txt"), "z.txt", &roots, &none).is_err());
    }

    #[test]
    fn move_creates_the_folder_inside_the_roots_and_refuses_outside_and_into_itself() {
        let (d, root, roots) = setup();
        let none = HashMap::new();
        let two = format!("{root}/a/two.txt");
        let r = move_to(&two, &format!("{root}/sorted/docs"), &roots, &none).unwrap();
        assert!(Path::new(r["path"].as_str().unwrap()).exists());
        assert!(
            move_to(
                &format!("{root}/a/one.txt"),
                &d.path().display().to_string(),
                &roots,
                &none
            )
            .is_err()
        );
        assert!(
            move_to(
                &format!("{root}/a"),
                &format!("{root}/a/inner"),
                &roots,
                &none
            )
            .is_err()
        );
        std::fs::write(format!("{root}/sorted/docs/one.txt"), "other").unwrap();
        assert_eq!(
            move_to(
                &format!("{root}/a/one.txt"),
                &format!("{root}/sorted/docs"),
                &roots,
                &none
            )
            .unwrap_err()
            .code,
            "app.exists"
        );
    }

    #[test]
    fn quarantined_files_are_not_touched() {
        let (_d, root, roots) = setup();
        let p = std::fs::canonicalize(format!("{root}/a/one.txt")).unwrap();
        let key = crate::config::norm(&crate::scan::display(&p));
        let mut facts = HashMap::new();
        facts.insert(
            key,
            FileFacts {
                quarantined: true,
                ..FileFacts::default()
            },
        );
        let one = format!("{root}/a/one.txt");
        assert_eq!(
            rename(&one, "x.txt", &roots, &facts).unwrap_err().code,
            "policy.quarantined"
        );
        assert_eq!(
            move_to(&one, &format!("{root}/b"), &roots, &facts)
                .unwrap_err()
                .code,
            "policy.quarantined"
        );
        assert_eq!(
            delete(std::slice::from_ref(&one), true, &roots, &facts)
                .unwrap_err()
                .code,
            "policy.quarantined"
        );
        // The folder that holds it is protected too.
        assert!(delete(&[format!("{root}/a")], true, &roots, &facts).is_err());
        assert!(Path::new(&one).exists());
    }

    #[test]
    fn permanent_delete_removes_files_and_folders_but_not_a_root() {
        let (_d, root, roots) = setup();
        let none = HashMap::new();
        let r = delete(&[format!("{root}/a/one.txt")], true, &roots, &none).unwrap();
        assert_eq!(r["deleted"].as_array().unwrap().len(), 1);
        let r = delete(&[format!("{root}/a")], true, &roots, &none).unwrap();
        assert_eq!(r["ok"], true);
        assert!(!Path::new(&format!("{root}/a")).exists());
        assert!(delete(std::slice::from_ref(&root), true, &roots, &none).is_err());
        assert!(delete(&[], true, &roots, &none).is_err());
    }
}

/// Copy one regular file without replacing an existing target or following a root escape.
pub fn copy_to(
    path: &str,
    dir: &str,
    read_roots: &[String],
    write_roots: &[String],
    facts: &HashMap<String, FileFacts>,
) -> Result<Value> {
    copy_to_pinned(
        path,
        dir,
        &RootCaps::capture(read_roots)?,
        &RootCaps::capture(write_roots)?,
        facts,
    )
}

/// Copy a regular file through separately retained read and write root capabilities.
pub fn copy_to_pinned(
    path: &str,
    dir: &str,
    reads: &RootCaps,
    writes: &RootCaps,
    facts: &HashMap<String, FileFacts>,
) -> Result<Value> {
    use std::io::{Read, Write};
    let from = reads.path(path)?;
    if quarantined_under(facts, &shown(&from)) {
        return Err(quarantine_refusal("This file"));
    }
    let dest = writes.path(dir)?;
    let (source_parent, name) = reads.parent(&from)?;
    let dest_parent = writes.directory(&dest)?;
    let to = dest.join(&name);
    let mut source = source_parent.open_read(&name).map_err(cap_failure)?;
    let mut output = dest_parent.create_new(&name).map_err(cap_failure)?;
    let result = (|| -> std::io::Result<()> {
        let mut bytes = [0u8; 65536];
        loop {
            let n = source.read(&mut bytes)?;
            if n == 0 {
                break;
            }
            output.write_all(&bytes[..n])?;
        }
        output.sync_all()
    })();
    if result.is_err() {
        let cleaned = dest_parent.discard_created(&name, output).is_ok();
        return Err(Failure::new(
            "app.io",
            if cleaned {
                "The copy did not finish; its incomplete output was removed."
            } else {
                "The copy did not finish. Its incomplete destination could not be removed; inspect it before retrying."
            },
        ));
    }
    Ok(json!({"ok":true,"path":shown(&to)}))
}

/// Create one folder in an explicitly writable existing folder.
pub fn create_folder(parent: &str, name: &str, roots: &[String]) -> Result<Value> {
    create_folder_pinned(parent, name, &RootCaps::capture(roots)?)
}

/// Create one folder through the retained managed-root authority.
pub fn create_folder_pinned(parent: &str, name: &str, roots: &RootCaps) -> Result<Value> {
    check_name(name)?;
    let parent = roots.path(parent)?;
    roots
        .directory(&parent)?
        .create_dir(std::ffi::OsStr::new(name))
        .map_err(cap_failure)?;
    Ok(json!({"ok":true,"path":shown(&parent.join(name))}))
}

#[cfg(test)]
mod management_tests {
    use super::*;
    #[test]
    fn copy_never_overwrites_and_cannot_escape_a_writable_root() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.txt");
        std::fs::write(&source, b"source").unwrap();
        let dest = dir.path().join("dest");
        std::fs::create_dir(&dest).unwrap();
        let roots = vec![dir.path().to_string_lossy().into_owned()];
        copy_to(
            source.to_str().unwrap(),
            dest.to_str().unwrap(),
            &roots,
            &roots,
            &HashMap::new(),
        )
        .unwrap();
        std::fs::write(dest.join("a.txt"), b"keep").unwrap();
        assert!(
            copy_to(
                source.to_str().unwrap(),
                dest.to_str().unwrap(),
                &roots,
                &roots,
                &HashMap::new()
            )
            .is_err()
        );
        assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"keep");
        let outside = tempfile::tempdir().unwrap();
        assert!(
            copy_to(
                source.to_str().unwrap(),
                outside.path().to_str().unwrap(),
                &roots,
                &roots,
                &HashMap::new()
            )
            .is_err()
        );
        assert_eq!(std::fs::read(source).unwrap(), b"source");
    }
    #[test]
    fn new_folder_refuses_traversal_and_existing_names() {
        let dir = tempfile::tempdir().unwrap();
        let roots = vec![dir.path().to_string_lossy().into_owned()];
        create_folder(dir.path().to_str().unwrap(), "new", &roots).unwrap();
        assert!(create_folder(dir.path().to_str().unwrap(), "new", &roots).is_err());
        assert!(create_folder(dir.path().to_str().unwrap(), "../outside", &roots).is_err());
    }
}

#[cfg(test)]
mod retained_delete_tests {
    use super::*;
    #[test]
    fn trash_receipt_survives_restart_and_restore_preserves_foreign_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir_all(root.join("folder")).unwrap();
        let file = root.join("folder/report.txt");
        std::fs::write(&file, b"keep me").unwrap();
        let pins = RootCaps::capture(&[shown(&root)]).unwrap();
        let path = temp.path().join("history.json");
        let journal = crate::history::Journal::new(path.clone());
        let facts = HashMap::new();
        let result = delete_pinned(&[shown(&file)], false, &pins, &facts, Some(&journal)).unwrap();
        assert_eq!(result["ok"], true);
        assert!(!file.exists());
        let history = journal.list();
        let record = &history["records"][0];
        let id = record["id"].as_str().unwrap().to_owned();
        assert_eq!(record["trash"], true);
        assert_eq!(record["bytes"], 7);
        let trashed = PathBuf::from(record["path"].as_str().unwrap());
        assert_eq!(std::fs::read(&trashed).unwrap(), b"keep me");
        drop(journal);
        let reloaded = crate::history::Journal::new(path);
        std::fs::write(&file, b"foreign").unwrap();
        assert!(reloaded.undo_pinned(&id, &pins, &facts).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"foreign");
        std::fs::remove_file(&file).unwrap();
        reloaded.undo_pinned(&id, &pins, &facts).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"keep me");
        assert!(!trashed.exists());
    }
    #[test]
    fn trash_preserves_folders_and_refuses_replaced_payload_on_restore() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir_all(root.join("folder/deep")).unwrap();
        std::fs::write(root.join("folder/deep/a"), b"data").unwrap();
        let pins = RootCaps::capture(&[shown(&root)]).unwrap();
        let journal = crate::history::Journal::new(temp.path().join("history.json"));
        let result = delete_pinned(
            &[shown(&root.join("folder"))],
            false,
            &pins,
            &HashMap::new(),
            Some(&journal),
        )
        .unwrap();
        assert_eq!(result["ok"], true);
        let list = journal.list();
        let record = &list["records"][0];
        let payload = PathBuf::from(record["path"].as_str().unwrap());
        assert_eq!(std::fs::read(payload.join("deep/a")).unwrap(), b"data");
        let changed = payload.with_file_name("keep-original");
        std::fs::rename(&payload, &changed).unwrap();
        std::fs::create_dir(&payload).unwrap();
        assert!(
            journal
                .undo_pinned(record["id"].as_str().unwrap(), &pins, &HashMap::new())
                .is_err()
        );
        assert!(!root.join("folder").exists());
        assert!(changed.join("deep/a").exists());
    }
    #[test]
    fn trash_is_hidden_even_when_optional_exclusions_are_removed() {
        let cfg = crate::config::IndexConfig {
            exclusions: vec![],
            exclude_hidden: false,
            exclude_system: false,
            ..Default::default()
        };
        let exclusions = crate::config::Excluder::new(&cfg);
        assert!(exclusions.skips_below(Path::new("/root/.zeus/trash/item"), Path::new("/root")));
    }
    #[cfg(windows)]
    #[test]
    fn permanent_tree_preflight_refuses_a_junction_without_removing_siblings() {
        use std::os::windows::process::CommandExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(root.join("folder")).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(root.join("folder/keep"), b"keep").unwrap();
        std::fs::write(outside.join("secret"), b"outside").unwrap();
        let junction = root.join("folder").join("link");
        let created = std::process::Command::new("cmd.exe")
            .current_dir(temp.path())
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(
            created.status.success(),
            "junction fixture failed: {} {}",
            String::from_utf8_lossy(&created.stdout),
            String::from_utf8_lossy(&created.stderr)
        );
        let roots = RootCaps::capture(&[shown(&root)]).unwrap();
        let result = delete_pinned(
            &[shown(&root.join("folder"))],
            true,
            &roots,
            &HashMap::new(),
            None,
        )
        .unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(std::fs::read(root.join("folder/keep")).unwrap(), b"keep");
        assert_eq!(std::fs::read(outside.join("secret")).unwrap(), b"outside");
        std::fs::remove_dir(junction).unwrap();
    }
}
