//! Explicit acquisition adapters. The daemon owns scope, identity, credentials,
//! immutable reviews and durable budgets; source code cannot create authority.
#![forbid(unsafe_code)]

#[cfg(feature = "hugging-face")]
pub mod hugging_face;
mod model;
#[cfg(feature = "s3")]
pub mod s3;
#[cfg(feature = "torrent")]
pub mod torrent;
#[cfg(any(feature = "s3", feature = "hugging-face"))]
mod transport;

pub use model::*;
#[cfg(any(feature = "s3", feature = "hugging-face"))]
pub use transport::{BoundSource, SourceTransport};

/// Durable authority supplied by the embedding for one exact source operation.
/// Checks must bind engine job identity, original requester and account revision.
pub trait SourceAuthority: Send + Sync {
    /// Recheck scope/cancellation and return remaining reviewed wall-clock time.
    fn remaining(&self) -> SourceResult<std::time::Duration>;
    /// Durably reserve cumulative received payload bytes before releasing/writing
    /// them. Counts retries; torrent control/handshake packets are not piece payload.
    fn charge(&self, bytes: u64) -> SourceResult<()>;
}

/// Failures contain fixed diagnostics, never source queries or credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// Invalid or out-of-scope immutable source specification.
    #[error("invalid immutable source specification")]
    Invalid,
    /// Adapter feature was not compiled into this build.
    #[error("source adapter is unavailable in this build")]
    Unsupported,
    /// Scope, owner, account revision or source binding was revoked or absent.
    #[error("source authority is unavailable or changed")]
    Denied,
    /// No explicitly configured credential exists.
    #[error("configured source credential is unavailable")]
    Credential,
    /// The provider did not preserve the reviewed immutable object.
    #[error("source object changed after review")]
    Changed,
    /// Provider authentication or access policy refused the request.
    #[error("source provider refused access")]
    ProviderDenied,
    /// The bounded provider protocol was not satisfied.
    #[error("source provider protocol failed")]
    Protocol,
    /// Network transport failed; no upstream diagnostic is retained.
    #[error("source transport failed")]
    Network,
    /// Reviewed cumulative receive limit or deadline was reached.
    #[error("reviewed source budget was reached")]
    Budget,
    /// Explicit cancellation stopped network activity.
    #[error("source operation was canceled")]
    Canceled,
    /// Private staging or checkpoint persistence failed.
    #[error("private source storage failed")]
    Storage,
    /// Received data failed a declared cryptographic content check.
    #[error("source content verification failed")]
    Integrity,
}

/// Result without upstream strings that might contain bearer addresses.
pub type SourceResult<T> = Result<T, SourceError>;
