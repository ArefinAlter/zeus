//! Endpoint naming, the discovery file and the per-user directories.
//!
//! * Windows endpoint: `\\.\pipe\zeusd-<h>`, `h` = first 16 hex digits of BLAKE3 over `"zeus-endpoint-v1"` and the
//!   user's identity (ADR 0002). The identity is `DOMAIN\user` here; a SID-based identity is a transport detail
//!   that does not change the derivation function.
//! * Unix endpoint: `<runtime>/zeus/zeusd.sock` inside a `0700` directory; if the path would not fit `sun_path`
//!   (104 bytes on macOS, 108 on Linux) a short fallback under the temp directory is used.
//! * State layout follows spec 03 section 3.1: `%LOCALAPPDATA%\Zeus`, `~/Library/Application Support/Zeus`,
//!   `$XDG_STATE_HOME/zeus`.

use std::io::Write;
use std::path::{Path, PathBuf};

use directories::BaseDirs;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeus_types::Timestamp;

use crate::error::{ApiError, codes};

/// Where the daemon listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// A Windows named pipe, full name `\\.\pipe\...`.
    NamedPipe(String),
    /// A Unix domain socket path.
    UnixSocket(PathBuf),
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NamedPipe(name) => f.write_str(name),
            Self::UnixSocket(path) => write!(f, "{}", path.display()),
        }
    }
}

/// Pipe name for a user identity. Deterministic, so clients find the daemon without a lookup.
pub fn pipe_name_for(user_identity: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"zeus-endpoint-v1");
    hasher.update(user_identity.as_bytes());
    let digest = hasher.finalize().to_hex();
    format!(r"\\.\pipe\zeusd-{}", &digest.as_str()[..16])
}

/// Longest socket path accepted before falling back (below the smallest `sun_path`, 104 on macOS).
const MAX_SOCKET_PATH_BYTES: usize = 100;

/// Socket path inside `runtime_dir`, or a short fallback when it would not fit.
pub fn unix_socket_path(runtime_dir: &Path, user_identity: &str) -> PathBuf {
    let preferred = runtime_dir.join("zeus").join("zeusd.sock");
    if preferred.as_os_str().len() <= MAX_SOCKET_PATH_BYTES {
        return preferred;
    }
    let digest = blake3::hash(user_identity.as_bytes()).to_hex();
    std::env::temp_dir()
        .join(format!("zeus-{}", &digest.as_str()[..16]))
        .join("zeusd.sock")
}

/// OS identity used for endpoint naming: SID on Windows, effective uid on Unix.
pub fn current_user_identity() -> Result<String, ApiError> {
    crate::transport::user_identity()
}

/// The directories Zeus uses for one user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZeusPaths {
    /// Journal, audit log, credentials, discovery file (owner only).
    pub state_dir: PathBuf,
    /// `config.toml`.
    pub config_dir: PathBuf,
    /// Unix socket directory; on Windows only used for the discovery file's sibling files.
    pub runtime_dir: PathBuf,
    /// Identity used for endpoint naming.
    pub user_identity: String,
    /// The selected profile, `None` for the default one. Its daemon has its own endpoint.
    pub profile: Option<String>,
}

/// Check a profile name: `[a-z][a-z0-9_-]{0,31}`. `default` (and an empty value) select the default directories.
pub fn profile_name(name: &str) -> Result<Option<&str>, ApiError> {
    if name.is_empty() || name == "default" {
        return Ok(None);
    }
    let mut chars = name.chars();
    let valid = name.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if valid {
        Ok(Some(name))
    } else {
        Err(ApiError::invalid_params(
            "profile names use lowercase letters, digits, - and _, start with a letter and have at most 32 characters",
        ))
    }
}

impl ZeusPaths {
    /// Resolve from the process environment (`ZEUS_STATE_DIR`, `ZEUS_CONFIG_DIR`, `ZEUS_RUNTIME_DIR` override;
    /// `ZEUS_PROFILE` selects a profile).
    pub fn discover() -> Result<Self, ApiError> {
        Self::resolve(
            |key| std::env::var(key).ok(),
            BaseDirs::new(),
            current_user_identity()?,
        )
    }

