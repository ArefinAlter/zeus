//! The method table: which method needs which permission, which are human-only, which mutate, which are MVP.
//!
//! This table is the single source of truth for authorisation. [`crate::server::Router`] consults it before it
//! calls any handler, so a handler cannot forget the check. Adding a method means adding a row here and a marker in
//! [`crate::methods`]; a test keeps both lists identical.

use serde_json::Value;

use crate::error::{ApiError, codes};
use crate::kind::{ClientIdentity, ClientRole, Permission};

/// Delivery stage of a method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Part of the MVP.
    Mvp,
    /// Defined now, implemented in a later phase.
    Later,
}

/// One row of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodSpec {
    /// Wire name.
    pub name: &'static str,
    /// Permission the credential must hold. `None` only for the pre-authentication methods.
    pub permission: Option<Permission>,
    /// Requires a human-kind (or admin) client regardless of permissions.
    pub human_only: bool,
    /// Changes state (informational: drives idempotency-key handling and audit).
    pub mutating: bool,
    /// MVP or later.
    pub stage: Stage,
    /// A boolean parameter that makes the call human-only when `true` (for example `reveal_urls`).
    pub human_param: Option<&'static str>,
    /// Restrict to one client role.
    pub role: Option<ClientRole>,
    /// The capability that gates the method (05 section 8.2), if any.
    pub capability: Option<&'static str>,
}

impl MethodSpec {
    const fn base(name: &'static str, permission: Option<Permission>) -> Self {
        Self {
            name,
            permission,
            human_only: false,
            mutating: false,
            stage: Stage::Mvp,
            human_param: None,
            role: None,
            capability: None,
        }
    }

    /// A method callable before authentication.
    const fn pre_auth(name: &'static str) -> Self {
        Self::base(name, None)
    }

    /// A method needing `permission`.
    const fn needs(name: &'static str, permission: Permission) -> Self {
        Self::base(name, Some(permission))
    }

    const fn mutates(mut self) -> Self {
        self.mutating = true;
        self
    }

    const fn human(mut self) -> Self {
        self.human_only = true;
        self
    }

    const fn later(mut self) -> Self {
        self.stage = Stage::Later;
        self
    }

    const fn human_if(mut self, param: &'static str) -> Self {
        self.human_param = Some(param);
        self
    }

    const fn only_role(mut self, role: ClientRole) -> Self {
        self.role = Some(role);
        self
    }

    const fn gated(mut self, capability: &'static str) -> Self {
        self.capability = Some(capability);
        self
    }

    /// True when the method is pre-authentication.
    pub const fn is_pre_auth(&self) -> bool {
        self.permission.is_none()
    }
}

use Permission::*;

