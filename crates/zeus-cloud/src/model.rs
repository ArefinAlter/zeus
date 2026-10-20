use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Result with diagnostics that never contain provider bodies, URLs or credentials.
pub type CloudResult<T> = Result<T, CloudError>;

/// Provider failures deliberately omit third-party error strings.
#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    /// Mode cannot satisfy the requested contract.
    #[error("cloud mode unsupported: {0:?}")]
    Unsupported(CapabilityReason),
    /// Invalid local configuration or request.
    #[error("invalid cloud request")]
    InvalidRequest,
    /// Credential could not be resolved.
    #[error("cloud credential unavailable")]
    CredentialUnavailable,
    /// Connection/timeout failure; remote mutation may already have occurred.
    #[error("cloud transport failed; reconcile the saved checkpoint")]
    Transport,
    /// Sanitized status only.
    #[error("cloud provider returned HTTP {0}")]
    Http(u16),
    /// Provider's numeric business error, without its untrusted message or request URL.
    #[error("cloud provider error {0}")]
    Provider(i64),
    /// Unexpected provider protocol response.
    #[error("invalid cloud provider response")]
    Protocol,
    /// Provider supplied an untrusted upload/operation location.
    #[error("cloud provider location refused")]
    UntrustedLocation,
    /// Source failed without exposing its original error.
    #[error("cloud source read failed")]
    Source,
    /// Source or remote size differs from the exact approved size.
    #[error("cloud content size mismatch")]
    SizeMismatch,
    /// Hash differs from the expected or locally observed content.
    #[error("cloud content hash mismatch")]
    HashMismatch,
    /// A checkpoint is bound to a different immutable operation.
    #[error("cloud checkpoint binding mismatch")]
    CheckpointMismatch,
    /// Cancellation leaves a checkpoint for cleanup/reconciliation.
    #[error("cloud operation cancelled; reconcile or clean up its checkpoint")]
    Cancelled,
    /// Existing object must never be overwritten.
    #[error("cloud destination already exists")]
    AlreadyExists,
    /// An ambiguous mutation cannot be replayed safely.
    #[error("cloud operation requires reconciliation before retry")]
    ReconciliationRequired,
    /// Checkpoint persistence refused.
    #[error("cloud checkpoint could not be persisted")]
    Checkpoint,
    /// External adapter failed; output is never surfaced.
    #[error("cloud subprocess failed")]
    Process,
    /// A bounded response/output exceeded its cap.
    #[error("cloud response exceeds the configured limit")]
    ResponseTooLarge,
}

/// Implemented native providers and the explicitly configured external adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Native Drive v3 resumable upload.
    GoogleDrive,
    /// Native Disk REST upload and URL import.
    YandexDisk,
    /// Explicitly configured rclone remote.
    Rclone,
    /// Native Baidu Netdisk multipart upload with an approved chunk manifest.
    BaiduNetdisk,
}

/// User-selected data path. Unsupported modes are never silently substituted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    /// Download to a local file, verify completely, then upload.
    StagedUpload,
    /// Read through Zeus with bounded buffers and no full local cache.
    StreamUpload,
    /// Cloud provider fetches the approved URL itself.
    RemoteImport,
}

/// Stable, actionable capability reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityReason {
    /// Provider has no verified URL-import implementation.
    RemoteImportUnavailable,
    /// No official remote-operation cancellation implementation exists.
    RemoteCancellationUnavailable,
    /// This implementation requires exact content length.
    KnownSizeRequired,
    /// Baidu's upload protocol requires all 4 MiB block MD5 values before starting.
    KnownBlockHashesRequired,
    /// The caller must supply the digest of its verified local file.
    StagedDigestRequired,
    /// This data path cannot replay bytes after restart.
    ResumeUnavailable,
    /// Native implementation is not available.
    ProviderUnavailable,
    /// Explicit provider feature discovery/configuration is required.
    BackendNotReady,
    /// A promised expected hash cannot be checked from the provider's returned metadata.
    RemoteHashUnavailable,
}

/// Advertised implementation capabilities, not inferred product marketing features.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderCapabilities {
    /// Provider implementation.
    pub provider: ProviderKind,
    /// Local verified upload supported.
    pub staged_upload: bool,
    /// Bounded forward-only upload supported.
    pub stream_upload: bool,
    /// Provider can retrieve a source URL.
    pub remote_import: bool,
    /// Durable staged upload may resume at confirmed provider offsets.
    pub resumable_upload: bool,
    /// Remote import can be cancelled at the provider.
    pub remote_cancel: bool,
    /// The provider can enforce a hard imported-byte ceiling before downloading.
    /// False means the daemon can only check size after the provider finishes.
    pub remote_import_hard_byte_limit: bool,
    /// Remote SHA-256 may be reported; receipts state whether it actually was.
    pub remote_sha256: bool,
}

