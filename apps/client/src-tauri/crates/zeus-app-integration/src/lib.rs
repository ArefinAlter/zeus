//! The Zeus app's browser integration, the MCP page and shell integration.
//!
//! Serves routes of the closed table through [`zeus_app::ext::Extension`]; the core table in `zeus-app` serves the rest.
//!
//! | Route | What |
//! |---|---|
//! | `GET /native/v1/integration` | The status: the packaged extension, each browser's registration, the host's files, whether an extension is connected. |
//! | `POST /native/v1/integration/plan` | What an installation would write (shown before the person consents). |
//! | `POST /native/v1/integration/install` | Unpack the extension, write the launcher, register the host. Needs consent. |
//! | `POST /native/v1/integration/uninstall` | Remove the registrations. |
//! | `POST /native/v1/integration/unpack` | Unpack the extension into the app's folder. |
//! | `POST /native/v1/integration/export` | Copy the extension (zips and folders) into a folder the person names. |
//! | `GET /native/v1/mcp/config` | The configuration text for each AI assistant. |
//! | `POST /native/v1/mcp/test` | A real MCP handshake with the bundled `zeus mcp`. |

#![forbid(unsafe_code)]

pub mod host;
pub mod mcp;
pub mod package;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use zeus_app::App;
use zeus_app::ext::{Extension, Reply};
use zeus_app::failure::Failure;

/// The module registered by `zeus-app-ext`.
pub struct Module;

impl Module {
    /// Build the module for `app`.
    pub fn new(_app: &Arc<App>) -> Arc<Self> {
        Arc::new(Self)
    }
}

impl Extension for Module {
    fn call<'a>(
        &'a self,
        app: &'a Arc<App>,
        route: &'a str,
        body: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>> {
        Box::pin(async move {
            let (method, path) = route.split_once(' ')?;
            let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
            match (method, parts.as_slice()) {
                ("GET", ["native", "v1", "integration"]) => Some(host::status(app).await),
                ("POST", ["native", "v1", "integration", "plan"]) => Some(host::plan(app, body)),
                ("POST", ["native", "v1", "integration", "install"]) => {
                    Some(host::install(app, body))
                }
                ("POST", ["native", "v1", "integration", "uninstall"]) => {
                    Some(host::uninstall(app, body))
                }
                ("POST", ["native", "v1", "integration", "unpack"]) => {
                    Some(package::unpack(&app.home))
                }
                ("POST", ["native", "v1", "integration", "export"]) => {
                    let dir = body.get("dir").and_then(Value::as_str).unwrap_or_default();
                    Some(package::export(&app.home, std::path::Path::new(dir)))
                }
                ("GET", ["native", "v1", "mcp", "config"]) => {
                    Some(Ok(mcp::config(app, cfg!(windows))))
                }
                ("POST", ["native", "v1", "mcp", "test"]) => Some(mcp::test(app).await),
                (_, ["native", "v1", "integration", ..] | ["native", "v1", "mcp", ..]) => {
                    Some(Err(Failure::new(
                        "app.unknown_route",
                        "This client asked for something this build does not offer.",
                    )))
                }
                _ => None,
            }
        })
    }
}
