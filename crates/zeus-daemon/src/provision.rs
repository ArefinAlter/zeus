//! First-run state: the IPC secret and the client credentials.
//!
//! `zeusd init` (and `zeus daemon install`) call [`provision`]. It is idempotent: existing secrets and
//! credentials are never rotated or overwritten. Files are created owner-only (`0600` in a `0700` directory on
//! Unix; on Windows they inherit the ACL of `%LOCALAPPDATA%\Zeus`, which grants the user alone). The daemon stores
//! only the **derived verifier** of each token, never the token (see `zeus_api::auth`).
//!
//! Residual risk (ADR 0007): any process running as the same OS user can read these files. Credentials separate
//! *kinds* of client, they do not defend against same-user code; approvals therefore also need channel evidence.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeus_api::auth::{
    AuthKey, ClientCredential, Credential, CredentialId, CredentialStore, IpcSecret, Token,
};
use zeus_api::endpoint::ZeusPaths;
use zeus_api::{ApiError, ClientKind, ClientRole, Permission};
use zeus_types::Timestamp;

/// Provisioning failures.
#[derive(Debug, Error)]
pub enum ProvisionError {
    /// File system error.
    #[error("{what}: {source}")]
    Io {
        /// What was being done.
        what: &'static str,
        /// Underlying error.
        source: std::io::Error,
    },
    /// A stored file is corrupt.
    #[error("{0}")]
    Corrupt(String),
}

fn io(what: &'static str) -> impl Fn(std::io::Error) -> ProvisionError {
    move |source| ProvisionError::Io { what, source }
}

/// The default credentials, by file name.
pub const DEFAULT_CREDENTIALS: &[(&str, ClientKind, ClientRole)] = &[
    ("cli", ClientKind::Admin, ClientRole::Cli),
    ("agent", ClientKind::Agent, ClientRole::Mcp),
    ("a2a", ClientKind::Agent, ClientRole::A2a),
    ("host", ClientKind::Human, ClientRole::NativeHost),
];

/// What [`provision`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ProvisionReport {
    /// The IPC secret file was created.
    pub created_ipc_secret: bool,
    /// Credential files created, by name (`cli`, `agent`, `a2a`, `host`).
    pub created_credentials: Vec<String>,
}

/// Create a directory owner-only.
fn create_private_dir(path: &Path) -> Result<(), ProvisionError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(io("create state directory"))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path).map_err(io("create state directory"))?;
    zeus_api::transport::secure_private_directory(path).map_err(io("protect private directory"))
}

/// Write a file owner-only, atomically (temp file then rename).
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), ProvisionError> {
    let dir = path
        .parent()
        .ok_or_else(|| ProvisionError::Corrupt("path has no parent".into()))?;
    let temp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let _ = std::fs::remove_file(&temp);
    let mut file = options.open(&temp).map_err(io("create temporary file"))?;
    zeus_api::transport::secure_private_file(&temp).map_err(io("protect private file"))?;
    file.write_all(bytes).map_err(io("write file"))?;
    file.sync_all().map_err(io("sync file"))?;
    drop(file);
    std::fs::rename(&temp, path).map_err(io("rename file"))
}

/// One credential record as stored in `credentials.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredCredential {
    id: String,
    kind: ClientKind,
    role: ClientRole,
    permissions: BTreeSet<Permission>,
    /// Hex of the derived verifier.
    auth_key: String,
    created_at: Timestamp,
    revoked: bool,
    /// File name stem under `credentials/`. Older stores omit it; it is inferred for the default credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revoked_at: Option<Timestamp>,
    /// Service clients only (05 section 7.6). Inactive once the credential is revoked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    roots: Vec<StoredRoot>,
}

/// A service-owned write root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredRoot {
    path: std::path::PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    created_at: Timestamp,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    credentials: Vec<StoredCredential>,
}

fn stored_from(credential: &Credential) -> StoredCredential {
    StoredCredential {
        id: credential.id.to_string(),
        kind: credential.kind,
        role: credential.role,
        permissions: credential.permissions.clone(),
        auth_key: hex::encode(credential.auth_key.to_bytes().as_slice()),
        created_at: credential.created_at,
        revoked: credential.revoked,
        name: None,
        revoked_at: None,
        roots: Vec::new(),
    }
}

