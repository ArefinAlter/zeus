//! Where provider keys live.
//!
//! Keys are stored by this process, never by the page. The first choice is the operating system's credential store
//! (Windows Credential Manager, the Secret Service on Linux, the Keychain on macOS). Where there is none (a headless
//! Linux box), they go to an encrypted file under the app's folder: each key sealed with ChaCha20-Poly1305 under a key
//! derived from a passphrase with Argon2id, the passphrase typed by the person and held only in memory after unlock.
//! Nothing here writes a key to a log, a URL, an argument, an event or the webview; the page learns only "stored" and
//! the last four characters.

use std::path::PathBuf;
use std::sync::Mutex;

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::provider::{ApiKey, Kind};
use zeus_app::engine_config::write_atomic;
use zeus_app::home::AppHome;

const SERVICE: &str = "dev.verne.zeus.client";

/// Where keys are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    /// The operating system's credential store.
    Os,
    /// An encrypted file, unlocked with a passphrase.
    File,
}

/// Why a key operation failed. The text is safe to show.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// The credential store is not available or refused access.
    #[error("The operating system's credential store is not available.")]
    Unavailable,
    /// The key file exists and has not been unlocked.
    #[error("The key file is locked. Enter its passphrase first.")]
    Locked,
    /// No key file exists yet.
    #[error(
        "Set a passphrase first: this computer has no credential store, so keys go to an encrypted file."
    )]
    NotInitialized,
    /// The passphrase did not open the file.
    #[error("That passphrase does not open the key file.")]
    WrongPassphrase,
    /// The passphrase is too short.
    #[error("Use a passphrase of at least 8 characters.")]
    WeakPassphrase,
    /// The file could not be read or written.
    #[error("The key file could not be read or written.")]
    Io,
    /// The file is damaged.
    #[error("The key file is damaged.")]
    Corrupt,
}

#[derive(Serialize, Deserialize, Default)]
struct Sealed {
    nonce: String,
    ct: String,
}

#[derive(Serialize, Deserialize)]
struct Kdf {
    alg: String,
    m: u32,
    t: u32,
    p: u32,
    salt: String,
}

#[derive(Serialize, Deserialize)]
struct FileFormat {
    v: u32,
    kdf: Kdf,
    check: Sealed,
    entries: std::collections::BTreeMap<String, Sealed>,
}

/// What the page may know.
#[derive(Debug, Clone, Serialize)]
pub struct KeystoreStatus {
    /// Which backend is in use.
    pub backend: Backend,
    /// File backend: a key file exists.
    pub initialized: bool,
    /// File backend: the passphrase was entered this session.
    pub unlocked: bool,
}

/// The key store.
pub struct KeyStore {
    prefix: String,
    file: PathBuf,
    forced: Option<Backend>,
    probed: Mutex<Option<Backend>>,
    unlocked: Mutex<Option<Zeroizing<[u8; 32]>>>,
    kdf: (u32, u32, u32),
}

const CHECK_TEXT: &[u8] = b"zeus-keys-ok";

impl KeyStore {
    /// Subscription connections never consult the app's API-key store, even when it is locked.
    pub fn for_provider(&self, kind: Kind, id: &str) -> Result<Option<ApiKey>, KeyError> {
        if kind.is_subscription() {
            Ok(None)
        } else {
            self.get(id)
        }
    }

    /// The store for `home`. `ZEUS_KEYSTORE=file` or `os` forces a backend (tests, headless installs).
    pub fn new(home: &AppHome) -> Self {
        let digest = hex::encode(Sha256::digest(
            home.root.display().to_string().to_lowercase().as_bytes(),
        ));
        let forced = match std::env::var("ZEUS_KEYSTORE").as_deref() {
            Ok("file") => Some(Backend::File),
            Ok("os") => Some(Backend::Os),
            _ => None,
        };
        Self {
            prefix: digest[..8].to_owned(),
            file: home.app_dir().join("agent").join("keys.enc.json"),
            forced,
            probed: Mutex::new(None),
            unlocked: Mutex::new(None),
            kdf: (65536, 3, 1),
        }
    }

