//! Registration of the host with the browsers (spec 02 section 3.1).
//!
//! Per-user registration only (HKCU or the home directory); the installer never registers system-wide hosts.
//! [`plan`] is pure: it computes the manifest files and registry values to create. [`apply`] performs them through
//! a [`Registrar`], which tests replace with a recorder. The Windows registry implementation sits behind
//! `cfg(windows)` and writes only under `HKCU\Software\...\NativeMessagingHosts\dev.zeus.native_host`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::HOST_NAME;
use crate::allowlist::{self, AllowList};
use crate::error::HostError;

/// A browser the host can be registered with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Browser {
    /// Google Chrome.
    Chrome,
    /// Microsoft Edge.
    Edge,
    /// Brave.
    Brave,
    /// Chromium.
    Chromium,
    /// Mozilla Firefox.
    Firefox,
}

impl Browser {
    /// Every supported browser.
    pub const ALL: [Self; 5] = [
        Self::Chrome,
        Self::Edge,
        Self::Brave,
        Self::Chromium,
        Self::Firefox,
    ];

    /// Command-line spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chrome => "chrome",
            Self::Edge => "edge",
            Self::Brave => "brave",
            Self::Chromium => "chromium",
            Self::Firefox => "firefox",
        }
    }

    /// Parse the command-line spelling.
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.as_str() == text)
    }

    /// Whether the manifest uses `allowed_origins` (Chromium family) or `allowed_extensions` (Firefox).
    pub const fn is_chromium_family(self) -> bool {
        !matches!(self, Self::Firefox)
    }
}

/// Operating system, as an argument so planning is testable on any host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    /// Windows.
    Windows,
    /// macOS.
    MacOs,
    /// Linux and other Unix.
    Linux,
}

impl Os {
    /// The OS this binary runs on.
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Linux
        }
    }
}

/// The host manifest (JSON read by the browser).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HostManifest {
    /// `dev.zeus.native_host`.
    pub name: &'static str,
    /// Human-readable description.
    pub description: &'static str,
    /// Absolute path of the host executable.
    pub path: String,
    /// Always `stdio`.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// Chromium family: exact `chrome-extension://<id>/` origins, no wildcards.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_origins: Option<Vec<String>>,
    /// Firefox: add-on ids.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_extensions: Option<Vec<String>>,
}

impl HostManifest {
    /// Build the manifest for `browser` pinning exactly `extension_ids`.
    pub fn new(
        browser: Browser,
        host_exe: &Path,
        extension_ids: &[String],
    ) -> Result<Self, HostError> {
        if extension_ids.is_empty() {
            return Err(HostError::Install(
                "at least one extension id is required".into(),
            ));
        }
        if extension_ids
            .iter()
            .any(|id| id.contains('*') || id.is_empty())
        {
            return Err(HostError::Install(
                "extension ids must be exact (no wildcards)".into(),
            ));
        }
        let path = host_exe
            .to_str()
            .ok_or_else(|| HostError::Install("host path is not valid UTF-8".into()))?
            .to_string();
        let (origins, extensions) = if browser.is_chromium_family() {
            (
                Some(
                    extension_ids
                        .iter()
                        .map(|id| format!("chrome-extension://{id}/"))
                        .collect(),
                ),
                None,
            )
        } else {
            (None, Some(extension_ids.to_vec()))
        };
        Ok(Self {
            name: HOST_NAME,
            description: "Zeus native messaging host",
            path,
            kind: "stdio",
            allowed_origins: origins,
            allowed_extensions: extensions,
        })
    }

    /// Pretty JSON text.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default() + "\n"
    }
}

/// Where the browser looks for the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestLocation {
    /// A file at this path (macOS, Linux).
    File(PathBuf),
    /// A registry key under `HKCU` whose default value is the path of a manifest file stored by Zeus (Windows).
    Registry {
        /// Key path below HKCU.
        key: String,
        /// Where Zeus stores the manifest file the key points to.
        manifest_file: PathBuf,
    },
}