fn credential_from(stored: &StoredCredential) -> Result<Credential, ProvisionError> {
    let bytes: [u8; 32] = hex::decode(&stored.auth_key)
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
        .ok_or_else(|| {
            ProvisionError::Corrupt("credentials.json holds a malformed verifier".into())
        })?;
    let id = CredentialId::parse(&stored.id)
        .map_err(|e: ApiError| ProvisionError::Corrupt(e.to_string()))?;
    Ok(Credential {
        id,
        kind: stored.kind,
        role: stored.role,
        permissions: stored.permissions.clone(),
        auth_key: AuthKey::from_bytes(bytes),
        created_at: stored.created_at,
        revoked: stored.revoked,
    })
}

/// What `clients.add` asks the store to create.
#[derive(Debug, Clone)]
pub struct NewCredential<'a> {
    /// The id the caller minted (and already wrote into the audit record).
    pub id: CredentialId,
    /// Token file name stem.
    pub name: &'a str,
    /// `agent` or `service`.
    pub kind: ClientKind,
    /// Requested role, if any.
    pub role: Option<ClientRole>,
    /// Narrowing within the kind's ceiling.
    pub permissions: Option<BTreeSet<Permission>>,
}

/// Names of the provisioned credentials; `clients.add` cannot reuse them.
pub const RESERVED_NAMES: &[&str] = &["cli", "agent", "a2a", "host"];
/// Roots one service credential may register (05 section 13.1).
pub const MAX_SERVICE_ROOTS: usize = 16;
/// Credentials one store holds, revoked ones included, so the index stays small.
pub const MAX_CREDENTIALS: usize = 256;

#[derive(Default)]
struct Records {
    stored: Vec<StoredCredential>,
    credentials: Vec<Credential>,
}

/// Credential store backed by `<state>/credentials.json`.
///
/// The daemon's handshake and its handlers share one store, so `clients.add`, `clients.revoke` and
/// `files.roots.register` take effect for new and existing connections at once. Writes are atomic (temporary file,
/// fsync, rename) and serialised; memory changes only after the file is durable.
pub struct FileCredentialStore {
    path: std::path::PathBuf,
    records: RwLock<Records>,
    writer: std::sync::Mutex<()>,
    last_used: std::sync::Mutex<std::collections::BTreeMap<CredentialId, Timestamp>>,
    revocations: tokio::sync::watch::Sender<u64>,
}

fn invalid(message: &str) -> ApiError {
    ApiError::invalid_params(message.to_owned())
}

/// A credential name: lowercase letter first, then lowercase letters, digits, `-` or `_`, at most 32 bytes.
pub(crate) fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

impl FileCredentialStore {
    /// Load the store. A missing file is an empty store. Records written before names were stored get the name of
    /// the provisioned token file that holds their id.
    pub fn load(path: &Path) -> Result<Self, ProvisionError> {
        let mut stored = match std::fs::read(path) {
            Ok(bytes) => {
                let file: StoreFile = serde_json::from_slice(&bytes)
                    .map_err(|e| ProvisionError::Corrupt(format!("credentials.json: {e}")))?;
                if file.version != 1 {
                    return Err(ProvisionError::Corrupt(format!(
                        "credentials.json has unsupported version {}",
                        file.version
                    )));
                }
                file.credentials
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(source) => {
                return Err(ProvisionError::Io {
                    what: "read credentials.json",
                    source,
                });
            }
        };
        let dir = path
            .parent()
            .map(|p| p.join("credentials"))
            .unwrap_or_default();
        for name in RESERVED_NAMES {
            let Some(id) = std::fs::read_to_string(dir.join(format!("{name}.token")))
                .ok()
                .and_then(|text| ClientCredential::from_file_text(&text).ok())
                .map(|c| c.id.to_string())
            else {
                continue;
            };
            if let Some(record) = stored.iter_mut().find(|r| r.id == id && r.name.is_none()) {
                record.name = Some((*name).to_string());
            }
        }
        let credentials = stored
            .iter()
            .map(credential_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            path: path.to_path_buf(),
            records: RwLock::new(Records {
                stored,
                credentials,
            }),
            writer: Default::default(),
            last_used: Default::default(),
            revocations: tokio::sync::watch::Sender::new(0),
        })
    }

