//! API errors: `api.*` codes, [`ApiError`], and the mapping to the 00 error envelope and to JSON-RPC codes.

use serde_json::Value;
use thiserror::Error;
use zeus_types::{ErrorClass, ErrorCode, ErrorEnvelope};

use crate::jsonrpc::ErrorObject;

/// Result type of API operations.
pub type ApiResult<T> = Result<T, ApiError>;

/// Codes of the `api` namespace used by this crate, with their class and retryability. Core registers
/// `api.idempotency_conflict`, `api.not_found`, `api.invalid_transition` and `api.not_interruptible`; the rest are
/// defined here until core's registry adopts them (a test checks that registered codes agree).
pub mod codes {
    use zeus_types::ErrorClass;

    /// The peer has not authenticated yet.
    pub const NOT_AUTHENTICATED: &str = "api.not_authenticated";
    /// Unknown credential or wrong proof (one code, so it is not an oracle).
    pub const BAD_CREDENTIAL: &str = "api.bad_credential";
    /// The credential lacks the permission for this method.
    pub const PERMISSION_DENIED: &str = "api.permission_denied";
    /// The method needs a human-kind client.
    pub const HUMAN_REQUIRED: &str = "api.human_required";
    /// The server did not prove it holds the IPC secret.
    pub const SERVER_PROOF_INVALID: &str = "api.server_proof_invalid";
    /// `daemon.authenticate` was already completed on this connection.
    pub const ALREADY_AUTHENTICATED: &str = "api.already_authenticated";
    /// No protocol version in common.
    pub const VERSION_UNSUPPORTED: &str = "api.version_unsupported";
    /// Malformed JSON.
    pub const PARSE_ERROR: &str = "api.parse_error";
    /// Valid JSON that is not a valid JSON-RPC message.
    pub const INVALID_REQUEST: &str = "api.invalid_request";
    /// Parameters do not match the method.
    pub const INVALID_PARAMS: &str = "api.invalid_params";
    /// No such method.
    pub const METHOD_NOT_FOUND: &str = "api.method_not_found";
    /// The method exists in the catalogue but this build does not implement it.
    pub const NOT_IMPLEMENTED: &str = "api.not_implemented";
    /// The method or parameter needs a capability the connection did not negotiate (05 section 8.2).
    pub const CAPABILITY_MISSING: &str = "api.capability_missing";
    /// A message line exceeded the limit.
    pub const PAYLOAD_TOO_LARGE: &str = "api.payload_too_large";
    /// Rate limit exceeded; `details.retry_after_ms` says when to retry.
    pub const RATE_LIMITED: &str = "api.rate_limited";
    /// The daemon is not ready (starting, shutting down).
    pub const UNAVAILABLE: &str = "api.unavailable";
    /// No answer within the deadline.
    pub const TIMEOUT: &str = "api.timeout";
    /// An event subscriber fell too far behind.
    pub const SLOW_CONSUMER: &str = "api.slow_consumer";
    /// A decision named a request hash that is not the current one (the human saw something else).
    pub const REQUEST_HASH_MISMATCH: &str = "policy.request_hash_mismatch";
    /// A decision chose a scope the approval does not allow.
    pub const SCOPE_NOT_ALLOWED: &str = "policy.scope_not_allowed";
    /// Unexpected failure inside the server.
    pub const INTERNAL: &str = "internal.bug";

