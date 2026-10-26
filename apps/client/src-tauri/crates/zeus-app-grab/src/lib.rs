//! The Zeus app's link grabber checks and site grabber crawler.
//!
//! Routes (all closed, all answered in Rust; the page never fetches anything itself):
//! * `POST /svc/v1/sitegrab/preview`: walk a site and list what would be fetched ([`crawl`]).
//! * `POST /native/v1/sitegrab/cancel`: stop a walk by its `walkId`.
//! * `POST /native/v1/jobs/{id}/verify`: re-hash a finished download through zeusd's `jobs.verify`.
//!
//! The online check of a link batch (`POST /svc/v1/links/check`) and the reviewed batch (`/api/v1/links/preview` and
//! `/enqueue`) are served by the core table in `zeus-app`.

#![forbid(unsafe_code)]

pub mod crawl;
pub mod html;
pub mod robots;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use zeus_app::App;
use zeus_app::ext::{Extension, Reply};
use zeus_app::failure::{Failure, Result};

/// The module registered by `zeus-app-ext`.
pub struct Module {
    walks: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl Module {
    /// Build the module (inside a Tokio runtime) for `app`.
    pub fn new(_app: &Arc<App>) -> Arc<Self> {
        Arc::new(Self {
            walks: Mutex::new(HashMap::new()),
        })
    }

    async fn preview(&self, body: &Value) -> Result<Value> {
        let req: crawl::SiteRequest = serde_json::from_value(body.clone())?;
        let id = req
            .walk_id
            .clone()
            .filter(|i| !i.is_empty() && i.len() <= 64)
            .unwrap_or_else(zeus_app::review::random_id);
        let cancel = Arc::new(AtomicBool::new(false));
        if let Ok(mut walks) = self.walks.lock() {
            if walks.len() >= 8 {
                return Err(Failure::new(
                    "app.busy",
                    "Too many site walks are running. Wait for one to finish.",
                ));
            }
            walks.insert(id.clone(), cancel.clone());
        }
        let result = crawl::walk(&req, cancel).await;
        if let Ok(mut walks) = self.walks.lock() {
            walks.remove(&id);
        }
        Ok(serde_json::to_value(result)?)
    }

    fn cancel(&self, body: &Value) -> Result<Value> {
        let id = body
            .get("walkId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let found = self
            .walks
            .lock()
            .ok()
            .and_then(|w| w.get(id).cloned())
            .map(|f| f.store(true, Ordering::SeqCst))
            .is_some();
        Ok(json!({"ok": true, "found": found}))
    }
}

/// `jobs.verify` through the app's credential, shaped for the page.
///
/// zeusd reports a file whose bytes no longer match as an `integrity.hash_mismatch` error: against the published
/// hash the person gave, or against the record it made when the download finished. The page wants the facts either
/// way, so each case is answered as a result: `matches_expected: false` when the file still matches its record, and
/// `matches_recorded: false` (with no digests, which zeusd does not return) when the file itself has changed.
async fn verify(app: &Arc<App>, job_id: &str, body: &Value) -> Result<Value> {
    let mut params = json!({"job_id": job_id});
    let mut expected = false;
    if let Some(h) = body
        .get("expected_sha256")
        .and_then(Value::as_str)
        .filter(|h| !h.is_empty())
    {
        let h = h.trim().to_ascii_lowercase();
        if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Failure::invalid("A SHA-256 is 64 hexadecimal digits."));
        }
        params["expected_sha256"] = json!(h);
        expected = true;
    }
    let changed = || {
        json!({"matches_recorded": false, "matches_expected": null, "sha256": null, "blake3": null})
    };
    match app.daemon.raw("jobs.verify", params).await {
        Err(f) if f.code == "integrity.hash_mismatch" => {
            if !expected {
                return Ok(changed());
            }
            match app
                .daemon
                .raw("jobs.verify", json!({"job_id": job_id}))
                .await
            {
                Ok(mut v) => {
                    v["matches_expected"] = json!(false);
                    Ok(v)
                }
                Err(f2) if f2.code == "integrity.hash_mismatch" => Ok(changed()),
                Err(f2) => Err(f2),
            }
        }
        other => other,
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
                ("POST", ["svc", "v1", "sitegrab", "preview"]) => Some(self.preview(body).await),
                ("POST", ["native", "v1", "sitegrab", "cancel"]) => Some(self.cancel(body)),
                ("POST", ["native", "v1", "jobs", id, "verify"]) => {
                    Some(verify(app, id, body).await)
                }
                _ => None,
            }
        })
    }
}