    /// Resolve from the process environment with an explicit profile (the CLI `--profile` flag), which wins over
    /// `ZEUS_PROFILE`.
    pub fn discover_profile(profile: Option<&str>) -> Result<Self, ApiError> {
        let profile = profile.map(str::to_owned);
        Self::resolve(
            |key| match (key, &profile) {
                ("ZEUS_PROFILE", Some(p)) => Some(p.clone()),
                _ => std::env::var(key).ok(),
            },
            BaseDirs::new(),
            current_user_identity()?,
        )
    }

    /// Resolution with an injectable environment and base directories, for tests.
    ///
    /// A profile (`ZEUS_PROFILE`) is a separate namespace: each default directory gains `profiles/<name>`, and the
    /// endpoint identity gains the profile, so two profiles never share state, credentials or a daemon. Explicit
    /// directory overrides are used exactly as given.
    pub fn resolve(
        env: impl Fn(&str) -> Option<String>,
        base: Option<BaseDirs>,
        user_identity: String,
    ) -> Result<Self, ApiError> {
        let profile =
            profile_name(env("ZEUS_PROFILE").as_deref().unwrap_or_default())?.map(str::to_owned);
        let scoped = |dir: PathBuf| match &profile {
            Some(name) => dir.join("profiles").join(name),
            None => dir,
        };
        let from_env = |key: &str| env(key).filter(|v| !v.is_empty()).map(PathBuf::from);
        let no_home = || {
            ApiError::new(
                codes::UNAVAILABLE,
                "cannot determine the user's home directory",
            )
        };
        let state_dir = match from_env("ZEUS_STATE_DIR") {
            Some(path) => path,
            None => {
                let base = base.as_ref().ok_or_else(no_home)?;
                scoped(if cfg!(windows) {
                    base.data_local_dir().join("Zeus")
                } else if cfg!(target_os = "macos") {
                    base.data_dir().join("Zeus")
                } else {
                    base.state_dir()
                        .unwrap_or_else(|| base.data_local_dir())
                        .join("zeus")
                })
            }
        };
        let config_dir = match from_env("ZEUS_CONFIG_DIR") {
            Some(path) => path,
            None => {
                let base = base.as_ref().ok_or_else(no_home)?;
                scoped(if cfg!(windows) {
                    base.config_dir().join("Zeus")
                } else if cfg!(target_os = "macos") {
                    base.data_dir().join("Zeus")
                } else {
                    base.config_dir().join("zeus")
                })
            }
        };
        let runtime_dir = match from_env("ZEUS_RUNTIME_DIR") {
            Some(path) => path,
            None => base
                .as_ref()
                .and_then(|b| b.runtime_dir().map(|dir| scoped(dir.to_path_buf())))
                .unwrap_or_else(|| state_dir.join("run")),
        };
        Ok(Self {
            state_dir,
            config_dir,
            runtime_dir,
            user_identity,
            profile,
        })
    }

    /// The identity endpoint names derive from: the user, plus the profile when one is selected.
    fn endpoint_identity(&self) -> String {
        match &self.profile {
            Some(name) => format!("{}#profile={name}", self.user_identity),
            None => self.user_identity.clone(),
        }
    }

    /// The endpoint the daemon of this user and profile listens on.
    pub fn endpoint(&self) -> Endpoint {
        if cfg!(windows) {
            Endpoint::NamedPipe(pipe_name_for(&self.endpoint_identity()))
        } else {
            Endpoint::UnixSocket(unix_socket_path(
                &self.runtime_dir,
                &self.endpoint_identity(),
            ))
        }
    }

    /// Advisory lock held for the daemon's life (03 section 3.1).
    pub fn lock_file(&self) -> PathBuf {
        self.state_dir.join("zeusd.lock")
    }

    /// The discovery record written after the listener is up.
    pub fn discovery_file(&self) -> PathBuf {
        self.state_dir.join("zeusd.json")
    }

