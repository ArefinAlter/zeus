//! Native verifier shared by the desktop and daemon. Team policy can only remove authority.
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// The team restriction signature domain; provenance is deliberately a different domain.
pub const POLICY_TYPE: &str = "application/vnd.verne.zeus.team-policy+json;version=1";
/// The signed, client-reported artifact assertion domain.
pub const PROVENANCE_TYPE: &str = "application/vnd.verne.zeus.team-provenance+json;version=1";
/// Administrator-provisioned trust, never downloaded or writable through an app route.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    /// HTTPS origin, with no path or user information.
    pub origin: String,
    /// The sole tenant this installation may join.
    pub tenant_id: String,
    /// Explicitly pinned raw Ed25519 public keys (64 hexadecimal characters).
    pub keys: BTreeMap<String, String>,
}
/// Strict DSSE envelope.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    #[serde(rename = "payloadType")]
    payload_type: String,
    payload: String,
    signatures: Vec<SignatureEntry>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SignatureEntry {
    keyid: String,
    sig: String,
}
/// Supported restrictions. A true value preserves existing local authorization requirements.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Features {
    /// New download effects.
    pub downloads: bool,
    /// New remote delivery effects.
    pub cloud_delivery: bool,
    /// New workflow effects.
    pub workflows: bool,
    /// Explicit metadata synchronization.
    pub metadata_sync: bool,
    /// Client-reported signed provenance.
    pub provenance: bool,
}
/// Verified lease; private fields cannot be supplied by the page as authority.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    schema_version: u32,
    issuer: String,
    tenant_id: String,
    /// Monotonic service lease sequence.
    pub sequence: u64,
    issued_at: String,
    /// Lease expiration displayed to the person.
    pub expires_at: String,
    min_policy_level: u32,
    /// Only these known feature restrictions are applied.
    pub features: Features,
}
/// Sanitized fail-closed verifier error. It never contains remote text or credentials.
#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("Team policy is unavailable, expired, untrusted, or does not allow this operation.")]
pub struct Denied;
/// Result from native trust operations.
pub type Result<T> = std::result::Result<T, Denied>;

