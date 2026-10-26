//! Durable model policy and spend enforcement.

use crate::LlmError;
use serde_json::Value;

/// Reject external schema resolution, excessive nesting, and invalid schemas before any request.
pub fn validate_schema(schema: &Value) -> Result<(), LlmError> {
    fn walk(v: &Value, depth: usize) -> bool {
        if depth > 32 {
            return false;
        }
        match v {
            Value::Object(map) => map.iter().all(|(k, v)| {
                if matches!(k.as_str(), "$ref" | "$dynamicRef")
                    && v.as_str().is_none_or(|s| !s.starts_with('#'))
                {
                    return false;
                }
                walk(v, depth + 1)
            }),
            Value::Array(items) => items.len() <= 4096 && items.iter().all(|v| walk(v, depth + 1)),
            _ => true,
        }
    }
    if serde_json::to_vec(schema)
        .map_err(|_| LlmError::SchemaViolation)?
        .len()
        > 65536
        || !walk(schema, 0)
    {
        return Err(LlmError::SchemaViolation);
    }
    jsonschema::options()
        .with_retriever(NoExternalSchema)
        .build(schema)
        .map_err(|_| LlmError::SchemaViolation)?;
    Ok(())
}
/// Validate generated JSON locally; provider structured-output claims are insufficient.
pub fn check_output(schema: &Value, value: &Value) -> Result<(), LlmError> {
    validate_schema(schema)?;
    if jsonschema::options()
        .with_retriever(NoExternalSchema)
        .build(schema)
        .map_err(|_| LlmError::SchemaViolation)?
        .is_valid(value)
    {
        Ok(())
    } else {
        Err(LlmError::SchemaViolation)
    }
}

use crate::policy::{PolicyError, verify_envelope};
use crate::provider::Locality;
use crate::{
    Ladder, LadderConfig, LlmProvider, LlmRequest, LlmResponse, PolicyEnvelope, PolicyManifest,
    PrivacyTier, ProviderKind, ProviderStatus, SpendCaps, SpendLedger, TrustedKeys, redact_text,
};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

/// Whether a manifest's `min_zeus_version` is above this build's policy level
/// ([`crate::policy::POLICY_LEVEL`]), which disables remote model providers. The release name of the build (for
/// example `0.0.1-beta`) plays no part.
pub fn requires_newer_build(min: &str) -> Result<bool, LlmError> {
    let level = semver::Version::parse(crate::policy::POLICY_LEVEL)
        .map_err(|_| LlmError::SchemaViolation)?;
    let min = semver::Version::parse(min).map_err(|_| LlmError::SchemaViolation)?;
    Ok(level < min)
}
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
};
use time::OffsetDateTime;

const STATE: TableDefinition<&str, &[u8]> = TableDefinition::new("llm_runtime_v1");
#[derive(Clone, Serialize, Deserialize)]
struct Reservation {
    id: u64,
    day: u32,
    month: u32,
    ceiling: u64,
}
#[derive(Serialize, Deserialize)]
struct State {
    ledger: SpendLedger,
    next_id: u64,
    pending: BTreeMap<u64, Reservation>,
    policy: Option<PolicyEnvelope>,
    policy_sequence: u64,
    quote_violated: bool,
    #[serde(default)]
    configuration: Option<Value>,
}

