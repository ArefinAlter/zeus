//! A small log of what the app's engine layer refused or could not do, in `logs/app.log`.
//!
//! Lines carry a timestamp, the route (never its body) and the failure's code and message. Repeats of the same failure
//! are written at most once every thirty seconds, so a poll that fails while the engine starts does not fill the file.
//! Nothing here records addresses, query strings, credentials or file contents.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The log file and the repeat filter.
pub struct AppLog {
    path: PathBuf,
    seen: Mutex<HashMap<String, Instant>>,
}

impl AppLog {
    /// A log writing to `path`.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// Append one line, unless the same `key` was written in the last thirty seconds.
    pub fn line(&self, key: &str, text: &str) {
        if let Ok(mut seen) = self.seen.lock() {
            if seen
                .get(key)
                .is_some_and(|t| t.elapsed() < Duration::from_secs(30))
            {
                return;
            }
            seen.insert(key.to_owned(), Instant::now());
            if seen.len() > 512 {
                seen.retain(|_, t| t.elapsed() < Duration::from_secs(30));
            }
        }
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() > 2 * 1024 * 1024) {
            let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
        }
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(f, "{ms} {text}");
        }
    }
}
