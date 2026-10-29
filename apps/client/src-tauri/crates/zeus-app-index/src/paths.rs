//! Path safety: every path the page names is resolved and checked against the folders it may touch.
//!
//! The page can only name a path; this module decides whether it is inside an allowed root after resolving links, so a
//! symlink or a `..` cannot lead out. Names for rename and move are checked as plain single names.

use std::path::{Component, Path, PathBuf};

use zeus_app::failure::{Failure, Result};

use crate::scan::display;

fn strip(p: PathBuf) -> PathBuf {
    let s = p.display().to_string();
    match s.strip_prefix(r"\\?\") {
        Some(rest) => match rest.strip_prefix("UNC\\") {
            Some(share) => PathBuf::from(format!(r"\\{share}")),
            None => PathBuf::from(rest),
        },
        _ => p,
    }
}

fn canon(p: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(p).ok().map(strip)
}

fn inside_canon(roots: &[PathBuf], p: &Path) -> bool {
    roots.iter().any(|r| p.starts_with(r))
}

fn canon_roots(roots: &[String]) -> Vec<PathBuf> {
    roots.iter().filter_map(|r| canon(Path::new(r))).collect()
}

/// Refuse relative paths, `..`, and NUL.
fn plain(path: &str) -> Result<PathBuf> {
    let p = Path::new(path);
    if path.is_empty() || path.contains('\0') || !p.is_absolute() {
        return Err(Failure::invalid(
            "That is not a path inside the download folders.",
        ));
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(Failure::invalid("A path may not contain .."));
    }
    Ok(p.to_path_buf())
}

/// An existing path inside one of `roots`, resolved (links followed). The result is the resolved path.
pub fn resolve_inside(path: &str, roots: &[String]) -> Result<PathBuf> {
    let p = plain(path)?;
    let resolved = canon(&p)
        .ok_or_else(|| Failure::new("app.not_found", "That file or folder no longer exists."))?;
    if !inside_canon(&canon_roots(roots), &resolved) {
        return Err(Failure::new(
            "app.path_outside",
            "That path is outside the folders Zeus indexes.",
        ));
    }
    Ok(resolved)
}

/// A path that may not exist yet (a move's destination folder), checked through its deepest existing ancestor.
/// Returns the path with the resolved ancestor and the remaining plain names.
pub fn resolve_new_inside(path: &str, roots: &[String]) -> Result<PathBuf> {
    let p = plain(path)?;
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = p.clone();
    let existing = loop {
        if let Some(c) = canon(&cur) {
            break c;
        }
        let Some(name) = cur.file_name().map(std::ffi::OsString::from) else {
            return Err(Failure::invalid("That folder cannot be created."));
        };
        rest.push(name);
        if !cur.pop() {
            return Err(Failure::invalid("That folder cannot be created."));
        }
    };
    if !inside_canon(&canon_roots(roots), &existing) {
        return Err(Failure::new(
            "app.path_outside",
            "That folder is outside the download folders Zeus may write to.",
        ));
    }
    let mut out = existing;
    for name in rest.into_iter().rev() {
        check_name(&name.to_string_lossy())?;
        out.push(name);
    }
    Ok(out)
}

/// A root directory's identity captured when a person enables it, rather than on each file action.
#[derive(Debug, Clone)]
pub struct RootIdentity {
    path: PathBuf,
    canonical: PathBuf,
    identity: zeus_types::FileId,
}
impl RootIdentity {
    /// Pin an existing directory without accepting a final link or junction.
    pub fn capture(path: &Path) -> Option<Self> {
        use zeus_fs::{Filesystem, RealFs};
        let meta = RealFs.metadata(path).ok()??;
        if !meta.is_dir || meta.is_symlink {
            return None;
        }
        Some(Self {
            path: path.to_path_buf(),
            canonical: canon(path)?,
            identity: meta.file_id?,
        })
    }
    /// Refuse a removed, replaced or redirected root.
    pub fn current(&self) -> bool {
        use zeus_fs::{Filesystem, RealFs};
        canon(&self.path).as_ref() == Some(&self.canonical)
            && RealFs.metadata(&self.path).ok().flatten().is_some_and(|m| {
                m.is_dir && !m.is_symlink && m.file_id.as_ref() == Some(&self.identity)
            })
    }