    /// A store forced to the file backend with a cheap KDF, for tests.
    pub fn file_for_tests(home: &AppHome) -> Self {
        let mut s = Self::new(home);
        s.forced = Some(Backend::File);
        s.kdf = (256, 1, 1);
        s
    }

    fn account(&self, id: &str) -> String {
        format!("{}/{}", self.prefix, id)
    }

    /// Which backend is in use (probes the credential store once).
    pub fn backend(&self) -> Backend {
        if let Some(b) = self.forced {
            return b;
        }
        let mut guard = self
            .probed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(b) = *guard {
            return b;
        }
        let probe = format!("{}/probe", self.prefix);
        let works = keyring::Entry::new(SERVICE, &probe)
            .and_then(|e| {
                e.set_password("zeus-probe")?;
                let ok = e.get_password().is_ok_and(|p| p == "zeus-probe");
                let _ = e.delete_credential();
                if ok {
                    Ok(())
                } else {
                    Err(keyring::Error::NoEntry)
                }
            })
            .is_ok();
        let b = if works { Backend::Os } else { Backend::File };
        *guard = Some(b);
        b
    }

    /// The status for the page.
    pub fn status(&self) -> KeystoreStatus {
        let backend = self.backend();
        KeystoreStatus {
            backend,
            initialized: backend == Backend::File && self.file.exists(),
            unlocked: backend == Backend::Os || self.unlocked.lock().is_ok_and(|g| g.is_some()),
        }
    }

    // ------------------------------------------------------------ the file backend

    fn derive(
        &self,
        passphrase: &str,
        salt: &[u8],
        m: u32,
        t: u32,
        p: u32,
    ) -> Result<Zeroizing<[u8; 32]>, KeyError> {
        let params = Params::new(m, t, p, Some(32)).map_err(|_| KeyError::Corrupt)?;
        let mut out = Zeroizing::new([0u8; 32]);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(passphrase.as_bytes(), salt, out.as_mut())
            .map_err(|_| KeyError::Corrupt)?;
        Ok(out)
    }

