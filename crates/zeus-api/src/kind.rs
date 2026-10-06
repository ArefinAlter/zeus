//! Who is calling: client kinds, roles, permissions and the authenticated identity.
//!
//! A credential fixes `(kind, role, permissions)`; a client cannot claim more in `daemon.hello`. Kinds are
//! ordered `agent < human < admin`. Only `human` and above can decide approvals (and only with channel
//! evidence, ADR 0007); no agent-kind credential can ever hold the permissions listed in
//! [`ClientKind::human_only_permissions`].

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeus_types::{Actor, ActorKind, Source, SourceKind, SourceVia, Surface};

use crate::auth::CredentialId;
use crate::error::{ApiError, codes};

/// Authority class of a client.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    /// MCP server, A2A bridge, scripts holding the agent credential. Never decides approvals.
    Agent,
    /// CLI or TUI on a terminal, GUI, native host relaying the extension popup.
    Human,
    /// A human-kind client that may also stop the daemon and manage credentials.
    Admin,
}

/// The concrete program behind a connection. Informational for audit and routing; authority comes from the kind.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ClientRole {
    /// The `zeus` command line.
    Cli,
    /// The terminal UI.
    Tui,
    /// The desktop GUI (later).
    Gui,
    /// `zeus-native-host`, relaying the browser extension.
    NativeHost,
    /// `zeus mcp` (stdio or Streamable HTTP bridge).
    Mcp,
    /// The A2A bridge (phase 2).
    A2a,
    /// An application embedding Zeus.
    Embedded,
    /// Test harnesses.
    Test,
}

impl ClientRole {
    /// The highest kind a credential with this role may have.
    pub const fn max_kind(self) -> ClientKind {
        match self {
            Self::Mcp | Self::A2a => ClientKind::Agent,
            Self::Tui | Self::Gui | Self::NativeHost => ClientKind::Human,
            Self::Cli | Self::Embedded | Self::Test => ClientKind::Admin,
        }
    }

    /// The job [`Source`] this role produces when it enqueues work through the API.
    pub fn job_source(self, client_id: &str) -> Source {
        let (kind, via) = match self {
            Self::Mcp => (SourceKind::Agent, SourceVia::Mcp),
            Self::A2a => (SourceKind::Agent, SourceVia::A2a),
            Self::Cli => (SourceKind::Cli, SourceVia::Cli),
            Self::Tui => (SourceKind::Cli, SourceVia::Tui),
            Self::Gui => (SourceKind::Cli, SourceVia::Jsonrpc),
            Self::NativeHost => (SourceKind::BrowserHandoff, SourceVia::NativeMessaging),
            Self::Embedded | Self::Test => (SourceKind::Agent, SourceVia::Jsonrpc),
        };
        let mut source = Source::new(kind);
        source.via = Some(via);
        source.client_id = Some(client_id.chars().take(128).collect());
        source
    }
}

/// Fine-grained permission. Spelled with a dot in JSON (`jobs.read`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Permission {
    /// Probe, list, get, wait for and verify jobs.
    JobsRead,
    /// Enqueue jobs.
    JobsWrite,
    /// Pause, resume, cancel, retry, reprioritize jobs.
    JobsControl,
    /// Subscribe to events.
    EventsRead,
    /// List session grants.
    SessionsRead,
    /// Ask for a session grant (raises an approval; never grants).
    SessionsRequest,
    /// Revoke session grants.
    SessionsRevoke,
    /// List and read approvals (agents: only their own).
    ApprovalsRead,
    /// Decide approvals.
    ApprovalsDecide,
    /// List files.
    FilesRead,
    /// Organize, convert, undo (dry run by default, destructive steps need an approval).
    FilesWrite,
    /// Plan, validate and dry-run workflows.
    WorkflowsPlan,
    /// Run and control workflows (needs an approval of the plan).
    WorkflowsRun,
    /// Read the audit log.
    AuditRead,
    /// Read configuration (secrets redacted).
    ConfigRead,
    /// Change configuration.
    ConfigWrite,
    /// Read LLM provider status.
    LlmRead,
    /// Configure LLM providers and credentials.
    LlmConfigure,
    /// Daemon status and doctor.
    DaemonRead,
    /// Stop the daemon, manage client credentials.
    DaemonAdmin,
    /// Revoke all grants and pause all jobs at once.
    KillSwitch,
    /// Relay native-messaging frames (role `native_host` only).
    NmRelay,
}