    /// A receiver that changes whenever a credential is revoked, so live connections can close.
    pub fn revocations(&self) -> tokio::sync::watch::Receiver<u64> {
        self.revocations.subscribe()
    }

    /// Record a successful authentication (kept in memory; reported as `last_used_at`).
    pub fn touch(&self, id: &CredentialId, now: Timestamp) {
        if let Ok(mut used) = self.last_used.lock() {
            used.insert(id.clone(), now);
        }
    }

    fn view(&self, stored: &StoredCredential) -> zeus_api::methods::admin::ClientRecord {
        let id = CredentialId::parse(&stored.id).ok();
        zeus_api::methods::admin::ClientRecord {
            credential_id: stored.id.clone(),
            name: stored.name.clone().unwrap_or_else(|| {
                format!(
                    "unnamed-{}",
                    stored.id[stored.id.len() - 6..].to_lowercase()
                )
            }),
            kind: stored.kind,
            role: stored.role,
            permissions: stored.permissions.clone(),
            created_at: stored.created_at,
            last_used_at: id.and_then(|id| self.last_used.lock().ok()?.get(&id).copied()),
            revoked: stored.revoked,
            revoked_at: stored.revoked_at,
            roots: stored
                .roots
                .iter()
                .map(|r| zeus_api::methods::admin::ServiceRoot {
                    path: r.path.display().to_string(),
                    label: r.label.clone(),
                    client_id: stored.id.clone(),
                    created_at: r.created_at,
                })
                .collect(),
        }
    }

    /// Every credential, oldest first; revoked ones only when asked. Never a token or verifier.
    pub fn records(&self, include_revoked: bool) -> Vec<zeus_api::methods::admin::ClientRecord> {
        let Ok(records) = self.records.read() else {
            return Vec::new();
        };
        let mut list: Vec<_> = records
            .stored
            .iter()
            .filter(|r| include_revoked || !r.revoked)
            .map(|r| self.view(r))
            .collect();
        list.sort_by(|a, b| {
            (a.created_at, &a.credential_id).cmp(&(b.created_at, &b.credential_id))
        });
        list
    }

    /// One credential's record.
    pub fn record(&self, id: &CredentialId) -> Option<zeus_api::methods::admin::ClientRecord> {
        let records = self.records.read().ok()?;
        records
            .stored
            .iter()
            .find(|r| r.id == id.as_str())
            .map(|r| self.view(r))
    }

