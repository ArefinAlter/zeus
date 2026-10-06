//! Credentials and the mutual challenge-response handshake (ADR 0002).
//!
//! ```text
//! client                                         daemon
//!   daemon.hello {versions, caps, client, client_nonce}  ->
//!                <-  {version, caps, limits, server_nonce, server_proof}
//!   verify server_proof = BLAKE3-keyed(ipc_secret, "zeus-server-v1" || client_nonce)
//!   daemon.authenticate {credential_id, proof, attach?}  ->
//!                <-  {kind, role, permissions, limits}
//! ```
//!
//! * `ipc_secret` is a 32-byte random value in a `0600` file only the user can read. A process that squatted the
//!   endpoint cannot produce `server_proof`, so the client never sends it anything.
//! * The **token** never crosses the wire. Client and daemon derive `auth_key = BLAKE3-derive-key(context, token)`;
//!   `proof = BLAKE3-keyed(auth_key, "zeus-client-v1" || credential_id || server_nonce || client_nonce)`. Both
//!   nonces are fresh per connection, so a recorded proof cannot be replayed.
//! * All comparisons are constant-time (`blake3::Hash` equality).
//! * A credential fixes the client's kind, role and permissions; none of them is claimed by the client.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Mutex;

use blake3::Hash;
use zeroize::Zeroizing;
use zeus_types::{IdGenerator, Timestamp};

use crate::capabilities;
use crate::error::{ApiError, codes};
use crate::kind::{ClientIdentity, ClientKind, ClientRole, Permission};
use crate::methods::daemon::{
    AttachInfo, AuthenticateParams, AuthenticateResult, ClientInfo, ConnectionLimits, DaemonInfo,
    HelloParams, HelloResult,
};

const TOKEN_PREFIX: &str = "zt1_";
const AUTH_KEY_CONTEXT: &str = "zeus 2026-10 client auth key v1";
const SERVER_LABEL: &[u8] = b"zeus-server-v1";
const CLIENT_LABEL: &[u8] = b"zeus-client-v1";

/// Identifier of a client credential: `cli_` followed by a 26-character Crockford ULID (00 section 5.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CredentialId(String);

impl CredentialId {
    /// Validate an id.
    pub fn parse(text: &str) -> Result<Self, ApiError> {
        let valid = text.strip_prefix("cli_").is_some_and(|ulid| {
            ulid.len() == 26
                && ulid.starts_with(|c: char| ('0'..='7').contains(&c))
                && ulid
                    .bytes()
                    .all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b))
        });
        if valid {
            Ok(Self(text.to_string()))
        } else {
            Err(ApiError::new(
                codes::BAD_CREDENTIAL,
                "malformed credential id",
            ))
        }
    }

    /// Mint a new id. Only the daemon (or the installer) does this.
    pub fn generate() -> Self {
        Self(format!("cli_{}", ulid::Ulid::generate()))
    }

    /// The text form.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CredentialId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn random_32() -> Zeroizing<[u8; 32]> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    rand::fill(&mut bytes[..]);
    bytes
}

fn parse_hex_32(text: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(text).ok()?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

/// A 32-byte random nonce in hex, fresh per connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nonce(String);

impl Nonce {
    /// A fresh nonce.
    pub fn generate() -> Self {
        Self(hex::encode(random_32().as_slice()))
    }

    /// Validate nonce text received from a peer: 64 lowercase hex digits.
    pub fn parse(text: &str) -> Result<Self, ApiError> {
        if text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            Ok(Self(text.to_string()))
        } else {
            Err(ApiError::invalid_params(
                "nonce must be 64 lowercase hex digits",
            ))
        }
    }

    /// The hex text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The secret that proves a server is the user's daemon. Lives in a user-only file; never leaves the machine.
pub struct IpcSecret(Zeroizing<[u8; 32]>);

impl fmt::Debug for IpcSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IpcSecret([REDACTED])")
    }
}

impl IpcSecret {
    /// A fresh secret (written once by the installer or the daemon on first start).
    pub fn generate() -> Self {
        Self(random_32())
    }

    /// Parse the file form: 64 hex digits, surrounding whitespace allowed.
    pub fn from_file_text(text: &str) -> Result<Self, ApiError> {
        parse_hex_32(text.trim())
            .map(|bytes| Self(Zeroizing::new(bytes)))
            .ok_or_else(|| {
                ApiError::new(codes::SERVER_PROOF_INVALID, "ipc secret file is malformed")
            })
    }

