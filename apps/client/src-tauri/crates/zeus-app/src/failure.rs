//! The one error type the page ever sees: a stable code and a sentence a person can read.

use serde::Serialize;
use zeus_api::ApiError;

/// A failure safe to show: a stable code (`engine.starting`, `api.invalid_transition`, ...) and a sentence.
/// Secrets, URL query strings and credentials never appear in either.
#[derive(Debug, Clone, Serialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct Failure {
    /// Stable code, from the daemon's envelope or this app's own `app.*` / `engine.*` set.
    pub code: String,
    /// What happened, calmly.
    pub message: String,
}

impl Failure {
    /// A failure with the given code and message.
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    /// A request the app refuses to serve (bad route, bad input).
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("api.invalid_params", message)
    }

    /// Map a daemon error. The daemon's messages are written for people and are already redacted.
    pub fn daemon(error: &ApiError) -> Self {
        let envelope = error.envelope();
        Self::new(envelope.code.as_str(), envelope.message.clone())
    }
}

impl From<std::io::Error> for Failure {
    fn from(e: std::io::Error) -> Self {
        Self::new("app.io", format!("A file operation failed: {}", e.kind()))
    }
}

impl From<serde_json::Error> for Failure {
    fn from(_: serde_json::Error) -> Self {
        Self::invalid("The request body is not valid for this operation.")
    }
}

/// Shorthand.
pub type Result<T> = std::result::Result<T, Failure>;
