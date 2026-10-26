//! Where the app keeps everything: one per-user folder, overridable for tests and portable use.
//!
//! * `ZEUS_APP_HOME` (an absolute path) wins, so an acceptance run or a second profile never touches the real folder.
//! * A folder named `ZeusData` next to the executable selects portable mode (the zip build ships an empty one).
//! * Otherwise the per-user data folder: `%LOCALAPPDATA%\dev.verne.zeus.client`, `~/.local/share/dev.verne.zeus.client`.
//!
//! Inside: `zeus/{state,config,run}` is the app's own Zeus installation (journal, credentials, `config.toml`), `app/`
//! holds what the client keeps (settings, lists, held approvals), `logs/` the daemon and app logs, `webview/` the
//! system webview's data, `downloads/` the download folder in portable or overridden mode.

use std::path::{Path, PathBuf};

use crate::failure::{Failure, Result};

/// The profile name that keeps this installation's endpoint apart from any other Zeus on the same account.
pub const PROFILE: &str = "zeus-app";

/// The profile of the installation under `root`. The default installation uses [`PROFILE`]; a contained one (a test
/// folder, a portable copy) gets its own, so two of them never share an endpoint.
pub fn profile_for(root: &Path, contained: bool) -> String {
    if !contained {
        return PROFILE.into();
    }
    use sha2::{Digest, Sha256};
    let digest = hex::encode(Sha256::digest(
        root.display().to_string().to_lowercase().as_bytes(),
    ));
    format!("{PROFILE}-{}", &digest[..8])
}

/// The app's folders.
#[derive(Debug, Clone)]
pub struct AppHome {
    /// The root of everything below.
    pub root: PathBuf,
    /// True in portable mode or when `ZEUS_APP_HOME` is set: downloads stay inside the root.
    pub contained: bool,
}

impl AppHome {
    /// Resolve the root from the environment and the executable's folder.
    pub fn resolve(exe_dir: Option<&Path>) -> Result<Self> {
        if let Some(root) = std::env::var_os("ZEUS_APP_HOME").filter(|v| !v.is_empty()) {
            let root = PathBuf::from(root);
            if !root.is_absolute() {
                return Err(Failure::new(
                    "app.home",
                    "ZEUS_APP_HOME must be an absolute path.",
                ));
            }
            return Ok(Self {
                root,
                contained: true,
            });
        }
        if let Some(dir) = exe_dir {
            let portable = dir.join("ZeusData");
            if portable.is_dir() {
                return Ok(Self {
                    root: portable,
                    contained: true,
                });
            }
        }
        let base = directories::BaseDirs::new()
            .ok_or_else(|| Failure::new("app.home", "Cannot find your user data folder."))?;
        Ok(Self {
            root: base.data_local_dir().join("dev.verne.zeus.client"),
            contained: false,
        })
    }

    /// An explicit root (tests).
    pub fn at(root: PathBuf) -> Self {
        Self {
            root,
            contained: true,
        }
    }

    /// The engine's state folder (journal, credentials, IPC secret).
    pub fn state_dir(&self) -> PathBuf {
        self.root.join("zeus").join("state")
    }

    /// The engine's configuration folder (`config.toml`).
    pub fn config_dir(&self) -> PathBuf {
        self.root.join("zeus").join("config")
    }

    /// The engine's runtime folder (Unix socket).
    pub fn run_dir(&self) -> PathBuf {
        self.root.join("zeus").join("run")
    }

    /// What the client keeps: settings slices, held approvals, integration files.
    pub fn app_dir(&self) -> PathBuf {
        self.root.join("app")
    }

    /// Logs of the app and its engine.
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// The system webview's data folder.
    pub fn webview_dir(&self) -> PathBuf {
        self.root.join("webview")
    }

    /// The default download folder: the user's Downloads folder plus `Zeus`, or `downloads/` when contained.
    pub fn default_download_root(&self) -> PathBuf {
        if self.contained {
            return self.root.join("downloads");
        }
        directories::UserDirs::new()
            .and_then(|d| d.download_dir().map(Path::to_path_buf))
            .or_else(|| directories::UserDirs::new().map(|d| d.home_dir().join("Downloads")))
            .map_or_else(|| self.root.join("downloads"), |d| d.join("Zeus"))
    }

    /// The environment that points `zeus`, `zeusd` and `zeus-native-host` at this installation.
    pub fn zeus_env(&self) -> Vec<(String, String)> {
        let path = |p: PathBuf| p.display().to_string();
        vec![
            (
                "ZEUS_PROFILE".into(),
                profile_for(&self.root, self.contained),
            ),
            ("ZEUS_STATE_DIR".into(), path(self.state_dir())),
            ("ZEUS_CONFIG_DIR".into(), path(self.config_dir())),
            ("ZEUS_RUNTIME_DIR".into(), path(self.run_dir())),
        ]
    }

    /// Create the folders that must exist before the engine starts.
    pub fn create(&self) -> Result<()> {
        for dir in [
            self.state_dir(),
            self.config_dir(),
            self.run_dir(),
            self.app_dir(),
            self.logs_dir(),
            self.webview_dir(),
        ] {
            std::fs::create_dir_all(&dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_contained_home_keeps_downloads_inside_it() {
        let home = AppHome::at(PathBuf::from("/tmp/zeus-home"));
        assert!(home.default_download_root().starts_with("/tmp/zeus-home"));
        let env = home.zeus_env();
        assert!(
            env.iter()
                .any(|(k, v)| k == "ZEUS_PROFILE" && v.starts_with(PROFILE) && v.len() <= 32)
        );
        assert_ne!(
            profile_for(std::path::Path::new("/a"), true),
            profile_for(std::path::Path::new("/b"), true)
        );
        assert_eq!(profile_for(std::path::Path::new("/a"), false), PROFILE);
        assert!(
            env.iter()
                .any(|(k, v)| k == "ZEUS_STATE_DIR" && v.contains("state"))
        );
    }
}