    /// The IPC secret (`0600`).
    pub fn ipc_secret_file(&self) -> PathBuf {
        self.state_dir.join("ipc.secret")
    }

    /// Directory of client credential files (`0700`): `cli.token`, `agent.token`, `host.token`.
    pub fn credentials_dir(&self) -> PathBuf {
        self.state_dir.join("credentials")
    }

    /// The credential file for a role name such as `cli`, `agent` or `host`.
    pub fn credential_file(&self, name: &str) -> PathBuf {
        self.credentials_dir().join(format!("{name}.token"))
    }

    /// `config.toml`.
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
}

/// The record `zeusd` writes to `<state>/zeusd.json` once it is listening, and removes on clean exit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiscoveryRecord {
    /// Process id.
    pub pid: u32,
    /// Random id of this run.
    pub instance_id: String,
    /// Zeus version.
    pub version: String,
    /// Protocol versions spoken.
    pub protocol_versions: Vec<u16>,
    /// Endpoint text (pipe name or socket path).
    pub endpoint: String,
    /// Start time.
    pub started_at: Timestamp,
}

impl DiscoveryRecord {
    /// Write atomically (temp file in the same directory, then rename).
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        let dir = path
            .parent()
            .ok_or_else(|| std::io::Error::other("discovery path has no parent"))?;
        std::fs::create_dir_all(dir)?;
        let temp = dir.join(format!(".zeusd.json.{}.tmp", std::process::id()));
        {
            let mut file = std::fs::File::create(&temp)?;
            file.write_all(&serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
            file.sync_all()?;
        }
        std::fs::rename(&temp, path)
    }

    /// Read the record; `Ok(None)` when absent. Whether the daemon is alive is decided by the lock, not by this
    /// file: a record without a held lock is stale (see [`DiscoveryRecord::is_stale`]).
    pub fn read(path: &Path) -> std::io::Result<Option<Self>> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(std::io::Error::other),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// A record whose daemon no longer holds the lock describes a dead process.
    pub fn is_stale(&self, lock_held: bool) -> bool {
        !lock_held
    }

