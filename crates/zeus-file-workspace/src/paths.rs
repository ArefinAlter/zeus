//! Paths resolved through retained roots supplied by the caller's existing grant boundary.
use crate::failure::{Failure, Result};
use std::path::{Component, Path, PathBuf};
use zeus_fs::DirectoryCap;
/// Retained read or write authority. A caller must acquire each root under its own current policy.
#[derive(Debug, Clone)]
pub struct RootCaps {
    roots: Vec<(PathBuf, DirectoryCap)>,
}
/// Display a local path without changing its underlying authority.
pub fn shown(path: &Path) -> String {
    let value = path.to_string_lossy();
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{}", rest)
    } else if let Some(rest) = value.strip_prefix(r"\\?\") {
        rest.into()
    } else {
        value.into_owned()
    }
}
/// Map filesystem failures without exposing arbitrary filesystem paths.
pub fn cap_failure(error: zeus_fs::FsError) -> Failure {
    match error {
        zeus_fs::FsError::AlreadyExists => Failure::new(
            "app.exists",
            "The destination exists. Nothing was replaced.",
        ),
        zeus_fs::FsError::OutsideRoots
        | zeus_fs::FsError::SymlinkRefused
        | zeus_fs::FsError::InvalidName(_) => Failure::new(
            "app.path_changed",
            "The selected path is not a plain entry inside the retained roots.",
        ),
        _ => Failure::new(
            "app.io",
            "The selected filesystem operation could not finish.",
        ),
    }
}
impl RootCaps {
    /// Accept already-retained roots. This does not open a new path or create a grant.
    pub fn from_retained(roots: Vec<(PathBuf, DirectoryCap)>) -> Result<Self> {
        if roots.iter().any(|(p, _)| {
            !p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir))
        }) {
            return Err(Failure::invalid(
                "Retained root paths must be absolute and normal.",
            ));
        }
        Ok(Self { roots })
    }
    /// Acquire explicit roots for local tests/tools. Applications should use `from_retained`.
    pub fn capture(paths: &[String]) -> Result<Self> {
        let roots = paths
            .iter()
            .map(|p| {
                let cap = DirectoryCap::open_root(Path::new(p)).map_err(cap_failure)?;
                Ok((PathBuf::from(shown(Path::new(p))), cap))
            })
            .collect::<Result<Vec<_>>>()?;
        Self::from_retained(roots)
    }
    /// Check lexical containment; descendants are opened through handles and never resolved via links.
    pub fn path(&self, text: &str) -> Result<PathBuf> {
        let path = PathBuf::from(shown(Path::new(text)));
        if text.is_empty()
            || text.contains('\0')
            || !path.is_absolute()
            || path.components().any(|c| matches!(c, Component::ParentDir))
        {
            return Err(Failure::invalid(
                "The path must be absolute and contain no parent traversal.",
            ));
        }
        if self.roots.iter().any(|(r, _)| path.starts_with(r)) {
            return Ok(path);
        }
        #[cfg(windows)]
        for (root, _) in &self.roots {
            let mut input = path.components();
            if root.components().all(|part| {
                input.next().is_some_and(|p| {
                    p.as_os_str()
                        .as_encoded_bytes()
                        .eq_ignore_ascii_case(part.as_os_str().as_encoded_bytes())
                })
            }) {
                return Ok(root.join(input.as_path()));
            }
        }
        Err(Failure::new(
            "app.path_outside",
            "The path is outside the enabled roots for this operation.",
        ))
    }
    /// Stable root identities, used only to compare grants acquired again during an explicit resume.
    pub fn bindings(&self) -> Result<Vec<(String, zeus_types::FileId)>> {
        self.roots
            .iter()
            .map(|(p, c)| Ok((shown(p), c.file_id().map_err(cap_failure)?)))
            .collect()
    }
    /// The longest already-retained root granting this path.
    pub fn binding_for(&self, path: &Path) -> Result<(String, zeus_types::FileId)> {
        let path = self.path(&shown(path))?;
        let (root, cap) = self
            .roots
            .iter()
            .filter(|(root, _)| path.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
            .ok_or_else(|| Failure::new("app.path_outside", "The path has no retained root."))?;
        Ok((shown(root), cap.file_id().map_err(cap_failure)?))
    }
    /// Retain a descendant directory through the longest permitted root, refusing links at every step.
    pub fn directory(&self, path: &Path) -> Result<DirectoryCap> {
        let path = self.path(&shown(path))?;
        let (root, cap) = self
            .roots
            .iter()
            .filter(|(r, _)| path.starts_with(r))
            .max_by_key(|(r, _)| r.components().count())
            .ok_or_else(|| {
                Failure::new("app.path_outside", "The directory has no retained root.")
            })?;
        cap.open_dir(path.strip_prefix(root).expect("matching root"))
            .map_err(cap_failure)
    }
    /// Retain a parent directory and a plain leaf, without following the leaf.
    pub fn parent(&self, path: &Path) -> Result<(DirectoryCap, std::ffi::OsString)> {
        let path = self.path(&shown(path))?;
        let parent = path
            .parent()
            .ok_or_else(|| Failure::invalid("Choose an entry inside a root."))?;
        let name = path
            .file_name()
            .ok_or_else(|| Failure::invalid("Choose an entry inside a root."))?;
        Ok((self.directory(parent)?, name.to_owned()))
    }
}