impl ProviderCapabilities {
    /// Validate before reading source bytes or mutating the cloud.
    pub fn plan(
        &self,
        mode: DeliveryMode,
        size: Option<u64>,
        digest: Option<&str>,
        resume: bool,
    ) -> CloudResult<()> {
        let supported = match mode {
            DeliveryMode::StagedUpload => self.staged_upload,
            DeliveryMode::StreamUpload => self.stream_upload,
            DeliveryMode::RemoteImport => self.remote_import,
        };
        if !supported {
            return Err(CloudError::Unsupported(
                if mode == DeliveryMode::RemoteImport {
                    CapabilityReason::RemoteImportUnavailable
                } else {
                    CapabilityReason::ProviderUnavailable
                },
            ));
        }
        if size.is_none() {
            return Err(CloudError::Unsupported(CapabilityReason::KnownSizeRequired));
        }
        if mode == DeliveryMode::StagedUpload && digest.is_none() {
            return Err(CloudError::Unsupported(
                CapabilityReason::StagedDigestRequired,
            ));
        }
        if resume && (mode != DeliveryMode::StagedUpload || !self.resumable_upload) {
            return Err(CloudError::Unsupported(CapabilityReason::ResumeUnavailable));
        }
        if let Some(hash) = digest {
            validate_hash(hash)?;
        }
        Ok(())
    }
}

/// Immutable, approval-bound destination and source metadata.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UploadRequest {
    /// Random durable operation identifier (at least 16 ASCII alphanumeric characters).
    pub operation_id: String,
    /// Explicit data path.
    pub mode: DeliveryMode,
    /// Filename only. Path separators and control characters are rejected.
    pub name: String,
    /// Exact approved source length.
    pub size: u64,
    /// Expected SHA-256; required for staged mode.
    pub expected_sha256: Option<String>,
    /// Baidu protocol chunk manifest; SHA-256 remains the content integrity digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_md5: Option<Vec<String>>,
}

impl UploadRequest {
    /// Validate names, bounds and digest syntax.
    pub fn validate(&self) -> CloudResult<()> {
        if self.operation_id.len() < 16
            || self.operation_id.len() > 96
            || !self
                .operation_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || self.name.is_empty()
            || self.name.len() > 200
            || self.name == "."
            || self.name == ".."
            || self
                .name
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '?' | '#'))
            || self.size > i64::MAX as u64
        {
            return Err(CloudError::InvalidRequest);
        }
        if let Some(hash) = &self.expected_sha256 {
            validate_hash(hash)?;
        }
        Ok(())
    }

    /// Collision-resistant object name. Zeus never replaces a user-selected existing path.
    pub fn unique_name(&self) -> String {
        format!("{}.zeus-{}", self.name, self.operation_id)
    }
}

/// Secret source URL is deliberately absent from serialization and Debug output.
pub struct ImportRequest {
    /// The destination identity and expected metadata; mode must be RemoteImport.
    pub upload: UploadRequest,
    /// Approved public HTTPS URL, possibly a signed URL whose disclosure was approved.
    pub source_url: crate::SecretString,
}

impl std::fmt::Debug for ImportRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportRequest")
            .field("upload", &self.upload)
            .field("source_url", &"[REDACTED]")
            .finish()
    }
}

/// Durable phase; Saved before mutation, never inferred from a missing acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointPhase {
    /// Unique identity reserved; mutation has not yet been acknowledged.
    Prepared,
    /// Upload session exists or streaming upload was started.
    Uploading,
    /// Provider is asynchronously fetching a URL.
    ImportPending,
    /// Remote object was reconciled and verified to the documented level.
    Complete,
}

/// PRIVATE daemon storage only. Contains bearer upload-session/operation URLs.
/// This type intentionally has no JsonSchema/public API projection and redacts its entire Debug.
#[derive(Clone, Serialize, Deserialize)]
pub struct PrivateCheckpoint {
    /// Provider identity.
    pub provider: ProviderKind,
    /// Exact request binding.
    pub request: UploadRequest,
    /// Unique provider object path, or Drive parent/name identity.
    pub object_path: String,
    /// Preallocated Drive ID or provider resource ID if known.
    pub object_id: Option<String>,
    /// PRIVATE upload session or operation URL. Never expose through API/event/audit output.
    pub session_url: Option<String>,
    /// Confirmed durable provider byte offset.
    pub committed_bytes: u64,
    /// Recovery phase.
    pub phase: CheckpointPhase,
    /// Provider-private recovery metadata, such as a multipart upload ID. Never public.
    #[serde(default)]
    pub provider_state: serde_json::Value,
}

