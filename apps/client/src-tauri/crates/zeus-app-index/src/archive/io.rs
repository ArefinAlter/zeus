//! Archive data access through retained roots; never reopen checked ambient data paths.
use super::{Entry, MAX_BYTES, MAX_ENTRIES, Plan, Stamp, failed, relative_name, stamp_meta};
use crate::{
    facts::{FileFacts, quarantined_under},
    paths::{RootCaps, cap_failure, shown},
};
use std::{
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use zeus_app::failure::Result;
use zeus_fs::{DirectoryCap, opened_file_metadata};

pub(super) fn file_stamp(file: &std::fs::File) -> Result<Stamp> {
    stamp_meta(opened_file_metadata(file).map_err(cap_failure)?)
}
pub(super) fn entry_stamp(parent: &DirectoryCap, name: &OsStr) -> Result<Stamp> {
    stamp_meta(parent.entry_metadata(name).map_err(cap_failure)?)
}
fn dir_stamp(dir: &DirectoryCap) -> Result<Stamp> {
    stamp_meta(dir.metadata().map_err(cap_failure)?)
}

pub(super) struct ScopedReader {
    pub file: std::fs::File,
    _parent: DirectoryCap,
}
pub(super) enum ArchiveSource {
    File(ScopedReader),
    Directory(DirectoryCap),
}
impl ArchiveSource {
    pub fn open(path: &Path, roots: &RootCaps) -> Result<Self> {
        // A whole enabled directory is an eligible source too; opening its outside parent is not.
        if let Ok(dir) = roots.directory(path) {
            return Ok(Self::Directory(dir));
        }
        let (parent, name) = roots.parent(path)?;
        let file = parent.open_read(&name).map_err(cap_failure)?;
        Ok(Self::File(ScopedReader {
            file,
            _parent: parent,
        }))
    }
    pub fn stamp(&self) -> Result<Stamp> {
        match self {
            Self::File(input) => file_stamp(&input.file),
            Self::Directory(dir) => dir_stamp(dir),
        }
    }
    pub fn zip_file(&mut self) -> Result<&mut std::fs::File> {
        match self {
            Self::File(input) => Ok(&mut input.file),
            Self::Directory(_) => Err(failed("Choose a regular ZIP file.")),
        }
    }
    pub fn member_stamp(&self, name: &str, directory: bool, source: &Path) -> Result<Stamp> {
        let relative = member(name, directory, source)?;
        match self {
            Self::File(input) if relative.as_os_str().is_empty() && !directory => {
                file_stamp(&input.file)
            }
            Self::Directory(root) if relative.as_os_str().is_empty() => dir_stamp(root),
            Self::Directory(root) => {
                let parent = root
                    .open_dir(
                        relative
                            .parent()
                            .ok_or_else(|| failed("The member has no parent."))?,
                    )
                    .map_err(cap_failure)?;
                entry_stamp(
                    &parent,
                    relative
                        .file_name()
                        .ok_or_else(|| failed("The member has no name."))?,
                )
            }
            _ => Err(failed("The manifest no longer matches its source.")),
        }
    }
    pub fn open_member(&self, name: &str, source: &Path) -> Result<ScopedReader> {
        let relative = member(name, false, source)?;
        match self {
            Self::File(input) if relative.as_os_str().is_empty() => Ok(ScopedReader {
                file: input.file.try_clone()?,
                _parent: input._parent.clone(),
            }),
            Self::Directory(root) => {
                let parent = root
                    .open_dir(
                        relative
                            .parent()
                            .ok_or_else(|| failed("The member has no parent."))?,
                    )
                    .map_err(cap_failure)?;
                let file = parent
                    .open_read(
                        relative
                            .file_name()
                            .ok_or_else(|| failed("The member has no name."))?,
                    )
                    .map_err(cap_failure)?;
                Ok(ScopedReader {
                    file,
                    _parent: parent,
                })
            }
            _ => Err(failed("The manifest no longer matches its source.")),
        }
    }
    pub fn manifest(
        &self,
        source: &Path,
        facts: &HashMap<String, FileFacts>,
    ) -> Result<(Vec<Entry>, u64)> {
        let base_name = source
            .file_name()
            .and_then(|x| x.to_str())
            .ok_or_else(|| failed("Choose a named file or folder with a UTF-8 name."))?;
        let started = Instant::now();
        let mut pending = vec![PathBuf::new()];
        let mut entries = Vec::new();
        let mut bytes = 0u64;
        let mut names = HashSet::new();
        let mut name_bytes = 0usize;
        while let Some(relative) = pending.pop() {
            time_bound(started)?;
            if entries.len() + pending.len() >= MAX_ENTRIES {
                return Err(failed("Archive at most 10,000 entries."));
            }
            let path = source.join(&relative);
            if quarantined_under(facts, &shown(&path)) {
                return Err(crate::ops::quarantine_refusal("An archive source"));
            }
            let st = if relative.as_os_str().is_empty() {
                self.stamp()?
            } else {
                let Self::Directory(root) = self else {
                    return Err(failed("The source changed type."));
                };
                let parent = root
                    .open_dir(relative.parent().unwrap())
                    .map_err(cap_failure)?;
                entry_stamp(&parent, relative.file_name().unwrap())?
            };
            let relative_text = relative
                .components()
                .map(|c| {
                    c.as_os_str()
                        .to_str()
                        .ok_or_else(|| failed("Archive member names must be valid UTF-8."))
                })
                .collect::<Result<Vec<_>>>()?
                .join("/");
            let name = if relative_text.is_empty() {
                base_name.to_owned()
            } else {
                format!("{base_name}/{relative_text}")
            };
            relative_name(&name, st.directory)?;
            name_bytes = name_bytes.saturating_add(name.len());
            if name_bytes > 8 * 1024 * 1024 {
                return Err(failed("Archive member paths exceed their 8 MiB bound."));
            }
            if !names.insert(name.to_lowercase()) {
                return Err(failed(
                    "Source names collide when compared without case; choose a portable subset.",
                ));
            }
            bytes = bytes
                .checked_add(st.bytes)
                .ok_or_else(|| failed("The archive size exceeds its limit."))?;
            if bytes > MAX_BYTES {
                return Err(failed(
                    "Archive at most 4 GiB of source data per operation.",
                ));
            }
            if st.directory {
                let Self::Directory(root) = self else {
                    return Err(failed("The source changed type."));
                };
                let dir = root.open_dir(&relative).map_err(cap_failure)?;
                if dir_stamp(&dir)? != st {
                    return Err(failed("A source directory changed during preview."));
                }
                for name in dir
                    .entry_names(MAX_ENTRIES - entries.len() - pending.len())
                    .map_err(cap_failure)?
                {
                    pending.push(relative.join(name));
                    if entries.len() + pending.len() >= MAX_ENTRIES {
                        return Err(failed("Archive at most 10,000 entries."));
                    }
                }
            }
            entries.push(Entry {
                name,
                bytes: st.bytes,
                directory: st.directory,
                stamp: Some(st),
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok((entries, bytes))
    }
}
fn member(name: &str, directory: bool, source: &Path) -> Result<PathBuf> {
    let path = relative_name(name, directory)?;
    let base = source
        .file_name()
        .ok_or_else(|| failed("Choose a named source."))?;
    path.strip_prefix(Path::new(base))
        .map(Path::to_path_buf)
        .map_err(|_| failed("The member is outside its reviewed source."))
}
pub(super) fn time_bound(started: Instant) -> Result<()> {
    if started.elapsed() > Duration::from_secs(600) {
        return Err(failed("The archive operation exceeded ten minutes."));
    }
    Ok(())
}
pub(super) fn stage_name(plan: &Plan, stage: &str, roots: &RootCaps) -> Result<OsString> {
    if !super::valid_id(&plan.id) {
        return Err(failed("The saved archive identifier is invalid."));
    }
    let target = roots.path(&plan.target)?;
    let path = roots.path(stage)?;
    if target.parent() != path.parent() {
        return Err(failed("Staging is not in the recorded destination parent."));
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| failed("The staging name is invalid."))?;
    let prefix = format!(".zeus-archive-{}-", plan.id);
    if !name
        .strip_prefix(&prefix)
        .is_some_and(|suffix| suffix.len() == 32 && suffix.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(failed(
            "The private staging name does not match this receipt.",
        ));
    }
    Ok(OsString::from(name))
}

pub(super) fn cleanup_stage(
    parent: &DirectoryCap,
    name: &OsStr,
    expected: &zeus_types::FileId,
) -> Result<()> {
    let before = entry_stamp(parent, name)?;
    if &before.file_id != expected {
        return Err(failed("The staging identity changed; cleanup was refused."));
    }
    if !before.directory {
        return parent
            .prepare_rename(name)
            .map_err(cap_failure)?
            .remove(expected)
            .map_err(cap_failure);
    }
    let root = parent.open_dir(Path::new(name)).map_err(cap_failure)?;
    if root.file_id().map_err(cap_failure)? != *expected {
        return Err(failed("The staging directory changed before cleanup."));
    }
    // Preflight all children before any removal: refuse links/special files and bound every walk.
    let started = Instant::now();
    let mut pending = vec![PathBuf::new()];
    let mut entries = Vec::new();
    while let Some(relative) = pending.pop() {
        time_bound(started)?;
        let dir = root.open_dir(&relative).map_err(cap_failure)?;
        for name in dir
            .entry_names(MAX_ENTRIES - entries.len())
            .map_err(cap_failure)?
        {
            let path = relative.join(&name);
            if path.components().count() > 128 {
                return Err(failed("Staging exceeds its folder-depth bound."));
            }
            let meta = entry_stamp(&dir, &name)?;
            entries.push((path.clone(), meta.file_id));
            if entries.len() > MAX_ENTRIES {
                return Err(failed("Staging exceeds its entry bound."));
            }
            if meta.directory {
                pending.push(path);
            }
        }
    }
    // Deepest paths first; every removal checks the opened object's identity. A newly inserted
    // foreign child prevents empty-directory removal, leaving the receipt for another review.
    entries.sort_by_key(|(p, _)| std::cmp::Reverse(p.components().count()));
    for (path, expected) in entries {
        time_bound(started)?;
        let dir = root.open_dir(path.parent().unwrap()).map_err(cap_failure)?;
        dir.prepare_rename(path.file_name().unwrap())
            .map_err(cap_failure)?
            .remove(&expected)
            .map_err(cap_failure)?;
    }
    drop(root);
    parent
        .prepare_rename(name)
        .map_err(cap_failure)?
        .remove(expected)
        .map_err(cap_failure)?;
    parent.sync().map_err(cap_failure)
}