/// Every method of the daemon API. See ARCHITECTURE section 5.4.
pub const METHODS: &[MethodSpec] = &[
    MethodSpec::needs("cloud.accounts.list", JobsRead),
    MethodSpec::needs("cloud.accounts.configure", ConfigWrite)
        .mutates()
        .human(),
    MethodSpec::needs("cloud.accounts.revoke", ConfigWrite)
        .mutates()
        .human(),
    MethodSpec::needs("cloud.plan", JobsWrite).mutates(),
    MethodSpec::needs("cloud.submit", JobsWrite).mutates(),
    MethodSpec::needs("cloud.list", JobsRead),
    MethodSpec::needs("cloud.get", JobsRead),
    MethodSpec::needs("cloud.control", JobsControl).mutates(),
    MethodSpec::pre_auth("daemon.hello"),
    MethodSpec::pre_auth("daemon.authenticate"),
    MethodSpec::needs("daemon.ping", DaemonRead),
    MethodSpec::needs("daemon.status", DaemonRead),
    MethodSpec::needs("daemon.shutdown", DaemonAdmin)
        .mutates()
        .human(),
    MethodSpec::needs("daemon.kill_switch", KillSwitch)
        .mutates()
        .human(),
    MethodSpec::needs("jobs.probe", JobsRead),
    MethodSpec::needs("jobs.enqueue", JobsWrite).mutates(),
    MethodSpec::needs("jobs.list", JobsRead),
    MethodSpec::needs("jobs.get", JobsRead).human_if("reveal_urls"),
    MethodSpec::needs("jobs.wait", JobsRead).gated("jobs.wait"),
    MethodSpec::needs("jobs.control", JobsControl).mutates(),
    MethodSpec::needs("jobs.verify", JobsRead),
    MethodSpec::needs("queues.list", JobsRead),
    MethodSpec::needs("queues.put", JobsControl).mutates(),
    MethodSpec::needs("queues.delete", JobsControl).mutates(),
    MethodSpec::needs("queues.control", JobsControl).mutates(),
    MethodSpec::needs("queues.configure", JobsControl).mutates(),
    MethodSpec::needs("events.subscribe", EventsRead).gated("events"),
    MethodSpec::needs("events.unsubscribe", EventsRead).gated("events"),
    MethodSpec::needs("sessions.list", SessionsRead),
    MethodSpec::needs("sessions.request", SessionsRequest).mutates(),
    MethodSpec::needs("sessions.revoke", SessionsRevoke)
        .mutates()
        .human(),
    MethodSpec::needs("approvals.list", ApprovalsRead),
    MethodSpec::needs("approvals.get", ApprovalsRead),
    MethodSpec::needs("approvals.decide", ApprovalsDecide)
        .mutates()
        .human(),
    MethodSpec::needs("files.list", FilesRead),
    MethodSpec::needs("files.organize", FilesWrite)
        .gated("files.organize")
        .mutates()
        .later(),
    MethodSpec::needs("files.convert", FilesWrite)
        .gated("files.convert")
        .mutates()
        .later(),
    MethodSpec::needs("files.undo", FilesWrite)
        .gated("files.organize")
        .mutates()
        .later(),
    MethodSpec::needs("workflows.plan", WorkflowsPlan)
        .gated("workflows")
        .mutates()
        .later(),
    MethodSpec::needs("workflows.dry_run", WorkflowsPlan)
        .gated("workflows")
        .later(),
    MethodSpec::needs("workflows.run", WorkflowsRun)
        .gated("workflows")
        .mutates()
        .later(),
    MethodSpec::needs("workflows.get", WorkflowsPlan)
        .gated("workflows")
        .later(),
    MethodSpec::needs("workflows.list", WorkflowsPlan)
        .gated("workflows")
        .later(),
    MethodSpec::needs("workflows.control", WorkflowsRun)
        .gated("workflows")
        .mutates()
        .later(),
    MethodSpec::needs("workflows.manifest", WorkflowsPlan)
        .gated("workflows")
        .mutates()
        .later(),
    MethodSpec::needs("audit.tail", AuditRead),
    MethodSpec::needs("audit.verify", AuditRead),
    MethodSpec::needs("config.get", ConfigRead),
    MethodSpec::needs("config.set", ConfigWrite)
        .mutates()
        .human(),
    MethodSpec::needs("doctor.run", DaemonRead),
    MethodSpec::needs("llm.status", LlmRead)
        .gated("llm")
        .later(),
    MethodSpec::needs("llm.configure", LlmConfigure)
        .gated("llm")
        .mutates()
        .human()
        .later(),
    MethodSpec::needs("llm.auth.start", LlmConfigure)
        .human()
        .later(),
    MethodSpec::needs("llm.auth.status", LlmConfigure)
        .human()
        .later(),
    MethodSpec::needs("llm.auth.cancel", LlmConfigure)
        .human()
        .later(),
    MethodSpec::needs("llm.auth.sign_out", LlmConfigure)
        .human()
        .later(),
    MethodSpec::needs("clients.list", DaemonAdmin)
        .gated("clients")
        .human(),
    MethodSpec::needs("clients.add", DaemonAdmin)
        .gated("clients")
        .mutates()
        .human(),
    MethodSpec::needs("clients.revoke", DaemonAdmin)
        .gated("clients")
        .mutates()
        .human(),
    MethodSpec::needs("files.roots.register", FilesRootsRegister)
        .gated("files.roots")
        .mutates()
        .only_role(ClientRole::Service),
    MethodSpec::needs("artifacts.get", FilesRead)
        .gated("artifacts")
        .later(),
    MethodSpec::needs("artifacts.list", FilesRead)
        .gated("artifacts")
        .later(),
    MethodSpec::needs("artifacts.lineage", FilesRead)
        .gated("artifacts")
        .later(),
    MethodSpec::needs("artifacts.ingest", FilesWrite)
        .gated("artifacts")
        .mutates()
        .later(),
    MethodSpec::needs("artifacts.lifecycle.preview", FilesWrite)
        .gated("artifacts")
        .later(),
    MethodSpec::needs("artifacts.lifecycle.apply", FilesWrite)
        .gated("artifacts")
        .mutates()
        .later(),
];