impl std::fmt::Debug for PrivateCheckpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrivateCheckpoint([REDACTED])")
    }
}

impl PrivateCheckpoint {
    pub(crate) fn validate_binding(
        &self,
        kind: ProviderKind,
        request: &UploadRequest,
        path: &str,
    ) -> CloudResult<()> {
        if self.provider != kind
            || self.object_path != path
            || self.request.operation_id != request.operation_id
            || self.request.mode != request.mode
            || self.request.name != request.name
            || self.request.size != request.size
            || self.request.expected_sha256 != request.expected_sha256
            || self.request.block_md5 != request.block_md5
            || self.committed_bytes > request.size
        {
            return Err(CloudError::CheckpointMismatch);
        }
        Ok(())
    }
}

/// Actual evidence obtained; a local hash alone is never described as a remote hash match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerificationEvidence {
    /// Local bytes hashed and provider SHA-256 matched.
    LocalAndRemoteSha256,
    /// Local bytes hashed; only provider size was available.
    LocalSha256RemoteSize,
    /// Expected hash matched the provider hash without reading source bytes locally.
    ExpectedAndRemoteSha256,
    /// Only provider-reported size was checked.
    RemoteSizeOnly,
}

/// Public remote artifact evidence. No URLs, credentials or upload sessions.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RemoteArtifact {
    /// Provider implementation.
    pub provider: ProviderKind,
    /// Durable operation identifier.
    pub operation_id: String,
    /// Opaque provider object ID, or unique reserved path for path-based providers.
    pub object_id: String,
    /// Unique destination path/name selected for this operation.
    pub object_path: String,
    /// Exact provider-reported bytes.
    pub size: u64,
    /// Locally observed stream SHA-256, absent for provider-only import.
    pub local_sha256: Option<String>,
    /// Actual provider-reported SHA-256, absent where unsupported/unavailable.
    pub remote_sha256: Option<String>,
    /// Strength of evidence actually obtained.
    pub verification: VerificationEvidence,
}

/// Asynchronous import state.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ImportStatus {
    /// Still running at the provider.
    Pending,
    /// Finished and destination metadata checked.
    Complete {
        /// Reconciled object.
        artifact: RemoteArtifact,
    },
    /// Provider reported failure, without leaking its remote error string.
    Failed,
}

pub(crate) fn validate_hash(hash: &str) -> CloudResult<()> {
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(CloudError::InvalidRequest);
    }
    Ok(())
}

pub(crate) fn receipt(
    cp: &PrivateCheckpoint,
    id: String,
    size: u64,
    remote: Option<String>,
    local: Option<String>,
) -> CloudResult<RemoteArtifact> {
    if size != cp.request.size {
        return Err(CloudError::SizeMismatch);
    }
    if let Some(hash) = &remote {
        validate_hash(hash).map_err(|_| CloudError::Protocol)?;
    }
    if cp.request.expected_sha256.is_some() && local.is_none() && remote.is_none() {
        return Err(CloudError::Unsupported(
            CapabilityReason::RemoteHashUnavailable,
        ));
    }
    if let (Some(expected), Some(observed)) = (
        &cp.request.expected_sha256,
        local.as_ref().or(remote.as_ref()),
    ) && !expected.eq_ignore_ascii_case(observed)
    {
        return Err(CloudError::HashMismatch);
    }
    if let (Some(expected), Some(remote)) = (
        local.as_ref().or(cp.request.expected_sha256.as_ref()),
        remote.as_ref(),
    ) && !expected.eq_ignore_ascii_case(remote)
    {
        return Err(CloudError::HashMismatch);
    }
    let verification = match (
        local.is_some(),
        remote.is_some(),
        cp.request.expected_sha256.is_some(),
    ) {
        (true, true, _) => VerificationEvidence::LocalAndRemoteSha256,
        (true, false, _) => VerificationEvidence::LocalSha256RemoteSize,
        (false, true, true) => VerificationEvidence::ExpectedAndRemoteSha256,
        _ => VerificationEvidence::RemoteSizeOnly,
    };
    Ok(RemoteArtifact {
        provider: cp.provider,
        operation_id: cp.request.operation_id.clone(),
        object_id: id,
        object_path: cp.object_path.clone(),
        size,
        local_sha256: local,
        remote_sha256: remote,
        verification,
    })
}
