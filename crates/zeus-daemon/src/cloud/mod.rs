//! Durable cloud delivery orchestration with exact scope, approval and recovery bindings.
mod source;
mod store;
#[cfg(test)]
mod tests;
mod worker;
use async_trait::async_trait;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use store::{Account, Delivery, Plan, Receipt, Store, encoded};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use zeus_api::methods::cloud::*;
use zeus_api::{ApiError, ApiResult, ClientIdentity, ClientKind};
use zeus_cloud::{CloudProvider, CredentialStore, DeliveryMode, ProviderConfig};
use zeus_journal::Journal;
use zeus_types::{Actor, ApprovalKind, ApprovalState, Hex256, JobState, Timestamp};
use zeus_verify::audit::{AuditEntry, AuditOutcome, AuditSink};
fn fail() -> ApiError {
    ApiError::internal("durable cloud state unavailable")
}
fn missing() -> ApiError {
    ApiError::new("api.not_found", "cloud account, plan or delivery not found")
}
fn invalid(message: &str) -> ApiError {
    ApiError::invalid_params(message)
}
fn cloud_error(error: zeus_cloud::CloudError) -> ApiError {
    use zeus_cloud::CloudError::*;
    let (code, message) = match error {
        Unsupported(reason) => {
            return ApiError::new(
                "cloud.unsupported",
                "requested cloud capability is unavailable",
            )
            .with_detail("reason", serde_json::to_value(reason).unwrap_or_default());
        }
        InvalidRequest => return invalid("invalid cloud provider configuration or request"),
        CredentialUnavailable => (
            "cloud.credential_unavailable",
            "cloud credential is unavailable; a human must configure the account",
        ),
        Transport => (
            "cloud.transport_failed",
            "provider transport failed; reconcile the durable checkpoint before retrying",
        ),
        Http(status) => {
            return ApiError::new(
                match status {
                    401 => "cloud.provider_auth_failed",
                    403 => "cloud.provider_denied",
                    429 => "cloud.provider_throttled",
                    507 => "cloud.provider_quota",
                    _ => "cloud.operation_failed",
                },
                "cloud provider rejected the request; checkpoint retained",
            )
            .with_detail("http_status", status);
        }
        Provider(code) => {
            return ApiError::new(
                "cloud.operation_failed",
                "cloud provider reported a business error; checkpoint retained",
            )
            .with_detail("provider_code", code);
        }
        Protocol | UntrustedLocation | ResponseTooLarge => (
            "cloud.provider_protocol",
            "provider response did not satisfy the bounded cloud protocol",
        ),
        Source => ("cloud.source_unavailable", "cloud source read failed"),
        HashMismatch | SizeMismatch => (
            "cloud.integrity_failed",
            "local or remote content differs from the approved digest or size",
        ),
        CheckpointMismatch => (
            "cloud.checkpoint_mismatch",
            "checkpoint belongs to a different immutable operation",
        ),
        Cancelled => (
            "cloud.delivery_stopped",
            "local cloud transfer stopped; remote checkpoint retained",
        ),
        AlreadyExists => (
            "cloud.already_exists",
            "reserved destination already exists; Zeus will not overwrite it",
        ),
        ReconciliationRequired => (
            "cloud.reconciliation_required",
            "remote outcome remains uncertain; blind retry is refused",
        ),
        Checkpoint => (
            "cloud.checkpoint_unavailable",
            "private cloud checkpoint could not be committed",
        ),
        Process => (
            "cloud.operation_failed",
            "explicit cloud adapter failed; output was withheld",
        ),
    };
    ApiError::new(code, message)
}
fn hash<T: Serialize>(value: &T) -> ApiResult<Hex256> {
    crate::approvals::facts_hash(&serde_json::to_value(value).map_err(|_| fail())?)
}
fn id(prefix: &str) -> String {
    format!("{prefix}_{}", ulid::Ulid::generate())
}
fn validate_id(value: &str, prefix: &str) -> ApiResult<()> {
    let tail = value
        .strip_prefix(prefix)
        .and_then(|v| v.strip_prefix('_'))
        .ok_or_else(|| invalid("invalid cloud identifier"))?;
    if tail.len() != 26
        || !matches!(tail.as_bytes().first(), Some(b'0'..=b'7'))
        || !tail
            .bytes()
            .all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b))
        || ulid::Ulid::from_string(tail).is_err()
    {
        return Err(invalid("invalid cloud identifier"));
    }
    Ok(())
}
fn visible(owner: &str, identity: &ClientIdentity) -> bool {
    owner == identity.credential_id.as_str()
        || matches!(identity.kind, ClientKind::Human | ClientKind::Admin)
}
fn terminal(state: CloudDeliveryState) -> bool {
    matches!(
        state,
        CloudDeliveryState::Completed | CloudDeliveryState::Failed | CloudDeliveryState::Canceled
    )
}
#[async_trait]
trait ProviderFactory: Send + Sync {
    async fn connect(
        &self,
        config: &ProviderConfig,
        credential: Option<zeus_cloud::SecretString>,
        cancel: &CancellationToken,
    ) -> zeus_cloud::CloudResult<Arc<dyn CloudProvider>>;
}
struct ConfiguredFactory;
#[async_trait]
impl ProviderFactory for ConfiguredFactory {
    async fn connect(
        &self,
        config: &ProviderConfig,
        credential: Option<zeus_cloud::SecretString>,
        cancel: &CancellationToken,
    ) -> zeus_cloud::CloudResult<Arc<dyn CloudProvider>> {
        config.connect(credential, cancel).await
    }
}
struct Worker {
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
/// Owns private cloud state; API callers never receive checkpoints or credential references.
pub struct CloudService {
    store: Arc<Store>,
    journal: Journal,
    consent: Arc<crate::consent::ConsentManager>,
    audit: Arc<dyn AuditSink>,
    credentials: Arc<dyn CredentialStore>,
    factory: Arc<dyn ProviderFactory>,
    staging: PathBuf,
    gate: Mutex<()>,
    workers: std::sync::Mutex<BTreeMap<String, Worker>>,
    stopping: AtomicBool,
    /// When retention last ran; it runs at startup and then at most hourly.
    retained_at: std::sync::Mutex<Option<std::time::Instant>>,
}
/// Finished deliveries (and the plans they used) are kept this long after their last change.
const TERMINAL_RETENTION_MS: u64 = 30 * 86_400_000;
/// Idempotency receipts outlive the 24 h minimum of 05 section 12 by a week.
const RECEIPT_RETENTION_MS: u64 = 7 * 86_400_000;
impl CloudService {
    /// Open durable state without connecting to configured providers or reading their credentials.
    pub fn open(
        state: &Path,
        journal: Journal,
        consent: Arc<crate::consent::ConsentManager>,
        audit: Arc<dyn AuditSink>,
        credentials: Arc<dyn CredentialStore>,
    ) -> ApiResult<Arc<Self>> {
        Self::open_with_factory(
            state,
            journal,
            consent,
            audit,
            credentials,
            Arc::new(ConfiguredFactory),
        )
    }
    fn open_with_factory(
        state: &Path,
        journal: Journal,
        consent: Arc<crate::consent::ConsentManager>,
        audit: Arc<dyn AuditSink>,
        credentials: Arc<dyn CredentialStore>,
        factory: Arc<dyn ProviderFactory>,
    ) -> ApiResult<Arc<Self>> {
        let staging = state.join("cloud-staging");
        if staging.is_symlink() {
            return Err(fail());
        }
        std::fs::create_dir_all(&staging).map_err(|_| fail())?;
        let store = Arc::new(Store::open(&state.join("cloud.redb"))?);
        for key in store.keys("delivery:")? {
            let row: Delivery = store.get(&key)?.ok_or_else(missing)?;
            if row.view.state == CloudDeliveryState::Running {
                store.mutate_delivery(&row.view.delivery_id, |r| {
                    r.view.state = if r.checkpoint.is_some() {
                        CloudDeliveryState::Reconciling
                    } else {
                        CloudDeliveryState::Queued
                    };
                    r.view.reason = Some("cloud.restart_reconciliation".into());
                    Ok(())
                })?;
            }
        }
        Ok(Arc::new(Self {
            store,
            journal,
            consent,
            audit,
            credentials,
            factory,
            staging,
            gate: Mutex::new(()),
            workers: Default::default(),
            stopping: AtomicBool::new(false),
            retained_at: Default::default(),
        }))
    }
    /// Apply bounded retention at `now` (roadmap D05); audited when it removed anything.
    pub(crate) fn retain_at(&self, now: Timestamp) -> ApiResult<()> {
        let pruned = self.store.prune(
            now,
            TERMINAL_RETENTION_MS,
            RECEIPT_RETENTION_MS,
            &self.staging,
        )?;
        if pruned.deliveries + pruned.plans + pruned.receipts > 0 {
            let mut entry = AuditEntry::new(
                Timestamp::now(),
                Actor::system(),
                "cloud.retention",
                AuditOutcome::Ok,
            );
            entry
                .data
                .insert("deliveries".into(), serde_json::json!(pruned.deliveries));
            entry
                .data
                .insert("plans".into(), serde_json::json!(pruned.plans));
            entry
                .data
                .insert("receipts".into(), serde_json::json!(pruned.receipts));
            self.audit.append(entry).map_err(|_| fail())?;
        }
        Ok(())
    }
    fn retain_hourly(&self) -> ApiResult<()> {
        let due = self
            .retained_at
            .lock()
            .map_err(|_| fail())?
            .is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(3600));
        if due {
            self.retain_at(Timestamp::now())?;
            *self.retained_at.lock().map_err(|_| fail())? = Some(std::time::Instant::now());
        }
        Ok(())
    }
    fn admit(&self) -> ApiResult<()> {
        if self.stopping.load(Ordering::Acquire) {
            return Err(ApiError::new("api.unavailable", "daemon is stopping"));
        }
        if self
            .journal
            .meta()
            .map_err(|_| fail())?
            .kill_switch_engaged()
        {
            return Err(ApiError::new(
                "policy.kill_switch_engaged",
                "kill switch engaged",
            ));
        }
        Ok(())
    }
    fn audit(&self, actor: Actor, action: &str, data: serde_json::Value) -> ApiResult<()> {
        let mut entry = AuditEntry::new(Timestamp::now(), actor, action, AuditOutcome::Ok);
        entry.write_ahead = true;
        entry.data = data.as_object().cloned().ok_or_else(fail)?;
        self.audit.append(entry).map_err(|_| fail())?;
        Ok(())
    }
    fn receipt(
        &self,
        identity: &ClientIdentity,
        method: &str,
        key: &str,
        params: &impl Serialize,
    ) -> ApiResult<(String, String, Option<Receipt>)> {
        let binding =
            crate::mutations::MutationStore::binding(identity.credential_id.as_str(), method, key)?;
        let key = format!("receipt:{binding}");
        let hash = hash(params)?.to_string();
        let old = self.store.get::<Receipt>(&key)?;
        if old.as_ref().is_some_and(|r| r.hash != hash) {
            return Err(ApiError::new(
                "api.idempotency_conflict",
                "idempotency key already binds different cloud parameters",
            ));
        }
        Ok((key, hash, old))
    }
    fn account(&self, id: &str) -> ApiResult<Account> {
        self.store
            .get(&format!("account:{id}"))?
            .ok_or_else(missing)
    }
    fn delivery(&self, id: &str, identity: &ClientIdentity) -> ApiResult<Delivery> {
        validate_id(id, "cld")?;
        let row: Delivery = self
            .store
            .get(&format!("delivery:{id}"))?
            .ok_or_else(missing)?;
        if !visible(&row.owner, identity) {
            return Err(missing());
        }
        Ok(row)
    }
    /// List nonsecret configured account scopes.
    pub fn accounts(&self) -> ApiResult<CloudAccountsListResult> {
        Ok(CloudAccountsListResult {
            accounts: self
                .store
                .list::<Account>("account:")?
                .into_iter()
                .map(|a| a.view)
                .collect(),
        })
    }
    /// Configure an account after the backend independently verifies a physical terminal.
    pub async fn configure(
        &self,
        identity: &ClientIdentity,
        actor: Actor,
        mut p: CloudAccountsConfigureParams,
    ) -> ApiResult<CloudAccountMutationResult> {
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
        if p.account_id.is_empty()
            || p.account_id.len() > 64
            || !p
                .account_id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
            || p.label.chars().count() > 128
            || p.label.chars().any(char::is_control)
            || p.max_bytes_per_delivery == 0
            || p.max_bytes_per_delivery > 9_007_199_254_740_991
        {
            return Err(invalid("invalid account alias, label or byte ceiling"));
        }
        p.config.validate().map_err(cloud_error)?;
        if let ProviderConfig::Rclone { config } = &mut p.config {
            let config_path = config.config_file.clone();
            let executable = config.executable.clone();
            let pins = tokio::task::spawn_blocking(move || -> ApiResult<(String, String)> {
                fn digest(path: &Path, limit: u64) -> ApiResult<String> {
                    let meta = std::fs::symlink_metadata(path)
                        .map_err(|_| invalid("rclone authorized file cannot be read"))?;
                    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > limit {
                        return Err(invalid(
                            "rclone authorized file is not a bounded regular file",
                        ));
                    }
                    let (digests, _) =
                        zeus_verify::hash::verify_file(path, &Default::default(), &mut |_| true)
                            .map_err(|_| invalid("rclone authorized file could not be verified"))?;
                    Ok(digests.sha256.to_string())
                }
                Ok((
                    digest(&config_path, 1024 * 1024)?,
                    digest(&executable, 512 * 1024 * 1024)?,
                ))
            })
            .await
            .map_err(|_| fail())??;
            config.config_sha256 = Some(pins.0);
            config.executable_sha256 = Some(pins.1);
        }
        let binding = serde_json::json!({"account_id":p.account_id,"label":p.label,"config":p.config,"max_bytes_per_delivery":p.max_bytes_per_delivery,"expected_revision":p.expected_revision,"credential_hash":p.credential.as_ref().map(|s|blake3::hash(s.expose().as_bytes()).to_hex().to_string())});
        let (key, hash, old) = self.receipt(
            identity,
            "cloud.accounts.configure",
            &p.idempotency_key,
            &binding,
        )?;
        if let Some(old) = old {
            let mut result: CloudAccountMutationResult =
                serde_json::from_value(old.result).map_err(|_| fail())?;
            result.replayed = true;
            return Ok(result);
        }
        let old = self
            .store
            .get::<Account>(&format!("account:{}", p.account_id))?;
        if old.is_none() && self.store.keys("account:")?.len() >= 64 {
            return Err(store::capacity());
        }
        if old.as_ref().map(|a| a.view.revision) != p.expected_revision {
            return Err(ApiError::new("api.conflict", "account revision changed"));
        }
        let revision = old
            .as_ref()
            .map_or(1, |a| a.view.revision.saturating_add(1));
        let needs_secret = !matches!(p.config, ProviderConfig::Rclone { .. });
        let credential = match p.credential {
            Some(s) => {
                if s.is_empty() || s.len() > 16384 {
                    return Err(invalid("invalid credential length"));
                }
                Some(zeus_cloud::SecretString::new(s.expose().to_owned()))
            }
            None => old
                .as_ref()
                .and_then(|a| a.credential_key.as_ref())
                .map(|key| self.credentials.get(key))
                .transpose()
                .map_err(cloud_error)?,
        };
        if needs_secret && credential.is_none() {
            return Err(ApiError::new(
                "cloud.credential_unavailable",
                "native providers require an explicitly supplied credential",
            ));
        }
        self.admit()?;
        let cancel = CancellationToken::new();
        let provider = self
            .factory
            .connect(&p.config, credential.clone(), &cancel)
            .await
            .map_err(cloud_error)?;
        let destination = match &p.config {
            ProviderConfig::GoogleDrive { parent_id } => format!("drive:{parent_id}"),
            ProviderConfig::YandexDisk { root_path } => root_path.clone(),
            ProviderConfig::Rclone { config } => format!("{}:{}", config.remote, config.root_path),
            ProviderConfig::BaiduNetdisk { root_path } => root_path.clone(),
        };
        let view = CloudAccountView {
            account_id: p.account_id.clone(),
            label: p.label,
            revision,
            provider: p.config.kind(),
            destination,
            capabilities: provider.capabilities(),
            max_bytes_per_delivery: p.max_bytes_per_delivery,
            revoked: false,
        };
        self.audit(actor,"cloud.account_configured",serde_json::json!({"account_id":p.account_id,"revision":revision,"provider":view.provider,"max_bytes_per_delivery":view.max_bytes_per_delivery}))?;
        let credential_key = if let Some(secret) = credential {
            let key = format!("{}-r{revision}", p.account_id);
            self.credentials.put(&key, &secret).map_err(cloud_error)?;
            Some(key)
        } else {
            None
        };
        let account = Account {
            view: view.clone(),
            config: p.config,
            credential_key,
        };
        let result = CloudAccountMutationResult {
            account: view,
            replayed: false,
        };
        self.store.save_many(&[
            encoded(format!("account:{}", p.account_id), &account)?,
            encoded(
                key,
                &Receipt {
                    created_at: Some(Timestamp::now()),
                    hash,
                    result: serde_json::to_value(&result).map_err(|_| fail())?,
                },
            )?,
        ])?;
        self.stop_account(&p.account_id, "cloud.account_changed")?;
        if let Some(key) = old.and_then(|a| a.credential_key) {
            let _ = self.credentials.delete(&key);
        }
        Ok(result)
    }
    /// Revoke an account and invalidate all approvals that bind its old revision.
    pub async fn revoke(
        &self,
        identity: &ClientIdentity,
        actor: Actor,
        p: CloudAccountsRevokeParams,
    ) -> ApiResult<CloudAccountMutationResult> {
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
        let (key, hash, old) =
            self.receipt(identity, "cloud.accounts.revoke", &p.idempotency_key, &p)?;
        if let Some(old) = old {
            let mut result: CloudAccountMutationResult =
                serde_json::from_value(old.result).map_err(|_| fail())?;
            result.replayed = true;
            return Ok(result);
        }
        let mut account = self.account(&p.account_id)?;
        if account.view.revision != p.expected_revision {
            return Err(ApiError::new("api.conflict", "account revision changed"));
        }
        self.audit(
            actor,
            "cloud.account_revoked",
            serde_json::json!({"account_id":p.account_id,"revision":p.expected_revision}),
        )?;
        account.view.revoked = true;
        account.view.revision = account.view.revision.saturating_add(1);
        let secret_key = account.credential_key.take();
        let result = CloudAccountMutationResult {
            account: account.view.clone(),
            replayed: false,
        };
        self.store.save_many(&[
            encoded(format!("account:{}", p.account_id), &account)?,
            encoded(
                key,
                &Receipt {
                    created_at: Some(Timestamp::now()),
                    hash,
                    result: serde_json::to_value(&result).map_err(|_| fail())?,
                },
            )?,
        ])?;
        self.stop_account(&p.account_id, "cloud.account_revoked")?;
        if let Some(key) = secret_key {
            let _ = self.credentials.delete(&key);
        }
        Ok(result)
    }
    fn stop_account(&self, id: &str, reason: &str) -> ApiResult<()> {
        for key in self.store.keys("delivery:")? {
            let row: Delivery = self.store.get(&key)?.ok_or_else(missing)?;
            if row.view.plan.account_id == id && !terminal(row.view.state) {
                self.stop_delivery(&row.view.delivery_id, CloudControlAction::Pause, reason)?;
            }
        }
        Ok(())
    }
    /// Build and persist an immutable plan without consuming source bytes or mutating a provider.
    pub async fn plan(
        &self,
        identity: &ClientIdentity,
        p: CloudPlanParams,
    ) -> ApiResult<CloudPlanResult> {
        let _gate = self.gate.lock().await;
        self.admit()?;
        let (key, request_hash, old) =
            self.receipt(identity, "cloud.plan", &p.idempotency_key, &p)?;
        if let Some(old) = old {
            let mut result: CloudPlanResult =
                serde_json::from_value(old.result).map_err(|_| fail())?;
            result.replayed = true;
            return Ok(result);
        }
        let plans = self.store.map::<Plan, bool>("plan:", |plan| {
            Some(
                plan.owner == identity.credential_id.as_str()
                    && plan.view.expires_at > Timestamp::now(),
            )
        })?;
        if plans.len() >= 1024 || plans.into_iter().filter(|active| *active).count() >= 128 {
            return Err(store::capacity());
        }
        let account = self.account(&p.account_id)?;
        if account.view.revoked {
            return Err(ApiError::new(
                "cloud.account_revoked",
                "cloud account is revoked",
            ));
        }
        if p.max_seconds == 0
            || p.max_seconds > 86400
            || p.max_bytes == 0
            || p.max_bytes > account.view.max_bytes_per_delivery
        {
            return Err(invalid(
                "delivery exceeds account byte ceiling or time limit",
            ));
        }
        let (size, digest, redacted, blocks) = match &p.source {
            CloudSource::CompletedJob { job_id } => {
                let record = self
                    .journal
                    .job(*job_id)
                    .map_err(|_| fail())?
                    .ok_or_else(missing)?;
                if record.job.state != JobState::Completed || record.job.quarantined {
                    return Err(invalid(
                        "source must be a completed, non-quarantined download",
                    ));
                }
                if !visible(
                    record.job.spec.source.client_id.as_deref().unwrap_or(""),
                    identity,
                ) {
                    return Err(missing());
                }
                let result = record
                    .job
                    .result
                    .ok_or_else(|| invalid("completed download has no verified result"))?;
                if p.mode != DeliveryMode::StagedUpload {
                    return Err(invalid("completed jobs require staged_upload mode"));
                }
                (
                    result.size_bytes,
                    Some(result.sha256),
                    format!("job:{job_id}"),
                    None,
                )
            }
            CloudSource::PublicUrl {
                url,
                size_bytes,
                sha256,
                block_md5,
            } => {
                source::validate_url(url, p.mode == DeliveryMode::RemoteImport)?;
                if let Some(hashes) = block_md5
                    && (hashes.len() > 8192
                        || hashes.len() as u64 != (*size_bytes).div_ceil(4 * 1024 * 1024).max(1)
                        || hashes
                            .iter()
                            .any(|h| h.len() != 32 || !h.bytes().all(|b| b.is_ascii_hexdigit())))
                {
                    return Err(invalid("invalid bounded block checksum manifest"));
                }
                (
                    *size_bytes,
                    *sha256,
                    url.origin().to_string(),
                    block_md5.clone(),
                )
            }
        };
        if account.view.provider == ProviderKind::BaiduNetdisk && size > 20 * 1024 * 1024 * 1024 {
            return Err(invalid("Baidu delivery exceeds supported 20 GiB limit"));
        }
        if size > p.max_bytes || size > 9_007_199_254_740_991 {
            return Err(invalid("source exceeds approved byte ceiling"));
        }
        if p.mode == DeliveryMode::RemoteImport && !p.allow_unbounded_remote {
            return Err(invalid(
                "remote_import requires explicit acceptance of provider byte-cap and cancellation limitations",
            ));
        }
        let delivery_id = id("cld");
        let upload = zeus_cloud::UploadRequest {
            operation_id: delivery_id.clone(),
            mode: p.mode,
            name: p.remote_name,
            size,
            expected_sha256: digest.map(|h| h.to_string()),
            block_md5: blocks,
        };
        upload.validate().map_err(cloud_error)?;
        // Staged URL hashing occurs after approval and before upload; no provider write is allowed without that digest.
        let staged_placeholder = "0000000000000000000000000000000000000000000000000000000000000000";
        let capability = account.view.capabilities.plan(
            p.mode,
            Some(size),
            upload
                .expected_sha256
                .as_deref()
                .or(if p.mode == DeliveryMode::StagedUpload {
                    Some(staged_placeholder)
                } else {
                    None
                }),
            false,
        );
        let mut unsupported = match capability {
            Ok(()) => None,
            Err(zeus_cloud::CloudError::Unsupported(reason)) => Some(reason),
            Err(e) => return Err(cloud_error(e)),
        };
        if account.view.provider == ProviderKind::BaiduNetdisk
            && p.mode == DeliveryMode::StreamUpload
            && upload.block_md5.is_none()
        {
            unsupported = Some(CapabilityReason::KnownBlockHashesRequired);
        }
        let now = Timestamp::now();
        let mut view = CloudPlanView {
            plan_id: id("clp"),
            delivery_id,
            plan_hash: Hex256::from_bytes([0; 32]),
            account_id: p.account_id,
            account_revision: account.view.revision,
            provider: account.view.provider,
            mode: p.mode,
            source_redacted: redacted,
            source_hash: hash(&p.source)?,
            destination: account.view.destination,
            remote_name: upload.unique_name(),
            size_bytes: size,
            max_bytes: p.max_bytes,
            max_seconds: p.max_seconds,
            expected_sha256: digest,
            supported: unsupported.is_none(),
            unsupported_reason: unsupported,
            allow_unbounded_remote: p.allow_unbounded_remote,
            remote_cancellation_supported: account.view.capabilities.remote_cancel,
            hard_byte_cap: p.mode != DeliveryMode::RemoteImport,
            expires_at: now.saturating_add_ms(86400 * 1000),
        };
        let mut facts = serde_json::to_value(&view).map_err(|_| fail())?;
        facts.as_object_mut().ok_or_else(fail)?.remove("plan_hash");
        view.plan_hash = hash(&facts)?;
        let plan = Plan {
            owner: identity.credential_id.to_string(),
            view: view.clone(),
            source: p.source,
            upload,
        };
        let result = CloudPlanResult {
            plan: view,
            replayed: false,
        };
        self.store.save_many(&[
            encoded(format!("plan:{}", plan.view.plan_id), &plan)?,
            encoded(
                key,
                &Receipt {
                    created_at: Some(Timestamp::now()),
                    hash: request_hash,
                    result: serde_json::to_value(&result).map_err(|_| fail())?,
                },
            )?,
        ])?;
        Ok(result)
    }
    /// Reserve one delivery per immutable plan, then create its idempotent human approval.
    pub async fn submit(
        &self,
        identity: &ClientIdentity,
        p: CloudSubmitParams,
    ) -> ApiResult<CloudMutationResult> {
        let _gate = self.gate.lock().await;
        self.admit()?;
        validate_id(&p.plan_id, "clp")?;
        let (key, hash, old) = self.receipt(identity, "cloud.submit", &p.idempotency_key, &p)?;
        let plan: Plan = self
            .store
            .get(&format!("plan:{}", p.plan_id))?
            .ok_or_else(missing)?;
        if plan.owner != identity.credential_id.as_str() {
            return Err(missing());
        }
        if plan.view.plan_hash != p.plan_hash {
            return Err(ApiError::new(
                "policy.request_hash_mismatch",
                "cloud plan hash changed",
            ));
        }
        if !plan.view.supported {
            return Err(ApiError::new(
                "cloud.unsupported",
                "requested mode is unavailable for this plan",
            ));
        }
        let account = self.account(&plan.view.account_id)?;
        if account.view.revoked || account.view.revision != plan.view.account_revision {
            return Err(ApiError::new(
                "cloud.account_changed",
                "account scope changed; create a new plan",
            ));
        }
        let mut row = self
            .store
            .get::<Delivery>(&format!("delivery:{}", plan.view.delivery_id))?;
        let newly_reserved = row.is_none();
        if newly_reserved
            && self
                .store
                .map::<Delivery, bool>("delivery:", |row| Some(!terminal(row.view.state)))?
                .into_iter()
                .filter(|active| *active)
                .count()
                >= 128
        {
            return Err(store::capacity());
        }
        if newly_reserved {
            self.audit(identity.action_actor(),"cloud.delivery_requested",serde_json::json!({"delivery_id":plan.view.delivery_id,"plan_hash":plan.view.plan_hash,"account_id":plan.view.account_id,"account_revision":plan.view.account_revision,"mode":plan.view.mode}))?;
        }
        if row.is_none() {
            if plan.view.expires_at <= Timestamp::now() {
                return Err(ApiError::new(
                    "policy.approval_expired",
                    "cloud plan expired",
                ));
            }
            let now = Timestamp::now();
            row = Some(Delivery {
                owner: plan.owner,
                view: CloudDeliveryView {
                    delivery_id: plan.view.delivery_id.clone(),
                    plan: plan.view,
                    state: CloudDeliveryState::AwaitingApproval,
                    approval_id: None,
                    committed_bytes: 0,
                    artifact: None,
                    reason: None,
                    cleanup_pending: false,
                    created_at: now,
                    updated_at: now,
                },
                source: plan.source,
                upload: plan.upload,
                checkpoint: None,
                started_at: None,
                stop: None,
                staged_sha256: None,
            });
        }
        let mut row = row.ok_or_else(fail)?;
        let reserved = CloudMutationResult {
            delivery: row.view.clone(),
            replayed: false,
        };
        let receipt = encoded(
            key.clone(),
            &Receipt {
                created_at: Some(Timestamp::now()),
                hash: hash.clone(),
                result: serde_json::to_value(&reserved).map_err(|_| fail())?,
            },
        )?;
        if newly_reserved {
            self.store.save_many(&[
                encoded(format!("delivery:{}", row.view.delivery_id), &row)?,
                receipt,
            ])?;
        } else {
            self.store.save_many(&[receipt])?;
        }
        self.ensure_approval(&mut row).await?;
        let result = CloudMutationResult {
            delivery: row.view,
            replayed: old.is_some(),
        };
        self.store.save(
            &key,
            &Receipt {
                created_at: Some(Timestamp::now()),
                hash,
                result: serde_json::to_value(&result).map_err(|_| fail())?,
            },
        )?;
        Ok(result)
    }
    async fn ensure_approval(&self, row: &mut Delivery) -> ApiResult<()> {
        if row.view.approval_id.is_none() {
            let approval = self
                .consent
                .request_cloud(&row.view.plan, &row.owner)
                .await?;
            row.view.approval_id = Some(approval.approval.id);
            self.store
                .mutate_delivery(&row.view.delivery_id, |stored| {
                    stored.view.approval_id = row.view.approval_id;
                    Ok(())
                })?;
        }
        Ok(())
    }
    /// Read one owner-isolated delivery.
    pub fn get(
        &self,
        identity: &ClientIdentity,
        p: CloudGetParams,
    ) -> ApiResult<CloudDeliveryView> {
        Ok(self.delivery(&p.delivery_id, identity)?.view)
    }
    /// Bounded newest-first list, with agent ownership isolation.
    pub fn list(
        &self,
        identity: &ClientIdentity,
        p: CloudListParams,
    ) -> ApiResult<CloudListResult> {
        if !(1..=100).contains(&p.limit) {
            return Err(invalid("limit must be 1 to 100"));
        }
        let mut rows = self
            .store
            .map::<Delivery, CloudDeliveryView>("delivery:", |r| {
                visible(&r.owner, identity).then_some(r.view)
            })?;
        rows.sort_by(|a, b| b.delivery_id.cmp(&a.delivery_id));
        rows.truncate(p.limit as usize);
        Ok(CloudListResult { deliveries: rows })
    }
    /// Controls an existing operation without granting any new authority.
    pub async fn control(
        &self,
        identity: &ClientIdentity,
        p: CloudControlParams,
    ) -> ApiResult<CloudMutationResult> {
        let _gate = self.gate.lock().await;
        let (key, hash, old) = self.receipt(identity, "cloud.control", &p.idempotency_key, &p)?;
        let row = self.delivery(&p.delivery_id, identity)?;
        if let Some(old) = old {
            let mut result: CloudMutationResult =
                serde_json::from_value(old.result).map_err(|_| fail())?;
            result.replayed = true;
            return Ok(result);
        }
        if terminal(row.view.state) {
            return Err(ApiError::new(
                "api.invalid_transition",
                "cloud delivery is terminal",
            ));
        }
        if p.action == CloudControlAction::Resume {
            self.admit()?;
            self.authorized(&row)?;
        }
        self.audit(identity.action_actor(),"cloud.delivery_controlled",serde_json::json!({"delivery_id":p.delivery_id,"action":p.action,"plan_hash":row.view.plan.plan_hash}))?;
        let result=self.store.control(&p.delivery_id,&key,hash,|current|{
            if terminal(current.view.state){return Err(ApiError::new("api.invalid_transition","cloud delivery is terminal"));}
            if p.action==CloudControlAction::Resume{
                if matches!(current.view.state,CloudDeliveryState::Running|CloudDeliveryState::RemotePending|CloudDeliveryState::RemoteCancellationUnavailable){return Err(ApiError::new("api.invalid_transition","cloud operation is already active or remote cancellation remains unresolved"));}
                current.stop=None;current.view.state=if current.checkpoint.is_some(){CloudDeliveryState::Reconciling}else{CloudDeliveryState::Queued};current.view.reason=None;
            }else{
                current.stop=Some(p.action);current.view.reason=Some("cloud.user_requested_stop".into());
                current.view.state=if current.view.plan.mode==DeliveryMode::RemoteImport&&current.checkpoint.is_some(){CloudDeliveryState::RemoteCancellationUnavailable}else if p.action==CloudControlAction::Cancel&&current.checkpoint.is_none(){CloudDeliveryState::Canceled}else{CloudDeliveryState::Paused};
            }
            if current.view.state==CloudDeliveryState::Canceled&&current.view.plan.mode==DeliveryMode::StagedUpload&&matches!(current.source,CloudSource::PublicUrl{..}){current.view.cleanup_pending=true;}
            Ok(())
        })?;
        if p.action != CloudControlAction::Resume
            && let Some(worker) = self.workers.lock().map_err(|_| fail())?.get(&p.delivery_id)
        {
            worker.cancel.cancel();
        }
        Ok(result)
    }