    fn open_capability(&self) -> Result<zeus_fs::DirectoryCap> {
        let cap = zeus_fs::DirectoryCap::open_root(&self.path).map_err(cap_failure)?;
        if cap.file_id().map_err(cap_failure)? != self.identity
            || canon(&self.path).as_ref() != Some(&self.canonical)
        {
            return Err(Failure::new(
                "app.path_changed",
                "An enabled folder changed. Choose it again in Settings.",
            ));
        }
        Ok(cap)
    }
}

/// Open directory authority acquired against the identities captured when roots were enabled.
/// Read and write sets are acquired separately and kept alive for the complete local operation.
#[derive(Debug, Clone)]
pub struct RootCaps {
    roots: Vec<(PathBuf, zeus_fs::DirectoryCap)>,
}

/// A path-free error suitable for the desktop response.
pub fn cap_failure(error: zeus_fs::FsError) -> Failure {
    match error {
        zeus_fs::FsError::AlreadyExists => Failure::new(
            "app.exists",
            "That destination already exists; nothing was replaced.",
        ),
        zeus_fs::FsError::OutsideRoots
        | zeus_fs::FsError::SymlinkRefused
        | zeus_fs::FsError::InvalidName(_) => Failure::new(
            "app.path_changed",
            "Choose a plain file or folder inside an enabled root, without links or junctions.",
        ),
        _ => Failure::new(
            "app.io",
            "The file operation could not use the selected folder.",
        ),
    }
}

impl RootCaps {
    /// Acquire already recorded root identities. No fallback to a newly substituted directory.
    pub fn from_pins(pins: &[RootIdentity]) -> Result<Self> {
        pins.iter()
            .map(|pin| Ok((pin.canonical.clone(), pin.open_capability()?)))
            .collect::<Result<Vec<_>>>()
            .map(|roots| Self { roots })
    }