    /// `(code, class, retryable)` for every code defined by this crate.
    pub const TABLE: &[(&str, ErrorClass, bool)] = &[
        (NOT_AUTHENTICATED, ErrorClass::Unauthenticated, false),
        (BAD_CREDENTIAL, ErrorClass::Unauthenticated, false),
        (PERMISSION_DENIED, ErrorClass::Unauthenticated, false),
        (HUMAN_REQUIRED, ErrorClass::PolicyDenied, false),
        (SERVER_PROOF_INVALID, ErrorClass::Unauthenticated, false),
        (ALREADY_AUTHENTICATED, ErrorClass::Conflict, false),
        (VERSION_UNSUPPORTED, ErrorClass::Unsupported, false),
        (PARSE_ERROR, ErrorClass::InvalidRequest, false),
        (INVALID_REQUEST, ErrorClass::InvalidRequest, false),
        (INVALID_PARAMS, ErrorClass::InvalidRequest, false),
        (METHOD_NOT_FOUND, ErrorClass::Unsupported, false),
        (NOT_IMPLEMENTED, ErrorClass::Unsupported, false),
        (CAPABILITY_MISSING, ErrorClass::Unsupported, false),
        (PAYLOAD_TOO_LARGE, ErrorClass::InvalidRequest, false),
        (RATE_LIMITED, ErrorClass::Unavailable, true),
        (UNAVAILABLE, ErrorClass::Unavailable, true),
        (TIMEOUT, ErrorClass::Timeout, true),
        (SLOW_CONSUMER, ErrorClass::Unavailable, true),
        (REQUEST_HASH_MISMATCH, ErrorClass::Conflict, false),
        (SCOPE_NOT_ALLOWED, ErrorClass::InvalidRequest, false),
        ("api.conflict", ErrorClass::Conflict, false),
        // Artifact lifecycle plans never authorize blind retries with refreshed parameters.
        ("artifact.plan_stale", ErrorClass::Conflict, false),
        ("artifact.plan_expired", ErrorClass::Conflict, false),
        (
            "artifact.lifecycle_reconciliation_required",
            ErrorClass::Conflict,
            false,
        ),
        (
            "artifact.operation_in_progress",
            ErrorClass::Conflict,
            false,
        ),
        // Immutable source retries must reconcile retained scope/budget; none renew authority.
        ("sources.denied", ErrorClass::PolicyDenied, false),
        ("sources.budget", ErrorClass::PolicyDenied, false),
        ("sources.changed", ErrorClass::Conflict, false),
        ("sources.integrity", ErrorClass::Integrity, false),
        ("sources.canceled", ErrorClass::PolicyDenied, false),
        ("sources.credential", ErrorClass::AuthExpired, false),
        ("sources.unsupported", ErrorClass::Unsupported, false),
        ("sources.failed", ErrorClass::Unavailable, false),
        ("sources.capacity", ErrorClass::Unavailable, false),
        ("sources.destination_exists", ErrorClass::Conflict, false),
        (
            "sources.restart_review_required",
            ErrorClass::Conflict,
            false,
        ),
        ("sources.transfer_failed", ErrorClass::Unavailable, false),
        // Cloud mutations can have succeeded remotely before a local error. None authorizes
        // blind transport retry; inspect the durable delivery/checkpoint first (spec 14).
        ("cloud.unsupported", ErrorClass::Unsupported, false),
        (
            "cloud.credential_unavailable",
            ErrorClass::AuthExpired,
            false,
        ),
        ("cloud.operation_failed", ErrorClass::Unavailable, false),
        (
            "cloud.transport_failed",
            ErrorClass::TransientNetwork,
            false,
        ),
        ("cloud.provider_auth_failed", ErrorClass::AuthExpired, false),
        ("cloud.provider_denied", ErrorClass::PolicyDenied, false),
        ("cloud.provider_throttled", ErrorClass::Throttled, false),
        ("cloud.provider_quota", ErrorClass::PolicyDenied, false),
        ("cloud.provider_protocol", ErrorClass::Integrity, false),
        ("cloud.integrity_failed", ErrorClass::Integrity, false),
        ("cloud.checkpoint_mismatch", ErrorClass::Conflict, false),
        (
            "cloud.checkpoint_unavailable",
            ErrorClass::Unavailable,
            false,
        ),
        ("cloud.already_exists", ErrorClass::Conflict, false),
        ("cloud.reconciliation_required", ErrorClass::Conflict, false),
        ("cloud.account_changed", ErrorClass::Conflict, false),
        ("cloud.account_revoked", ErrorClass::PolicyDenied, false),
        ("cloud.requester_revoked", ErrorClass::PolicyDenied, false),
        ("cloud.approval_required", ErrorClass::PolicyDenied, false),
        ("cloud.deadline_exceeded", ErrorClass::Timeout, false),
        ("cloud.delivery_stopped", ErrorClass::PolicyDenied, false),
        (
            "cloud.source_unavailable",
            ErrorClass::TransientNetwork,
            false,
        ),
        ("cloud.source_changed", ErrorClass::Integrity, false),
        ("cloud.capacity_exceeded", ErrorClass::Throttled, false),
        ("cloud.oauth_canceled", ErrorClass::PolicyDenied, false),
        ("cloud.oauth_denied", ErrorClass::PolicyDenied, false),
        ("cloud.oauth_expired", ErrorClass::AuthExpired, false),
        ("cloud.oauth_failed", ErrorClass::Unavailable, false),
        ("cloud.oauth_outcome_uncertain", ErrorClass::Conflict, false),
        ("cloud.oauth_pending", ErrorClass::Unavailable, false),
        ("cloud.oauth_protocol", ErrorClass::Integrity, false),
        (
            "cloud.oauth_remote_revocation_unavailable",
            ErrorClass::Unsupported,
            false,
        ),
        (
            "cloud.oauth_remote_revocation_uncertain",
            ErrorClass::Conflict,
            false,
        ),
        (
            "cloud.oauth_sign_in_required",
            ErrorClass::AuthExpired,
            false,
        ),
        ("cloud.oauth_slow_down", ErrorClass::Throttled, false),
        ("cloud.oauth_transport", ErrorClass::TransientNetwork, false),
        ("cloud.oauth_unsupported", ErrorClass::Unsupported, false),
        ("llm.not_configured", ErrorClass::Unavailable, false),
        ("llm.privacy_violation", ErrorClass::PolicyDenied, false),
        ("llm.budget_exceeded", ErrorClass::PolicyDenied, false),
        ("llm.disabled_by_policy", ErrorClass::PolicyDenied, false),
        ("llm.auth_failed", ErrorClass::Unauthenticated, false),
        ("llm.schema_violation", ErrorClass::InvalidRequest, false),
        ("llm.secret_store", ErrorClass::Unavailable, false),
        ("llm.provider_unavailable", ErrorClass::Unavailable, false),
        ("llm.auth_not_found", ErrorClass::NotFound, false),
        ("llm.auth_busy", ErrorClass::Conflict, false),
        ("llm.too_many_auth_flows", ErrorClass::Throttled, false),
        (
            "api.authentication_failed",
            ErrorClass::Unauthenticated,
            false,
        ),
        ("api.canceled", ErrorClass::PolicyDenied, false),
        ("api.integrity", ErrorClass::Integrity, false),
        ("api.resource_too_large", ErrorClass::InvalidRequest, false),
        ("nm.handshake_required", ErrorClass::Unauthenticated, false),
        ("plan.convert_unavailable", ErrorClass::Unsupported, false),
        ("policy.approval_required", ErrorClass::PolicyDenied, false),
        (
            "workflow.capability_missing",
            ErrorClass::Unsupported,
            false,
        ),
        (
            "workflow.destructive_capability_missing",
            ErrorClass::Unsupported,
            false,
        ),
        (
            "workflow.document_unavailable",
            ErrorClass::Unavailable,
            false,
        ),
        ("workflow.enqueue_failed", ErrorClass::Unavailable, false),
        ("workflow.manifest_pending", ErrorClass::Unavailable, false),
        (
            "workflow.sample_fetch_failed",
            ErrorClass::Unavailable,
            false,
        ),
        ("workflow.shutting_down", ErrorClass::Unavailable, false),
        (
            "workflow.storage_unavailable",
            ErrorClass::Unavailable,
            false,
        ),
        ("workflow.dry_run_failed", ErrorClass::Unavailable, false),
        (
            "workflow.domain_not_allowed",
            ErrorClass::PolicyDenied,
            false,
        ),
        (
            "workflow.redirect_insecure",
            ErrorClass::PolicyDenied,
            false,
        ),
        ("workflow.dry_run_required", ErrorClass::Conflict, false),
        ("workflow.invalid_transition", ErrorClass::Conflict, false),
        ("workflow.plan_hash_mismatch", ErrorClass::Conflict, false),
        ("workflow.sample_state", ErrorClass::Conflict, false),
        ("workflow.key_limit", ErrorClass::InvalidRequest, false),
        ("workflow.page_too_large", ErrorClass::InvalidRequest, false),
        ("workflow.plan_invalid", ErrorClass::InvalidRequest, false),
        (
            "workflow.redirect_invalid",
            ErrorClass::InvalidRequest,
            false,
        ),
        ("workflow.redirect_limit", ErrorClass::InvalidRequest, false),
        (
            "workflow.sample_too_large",
            ErrorClass::InvalidRequest,
            false,
        ),
        ("workflow.url_invalid", ErrorClass::InvalidRequest, false),
        (
            "workflow.document_accept",
            ErrorClass::InvalidRequest,
            false,
        ),
        ("workflow.sitemap_encoding", ErrorClass::Integrity, false),
        ("workflow.sitemap_scope", ErrorClass::PolicyDenied, false),
        (
            "workflow.sitemap_dtd_forbidden",
            ErrorClass::PolicyDenied,
            false,
        ),
        ("workflow.sitemap_limit", ErrorClass::PolicyDenied, false),
        ("workflow.sitemap_depth", ErrorClass::PolicyDenied, false),
        (
            "workflow.sitemap_pattern",
            ErrorClass::InvalidRequest,
            false,
        ),
        ("workflow.too_many_drafts", ErrorClass::Throttled, false),
        ("workflow.too_many_runs", ErrorClass::Throttled, false),
        (INTERNAL, ErrorClass::Internal, false),
    ];
}