/// The manifest location of `browser` on `os` (spec 02 section 3.1). `home` is the user's home directory;
/// `state_dir` is where Windows manifest files are stored.
pub fn manifest_location(
    browser: Browser,
    os: Os,
    home: &Path,
    state_dir: &Path,
) -> ManifestLocation {
    let file_name = format!("{HOST_NAME}.json");
    match os {
        Os::Windows => {
            let vendor = match browser {
                Browser::Chrome => r"Software\Google\Chrome",
                Browser::Edge => r"Software\Microsoft\Edge",
                Browser::Brave => r"Software\BraveSoftware\Brave-Browser",
                Browser::Chromium => r"Software\Chromium",
                Browser::Firefox => r"Software\Mozilla",
            };
            ManifestLocation::Registry {
                key: format!(r"{vendor}\NativeMessagingHosts\{HOST_NAME}"),
                manifest_file: state_dir
                    .join("native-messaging")
                    .join(format!("{}.json", browser.as_str())),
            }
        }
        Os::MacOs => {
            let base = home.join("Library").join("Application Support");
            let dir = match browser {
                Browser::Chrome => base.join("Google").join("Chrome"),
                Browser::Edge => base.join("Microsoft Edge"),
                Browser::Brave => base.join("BraveSoftware").join("Brave-Browser"),
                Browser::Chromium => base.join("Chromium"),
                Browser::Firefox => base.join("Mozilla"),
            };
            ManifestLocation::File(dir.join("NativeMessagingHosts").join(file_name))
        }
        Os::Linux => {
            let dir = match browser {
                Browser::Chrome => home
                    .join(".config")
                    .join("google-chrome")
                    .join("NativeMessagingHosts"),
                Browser::Edge => home
                    .join(".config")
                    .join("microsoft-edge")
                    .join("NativeMessagingHosts"),
                Browser::Brave => home
                    .join(".config")
                    .join("BraveSoftware")
                    .join("Brave-Browser")
                    .join("NativeMessagingHosts"),
                Browser::Chromium => home
                    .join(".config")
                    .join("chromium")
                    .join("NativeMessagingHosts"),
                Browser::Firefox => home.join(".mozilla").join("native-messaging-hosts"),
            };
            ManifestLocation::File(dir.join(file_name))
        }
    }
}

/// One step of an installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Write a file (creating parent directories).
    WriteFile {
        /// Destination.
        path: PathBuf,
        /// Contents.
        contents: String,
    },
    /// Set the default value of `HKCU\<key>` to `value`.
    SetRegistryDefault {
        /// Key path below HKCU.
        key: String,
        /// The manifest file path.
        value: String,
    },
    /// Remove a file if present.
    RemoveFile {
        /// Path.
        path: PathBuf,
    },
    /// Delete `HKCU\<key>` if present.
    DeleteRegistryKey {
        /// Key path below HKCU.
        key: String,
    },
}

/// Inputs of an installation.
#[derive(Debug, Clone)]
pub struct InstallRequest {
    /// Browsers to register with.
    pub browsers: Vec<Browser>,
    /// Absolute path of `zeus-native-host`.
    pub host_exe: PathBuf,
    /// Exact extension ids to pin (Chromium ids and/or Firefox add-on ids, matched per browser family).
    pub extension_ids: Vec<String>,
    /// Development ids, accepted by the host only because they are listed under `dev_extensions`.
    pub dev_extension_ids: Vec<String>,
    /// The user's home directory.
    pub home: PathBuf,
    /// Zeus state directory.
    pub state_dir: PathBuf,
    /// The OS.
    pub os: Os,
}

fn ids_for(browser: Browser, all: &[String]) -> Vec<String> {
    all.iter()
        .filter(|id| browser.is_chromium_family() != (id.contains('@') || id.starts_with('{')))
        .cloned()
        .collect()
}

/// Compute the steps to install the host for the requested browsers, plus the host's own allow-list file.
pub fn plan(request: &InstallRequest) -> Result<Vec<Step>, HostError> {
    let mut steps = Vec::new();
    let all_ids: Vec<String> = request
        .extension_ids
        .iter()
        .chain(&request.dev_extension_ids)
        .cloned()
        .collect();
    for browser in &request.browsers {
        let ids = ids_for(*browser, &all_ids);
        let manifest = HostManifest::new(*browser, &request.host_exe, &ids)?;
        match manifest_location(*browser, request.os, &request.home, &request.state_dir) {
            ManifestLocation::File(path) => steps.push(Step::WriteFile {
                path,
                contents: manifest.to_json(),
            }),
            ManifestLocation::Registry { key, manifest_file } => {
                steps.push(Step::WriteFile {
                    path: manifest_file.clone(),
                    contents: manifest.to_json(),
                });
                steps.push(Step::SetRegistryDefault {
                    key,
                    value: manifest_file.to_string_lossy().into_owned(),
                });
            }
        }
    }
    let list = AllowList {
        extensions: request.extension_ids.clone(),
        dev_extensions: request.dev_extension_ids.clone(),
    };
    let dir = request
        .host_exe
        .parent()
        .ok_or_else(|| HostError::Install("host path has no directory".into()))?;
    steps.push(Step::WriteFile {
        path: dir.join(allowlist::FILE_NAME),
        contents: serde_json::to_string_pretty(&list).unwrap_or_default() + "\n",
    });
    Ok(steps)
}

