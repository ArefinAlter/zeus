//! Anonymous visitor sessions for the hosted mode, kept in a small JSON file beside the bridge.
//!
//! A session is a random 128-bit identifier handed out as a cookie (or a bearer token for the desktop app). It owns
//! one download folder under the daemon's write root and the job identifiers it created; it expires 24 hours after it
//! was created. Nothing about a visitor is stored beyond that: no address, no user agent, no IP.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Milliseconds since the epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 32 lowercase hex characters from the operating system's random source.
pub fn random_id() -> String {
    let mut bytes = [0u8; 16];
    rand::fill(&mut bytes[..]);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// True for a well-formed session identifier.
pub fn well_formed(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// One visitor sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// The identifier (also the folder name).
    pub id: String,
    /// Creation instant, epoch ms.
    pub created_ms: u64,
    /// Job identifiers this session created.
    #[serde(default)]
    pub jobs: Vec<String>,
    /// Files counted against the quota.
    #[serde(default)]
    pub files: u32,
    /// Bytes counted against the quota (from the fixture catalogue).
    #[serde(default)]
    pub bytes: u64,
}

/// All sessions, persisted on every change.
pub struct Sessions {
    path: PathBuf,
    map: HashMap<String, Session>,
}

impl Sessions {
    /// Load from `dir/sessions.json`, or start empty.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("sessions.json");
        let map = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<Session>>(&b).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|s| well_formed(&s.id))
            .map(|s| (s.id.clone(), s))
            .collect();
        Self { path, map }
    }

    fn save(&self) {
        let list: Vec<&Session> = self.map.values().collect();
        let Ok(bytes) = serde_json::to_vec(&list) else {
            return;
        };
        let temp = self.path.with_extension("json.tmp");
        if std::fs::write(&temp, bytes).is_ok() {
            let _ = std::fs::rename(&temp, &self.path);
        }
    }

    /// A live session.
    pub fn get(&self, id: &str, ttl_ms: u64) -> Option<&Session> {
        self.map
            .get(id)
            .filter(|s| now_ms().saturating_sub(s.created_ms) < ttl_ms)
    }

    /// Number of sessions held.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Create a new session.
    pub fn create(&mut self) -> Session {
        let s = Session {
            id: random_id(),
            created_ms: now_ms(),
            jobs: Vec::new(),
            files: 0,
            bytes: 0,
        };
        self.map.insert(s.id.clone(), s.clone());
        self.save();
        s
    }

    /// Change a session and persist.
    pub fn update(&mut self, id: &str, f: impl FnOnce(&mut Session)) {
        if let Some(s) = self.map.get_mut(id) {
            f(s);
            self.save();
        }
    }

    /// Forget expired sessions; returns their identifiers.
    pub fn prune(&mut self, ttl_ms: u64) -> Vec<String> {
        let now = now_ms();
        let gone: Vec<String> = self
            .map
            .values()
            .filter(|s| now.saturating_sub(s.created_ms) >= ttl_ms)
            .map(|s| s.id.clone())
            .collect();
        for id in &gone {
            self.map.remove(id);
        }
        if !gone.is_empty() {
            self.save();
        }
        gone
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_random_hex_and_validated() {
        let a = random_id();
        assert!(well_formed(&a));
        assert_ne!(a, random_id());
        assert!(!well_formed("../etc"));
        assert!(!well_formed(&a.to_uppercase()));
    }

    #[test]
    fn sessions_expire_and_persist() {
        let dir = std::env::temp_dir().join(format!("zeus-bridge-test-{}", random_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = Sessions::load(&dir);
        let one = s.create();
        s.update(&one.id, |x| x.files = 3);
        let again = Sessions::load(&dir);
        assert_eq!(again.get(&one.id, 1000).unwrap().files, 3);
        let mut again = again;
        assert_eq!(again.prune(0), vec![one.id.clone()]);
        assert!(again.get(&one.id, 1000).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