/// The relay method for native-messaging frames. It is a notification, not an RPC method, so it has its own
/// entry: only a `native_host`-role credential holding `nm.relay` may send it.
pub const NM_FRAME: MethodSpec = MethodSpec::needs("nm.frame", NmRelay)
    .gated("nm.relay")
    .only_role(ClientRole::NativeHost);

/// Look a method up by wire name.
pub fn lookup(name: &str) -> Option<&'static MethodSpec> {
    METHODS.iter().find(|m| m.name == name)
}

/// Decide whether `identity` may call `spec` with `params`.
///
/// `identity` is `None` before authentication, when only pre-auth methods are allowed.
pub fn authorize(
    identity: Option<&ClientIdentity>,
    spec: &MethodSpec,
    params: &Value,
) -> Result<(), ApiError> {
    if spec.is_pre_auth() {
        return Ok(());
    }
    let identity = identity.ok_or_else(ApiError::not_authenticated)?;
    if identity.role == ClientRole::NativeHost
        && spec
            .permission
            .is_some_and(|permission| !ClientRole::native_host_permissions().contains(&permission))
    {
        return Err(ApiError::permission_denied(spec.name));
    }
    if let Some(permission) = spec.permission
        && !identity.has(permission)
    {
        return Err(ApiError::permission_denied(spec.name));
    }
    if let Some(role) = spec.role
        && identity.role != role
    {
        return Err(ApiError::permission_denied(spec.name));
    }
    let human_by_param = spec
        .human_param
        .is_some_and(|name| params.get(name).and_then(Value::as_bool).unwrap_or(false));
    if (spec.human_only || human_by_param) && !identity.kind.is_human_capable() {
        return Err(ApiError::new(
            codes::HUMAN_REQUIRED,
            format!("{} can only be called by a human-kind client", spec.name),
        ));
    }
    Ok(())
}