/// Compute the steps to remove everything [`plan`] creates.
pub fn plan_uninstall(request: &InstallRequest) -> Vec<Step> {
    let mut steps = Vec::new();
    for browser in &request.browsers {
        match manifest_location(*browser, request.os, &request.home, &request.state_dir) {
            ManifestLocation::File(path) => steps.push(Step::RemoveFile { path }),
            ManifestLocation::Registry { key, manifest_file } => {
                steps.push(Step::DeleteRegistryKey { key });
                steps.push(Step::RemoveFile {
                    path: manifest_file,
                });
            }
        }
    }
    if let Some(dir) = request.host_exe.parent() {
        steps.push(Step::RemoveFile {
            path: dir.join(allowlist::FILE_NAME),
        });
    }
    steps
}

/// Performs [`Step`]s. Implemented for the real file system and registry, and by recorders in tests.
pub trait Registrar {
    /// Execute one step.
    fn execute(&mut self, step: &Step) -> Result<(), HostError>;
}

/// Execute every step in order, stopping at the first failure.
pub fn apply(steps: &[Step], registrar: &mut dyn Registrar) -> Result<(), HostError> {
    steps.iter().try_for_each(|step| registrar.execute(step))
}

/// The real registrar: files on disk and, on Windows, the current user's registry hive.
#[derive(Debug, Default)]
pub struct SystemRegistrar;

impl Registrar for SystemRegistrar {
    fn execute(&mut self, step: &Step) -> Result<(), HostError> {
        match step {
            Step::WriteFile { path, contents } => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(path, contents)?;
                Ok(())
            }
            Step::RemoveFile { path } => match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            },
            Step::SetRegistryDefault { key, value } => registry::set_default(key, value),
            Step::DeleteRegistryKey { key } => registry::delete_key(key),
        }
    }
}

#[cfg(windows)]
mod registry {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    use crate::error::HostError;

