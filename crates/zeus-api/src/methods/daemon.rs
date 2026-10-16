//! `daemon.*`: negotiation, authentication, status, shutdown and the kill switch.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeus_types::{BrowserName, ConnectionId, JobState, Timestamp};

use crate::kind::{ClientKind, ClientRole, Permission};

/// Who is connecting. Self-declared and untrusted: used for logs and display only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientInfo {
    /// Program name, for example `zeus-cli`.
    pub name: String,
    /// Program version.
    pub version: String,
    /// The role the client claims. Must equal the role of the credential it later presents.
    pub role: ClientRole,
}

/// Hard maxima for one connection (00 section 10 plus API specifics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConnectionLimits {
    /// Longest request line the server accepts.
    pub max_request_bytes: u64,
    /// Longest response or notification line the server sends.
    pub max_response_bytes: u64,
    /// Requests being processed at once on this connection.
    pub max_in_flight_requests: u32,
    /// Event subscriptions on this connection.
    pub max_subscriptions: u32,
    /// Events buffered per subscription before `events.lagged`.
    pub event_buffer: u32,
}

impl ConnectionLimits {
    /// Limits of an ordinary connection.
    pub const STANDARD: Self = Self {
        max_request_bytes: 1024 * 1024,
        max_response_bytes: 16 * 1024 * 1024,
        max_in_flight_requests: 32,
        max_subscriptions: 4,
        event_buffer: 1024,
    };

    /// Limits of a `native_host` connection (5 MiB lines, ADR 0002).
    pub const NATIVE_HOST: Self = Self {
        max_request_bytes: 5 * 1024 * 1024,
        ..Self::STANDARD
    };

    /// The limits for a role.
    pub const fn for_role(role: ClientRole) -> Self {
        match role {
            ClientRole::NativeHost => Self::NATIVE_HOST,
            _ => Self::STANDARD,
        }
    }
}

/// The daemon's identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonInfo {
    /// Zeus version.
    pub version: String,
    /// Random id of this daemon run (changes on restart).
    pub instance_id: String,
}

/// `daemon.hello` parameters. Sent with the lowest protocol version so any future daemon can parse it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HelloParams {
    /// Protocol versions the client speaks.
    pub protocol_versions: Vec<u16>,
    /// Optional behaviours the client implements.
    #[serde(default)]
    pub capabilities: BTreeSet<String>,
    /// Who is connecting.
    pub client: ClientInfo,
    /// 32 random bytes as 64 lowercase hex digits, fresh per connection.
    pub client_nonce: String,
}

/// `daemon.hello` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HelloResult {
    /// The chosen protocol version (highest in common).
    pub protocol_version: u16,
    /// Intersection of both capability sets.
    pub capabilities: BTreeSet<String>,
    /// Limits that apply before authentication.
    pub limits: ConnectionLimits,
    /// The daemon.
    pub daemon: DaemonInfo,
    /// 32 random bytes as hex, fresh per connection.
    pub server_nonce: String,
    /// `BLAKE3-keyed(ipc_secret, "zeus-server-v1" || client_nonce)` in hex. The client verifies it before sending
    /// any proof, so a process that squatted the endpoint cannot collect credentials.
    pub server_proof: String,
}

/// What the native host reports about how it was launched (spec 02 section 3.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttachInfo {
    /// Which browser launched the host.
    pub browser: BrowserName,
    /// The extension id taken from the host's `argv` and found in its allow-list. The only authoritative identity.
    pub attested_extension: String,
    /// Version of the host.
    pub host_version: String,
    /// Browser-parent attestation; false until the host has verified the parent.
    #[serde(default)]
    pub parent_verified: bool,
}

/// `daemon.authenticate` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuthenticateParams {
    /// The credential id (`cli_...`).
    pub credential_id: String,
    /// `BLAKE3-keyed(auth_key, "zeus-client-v1" || credential_id || server_nonce || client_nonce)` in hex.
    pub proof: String,
    /// Required for role `native_host`, forbidden otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach: Option<AttachInfo>,
}

/// `daemon.authenticate` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuthenticateResult {
    /// Authority on the stored credential, before executable verification.
    pub credential_kind: ClientKind,
    /// The daemon reduced human/admin authority because peer verification failed.
    pub downgraded: bool,
    /// The credential id, echoed.
    pub client_id: String,
    /// Authority class of the credential.
    pub kind: ClientKind,
    /// Role of the credential.
    pub role: ClientRole,
    /// Effective permissions.
    pub permissions: Vec<Permission>,
    /// Limits that now apply.
    pub limits: ConnectionLimits,
    /// Connection id for logs.
    pub connection_id: ConnectionId,
}

/// `daemon.ping` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PingParams {}

/// `daemon.ping` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PingResult {
    /// Daemon wall-clock time.
    pub server_time: Timestamp,
}

/// `daemon.status` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatusParams {}

/// `daemon.status` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatusResult {
    /// The daemon.
    pub daemon: DaemonInfo,
    /// Protocol version of this connection.
    pub protocol_version: u16,
    /// When this run started.
    pub started_at: Timestamp,
    /// Milliseconds since start (monotonic).
    pub uptime_ms: u64,
    /// Newest durable event sequence (0 when none).
    pub head_seq: u64,
    /// Job counts per state.
    pub jobs: BTreeMap<JobState, u32>,
    /// Approvals waiting for a human.
    pub pending_approvals: u32,
    /// Whether a browser extension is connected.
    pub extension_connected: bool,
    /// Whether the kill switch is currently engaged (grants revoked, jobs paused).
    pub kill_switch_engaged: bool,
    /// Journal opened and writable.
    pub journal_ok: bool,
}

/// `daemon.shutdown` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ShutdownParams {
    /// Time to let transfers reach a commit point; default 10 000, at most 60 000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_ms: Option<u64>,
}

/// `daemon.shutdown` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ShutdownResult {
    /// True when this response replays a durable mutation receipt.
    #[serde(default)]
    pub replayed: bool,
    /// The daemon accepted the request and is stopping.
    pub accepted: bool,
    /// Accepted grace period before connections close; default 10 000, at most 60 000.
    pub grace_ms: u64,
}

/// A human may engage or explicitly release the persistent kill switch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KillSwitchAction {
    /// Revoke permissions and pause work.
    #[default]
    Engage,
    /// Clear the latch without resuming jobs or restoring grants.
    Release,
}

/// `daemon.kill_switch` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct KillSwitchParams {
    /// Requested state transition.
    #[serde(default)]
    pub action: KillSwitchAction,
    /// Human trust surface, checked against the authenticated role.
    #[serde(default)]
    pub surface: Option<zeus_types::Surface>,
    /// Free text for the audit record. Untrusted, at most 280 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `daemon.kill_switch` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct KillSwitchResult {
    /// Whether the durable latch changed value; engage still sweeps when already engaged.
    #[serde(default)]
    pub changed: bool,
    /// Whether a durable API receipt supplied the result.
    #[serde(default)]
    pub replayed: bool,
    /// Persisted state after the action.
    pub engaged: bool,
    /// Pending approvals invalidated when engaged.
    pub approvals_pending: u32,
    /// Grants revoked by this call.
    pub grants_revoked: u32,
    /// Jobs paused by this call.
    pub jobs_paused: u32,
}
