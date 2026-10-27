//! What zeusd knows about files: which are quarantined, which job wrote them, their digests.
//!
//! Read from the daemon's job list (a small cache, so a burst of previews asks once). Nothing here is trusted to widen
//! what the app may do: a file the daemon quarantined is never previewed, opened, moved, renamed or deleted here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use zeus_app::App;

use crate::config::norm;

/// The daemon's facts about one file.
#[derive(Debug, Clone, Default)]
pub struct FileFacts {
    /// Zeus's own quarantine flag.
    pub quarantined: bool,
    /// The job that wrote the file.
    pub job_id: Option<String>,
    /// SHA-256 of the completed file.
    pub sha256: Option<String>,
}

type Table = Arc<HashMap<String, FileFacts>>;

/// A three-second cache of the daemon's file facts.
#[derive(Default)]
pub struct FactsCache {
    inner: Mutex<Option<(Instant, Table)>>,
}

impl FactsCache {
    /// The facts by normalized path; empty when the engine does not answer (the callers then refuse what needs a fact).
    pub async fn table(&self, app: &Arc<App>) -> Option<Table> {
        if let Ok(g) = self.inner.lock()
            && let Some((t, table)) = g.as_ref()
            && t.elapsed() < Duration::from_secs(3)
        {
            return Some(table.clone());
        }
        let items = app
            .daemon
            .all_complete("jobs.list", json!({}), 30)
            .await
            .ok()?;
        let mut map = HashMap::new();
        for job in items {
            let Some(path) = job.get("path").and_then(Value::as_str) else {
                continue;
            };
            map.insert(
                norm(&crate::scan::display(std::path::Path::new(path))),
                FileFacts {
                    quarantined: job
                        .get("quarantined")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    job_id: job.get("id").and_then(Value::as_str).map(str::to_owned),
                    sha256: job.get("sha256").and_then(Value::as_str).map(str::to_owned),
                },
            );
        }
        let table = Arc::new(map);
        if let Ok(mut g) = self.inner.lock() {
            *g = Some((Instant::now(), table.clone()));
        }
        Some(table)
    }

    /// Forget the cache (after a file operation).
    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.lock() {
            *g = None;
        }
    }
}

/// True when `path` or anything below it is quarantined.
pub fn quarantined_under(table: &HashMap<String, FileFacts>, path: &str) -> bool {
    let key = norm(path);
    let prefix = format!("{key}{}", std::path::MAIN_SEPARATOR);
    table
        .iter()
        .any(|(k, f)| f.quarantined && (*k == key || k.starts_with(&prefix)))
}

/// The BLAKE3 digest zeusd recorded for a job, from its artifact record.
pub async fn blake3_of(app: &Arc<App>, job_id: &str) -> Option<String> {
    let items = app.daemon.all("artifacts.list", json!({}), 10).await.ok()?;
    items
        .iter()
        .find(|a| a.get("job_id").and_then(Value::as_str) == Some(job_id))
        .and_then(|a| a.get("blake3").and_then(Value::as_str).map(str::to_owned))
}
