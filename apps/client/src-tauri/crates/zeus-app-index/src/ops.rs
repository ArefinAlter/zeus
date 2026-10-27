//! File operations on the person's click: rename, move and delete inside the download folders.
//!
//! These are the app acting for the person at the window. The daemon's credential for this app has no file write
//! permission, and an agent has no way to reach these routes. Rules: only inside the download folders (index-only
//! folders are read-only), never replace an existing file, never touch a file the daemon quarantined (that needs
//! `zeus files organize` and an approval at a terminal), deletes go to the recycle bin or trash.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use zeus_app::failure::{Failure, Result};

use crate::facts::{FileFacts, quarantined_under};
use crate::paths::{check_name, resolve_inside, resolve_new_inside, shown};

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

fn exists_nofollow(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

/// Move `from` to `to` without ever replacing something at `to`.
pub fn move_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    if exists_nofollow(to) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "the target exists",
        ));
    }
    let meta = std::fs::symlink_metadata(from)?;
    if meta.is_file() {
        // A hard link fails when the target exists, so this cannot replace a file that appears at the last moment.
        match std::fs::hard_link(from, to) {
            Ok(()) => return std::fs::remove_file(from),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(e),
            Err(_) => {}
        }
    }
    if exists_nofollow(to) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "the target exists",
        ));
    }
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        // Across volumes: copy, then remove, for files.
        Err(_) if meta.is_file() => {
            std::fs::copy(from, to)?;
            std::fs::remove_file(from)
        }
        Err(e) => Err(e),
    }
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

/// Delete the given paths: to the recycle bin or trash, or for good when `permanent`.
pub fn delete(
    paths: &[String],
    permanent: bool,
    write_roots: &[String],
    facts: &HashMap<String, FileFacts>,
) -> Result<Value> {
    if paths.is_empty() || paths.len() > 500 {
        return Err(Failure::invalid("Delete 1 to 500 items at a time."));
    }
    let mut targets: Vec<PathBuf> = Vec::new();
    for p in paths {
        let resolved = resolve_inside(p, write_roots)?;
        if write_roots
            .iter()
            .any(|r| crate::config::norm(r) == crate::config::norm(&shown(&resolved)))
        {
            return Err(Failure::invalid(
                "A download folder itself is not deleted from here.",
            ));
        }
        if quarantined_under(facts, &shown(&resolved)) {
            return Err(quarantine_refusal("This item holds a quarantined file and"));
        }
        targets.push(resolved);
    }
    let mut trashed = Vec::new();
    let mut deleted = Vec::new();
    let mut failed = Vec::new();
    for t in targets {
        let label = shown(&t);
        if permanent {
            let r = if t.is_dir() {
                std::fs::remove_dir_all(&t)
            } else {
                std::fs::remove_file(&t)
            };
            match r {
                Ok(()) => deleted.push(label),
                Err(e) => failed.push(json!({"path": label, "code": "app.io", "message": format!("Could not delete it ({:?}).", e.kind())})),
            }
        } else {
            match trash::delete(&t) {
                Ok(()) => trashed.push(label),
                Err(_) => failed.push(json!({"path": label, "code": "app.trash_unavailable", "message": "The recycle bin is not available here. You can delete it permanently instead."})),
            }
        }
    }
    Ok(json!({"ok": failed.is_empty(), "trashed": trashed, "deleted": deleted, "failed": failed}))
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
            delete(&[one.clone()], true, &roots, &facts)
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
        assert!(delete(&[root.clone()], true, &roots, &none).is_err());
        assert!(delete(&[], true, &roots, &none).is_err());
    }
}
