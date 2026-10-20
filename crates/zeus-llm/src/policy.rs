//! The signed provider policy manifest.
//!
//! Scheme (ADR 0008): the manifest is JSON carried in a DSSE envelope. The signature is ed25519 over the DSSE
//! pre-authentication encoding `"DSSEv1" SP len(type) SP type SP len(payload) SP payload`, so the signed bytes are
//! exactly the payload bytes and no JSON canonicalisation is needed. A set of public keys is pinned in the binary
//! ([`PINNED_KEYS`]) so keys can rotate through releases. Rollback is blocked by a monotonic `sequence`; offline,
//! the last verified manifest applies until `expires_at`.

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, VerifyingKey};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::provider::{ProviderKind, ProviderStatus};

/// DSSE `payloadType` of the manifest.
pub const PAYLOAD_TYPE: &str = "application/vnd.zeus.policy+json;version=1";

/// Public keys pinned in this build as `(key id, raw 32-byte ed25519 public key)`. Empty in the skeleton: a build
/// without pinned keys rejects every manifest, which means "no remote policy" and keeps user-configured providers
/// working. Release builds fill this list; a rotation adds the new key one release before the old one is dropped.
pub const PINNED_KEYS: &[(&str, [u8; 32])] = &[];

/// Failure to accept a manifest.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    /// The envelope has a different `payloadType`.
    #[error("unexpected payload type")]
    PayloadType,
    /// No signature verified under a trusted key.
    #[error("no valid signature from a trusted key")]
    Signature,
    /// The payload is not a valid manifest.
    #[error("malformed manifest: {0}")]
    Malformed(String),
    /// The manifest's schema version is newer than this build understands.
    #[error("unsupported manifest schema version {0}")]
    SchemaVersion(u32),
    /// The sequence number is lower than one already accepted (rollback).
    #[error("manifest sequence {got} is older than accepted sequence {have}")]
    Rollback {
        /// Sequence of the offered manifest.
        got: u64,
        /// Highest sequence accepted so far.
        have: u64,
    },
    /// The manifest has expired.
    #[error("manifest expired")]
    Expired,
    /// The manifest is not valid yet (issued in the future beyond tolerated skew).
    #[error("manifest not yet valid")]
    NotYetValid,
}

/// Per-provider entry of the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderPolicy {
    /// `enabled`, `experimental` or `disabled`.
    pub status: ProviderStatus,
    /// Human-readable reason (shown by `zeus auth`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Page documenting the status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// When the status was last verified (RFC 3339 UTC).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verified: Option<String>,
}

/// The policy document. All numbers are integers and instants are RFC 3339 strings (00 section 7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PolicyManifest {
    /// Manifest schema version; this build understands `1`.
    pub schema_version: u32,
    /// Monotonic sequence; a lower value than one already accepted is rejected.
    pub sequence: u64,
    /// When the manifest was issued (RFC 3339 UTC).
    pub issued_at: String,
    /// When it stops applying (RFC 3339 UTC).
    pub expires_at: String,
    /// Minimum Zeus version allowed to use remote-model features; older builds disable them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_zeus_version: Option<String>,
    /// Status per provider. Absent providers are treated as enabled.
    pub providers: BTreeMap<ProviderKind, ProviderPolicy>,
    /// Named kill switches (for example `all_remote_models`).
    #[serde(default)]
    pub kill_switches: BTreeSet<String>,
}

impl PolicyManifest {
    /// Provider statuses in the form the [`crate::ladder::Ladder`] consumes.
    pub fn status_map(&self) -> crate::provider::StatusMap {
        self.providers.iter().map(|(k, v)| (*k, v.status)).collect()
    }
}

/// A DSSE signature entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DsseSignature {
    /// Key id the signer claims.
    pub keyid: String,
    /// Standard base64 of the 64-byte ed25519 signature.
    pub sig: String,
}

/// The DSSE envelope carrying the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PolicyEnvelope {
    /// Must equal [`PAYLOAD_TYPE`].
    pub payload_type: String,
    /// Standard base64 of the manifest JSON bytes.
    pub payload: String,
    /// Signatures.
    pub signatures: Vec<DsseSignature>,
}