    pub(super) fn set_default(key: &str, value: &str) -> Result<(), HostError> {
        let (subkey, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(key)?;
        subkey.set_value("", &value)?;
        Ok(())
    }

    pub(super) fn delete_key(key: &str) -> Result<(), HostError> {
        match RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(key) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

#[cfg(not(windows))]
mod registry {
    use crate::error::HostError;

    pub(super) fn set_default(_key: &str, _value: &str) -> Result<(), HostError> {
        Err(HostError::Install(
            "registry steps exist only on Windows".into(),
        ))
    }

    pub(super) fn delete_key(_key: &str) -> Result<(), HostError> {
        Err(HostError::Install(
            "registry steps exist only on Windows".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHROME_ID: &str = "abcdefghijklmnopabcdefghijklmnop";

    fn request(os: Os, browsers: &[Browser]) -> InstallRequest {
        InstallRequest {
            browsers: browsers.to_vec(),
            host_exe: PathBuf::from(if os == Os::Windows {
                r"C:\Zeus\zeus-native-host.exe"
            } else {
                "/opt/zeus/zeus-native-host"
            }),
            extension_ids: vec![CHROME_ID.into(), "zeus@zeus.invalid".into()],
            dev_extension_ids: vec![],
            home: PathBuf::from("/home/ada"),
            state_dir: PathBuf::from("/state"),
            os,
        }
    }

    #[test]
    fn chromium_manifests_pin_exact_origins_and_firefox_uses_add_on_ids() {
        let chrome =
            HostManifest::new(Browser::Chrome, Path::new("/x/host"), &[CHROME_ID.into()]).unwrap();
        assert_eq!(
            chrome.allowed_origins.unwrap(),
            [format!("chrome-extension://{CHROME_ID}/")]
        );
        assert!(chrome.allowed_extensions.is_none());
        let firefox = HostManifest::new(
            Browser::Firefox,
            Path::new("/x/host"),
            &["zeus@zeus.invalid".into()],
        )
        .unwrap();
        assert_eq!(firefox.allowed_extensions.unwrap(), ["zeus@zeus.invalid"]);
        assert!(firefox.allowed_origins.is_none());
    }

    #[test]
    fn wildcards_and_empty_pins_are_refused() {
        assert!(HostManifest::new(Browser::Chrome, Path::new("/x"), &[]).is_err());
        assert!(HostManifest::new(Browser::Chrome, Path::new("/x"), &["*".into()]).is_err());
    }

    #[test]
    fn manifest_json_has_the_documented_shape() {
        let json: serde_json::Value = serde_json::from_str(
            &HostManifest::new(Browser::Chrome, Path::new("/x/host"), &[CHROME_ID.into()])
                .unwrap()
                .to_json(),
        )
        .unwrap();
        assert_eq!(json["name"], "dev.zeus.native_host");
        assert_eq!(json["type"], "stdio");
        assert_eq!(json["path"], "/x/host");
    }

    #[test]
    fn locations_match_spec_02() {
        let home = Path::new("/home/ada");
        let state = Path::new("/state");
        assert_eq!(
            manifest_location(Browser::Chrome, Os::Linux, home, state),
            ManifestLocation::File(
                home.join(".config/google-chrome/NativeMessagingHosts/dev.zeus.native_host.json")
            )
        );
        assert_eq!(
            manifest_location(Browser::Firefox, Os::Linux, home, state),
            ManifestLocation::File(
                home.join(".mozilla/native-messaging-hosts/dev.zeus.native_host.json")
            )
        );
        assert_eq!(
            manifest_location(Browser::Edge, Os::MacOs, home, state),
            ManifestLocation::File(home.join("Library/Application Support/Microsoft Edge/NativeMessagingHosts/dev.zeus.native_host.json"))
        );
        match manifest_location(Browser::Chrome, Os::Windows, home, state) {
            ManifestLocation::Registry { key, .. } => assert_eq!(
                key,
                r"Software\Google\Chrome\NativeMessagingHosts\dev.zeus.native_host"
            ),
            other => panic!("{other:?}"),
        }
        match manifest_location(Browser::Firefox, Os::Windows, home, state) {
            ManifestLocation::Registry { key, .. } => assert_eq!(
                key,
                r"Software\Mozilla\NativeMessagingHosts\dev.zeus.native_host"
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ids_are_routed_to_the_right_browser_family() {
        let steps = plan(&request(Os::Linux, &[Browser::Chrome, Browser::Firefox])).unwrap();
        let manifests: Vec<_> = steps
            .iter()
            .filter_map(|s| match s {
                Step::WriteFile { path, contents }
                    if path.to_string_lossy().contains("NativeMessagingHosts")
                        || path.to_string_lossy().contains("native-messaging-hosts") =>
                {
                    Some(contents.clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(manifests.len(), 2);
        assert!(
            manifests[0].contains("allowed_origins")
                && manifests[0].contains(CHROME_ID)
                && !manifests[0].contains("zeus@zeus.invalid")
        );
        assert!(
            manifests[1].contains("allowed_extensions")
                && manifests[1].contains("zeus@zeus.invalid")
                && !manifests[1].contains(CHROME_ID)
        );
    }

    #[test]
    fn windows_plans_write_a_manifest_file_and_a_registry_value() {
        let steps = plan(&request(Os::Windows, &[Browser::Chrome])).unwrap();
        assert!(steps.iter().any(|s| matches!(s, Step::SetRegistryDefault { key, .. } if key.ends_with(r"NativeMessagingHosts\dev.zeus.native_host"))));
        assert!(steps.iter().any(
            |s| matches!(s, Step::WriteFile { path, .. } if path.ends_with(allowlist::FILE_NAME))
        ));
    }

    #[test]
    fn dev_ids_land_in_the_allow_list_under_dev_extensions() {
        let mut req = request(Os::Linux, &[Browser::Chrome]);
        req.extension_ids = vec![];
        req.dev_extension_ids = vec![CHROME_ID.into()];
        let steps = plan(&req).unwrap();
        let list_text = steps
            .iter()
            .find_map(|s| match s {
                Step::WriteFile { path, contents } if path.ends_with(allowlist::FILE_NAME) => {
                    Some(contents.clone())
                }
                _ => None,
            })
            .unwrap();
        let list: AllowList = serde_json::from_str(&list_text).unwrap();
        assert!(list.extensions.is_empty());
        assert_eq!(list.dev_extensions, [CHROME_ID]);
    }

    #[test]
    fn uninstall_mirrors_install() {
        let req = request(Os::Windows, &[Browser::Chrome, Browser::Edge]);
        let steps = plan_uninstall(&req);
        assert_eq!(
            steps
                .iter()
                .filter(|s| matches!(s, Step::DeleteRegistryKey { .. }))
                .count(),
            2
        );
        assert!(steps.iter().any(
            |s| matches!(s, Step::RemoveFile { path } if path.ends_with(allowlist::FILE_NAME))
        ));
    }

    #[test]
    fn the_system_registrar_writes_and_removes_real_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b.json");
        let mut registrar = SystemRegistrar;
        apply(
            &[Step::WriteFile {
                path: path.clone(),
                contents: "{}\n".into(),
            }],
            &mut registrar,
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}\n");
        apply(
            &[
                Step::RemoveFile { path: path.clone() },
                Step::RemoveFile { path },
            ],
            &mut registrar,
        )
        .unwrap();
    }
}
