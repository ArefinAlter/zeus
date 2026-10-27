//! Extensions: modules that serve more routes of the closed table.
//!
//! The core (`routes`) serves jobs, queues, link review, settings and the engine. Larger features (the file index, the
//! grabbers, the agent, browser integration) live in their own crates and register an [`Extension`]. A route that no
//! extension claims falls through to the core table, so an extension is asked first and must return `None` for what is
//! not its own.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;

use crate::App;
use crate::failure::Result;

/// The answer of an extension: `None` when the route is not its own.
pub type Reply = Option<Result<Value>>;

/// A module that serves some routes.
pub trait Extension: Send + Sync {
    /// Serve `route` (`"GET /native/v1/index/search"`), or return `None`.
    fn call<'a>(
        &'a self,
        app: &'a Arc<App>,
        route: &'a str,
        body: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>>;

    /// Called once when the app shuts down, before the engine stops.
    fn shutdown<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }
}