/// DSSE pre-authentication encoding of `(payload_type, payload)`.
pub fn pae(payload_type: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + payload_type.len() + 32);
    out.extend_from_slice(b"DSSEv1 ");
    out.extend_from_slice(payload_type.len().to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload_type.as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload.len().to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload);
    out
}

/// The set of public keys a manifest may be signed with.
#[derive(Debug, Clone, Default)]
pub struct TrustedKeys {
    keys: Vec<(String, VerifyingKey)>,
}

impl TrustedKeys {
    /// The keys pinned in this binary ([`PINNED_KEYS`]).
    pub fn pinned() -> Self {
        let mut trusted = Self::default();
        for (id, bytes) in PINNED_KEYS {
            // A malformed pinned key is a build bug; skipping it fails closed (fewer trusted keys).
            let _ = trusted.add(id, *bytes);
        }
        trusted
    }

    /// Add a key. Used by tests and by release tooling.
    pub fn add(&mut self, key_id: &str, public_key: [u8; 32]) -> Result<(), PolicyError> {
        let key = VerifyingKey::from_bytes(&public_key).map_err(|_| PolicyError::Signature)?;
        self.keys.push((key_id.to_string(), key));
        Ok(())
    }

    fn verify(&self, keyid: &str, message: &[u8], signature: &Signature) -> bool {
        self.keys
            .iter()
            .filter(|(id, _)| id == keyid)
            .any(|(_, key)| key.verify_strict(message, signature).is_ok())
    }
}

/// Maximum tolerated clock skew for `issued_at` in the future.
const MAX_SKEW_SECONDS: i64 = 24 * 3600;

