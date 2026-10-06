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
        (HUMAN_REQUIRED, ErrorClass::Unauthenticated, false),
        (SERVER_PROOF_INVALID, ErrorClass::Unauthenticated, false),
        (ALREADY_AUTHENTICATED, ErrorClass::Conflict, false),
        (VERSION_UNSUPPORTED, ErrorClass::Unsupported, false),
        (PARSE_ERROR, ErrorClass::InvalidRequest, false),
        (INVALID_REQUEST, ErrorClass::InvalidRequest, false),
        (INVALID_PARAMS, ErrorClass::InvalidRequest, false),
        (METHOD_NOT_FOUND, ErrorClass::Unsupported, false),
        (NOT_IMPLEMENTED, ErrorClass::Unsupported, false),
        (PAYLOAD_TOO_LARGE, ErrorClass::InvalidRequest, false),
        (RATE_LIMITED, ErrorClass::Unavailable, true),
        (UNAVAILABLE, ErrorClass::Unavailable, true),
        (TIMEOUT, ErrorClass::Timeout, true),
        (SLOW_CONSUMER, ErrorClass::Unavailable, true),
        (REQUEST_HASH_MISMATCH, ErrorClass::Conflict, false),
        (SCOPE_NOT_ALLOWED, ErrorClass::InvalidRequest, false),
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
