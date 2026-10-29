//! Opened regular-file reads through held read-root authority.
use crate::paths::{RootCaps, cap_failure};
use std::{ffi::OsString, path::Path, time::SystemTime};
use zeus_app::failure::{Failure, Result};
use zeus_fs::{DirectoryCap, FileMeta, opened_file_metadata};

pub(crate) struct OpenedRead {
    pub file: std::fs::File,
    pub before: FileMeta,
    parent: DirectoryCap,
    name: OsString,
    modified: Option<SystemTime>,
}
impl OpenedRead {
    pub fn open(roots: &RootCaps, path: &Path) -> Result<Self> {
        let (parent, name) = roots.parent(path)?;
        let file = parent.open_read(&name).map_err(cap_failure)?;
        let before = opened_file_metadata(&file).map_err(cap_failure)?;
        let modified = file.metadata()?.modified().ok();
        if before.file_id.is_none() {
            return Err(changed());
        }
        let read = Self {
            file,
            before,
            parent,
            name,
            modified,
        };
        read.check_unchanged()?;
        Ok(read)
    }
    pub fn check_unchanged(&self) -> Result<()> {
        if opened_file_metadata(&self.file).map_err(cap_failure)? != self.before
            || self.file.metadata()?.modified().ok() != self.modified
            || self
                .parent
                .entry_metadata(&self.name)
                .map_err(cap_failure)?
                != self.before
        {
            return Err(changed());
        }
        Ok(())
    }
}
pub(crate) fn changed() -> Failure {
    Failure::new(
        "app.file_changed",
        "A selected file changed during the read. Select it again.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{analytics, paths::shown, preview};
    use std::collections::HashMap;

    #[test]
    fn retained_read_detects_changed_or_replaced_entries() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source");
        std::fs::write(&path, b"original").unwrap();
        let roots = RootCaps::capture(&[shown(temp.path())]).unwrap();
        let input = OpenedRead::open(&roots, &path).unwrap();
        input.check_unchanged().unwrap();
        std::fs::write(&path, b"different length").unwrap();
        assert!(input.check_unchanged().is_err());
        drop(input);
        let input = OpenedRead::open(&roots, &path).unwrap();
        match std::fs::rename(&path, temp.path().join("original")) {
            Ok(()) => {
                std::fs::write(&path, b"replacement").unwrap();
                assert!(input.check_unchanged().is_err());
            }
            Err(error) => {
                #[cfg(not(windows))]
                panic!("Unexpected replacement failure: {error}");
                #[cfg(windows)]
                {
                    assert_eq!(
                        error.raw_os_error(),
                        Some(32),
                        "only a sharing violation can prevent replacement"
                    );
                    input.check_unchanged().unwrap();
                }
            }
        }
    }

    #[test]
    fn preview_and_hashing_refuse_linked_children_and_ungranted_sources() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("read");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("canary"), b"outside unchanged").unwrap();
        std::fs::write(root.join("local"), b"outside unchanged").unwrap();
        let link = root.join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        #[cfg(windows)]
        assert!(
            std::process::Command::new("cmd.exe")
                .current_dir(&root)
                .args(["/d", "/c", "mklink", "/j"])
                .arg(&link)
                .arg(&outside)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        );
        let roots = RootCaps::capture(&[shown(&root)]).unwrap();
        for path in [&link, &link.join("canary"), &outside.join("canary")] {
            assert_eq!(
                preview::build_pinned(path, &shown(path), &roots)["kind"],
                "refused"
            );
            assert!(
                analytics::verify_duplicates_pinned(
                    &[shown(path), shown(&root.join("local"))],
                    &roots,
                    &HashMap::new()
                )
                .is_err()
            );
        }
        assert_eq!(
            std::fs::read(outside.join("canary")).unwrap(),
            b"outside unchanged"
        );
        #[cfg(windows)]
        std::fs::remove_dir(&link).unwrap();
        #[cfg(unix)]
        std::fs::remove_file(&link).unwrap();
    }
}