/// An error that crosses the API boundary: always an [`ErrorEnvelope`]. Messages never contain secrets.
#[derive(Debug, Clone, PartialEq, Error)]
#[error("{}: {}", .0.code, .0.message)]
pub struct ApiError(pub ErrorEnvelope);

impl ApiError {
    /// Build an error from one of the codes in [`codes`] (or any registered core code). Class and retryability come
    /// from [`codes::TABLE`] or core's registry; an unknown code is a bug and becomes `internal`.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        if let Some((_, class, retryable)) = codes::TABLE.iter().find(|(c, _, _)| *c == code) {
            return Self(ErrorEnvelope::new(
                ErrorCode::from_static(code),
                *class,
                *retryable,
                message,
            ));
        }
        Self(ErrorEnvelope::from_code(code, message))
    }

    /// Attach a `details` entry.
    #[must_use]
    pub fn with_detail(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.0 = self.0.with_detail(key, value);
        self
    }

    /// The envelope.
    pub fn envelope(&self) -> &ErrorEnvelope {
        &self.0
    }

    /// The class of the envelope.
    pub fn class(&self) -> ErrorClass {
        self.0.class
    }

    /// `api.not_implemented` for a feature that is declared but not built yet.
    pub fn not_implemented(what: &'static str) -> Self {
        Self::new(codes::NOT_IMPLEMENTED, format!("not implemented: {what}"))
    }

    /// `api.invalid_params`.
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(codes::INVALID_PARAMS, message)
    }

    /// `api.not_authenticated`.
    pub fn not_authenticated() -> Self {
        Self::new(
            codes::NOT_AUTHENTICATED,
            "authenticate first (daemon.hello, then daemon.authenticate)",
        )
    }

    /// `api.permission_denied` naming the method (never secrets).
    pub fn permission_denied(method: &str) -> Self {
        Self::new(
            codes::PERMISSION_DENIED,
            format!("this credential may not call {method}"),
        )
    }

    /// `api.human_required`.
    pub fn human_required(method: &str) -> Self {
        Self::new(
            codes::HUMAN_REQUIRED,
            format!("{method} can only be called by a human-kind client"),
        )
    }

    /// `internal.bug` without leaking internals.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(codes::INTERNAL, message)
    }

    /// The JSON-RPC error object for this error.
    pub fn to_error_object(&self) -> ErrorObject {
        ErrorObject {
            code: rpc_code(&self.0),
            message: self.0.message.clone(),
            data: Some(self.0.clone()),
        }
    }

    /// Rebuild the error from a received JSON-RPC error object. A missing envelope becomes `internal`.
    pub fn from_error_object(object: &ErrorObject) -> Self {
        match &object.data {
            Some(envelope) => Self(envelope.clone()),
            None => Self::new(
                codes::INTERNAL,
                format!("peer sent an error without an envelope: {}", object.message),
            ),
        }
    }
}

