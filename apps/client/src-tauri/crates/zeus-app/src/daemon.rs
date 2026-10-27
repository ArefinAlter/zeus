//! The one authenticated connection to this app's zeusd.
//!
//! The credential is the installation's `app.token` (kind `agent`, role `gui`): it reads and controls downloads and
//! queues, plans workflows and reads files, approvals and configuration, and it can never decide an approval, change
//! a grant, write a file or reveal a job's original address. The connection is refused unless the daemon confirms
//! exactly that identity. The token is read here, in the Rust process, and never leaves it.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::Mutex;
use zeus_api::auth::{ClientCredential, IpcSecret};
use zeus_api::client::ClientConfig;
use zeus_api::endpoint::ZeusPaths;
use zeus_api::methods::daemon::ClientInfo;
use zeus_api::{ApiError, Client, ClientKind, ClientRole, IpcClient};
use zeus_types::SecretString;

use crate::failure::{Failure, Result};
use crate::home::AppHome;

/// The credential file this app uses.
pub const CREDENTIAL: &str = "app";

fn read_private(path: &std::path::Path) -> Result<SecretString> {
    let missing = || {
        Failure::new(
            "engine.not_installed",
            "The Zeus engine has not been set up yet.",
        )
    };
    zeus_api::transport::secure_private_file(path).map_err(|_| missing())?;
    let mut data = String::new();
    let read = std::fs::File::open(path)
        .map_err(|_| missing())?
        .take(4097)
        .read_to_string(&mut data);
    let data = SecretString::new(data);
    read.map_err(|_| missing())?;
    if data.len() > 4096 {
        return Err(missing());
    }
    Ok(data)
}

/// Resolve the engine's paths for `home` without touching the process environment.
pub fn zeus_paths(home: &AppHome) -> Result<ZeusPaths> {
    let env: std::collections::HashMap<String, String> = home.zeus_env().into_iter().collect();
    let identity = zeus_api::endpoint::current_user_identity().map_err(|e| Failure::daemon(&e))?;
    ZeusPaths::resolve(
        |k| env.get(k).cloned(),
        directories::BaseDirs::new(),
        identity,
    )
    .map_err(|e| Failure::daemon(&e))
}

/// The lazily opened, self-healing connection.
pub struct Daemon {
    paths: ZeusPaths,
    connection: Mutex<Option<Arc<IpcClient>>>,
}

impl Daemon {
    /// A handle for the installation under `home`. Nothing is opened yet.
    pub fn new(home: &AppHome) -> Result<Self> {
        Ok(Self {
            paths: zeus_paths(home)?,
            connection: Mutex::new(None),
        })
    }

    /// The paths of the installation.
    pub fn paths(&self) -> &ZeusPaths {
        &self.paths
    }

    async fn open(&self) -> Result<Arc<IpcClient>> {
        let secret =
            IpcSecret::from_file_text(read_private(&self.paths.ipc_secret_file())?.expose())
                .map_err(|e| Failure::daemon(&e))?;
        let credential = ClientCredential::from_file_text(
            read_private(&self.paths.credential_file(CREDENTIAL))?.expose(),
        )
        .map_err(|e| Failure::daemon(&e))?;
        let mut config = ClientConfig::new(ClientInfo {
            name: "zeus-app".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            role: ClientRole::Gui,
        });
        config.call_timeout = Duration::from_secs(30);
        let client = IpcClient::connect(&self.paths.endpoint(), credential, &secret, config)
            .await
            .map_err(|e| {
                Failure::new(
                    "engine.unreachable",
                    format!(
                        "The Zeus engine is not running or refused the connection ({}).",
                        e.envelope().code.as_str()
                    ),
                )
            })?;
        if client.session().role != ClientRole::Gui
            || client.session().credential_kind != ClientKind::Agent
        {
            return Err(Failure::new(
                "engine.identity_mismatch",
                "The engine did not accept the app's restricted credential.",
            ));
        }
        Ok(Arc::new(client))
    }

    /// The open connection, connecting first when needed.
    pub async fn client(&self) -> Result<Arc<IpcClient>> {
        let mut guard = self.connection.lock().await;
        if let Some(c) = guard.as_ref() {
            return Ok(c.clone());
        }
        // A busy pipe or a daemon that is just accepting can refuse once; try a few times before giving up.
        let mut last = None;
        for attempt in 0..4 {
            match self.open().await {
                Ok(c) => {
                    *guard = Some(c.clone());
                    return Ok(c);
                }
                Err(f) if f.code == "engine.unreachable" => {
                    last = Some(f);
                    tokio::time::sleep(Duration::from_millis(150 * (attempt + 1))).await;
                }
                Err(f) => return Err(f),
            }
        }
        Err(last.unwrap_or_else(|| {
            Failure::new("engine.unreachable", "The Zeus engine did not answer.")
        }))
    }

    /// Forget the connection (the next call reconnects).
    pub async fn reset(&self) {
        *self.connection.lock().await = None;
    }

    /// Call `method` with `params`. Transport failures drop the connection and are retried once on a fresh one;
    /// ordinary refusals keep it.
    pub async fn raw(&self, method: &str, params: Value) -> Result<Value> {
        for attempt in 0..2 {
            let client = self.client().await?;
            match client.call_raw(method, params.clone()).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport(&e) && attempt == 0 => {
                    self.reset().await;
                }
                Err(e) => {
                    if is_transport(&e) {
                        self.reset().await;
                    }
                    return Err(Failure::daemon(&e));
                }
            }
        }
        Err(Failure::new(
            "engine.unreachable",
            "The Zeus engine did not answer.",
        ))
    }

    /// Every item of a paged list method, up to `max_pages` pages of 100.
    pub async fn all(&self, method: &str, extra: Value, max_pages: usize) -> Result<Vec<Value>> {
        let mut items = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..max_pages {
            let mut params = extra.clone();
            if let Some(obj) = params.as_object_mut() {
                obj.insert("limit".into(), json!(100));
                if let Some(c) = &cursor {
                    obj.insert("cursor".into(), json!(c));
                }
            }
            let page = self.raw(method, params).await?;
            if let Some(list) = page.get("items").and_then(Value::as_array) {
                items.extend(list.iter().cloned());
            }
            match page.get("next_cursor").and_then(Value::as_str) {
                Some(next) => cursor = Some(next.to_owned()),
                None => break,
            }
        }
        Ok(items)
    }

    /// True when the daemon answers `daemon.ping` right now.
    pub async fn alive(&self) -> bool {
        self.raw("daemon.ping", json!({})).await.is_ok()
    }

    /// Capabilities negotiated on the current connection, if connected.
    pub async fn capabilities(&self) -> Vec<String> {
        match self.client().await {
            Ok(c) => c.hello().capabilities.iter().cloned().collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Permissions of the current connection, if connected.
    pub async fn permissions(&self) -> Vec<String> {
        match self.client().await {
            Ok(c) => c
                .session()
                .permissions
                .iter()
                .map(|p| p.as_str().to_owned())
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

fn is_transport(error: &ApiError) -> bool {
    let code = error.envelope().code.as_str();
    matches!(
        code,
        "api.unavailable" | "api.timeout" | "api.not_authenticated" | "api.slow_consumer"
    ) || code.starts_with("internal")
}