    /// The file form.
    pub fn to_file_text(&self) -> Zeroizing<String> {
        Zeroizing::new(format!("{}\n", hex::encode(self.0.as_slice())))
    }

    /// `server_proof` for a client nonce.
    pub fn server_proof(&self, client_nonce: &Nonce) -> String {
        let mut hasher = blake3::Hasher::new_keyed(&self.0);
        hasher.update(SERVER_LABEL);
        hasher.update(client_nonce.as_str().as_bytes());
        hasher.finalize().to_hex().to_string()
    }

    /// Verify a `server_proof` in constant time.
    pub fn verify_server_proof(&self, client_nonce: &Nonce, proof: &str) -> bool {
        match Hash::from_hex(proof) {
            Ok(given) => {
                given
                    == Hash::from_hex(self.server_proof(client_nonce))
                        .unwrap_or(Hash::from_bytes([0; 32]))
            }
            Err(_) => false,
        }
    }
}

/// The bearer secret a client holds, `zt1_` plus 64 hex digits. Zeroized on drop; never printed.
pub struct Token(Zeroizing<[u8; 32]>);

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token([REDACTED])")
    }
}

impl Token {
    /// A fresh token.
    pub fn generate() -> Self {
        Self(random_32())
    }

    /// Parse `zt1_<64 hex>`.
    pub fn parse(text: &str) -> Result<Self, ApiError> {
        text.trim()
            .strip_prefix(TOKEN_PREFIX)
            .and_then(parse_hex_32)
            .map(|bytes| Self(Zeroizing::new(bytes)))
            .ok_or_else(|| ApiError::new(codes::BAD_CREDENTIAL, "malformed token"))
    }

    /// The text form.
    pub fn to_text(&self) -> Zeroizing<String> {
        Zeroizing::new(format!("{TOKEN_PREFIX}{}", hex::encode(self.0.as_slice())))
    }

    /// The key both sides derive from the token. The daemon stores this, not the token.
    pub fn auth_key(&self) -> AuthKey {
        AuthKey(Zeroizing::new(blake3::derive_key(
            AUTH_KEY_CONTEXT,
            self.0.as_slice(),
        )))
    }
}

/// Key derived from a [`Token`]; the daemon's stored verifier for a credential.
#[derive(Clone)]
pub struct AuthKey(Zeroizing<[u8; 32]>);

impl fmt::Debug for AuthKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthKey([REDACTED])")
    }
}

impl AuthKey {
    /// Rebuild from stored bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// The raw bytes, for storage by the daemon (owner-only file or journal table).
    pub fn to_bytes(&self) -> Zeroizing<[u8; 32]> {
        self.0.clone()
    }

    /// Proof for one connection.
    pub fn proof(
        &self,
        credential_id: &CredentialId,
        server_nonce: &Nonce,
        client_nonce: &Nonce,
    ) -> String {
        let mut hasher = blake3::Hasher::new_keyed(&self.0);
        hasher.update(CLIENT_LABEL);
        hasher.update(credential_id.as_str().as_bytes());
        hasher.update(server_nonce.as_str().as_bytes());
        hasher.update(client_nonce.as_str().as_bytes());
        hasher.finalize().to_hex().to_string()
    }

    fn verify(
        &self,
        credential_id: &CredentialId,
        server_nonce: &Nonce,
        client_nonce: &Nonce,
        proof: &str,
    ) -> bool {
        let expected = Hash::from_hex(self.proof(credential_id, server_nonce, client_nonce))
            .unwrap_or(Hash::from_bytes([0; 32]));
        match Hash::from_hex(proof) {
            Ok(given) => given == expected,
            Err(_) => false,
        }
    }
}

/// A credential as the daemon stores it.
#[derive(Debug, Clone)]
pub struct Credential {
    /// Identifier.
    pub id: CredentialId,
    /// Authority class.
    pub kind: ClientKind,
    /// Role.
    pub role: ClientRole,
    /// Effective permissions.
    pub permissions: BTreeSet<Permission>,
    /// Verifier derived from the token.
    pub auth_key: AuthKey,
    /// Creation time.
    pub created_at: Timestamp,
    /// Revoked credentials fail authentication.
    pub revoked: bool,
}

