//! Credential storage abstraction. Provider keys and OAuth tokens live in the OS keyring (encrypted file
//! fallback is a later addition), never in configuration files, logs or audit records.

use std::collections::HashMap;
use std::sync::Mutex;

use zeroize::Zeroizing;

use crate::error::LlmError;
use crate::provider::ProviderKind;

/// Identifies one stored secret: a provider and a purpose (for example `api_key`, `access_token`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SecretKey {
    /// Which provider the secret belongs to.
    pub provider: ProviderKind,
    /// What the secret is.
    pub purpose: String,
}

impl SecretKey {
    /// The keyring account name for this key: `<provider>/<purpose>`.
    pub fn account(&self) -> String {
        format!("{}/{}", self.provider.as_str(), self.purpose)
    }
}

/// Store for provider secrets. Values are returned in zeroizing strings.
pub trait SecretStore: Send + Sync {
    /// Read a secret, or `None` when absent.
    fn get(&self, key: &SecretKey) -> Result<Option<Zeroizing<String>>, LlmError>;
    /// Write or replace a secret.
    fn set(&self, key: &SecretKey, value: &str) -> Result<(), LlmError>;
    /// Remove a secret. Removing an absent secret succeeds.
    fn delete(&self, key: &SecretKey) -> Result<(), LlmError>;
}

/// In-memory store for tests and for ephemeral sessions.
#[derive(Default)]
pub struct MemorySecretStore {
    values: Mutex<HashMap<SecretKey, Zeroizing<String>>>,
}

impl std::fmt::Debug for MemorySecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MemorySecretStore([REDACTED])")
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, key: &SecretKey) -> Result<Option<Zeroizing<String>>, LlmError> {
        let guard = self
            .values
            .lock()
            .map_err(|_| LlmError::SecretStore("poisoned".into()))?;
        Ok(guard.get(key).cloned())
    }

    fn set(&self, key: &SecretKey, value: &str) -> Result<(), LlmError> {
        let mut guard = self
            .values
            .lock()
            .map_err(|_| LlmError::SecretStore("poisoned".into()))?;
        guard.insert(key.clone(), Zeroizing::new(value.to_string()));
        Ok(())
    }

    fn delete(&self, key: &SecretKey) -> Result<(), LlmError> {
        let mut guard = self
            .values
            .lock()
            .map_err(|_| LlmError::SecretStore("poisoned".into()))?;
        guard.remove(key);
        Ok(())
    }
}

/// OS keyring backend (Windows Credential Manager, macOS Keychain, Secret Service).
/// Backend errors are deliberately opaque: platform diagnostics can contain account data.
#[cfg(feature = "os-keyring")]
#[derive(Debug, Clone)]
pub struct OsKeyringStore {
    /// Keyring service name, `dev.zeus`.
    pub service: String,
}

#[cfg(feature = "os-keyring")]
impl SecretStore for OsKeyringStore {
    fn get(&self, key: &SecretKey) -> Result<Option<Zeroizing<String>>, LlmError> {
        let entry = keyring::Entry::new(&self.service, &key.account()).map_err(keyring_error)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(Zeroizing::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(keyring_error(error)),
        }
    }

    fn set(&self, key: &SecretKey, value: &str) -> Result<(), LlmError> {
        keyring::Entry::new(&self.service, &key.account())
            .map_err(keyring_error)?
            .set_password(value)
            .map_err(keyring_error)
    }

    fn delete(&self, key: &SecretKey) -> Result<(), LlmError> {
        let entry = keyring::Entry::new(&self.service, &key.account()).map_err(keyring_error)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(keyring_error(error)),
        }
    }
}

#[cfg(feature = "os-keyring")]
fn keyring_error(_: keyring::Error) -> LlmError {
    LlmError::SecretStore("OS credential store is unavailable or access was denied".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trip() {
        let store = MemorySecretStore::default();
        let key = SecretKey {
            provider: ProviderKind::ApiKeyAnthropic,
            purpose: "api_key".into(),
        };
        assert!(store.get(&key).unwrap().is_none());
        store.set(&key, "ZEUS-CANARY-key").unwrap();
        assert_eq!(
            store.get(&key).unwrap().unwrap().as_str(),
            "ZEUS-CANARY-key"
        );
        store.delete(&key).unwrap();
        assert!(store.get(&key).unwrap().is_none());
        store.delete(&key).unwrap();
    }

    #[test]
    fn debug_hides_contents() {
        let store = MemorySecretStore::default();
        let key = SecretKey {
            provider: ProviderKind::LocalOllama,
            purpose: "api_key".into(),
        };
        store.set(&key, "ZEUS-CANARY-secret").unwrap();
        assert!(!format!("{store:?}").contains("ZEUS-CANARY"));
        assert_eq!(key.account(), "local_ollama/api_key");
    }
}
