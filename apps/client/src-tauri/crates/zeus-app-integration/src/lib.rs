//! The Zeus app's browser integration, the MCP page and shell integration.
//!
//! Serves routes of the closed table through [`zeus_app::ext::Extension`]; the core table in `zeus-app` serves the rest.

#![forbid(unsafe_code)]

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use zeus_app::App;
use zeus_app::ext::{Extension, Reply};

/// The module registered by `zeus-app-ext`.
pub struct Module;

impl Module {
    /// Build the module (inside a Tokio runtime) for `app`.
    pub fn new(_app: &Arc<App>) -> Arc<Self> {
        Arc::new(Self)
    }
}

impl Extension for Module {
    fn call<'a>(
        &'a self,
        _app: &'a Arc<App>,
        _route: &'a str,
        _body: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>> {
        Box::pin(async { None })
    }
}