impl Credential {
    /// Create a credential with the default permissions of its kind (plus `nm.relay` for the native host).
    ///
    /// Fails when the kind exceeds what the role allows (an MCP credential can never be human or admin).
    pub fn new(
        id: CredentialId,
        kind: ClientKind,
        role: ClientRole,
        auth_key: AuthKey,
        now: Timestamp,
    ) -> Result<Self, ApiError> {
        if kind > role.max_kind() {
            return Err(ApiError::new(
                codes::PERMISSION_DENIED,
                "the role cannot hold that kind",
            ));
        }
        let mut permissions = kind.default_permissions();
        if role == ClientRole::NativeHost {
            permissions.insert(Permission::NmRelay);
        }
        Ok(Self {
            id,
            kind,
            role,
            permissions,
            auth_key,
            created_at: now,
            revoked: false,
        })
    }

    /// Narrow the permissions. A permission outside the kind's defaults (and `nm.relay` for the native host) is
    /// refused: credentials can shrink, never grow.
    pub fn narrowed(mut self, permissions: BTreeSet<Permission>) -> Result<Self, ApiError> {
        let mut ceiling = self.kind.default_permissions();
        if self.role == ClientRole::NativeHost {
            ceiling.insert(Permission::NmRelay);
        }
        if !permissions.is_subset(&ceiling) {
            return Err(ApiError::new(
                codes::PERMISSION_DENIED,
                "a credential cannot gain permissions",
            ));
        }
        self.permissions = permissions;
        Ok(self)
    }
}

/// Lookup of stored credentials.
pub trait CredentialStore: Send + Sync {
    /// The credential with this id, revoked or not.
    fn get(&self, id: &CredentialId) -> Option<Credential>;
}

/// In-memory store for tests and embedding.
#[derive(Default)]
pub struct MemoryCredentialStore {
    credentials: Mutex<BTreeMap<CredentialId, Credential>>,
}

impl MemoryCredentialStore {
    /// Add or replace a credential.
    pub fn insert(&self, credential: Credential) {
        if let Ok(mut map) = self.credentials.lock() {
            map.insert(credential.id.clone(), credential);
        }
    }

    /// Mark a credential revoked.
    pub fn revoke(&self, id: &CredentialId) {
        if let Ok(mut map) = self.credentials.lock()
            && let Some(c) = map.get_mut(id)
        {
            c.revoked = true;
        }
    }
}

impl CredentialStore for MemoryCredentialStore {
    fn get(&self, id: &CredentialId) -> Option<Credential> {
        self.credentials
            .lock()
            .ok()
            .and_then(|map| map.get(id).cloned())
    }
}

/// A credential as a client holds it (the file `<state>/credentials/<name>.token`): id and token, one line.
pub struct ClientCredential {
    /// Id.
    pub id: CredentialId,
    /// Secret.
    pub token: Token,
}

impl fmt::Debug for ClientCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ClientCredential({}, [REDACTED])", self.id)
    }
}

impl ClientCredential {
    /// The file form: `<credential_id> <token>` and a newline.
    pub fn to_file_text(&self) -> Zeroizing<String> {
        Zeroizing::new(format!("{} {}\n", self.id, self.token.to_text().as_str()))
    }

    /// Parse the file form.
    pub fn from_file_text(text: &str) -> Result<Self, ApiError> {
        let mut parts = text.split_whitespace();
        let (Some(id), Some(token), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(ApiError::new(
                codes::BAD_CREDENTIAL,
                "credential file is malformed",
            ));
        };
        Ok(Self {
            id: CredentialId::parse(id)?,
            token: Token::parse(token)?,
        })
    }
}

/// Server side of the handshake for one connection.
pub struct ServerHandshake<'a> {
    secret: &'a IpcSecret,
    store: &'a dyn CredentialStore,
    daemon: DaemonInfo,
    ids: IdGenerator,
    state: HandshakeState,
}

enum HandshakeState {
    AwaitHello,
    AwaitAuthenticate {
        client_nonce: Nonce,
        server_nonce: Nonce,
        client: ClientInfo,
    },
    Done(ClientIdentity),
}

impl<'a> ServerHandshake<'a> {
    /// Start a handshake for a new connection.
    pub fn new(secret: &'a IpcSecret, store: &'a dyn CredentialStore, daemon: DaemonInfo) -> Self {
        Self {
            secret,
            store,
            daemon,
            ids: IdGenerator::new(),
            state: HandshakeState::AwaitHello,
        }
    }