/// Verify an envelope and return the manifest it carries.
///
/// `highest_accepted_sequence` is the highest sequence this installation has ever accepted (0 for none);
/// a manifest with a lower sequence is a rollback and is rejected, an equal one is accepted so a stored manifest
/// can be re-verified at startup.
pub fn verify_envelope(
    envelope: &PolicyEnvelope,
    trusted: &TrustedKeys,
    now: OffsetDateTime,
    highest_accepted_sequence: u64,
) -> Result<PolicyManifest, PolicyError> {
    if envelope.payload.len() > 350_000 || envelope.signatures.len() > 16 {
        return Err(PolicyError::Malformed("policy too large".into()));
    }
    if envelope.payload_type != PAYLOAD_TYPE {
        return Err(PolicyError::PayloadType);
    }
    let payload = STANDARD
        .decode(&envelope.payload)
        .map_err(|e| PolicyError::Malformed(format!("payload is not base64: {e}")))?;
    let message = pae(&envelope.payload_type, &payload);
    let verified = envelope.signatures.iter().any(|entry| {
        let Ok(raw) = STANDARD.decode(&entry.sig) else {
            return false;
        };
        let Ok(bytes) = <[u8; 64]>::try_from(raw.as_slice()) else {
            return false;
        };
        trusted.verify(&entry.keyid, &message, &Signature::from_bytes(&bytes))
    });
    if !verified {
        return Err(PolicyError::Signature);
    }
    let manifest: PolicyManifest =
        serde_json::from_slice(&payload).map_err(|e| PolicyError::Malformed(e.to_string()))?;
    if manifest.schema_version != 1 {
        return Err(PolicyError::SchemaVersion(manifest.schema_version));
    }
    if manifest.sequence < highest_accepted_sequence {
        return Err(PolicyError::Rollback {
            got: manifest.sequence,
            have: highest_accepted_sequence,
        });
    }
    let expires = OffsetDateTime::parse(&manifest.expires_at, &Rfc3339)
        .map_err(|e| PolicyError::Malformed(format!("expires_at: {e}")))?;
    let issued = OffsetDateTime::parse(&manifest.issued_at, &Rfc3339)
        .map_err(|e| PolicyError::Malformed(format!("issued_at: {e}")))?;
    if expires <= issued {
        return Err(PolicyError::Malformed(
            "expiry must follow issue time".into(),
        ));
    }
    if expires <= now {
        return Err(PolicyError::Expired);
    }
    if issued.unix_timestamp() > now.unix_timestamp() + MAX_SKEW_SECONDS {
        return Err(PolicyError::NotYetValid);
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn trusted() -> TrustedKeys {
        let mut t = TrustedKeys::default();
        t.add("k1", key().verifying_key().to_bytes()).unwrap();
        t
    }

    fn manifest(sequence: u64) -> PolicyManifest {
        let mut providers = BTreeMap::new();
        providers.insert(
            ProviderKind::CliDelegate,
            ProviderPolicy {
                status: ProviderStatus::Experimental,
                reason: Some("terms under review".into()),
                url: None,
                last_verified: Some("2026-10-05T00:00:00Z".into()),
            },
        );
        PolicyManifest {
            schema_version: 1,
            sequence,
            issued_at: "2026-10-01T00:00:00Z".into(),
            expires_at: "2026-12-01T00:00:00Z".into(),
            min_zeus_version: Some("0.1.0".into()),
            providers,
            kill_switches: BTreeSet::new(),
        }
    }

    fn sign(manifest: &PolicyManifest, keyid: &str) -> PolicyEnvelope {
        let payload = serde_json::to_vec(manifest).unwrap();
        let signature = key().sign(&pae(PAYLOAD_TYPE, &payload));
        PolicyEnvelope {
            payload_type: PAYLOAD_TYPE.into(),
            payload: STANDARD.encode(&payload),
            signatures: vec![DsseSignature {
                keyid: keyid.into(),
                sig: STANDARD.encode(signature.to_bytes()),
            }],
        }
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::parse("2026-10-05T12:00:00Z", &Rfc3339).unwrap()
    }

    #[test]
    fn accepts_a_valid_manifest() {
        let envelope = sign(&manifest(5), "k1");
        let accepted = verify_envelope(&envelope, &trusted(), now(), 5).unwrap();
        assert_eq!(accepted.sequence, 5);
        assert_eq!(
            accepted.status_map()[&ProviderKind::CliDelegate],
            ProviderStatus::Experimental
        );
    }

    #[test]
    fn rejects_unknown_key_or_tampered_payload() {
        let envelope = sign(&manifest(5), "other");
        assert_eq!(
            verify_envelope(&envelope, &trusted(), now(), 0).unwrap_err(),
            PolicyError::Signature
        );

        let mut tampered = sign(&manifest(5), "k1");
        let mut bytes = STANDARD.decode(&tampered.payload).unwrap();
        bytes[2] ^= 0x01;
        tampered.payload = STANDARD.encode(bytes);
        assert_eq!(
            verify_envelope(&tampered, &trusted(), now(), 0).unwrap_err(),
            PolicyError::Signature
        );
    }

    #[test]
    fn rejects_rollback_and_expiry() {
        let envelope = sign(&manifest(4), "k1");
        assert_eq!(
            verify_envelope(&envelope, &trusted(), now(), 5).unwrap_err(),
            PolicyError::Rollback { got: 4, have: 5 }
        );
        let late = OffsetDateTime::parse("2027-01-01T00:00:00Z", &Rfc3339).unwrap();
        assert_eq!(
            verify_envelope(&envelope, &trusted(), late, 0).unwrap_err(),
            PolicyError::Expired
        );
    }

    #[test]
    fn rejects_wrong_payload_type_and_future_schema() {
        let mut envelope = sign(&manifest(1), "k1");
        envelope.payload_type = "text/plain".into();
        assert_eq!(
            verify_envelope(&envelope, &trusted(), now(), 0).unwrap_err(),
            PolicyError::PayloadType
        );

        let mut future = manifest(1);
        future.schema_version = 2;
        let envelope = sign(&future, "k1");
        assert_eq!(
            verify_envelope(&envelope, &trusted(), now(), 0).unwrap_err(),
            PolicyError::SchemaVersion(2)
        );
    }

    #[test]
    fn build_without_pinned_keys_trusts_nothing() {
        let envelope = sign(&manifest(1), "k1");
        assert_eq!(
            verify_envelope(&envelope, &TrustedKeys::pinned(), now(), 0).unwrap_err(),
            PolicyError::Signature
        );
    }

    #[test]
    fn pae_matches_the_dsse_spec_example_shape() {
        assert_eq!(
            pae("http://example.com/HelloWorld", b"hello world"),
            b"DSSEv1 29 http://example.com/HelloWorld 11 hello world".to_vec()
        );
    }
}
