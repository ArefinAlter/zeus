//! What the client keeps that zeusd does not hold: settings sections, lists, threads, bookmarks, preferences.
//!
//! One JSON file per named slice under `app/state/`, written atomically. The set of slice names is closed. A value that
//! looks like a provider key is refused: keys live in the operating system's credential store, never in these files.

use std::path::PathBuf;

use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::engine_config::write_atomic;
use crate::failure::{Failure, Result};
use crate::home::AppHome;

/// The slices the client may store, and nothing else.
pub const SLICES: &[&str] = &[
    "queues",
    "limiter",
    "settings",
    "skills",
    "threads",
    "bookmarks",
    "history",
    "connectors",
    "projects",
    "grabber",
    "jobmeta",
    "pending",
    "killswitch",
    "prefs",
    "keybindings",
];

const MAX_SLICE: usize = 2 * 1024 * 1024;

/// The slice store.
pub struct StateStore {
    dir: PathBuf,
    lock: Mutex<()>,
}

/// True when `text` contains something shaped like a provider key or a Zeus leak marker.
pub fn looks_secret(text: &str) -> bool {
    let bytes = text.as_bytes();
    let run = |from: usize, min: usize| {
        bytes[from.min(bytes.len())..]
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_' || **b == b'-')
            .count()
            >= min
    };
    if text.contains("ZEUS-CANARY-") {
        return true;
    }
    for (needle, min) in [("sk-", 16usize), ("AIza", 20), ("sk-ant-", 16)] {
        let mut start = 0;
        while let Some(i) = text[start..].find(needle) {
            let at = start + i;
            let boundary = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
            if boundary && run(at + needle.len(), min) {
                return true;
            }
            start = at + needle.len();
        }
    }
    false
}

impl StateStore {
    /// The store under `home`.
    pub fn new(home: &AppHome) -> Self {
        Self {
            dir: home.app_dir().join("state"),
            lock: Mutex::new(()),
        }
    }

    fn path(&self, slice: &str) -> PathBuf {
        self.dir.join(format!("{slice}.json"))
    }

    /// Every stored slice, as the client's `GET /svc/v1/state` expects.
    pub async fn all(&self) -> Result<Value> {
        let _guard = self.lock.lock().await;
        let mut slices = serde_json::Map::new();
        let mut first_seen: Option<u64> = None;
        for name in SLICES {
            let path = self.path(name);
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let Ok(value) = serde_json::from_slice::<Value>(&bytes) else { continue };
            let updated = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as u64);
            first_seen = Some(first_seen.map_or(updated, |f| f.min(updated)));
            slices.insert((*name).into(), json!({"value": value, "updated": updated}));
        }
        Ok(json!({"slices": slices, "first_seen": first_seen, "ttl_ms": Value::Null}))
    }

    /// Store one slice.
    pub async fn put(&self, slice: &str, value: &Value) -> Result<Value> {
        if !SLICES.contains(&slice) {
            return Err(Failure::new("app.unknown_slice", "That setting is not stored here."));
        }
        let text = serde_json::to_string(value)?;
        if text.len() > MAX_SLICE {
            return Err(Failure::new("app.too_large", "That list is too large to store."));
        }
        if looks_secret(&text) {
            return Err(Failure::new(
                "app.secret_refused",
                "Provider keys are never stored in settings; they go to the operating system's credential store.",
            ));
        }
        let _guard = self.lock.lock().await;
        write_atomic(&self.path(slice), text.as_bytes())?;
        Ok(json!({"ok": true}))
    }

    /// Remove every slice (Settings, About: clear what this app stored).
    pub async fn clear(&self) -> Result<Value> {
        let _guard = self.lock.lock().await;
        for name in SLICES {
            let _ = std::fs::remove_file(self.path(name));
        }
        Ok(json!({"ok": true}))
    }

    /// One stored slice, if any (for the Rust side's own use).
    pub async fn get(&self, slice: &str) -> Option<Value> {
        let _guard = self.lock.lock().await;
        let bytes = std::fs::read(self.path(slice)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_markers_are_recognised() {
        assert!(looks_secret("{\"key\":\"sk-abcdefghijklmnop1234\"}"));
        assert!(looks_secret("x ZEUS-CANARY-1"));
        assert!(looks_secret("AIzaSyA1234567890abcdefghij"));
        assert!(!looks_secret("{\"name\":\"task-list\",\"sk\":\"sk-short\"}"));
        assert!(!looks_secret("risk-assessment-report-final-version"));
    }

    #[tokio::test]
    async fn slices_round_trip_and_unknown_ones_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(&AppHome::at(dir.path().to_path_buf()));
        store.put("settings", &json!({"a": 1})).await.unwrap();
        let all = store.all().await.unwrap();
        assert_eq!(all["slices"]["settings"]["value"]["a"], 1);
        assert!(store.put("evil", &json!(1)).await.is_err());
        assert!(store.put("settings", &json!({"k": "sk-abcdefghijklmnop1234"})).await.is_err());
        store.clear().await.unwrap();
        assert!(store.all().await.unwrap()["slices"].as_object().unwrap().is_empty());
    }
}