/// Step 4 of 05 section 7.4: the method's capability, and that of a gated parameter, must have been negotiated in
/// `daemon.hello`. The gated parameters are `idempotency_key` on mutating methods (`idempotency`) and `since_seq`
/// of `events.subscribe` (`events.replay`).
pub fn require_capabilities(
    spec: &MethodSpec,
    params: &Value,
    negotiated: &std::collections::BTreeSet<String>,
) -> Result<(), ApiError> {
    let present = |name: &str| params.get(name).is_some_and(|v| !v.is_null());
    let needed = spec
        .capability
        .into_iter()
        .chain(
            (spec.mutating && present("idempotency_key"))
                .then_some(crate::capabilities::IDEMPOTENCY),
        )
        .chain(
            (spec.name == "events.subscribe" && present("since_seq"))
                .then_some(crate::capabilities::EVENTS_REPLAY),
        );
    for capability in needed {
        if !negotiated.contains(capability) {
            return Err(ApiError::new(
                codes::CAPABILITY_MISSING,
                format!(
                    "{} needs capability `{capability}`, which this connection did not negotiate",
                    spec.name
                ),
            )
            .with_detail("capability", capability));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::CredentialId;
    use crate::kind::ClientKind;
    use std::collections::BTreeSet;

    fn identity(kind: ClientKind, role: ClientRole) -> ClientIdentity {
        let mut permissions = kind.default_permissions();
        if role == ClientRole::NativeHost {
            permissions.insert(Permission::NmRelay);
        }
        ClientIdentity {
            credential_id: CredentialId::parse("cli_01M45NHJ2AHWMXT8JVJ7BEEJX7").unwrap(),
            kind,
            role,
            permissions,
            client_name: "t".into(),
        }
    }

    #[test]
    fn names_are_unique() {
        let mut seen = BTreeSet::new();
        for method in METHODS {
            assert!(seen.insert(method.name), "{}", method.name);
        }
    }

    #[test]
    fn an_agent_can_call_no_human_only_method_even_with_every_agent_permission() {
        let agent = identity(ClientKind::Agent, ClientRole::Mcp);
        for method in METHODS {
            let result = authorize(Some(&agent), method, &serde_json::json!({}));
            if method.human_only {
                assert!(result.is_err(), "{} must be refused to agents", method.name);
            }
        }
    }

    #[test]
    fn agents_hold_no_permission_that_a_human_only_method_needs() {
        let agent = ClientKind::Agent.default_permissions();
        for method in METHODS.iter().filter(|m| m.human_only) {
            let permission = method
                .permission
                .expect("human-only methods are not pre-auth");
            assert!(
                !agent.contains(&permission),
                "{} needs {permission}, which agents must not hold",
                method.name
            );
        }
    }

    #[test]
    fn approval_decisions_and_kill_switch_are_human_only() {
        for name in [
            "approvals.decide",
            "sessions.revoke",
            "daemon.kill_switch",
            "config.set",
            "daemon.shutdown",
        ] {
            assert!(lookup(name).unwrap().human_only, "{name}");
        }
    }

    #[test]
    fn mutating_agent_methods_are_exactly_the_documented_ones() {
        let agent_mutating: BTreeSet<&str> = METHODS
            .iter()
            .filter(|m| m.mutating && !m.human_only)
            .map(|m| m.name)
            .collect();
        let expected: BTreeSet<&str> = [
            "cloud.plan",
            "cloud.submit",
            "cloud.control",
            "jobs.enqueue",
            "jobs.control",
            "queues.put",
            "queues.delete",
            "queues.control",
            "queues.configure",
            "sessions.request",
            "files.organize",
            "files.convert",
            "files.undo",
            "files.roots.register",
            "artifacts.ingest",
            "artifacts.lifecycle.apply",
            "workflows.plan",
            "workflows.run",
            "workflows.control",
            "workflows.manifest",
        ]
        .into_iter()
        .collect();
        assert_eq!(agent_mutating, expected);
    }

    #[test]
    fn nothing_but_hello_and_authenticate_works_before_authentication() {
        for method in METHODS {
            let result = authorize(None, method, &serde_json::json!({}));
            let pre_auth = matches!(method.name, "daemon.hello" | "daemon.authenticate");
            assert_eq!(result.is_ok(), pre_auth, "{}", method.name);
        }
    }

    #[test]
    fn revealing_urls_is_human_only() {
        let spec = lookup("jobs.get").unwrap();
        let agent = identity(ClientKind::Agent, ClientRole::Mcp);
        let human = identity(ClientKind::Human, ClientRole::Cli);
        assert!(
            authorize(
                Some(&agent),
                spec,
                &serde_json::json!({"reveal_urls": false})
            )
            .is_ok()
        );
        let err = authorize(
            Some(&agent),
            spec,
            &serde_json::json!({"reveal_urls": true}),
        )
        .unwrap_err();
        assert_eq!(err.envelope().code.as_str(), codes::HUMAN_REQUIRED);
        assert!(
            authorize(
                Some(&human),
                spec,
                &serde_json::json!({"reveal_urls": true})
            )
            .is_ok()
        );
    }

    #[test]
    fn nm_frame_is_for_the_native_host_only() {
        let host = identity(ClientKind::Human, ClientRole::NativeHost);
        let cli = identity(ClientKind::Admin, ClientRole::Cli);
        assert!(authorize(Some(&host), &NM_FRAME, &Value::Null).is_ok());
        assert!(authorize(Some(&cli), &NM_FRAME, &Value::Null).is_err());
    }

    #[test]
    fn native_relay_cannot_use_broad_legacy_permissions_for_general_api_authority() {
        let host = identity(ClientKind::Human, ClientRole::NativeHost);
        // Deliberately inject the old broad identity: the table must still protect every method.
        assert!(host.has(Permission::ApprovalsDecide));
        for method in METHODS.iter().filter(|method| !method.is_pre_auth()) {
            let permitted = method.permission == Some(Permission::DaemonRead);
            assert_eq!(
                authorize(Some(&host), method, &serde_json::json!({})).is_ok(),
                permitted,
                "{}",
                method.name
            );
        }
        assert!(authorize(Some(&host), &NM_FRAME, &serde_json::json!({})).is_ok());
    }

    #[test]
    fn admin_methods_need_the_admin_permission() {
        let human = identity(ClientKind::Human, ClientRole::Cli);
        let admin = identity(ClientKind::Admin, ClientRole::Cli);
        let shutdown = lookup("daemon.shutdown").unwrap();
        assert!(authorize(Some(&human), shutdown, &Value::Null).is_err());
        assert!(authorize(Some(&admin), shutdown, &Value::Null).is_ok());
    }

    #[test]
    fn a_service_calls_no_human_only_method_and_alone_registers_roots() {
        let service = identity(ClientKind::Service, ClientRole::Service);
        let agent = identity(ClientKind::Agent, ClientRole::Mcp);
        let admin = identity(ClientKind::Admin, ClientRole::Cli);
        for method in METHODS.iter().filter(|m| m.human_only) {
            for params in [
                serde_json::json!({}),
                serde_json::json!({"reveal_urls": true}),
            ] {
                assert!(
                    authorize(Some(&service), method, &params).is_err(),
                    "{}",
                    method.name
                );
            }
        }
        let spec = lookup("jobs.get").unwrap();
        assert!(
            authorize(
                Some(&service),
                spec,
                &serde_json::json!({"reveal_urls": true})
            )
            .is_err()
        );
        let roots = lookup("files.roots.register").unwrap();
        assert!(authorize(Some(&service), roots, &Value::Null).is_ok());
        assert!(authorize(Some(&agent), roots, &Value::Null).is_err());
        assert!(authorize(Some(&admin), roots, &Value::Null).is_err());
        // A forged service-kind identity on another role still cannot register roots.
        let mut forged = identity(ClientKind::Service, ClientRole::Mcp);
        forged.permissions.insert(Permission::FilesRootsRegister);
        assert!(authorize(Some(&forged), roots, &Value::Null).is_err());
    }

    #[test]
    fn gated_methods_follow_the_capability_table() {
        for method in METHODS.iter().chain([&NM_FRAME]) {
            if let Some(capability) = method.capability {
                assert!(
                    crate::capabilities::ALL.contains(&capability) || capability == "files.convert",
                    "{}",
                    method.name
                );
            }
        }
        assert_eq!(
            lookup("artifacts.ingest").unwrap().capability,
            Some("artifacts")
        );
        assert_eq!(
            lookup("files.undo").unwrap().capability,
            Some("files.organize")
        );
        assert_eq!(lookup("jobs.list").unwrap().capability, None);
        assert_eq!(lookup("cloud.submit").unwrap().capability, None);
        let negotiated = ["events".to_string()].into_iter().collect();
        let subscribe = lookup("events.subscribe").unwrap();
        assert!(require_capabilities(subscribe, &serde_json::json!({}), &negotiated).is_ok());
        let replay = serde_json::json!({"since_seq": 4});
        let error = require_capabilities(subscribe, &replay, &negotiated).unwrap_err();
        assert_eq!(error.envelope().code.as_str(), codes::CAPABILITY_MISSING);
    }

    #[test]
    fn later_methods_are_marked() {
        assert_eq!(lookup("workflows.run").unwrap().stage, Stage::Later);
        assert_eq!(lookup("jobs.enqueue").unwrap().stage, Stage::Mvp);
    }
}
