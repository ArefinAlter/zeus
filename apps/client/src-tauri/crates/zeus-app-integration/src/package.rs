//! The browser extension the installer carries, and unpacking it into the app's own folder.
//!
//! `apps/client/scripts/package-extension.mjs` builds the extension and its zips; `build.rs` embeds them. Unpacking is
//! idempotent (a marker holds the SHA-256 of the package that was unpacked), refuses entries that would leave the
//! target folder and caps the number and size of entries.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zeus_app::failure::{Failure, Result};
use zeus_app::home::AppHome;

const CHROMIUM_ZIP: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/zeus-extension-chromium.zip"));
const FIREFOX_ZIP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/zeus-extension-firefox.zip"));
const META: &str = include_str!(concat!(env!("OUT_DIR"), "/extension.json"));

const MAX_ENTRIES: usize = 500;
const MAX_TOTAL: u64 = 24 * 1024 * 1024;

/// Which build of the extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Chrome, Edge, Brave, Chromium.
    Chromium,
    /// Firefox.
    Firefox,
}

impl Family {
    fn folder(self) -> &'static str {
        match self {
            Self::Chromium => "chromium",
            Self::Firefox => "firefox",
        }
    }

    fn zip(self) -> &'static [u8] {
        match self {
            Self::Chromium => CHROMIUM_ZIP,
            Self::Firefox => FIREFOX_ZIP,
        }
    }

    fn zip_name(self) -> &'static str {
        match self {
            Self::Chromium => "zeus-extension-chromium.zip",
            Self::Firefox => "zeus-extension-firefox.zip",
        }
    }
}

/// What the package says about itself (`extension.json`).
pub fn meta() -> Value {
    serde_json::from_str(META).unwrap_or_else(|_| json!({}))
}

/// True when this build carries the extension.
pub fn packaged() -> bool {
    !CHROMIUM_ZIP.is_empty() && !FIREFOX_ZIP.is_empty()
}

