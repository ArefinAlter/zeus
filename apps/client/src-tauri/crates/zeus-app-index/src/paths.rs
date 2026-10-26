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
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
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
}
