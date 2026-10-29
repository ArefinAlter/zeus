//! Human-configured immutable acquisition, with durable identity and budget authority.
mod operations;
mod store;
#[cfg(test)]
mod tests;
mod torrent;
mod transport;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use store::Store;
use tokio::sync::Mutex;
use zeus_api::methods::sources::*;
use zeus_api::{ApiError, ApiResult, ClientIdentity, ClientKind};
use zeus_cloud::CredentialStore;
use zeus_sources::{
    BoundSource, SourceAuthority, SourceError, SourceResult, SourceSecret, SourceTransport,
};
use zeus_types::{Actor, JobId, Timestamp, ToEnvelope};
fn storage() -> ApiError {
    ApiError::internal("private source state unavailable")
}
fn missing() -> ApiError {
    ApiError::new(
        "api.not_found",
        "source account, review or operation not found",
    )
}
fn invalid() -> ApiError {
    ApiError::invalid_params("invalid immutable source scope or request")
}
fn denied() -> ApiError {
    ApiError::new(
        "sources.denied",
        "source authority is absent, expired or revoked",
    )
}
fn source_error(error: SourceError) -> ApiError {
    ApiError::new(
        match error {
            SourceError::Budget => "sources.budget",
            SourceError::Changed => "sources.changed",
            SourceError::Integrity => "sources.integrity",
            SourceError::Canceled => "sources.canceled",
            SourceError::Credential => "sources.credential",
            SourceError::Unsupported => "sources.unsupported",
            SourceError::Denied => "sources.denied",
            SourceError::Invalid => "api.invalid_params",
            _ => "sources.failed",
        },
        error.to_string(),
    )
}
fn id(prefix: &str) -> String {
    format!("{prefix}_{}", ulid::Ulid::generate())
}
fn encoded<T: Serialize>(value: &T) -> ApiResult<Vec<u8>> {
    serde_json::to_vec(value).map_err(|_| storage())
}
fn digest<T: Serialize>(value: &T) -> ApiResult<String> {
    let bytes = zeroize::Zeroizing::new(encoded(value)?);
    Ok(zeus_verify::hash_bytes(&bytes).blake3.to_string())
}
fn receipt_key(identity: &ClientIdentity, method: &str, key: &str) -> ApiResult<String> {
    if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
        return Err(invalid());
    }
    Ok(format!(
        "receipt:{}",
        digest(&(identity.credential_id.as_str(), method, key))?
    ))
}
#[derive(Clone, Serialize, Deserialize)]
struct Account {
    view: SourceAccountView,
    credential_key: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Plan {
    owner: String,
    view: SourcePlanView,
    target: SourceTarget,
    metainfo: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Operation {
    owner: String,
    view: SourceOperationView,
    target: SourceTarget,
    deadline: Timestamp,
    created_at: Timestamp,
    dest_dir: String,
    dest_file_id: zeus_types::FileId,
    filename: String,
    metainfo: Option<String>,
    request: SourcesSubmitParams,
    actor_kind: zeus_types::ActorKind,
    source: zeus_types::Source,
    #[serde(default)]
    publications: Vec<Publication>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Publication {
    file: SourcePublishedFile,
    temp_name: String,
    file_id: Option<zeus_types::FileId>,
    published: bool,
}
#[derive(Serialize, Deserialize)]
struct Receipt {
    hash: String,
    result: serde_json::Value,
}
type RequesterCheck = Arc<dyn Fn(&str) -> bool + Send + Sync>;
type RequesterRoots = Arc<dyn Fn(&str) -> Vec<PathBuf> + Send + Sync>;

/// Owns immutable source scopes and their private credentials; no ambient discovery.
pub struct SourceService {
    store: Arc<Store>,
    credentials: Arc<dyn CredentialStore>,
    gate: Mutex<()>,
    engine: OnceLock<zeus_engine::Engine>,
    journal: zeus_journal::Journal,
    network: Arc<dyn zeus_http::HttpTransport>,
    write_roots: Vec<PathBuf>,
    state_dir: PathBuf,
    requester: RequesterCheck,
    roots: RequesterRoots,
    active:
        std::sync::Mutex<std::collections::BTreeMap<String, tokio_util::sync::CancellationToken>>,
    boot: Instant,
    boot_time: Timestamp,
}
impl SourceService {
    /// Open without contacting providers; interrupted source operations require explicit resume.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        state: &Path,
        journal: zeus_journal::Journal,
        credentials: Arc<dyn CredentialStore>,
        network: Arc<dyn zeus_http::HttpTransport>,
        write_roots: Vec<PathBuf>,
        requester: RequesterCheck,
        roots: RequesterRoots,
    ) -> ApiResult<Arc<Self>> {
        let store = Arc::new(Store::open(&state.join("sources.redb"))?);
        let retained = store
            .list::<Account>("account:")?
            .into_iter()
            .filter_map(|a| a.credential_key)
            .collect::<std::collections::BTreeSet<_>>();
        for key in store.list::<String>("key-intent:")? {
            if !retained.contains(&key) && credentials.delete(&key).is_ok() {
                store.remove(&format!("key-intent:{key}"))?;
            }
        }
        for mut op in store.list::<Operation>("operation:")? {
            if matches!(op.view.state.as_str(), "queued" | "running") {
                op.view.state = "paused".into();
                op.view.error_code = Some("sources.restart_review_required".into());
                store.save(&format!("operation:{}", op.view.operation_id), &op)?;
            }
        }
        Ok(Arc::new(Self {
            store,
            credentials,
            gate: Mutex::new(()),
            engine: OnceLock::new(),
            journal,
            network,
            write_roots,
            state_dir: state.join("source-staging"),
            requester,
            roots,
            active: Default::default(),
            boot: Instant::now(),
            boot_time: Timestamp::now(),
        }))
    }
    /// Attach the transfer engine after installing the identity-bound factory.
    pub fn attach_engine(&self, engine: zeus_engine::Engine) -> ApiResult<()> {
        self.engine.set(engine).map_err(|_| storage())
    }
    fn now(&self) -> Timestamp {
        self.boot_time
            .saturating_add_ms(
                self.boot
                    .elapsed()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
            )
            .max(Timestamp::now())
    }
    fn replay<T: serde::de::DeserializeOwned>(
        &self,
        key: &str,
        hash: &str,
    ) -> ApiResult<Option<T>> {
        self.store
            .get::<Receipt>(key)?
            .map(|row| {
                if row.hash != hash {
                    return Err(ApiError::new(
                        "api.idempotency_conflict",
                        "source retry key has different parameters",
                    ));
                }
                serde_json::from_value(row.result).map_err(|_| storage())
            })
            .transpose()
    }
    fn receipt<T: Serialize>(
        &self,
        key: String,
        hash: String,
        result: &T,
        mut rows: Vec<(String, Vec<u8>)>,
    ) -> ApiResult<()> {
        rows.push((
            key,
            encoded(&Receipt {
                hash,
                result: serde_json::to_value(result).map_err(|_| storage())?,
            })?,
        ));
        self.store.save_many(&rows)
    }
    fn account(&self, owner: &str, id: &str, revision: u64) -> ApiResult<Account> {
        let row = self
            .store
            .get::<Account>(&format!("account:{id}"))?
            .ok_or_else(missing)?;
        if row.view.revoked
            || row.view.revision != revision
            || !row.view.allowed_credential_ids.iter().any(|v| v == owner)
            || !(self.requester)(owner)
        {
            return Err(denied());
        }
        if self
            .journal
            .meta()
            .map_err(|_| storage())?
            .kill_switch_engaged()
        {
            return Err(denied());
        }
        Ok(row)
    }
    fn secret(&self, account: &Account) -> ApiResult<Option<SourceSecret>> {
        account
            .credential_key
            .as_ref()
            .map(|key| {
                self.credentials
                    .get(key)
                    .map(|s| SourceSecret::new(s.expose().to_owned()))
                    .map_err(|_| source_error(SourceError::Credential))
            })
            .transpose()
    }
    /// Project only accounts this requester may inspect, without credential references.
    pub fn accounts(&self, identity: &ClientIdentity) -> ApiResult<SourcesAccountsResult> {
        Ok(SourcesAccountsResult {
            accounts: self
                .store
                .list::<Account>("account:")?
                .into_iter()
                .filter(|a| {
                    matches!(identity.kind, ClientKind::Human | ClientKind::Admin)
                        || a.view
                            .allowed_credential_ids
                            .iter()
                            .any(|v| v == identity.credential_id.as_str())
                })
                .map(|a| a.view)
                .collect(),
        })
    }
    /// Save an explicit grant at a verified human terminal; revisions revoke prior authority.
    pub async fn configure(
        &self,
        identity: &ClientIdentity,
        actor: Actor,
        p: SourcesAccountConfigureParams,
    ) -> ApiResult<SourcesAccountMutationResult> {
        if !matches!(identity.kind, ClientKind::Human | ClientKind::Admin)
            || !actor.is_human_on_trust_surface()
            || !matches!(
                actor.surface,
                Some(zeus_types::Surface::CliInteractive | zeus_types::Surface::Tui)
            )
        {
            return Err(ApiError::human_required("verified human terminal"));
        }
        let _gate = self.gate.lock().await;
        let key = receipt_key(identity, "configure", &p.idempotency_key)?;
        let hash = digest(&p)?;
        if let Some(mut prior) = self.replay::<SourcesAccountMutationResult>(&key, &hash)? {
            prior.replayed = true;
            return Ok(prior);
        }
        if p.account_id.is_empty()
            || p.account_id.len() > 64
            || !p
                .account_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            || p.untrusted_label.len() > 256
            || p.untrusted_label.chars().any(char::is_control)
            || p.allowed_credential_ids.is_empty()
            || p.allowed_credential_ids.len() > 64
            || p.allowed_credential_ids
                .iter()
                .any(|v| !(self.requester)(v))
        {
            return Err(invalid());
        }
        p.budget_ceiling.validate().map_err(source_error)?;
        let old = self
            .store
            .get::<Account>(&format!("account:{}", p.account_id))?;
        if old.as_ref().map(|a| a.view.revision) != p.expected_revision {
            return Err(ApiError::new(
                "api.conflict",
                "source account revision changed",
            ));
        }
        let secret = p
            .credential
            .as_ref()
            .map(|s| SourceSecret::new(s.expose().to_owned()));
        let target = match &p.config {
            SourceAccountConfig::S3 {
                bucket, key_prefix, ..
            } => SourceTarget::S3 {
                bucket: bucket.clone(),
                key: format!("{key_prefix}scope-validation"),
                version_id: None,
            },
            SourceAccountConfig::HuggingFace {
                repository_type,
                repository,
            } => SourceTarget::HuggingFace {
                repository_type: *repository_type,
                repository: repository.clone(),
                commit: "0".repeat(40),
                path: "scope-validation".into(),
            },
            SourceAccountConfig::Torrent {
                max_peers,
                max_download_bytes_per_second,
                allowed_peer_ips,
            } => {
                if !(1..=32).contains(max_peers)
                    || *max_download_bytes_per_second == 0
                    || *max_download_bytes_per_second > u32::MAX as u64
                    || allowed_peer_ips.is_empty()
                    || allowed_peer_ips.len() > 256
                    || allowed_peer_ips
                        .iter()
                        .any(|v| v.parse::<std::net::IpAddr>().is_err())
                    || secret.is_some()
                {
                    return Err(invalid());
                }
                SourceTarget::Torrent {
                    metainfo_sha256: String::new(),
                    file_indices: vec![],
                    initial_peers: vec![],
                }
            }
        };
        if !matches!(target, SourceTarget::Torrent { .. }) {
            BoundSource::prepare(&p.config, &target, secret.as_ref()).map_err(source_error)?;
        }
        let credential_key = if let Some(secret) = &secret {
            let k = id("zeus-source-key");
            self.store.save(&format!("key-intent:{k}"), &k)?;
            self.credentials
                .put(
                    &k,
                    &zeus_cloud::SecretString::new(secret.expose().to_owned()),
                )
                .map_err(|_| source_error(SourceError::Credential))?;
            Some(k)
        } else {
            None
        };
        let account = Account {
            view: SourceAccountView {
                account_id: p.account_id.clone(),
                untrusted_label: p.untrusted_label,
                config: p.config,
                revision: old
                    .as_ref()
                    .map(|a| a.view.revision.checked_add(1).ok_or_else(invalid))
                    .transpose()?
                    .unwrap_or(1),
                allowed_credential_ids: p.allowed_credential_ids,
                budget_ceiling: p.budget_ceiling,
                credential_configured: credential_key.is_some(),
                revoked: false,
            },
            credential_key,
        };
        let result = SourcesAccountMutationResult {
            account: account.view.clone(),
            replayed: false,
        };
        self.receipt(
            key,
            hash,
            &result,
            vec![(format!("account:{}", p.account_id), encoded(&account)?)],
        )?;
        if let Some(key) = old.and_then(|a| a.credential_key)
            && self.credentials.delete(&key).is_ok()
        {
            self.store.remove(&format!("key-intent:{key}"))?;
        }
        Ok(result)
    }
    /// Revoke local reads and publication before attempting provider-key cleanup.
    pub async fn revoke(
        &self,
        identity: &ClientIdentity,
        actor: Actor,
        p: SourcesAccountRevokeParams,
    ) -> ApiResult<SourcesAccountMutationResult> {
        if !matches!(identity.kind, ClientKind::Human | ClientKind::Admin)
            || !actor.is_human_on_trust_surface()
        {
            return Err(ApiError::human_required("verified human terminal"));
        }
        let _gate = self.gate.lock().await;
        let key = receipt_key(identity, "revoke", &p.idempotency_key)?;
        let hash = digest(&p)?;
        if let Some(mut prior) = self.replay::<SourcesAccountMutationResult>(&key, &hash)? {
            prior.replayed = true;
            return Ok(prior);
        }
        let mut account = self
            .store
            .get::<Account>(&format!("account:{}", p.account_id))?
            .ok_or_else(missing)?;
        if account.view.revision != p.expected_revision {
            return Err(ApiError::new(
                "api.conflict",
                "source account revision changed",
            ));
        }
        account.view.revoked = true;
        account.view.revision = account.view.revision.checked_add(1).ok_or_else(invalid)?;
        account.view.credential_configured = false;
        let credential = account.credential_key.take();
        let result = SourcesAccountMutationResult {
            account: account.view.clone(),
            replayed: false,
        };
        self.receipt(
            key,
            hash,
            &result,
            vec![(format!("account:{}", p.account_id), encoded(&account)?)],
        )?;
        if let Some(key) = credential
            && self.credentials.delete(&key).is_ok()
        {
            self.store.remove(&format!("key-intent:{key}"))?;
        }
        Ok(result)
    }
}
