//! Cloud delivery without implicit staging or provider capability guesses.
//!
//! The embedding daemon owns source authorization, local verification before staged upload,
//! durable checkpoints, and cancellation policy. Providers never fetch source URLs themselves:
//! uploads consume a bounded asynchronous reader, while remote import delegates to the provider.
#![forbid(unsafe_code)]

mod baidu;
mod config;
mod credential;
mod google;
mod http_client;
mod model;
pub mod oauth;
mod rclone;
mod source;
#[cfg(test)]
mod tests;
mod yandex;

pub use baidu::BaiduNetdisk;
pub use config::ProviderConfig;
pub use credential::*;
pub use google::GoogleDrive;
pub use model::*;
pub use rclone::{Rclone, RcloneConfig};
pub use yandex::YandexDisk;

use async_trait::async_trait;
use tokio::io::AsyncRead;
use tokio_util::sync::CancellationToken;

/// Source positioned at byte zero. It is never buffered into a complete temporary file.
pub type UploadSource = Box<dyn AsyncRead + Unpin + Send>;

/// Durable private checkpoint boundary. A successful save must survive daemon restart.
/// Implementations may also reject a save when approval, grant, or kill-switch state changed.
#[async_trait]
pub trait CheckpointSink: Send + Sync {
    /// Persist before the next remote mutation. Never publish this object as an event or log.
    async fn save(&self, checkpoint: &PrivateCheckpoint) -> CloudResult<()>;
}

/// A configured cloud account. Credentials are supplied explicitly, not discovered implicitly.
#[async_trait]
pub trait CloudProvider: Send + Sync {
    /// Provider and implemented mode capabilities.
    fn capabilities(&self) -> ProviderCapabilities;

    /// Upload from an already authorized source. Staged mode requires a prior complete local
    /// verification by the caller; stream mode verifies incrementally and has no local fallback.
    async fn upload(
        &self,
        request: &UploadRequest,
        source: UploadSource,
        checkpoint: Option<PrivateCheckpoint>,
        sink: &dyn CheckpointSink,
        cancel: &CancellationToken,
    ) -> CloudResult<RemoteArtifact>;

    /// Ask the provider to retrieve the URL. No source bytes pass through this process.
    async fn import_url(
        &self,
        _request: &ImportRequest,
        _sink: &dyn CheckpointSink,
        _cancel: &CancellationToken,
    ) -> CloudResult<PrivateCheckpoint> {
        Err(CloudError::Unsupported(
            CapabilityReason::RemoteImportUnavailable,
        ))
    }

    /// Poll a provider-side operation using a checkpoint previously issued by this provider.
    async fn poll_import(
        &self,
        _checkpoint: &PrivateCheckpoint,
        _cancel: &CancellationToken,
    ) -> CloudResult<ImportStatus> {
        Err(CloudError::Unsupported(
            CapabilityReason::RemoteImportUnavailable,
        ))
    }

    /// Cancel a provider-side operation, only where an official cancellation API exists.
    async fn cancel_import(
        &self,
        _checkpoint: &PrivateCheckpoint,
        _cancel: &CancellationToken,
    ) -> CloudResult<()> {
        Err(CloudError::Unsupported(
            CapabilityReason::RemoteCancellationUnavailable,
        ))
    }

    /// Inspect the unique reserved object after an ambiguous mutation. Absence is not permission
    /// to restart an asynchronous import; it may still be running at the provider.
    async fn reconcile(
        &self,
        checkpoint: &PrivateCheckpoint,
        cancel: &CancellationToken,
    ) -> CloudResult<Option<RemoteArtifact>>;

    /// Remove only the operation's unique object. Does not cancel a running remote import.
    async fn cleanup(
        &self,
        checkpoint: &PrivateCheckpoint,
        cancel: &CancellationToken,
    ) -> CloudResult<()>;
}