/// Durable budget reservations and signed-policy anti-rollback state. One database owner per daemon.
/// Charges are reserved before sending; ambiguous failure or restart retains the full ceiling.
pub struct RuntimeStore {
    db: Database,
}
impl RuntimeStore {
    /// Open a private daemon-owned database. Existing caps remain authoritative until explicitly changed.
    pub fn open(path: &Path, caps: SpendCaps, now: OffsetDateTime) -> Result<Self, LlmError> {
        if path.is_symlink() {
            return Err(storage_error());
        }
        let db = Database::create(path).map_err(|_| storage_error())?;
        let store = Self { db };
        let (day, month) = periods(now)?;
        let mut tx = store.db.begin_write().map_err(|_| storage_error())?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| storage_error())?;
        {
            let mut table = tx.open_table(STATE).map_err(|_| storage_error())?;
            let saved = table
                .get("state")
                .map_err(|_| storage_error())?
                .map(|v| v.value().to_vec());
            let mut state = match saved {
                Some(bytes) => {
                    serde_json::from_slice::<State>(&bytes).map_err(|_| storage_error())?
                }
                None => State {
                    ledger: SpendLedger::new(caps, day, month),
                    next_id: 0,
                    pending: BTreeMap::new(),
                    policy: None,
                    policy_sequence: 0,
                    quote_violated: false,
                    configuration: None,
                },
            };
            // A prior process may have sent these requests. Their reservations remain charged.
            state.pending.clear();
            let bytes = serde_json::to_vec(&state).map_err(|_| storage_error())?;
            table
                .insert("state", bytes.as_slice())
                .map_err(|_| storage_error())?;
        }
        tx.commit().map_err(|_| storage_error())?;
        Ok(store)
    }
    fn mutate<R>(
        &self,
        operation: impl FnOnce(&mut State) -> Result<R, LlmError>,
    ) -> Result<R, LlmError> {
        let mut tx = self.db.begin_write().map_err(|_| storage_error())?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| storage_error())?;
        let output;
        {
            let mut table = tx.open_table(STATE).map_err(|_| storage_error())?;
            let bytes = table
                .get("state")
                .map_err(|_| storage_error())?
                .ok_or_else(storage_error)?
                .value()
                .to_vec();
            let mut state: State = serde_json::from_slice(&bytes).map_err(|_| storage_error())?;
            output = operation(&mut state)?;
            let bytes = serde_json::to_vec(&state).map_err(|_| storage_error())?;
            table
                .insert("state", bytes.as_slice())
                .map_err(|_| storage_error())?;
        }
        tx.commit().map_err(|_| storage_error())?;
        Ok(output)
    }
    fn state(&self) -> Result<State, LlmError> {
        let tx = self.db.begin_read().map_err(|_| storage_error())?;
        let table = tx.open_table(STATE).map_err(|_| storage_error())?;
        let bytes = table
            .get("state")
            .map_err(|_| storage_error())?
            .ok_or_else(storage_error)?;
        serde_json::from_slice(bytes.value()).map_err(|_| storage_error())
    }
    /// Current durable conservative spend, including ambiguous/in-flight requests.
    pub fn spend(&self) -> Result<SpendLedger, LlmError> {
        Ok(self.state()?.ledger)
    }
    /// Current-period spend; advancing the clock cannot reset an earlier period twice.
    pub fn spend_at(&self, now: OffsetDateTime) -> Result<SpendLedger, LlmError> {
        let (day, month) = periods(now)?;
        self.mutate(|state| {
            state.ledger.advance(day, month);
            Ok(state.ledger.clone())
        })
    }
    /// Non-secret application configuration stored with spend and policy state.
    pub fn configuration(&self) -> Result<Option<Value>, LlmError> {
        Ok(self.state()?.configuration)
    }
    /// Persist an explicit human configuration change without resetting accumulated spend.
    pub fn configure(&self, configuration: Value, caps: Option<SpendCaps>) -> Result<(), LlmError> {
        self.configure_reviewed(configuration, caps, false)
    }
    /// Persist a trusted human review of explicit adapter pricing. A review can clear a quote
    /// violation only after all earlier requests settle; accumulated spend is always retained.
    pub fn configure_reviewed(
        &self,
        configuration: Value,
        caps: Option<SpendCaps>,
        pricing_reviewed: bool,
    ) -> Result<(), LlmError> {
        if serde_json::to_vec(&configuration)
            .map_err(|_| LlmError::SchemaViolation)?
            .len()
            > 65536
        {
            return Err(LlmError::SchemaViolation);
        }
        self.mutate(|state| {
            if pricing_reviewed && state.quote_violated {
                if !state.pending.is_empty() {
                    return Err(LlmError::ProviderUnavailable(
                        "model requests must settle before reviewing pricing".into(),
                    ));
                }
                state.quote_violated = false;
            }
            state.configuration = Some(configuration);
            if let Some(caps) = caps {
                state.ledger.set_caps(caps);
            }
            Ok(())
        })
    }

    fn reserve(&self, ceiling: u64, now: OffsetDateTime) -> Result<Reservation, LlmError> {
        let (day, month) = periods(now)?;
        self.mutate(|state| {
            if state.quote_violated {
                return Err(LlmError::DisabledByPolicy("unreliable_cost_quote"));
            }
            if state.pending.len() >= 1024 {
                return Err(LlmError::ProviderUnavailable(
                    "too many active model requests".into(),
                ));
            }
            state.ledger.advance(day, month);
            state.ledger.charge(ceiling)?;
            state.next_id = state.next_id.checked_add(1).ok_or_else(storage_error)?;
            let reservation = Reservation {
                id: state.next_id,
                day,
                month,
                ceiling,
            };
            state.pending.insert(reservation.id, reservation.clone());
            Ok(reservation)
        })
    }
    fn settle(&self, reservation: Reservation, actual: Option<u64>) -> Result<(), LlmError> {
        let over = self.mutate(|state| {
            let pending = state
                .pending
                .remove(&reservation.id)
                .ok_or_else(storage_error)?;
            let actual = actual.unwrap_or(pending.ceiling);
            if actual > pending.ceiling {
                state
                    .ledger
                    .record_overrun(pending.day, pending.month, actual - pending.ceiling);
                state.quote_violated = true;
                return Ok(true);
            }
            state
                .ledger
                .refund(pending.day, pending.month, pending.ceiling - actual);
            Ok(false)
        })?;
        if over {
            Err(LlmError::DisabledByPolicy("unreliable_cost_quote"))
        } else {
            Ok(())
        }
    }
    /// Accept a signed policy only under provisioned keys, atomically persisting its sequence.
    /// The same sequence may only replay the identical payload (signature rotation is permitted).
    pub fn install_policy(
        &self,
        envelope: &PolicyEnvelope,
        trusted: &TrustedKeys,
        now: OffsetDateTime,
    ) -> Result<PolicyManifest, LlmError> {
        if serde_json::to_vec(envelope)
            .map_err(|_| LlmError::SchemaViolation)?
            .len()
            > 256 * 1024
        {
            return Err(LlmError::SchemaViolation);
        }
        self.mutate(|state| {
            let manifest = verify_envelope(envelope, trusted, now, state.policy_sequence)?;
            if manifest.sequence == state.policy_sequence
                && state
                    .policy
                    .as_ref()
                    .is_some_and(|p| p.payload != envelope.payload)
            {
                return Err(
                    PolicyError::Malformed("sequence reused with different policy".into()).into(),
                );
            }
            state.policy_sequence = manifest.sequence;
            state.policy = Some(envelope.clone());
            Ok(manifest)
        })
    }
    fn policy(
        &self,
        trusted: &TrustedKeys,
        now: OffsetDateTime,
    ) -> Result<Option<PolicyManifest>, LlmError> {
        let state = self.state()?;
        match state.policy {
            Some(policy) => match verify_envelope(&policy, trusted, now, state.policy_sequence) {
                Ok(p) => Ok(Some(p)),
                Err(PolicyError::Expired) => Ok(None),
                Err(e) => Err(e.into()),
            },
            None => Ok(None),
        }
    }
}
fn periods(now: OffsetDateTime) -> Result<(u32, u32), LlmError> {
    let day = u32::try_from(now.unix_timestamp().div_euclid(86400)).map_err(|_| storage_error())?;
    let month = u32::try_from(i64::from(now.year()) * 12 + i64::from(u8::from(now.month())) - 1)
        .map_err(|_| storage_error())?;
    Ok((day, month))
}
fn storage_error() -> LlmError {
    LlmError::ProviderUnavailable("durable model state is unavailable".into())
}