/// The Chromium development id (from the manifest's fixed public key), if the package is present.
pub fn chromium_dev_id() -> Option<String> {
    meta()
        .get("chromium_dev_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// The Firefox add-on id, if the package is present.
pub fn firefox_id() -> Option<String> {
    meta()
        .get("firefox_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Where the extension is unpacked.
pub fn folder(home: &AppHome, family: Family) -> PathBuf {
    home.app_dir()
        .join("integration")
        .join("extension")
        .join(family.folder())
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn extract(zip: &[u8], target: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip))
        .map_err(|_| Failure::new("integration.package", "The packaged extension is damaged."))?;
    if archive.len() > MAX_ENTRIES {
        return Err(Failure::new(
            "integration.package",
            "The packaged extension has too many files.",
        ));
    }
    let mut total = 0u64;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|_| {
            Failure::new("integration.package", "The packaged extension is damaged.")
        })?;
        let Some(relative) = file.enclosed_name() else {
            return Err(Failure::new(
                "integration.package",
                "The packaged extension holds a path outside its folder.",
            ));
        };
        let dest = target.join(relative);
        if file.is_dir() {
            std::fs::create_dir_all(&dest)?;
            continue;
        }
        total += file.size();
        if total > MAX_TOTAL {
            return Err(Failure::new(
                "integration.package",
                "The packaged extension is larger than expected.",
            ));
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut data = Vec::with_capacity(usize::try_from(file.size()).unwrap_or(0));
        file.by_ref().take(MAX_TOTAL).read_to_end(&mut data)?;
        std::fs::write(&dest, data)?;
    }
    Ok(())
}

/// Unpack both builds into the app's folder (skipped when already current). Returns the two folders.
pub fn unpack(home: &AppHome) -> Result<Value> {
    if !packaged() {
        return Err(Failure::new(
            "integration.not_packaged",
            "This build of Zeus does not carry the browser extension. Build it with apps/client/scripts/package-extension.mjs.",
        ));
    }
    let mut out = serde_json::Map::new();
    for family in [Family::Chromium, Family::Firefox] {
        let dir = folder(home, family);
        let marker = dir.join(".zeus-package.sha256");
        let want = digest(family.zip());
        if std::fs::read_to_string(&marker).is_ok_and(|m| m.trim() == want) {
            out.insert(family.folder().into(), json!(dir.display().to_string()));
            continue;
        }
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::create_dir_all(&dir)?;
        extract(family.zip(), &dir)?;
        std::fs::write(&marker, &want)?;
        out.insert(family.folder().into(), json!(dir.display().to_string()));
    }
    Ok(Value::Object(out))
}

/// True when the folder for `family` has been unpacked from this package.
pub fn is_unpacked(home: &AppHome, family: Family) -> bool {
    std::fs::read_to_string(folder(home, family).join(".zeus-package.sha256"))
        .is_ok_and(|m| m.trim() == digest(family.zip()))
}

/// Copy the zips and the unpacked folders into `dir` (a folder the person picked), for loading by hand.
pub fn export(home: &AppHome, dir: &Path) -> Result<Value> {
    if !dir.is_absolute() || !dir.is_dir() {
        return Err(Failure::invalid(
            "Choose an existing folder (an absolute path).",
        ));
    }
    unpack(home)?;
    let root = dir.join("zeus-browser-extension");
    std::fs::create_dir_all(&root)?;
    let mut written = Vec::new();
    for family in [Family::Chromium, Family::Firefox] {
        let zip = root.join(family.zip_name());
        std::fs::write(&zip, family.zip())?;
        written.push(zip.display().to_string());
        let unpacked = root.join(family.folder());
        if unpacked.exists() {
            std::fs::remove_dir_all(&unpacked)?;
        }
        copy_dir(&folder(home, family), &unpacked)?;
        written.push(unpacked.display().to_string());
    }
    Ok(json!({"folder": root.display().to_string(), "written": written}))
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if name == ".zeus-package.sha256" {
            continue;
        }
        if path.is_dir() {
            copy_dir(&path, &to.join(&name))?;
        } else {
            std::fs::copy(&path, to.join(&name))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_without_the_package_says_so_and_a_build_with_it_unpacks_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let home = AppHome::at(dir.path().to_path_buf());
        if !packaged() {
            assert_eq!(unpack(&home).unwrap_err().code, "integration.not_packaged");
            return;
        }
        let first = unpack(&home).unwrap();
        let manifest = folder(&home, Family::Chromium).join("manifest.json");
        let text = std::fs::read_to_string(&manifest).unwrap();
        assert!(text.contains("\"manifest_version\": 3"));
        assert!(is_unpacked(&home, Family::Chromium));
        // A second call changes nothing.
        let stamp = std::fs::metadata(&manifest).unwrap().modified().unwrap();
        assert_eq!(unpack(&home).unwrap(), first);
        assert_eq!(
            std::fs::metadata(&manifest).unwrap().modified().unwrap(),
            stamp
        );
        assert!(chromium_dev_id().is_some_and(|id| id.len() == 32));
    }

    #[test]
    fn export_copies_zips_and_folders_to_the_chosen_folder() {
        if !packaged() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let home = AppHome::at(dir.path().join("home"));
        let target = tempfile::tempdir().unwrap();
        let out = export(&home, target.path()).unwrap();
        let root = PathBuf::from(out["folder"].as_str().unwrap());
        assert!(root.join("zeus-extension-chromium.zip").is_file());
        assert!(root.join("firefox").join("manifest.json").is_file());
        assert!(!root.join("chromium").join(".zeus-package.sha256").exists());
        assert!(export(&home, Path::new("relative")).is_err());
    }

    #[test]
    fn entries_that_leave_the_folder_are_refused() {
        // A zip with `../evil.txt` must never be written outside the target.
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(Cursor::new(&mut buf));
            w.start_file("../evil.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut w, b"x").unwrap();
            w.finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("t");
        std::fs::create_dir_all(&target).unwrap();
        assert!(extract(&buf, &target).is_err());
        assert!(!dir.path().join("evil.txt").exists());
    }
}