    /// The authenticated identity once the handshake completed.
    pub fn identity(&self) -> Option<&ClientIdentity> {
        match &self.state {
            HandshakeState::Done(identity) => Some(identity),
            _ => None,
        }
    }

    /// Handle `daemon.hello`: choose the version, intersect capabilities, prove possession of the IPC secret.
    pub fn on_hello(&mut self, params: HelloParams) -> Result<HelloResult, ApiError> {
        if !matches!(self.state, HandshakeState::AwaitHello) {
            return Err(ApiError::new(
                codes::ALREADY_AUTHENTICATED,
                "daemon.hello was already completed",
            ));
        }
        let version = params
            .protocol_versions
            .iter()
            .copied()
            .filter(|v| *v == crate::PROTOCOL_VERSION)
            .max()
            .ok_or_else(|| {
                ApiError::new(codes::VERSION_UNSUPPORTED, "no protocol version in common")
                    .with_detail("supported", serde_json::json!([crate::PROTOCOL_VERSION]))
            })?;
        let client_nonce = Nonce::parse(&params.client_nonce)?;
        let capabilities: BTreeSet<String> = params
            .capabilities
            .into_iter()
            .filter(|c| capabilities::ALL.contains(&c.as_str()))
            .collect();
        let server_nonce = Nonce::generate();
        let result = HelloResult {
            protocol_version: version,
            capabilities,
            limits: ConnectionLimits::STANDARD,
            daemon: self.daemon.clone(),
            server_nonce: server_nonce.as_str().to_string(),
            server_proof: self.secret.server_proof(&client_nonce),
        };
        self.state = HandshakeState::AwaitAuthenticate {
            client_nonce,
            server_nonce,
            client: params.client,
        };
        Ok(result)
    }

    /// Handle `daemon.authenticate`. Every failure is `api.bad_credential` (or a missing-attach error) so the
    /// answer is not an oracle for which credential ids exist.
    pub fn on_authenticate(
        &mut self,
        params: AuthenticateParams,
    ) -> Result<AuthenticateResult, ApiError> {
        let (client_nonce, server_nonce, client) = match &self.state {
            HandshakeState::AwaitAuthenticate {
                client_nonce,
                server_nonce,
                client,
            } => (client_nonce.clone(), server_nonce.clone(), client.clone()),
            HandshakeState::AwaitHello => return Err(ApiError::not_authenticated()),
            HandshakeState::Done(_) => {
                return Err(ApiError::new(
                    codes::ALREADY_AUTHENTICATED,
                    "already authenticated",
                ));
            }
        };
        let bad = || ApiError::new(codes::BAD_CREDENTIAL, "authentication failed");
        let id = CredentialId::parse(&params.credential_id).map_err(|_| bad())?;
        // Always do the same work, whether or not the credential exists.
        let stored = self.store.get(&id);
        let dummy = AuthKey::from_bytes([0; 32]);
        let key = stored.as_ref().map_or(&dummy, |c| &c.auth_key);
        let proof_ok = key.verify(&id, &server_nonce, &client_nonce, &params.proof);
        let credential = match stored {
            Some(c) if proof_ok && !c.revoked && c.role == client.role => c,
            _ => return Err(bad()),
        };
        match (&params.attach, credential.role) {
            (Some(attach), ClientRole::NativeHost) => validate_attach(attach)?,
            (None, ClientRole::NativeHost) => {
                return Err(ApiError::new(
                    codes::BAD_CREDENTIAL,
                    "the native host must report how it was launched",
                ));
            }
            (Some(_), _) => {
                return Err(ApiError::invalid_params(
                    "attach is only for the native host",
                ));
            }
            (None, _) => {}
        }
        let limits = ConnectionLimits::for_role(credential.role);
        let identity = ClientIdentity {
            credential_id: credential.id.clone(),
            kind: credential.kind,
            role: credential.role,
            permissions: credential.permissions.clone(),
            client_name: format!("{} {}", client.name, client.version)
                .chars()
                .take(128)
                .collect(),
        };
        let result = AuthenticateResult {
            client_id: credential.id.to_string(),
            kind: credential.kind,
            role: credential.role,
            permissions: credential.permissions.iter().copied().collect(),
            limits,
            connection_id: self.ids.connection_id(),
        };
        self.state = HandshakeState::Done(identity);
        Ok(result)
    }
}