    /// Capture explicit roots for direct local library callers. Desktop routes use the service's
    /// previously recorded identities through `from_pins`, never this fresh authority boundary.
    pub fn capture(roots: &[String]) -> Result<Self> {
        let pins = roots
            .iter()
            .map(|p| {
                RootIdentity::capture(Path::new(p)).ok_or_else(|| {
                    Failure::new("app.path_changed", "An enabled folder is unavailable.")
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Self::from_pins(&pins)
    }

    /// The retained roots' display paths, for existing quarantine and receipt policy checks.
    pub fn paths(&self) -> Vec<String> {
        self.roots.iter().map(|(p, _)| shown(p)).collect()
    }

    /// Check lexical containment without resolving links into a different permitted path.
    pub fn path(&self, path: &str) -> Result<PathBuf> {
        let path = strip(plain(path)?);
        if self.roots.iter().any(|(root, _)| path.starts_with(root)) {
            return Ok(path);
        }
        // Address-bar ASCII case variations on Windows must still resolve through the retained
        // root. Exact matches win, including on case-sensitive Windows directories. Never reopen
        // the differently spelled ambient root, or case-fold a descendant outside its capability.
        #[cfg(windows)]
        for (root, _) in &self.roots {
            let mut input = path.components();
            if root.components().all(|part| {
                input.next().is_some_and(|p| {
                    part.as_os_str()
                        .as_encoded_bytes()
                        .eq_ignore_ascii_case(p.as_os_str().as_encoded_bytes())
                })
            }) {
                return Ok(root.join(input.as_path()));
            }
        }
        Err(Failure::new(
            "app.path_outside",
            "That path is outside the enabled folders for this operation.",
        ))
    }

    /// Display paths of the authority already retained by this set, without acquiring new roots.
    pub fn scopes(&self) -> Vec<String> {
        self.roots.iter().map(|(path, _)| shown(path)).collect()
    }

    /// Stable root identities for a durable operation. Paths are authority locators, not grants.
    pub fn bindings(&self) -> Result<Vec<(String, zeus_types::FileId)>> {
        self.roots
            .iter()
            .map(|(path, cap)| Ok((shown(path), cap.file_id().map_err(cap_failure)?)))
            .collect()
    }

    /// Bind only the longest enabled root that grants this path, excluding unrelated scopes.
    pub fn binding_for(&self, path: &Path) -> Result<(String, zeus_types::FileId)> {
        let path = self.path(&shown(path))?;
        let (root, cap) = self
            .roots
            .iter()
            .filter(|(root, _)| path.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
            .ok_or_else(|| {
                Failure::new("app.path_outside", "The file is outside enabled roots.")
            })?;
        Ok((shown(root), cap.file_id().map_err(cap_failure)?))
    }

    /// Open a descendant directory through a retained root; every intermediate link is refused.
    pub fn directory(&self, path: &Path) -> Result<zeus_fs::DirectoryCap> {
        let path = self.path(&shown(path))?;
        let (root, cap) = self
            .roots
            .iter()
            .filter(|(r, _)| path.starts_with(r))
            .max_by_key(|(r, _)| r.components().count())
            .ok_or_else(|| {
                Failure::new(
                    "app.path_outside",
                    "The folder is outside the enabled roots.",
                )
            })?;
        cap.open_dir(path.strip_prefix(root).expect("matching root"))
            .map_err(cap_failure)
    }

    /// Native metadata through held roots. Only a confirmed absent leaf is None; inaccessible
    /// parents and rejected links remain errors, so refresh cannot prune rows on an uncertain read.
    pub fn os_metadata(&self, path: &Path) -> Result<Option<std::fs::Metadata>> {
        let path = self.path(&shown(path))?;
        if self.roots.iter().any(|(root, _)| root == &path) {
            let dir = self.directory(&path)?;
            return dir.os_metadata().map(Some).map_err(cap_failure);
        }
        let (parent, name) = self.parent(&path)?;
        if !parent.entry_exists(&name).map_err(cap_failure)? {
            return Ok(None);
        }
        parent
            .entry_os_metadata(&name)
            .map(Some)
            .map_err(cap_failure)
    }

    /// Retain an item's parent; the leaf is never resolved through a symlink.
    pub fn parent(&self, path: &Path) -> Result<(zeus_fs::DirectoryCap, std::ffi::OsString)> {
        let path = self.path(&shown(path))?;
        let parent = path
            .parent()
            .ok_or_else(|| Failure::invalid("That item has no parent."))?;
        let name = path
            .file_name()
            .ok_or_else(|| Failure::invalid("Choose an item inside a root."))?;
        Ok((self.directory(parent)?, name.to_owned()))
    }
}

const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// A single file or folder name: no separators, no reserved characters, nothing Windows would reinterpret.
pub fn check_name(name: &str) -> Result<()> {
    let bad = |m: &str| Err(Failure::invalid(m.to_owned()));
    if name.is_empty() || name.len() > 255 {
        return bad("A name has 1 to 255 characters.");
    }
    if name == "." || name == ".." {
        return bad("That is not a file name.");
    }
    if name.chars().any(|c| {
        c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
    }) {
        return bad("A name cannot contain / \\ : * ? \" < > | or control characters.");
    }
    if name.ends_with(['.', ' ']) {
        return bad("A name cannot end with a dot or a space.");
    }
    let stem = name.split('.').next().unwrap_or("").to_lowercase();
    if RESERVED.contains(&stem.as_str()) {
        return bad("That name is reserved by the operating system.");
    }
    zeus_fs::directory::validate_filename(std::ffi::OsStr::new(name)).map_err(|_| {
        Failure::invalid("That name is reserved or unsupported by the file system.")
    })?;
    Ok(())
}

/// The display form of a resolved path.
pub fn shown(p: &Path) -> String {
    display(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_plain() {
        assert!(check_name("report 2026.pdf").is_ok());
        for bad in [
            "", ".", "..", "a/b", "a\\b", "a:b", "CON", "nul.txt", "x.", "x ", "q?.txt",
        ] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn paths_must_be_inside_after_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("sub/a.txt"), "a").unwrap();
        std::fs::write(outside.join("b.txt"), "b").unwrap();
        let roots = vec![root.display().to_string()];
        assert!(resolve_inside(&root.join("sub/a.txt").display().to_string(), &roots).is_ok());
        assert!(resolve_inside(&outside.join("b.txt").display().to_string(), &roots).is_err());
        assert!(
            resolve_inside(
                &format!("{}/sub/../../outside/b.txt", root.display()),
                &roots
            )
            .is_err()
        );
        assert!(resolve_inside("relative/a.txt", &roots).is_err());
        assert!(resolve_inside(&root.join("missing").display().to_string(), &roots).is_err());
        // A destination that does not exist yet is checked through its ancestor.
        let fresh = root.join("a/b/c").display().to_string();
        assert!(resolve_new_inside(&fresh, &roots).is_ok());
        assert!(resolve_new_inside(&outside.join("new").display().to_string(), &roots).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_root_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("b.txt"), "b").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let roots = vec![root.display().to_string()];
        assert!(resolve_inside(&root.join("link/b.txt").display().to_string(), &roots).is_err());
    }
    #[test]
    fn enabled_root_identity_does_not_follow_a_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let pin = RootIdentity::capture(&root).unwrap();
        assert!(pin.current());
        std::fs::rename(&root, dir.path().join("old-root")).unwrap();
        std::fs::create_dir(&root).unwrap();
        assert!(!pin.current());
        assert!(RootIdentity::capture(&root).unwrap().current());
    }
}

#[cfg(test)]
mod capability_tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    fn canonical_network_share_prefix_remains_an_absolute_unc_path() {
        assert_eq!(
            strip(PathBuf::from(r"\\?\UNC\host\share\folder")),
            PathBuf::from(r"\\host\share\folder")
        );
        assert!(strip(PathBuf::from(r"\\?\UNC\host\share\folder")).is_absolute());
    }
    #[test]
    fn opening_against_enablement_identity_refuses_a_replacement_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let pin = RootIdentity::capture(&root).unwrap();
        std::fs::rename(&root, temp.path().join("old")).unwrap();
        std::fs::create_dir(&root).unwrap();
        assert!(RootCaps::from_pins(&[pin]).is_err());
    }
    #[test]
    fn read_and_write_caps_do_not_expand_each_other_and_root_is_not_an_entry() {
        let temp = tempfile::tempdir().unwrap();
        let read = temp.path().join("read");
        let write = temp.path().join("write");
        std::fs::create_dir(&read).unwrap();
        std::fs::create_dir(&write).unwrap();
        let reads = RootCaps::capture(&[shown(&read)]).unwrap();
        let writes = RootCaps::capture(&[shown(&write)]).unwrap();
        assert!(reads.directory(&read).is_ok());
        assert!(writes.directory(&read).is_err());
        assert!(reads.directory(&write).is_err());
        assert!(writes.parent(&write).is_err());
        assert!(writes.path(&shown(&write.join("../read"))).is_err());
    }
    #[cfg(windows)]
    #[test]
    fn live_native_operations_refuse_junctions_and_do_not_touch_outside_contents() {
        use crate::{history::Journal, ops};
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("source"), b"source").unwrap();
        std::fs::write(outside.path().join("victim"), b"keep").unwrap();
        let cap = RootCaps::capture(&[shown(&root)]).unwrap();
        assert!(
            cap.directory(Path::new(&shown(&root).to_ascii_uppercase()))
                .is_ok()
        );
        let link = root.join("junction");
        assert!(
            std::process::Command::new("cmd.exe")
                .current_dir(temp.path())
                .args(["/d", "/c", "mklink", "/j"])
                .arg(&link)
                .arg(outside.path())
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        );
        let facts = std::collections::HashMap::new();
        assert!(
            ops::copy_to_pinned(
                &shown(&root.join("source")),
                &shown(&link),
                &cap,
                &cap,
                &facts
            )
            .is_err()
        );
        assert!(
            ops::copy_to_pinned(
                &shown(&link.join("victim")),
                &shown(&root),
                &cap,
                &cap,
                &facts
            )
            .is_err()
        );
        assert!(ops::create_folder_pinned(&shown(&link), "new", &cap).is_err());
        let journal = Journal::new(temp.path().join("history.json"));
        assert!(
            journal
                .relocate_pinned(
                    &shown(&root.join("source")),
                    &shown(&link.join("moved")),
                    &cap,
                    &facts
                )
                .is_err()
        );
        assert!(
            journal
                .relocate_pinned(
                    &shown(&link),
                    &shown(&root.join("moved-link")),
                    &cap,
                    &facts
                )
                .is_err()
        );
        assert_eq!(
            std::fs::read(outside.path().join("victim")).unwrap(),
            b"keep"
        );
        assert!(!outside.path().join("source").exists());
        assert!(!outside.path().join("new").exists());
        std::fs::remove_dir(&link).unwrap();
    }
}