    /// Remove the record on clean shutdown. Missing is fine.
    pub fn remove(path: &Path) -> std::io::Result<()> {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn pipe_names_are_deterministic_per_user_and_well_formed() {
        let a = pipe_name_for(r"WORKGROUP\ada");
        assert_eq!(a, pipe_name_for(r"WORKGROUP\ada"));
        assert_ne!(a, pipe_name_for(r"WORKGROUP\bob"));
        assert!(a.starts_with(r"\\.\pipe\zeusd-"));
        assert_eq!(a.len(), r"\\.\pipe\zeusd-".len() + 16);
        assert!(
            a[r"\\.\pipe\zeusd-".len()..]
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        );
    }

    #[test]
    fn socket_paths_fall_back_when_too_long() {
        let short = unix_socket_path(Path::new("/run/user/1000"), "ada");
        assert_eq!(short, Path::new("/run/user/1000/zeus/zeusd.sock"));
        let long_dir = PathBuf::from(format!("/{}", "x".repeat(120)));
        let fallback = unix_socket_path(&long_dir, "ada");
        assert!(fallback.as_os_str().len() <= MAX_SOCKET_PATH_BYTES + 20);
        assert!(fallback.starts_with(std::env::temp_dir()));
    }

    #[test]
    fn environment_overrides_win() {
        let env: HashMap<&str, &str> = [
            ("ZEUS_STATE_DIR", "/s"),
            ("ZEUS_CONFIG_DIR", "/c"),
            ("ZEUS_RUNTIME_DIR", "/r"),
        ]
        .into();
        let paths =
            ZeusPaths::resolve(|k| env.get(k).map(|v| (*v).to_string()), None, "ada".into())
                .unwrap();
        assert_eq!(paths.state_dir, Path::new("/s"));
        assert_eq!(paths.config_dir, Path::new("/c"));
        assert_eq!(paths.runtime_dir, Path::new("/r"));
        assert_eq!(paths.lock_file(), Path::new("/s/zeusd.lock"));
        assert_eq!(
            paths.credential_file("agent"),
            Path::new("/s/credentials/agent.token")
        );
        assert_eq!(paths.config_file(), Path::new("/c/config.toml"));
    }

    #[test]
    fn a_profile_is_a_separate_namespace_with_its_own_endpoint() {
        let resolve = |profile: Option<&str>| {
            let mut env: HashMap<&str, String> = HashMap::new();
            if let Some(p) = profile {
                env.insert("ZEUS_PROFILE", p.into());
            }
            ZeusPaths::resolve(|k| env.get(k).cloned(), BaseDirs::new(), "ada".into())
        };
        let default = resolve(None).unwrap();
        assert_eq!(resolve(Some("default")).unwrap(), default);
        assert_eq!(resolve(Some("")).unwrap(), default);
        let work = resolve(Some("work")).unwrap();
        assert_eq!(
            work.state_dir,
            default.state_dir.join("profiles").join("work")
        );
        assert_eq!(
            work.config_dir,
            default.config_dir.join("profiles").join("work")
        );
        assert_ne!(work.endpoint(), default.endpoint());
        assert_ne!(work.endpoint(), resolve(Some("home")).unwrap().endpoint());
        for bad in ["Work", "1st", "a b", "../x", &"x".repeat(33)] {
            assert!(resolve(Some(bad)).is_err(), "{bad}");
        }
        // Explicit directories are exact, but the endpoint still belongs to the profile.
        let env: HashMap<&str, &str> = [("ZEUS_STATE_DIR", "/s"), ("ZEUS_PROFILE", "work")].into();
        let explicit =
            ZeusPaths::resolve(|k| env.get(k).map(|v| (*v).to_string()), None, "ada".into())
                .unwrap_err();
        // Without a home directory the config directory cannot be derived; with it the state stays exact.
        assert_eq!(explicit.envelope().code.as_str(), codes::UNAVAILABLE);
        let explicit = ZeusPaths::resolve(
            |k| env.get(k).map(|v| (*v).to_string()),
            BaseDirs::new(),
            "ada".into(),
        )
        .unwrap();
        assert_eq!(explicit.state_dir, Path::new("/s"));
        assert_eq!(explicit.endpoint(), work.endpoint());
    }

    #[test]
    fn without_a_home_directory_resolution_fails_cleanly() {
        let error = ZeusPaths::resolve(|_| None, None, "ada".into()).unwrap_err();
        assert_eq!(error.envelope().code.as_str(), codes::UNAVAILABLE);
    }

    #[test]
    fn default_locations_follow_spec_03() {
        let Some(base) = BaseDirs::new() else { return };
        let paths = ZeusPaths::resolve(|_| None, Some(base), "ada".into()).unwrap();
        let state = paths.state_dir.to_string_lossy().to_lowercase();
        assert!(state.ends_with("zeus"), "{state}");
        if cfg!(windows) {
            assert!(matches!(paths.endpoint(), Endpoint::NamedPipe(_)));
        } else {
            assert!(matches!(paths.endpoint(), Endpoint::UnixSocket(_)));
        }
    }

    #[test]
    fn discovery_record_round_trips_and_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state").join("zeusd.json");
        assert!(DiscoveryRecord::read(&path).unwrap().is_none());
        let record = DiscoveryRecord {
            pid: 4242,
            instance_id: "3f9a6c2d1b7e4a08c5d2e9f1a0b3c4d5".into(),
            version: "0.1.0".into(),
            protocol_versions: vec![1],
            endpoint: pipe_name_for("ada"),
            started_at: Timestamp::now(),
        };
        record.write(&path).unwrap();
        assert_eq!(DiscoveryRecord::read(&path).unwrap().unwrap(), record);
        assert!(record.is_stale(false) && !record.is_stale(true));
        DiscoveryRecord::remove(&path).unwrap();
        DiscoveryRecord::remove(&path).unwrap();
        assert!(DiscoveryRecord::read(&path).unwrap().is_none());
    }
}