impl Permission {
    /// Every permission.
    pub const ALL: [Self; 22] = [
        Self::JobsRead,
        Self::JobsWrite,
        Self::JobsControl,
        Self::EventsRead,
        Self::SessionsRead,
        Self::SessionsRequest,
        Self::SessionsRevoke,
        Self::ApprovalsRead,
        Self::ApprovalsDecide,
        Self::FilesRead,
        Self::FilesWrite,
        Self::WorkflowsPlan,
        Self::WorkflowsRun,
        Self::AuditRead,
        Self::ConfigRead,
        Self::ConfigWrite,
        Self::LlmRead,
        Self::LlmConfigure,
        Self::DaemonRead,
        Self::DaemonAdmin,
        Self::KillSwitch,
        Self::NmRelay,
    ];

    /// The dotted wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::JobsRead => "jobs.read",
            Self::JobsWrite => "jobs.write",
            Self::JobsControl => "jobs.control",
            Self::EventsRead => "events.read",
            Self::SessionsRead => "sessions.read",
            Self::SessionsRequest => "sessions.request",
            Self::SessionsRevoke => "sessions.revoke",
            Self::ApprovalsRead => "approvals.read",
            Self::ApprovalsDecide => "approvals.decide",
            Self::FilesRead => "files.read",
            Self::FilesWrite => "files.write",
            Self::WorkflowsPlan => "workflows.plan",
            Self::WorkflowsRun => "workflows.run",
            Self::AuditRead => "audit.read",
            Self::ConfigRead => "config.read",
            Self::ConfigWrite => "config.write",
            Self::LlmRead => "llm.read",
            Self::LlmConfigure => "llm.configure",
            Self::DaemonRead => "daemon.read",
            Self::DaemonAdmin => "daemon.admin",
            Self::KillSwitch => "kill_switch",
            Self::NmRelay => "nm.relay",
        }
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Permission {
    type Err = ApiError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|p| p.as_str() == text)
            .ok_or_else(|| ApiError::invalid_params(format!("unknown permission `{text}`")))
    }
}

impl Serialize for Permission {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Permission {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse()
            .map_err(|e: ApiError| serde::de::Error::custom(e.envelope().message.clone()))
    }
}

impl JsonSchema for Permission {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Permission".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let names: Vec<&str> = Self::ALL.iter().map(|p| p.as_str()).collect();
        schemars::json_schema!({ "type": "string", "enum": names })
    }
}

impl ClientKind {
    /// Permissions that exist only for human-kind clients and above. An agent credential can never hold them.
    pub const fn human_only_permissions() -> &'static [Permission] {
        &[
            Permission::SessionsRevoke,
            Permission::ApprovalsDecide,
            Permission::ConfigWrite,
            Permission::LlmConfigure,
            Permission::KillSwitch,
        ]
    }

    /// Permissions only an admin holds.
    pub const fn admin_only_permissions() -> &'static [Permission] {
        &[Permission::DaemonAdmin]
    }

    /// The default (and maximum) permission set of this kind. `NmRelay` is granted separately to credentials
    /// whose role is `native_host`.
    pub fn default_permissions(self) -> BTreeSet<Permission> {
        use Permission::*;
        let mut set: BTreeSet<Permission> = [
            JobsRead,
            JobsWrite,
            JobsControl,
            EventsRead,
            SessionsRead,
            SessionsRequest,
            ApprovalsRead,
            FilesRead,
            FilesWrite,
            WorkflowsPlan,
            WorkflowsRun,
            AuditRead,
            ConfigRead,
            LlmRead,
            DaemonRead,
        ]
        .into_iter()
        .collect();
        if self >= Self::Human {
            set.extend(Self::human_only_permissions());
        }
        if self >= Self::Admin {
            set.extend(Self::admin_only_permissions());
        }
        set
    }
}

/// An authenticated client, as established by `daemon.authenticate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientIdentity {
    /// The credential used.
    pub credential_id: CredentialId,
    /// Authority class.
    pub kind: ClientKind,
    /// Concrete program.
    pub role: ClientRole,
    /// Effective permissions (subset of the kind's defaults, plus `nm.relay` for the native host).
    pub permissions: BTreeSet<Permission>,
    /// Self-declared name and version, for logs only. Untrusted.
    pub client_name: String,
}

impl ClientIdentity {
    /// True when the identity holds `permission`.
    pub fn has(&self, permission: Permission) -> bool {
        self.permissions.contains(&permission)
    }