impl From<ErrorEnvelope> for ApiError {
    fn from(envelope: ErrorEnvelope) -> Self {
        Self(envelope)
    }
}

/// JSON-RPC numeric code for an envelope. The envelope's `class` is authoritative; the number exists for generic
/// JSON-RPC tooling (ARCHITECTURE section 5.5).
pub fn rpc_code(envelope: &ErrorEnvelope) -> i32 {
    match envelope.code.as_str() {
        codes::PARSE_ERROR => return -32700,
        codes::INVALID_REQUEST => return -32600,
        codes::METHOD_NOT_FOUND => return -32601,
        codes::PERMISSION_DENIED | codes::HUMAN_REQUIRED => return -32002,
        _ => {}
    }
    match envelope.class {
        ErrorClass::InvalidRequest => -32602,
        ErrorClass::Internal => -32603,
        ErrorClass::Unauthenticated => -32001,
        ErrorClass::NotFound => -32003,
        ErrorClass::Conflict => -32004,
        ErrorClass::Unavailable => -32005,
        ErrorClass::Timeout => -32006,
        ErrorClass::Unsupported => -32007,
        _ => -32000,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_codes_are_valid_and_agree_with_core_where_registered() {
        for (code, class, retryable) in codes::TABLE {
            ErrorCode::new(*code).expect("valid code syntax");
            if let Some(info) = zeus_types::error::lookup(code) {
                assert_eq!(info.class, *class, "{code}");
                assert_eq!(info.retryable, *retryable, "{code}");
            }
        }
    }

    #[test]
    fn codes_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for (code, _, _) in codes::TABLE {
            assert!(seen.insert(*code), "{code}");
        }
    }

    #[test]
    fn rpc_codes_follow_the_documented_table() {
        let code_of = |c: &'static str| rpc_code(ApiError::new(c, "x").envelope());
        assert_eq!(code_of(codes::PARSE_ERROR), -32700);
        assert_eq!(code_of(codes::INVALID_REQUEST), -32600);
        assert_eq!(code_of(codes::METHOD_NOT_FOUND), -32601);
        assert_eq!(code_of(codes::INVALID_PARAMS), -32602);
        assert_eq!(code_of(codes::INTERNAL), -32603);
        assert_eq!(code_of(codes::NOT_AUTHENTICATED), -32001);
        assert_eq!(code_of(codes::PERMISSION_DENIED), -32002);
        assert_eq!(code_of(zeus_types::codes::API_NOT_FOUND), -32003);
        assert_eq!(code_of(zeus_types::codes::API_INVALID_TRANSITION), -32004);
        assert_eq!(code_of(codes::RATE_LIMITED), -32005);
        assert_eq!(code_of(codes::TIMEOUT), -32006);
        assert_eq!(code_of(codes::VERSION_UNSUPPORTED), -32007);
    }

    #[test]
    fn error_objects_round_trip_the_envelope() {
        let error = ApiError::permission_denied("approvals.decide")
            .with_detail("method", "approvals.decide");
        let object = error.to_error_object();
        assert_eq!(object.code, -32002);
        assert_eq!(ApiError::from_error_object(&object), error);
    }

    #[test]
    fn retryability_matches_class_defaults() {
        assert!(ApiError::new(codes::RATE_LIMITED, "x").envelope().retryable);
        assert!(
            !ApiError::new(codes::INVALID_PARAMS, "x")
                .envelope()
                .retryable
        );
    }
}
