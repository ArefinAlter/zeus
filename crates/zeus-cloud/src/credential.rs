use crate::{CloudError, CloudResult};
use zeroize::Zeroizing;

/// An explicitly supplied secret. Never serializable and always redacted by Debug.
#[derive(Clone)]
pub struct SecretString(Zeroizing<String>);
impl SecretString {
    /// Wrap configured credentials or a signed source URL.
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }
    /// Access solely for the authenticated provider/source request.
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretString([REDACTED])")
    }
}

/// Account-scoped credential lookup boundary. No environment or browser credential discovery.
pub trait CredentialStore: Send + Sync {
    /// Load the already-configured account credential.
    fn get(&self, account_id: &str) -> CloudResult<SecretString>;
    /// Store an explicitly supplied credential after human configuration authorization.
    fn put(&self, account_id: &str, value: &SecretString) -> CloudResult<()>;
    /// Remove the configured account credential.
    fn delete(&self, account_id: &str) -> CloudResult<()>;
}

/// OS keyring lookup using a fixed caller-selected service and `cloud/<account_id>` account.
#[cfg(feature = "os-keyring")]
#[derive(Debug, Clone)]
pub struct OsCredentialStore {
    /// Keyring service, normally `dev.zeus`.
    pub service: String,
}
#[cfg(feature = "os-keyring")]
impl CredentialStore for OsCredentialStore {
    fn get(&self, account_id: &str) -> CloudResult<SecretString> {
        validate_account(account_id)?;
        let value = keyring::Entry::new(&self.service, &format!("cloud/{account_id}"))
            .map_err(|_| CloudError::CredentialUnavailable)?
            .get_password()
            .map_err(|_| CloudError::CredentialUnavailable)?;
        Ok(SecretString::new(value))
    }
    fn put(&self, account_id: &str, value: &SecretString) -> CloudResult<()> {
        validate_account(account_id)?;
        keyring::Entry::new(&self.service, &format!("cloud/{account_id}"))
            .map_err(|_| CloudError::CredentialUnavailable)?
            .set_password(value.expose())
            .map_err(|_| CloudError::CredentialUnavailable)
    }
    fn delete(&self, account_id: &str) -> CloudResult<()> {
        validate_account(account_id)?;
        match keyring::Entry::new(&self.service, &format!("cloud/{account_id}"))
            .map_err(|_| CloudError::CredentialUnavailable)?
            .delete_credential()
        {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(CloudError::CredentialUnavailable),
        }
    }
}

/// Validate the configured account identifier without accepting arbitrary keyring paths.
pub fn validate_account(value: &str) -> CloudResult<()> {
    if value.is_empty()
        || value.len() > 96
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(CloudError::InvalidRequest);
    }
    Ok(())
}
