//! Desktop adapter for the shared retained-capability copy worker. Grants remain in this app.
use serde_json::Value;
use std::path::PathBuf;
use zeus_app::failure::{Failure, Result};
/// Current retained roots and host policy callbacks supplied to a shared copy worker.
pub use zeus_file_workspace::copies::Authority;
/// Translate the library's redacted failure into the native application's envelope.
pub(crate) fn from_shared(error: zeus_file_workspace::failure::Failure) -> Failure {
    Failure::new(&error.code, error.message)
}
/// Supply a native current-authority failure to the shared worker without exposing paths.
pub(crate) fn to_shared(error: Failure) -> zeus_file_workspace::failure::Failure {
    zeus_file_workspace::failure::Failure::new(&error.code, error.message)
}
/// Native copy receipts, backed by the same implementation the daemon uses for reviewed sync.
pub struct Transfers(zeus_file_workspace::copies::Transfers);
impl Transfers {
    /// Load receipts without resuming or publishing any operation.
    pub fn new(path: PathBuf) -> Self {
        Self(zeus_file_workspace::copies::Transfers::new(path))
    }
    /// Start from the person's explicit source and destination under current local scopes.
    pub fn start(&self, from: String, to: String, authority: Authority) -> Result<Value> {
        self.0.start(from, to, authority).map_err(from_shared)
    }
    /// Resume explicitly with freshly acquired roots and policy callbacks.
    pub fn resume(&self, id: &str, authority: Authority) -> Result<Value> {
        self.0.resume(id, authority).map_err(from_shared)
    }
    /// List durable progress, including interrupted operations.
    pub fn list(&self) -> Value {
        self.0.list()
    }
    /// Request cancellation without deleting any staging or source data.
    pub fn cancel_all(&self) {
        self.0.cancel_all();
    }
    /// Observe or cancel one existing operation.
    pub fn get(&self, id: &str, cancel: bool) -> Result<Value> {
        self.0.get(id, cancel).map_err(from_shared)
    }
}