/// Validate a bounded identifier used in paths and public metadata.
pub fn valid_id(s: &str) -> bool {
    !["sk-", "sk_ant_", "aiza", "akia", "bearer", "zeus-canary-"]
        .iter()
        .any(|p| s.to_ascii_lowercase().starts_with(p))
        && !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        && s.as_bytes()[0].is_ascii_alphanumeric()
}
impl Pin {
    /// Parse and validate administrator trust without network access.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 16384 {
            return Err(Denied);
        }
        let pin: Self = serde_json::from_slice(bytes).map_err(|_| Denied)?;
        let u = url::Url::parse(&pin.origin).map_err(|_| Denied)?;
        if u.scheme() != "https"
            || !u.username().is_empty()
            || u.password().is_some()
            || u.query().is_some()
            || u.fragment().is_some()
            || u.path() != "/"
            || u.origin().ascii_serialization() != pin.origin
            || !valid_id(&pin.tenant_id)
            || pin.keys.is_empty()
            || pin.keys.len() > 8
        {
            return Err(Denied);
        }
        for (id, key) in &pin.keys {
            if !valid_id(id) {
                return Err(Denied);
            }
            let _: [u8; 32] = hex::decode(key)
                .map_err(|_| Denied)?
                .try_into()
                .map_err(|_| Denied)?;
        }
        Ok(pin)
    }
}
fn decoded(s: &str, max: usize) -> Result<Vec<u8>> {
    if s.len() > max * 2 {
        return Err(Denied);
    }
    let b = STANDARD.decode(s).map_err(|_| Denied)?;
    if b.len() > max || STANDARD.encode(&b) != s {
        return Err(Denied);
    }
    Ok(b)
}
/// Verify the exact DSSE domain and explicitly pinned key before parsing any signed content.
pub fn verify(pin: &Pin, envelope: &Envelope, domain: &str) -> Result<Value> {
    if envelope.payload_type != domain
        || envelope.signatures.is_empty()
        || envelope.signatures.len() > 8
    {
        return Err(Denied);
    }
    let bytes = decoded(&envelope.payload, 65536)?;
    let mut message = format!("DSSEv1 {} {} {} ", domain.len(), domain, bytes.len()).into_bytes();
    message.extend_from_slice(&bytes);
    let valid = envelope.signatures.iter().any(|s| {
        let Some(raw) = pin.keys.get(&s.keyid) else {
            return false;
        };
        let Ok(key_bytes) = hex::decode(raw).and_then(|v| {
            <[u8; 32]>::try_from(v).map_err(|_| hex::FromHexError::InvalidStringLength)
        }) else {
            return false;
        };
        let Ok(key) = VerifyingKey::from_bytes(&key_bytes) else {
            return false;
        };
        let Ok(sig) = decoded(&s.sig, 64).and_then(|v| <[u8; 64]>::try_from(v).map_err(|_| Denied))
        else {
            return false;
        };
        key.verify_strict(&message, &Signature::from_bytes(&sig))
            .is_ok()
    });
    if !valid {
        return Err(Denied);
    }
    let v: Value = serde_json::from_slice(&bytes).map_err(|_| Denied)?;
    if v.get("issuer").and_then(Value::as_str) != Some(&pin.origin)
        || v.get("tenant_id").and_then(Value::as_str) != Some(&pin.tenant_id)
        || v.get("schema_version").and_then(Value::as_u64) != Some(1)
    {
        return Err(Denied);
    }
    Ok(v)
}
/// Current UTC clock for policy validation.
pub fn now_ms() -> i64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}
fn millis(value: &str) -> Result<i64> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map(|v| (v.unix_timestamp_nanos() / 1_000_000) as i64)
        .map_err(|_| Denied)
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Watermark {
    sequence: u64,
    payload_hash: String,
    clock_ms: i64,
}
fn policy(
    pin: &Pin,
    envelope: &Envelope,
    floor: &Watermark,
    now: i64,
) -> Result<(Policy, Watermark)> {
    let value = verify(pin, envelope, POLICY_TYPE)?;
    let policy: Policy = serde_json::from_value(value).map_err(|_| Denied)?;
    let issued = millis(&policy.issued_at)?;
    let expires = millis(&policy.expires_at)?;
    let hash = hex::encode(Sha256::digest(envelope.payload.as_bytes()));
    if policy.min_policy_level != 1
        || policy.sequence == 0
        || policy.sequence > 9_007_199_254_740_991
        || expires <= now
        || issued > now.saturating_add(30_000)
        || expires <= issued
        || expires - issued > 86_400_000
        || now < floor.clock_ms.saturating_sub(30_000)
        || policy.sequence < floor.sequence
        || (policy.sequence == floor.sequence && hash != floor.payload_hash)
    {
        return Err(Denied);
    }
    let next = Watermark {
        sequence: policy.sequence,
        payload_hash: hash,
        clock_ms: now.max(floor.clock_ms),
    };
    Ok((policy, next))
}
fn bounded_file(path: &Path, cap: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    // Open the reparse entry itself; a linked file is rejected by metadata on the opened handle.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path).map_err(|_| Denied)?;
    let meta = file.metadata().map_err(|_| Denied)?;
    if !meta.is_file() || meta.len() > cap {
        return Err(Denied);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err(Denied);
        }
    }
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| Denied)?;
    if bytes.len() as u64 > cap {
        return Err(Denied);
    }
    Ok(bytes)
}
/// The local trust snapshot and durable anti-rollback state. Removing a pin while running cannot disable it.
pub struct Guard {
    pin: Option<std::result::Result<Pin, Denied>>,
    dir: PathBuf,
    watermark_path: PathBuf,
    watermark: Mutex<std::result::Result<Watermark, Denied>>,
}
impl Guard {
    /// Load pin.json once; every covered effect rechecks policy.json and expiry. `role` names separate per-process watermarks.
    pub fn new(state_dir: &Path, role: &str) -> Self {
        let dir = state_dir.join("team");
        let path = dir.join("pin.json");
        let pin = match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            _ => Some(bounded_file(&path, 16384).and_then(|v| Pin::parse(&v))),
        };
        let watermark_path = dir.join(format!("{role}-watermark.json"));
        let watermark = match std::fs::symlink_metadata(&watermark_path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Watermark::default()),
            _ => bounded_file(&watermark_path, 2048)
                .and_then(|v| serde_json::from_slice(&v).map_err(|_| Denied)),
        };
        Self {
            pin,
            dir,
            watermark_path,
            watermark: Mutex::new(watermark),
        }
    }
    /// Whether this process started with a team pin, including an invalid pin.
    pub fn configured(&self) -> bool {
        self.pin.is_some()
    }
    /// The immutable local trust, if valid and configured.
    pub fn pin(&self) -> Result<&Pin> {
        self.pin
            .as_ref()
            .ok_or(Denied)?
            .as_ref()
            .map_err(|_| Denied)
    }
    fn check(&self, envelope: &Envelope, persist: bool) -> Result<Policy> {
        self.check_at(envelope, persist, now_ms())
    }
    fn check_at(&self, envelope: &Envelope, persist: bool, now: i64) -> Result<Policy> {
        let mut floor = self.watermark.lock().map_err(|_| Denied)?;
        let old = floor.as_ref().map_err(|_| Denied)?;
        let (p, next) = policy(self.pin()?, envelope, old, now)?;
        if next.sequence != old.sequence
            || next.clock_ms.div_euclid(30_000) != old.clock_ms.div_euclid(30_000)
        {
            atomic(
                &self.watermark_path,
                &serde_json::to_vec(&next).map_err(|_| Denied)?,
            )?;
        }
        // Preserve a durable higher watermark even if publishing policy.json subsequently fails.
        *floor = Ok(next.clone());
        if persist {
            atomic(
                &self.dir.join("policy.json"),
                &serde_json::to_vec(envelope).map_err(|_| Denied)?,
            )?;
        }
        *floor = Ok(next);
        Ok(p)
    }
    /// Accept and durably publish a newly fetched signed policy. Failure leaves no usable newer authority.
    pub fn accept(&self, envelope: &Envelope) -> Result<Policy> {
        self.check(envelope, true)
    }
    /// Re-read and verify the policy on every new covered effect; no unverified cached payload is used.
    pub fn current(&self) -> Result<Policy> {
        let bytes = bounded_file(&self.dir.join("policy.json"), 100000)?;
        let e = serde_json::from_slice(&bytes).map_err(|_| Denied)?;
        self.check(&e, false)
    }
    /// Enforce a feature restriction; installations without a pin retain their existing behavior.
    pub fn admit(&self, feature: &str) -> Result<()> {
        if !self.configured() {
            return Ok(());
        }
        let f = self.current()?.features;
        let enabled = match feature {
            "downloads" => f.downloads,
            "cloud_delivery" => f.cloud_delivery,
            "workflows" => f.workflows,
            "metadata_sync" => f.metadata_sync,
            "provenance" => f.provenance,
            _ => false,
        };
        if enabled { Ok(()) } else { Err(Denied) }
    }
}
/// Atomic replacement of non-secret team state; only the owning native components call this.
pub fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().ok_or(Denied)?;
    let tmp = parent.join(format!(
        ".team-{}.tmp",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp).map_err(|_| Denied)?;
    let result = (|| {
        file.write_all(bytes).map_err(|_| Denied)?;
        file.sync_all().map_err(|_| Denied)?;
        drop(file);
        std::fs::rename(&tmp, path).map_err(|_| Denied)?;
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|_| Denied)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}
/// Features whose new effects require a lease. Pause/cancel/read/revoke remain available.
pub fn method_features(method: &str, params: &Value) -> &'static [&'static str] {
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match method {
        "jobs_enqueue"
        | "jobs_probe"
        | "inbox_submit"
        | "inbox_submission_retry"
        | "sources_inspect"
        | "sources_submit" => &["downloads"],
        "jobs_control" | "sources_control" if !matches!(action, "pause" | "cancel") => {
            &["downloads"]
        }
        "queues_control" if !matches!(action, "pause" | "stop" | "cancel") => &["downloads"],
        "queues_put" | "queues_configure" => &["downloads"],
        "cloud_plan" | "cloud_submit" => &["cloud_delivery"],
        "cloud_control" if !matches!(action, "pause" | "cancel") => &["cloud_delivery"],
        "workflows_run" => &["workflows", "downloads"],
        "workflows_control" if !matches!(action, "pause" | "cancel") => &["workflows", "downloads"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;
    fn fixture(sequence: u64, now: i64) -> (Pin, Envelope) {
        let key = SigningKey::from_bytes(&[7; 32]);
        let pin = Pin {
            origin: "https://team.example.com".into(),
            tenant_id: "alpha".into(),
            keys: BTreeMap::from([(
                "fixture".into(),
                hex::encode(key.verifying_key().as_bytes()),
            )]),
        };
        let stamp = |t: i64| {
            OffsetDateTime::from_unix_timestamp_nanos(i128::from(t) * 1_000_000)
                .unwrap()
                .format(&Rfc3339)
                .unwrap()
        };
        let payload = serde_json::to_vec(&json!({"schema_version":1,"issuer":pin.origin,"tenant_id":pin.tenant_id,"sequence":sequence,"issued_at":stamp(now),"expires_at":stamp(now+900000),"min_policy_level":1,"features":{"downloads":false,"cloud_delivery":true,"workflows":true,"metadata_sync":true,"provenance":true}})).unwrap();
        let mut pae = format!(
            "DSSEv1 {} {} {} ",
            POLICY_TYPE.len(),
            POLICY_TYPE,
            payload.len()
        )
        .into_bytes();
        pae.extend_from_slice(&payload);
        (
            pin,
            Envelope {
                payload_type: POLICY_TYPE.into(),
                payload: STANDARD.encode(payload),
                signatures: vec![SignatureEntry {
                    keyid: "fixture".into(),
                    sig: STANDARD.encode(key.sign(&pae).to_bytes()),
                }],
            },
        )
    }
    #[test]
    fn signature_domain_tenant_expiry_and_replay_are_enforced() {
        let now = 1_800_000_000_000;
        let (pin, e) = fixture(2, now);
        let (_, floor) = policy(&pin, &e, &Watermark::default(), now).unwrap();
        assert!(policy(&pin, &e, &floor, now).is_ok());
        assert!(policy(&pin, &e, &floor, now + 900001).is_err());
        assert!(policy(&pin, &e, &floor, now - 31000).is_err());
        let (_, old) = fixture(1, now);
        assert!(policy(&pin, &old, &floor, now).is_err());
        let (_, equivocation) = fixture(2, now + 1);
        assert!(policy(&pin, &equivocation, &floor, now + 1).is_err());
        assert!(verify(&pin, &e, PROVENANCE_TYPE).is_err());
        let mut other = pin.clone();
        other.tenant_id = "beta".into();
        assert!(policy(&other, &e, &Watermark::default(), now).is_err());
        let mut tamper = e.clone();
        tamper.payload = STANDARD.encode(b"{}");
        assert!(verify(&pin, &tamper, POLICY_TYPE).is_err());
    }
    #[test]
    fn absent_trust_is_unchanged_and_broken_trust_stays_closed() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Guard::new(dir.path(), "daemon").admit("downloads").is_ok());
        std::fs::create_dir(dir.path().join("team")).unwrap();
        std::fs::write(dir.path().join("team/pin.json"), b"{}").unwrap();
        let guard = Guard::new(dir.path(), "daemon");
        assert!(guard.configured());
        assert!(guard.admit("downloads").is_err());
        std::fs::remove_file(dir.path().join("team/pin.json")).unwrap();
        assert!(guard.admit("downloads").is_err());
    }
    #[test]
    fn durable_guard_accepts_only_verified_leases_and_retains_replay_floor() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("team")).unwrap();
        let (pin, e) = fixture(2, now_ms());
        std::fs::write(
            dir.path().join("team/pin.json"),
            serde_json::to_vec(&pin).unwrap(),
        )
        .unwrap();
        let guard = Guard::new(dir.path(), "daemon");
        assert!(guard.current().is_err());
        guard.accept(&e).unwrap();
        assert!(guard.admit("downloads").is_err());
        assert!(guard.admit("cloud_delivery").is_ok());
        let reopened = Guard::new(dir.path(), "daemon");
        let (_, old) = fixture(1, now_ms());
        assert!(reopened.accept(&old).is_err());
        assert_eq!(reopened.current().unwrap().sequence, 2);
        std::fs::write(dir.path().join("team/policy.json"), b"{}").unwrap();
        assert!(reopened.admit("cloud_delivery").is_err());
    }
    #[test]
    fn restriction_map_covers_new_effects_but_keeps_stop_and_read_paths() {
        for m in [
            "jobs_enqueue",
            "inbox_submit",
            "sources_submit",
            "queues_configure",
            "cloud_submit",
            "workflows_run",
        ] {
            assert!(!method_features(m, &json!({})).is_empty());
        }
        for m in [
            "jobs_control",
            "sources_control",
            "queues_control",
            "cloud_control",
            "workflows_control",
        ] {
            assert!(method_features(m, &json!({"action":"cancel"})).is_empty());
        }
        for m in [
            "jobs_list",
            "cloud_get",
            "approvals_decide",
            "sessions_revoke",
        ] {
            assert!(method_features(m, &json!({})).is_empty());
        }
    }
    #[test]
    fn frequent_checks_persist_clock_and_reject_rollback_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("team")).unwrap();
        let now = 1_800_000_000_000;
        let (pin, e) = fixture(2, now);
        std::fs::write(
            dir.path().join("team/pin.json"),
            serde_json::to_vec(&pin).unwrap(),
        )
        .unwrap();
        let guard = Guard::new(dir.path(), "daemon");
        guard.check_at(&e, true, now).unwrap();
        for offset in (1000..=120000).step_by(1000) {
            guard.check_at(&e, false, now + offset).unwrap();
        }
        let reopened = Guard::new(dir.path(), "daemon");
        assert!(reopened.check_at(&e, false, now + 120001).is_ok());
        assert!(reopened.check_at(&e, false, now + 80000).is_err());
        let floor: Watermark = serde_json::from_slice(
            &std::fs::read(dir.path().join("team/daemon-watermark.json")).unwrap(),
        )
        .unwrap();
        assert!(floor.clock_ms >= now + 90000);
    }
    #[test]
    fn bounded_reads_and_atomic_replacement_do_not_reuse_predictable_temporary_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.json");
        std::fs::write(path.with_extension("new"), b"canary").unwrap();
        atomic(&path, b"first").unwrap();
        atomic(&path, b"second").unwrap();
        assert_eq!(bounded_file(&path, 6).unwrap(), b"second");
        assert!(bounded_file(&path, 5).is_err());
        assert_eq!(
            std::fs::read(path.with_extension("new")).unwrap(),
            b"canary"
        );
        #[cfg(unix)]
        {
            let link = dir.path().join("linked");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(bounded_file(&link, 100).is_err());
        }
    }

    #[test]
    fn failed_policy_publication_cannot_rollback_the_durable_watermark() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("team")).unwrap();
        let now = now_ms();
        let (pin, new) = fixture(3, now);
        let (_, old) = fixture(2, now);
        std::fs::write(
            dir.path().join("team/pin.json"),
            serde_json::to_vec(&pin).unwrap(),
        )
        .unwrap();
        let guard = Guard::new(dir.path(), "daemon");
        std::fs::create_dir(dir.path().join("team/policy.json")).unwrap();
        assert!(guard.accept(&new).is_err());
        std::fs::remove_dir(dir.path().join("team/policy.json")).unwrap();
        assert!(guard.accept(&old).is_err());
        let reopened = Guard::new(dir.path(), "daemon");
        assert!(reopened.accept(&old).is_err());
        reopened.accept(&new).unwrap();
    }

    #[test]
    fn node_service_envelopes_verify_in_native_rust_with_domain_separation() {
        let pin = Pin::parse(include_bytes!("fixtures/team-pin.json")).unwrap();
        let e: Envelope =
            serde_json::from_slice(include_bytes!("fixtures/team-policy.json")).unwrap();
        let (p, _) = policy(&pin, &e, &Watermark::default(), 1_800_000_000_000).unwrap();
        assert_eq!(p.sequence, 4);
        assert!(!p.features.downloads);
        let provenance: Envelope =
            serde_json::from_slice(include_bytes!("fixtures/team-provenance.json")).unwrap();
        let report = verify(&pin, &provenance, PROVENANCE_TYPE).unwrap();
        assert_eq!(report["assertion_kind"], "client_reported");
        assert_eq!(report["artifact"]["size_bytes"], 123);
        assert!(policy(&pin, &provenance, &Watermark::default(), 1_800_000_000_000).is_err());
    }
}