fn validate_attach(attach: &AttachInfo) -> Result<(), ApiError> {
    if attach.attested_extension.is_empty()
        || attach.attested_extension.len() > 128
        || attach.host_version.len() > 64
    {
        return Err(ApiError::invalid_params("attach fields are out of range"));
    }
    Ok(())
}

/// Client side of the handshake.
pub struct ClientHandshake {
    credential: ClientCredential,
    client: ClientInfo,
    client_nonce: Nonce,
    protocol_versions: Vec<u16>,
}

impl ClientHandshake {
    /// Prepare a handshake with a fresh nonce.
    pub fn new(credential: ClientCredential, client: ClientInfo) -> Self {
        Self {
            credential,
            client,
            client_nonce: Nonce::generate(),
            protocol_versions: vec![crate::PROTOCOL_VERSION],
        }
    }

    /// The `daemon.hello` parameters.
    pub fn hello_params(&self) -> HelloParams {
        HelloParams {
            protocol_versions: self.protocol_versions.clone(),
            capabilities: capabilities::ALL.iter().map(|c| (*c).to_string()).collect(),
            client: self.client.clone(),
            client_nonce: self.client_nonce.as_str().to_string(),
        }
    }

    /// Verify the daemon's proof and build the `daemon.authenticate` parameters. Refuses (and sends nothing
    /// derived from the token) when the server cannot prove it holds the IPC secret.
    pub fn authenticate_params(
        &self,
        ipc_secret: &IpcSecret,
        hello: &HelloResult,
        attach: Option<AttachInfo>,
    ) -> Result<AuthenticateParams, ApiError> {
        if !ipc_secret.verify_server_proof(&self.client_nonce, &hello.server_proof) {
            return Err(ApiError::new(
                codes::SERVER_PROOF_INVALID,
                "the endpoint did not prove it is the Zeus daemon of this user; refusing to send credentials",
            ));
        }
        let server_nonce = Nonce::parse(&hello.server_nonce)?;
        let proof = self.credential.token.auth_key().proof(
            &self.credential.id,
            &server_nonce,
            &self.client_nonce,
        );
        Ok(AuthenticateParams {
            credential_id: self.credential.id.to_string(),
            proof,
            attach,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeus_types::BrowserName;

    struct Fixture {
        secret: IpcSecret,
        store: MemoryCredentialStore,
        credential: ClientCredential,
        role: ClientRole,
    }

    fn fixture(kind: ClientKind, role: ClientRole) -> Fixture {
        let token = Token::generate();
        let id = CredentialId::generate();
        let store = MemoryCredentialStore::default();
        store.insert(
            Credential::new(id.clone(), kind, role, token.auth_key(), Timestamp::now()).unwrap(),
        );
        Fixture {
            secret: IpcSecret::generate(),
            store,
            credential: ClientCredential { id, token },
            role,
        }
    }

    fn daemon() -> DaemonInfo {
        DaemonInfo {
            version: "0.1.0".into(),
            instance_id: "3f9a6c2d1b7e4a08c5d2e9f1a0b3c4d5".into(),
        }
    }

    fn client(role: ClientRole) -> ClientInfo {
        ClientInfo {
            name: "test".into(),
            version: "0".into(),
            role,
        }
    }

    fn rebuild(credential: &ClientCredential) -> ClientCredential {
        ClientCredential {
            id: credential.id.clone(),
            token: Token::parse(&credential.token.to_text()).unwrap(),
        }
    }

    fn full_handshake(
        f: &Fixture,
        attach: Option<AttachInfo>,
    ) -> Result<(ClientIdentity, AuthenticateResult), ApiError> {
        let client_side = ClientHandshake::new(rebuild(&f.credential), client(f.role));
        let mut server = ServerHandshake::new(&f.secret, &f.store, daemon());
        let hello = server.on_hello(client_side.hello_params())?;
        let auth = client_side.authenticate_params(&f.secret, &hello, attach)?;
        let result = server.on_authenticate(auth)?;
        Ok((server.identity().unwrap().clone(), result))
    }

    #[test]
    fn handshake_succeeds_and_binds_kind_and_permissions_to_the_credential() {
        let f = fixture(ClientKind::Agent, ClientRole::Mcp);
        let (identity, result) = full_handshake(&f, None).unwrap();
        assert_eq!(identity.kind, ClientKind::Agent);
        assert!(!identity.has(Permission::ApprovalsDecide));
        assert_eq!(result.kind, ClientKind::Agent);
        assert_eq!(result.limits, ConnectionLimits::STANDARD);
    }

    #[test]
    fn the_client_refuses_a_server_that_does_not_know_the_ipc_secret() {
        let f = fixture(ClientKind::Human, ClientRole::Cli);
        let squatter_secret = IpcSecret::generate();
        let client_side = ClientHandshake::new(rebuild(&f.credential), client(f.role));
        let mut squatter = ServerHandshake::new(&squatter_secret, &f.store, daemon());
        let hello = squatter.on_hello(client_side.hello_params()).unwrap();
        let error = client_side
            .authenticate_params(&f.secret, &hello, None)
            .unwrap_err();
        assert_eq!(error.envelope().code.as_str(), codes::SERVER_PROOF_INVALID);
    }

    #[test]
    fn wrong_token_unknown_id_revoked_and_role_mismatch_all_fail_the_same_way() {
        let f = fixture(ClientKind::Human, ClientRole::Cli);
        let attempt = |credential: ClientCredential, role: ClientRole| {
            let client_side = ClientHandshake::new(credential, client(role));
            let mut server = ServerHandshake::new(&f.secret, &f.store, daemon());
            let hello = server.on_hello(client_side.hello_params()).unwrap();
            let auth = client_side
                .authenticate_params(&f.secret, &hello, None)
                .unwrap();
            server.on_authenticate(auth).unwrap_err()
        };
        let wrong_token = attempt(
            ClientCredential {
                id: f.credential.id.clone(),
                token: Token::generate(),
            },
            ClientRole::Cli,
        );
        let unknown_id = attempt(
            ClientCredential {
                id: CredentialId::generate(),
                token: Token::generate(),
            },
            ClientRole::Cli,
        );
        let wrong_role = attempt(rebuild(&f.credential), ClientRole::Mcp);
        f.store.revoke(&f.credential.id);
        let revoked = attempt(rebuild(&f.credential), ClientRole::Cli);
        for error in [wrong_token, unknown_id, wrong_role, revoked] {
            assert_eq!(error.envelope().code.as_str(), codes::BAD_CREDENTIAL);
            assert_eq!(error.envelope().message, "authentication failed");
        }
    }

    #[test]
    fn a_recorded_proof_cannot_be_replayed_on_another_connection() {
        let f = fixture(ClientKind::Human, ClientRole::Cli);
        let client_side = ClientHandshake::new(rebuild(&f.credential), client(f.role));
        let mut first = ServerHandshake::new(&f.secret, &f.store, daemon());
        let hello = first.on_hello(client_side.hello_params()).unwrap();
        let auth = client_side
            .authenticate_params(&f.secret, &hello, None)
            .unwrap();
        first.on_authenticate(auth.clone()).unwrap();

        let mut second = ServerHandshake::new(&f.secret, &f.store, daemon());
        second.on_hello(client_side.hello_params()).unwrap();
        let error = second.on_authenticate(auth).unwrap_err();
        assert_eq!(error.envelope().code.as_str(), codes::BAD_CREDENTIAL);
    }

    #[test]
    fn methods_before_hello_and_double_hello_are_rejected() {
        let f = fixture(ClientKind::Human, ClientRole::Cli);
        let mut server = ServerHandshake::new(&f.secret, &f.store, daemon());
        let error = server
            .on_authenticate(AuthenticateParams {
                credential_id: f.credential.id.to_string(),
                proof: "00".repeat(32),
                attach: None,
            })
            .unwrap_err();
        assert_eq!(error.envelope().code.as_str(), codes::NOT_AUTHENTICATED);
        let client_side = ClientHandshake::new(rebuild(&f.credential), client(f.role));
        server.on_hello(client_side.hello_params()).unwrap();
        assert_eq!(
            server
                .on_hello(client_side.hello_params())
                .unwrap_err()
                .envelope()
                .code
                .as_str(),
            codes::ALREADY_AUTHENTICATED
        );
    }

    #[test]
    fn version_negotiation_picks_the_highest_common_or_fails_with_supported() {
        let f = fixture(ClientKind::Human, ClientRole::Cli);
        let client_side = ClientHandshake::new(rebuild(&f.credential), client(f.role));
        let mut server = ServerHandshake::new(&f.secret, &f.store, daemon());
        let mut params = client_side.hello_params();
        params.protocol_versions = vec![1, 7];
        assert_eq!(server.on_hello(params).unwrap().protocol_version, 1);

        let mut server = ServerHandshake::new(&f.secret, &f.store, daemon());
        let mut params = client_side.hello_params();
        params.protocol_versions = vec![2, 3];
        let error = server.on_hello(params).unwrap_err();
        assert_eq!(error.envelope().code.as_str(), codes::VERSION_UNSUPPORTED);
        assert_eq!(
            error.envelope().details.as_ref().unwrap()["supported"],
            serde_json::json!([1])
        );
    }

    #[test]
    fn capabilities_are_intersected() {
        let f = fixture(ClientKind::Human, ClientRole::Cli);
        let client_side = ClientHandshake::new(rebuild(&f.credential), client(f.role));
        let mut server = ServerHandshake::new(&f.secret, &f.store, daemon());
        let mut params = client_side.hello_params();
        params.capabilities = ["events", "something.new"]
            .into_iter()
            .map(String::from)
            .collect();
        let result = server.on_hello(params).unwrap();
        assert_eq!(
            result.capabilities.into_iter().collect::<Vec<_>>(),
            ["events"]
        );
    }

    #[test]
    fn the_native_host_must_attach_and_others_must_not() {
        let host = fixture(ClientKind::Human, ClientRole::NativeHost);
        assert!(full_handshake(&host, None).is_err());
        let attach = AttachInfo {
            browser: BrowserName::Chrome,
            attested_extension: "abcdefghijklmnopabcdefghijklmnop".into(),
            host_version: "0.1.0".into(),
        };
        let (identity, result) = full_handshake(&host, Some(attach.clone())).unwrap();
        assert!(identity.has(Permission::NmRelay));
        assert_eq!(result.limits, ConnectionLimits::NATIVE_HOST);

        let cli = fixture(ClientKind::Human, ClientRole::Cli);
        assert!(full_handshake(&cli, Some(attach)).is_err());
    }

    #[test]
    fn credentials_cannot_exceed_their_role_or_gain_permissions() {
        let key = Token::generate().auth_key();
        assert!(
            Credential::new(
                CredentialId::generate(),
                ClientKind::Human,
                ClientRole::Mcp,
                key.clone(),
                Timestamp::now()
            )
            .is_err()
        );
        assert!(
            Credential::new(
                CredentialId::generate(),
                ClientKind::Admin,
                ClientRole::Tui,
                key.clone(),
                Timestamp::now()
            )
            .is_err()
        );
        let agent = Credential::new(
            CredentialId::generate(),
            ClientKind::Agent,
            ClientRole::Mcp,
            key,
            Timestamp::now(),
        )
        .unwrap();
        let widened: BTreeSet<Permission> = [Permission::ApprovalsDecide].into_iter().collect();
        assert!(agent.clone().narrowed(widened).is_err());
        let narrowed: BTreeSet<Permission> = [Permission::JobsRead].into_iter().collect();
        assert_eq!(agent.narrowed(narrowed).unwrap().permissions.len(), 1);
    }

    #[test]
    fn secrets_never_appear_in_debug_output_and_files_round_trip() {
        let token = Token::generate();
        let text = token.to_text();
        assert!(!format!("{token:?}").contains(&text[4..]));
        let credential = ClientCredential {
            id: CredentialId::generate(),
            token,
        };
        assert!(!format!("{credential:?}").contains("zt1_"));
        let parsed = ClientCredential::from_file_text(&credential.to_file_text()).unwrap();
        assert_eq!(parsed.id, credential.id);
        let secret = IpcSecret::generate();
        let again = IpcSecret::from_file_text(&secret.to_file_text()).unwrap();
        let nonce = Nonce::generate();
        assert_eq!(secret.server_proof(&nonce), again.server_proof(&nonce));
        assert!(!format!("{secret:?}").contains(&hex::encode([0u8; 4])));
    }

    #[test]
    fn credential_ids_and_nonces_are_validated() {
        assert!(CredentialId::parse("cli_01M45NHJ2AHWMXT8JVJ7BEEJX7").is_ok());
        assert!(CredentialId::parse("job_01M45NHJ2AHWMXT8JVJ7BEEJX7").is_err());
        assert!(CredentialId::parse(CredentialId::generate().as_str()).is_ok());
        assert!(Nonce::parse(&"a".repeat(64)).is_ok());
        assert!(Nonce::parse(&"A".repeat(64)).is_err());
        assert!(Nonce::parse("abc").is_err());
    }
}
