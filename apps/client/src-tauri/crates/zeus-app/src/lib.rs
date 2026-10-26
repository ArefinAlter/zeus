//! The Zeus desktop app's engine.
//!
//! The app owns its Zeus installation: on first launch it provisions the state folder, writes the engine's
//! configuration, starts the bundled `zeusd` as a supervised child process and talks to it over zeusd's local IPC
//! with its own restricted `app` credential. The page never sees that credential. It calls one closed route table
//! ([`App::call`]), whose routes mirror the HTTP routes of `zeus-web-bridge` so the same client code serves the
//! hosted pre-alpha, a bridge on loopback, and this app.
//!
//! The daemon keeps one listener, its IPC endpoint (ADR 0010); nothing here opens a network port except the optional
//! `http-harness` feature, a loopback bridge used only to test the interface in an ordinary browser.

#![forbid(unsafe_code)]

pub mod daemon;
pub mod engine_config;
pub mod failure;
pub mod held;
pub mod home;
pub mod links;
pub mod osopen;
pub mod review;
pub mod routes;
pub mod state;
pub mod supervisor;

#[cfg(feature = "http-harness")]
pub mod http;

use std::sync::Arc;

use serde_json::Value;

use crate::daemon::Daemon;
use crate::failure::Result;
use crate::held::Held;
use crate::home::AppHome;
use crate::review::Reviews;
use crate::state::StateStore;
use crate::supervisor::Supervisor;

/// The running app engine.
pub struct App {
    /// The app's folders.
    pub home: AppHome,
    /// The connection to zeusd.
    pub daemon: Arc<Daemon>,
    /// The supervisor of the zeusd child process.
    pub supervisor: Arc<Supervisor>,
    /// Settings and lists the client keeps.
    pub state: StateStore,
    /// Requests the app itself holds (site-grabber plans, file moves).
    pub held: Held,
    /// Link reviews waiting to be started.
    pub reviews: Reviews,
    /// When the app started, in epoch milliseconds.
    pub started_ms: u64,
    /// Signalled when the person's queue asked the app to close (the shell exits on it).
    pub quit: tokio::sync::Notify,
}

impl App {
    /// Build the app engine for `home` and start the engine supervisor in the background.
    ///
    /// Must be called inside a Tokio runtime. Returns at once; the engine reports its progress through
    /// [`Supervisor::status`].
    pub fn start(home: AppHome, bin_dir: std::path::PathBuf) -> Result<Arc<Self>> {
        home.create()?;
        let daemon = Arc::new(Daemon::new(&home)?);
        let supervisor = Supervisor::new(home.clone(), bin_dir, daemon.clone());
        supervisor.start();
        let started_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        Ok(Arc::new(Self {
            state: StateStore::new(&home),
            held: Held::new(&home),
            reviews: Reviews::default(),
            home,
            daemon,
            supervisor,
            started_ms,
            quit: tokio::sync::Notify::new(),
        }))
    }

    /// Serve one route of the closed table: `"GET /api/v1/jobs"`, `"POST /native/v1/queues/put"`, ...
    pub async fn call(self: &Arc<Self>, route: &str, body: Value) -> Result<Value> {
        routes::dispatch(self, route, body).await
    }

    /// Stop the engine cleanly (the app is closing).
    pub async fn shutdown(&self) {
        self.supervisor.shutdown().await;
    }
}
