//! Typed parameters and results of every daemon API method.
//!
//! Each method is a marker type implementing [`RpcMethod`], which ties the wire name to its parameter and result
//! types. [`crate::policy::METHODS`] holds the matching authorisation table; a test keeps the two in sync.
//! All wire types derive `serde` and `schemars` so `docs/spec/schema/daemon-api.v1.schema.json` can be generated.

use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub mod admin;
pub mod daemon;
pub mod events;
pub mod files;
pub mod jobs;
pub mod sessions;
pub mod workflows;

/// A method of the daemon API: its wire name and types.
pub trait RpcMethod {
    /// Dotted wire name, for example `jobs.enqueue`.
    const NAME: &'static str;
    /// Parameters object.
    type Params: Serialize + DeserializeOwned + JsonSchema + Send + 'static;
    /// Result value.
    type Result: Serialize + DeserializeOwned + JsonSchema + Send + 'static;
}

macro_rules! rpc_methods {
    ($( $(#[$meta:meta])* $marker:ident = $name:literal ($params:ty) -> $result:ty; )*) => {
        $(
            $(#[$meta])*
            #[derive(Debug, Clone, Copy)]
            pub struct $marker;

            impl RpcMethod for $marker {
                const NAME: &'static str = $name;
                type Params = $params;
                type Result = $result;
            }
        )*

        /// Every method name defined by [`RpcMethod`] implementations, in catalogue order.
        pub const ALL_NAMES: &[&str] = &[ $( $name ),* ];

        /// `(method, params schema, result schema)` for every method, registering shared definitions in `generator`.
        pub fn describe_all(
            generator: &mut schemars::SchemaGenerator,
        ) -> Vec<(&'static str, schemars::Schema, schemars::Schema)> {
            vec![ $( ( $name, generator.subschema_for::<$params>(), generator.subschema_for::<$result>() ) ),* ]
        }
    };
}

use self::admin::*;
use self::daemon::*;
use self::events::*;
use self::files::*;
use self::jobs::*;
use self::sessions::*;
use self::workflows::*;

rpc_methods! {
    /// Negotiate the protocol version and capabilities; first call on a connection.
    DaemonHello = "daemon.hello" (HelloParams) -> HelloResult;
    /// Prove possession of the credential; second call on a connection.
    DaemonAuthenticate = "daemon.authenticate" (AuthenticateParams) -> AuthenticateResult;
    /// Liveness check.
    DaemonPing = "daemon.ping" (PingParams) -> PingResult;
    /// Daemon status summary.
    DaemonStatus = "daemon.status" (StatusParams) -> StatusResult;
    /// Stop the daemon (admin).
    DaemonShutdown = "daemon.shutdown" (ShutdownParams) -> ShutdownResult;
    /// Revoke all grants and pause all jobs (human).
    DaemonKillSwitch = "daemon.kill_switch" (KillSwitchParams) -> KillSwitchResult;

    /// Probe a URL without enqueueing.
    JobsProbe = "jobs.probe" (ProbeParams) -> ProbeResultView;
    /// Enqueue one or more downloads.
    JobsEnqueue = "jobs.enqueue" (EnqueueParams) -> EnqueueResult;
    /// List jobs.
    JobsList = "jobs.list" (JobsListParams) -> JobsListResult;
    /// Get one job.
    JobsGet = "jobs.get" (JobsGetParams) -> JobsGetResult;
    /// Wait for jobs to change state, with a timeout of at most 60 seconds.
    JobsWait = "jobs.wait" (JobsWaitParams) -> JobsWaitResult;
    /// Pause, resume, cancel, retry or reprioritize a job.
    JobsControl = "jobs.control" (JobsControlParams) -> JobsControlResult;
    /// Re-hash a completed file and compare.
    JobsVerify = "jobs.verify" (JobsVerifyParams) -> JobsVerifyResult;

    /// Subscribe to events.
    EventsSubscribe = "events.subscribe" (SubscribeParams) -> SubscribeResult;
    /// Cancel a subscription.
    EventsUnsubscribe = "events.unsubscribe" (UnsubscribeParams) -> UnsubscribeResult;

    /// List session grants (never material).
    SessionsList = "sessions.list" (SessionsListParams) -> SessionsListResult;
    /// Ask for a session grant; raises an approval, never grants.
    SessionsRequest = "sessions.request" (SessionsRequestParams) -> SessionsRequestResult;
    /// Revoke a grant (human).
    SessionsRevoke = "sessions.revoke" (SessionsRevokeParams) -> SessionsRevokeResult;

    /// List approvals.
    ApprovalsList = "approvals.list" (ApprovalsListParams) -> ApprovalsListResult;
    /// Get one approval.
    ApprovalsGet = "approvals.get" (ApprovalsGetParams) -> ApprovalsGetResult;
    /// Decide an approval (human, with channel evidence).
    ApprovalsDecide = "approvals.decide" (ApprovalsDecideParams) -> ApprovalsDecideResult;

    /// List files under the write roots.
    FilesList = "files.list" (FilesListParams) -> FilesListResult;
    /// Plan or apply an organization of files (later).
    FilesOrganize = "files.organize" (FilesOrganizeParams) -> FilesOrganizeResult;
    /// Convert files (later).
    FilesConvert = "files.convert" (FilesConvertParams) -> FilesConvertResult;
    /// Undo a previous file operation from the trash journal (later).
    FilesUndo = "files.undo" (FilesUndoParams) -> FilesUndoResult;

    /// Draft or validate a workflow plan (later).
    WorkflowsPlan = "workflows.plan" (WorkflowsPlanParams) -> WorkflowsPlanResult;
    /// Dry-run a plan (later).
    WorkflowsDryRun = "workflows.dry_run" (WorkflowsDryRunParams) -> WorkflowsDryRunResult;
    /// Run an approved plan (later).
    WorkflowsRun = "workflows.run" (WorkflowsRunParams) -> WorkflowsRunResult;
    /// Get a run report (later).
    WorkflowsGet = "workflows.get" (WorkflowsGetParams) -> WorkflowsGetResult;
    /// List runs (later).
    WorkflowsList = "workflows.list" (WorkflowsListParams) -> WorkflowsListResult;
    /// Pause, resume or cancel a run (later).
    WorkflowsControl = "workflows.control" (WorkflowsControlParams) -> WorkflowsControlResult;
    /// Export the manifest of a run (later).
    WorkflowsManifest = "workflows.manifest" (WorkflowsManifestParams) -> WorkflowsManifestResult;

    /// Read the newest audit records.
    AuditTail = "audit.tail" (AuditTailParams) -> AuditTailResult;
    /// Verify the audit hash chain.
    AuditVerify = "audit.verify" (AuditVerifyParams) -> AuditVerifyResult;
    /// Read configuration with secrets redacted.
    ConfigGet = "config.get" (ConfigGetParams) -> ConfigGetResult;
    /// Change configuration (human).
    ConfigSet = "config.set" (ConfigSetParams) -> ConfigSetResult;
    /// Run the health checks.
    DoctorRun = "doctor.run" (DoctorParams) -> DoctorResult;
    /// LLM provider status (later).
    LlmStatus = "llm.status" (LlmStatusParams) -> LlmStatusResult;
    /// Configure LLM providers (human, later).
    LlmConfigure = "llm.configure" (LlmConfigureParams) -> LlmConfigureResult;
    /// Begin provider sign-in on a trusted human surface.
    LlmAuthStart = "llm.auth.start" (LlmAuthStartParams) -> LlmAuthStartResult;
    /// Inspect sign-in/account status on a trusted human surface.
    LlmAuthStatus = "llm.auth.status" (LlmAuthStatusParams) -> LlmAuthStatusResult;
    /// Cancel an owned provider sign-in.
    LlmAuthCancel = "llm.auth.cancel" (LlmAuthCancelParams) -> LlmAuthFlow;
    /// Clear provider credentials and request remote revocation.
    LlmAuthSignOut = "llm.auth.sign_out" (LlmAuthSignOutParams) -> LlmAuthSignOutResult;
    /// List client credentials (admin, later).
    ClientsList = "clients.list" (ClientsListParams) -> ClientsListResult;
    /// Revoke a client credential (admin, later).
    ClientsRevoke = "clients.revoke" (ClientsRevokeParams) -> ClientsRevokeResult;
}

/// Names of notifications sent by the daemon to clients (they have no response).
pub mod notifications {
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    /// One event of a subscription (`params`: [`super::events::EventNotification`]).
    pub const EVENTS_EVENT: &str = "events.event";
    /// A subscription fell behind (`params`: [`super::events::LaggedNotification`]).
    pub const EVENTS_LAGGED: &str = "events.lagged";
    /// The daemon is about to stop (`params`: [`StoppingNotification`]).
    pub const DAEMON_STOPPING: &str = "daemon.stopping";
    /// A native-messaging frame, in both directions (`params`: [`NmFrame`]). Role `native_host` only.
    pub const NM_FRAME: &str = "nm.frame";

    /// `params` of `daemon.stopping`.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
    pub struct StoppingNotification {
        /// Why the daemon stops, for example `shutdown_requested`.
        pub reason: String,
        /// Milliseconds until connections are closed.
        pub grace_ms: u64,
    }

    /// `params` of `nm.frame`: one native-messaging message exactly as spec 02 defines it (the host adds and
    /// removes only the native-messaging length prefix).
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
    pub struct NmFrame {
        /// The message object (`v`, `id`, `type`, `body`, optionally `re`).
        pub frame: serde_json::Value,
    }

    /// Names of every notification, for the policy table and tests.
    pub const ALL_NAMES: &[&str] = &[EVENTS_EVENT, EVENTS_LAGGED, DAEMON_STOPPING, NM_FRAME];
}

/// Method names as constants, for call sites that do not need the types.
pub mod names {
    use super::RpcMethod;

    /// `daemon.hello`.
    pub const DAEMON_HELLO: &str = super::DaemonHello::NAME;
    /// `daemon.authenticate`.
    pub const DAEMON_AUTHENTICATE: &str = super::DaemonAuthenticate::NAME;
    /// `approvals.decide`, the human-only method.
    pub const APPROVALS_DECIDE: &str = super::ApprovalsDecide::NAME;
    /// `jobs.get`.
    pub const JOBS_GET: &str = super::JobsGet::NAME;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn method_names_are_unique_dotted_lowercase() {
        let mut seen = BTreeSet::new();
        for name in ALL_NAMES {
            assert!(seen.insert(*name), "{name}");
            assert!(name.contains('.'), "dotted");
            for part in name.split('.') {
                assert!(
                    !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn policy_table_and_method_list_agree() {
        let table: BTreeSet<&str> = crate::policy::METHODS.iter().map(|m| m.name).collect();
        let methods: BTreeSet<&str> = ALL_NAMES.iter().copied().collect();
        assert_eq!(table, methods);
    }

    #[test]
    fn every_schema_can_be_generated() {
        fn schema<M: RpcMethod>() {
            let params = schemars::schema_for!(M::Params);
            let result = schemars::schema_for!(M::Result);
            assert!(
                serde_json::to_string(&params).is_ok() && serde_json::to_string(&result).is_ok(),
                "{}",
                M::NAME
            );
        }
        schema::<DaemonHello>();
        schema::<DaemonAuthenticate>();
        schema::<JobsEnqueue>();
        schema::<JobsGet>();
        schema::<JobsList>();
        schema::<EventsSubscribe>();
        schema::<SessionsRequest>();
        schema::<ApprovalsDecide>();
        schema::<ApprovalsList>();
        schema::<FilesList>();
        schema::<WorkflowsPlan>();
        schema::<WorkflowsRun>();
        schema::<AuditVerify>();
        schema::<ConfigSet>();
        schema::<DoctorRun>();
        schema::<LlmStatus>();
    }
}
