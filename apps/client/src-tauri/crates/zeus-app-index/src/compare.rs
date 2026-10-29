//! Desktop adapter for the shared bounded recursive comparison. It never authorizes sync effects.
use crate::transfers::{Authority, from_shared};
use serde_json::Value;
use zeus_app::failure::Result;
/// The person's exact comparison folders and explicit content-verification preference.
pub use zeus_file_workspace::compare::Request;
/// Native comparison observations; all current read authority comes from the app service.
#[derive(Default)]
pub struct Comparisons(zeus_file_workspace::compare::Comparisons);
impl Comparisons {
    /// Start a bounded read-only comparison with retained roots and current policy checks.
    pub fn start(&self, request: Request, authority: Authority) -> Result<Value> {
        self.0.start(request, authority).map_err(from_shared)
    }
    /// Read status or cancel a comparison; neither action mutates files.
    pub fn status(&self, id: &str, cancel: bool) -> Result<Value> {
        self.0.status(id, cancel).map_err(from_shared)
    }
    /// Read at most 500 comparison rows.
    pub fn page(&self, id: &str, offset: usize, limit: usize) -> Result<Value> {
        self.0.page(id, offset, limit).map_err(from_shared)
    }
    /// Stop current scans at their next filesystem boundary.
    pub fn cancel_all(&self) {
        self.0.cancel_all();
    }
}
