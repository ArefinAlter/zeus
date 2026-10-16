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

/// Credential store backed by `<state>/credentials.json`.
pub struct FileCredentialStore {
    credentials: RwLock<Vec<Credential>>,
}

impl FileCredentialStore {
    /// Load the store. A missing file is an empty store.
    pub fn load(path: &Path) -> Result<Self, ProvisionError> {
        let credentials = match std::fs::read(path) {
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
                    .iter()
                    .map(credential_from)
                    .collect::<Result<Vec<_>, _>>()?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(source) => {
                return Err(ProvisionError::Io {
                    what: "read credentials.json",
                    source,
                });
            }
        };
        Ok(Self {
            credentials: RwLock::new(credentials),
        })
    }
}

impl CredentialStore for FileCredentialStore {
    fn get(&self, id: &CredentialId) -> Option<Credential> {
        self.credentials
            .read()
            .ok()?
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
        records.push(stored_from(&credential));
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
}