    /// Apply `change` to a copy of the records, persist it, then publish it. Nothing changes on failure.
    fn mutate<T>(
        &self,
        change: impl FnOnce(&mut Vec<StoredCredential>) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        let _writer = self
            .writer
            .lock()
            .map_err(|_| ApiError::internal("credential store unavailable"))?;
        let mut stored = self
            .records
            .read()
            .map_err(|_| ApiError::internal("credential store unavailable"))?
            .stored
            .clone();
        let value = change(&mut stored)?;
        let credentials = stored
            .iter()
            .map(credential_from)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ApiError::internal("credential record invalid"))?;
        let body = serde_json::to_vec_pretty(&StoreFile {
            version: 1,
            credentials: stored.clone(),
        })
        .map_err(|_| ApiError::internal("credential index serialization"))?;
        write_private(&self.path, &body).map_err(|_| {
            ApiError::new(
                zeus_api::error::codes::UNAVAILABLE,
                "credential index could not be written",
            )
        })?;
        *self
            .records
            .write()
            .map_err(|_| ApiError::internal("credential store unavailable"))? = Records {
            stored,
            credentials,
        };
        Ok(value)
    }

    /// Create an `agent` or `service` credential (05 section 9.11) with the id the caller minted, so the
    /// write-ahead audit record can name it: mint the token, write the token file owner-only, then persist
    /// the verifier. Returns the record and the token file path; the token itself never leaves the file.
    /// API-created agents use the `mcp`, `a2a` or `embedded` role, so a requester label can never read as
    /// the human's CLI.
    pub fn add(
        &self,
        credentials_dir: &Path,
        request: NewCredential<'_>,
        now: Timestamp,
    ) -> Result<(zeus_api::methods::admin::ClientRecord, std::path::PathBuf), ApiError> {
        let NewCredential {
            id,
            name,
            kind,
            role,
            permissions,
        } = request;
        if !valid_name(name) || RESERVED_NAMES.contains(&name) {
            return Err(invalid(
                "name must match [a-z][a-z0-9_-]{0,31} and must not be cli, agent, a2a or host",
            ));
        }
        let role = match (kind, role) {
            (ClientKind::Service, None | Some(ClientRole::Service)) => ClientRole::Service,
            (ClientKind::Agent, None) => ClientRole::Mcp,
            (
                ClientKind::Agent,
                Some(role @ (ClientRole::Mcp | ClientRole::A2a | ClientRole::Embedded)),
            ) => role,
            (ClientKind::Agent | ClientKind::Service, Some(_)) => {
                return Err(invalid("that role is not allowed for this kind"));
            }
            _ => {
                return Err(invalid(
                    "only agent and service credentials are created through the API",
                ));
            }
        };
        let ceiling = kind.default_permissions();
        let permissions = permissions.unwrap_or_else(|| ceiling.clone());
        if !permissions.is_subset(&ceiling) {
            return Err(invalid("permissions may only narrow the kind's ceiling"));
        }
        let token = Token::generate();
        let mut credential = Credential::new(id.clone(), kind, role, token.auth_key(), now)?;
        credential.permissions = permissions;
        let file = credentials_dir.join(format!("{name}.token"));
        let mut record = stored_from(&credential);
        record.name = Some(name.to_owned());
        let view = self.mutate(|stored| {
            if stored
                .iter()
                .any(|r| !r.revoked && r.name.as_deref() == Some(name))
            {
                return Err(ApiError::new(
                    "api.conflict",
                    "a credential with that name exists",
                ));
            }
            if stored.len() >= MAX_CREDENTIALS {
                return Err(ApiError::new(
                    zeus_api::error::codes::RATE_LIMITED,
                    "credential limit reached",
                )
                .with_detail("limit", "credentials"));
            }
            // The token file goes first: an index without its file cannot authenticate anyone.
            write_private(
                &file,
                ClientCredential {
                    id: id.clone(),
                    token,
                }
                .to_file_text()
                .as_bytes(),
            )
            .map_err(|_| {
                ApiError::new(
                    zeus_api::error::codes::UNAVAILABLE,
                    "credential file could not be written",
                )
            })?;
            stored.push(record.clone());
            Ok(())
        });
        if let Err(error) = view {
            if !self.records.read().is_ok_and(|r| {
                r.stored
                    .iter()
                    .any(|s| !s.revoked && s.name.as_deref() == Some(name))
            }) {
                let _ = std::fs::remove_file(&file);
            }
            return Err(error);
        }
        Ok((self.view(&record), file))
    }

    /// Revoke a credential (idempotent), deactivate its roots and remove its token file. Live connections are told
    /// through [`Self::revocations`].
    pub fn revoke(
        &self,
        credentials_dir: &Path,
        id: &CredentialId,
        now: Timestamp,
    ) -> Result<zeus_api::methods::admin::ClientRecord, ApiError> {
        let (record, changed) = self.mutate(|stored| {
            let record = stored
                .iter_mut()
                .find(|r| r.id == id.as_str())
                .ok_or_else(|| ApiError::new("api.not_found", "credential not found"))?;
            let changed = !record.revoked;
            if changed {
                record.revoked = true;
                record.revoked_at = Some(now);
            }
            Ok((record.clone(), changed))
        })?;
        if changed {
            if let Some(name) = &record.name {
                let _ = std::fs::remove_file(credentials_dir.join(format!("{name}.token")));
            }
            self.revocations.send_modify(|n| *n = n.wrapping_add(1));
        }
        Ok(self.view(&record))
    }

    /// Register a canonical root for an active service credential. The same path again returns the existing root
    /// with `false`. Overlap with another service's root is refused, so roots never widen visibility.
    pub fn register_root(
        &self,
        id: &CredentialId,
        path: std::path::PathBuf,
        label: Option<String>,
        now: Timestamp,
    ) -> Result<(zeus_api::methods::admin::ServiceRoot, bool), ApiError> {
        let (root, created) = self.mutate(|stored| {
            if stored.iter().any(|r| {
                r.id != id.as_str()
                    && r.kind == ClientKind::Service
                    && !r.revoked
                    && r.roots
                        .iter()
                        .any(|o| path.starts_with(&o.path) || o.path.starts_with(&path))
            }) {
                return Err(invalid("the path overlaps another service's root"));
            }
            let record = stored
                .iter_mut()
                .find(|r| r.id == id.as_str() && !r.revoked && r.kind == ClientKind::Service)
                .ok_or_else(|| ApiError::new("api.not_found", "service credential not found"))?;
            if let Some(existing) = record.roots.iter().find(|r| r.path == path) {
                return Ok((existing.clone(), false));
            }
            if record.roots.len() >= MAX_SERVICE_ROOTS {
                return Err(ApiError::new(
                    zeus_api::error::codes::RATE_LIMITED,
                    "service root limit reached",
                )
                .with_detail("limit", "service_roots"));
            }
            let root = StoredRoot {
                path: path.clone(),
                label,
                created_at: now,
            };
            record.roots.push(root.clone());
            Ok((root, true))
        })?;
        Ok((
            zeus_api::methods::admin::ServiceRoot {
                path: root.path.display().to_string(),
                label: root.label,
                client_id: id.to_string(),
                created_at: root.created_at,
            },
            created,
        ))
    }

    /// The active roots of a service credential, oldest first; empty when revoked or unregistered.
    pub fn roots(&self, id: &CredentialId) -> Vec<std::path::PathBuf> {
        self.records
            .read()
            .ok()
            .and_then(|r| {
                r.stored
                    .iter()
                    .find(|s| s.id == id.as_str() && !s.revoked)
                    .map(|s| s.roots.iter().map(|r| r.path.clone()).collect())
            })
            .unwrap_or_default()
    }
}

