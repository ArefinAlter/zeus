use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{SourceError, SourceResult};

/// A secret configured explicitly by the operator. Never serializable.
#[derive(Clone)]
pub struct SourceSecret(Zeroizing<String>);
impl SourceSecret {
    /// Wrap an explicitly supplied value without ambient credential discovery.
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }
    /// Read only while preparing an authenticated request.
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for SourceSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceSecret([REDACTED])")
    }
}

/// Immutable object selection. No bearer URL or authorization header exists here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceTarget {
    /// Amazon S3 object. Review pins a version or a strong entity validator.
    S3 {
        /// DNS-compatible bucket name, never an endpoint or URL.
        bucket: String,
        /// Exact object key within the configured account prefix.
        key: String,
        /// Explicit object version, when versioning is used.
        version_id: Option<String>,
    },
    /// One immutable Hugging Face repository file.
    HuggingFace {
        /// Repository family.
        repository_type: HubRepositoryType,
        /// Exactly owner/repository.
        repository: String,
        /// Full forty-hex Git commit, never a moving branch/tag.
        commit: String,
        /// Exact file path in that commit.
        path: String,
    },
    /// A reviewed metainfo file, retained privately by the daemon.
    Torrent {
        /// SHA-256 of the metainfo bytes; not an arbitrary local path.
        metainfo_sha256: String,
        /// Exact selected file indices from the bounded review.
        file_indices: Vec<usize>,
        /// Explicit peer IP:port endpoints inside the configured IP allowlist.
        initial_peers: Vec<String>,
    },
}

/// Hub URL family. These have distinct canonical resolve prefixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HubRepositoryType {
    /// Models use the root repository namespace.
    Model,
    /// Datasets use /datasets/.
    Dataset,
    /// Spaces use /spaces/.
    Space,
}

/// Public evidence for an immutable reviewed object, without signed CDN addresses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectMetadata {
    /// Exact complete object length.
    pub bytes: u64,
    /// Strong entity validator. It is not automatically a content digest.
    pub strong_etag: Option<String>,
    /// Publisher-declared whole-object SHA-256, only when its semantics are known.
    pub sha256: Option<String>,
    /// Exact provider version or Hub commit pinned during review.
    pub immutable_revision: String,
    /// Plain description of how the digest was interpreted.
    pub integrity_kind: String,
}

/// Explicit account scope. Human configuration separately binds who may use it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceAccountConfig {
    /// Amazon's native HTTPS S3 API; arbitrary endpoints are unavailable.
    S3 {
        /// AWS signing region, e.g. eu-west-1.
        region: String,
        /// One allowed bucket.
        bucket: String,
        /// Exact key prefix. Empty allows the complete configured bucket.
        key_prefix: String,
    },
    /// Canonical Hub HTTPS API, restricted to one repository.
    HuggingFace {
        /// Repository family.
        repository_type: HubRepositoryType,
        /// Exactly owner/repository.
        repository: String,
    },
    /// Torrent metadata requires explicit peer/tracker network review.
    Torrent {
        /// Maximum peer connections, one through thirty-two.
        max_peers: u16,
        /// Receive-rate ceiling. Zero/unlimited is unavailable.
        max_download_bytes_per_second: u64,
        /// Exact IPv4/IPv6 peer addresses. Discovery cannot escape these IPs.
        allowed_peer_ips: Vec<String>,
    },
}

/// A whole task's cumulative network budget, including retry bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceBudget {
    /// At most one TiB; not merely the final file's length.
    pub max_received_bytes: u64,
    /// At most twenty-four hours, measured from reviewed operation creation.
    pub timeout_ms: u64,
}
impl SourceBudget {
    /// Refuse unbounded receive/time authority.
    pub fn validate(self) -> SourceResult<()> {
        if self.max_received_bytes == 0
            || self.max_received_bytes > 1 << 40
            || self.timeout_ms == 0
            || self.timeout_ms > 86_400_000
        {
            return Err(SourceError::Invalid);
        }
        Ok(())
    }
}

#[cfg(any(feature = "s3", feature = "hugging-face"))]
pub(crate) fn relative_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.starts_with('/')
        && !value.contains(['\\', '\0', '\r', '\n'])
        && value
            .split('/')
            .all(|part| !part.is_empty() && !matches!(part, "." | ".."))
}

#[cfg(any(feature = "s3", feature = "hugging-face"))]
pub(crate) fn header_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.bytes().all(|byte| matches!(byte, 0x21..=0x7e))
}