    fn seal(key: &[u8; 32], aad: &str, plain: &[u8]) -> Result<Sealed, KeyError> {
        let cipher = ChaCha20Poly1305::new_from_slice(key).map_err(|_| KeyError::Corrupt)?;
        let mut nonce = [0u8; 12];
        rand::fill(&mut nonce[..]);
        let ct = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plain,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| KeyError::Corrupt)?;
        Ok(Sealed {
            nonce: B64.encode(nonce),
            ct: B64.encode(ct),
        })
    }

    fn open(key: &[u8; 32], aad: &str, sealed: &Sealed) -> Result<Zeroizing<Vec<u8>>, KeyError> {
        let cipher = ChaCha20Poly1305::new_from_slice(key).map_err(|_| KeyError::Corrupt)?;
        let nonce = B64.decode(&sealed.nonce).map_err(|_| KeyError::Corrupt)?;
        let ct = B64.decode(&sealed.ct).map_err(|_| KeyError::Corrupt)?;
        if nonce.len() != 12 {
            return Err(KeyError::Corrupt);
        }
        cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &ct,
                    aad: aad.as_bytes(),
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| KeyError::WrongPassphrase)
    }

    fn read_file(&self) -> Result<FileFormat, KeyError> {
        let bytes = std::fs::read(&self.file).map_err(|_| KeyError::NotInitialized)?;
        serde_json::from_slice(&bytes).map_err(|_| KeyError::Corrupt)
    }

    fn write_file(&self, f: &FileFormat) -> Result<(), KeyError> {
        let bytes = serde_json::to_vec_pretty(f).map_err(|_| KeyError::Io)?;
        write_atomic(&self.file, &bytes).map_err(|_| KeyError::Io)
    }

    /// Create the key file with a new passphrase (file backend).
    pub fn initialize(&self, passphrase: &str) -> Result<(), KeyError> {
        if passphrase.chars().count() < 8 {
            return Err(KeyError::WeakPassphrase);
        }
        let mut salt = [0u8; 16];
        rand::fill(&mut salt[..]);
        let (m, t, p) = self.kdf;
        let key = self.derive(passphrase, &salt, m, t, p)?;
        let file = FileFormat {
            v: 1,
            kdf: Kdf {
                alg: "argon2id".into(),
                m,
                t,
                p,
                salt: B64.encode(salt),
            },
            check: Self::seal(&key, "check", CHECK_TEXT)?,
            entries: Default::default(),
        };
        self.write_file(&file)?;
        *self.unlocked.lock().map_err(|_| KeyError::Io)? = Some(key);
        Ok(())
    }

    /// Unlock the key file with its passphrase (file backend).
    pub fn unlock(&self, passphrase: &str) -> Result<(), KeyError> {
        let file = self.read_file()?;
        let salt = B64.decode(&file.kdf.salt).map_err(|_| KeyError::Corrupt)?;
        let key = self.derive(passphrase, &salt, file.kdf.m, file.kdf.t, file.kdf.p)?;
        let check = Self::open(&key, "check", &file.check)?;
        if check.as_slice() != CHECK_TEXT {
            return Err(KeyError::WrongPassphrase);
        }
        *self.unlocked.lock().map_err(|_| KeyError::Io)? = Some(key);
        Ok(())
    }

    /// Forget the passphrase-derived key (file backend).
    pub fn lock(&self) {
        if let Ok(mut g) = self.unlocked.lock() {
            *g = None;
        }
    }

    fn file_key(&self) -> Result<Zeroizing<[u8; 32]>, KeyError> {
        if !self.file.exists() {
            return Err(KeyError::NotInitialized);
        }
        self.unlocked
            .lock()
            .map_err(|_| KeyError::Io)?
            .as_ref()
            .map(|k| Zeroizing::new(**k))
            .ok_or(KeyError::Locked)
    }

    // ------------------------------------------------------------ the operations

    /// Store the key of provider `id`; returns its last four characters.
    pub fn set(&self, id: &str, key: &str) -> Result<String, KeyError> {
        let key = key.trim();
        let last4 = ApiKey::new(key.to_owned()).last4();
        match self.backend() {
            Backend::Os => keyring::Entry::new(SERVICE, &self.account(id))
                .and_then(|e| e.set_password(key))
                .map_err(|_| KeyError::Unavailable)?,
            Backend::File => {
                let k = self.file_key()?;
                let mut file = self.read_file()?;
                file.entries
                    .insert(id.to_owned(), Self::seal(&k, id, key.as_bytes())?);
                self.write_file(&file)?;
            }
        }
        Ok(last4)
    }

    /// The key of provider `id`, if stored.
    pub fn get(&self, id: &str) -> Result<Option<ApiKey>, KeyError> {
        match self.backend() {
            Backend::Os => match keyring::Entry::new(SERVICE, &self.account(id))
                .map_err(|_| KeyError::Unavailable)?
                .get_password()
            {
                Ok(p) => Ok(Some(ApiKey::new(p))),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(_) => Err(KeyError::Unavailable),
            },
            Backend::File => {
                if !self.file.exists() {
                    return Ok(None);
                }
                let k = self.file_key()?;
                let file = self.read_file()?;
                match file.entries.get(id) {
                    None => Ok(None),
                    Some(s) => {
                        let plain = Self::open(&k, id, s)?;
                        Ok(Some(ApiKey::new(
                            String::from_utf8(plain.to_vec()).map_err(|_| KeyError::Corrupt)?,
                        )))
                    }
                }
            }
        }
    }

    /// Whether a key is stored for `id`, and its last four characters when readable. Never an error for "locked".
    pub fn peek(&self, id: &str) -> (bool, Option<String>) {
        match self.get(id) {
            Ok(Some(k)) => (true, Some(k.last4())),
            Ok(None) => (false, None),
            Err(KeyError::Locked) => (
                self.read_file().is_ok_and(|f| f.entries.contains_key(id)),
                None,
            ),
            Err(_) => (false, None),
        }
    }

    /// Remove the key of provider `id`.
    pub fn delete(&self, id: &str) -> Result<(), KeyError> {
        match self.backend() {
            Backend::Os => match keyring::Entry::new(SERVICE, &self.account(id))
                .map_err(|_| KeyError::Unavailable)?
                .delete_credential()
            {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(_) => Err(KeyError::Unavailable),
            },
            Backend::File => {
                if !self.file.exists() {
                    return Ok(());
                }
                let mut file = self.read_file()?;
                if file.entries.remove(id).is_some() {
                    self.write_file(&file)?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> (tempfile::TempDir, AppHome) {
        let dir = tempfile::tempdir().unwrap();
        let home = AppHome::at(dir.path().to_path_buf());
        (dir, home)
    }

    #[test]
    fn the_file_backend_seals_keys_and_needs_the_passphrase() {
        let (dir, home) = home();
        let store = KeyStore::file_for_tests(&home);
        assert!(store.get("p1").unwrap().is_none());
        assert_eq!(store.set("p1", "k").unwrap_err(), KeyError::NotInitialized);
        assert_eq!(
            store.initialize("short").unwrap_err(),
            KeyError::WeakPassphrase
        );
        store.initialize("correct horse battery").unwrap();
        let last4 = store.set("p1", "ZEUS-CANARY-secret-1234").unwrap();
        assert_eq!(last4, "1234");
        assert_eq!(
            store.get("p1").unwrap().unwrap().expose(),
            "ZEUS-CANARY-secret-1234"
        );
        // The file holds no plaintext key and no passphrase.
        let raw = std::fs::read_to_string(dir.path().join("app/agent/keys.enc.json")).unwrap();
        assert!(
            !raw.contains("ZEUS-CANARY")
                && !raw.contains("secret")
                && !raw.contains("correct horse")
        );
        // A new process: locked until the passphrase is given.
        let again = KeyStore::file_for_tests(&home);
        assert_eq!(again.get("p1").unwrap_err(), KeyError::Locked);
        assert_eq!(again.peek("p1"), (true, None));
        assert_eq!(
            again.unlock("wrong passphrase").unwrap_err(),
            KeyError::WrongPassphrase
        );
        again.unlock("correct horse battery").unwrap();
        assert_eq!(again.get("p1").unwrap().unwrap().last4(), "1234");
        again.delete("p1").unwrap();
        assert!(again.get("p1").unwrap().is_none());
        again.lock();
        assert_eq!(again.get("p1").unwrap_err(), KeyError::Locked);
    }

    #[test]
    fn a_swapped_entry_does_not_open_under_another_providers_name() {
        let (dir, home) = home();
        let store = KeyStore::file_for_tests(&home);
        store.initialize("correct horse battery").unwrap();
        store.set("a", "ZEUS-CANARY-aaaa").unwrap();
        let path = dir.path().join("app/agent/keys.enc.json");
        let mut f: FileFormat = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let moved = f.entries.remove("a").unwrap();
        f.entries.insert("b".into(), moved);
        std::fs::write(&path, serde_json::to_vec(&f).unwrap()).unwrap();
        assert!(store.get("b").is_err());
    }

    /// One real round trip through the operating system's credential store, cleaned up afterwards. Skipped where no
    /// store exists (a headless Linux machine).
    #[test]
    fn the_os_store_round_trips_when_there_is_one() {
        let (_dir, home) = home();
        let mut store = KeyStore::new(&home);
        store.forced = None;
        if store.backend() != Backend::Os {
            return;
        }
        store.set("test-provider", "ZEUS-CANARY-os-9876").unwrap();
        assert_eq!(
            store.get("test-provider").unwrap().unwrap().expose(),
            "ZEUS-CANARY-os-9876"
        );
        store.delete("test-provider").unwrap();
        assert!(store.get("test-provider").unwrap().is_none());
    }
    #[test]
    fn subscriptions_do_not_require_an_initialized_api_key_store() {
        let d = tempfile::tempdir().unwrap();
        let store = KeyStore::file_for_tests(&AppHome::at(d.path().to_path_buf()));
        assert_eq!(store.get("p-claude").err(), Some(KeyError::NotInitialized));
        assert!(
            store
                .for_provider(Kind::Claude, "p-claude")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .for_provider(Kind::Codex, "p-codex")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store.for_provider(Kind::Anthropic, "p-api").err(),
            Some(KeyError::NotInitialized)
        );
    }
}