impl CredentialStore for FileCredentialStore {
    fn get(&self, id: &CredentialId) -> Option<Credential> {
        self.records
            .read()
            .ok()?
            .credentials
            .iter()
            .find(|c| &c.id == id)
            .cloned()
    }
}

/// Create the state directory, the IPC secret and the default credentials if they do not exist yet.
pub fn provision(paths: &ZeusPaths, now: Timestamp) -> Result<ProvisionReport, ProvisionError> {
    let mut report = ProvisionReport::default();
    create_private_dir(&paths.state_dir)?;
    create_private_dir(&paths.credentials_dir())?;
    if let Some(dir) = paths.discovery_file().parent() {
        create_private_dir(dir)?;
    }

    let secret_path = paths.ipc_secret_file();
    if !secret_path.exists() {
        write_private(
            &secret_path,
            IpcSecret::generate().to_file_text().as_bytes(),
        )?;
        report.created_ipc_secret = true;
    }
    zeus_api::transport::secure_private_file(&secret_path).map_err(io("protect IPC secret"))?;

    let store_path = paths.state_dir.join("credentials.json");
    if store_path.exists() {
        zeus_api::transport::secure_private_file(&store_path)
            .map_err(io("protect credential index"))?;
    }
    let mut records: Vec<StoredCredential> = match std::fs::read(&store_path) {
        Ok(bytes) => {
            serde_json::from_slice::<StoreFile>(&bytes)
                .map_err(|e| ProvisionError::Corrupt(format!("credentials.json: {e}")))?
                .credentials
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(source) => {
            return Err(ProvisionError::Io {
                what: "read credentials.json",
                source,
            });
        }
    };

    for (name, kind, role) in DEFAULT_CREDENTIALS {
        let file = paths.credential_file(name);
        if file.exists() {
            zeus_api::transport::secure_private_file(&file)
                .map_err(io("protect existing credential"))?;
            continue;
        }
        let token = Token::generate();
        let id = CredentialId::generate();
        let credential = Credential::new(id.clone(), *kind, *role, token.auth_key(), now)
            .map_err(|e| ProvisionError::Corrupt(e.to_string()))?;
        let mut record = stored_from(&credential);
        record.name = Some((*name).to_string());
        records.push(record);
        let client = ClientCredential { id, token };
        write_private(&file, client.to_file_text().as_bytes())?;
        report.created_credentials.push((*name).to_string());
    }

    if !report.created_credentials.is_empty() {
        let body = serde_json::to_vec_pretty(&StoreFile {
            version: 1,
            credentials: records,
        })
        .map_err(|e| ProvisionError::Corrupt(e.to_string()))?;
        write_private(&store_path, &body)?;
    }
    Ok(report)
}

/// Read the IPC secret file.
pub fn load_ipc_secret(paths: &ZeusPaths) -> Result<IpcSecret, ProvisionError> {
    let text = std::fs::read_to_string(paths.ipc_secret_file()).map_err(io("read ipc.secret"))?;
    IpcSecret::from_file_text(&text).map_err(|e| ProvisionError::Corrupt(e.to_string()))
}

/// Read a client credential file (`cli`, `agent` or `host`).
pub fn load_client_credential(
    paths: &ZeusPaths,
    name: &str,
) -> Result<ClientCredential, ProvisionError> {
    let text =
        std::fs::read_to_string(paths.credential_file(name)).map_err(io("read credential file"))?;
    ClientCredential::from_file_text(&text).map_err(|e| ProvisionError::Corrupt(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use zeus_api::client::{Client, ClientConfig, IpcClient, call};
    use zeus_api::methods::DaemonPing;
    use zeus_api::methods::daemon::{ClientInfo, DaemonInfo, PingParams};
    use zeus_api::server::{ServerContext, serve_connection};

    fn paths(dir: &Path) -> ZeusPaths {
        ZeusPaths {
            state_dir: dir.join("state"),
            config_dir: dir.join("config"),
            runtime_dir: dir.join("run"),
            user_identity: "test".into(),
            profile: None,
        }
    }

    #[test]
    fn provisioning_creates_files_once_and_never_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let first = provision(&paths, Timestamp::now()).unwrap();
        assert!(first.created_ipc_secret);
        assert_eq!(first.created_credentials, ["cli", "agent", "a2a", "host"]);
        let before = std::fs::read(paths.credential_file("agent")).unwrap();
        let secret_before = std::fs::read(paths.ipc_secret_file()).unwrap();

        let second = provision(&paths, Timestamp::now()).unwrap();
        assert_eq!(second, ProvisionReport::default());
        assert_eq!(
            std::fs::read(paths.credential_file("agent")).unwrap(),
            before
        );
        assert_eq!(
            std::fs::read(paths.ipc_secret_file()).unwrap(),
            secret_before
        );
    }

    #[test]
    fn the_store_never_contains_a_token() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        provision(&paths, Timestamp::now()).unwrap();
        let store_text = std::fs::read_to_string(paths.state_dir.join("credentials.json")).unwrap();
        for name in ["cli", "agent", "a2a", "host"] {
            let client = load_client_credential(&paths, name).unwrap();
            let token_hex = &client.token.to_text()[4..];
            assert!(
                !store_text.contains(token_hex),
                "{name}: the token must not be stored"
            );
        }
    }

    #[test]
    fn kinds_follow_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        provision(&paths, Timestamp::now()).unwrap();
        let store = FileCredentialStore::load(&paths.state_dir.join("credentials.json")).unwrap();
        for (name, kind, role) in DEFAULT_CREDENTIALS {
            let credential = store
                .get(&load_client_credential(&paths, name).unwrap().id)
                .unwrap();
            assert_eq!((credential.kind, credential.role), (*kind, *role));
        }
        let agent = store
            .get(&load_client_credential(&paths, "agent").unwrap().id)
            .unwrap();
        assert!(!agent.permissions.contains(&Permission::ApprovalsDecide));
    }

    #[cfg(unix)]
    #[test]
    fn files_are_owner_only_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        provision(&paths, Timestamp::now()).unwrap();
        for file in [
            paths.ipc_secret_file(),
            paths.credential_file("cli"),
            paths.state_dir.join("credentials.json"),
        ] {
            assert_eq!(
                std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            std::fs::metadata(&paths.state_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[tokio::test]
    async fn provisioned_credentials_authenticate_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        provision(&paths, Timestamp::now()).unwrap();
        let secret = Arc::new(load_ipc_secret(&paths).unwrap());
        let store =
            Arc::new(FileCredentialStore::load(&paths.state_dir.join("credentials.json")).unwrap());
        let router = Arc::new(crate::build_router(Arc::new(crate::NotImplementedBackend)));
        let ctx = Arc::new(ServerContext {
            secret: Arc::clone(&secret),
            credentials: store,
            daemon: DaemonInfo {
                version: "0.1.0".into(),
                instance_id: "i".into(),
            },
            router,
        });
        for (name, kind, role) in DEFAULT_CREDENTIALS {
            let (client_end, server_end) = tokio::io::duplex(64 * 1024);
            let ctx = Arc::clone(&ctx);
            tokio::spawn(async move {
                let _ = serve_connection(server_end, ctx).await;
            });
            let credential = load_client_credential(&paths, name).unwrap();
            let mut config = ClientConfig::new(ClientInfo {
                name: "t".into(),
                version: "0".into(),
                role: *role,
            });
            if *role == ClientRole::NativeHost {
                config.attach = Some(zeus_api::methods::daemon::AttachInfo {
                    browser: zeus_types::BrowserName::Chrome,
                    attested_extension: "abcdefghijklmnopabcdefghijklmnop".into(),
                    host_version: "0.1.0".into(),
                    parent_verified: false,
                });
            }
            let client = IpcClient::from_stream(client_end, credential, &secret, config)
                .await
                .unwrap();
            assert_eq!(client.session().kind, *kind, "{name}");
            let _ = call::<DaemonPing, _>(&client, PingParams::default()).await;
        }
    }

    #[tokio::test]
    async fn a_revoked_service_credential_loses_its_live_connection_and_cannot_return() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        provision(&paths, Timestamp::now()).unwrap();
        let secret = Arc::new(load_ipc_secret(&paths).unwrap());
        let store =
            Arc::new(FileCredentialStore::load(&paths.state_dir.join("credentials.json")).unwrap());
        let id = CredentialId::generate();
        let (record, file) = store
            .add(
                &paths.credentials_dir(),
                NewCredential {
                    id: id.clone(),
                    name: "ogun",
                    kind: ClientKind::Service,
                    role: None,
                    permissions: None,
                },
                Timestamp::now(),
            )
            .unwrap();
        assert_eq!(file, paths.credential_file("ogun"));
        assert!(record.permissions.contains(&Permission::FilesRootsRegister));
        // The index survives a reload with the name, and still never holds the token.
        let reloaded =
            FileCredentialStore::load(&paths.state_dir.join("credentials.json")).unwrap();
        assert_eq!(reloaded.record(&id).unwrap().name, "ogun");
        let token = load_client_credential(&paths, "ogun").unwrap();
        let index = std::fs::read_to_string(paths.state_dir.join("credentials.json")).unwrap();
        assert!(!index.contains(&token.token.to_text()[4..]));
        let router = Arc::new(
            crate::build_router(Arc::new(crate::NotImplementedBackend))
                .with_revocations(store.revocations()),
        );
        let ctx = Arc::new(ServerContext {
            secret: Arc::clone(&secret),
            credentials: store.clone(),
            daemon: DaemonInfo {
                version: "0.1.0".into(),
                instance_id: "i".into(),
            },
            router,
        });
        let connect = |ctx: Arc<ServerContext>| {
            let secret = secret.clone();
            let credential = load_client_credential(&paths, "ogun");
            async move {
                let (client_end, server_end) = tokio::io::duplex(64 * 1024);
                let serving = tokio::spawn(serve_connection(server_end, ctx));
                let client = IpcClient::from_stream(
                    client_end,
                    credential?,
                    &secret,
                    ClientConfig::new(ClientInfo {
                        name: "ogund".into(),
                        version: "0".into(),
                        role: ClientRole::Service,
                    }),
                )
                .await
                .map_err(|e| ProvisionError::Corrupt(e.to_string()))?;
                Ok::<_, ProvisionError>((client, serving))
            }
        };
        let (client, serving) = connect(ctx.clone()).await.unwrap();
        assert_eq!(client.session().kind, ClientKind::Service);
        call::<DaemonPing, _>(&client, PingParams::default())
            .await
            .unwrap();
        store
            .revoke(&paths.credentials_dir(), &id, Timestamp::now())
            .unwrap();
        // The idle connection is closed by the revocation signal, without another request.
        tokio::time::timeout(std::time::Duration::from_secs(5), serving)
            .await
            .expect("the revoked connection closes")
            .unwrap()
            .unwrap();
        assert!(
            call::<DaemonPing, _>(&client, PingParams::default())
                .await
                .is_err()
        );
        assert!(!paths.credential_file("ogun").exists());
        assert!(store.get(&id).unwrap().revoked);
    }
}