/// Explicit user settings. Default leaves all model access off.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeSettings {
    /// User enabled metadata model tasks.
    pub enabled: bool,
    /// User opted into experimental rungs.
    pub allow_experimental: bool,
    /// User-disabled rungs; signed policy cannot re-enable these.
    pub disabled: BTreeSet<ProviderKind>,
}
/// Per-request context declaration. File/page contents need both this classification and task consent.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestAuthorization {
    /// True if any message contains extracted document or page content.
    pub contains_content: bool,
    /// User explicitly permitted content for this task, through a trusted UI.
    pub content_opt_in: bool,
    /// Maximum cost permitted for this one request; zero permits only free/local requests.
    pub max_cost_micro_usd: u64,
}

/// The daemon's model entry point: consent, policy, privacy, redaction, durable caps, validation.
/// A failed/ambiguous request is never automatically retried or sent to another provider.
pub struct ModelRuntime {
    providers: BTreeMap<ProviderKind, Arc<dyn LlmProvider>>,
    settings: RuntimeSettings,
    store: Arc<RuntimeStore>,
    trusted: TrustedKeys,
}
impl ModelRuntime {
    /// Register explicitly configured adapters; no model discovery or network is performed.
    pub fn new(
        settings: RuntimeSettings,
        store: Arc<RuntimeStore>,
        trusted: TrustedKeys,
        providers: Vec<Arc<dyn LlmProvider>>,
    ) -> Result<Self, LlmError> {
        let mut map = BTreeMap::new();
        for provider in providers {
            if map.insert(provider.kind(), provider).is_some() {
                return Err(LlmError::NotConfigured);
            }
        }
        Ok(Self {
            providers: map,
            settings,
            store,
            trusted,
        })
    }
    fn ladder(&self, now: OffsetDateTime) -> Result<(Ladder, Option<u64>), LlmError> {
        let policy = self.store.policy(&self.trusted, now)?;
        let mut config = LadderConfig {
            allow_experimental: self.settings.allow_experimental,
            ..Default::default()
        };
        let sequence = policy.as_ref().map(|p| p.sequence);
        if let Some(policy) = policy {
            config.status = policy.status_map();
            let too_old = match policy.min_zeus_version {
                Some(min) => requires_newer_build(&min)?,
                None => false,
            };
            for kind in ProviderKind::LADDER {
                if policy.kill_switches.contains(kind.as_str())
                    || policy.kill_switches.contains("all_models")
                    || (kind.locality() == Locality::Remote
                        && (too_old || policy.kill_switches.contains("all_remote_models")))
                {
                    config.status.insert(kind, ProviderStatus::Disabled);
                }
            }
        } else {
            // Unprovisioned or expired policy permits only explicit API keys and local models.
            for kind in [
                ProviderKind::ChatgptOauth,
                ProviderKind::CliDelegate,
                ProviderKind::OpenrouterOauth,
            ] {
                config.status.insert(kind, ProviderStatus::Disabled);
            }
        }
        for kind in &self.settings.disabled {
            config.status.insert(*kind, ProviderStatus::Disabled);
        }
        Ok((Ladder::new(config), sequence))
    }
    /// Effective statuses and configured state without making network calls.
    pub fn status(&self) -> Result<RuntimeStatus, LlmError> {
        let (ladder, policy_sequence) = self.ladder(OffsetDateTime::now_utc())?;
        let latched = self.store.state()?.quote_violated;
        let providers = ProviderKind::LADDER
            .into_iter()
            .map(|kind| {
                let configured = self.providers.get(&kind).is_some_and(|p| p.is_configured());
                let status = if !self.settings.enabled || latched {
                    ProviderStatus::Disabled
                } else {
                    ladder.status_of(kind)
                };
                let reason = if !self.settings.enabled {
                    Some("Model access is disabled".into())
                } else if latched {
                    Some("A provider exceeded its cost quote; review is required".into())
                } else if status == ProviderStatus::Disabled {
                    Some("Disabled by local settings or signed-policy requirements".into())
                } else if !configured {
                    Some("No adapter or credential configured".into())
                } else {
                    None
                };
                ProviderRuntimeStatus {
                    provider: kind,
                    status,
                    configured,
                    reason,
                }
            })
            .collect();
        Ok(RuntimeStatus {
            providers,
            policy_sequence,
        })
    }
    /// Execute one task. Prompts/responses are not written into the spend or policy database.
    pub async fn complete(
        &self,
        mut request: LlmRequest,
        authorization: RequestAuthorization,
    ) -> Result<LlmResponse, LlmError> {
        if !self.settings.enabled {
            return Err(LlmError::NotConfigured);
        }
        if request.max_output_tokens == 0
            || request.messages.is_empty()
            || request.messages.len() > 128
            || request
                .messages
                .iter()
                .map(|m| m.content.len())
                .sum::<usize>()
                > 256 * 1024
        {
            return Err(LlmError::SchemaViolation);
        }
        if authorization.contains_content
            && (request.privacy != PrivacyTier::Content || !authorization.content_opt_in)
        {
            return Err(LlmError::PrivacyViolation {
                tier: request.privacy.as_str(),
                reason: "file or page content needs explicit consent for this task",
            });
        }
        if request.privacy == PrivacyTier::Content && !authorization.content_opt_in {
            return Err(LlmError::PrivacyViolation {
                tier: "content",
                reason: "content tier needs explicit consent for this task",
            });
        }
        if let Some(schema) = &request.response_schema {
            validate_schema(schema)?
        }
        for message in &mut request.messages {
            message.content = redact_text(&message.content)
        }
        let now = OffsetDateTime::now_utc();
        let (ladder, _sequence) = self.ladder(now)?;
        let configured = self
            .providers
            .iter()
            .filter(|(_, p)| p.is_configured())
            .map(|(k, _)| *k)
            .collect();
        let kind = ladder.select(&configured, request.privacy)?;
        let provider = self.providers.get(&kind).ok_or(LlmError::NotConfigured)?;
        let ceiling = provider.cost_ceiling(&request)?;
        if ceiling > authorization.max_cost_micro_usd {
            return Err(LlmError::BudgetExceeded {
                period: "request",
                spent_micro_usd: ceiling,
                cap_micro_usd: authorization.max_cost_micro_usd,
            });
        }
        let reservation = self.store.reserve(ceiling, now)?;
        let response = provider.complete(request.clone()).await;
        match response {
            Ok(response) => {
                self.store
                    .settle(reservation, response.usage.cost_micro_usd)?;
                if response.provider != kind {
                    return Err(LlmError::SchemaViolation);
                }
                if let Some(schema) = &request.response_schema {
                    match &response.content {
                        crate::LlmContent::Json(value) => check_output(schema, value)?,
                        _ => return Err(LlmError::SchemaViolation),
                    }
                }
                Ok(response)
            }
            Err(error) => {
                self.store.settle(reservation, None)?;
                Err(error)
            }
        }
    }
}

