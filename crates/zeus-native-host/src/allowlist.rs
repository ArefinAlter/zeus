//! The host's own allow-list of extension ids (spec 02 section 13.1 rule 1).
//!
//! The browser's host manifest already pins the extension (`allowed_origins` or `allowed_extensions`), but that
//! file is just a file. The host therefore checks the id from `argv` against a second list written by the
//! installer next to the binary, so tampering with the browser manifest alone does not widen access. Development
//! ids are accepted only when `dev_extensions` names them explicitly, and each start then logs a warning.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::HostError;

/// File name of the allow-list, next to the host binary.
pub const FILE_NAME: &str = "allowed-extensions.json";

/// The allow-list file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowList {
    /// Production extension ids.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Development extension ids (unpacked builds).
    #[serde(default)]
    pub dev_extensions: Vec<String>,
}

/// Why an id was accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allowed {
    /// A production id.
    Production,
    /// A development id: the caller logs a warning on every start.
    Development,
}

impl AllowList {
    /// Read the allow-list. A missing or unreadable file allows nothing.
    pub fn load(path: &Path) -> Result<Self, HostError> {
        let bytes = std::fs::read(path)?;
        serde_json::from_slice(&bytes)
            .map_err(|_| HostError::Install("allow-list file is malformed".into()))
    }

    /// Check an id taken from `argv`.
    pub fn check(&self, extension_id: &str) -> Result<Allowed, HostError> {
        if self.extensions.iter().any(|e| e == extension_id) {
            Ok(Allowed::Production)
        } else if self.dev_extensions.iter().any(|e| e == extension_id) {
            Ok(Allowed::Development)
        } else {
            Err(HostError::OriginNotAllowed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_ids_only() {
        let list = AllowList {
            extensions: vec!["aaaa".into()],
            dev_extensions: vec!["bbbb".into()],
        };
        assert_eq!(list.check("aaaa").unwrap(), Allowed::Production);
        assert_eq!(list.check("bbbb").unwrap(), Allowed::Development);
        for other in ["", "aaa", "aaaaa", "AAAA", "*"] {
            assert!(list.check(other).is_err(), "{other}");
        }
    }

    #[test]
    fn an_empty_list_allows_nothing() {
        assert!(AllowList::default().check("anything").is_err());
    }

    #[test]
    fn a_missing_file_is_an_error_not_an_open_door() {
        let dir = tempfile::tempdir().unwrap();
        assert!(AllowList::load(&dir.path().join(FILE_NAME)).is_err());
        std::fs::write(dir.path().join(FILE_NAME), br#"{"extensions":["x"]}"#).unwrap();
        assert_eq!(
            AllowList::load(&dir.path().join(FILE_NAME))
                .unwrap()
                .extensions,
            ["x"]
        );
    }
}