    fn stop_delivery(&self, id: &str, action: CloudControlAction, reason: &str) -> ApiResult<()> {
        self.store.mutate_delivery(id, |r| {
            if terminal(r.view.state) {
                return Ok(());
            }
            r.stop = Some(action);
            r.view.reason = Some(reason.into());
            r.view.state =
                if r.view.plan.mode == DeliveryMode::RemoteImport && r.checkpoint.is_some() {
                    CloudDeliveryState::RemoteCancellationUnavailable
                } else if action == CloudControlAction::Cancel && r.checkpoint.is_none() {
                    CloudDeliveryState::Canceled
                } else {
                    CloudDeliveryState::Paused
                };
            if r.view.state == CloudDeliveryState::Canceled
                && r.view.plan.mode == DeliveryMode::StagedUpload
                && matches!(r.source, CloudSource::PublicUrl { .. })
            {
                r.view.cleanup_pending = true;
            }
            Ok(())
        })?;
        if let Some(worker) = self.workers.lock().map_err(|_| fail())?.get(id) {
            worker.cancel.cancel();
        }
        Ok(())
    }
    /// Kill switch pauses local work and retains all uncertain remote receipts.
    pub async fn pause_all_for_kill_switch(&self) -> ApiResult<()> {
        let _gate = self.gate.lock().await;
        for key in self.store.keys("delivery:")? {
            let row: Delivery = self.store.get(&key)?.ok_or_else(missing)?;
            if !terminal(row.view.state) {
                self.stop_delivery(
                    &row.view.delivery_id,
                    CloudControlAction::Pause,
                    "policy.kill_switch_engaged",
                )?;
            }
        }
        Ok(())
    }
    /// Stop all local cloud workers at daemon shutdown; private checkpoints remain durable.
    pub async fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        let workers = match self.workers.lock() {
            Ok(mut workers) => std::mem::take(&mut *workers),
            Err(_) => return,
        };
        for worker in workers.values() {
            worker.cancel.cancel();
        }
        for (_, worker) in workers {
            let _ = worker.task.await;
        }
    }
}