struct NoExternalSchema;
impl jsonschema::Retrieve for NoExternalSchema {
    fn retrieve(
        &self,
        _: &jsonschema::Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        Err("external model response schemas are forbidden".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{DsseSignature, PAYLOAD_TYPE, pae};
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};
    fn signed(sequence: u64, expires: &str, key: &SigningKey) -> PolicyEnvelope {
        let payload = serde_json::to_vec(&PolicyManifest {
            schema_version: 1,
            sequence,
            issued_at: "2026-10-15T00:00:00Z".into(),
            expires_at: expires.into(),
            min_zeus_version: None,
            providers: BTreeMap::new(),
            kill_switches: BTreeSet::new(),
        })
        .unwrap();
        PolicyEnvelope {
            payload_type: PAYLOAD_TYPE.into(),
            payload: base64::engine::general_purpose::STANDARD.encode(&payload),
            signatures: vec![DsseSignature {
                keyid: "test-only".into(),
                sig: base64::engine::general_purpose::STANDARD
                    .encode(key.sign(&pae(PAYLOAD_TYPE, &payload)).to_bytes()),
            }],
        }
    }
    #[test]
    fn a_manifest_minimum_is_compared_with_the_policy_level_not_the_release_name() {
        // Release names such as 0.0.1-beta do not change which providers a build may use.
        assert!(!requires_newer_build("0.1.0").unwrap());
        assert!(!requires_newer_build("0.0.1-beta").unwrap());
        assert!(requires_newer_build("0.2.0").unwrap());
        assert!(requires_newer_build("1.0.0").unwrap());
        assert!(requires_newer_build("soon").is_err());
        assert!(!env!("CARGO_PKG_VERSION").is_empty());
    }
    #[test]
    fn policy_rollback_and_equivocation_rejected_after_restart_and_no_trust_keys_are_invented() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("models");
        let now = time::macros::datetime!(2026-10-16 00:00 UTC);
        let key = SigningKey::from_bytes(&[9; 32]);
        let mut trusted = TrustedKeys::default();
        trusted
            .add("test-only", key.verifying_key().to_bytes())
            .unwrap();
        let store = RuntimeStore::open(&path, SpendCaps::default(), now).unwrap();
        let envelope = signed(5, "2026-11-01T00:00:00Z", &key);
        assert!(
            store
                .install_policy(&envelope, &TrustedKeys::pinned(), now)
                .is_err()
        );
        store.install_policy(&envelope, &trusted, now).unwrap();
        drop(store);
        let reopened = RuntimeStore::open(&path, SpendCaps::default(), now).unwrap();
        assert!(
            reopened
                .install_policy(&signed(4, "2026-11-01T00:00:00Z", &key), &trusted, now)
                .is_err()
        );
        assert!(
            reopened
                .install_policy(&signed(5, "2026-12-01T00:00:00Z", &key), &trusted, now)
                .is_err()
        );
        assert!(
            reopened
                .policy(&trusted, time::macros::datetime!(2026-12-01 00:00 UTC))
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn crash_keeps_reservation_and_late_refund_never_reduces_new_period_spend() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("models");
        let now = time::macros::datetime!(2026-10-16 00:00 UTC);
        let store = RuntimeStore::open(
            &path,
            SpendCaps {
                daily_micro_usd: 100,
                monthly_micro_usd: 100,
            },
            now,
        )
        .unwrap();
        let old = store.reserve(60, now).unwrap();
        drop(store);
        let reopened = RuntimeStore::open(&path, SpendCaps::default(), now).unwrap();
        assert_eq!(reopened.spend().unwrap().day_spent(), 60);
        assert!(reopened.settle(old, Some(1)).is_err());
        let later = time::macros::datetime!(2026-11-01 00:00 UTC);
        let reserved = reopened.reserve(90, later).unwrap();
        assert_eq!(reopened.spend().unwrap().month_spent(), 90);
        reopened.settle(reserved, Some(10)).unwrap();
        assert_eq!(reopened.spend().unwrap().day_spent(), 10);
    }
}

/// Status of one rung without exposing endpoints, credentials or prompt content.
#[derive(Debug, Clone)]
pub struct ProviderRuntimeStatus {
    /// Provider kind.
    pub provider: ProviderKind,
    /// Effective policy status.
    pub status: ProviderStatus,
    /// An adapter and required credential are configured.
    pub configured: bool,
    /// User-facing reason when unavailable.
    pub reason: Option<String>,
}
/// Current policy applied to a configured runtime.
#[derive(Debug, Clone)]
pub struct RuntimeStatus {
    /// All provider rungs in ladder order.
    pub providers: Vec<ProviderRuntimeStatus>,
    /// Sequence of the currently valid manifest, absent when unprovisioned/expired.
    pub policy_sequence: Option<u64>,
}