    /// The [`Actor`] to record when this client decides something from `surface`.
    ///
    /// A human actor requires a human-or-admin kind **and** a trust surface (01 section 5.3 rule 1). Anything else
    /// is refused with `api.human_required`; agents never get a `human` actor, whatever surface they name.
    pub fn decision_actor(&self, surface: Surface) -> Result<Actor, ApiError> {
        if self.kind >= ClientKind::Human && surface.is_trust_surface() {
            let mut actor = Actor::human(surface);
            actor.client_id = Some(self.credential_id.to_string());
            Ok(actor)
        } else {
            Err(ApiError::new(
                codes::HUMAN_REQUIRED,
                "a decision needs a human-kind client on a trust surface",
            ))
        }
    }

    /// The [`Actor`] for non-decision actions: an agent, or the human's CLI as a program.
    pub fn action_actor(&self) -> Actor {
        let surface = match self.role {
            ClientRole::Mcp => Surface::Mcp,
            ClientRole::A2a => Surface::A2a,
            ClientRole::Tui => Surface::Tui,
            _ => Surface::Jsonrpc,
        };
        let kind = match self.kind {
            ClientKind::Agent => ActorKind::Agent,
            ClientKind::Human | ClientKind::Admin => ActorKind::Cli,
        };
        Actor {
            kind,
            surface: Some(surface),
            client_id: Some(self.credential_id.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(kind: ClientKind, role: ClientRole) -> ClientIdentity {
        ClientIdentity {
            credential_id: CredentialId::parse("cli_01M45NHJ2AHWMXT8JVJ7BEEJX7").unwrap(),
            kind,
            role,
            permissions: kind.default_permissions(),
            client_name: "test".into(),
        }
    }

    #[test]
    fn kinds_are_ordered() {
        assert!(ClientKind::Agent < ClientKind::Human && ClientKind::Human < ClientKind::Admin);
    }

    #[test]
    fn agents_never_hold_human_only_permissions() {
        let agent = ClientKind::Agent.default_permissions();
        for permission in ClientKind::human_only_permissions()
            .iter()
            .chain(ClientKind::admin_only_permissions())
        {
            assert!(!agent.contains(permission), "{permission}");
        }
        let human = ClientKind::Human.default_permissions();
        for permission in ClientKind::human_only_permissions() {
            assert!(human.contains(permission));
        }
        assert!(!human.contains(&Permission::DaemonAdmin));
        assert!(
            ClientKind::Admin
                .default_permissions()
                .contains(&Permission::DaemonAdmin)
        );
    }

    #[test]
    fn nm_relay_is_in_no_default_set() {
        for kind in [ClientKind::Agent, ClientKind::Human, ClientKind::Admin] {
            assert!(!kind.default_permissions().contains(&Permission::NmRelay));
        }
    }

    #[test]
    fn agent_roles_cap_at_agent_kind() {
        assert_eq!(ClientRole::Mcp.max_kind(), ClientKind::Agent);
        assert_eq!(ClientRole::A2a.max_kind(), ClientKind::Agent);
        assert_eq!(ClientRole::NativeHost.max_kind(), ClientKind::Human);
    }

    #[test]
    fn permission_names_round_trip_and_are_unique() {
        let mut names = BTreeSet::new();
        for permission in Permission::ALL {
            assert!(names.insert(permission.as_str()));
            assert_eq!(
                permission.as_str().parse::<Permission>().unwrap(),
                permission
            );
            let json = serde_json::to_string(&permission).unwrap();
            assert_eq!(
                serde_json::from_str::<Permission>(&json).unwrap(),
                permission
            );
        }
        assert!("jobs.nope".parse::<Permission>().is_err());
    }

    #[test]
    fn only_humans_on_trust_surfaces_get_a_human_actor() {
        let human = identity(ClientKind::Human, ClientRole::Cli);
        let actor = human.decision_actor(Surface::CliInteractive).unwrap();
        assert!(actor.is_human_on_trust_surface());
        assert!(human.decision_actor(Surface::Mcp).is_err());
        assert!(human.decision_actor(Surface::Jsonrpc).is_err());

        let agent = identity(ClientKind::Agent, ClientRole::Mcp);
        assert!(agent.decision_actor(Surface::CliInteractive).is_err());
        assert!(agent.decision_actor(Surface::ExtensionPopup).is_err());
        assert!(!agent.action_actor().is_human_on_trust_surface());
    }

    #[test]
    fn job_source_follows_the_role() {
        let source = ClientRole::Mcp.job_source("cli_x");
        assert_eq!(source.kind, SourceKind::Agent);
        assert_eq!(source.via, Some(SourceVia::Mcp));
        assert_eq!(ClientRole::Cli.job_source("cli_x").kind, SourceKind::Cli);
    }
}
