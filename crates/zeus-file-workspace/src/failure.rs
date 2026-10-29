//! Redacted local file-workspace errors, independent of any desktop or daemon transport.
use serde::Serialize;
/// A bounded public code and a path-free explanation. Adapters map codes to their own wire envelope.
#[derive(Debug, Clone, Serialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct Failure {
    /// Local file-workspace error code; never a source path or credential.
    pub code: String,
    /// Redacted explanation suitable for local review.
    pub message: String,
}
impl Failure {
    /// Construct a redacted failure.
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
    /// Refuse an invalid explicit input.
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("api.invalid_params", message)
    }
}
impl From<std::io::Error> for Failure {
    fn from(e: std::io::Error) -> Self {
        Self::new("app.io", format!("A file operation failed: {}", e.kind()))
    }
}
impl From<serde_json::Error> for Failure {
    fn from(_: serde_json::Error) -> Self {
        Self::invalid("The stored or requested file operation has an invalid format.")
    }
}
/// File-workspace operation result.
pub type Result<T> = std::result::Result<T, Failure>;
