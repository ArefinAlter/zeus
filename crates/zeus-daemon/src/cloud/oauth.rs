//! Human-gated cloud OAuth lifecycle. Secret flow state is bounded, volatile and never audited.
use super::*;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use zeus_api::methods::cloud_oauth::*;
use zeus_cloud::oauth::{
    self, CodeExchange, DeviceAuthorization, OAuthError, OAuthHttp, OAuthProof, OAuthRegistration,
    OAuthTokens,
};

/// Nonsecret provider registration attached atomically to the destination scope.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct OAuthAccount {
    pub registration: OAuthRegistration,
    pub device_id: String,
    #[serde(default)]
    pub refresh_uncertain: bool,
}
pub(super) struct OAuthManager {
    pub(super) http: OAuthHttp,
    flows: Mutex<BTreeMap<String, Arc<FlowSlot>>>,
    locks: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
    network_epoch: Mutex<CancellationToken>,
}
struct FlowSlot {
    owner: String,
    request_hash: Hex256,
    request_key: String,
    max_deadline: Instant,
    cancel: CancellationToken,
    flow: Mutex<Flow>,
}
struct Flow {
    initialized: bool,
    params: CloudOAuthBeginParams,
    proof: OAuthProof,
    device_id: String,
    device: Option<DeviceAuthorization>,
    authorization_url: zeus_cloud::SecretString,
    expires_at: Timestamp,
    deadline: Instant,
    next_poll: Instant,
    interval_ms: u64,
    status: CloudOAuthState,
    account: Option<CloudAccountView>,
    token_expires_at: Option<Timestamp>,
    refresh_available: bool,
    local_cleanup_pending: bool,
}
impl OAuthManager {
    pub fn new() -> ApiResult<Self> {
        Ok(Self {
            http: OAuthHttp::new().map_err(oauth_error)?,
            flows: Mutex::new(BTreeMap::new()),
            locks: Mutex::new(BTreeMap::new()),
            network_epoch: Mutex::new(CancellationToken::new()),
        })
    }
    pub(super) async fn cancel_flows(&self) {
        for slot in self.flows.lock().await.values() {
            slot.cancel.cancel();
        }
        let mut epoch = self.network_epoch.lock().await;
        epoch.cancel();
        *epoch = CancellationToken::new();
    }
    async fn slot(&self, id: &str, identity: &ClientIdentity) -> ApiResult<Arc<FlowSlot>> {
        self.flows
            .lock()
            .await
            .get(id)
            .filter(|slot| slot.owner == identity.credential_id.as_str())
            .cloned()
            .ok_or_else(missing)
    }
    async fn lock(&self, key: &str) -> Arc<Mutex<()>> {
        let mut locks = self.locks.lock().await;
        locks
            .entry(key.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}
pub(super) fn oauth_error(value: OAuthError) -> ApiError {
    let code = match value {
        OAuthError::Invalid => "api.invalid_params",
        OAuthError::Unsupported => "cloud.oauth_unsupported",
        OAuthError::Denied => "cloud.oauth_denied",
        OAuthError::Expired => "cloud.oauth_expired",
        OAuthError::Pending => "cloud.oauth_pending",
        OAuthError::SlowDown => "cloud.oauth_slow_down",
        OAuthError::SignInRequired => "cloud.oauth_sign_in_required",
        OAuthError::Protocol => "cloud.oauth_protocol",
        OAuthError::Transport => "cloud.oauth_transport",
        OAuthError::Canceled => "cloud.oauth_canceled",
    };
    ApiError::new(code, value.to_string())
}
fn human(identity: &ClientIdentity, actor: &Actor) -> ApiResult<()> {
    if !matches!(identity.kind, ClientKind::Human | ClientKind::Admin)
        || !actor.is_human_on_trust_surface()
        || !matches!(
            actor.surface,
            Some(zeus_types::Surface::CliInteractive | zeus_types::Surface::Tui)
        )
    {
        return Err(ApiError::human_required("verified human terminal"));
    }
    Ok(())
}
fn project(id: &str, flow: &Flow) -> CloudOAuthStatus {
    CloudOAuthStatus {
        flow_id: Some(id.to_owned()),
        account_id: flow.params.account_id.clone(),
        state: if flow.status == CloudOAuthState::Waiting && Instant::now() >= flow.deadline {
            CloudOAuthState::Expired
        } else {
            flow.status
        },
        expires_at: Some(flow.expires_at),
        token_expires_at: flow.token_expires_at,
        refresh_available: flow.refresh_available,
        local_cleanup_pending: flow.local_cleanup_pending,
        account: flow.account.clone(),
    }
}
impl CloudService {
    /// Begin reviewed provider authorization; no account scope changes before token completion.
    pub async fn oauth_begin(
        &self,
        identity: &ClientIdentity,
        actor: Actor,
        p: CloudOAuthBeginParams,
    ) -> ApiResult<CloudOAuthBeginResult> {
        human(identity, &actor)?;
        self.admit()?;
        p.registration.validate().map_err(oauth_error)?;
        p.config.validate().map_err(cloud_error)?;
        if p.config.kind() != p.registration.provider
            || p.account_id.is_empty()
            || p.account_id.len() > 64
            || !p
                .account_id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
            || p.label.chars().count() > 128
            || p.label.chars().any(char::is_control)
            || p.max_bytes_per_delivery == 0
            || p.max_bytes_per_delivery > 9_007_199_254_740_991
            || p.idempotency_key.is_empty()
            || p.idempotency_key.len() > 128
            || p.idempotency_key.chars().any(char::is_control)
            || p.client_secret.as_ref().is_some_and(|s| {
                s.is_empty() || s.len() > 8192 || s.expose().chars().any(char::is_control)
            })
        {
            return Err(invalid(
                "invalid OAuth account, app registration or destination scope",
            ));
        }
        let old = self
            .store
            .get::<Account>(&format!("account:{}", p.account_id))?;
        if old.as_ref().map(|a| a.view.revision) != p.expected_revision {
            return Err(ApiError::new("api.conflict", "account revision changed"));
        }
        if old.is_none() && self.store.keys("account:")?.len() >= 64 {
            return Err(store::capacity());
        }
        if matches!(
            p.registration.provider,
            zeus_cloud::ProviderKind::YandexDisk | zeus_cloud::ProviderKind::BaiduNetdisk
        ) && p.client_secret.is_none()
        {
            return Err(invalid(
                "this provider needs its registered app password for refresh/revocation; enter it at the controlling terminal",
            ));
        }
        match p.registration.provider {
            zeus_cloud::ProviderKind::BaiduNetdisk if p.redirect_uri.is_none() => {}
            zeus_cloud::ProviderKind::GoogleDrive | zeus_cloud::ProviderKind::YandexDisk => {
                oauth::validate_redirect(
                    p.redirect_uri
                        .as_deref()
                        .ok_or_else(|| invalid("CLI loopback redirect required"))?,
                )
                .map_err(oauth_error)?;
            }
            _ => return Err(invalid("unsupported OAuth flow or redirect")),
        }
        let binding = hash(
            &serde_json::json!({"account_id":p.account_id,"label":p.label,"config":p.config,
            "max_bytes_per_delivery":p.max_bytes_per_delivery,"expected_revision":p.expected_revision,
            "registration":p.registration,"redirect_uri":p.redirect_uri,
            "client_secret_hash":p.client_secret.as_ref().map(|s|blake3::hash(s.expose().as_bytes()).to_hex().to_string())}),
        )?;
        let (flow_id, slot) = {
            let mut flows = self.oauth.flows.lock().await;
            flows.retain(|_, slot| {
                let keep = Instant::now() < slot.max_deadline;
                if !keep {
                    slot.cancel.cancel();
                }
                keep
            });
            if let Some((id, slot)) = flows.iter().find(|(_, slot)| {
                slot.owner == identity.credential_id.as_str()
                    && slot.request_key == p.idempotency_key
            }) {
                if slot.request_hash != binding {
                    return Err(ApiError::new(
                        "api.idempotency_conflict",
                        "OAuth intent changed",
                    ));
                }
                (id.clone(), slot.clone())
            } else {
                if flows.len() >= 32 {
                    return Err(store::capacity());
                }
                let flow_id = id("cof");
                let deadline = Instant::now() + std::time::Duration::from_secs(600);
                let device_id = old
                    .as_ref()
                    .and_then(|a| a.oauth.as_ref())
                    .map(|m| m.device_id.clone())
                    .unwrap_or_else(|| id("zeus"));
                let slot = Arc::new(FlowSlot {
                    owner: identity.credential_id.to_string(),
                    request_hash: binding,
                    request_key: p.idempotency_key.clone(),
                    max_deadline: deadline,
                    cancel: CancellationToken::new(),
                    flow: Mutex::new(Flow {
                        initialized: false,
                        params: p,
                        proof: OAuthProof::generate(),
                        device_id,
                        device: None,
                        authorization_url: zeus_cloud::SecretString::new(String::new()),
                        expires_at: Timestamp::now().saturating_add_ms(600_000),
                        deadline,
                        next_poll: Instant::now(),
                        interval_ms: 0,
                        status: CloudOAuthState::Waiting,
                        account: None,
                        token_expires_at: None,
                        refresh_available: false,
                        local_cleanup_pending: false,
                    }),
                });
                flows.insert(flow_id.clone(), slot.clone());
                (flow_id, slot)
            }
        };
        // Only this flow is locked during provider I/O; cancellation can signal its slot immediately.
        let mut flow = slot.flow.lock().await;
        if !flow.initialized {
            flow.initialized = true;
            let initialized: ApiResult<()> = async {
                if flow.params.registration.provider == zeus_cloud::ProviderKind::BaiduNetdisk {
                    let device = self
                        .oauth
                        .http
                        .device(&flow.params.registration, &slot.cancel)
                        .await
                        .map_err(oauth_error)?;
                    let ttl = device.expires_in_ms.min(
                        slot.max_deadline
                            .saturating_duration_since(Instant::now())
                            .as_millis() as u64,
                    );
                    flow.deadline = Instant::now() + std::time::Duration::from_millis(ttl);
                    flow.expires_at = Timestamp::now().saturating_add_ms(ttl);
                    flow.interval_ms = device.interval_ms;
                    flow.next_poll =
                        Instant::now() + std::time::Duration::from_millis(device.interval_ms);
                    flow.authorization_url = device.verification_uri.clone();
                    flow.device = Some(device);
                } else {
                    flow.authorization_url = oauth::authorization_url(
                        &flow.params.registration,
                        flow.params.redirect_uri.as_deref().unwrap_or_default(),
                        &flow.proof,
                        &flow.device_id,
                    )
                    .map_err(oauth_error)?;
                }
                Ok(())
            }
            .await;
            if let Err(error) = initialized {
                flow.status = CloudOAuthState::Failed;
                return Err(error);
            }
        }
        if slot.cancel.is_cancelled() {
            flow.status = CloudOAuthState::Canceled;
            return Err(oauth_error(OAuthError::Canceled));
        }
        self.admit()?;
        if flow.authorization_url.expose().is_empty() {
            return Err(ApiError::new(
                "cloud.oauth_failed",
                "sign-in initialization failed; start a new flow",
            ));
        }
        Ok(CloudOAuthBeginResult {
            status: project(&flow_id, &flow),
            authorization_url: CloudOAuthHumanText::new(flow.authorization_url.expose().to_owned()),
            user_code: flow
                .device
                .as_ref()
                .map(|d| CloudOAuthHumanText::new(d.user_code.expose().to_owned())),
            poll_interval_ms: flow.interval_ms,
        })
    }
    /// Finish one state-bound browser response, or one rate-limited device poll.
    pub async fn oauth_finish(
        &self,
        identity: &ClientIdentity,
        actor: Actor,
        p: CloudOAuthFinishParams,
    ) -> ApiResult<CloudOAuthStatus> {
        human(identity, &actor)?;
        validate_id(&p.flow_id, "cof")?;
        self.admit()?;
        let slot = self.oauth.slot(&p.flow_id, identity).await?;
        let mut guard = slot.flow.lock().await;
        let flow = &mut *guard;
        if slot.cancel.is_cancelled() {
            if flow.status == CloudOAuthState::Waiting {
                flow.status = CloudOAuthState::Canceled;
            }
            return Ok(project(&p.flow_id, flow));
        }
        if !flow.initialized {
            return Ok(project(&p.flow_id, flow));
        }
        if flow.status != CloudOAuthState::Waiting {
            return Ok(project(&p.flow_id, flow));
        }
        if Instant::now() >= flow.deadline {
            flow.status = CloudOAuthState::Expired;
            return Ok(project(&p.flow_id, flow));
        }
        let result = if let Some(device) = &flow.device {
            if p.code.is_some() || p.state.is_some() {
                return Err(invalid("device flow does not accept callback material"));
            }
            if Instant::now() < flow.next_poll {
                return Ok(project(&p.flow_id, flow));
            }
            flow.next_poll = Instant::now() + std::time::Duration::from_millis(flow.interval_ms);
            let secret = flow
                .params
                .client_secret
                .as_ref()
                .ok_or_else(|| invalid("registered app password missing"))?;
            self.oauth
                .http
                .poll_device(
                    &flow.params.registration,
                    &device.device_code,
                    zeus_cloud::SecretString::new(secret.expose().to_owned()),
                    &slot.cancel,
                )
                .await
        } else {
            let code = p
                .code
                .as_ref()
                .ok_or_else(|| invalid("callback code required"))?;
            if !p
                .state
                .as_ref()
                .is_some_and(|s| flow.proof.matches_state(s.expose()))
            {
                return Err(invalid("OAuth callback state mismatch"));
            }
            // Consume the flow before the one-use exchange: uncertain dispatch cannot replay a code.
            flow.status = CloudOAuthState::Failed;
            self.oauth
                .http
                .exchange(
                    CodeExchange {
                        registration: &flow.params.registration,
                        proof: &flow.proof,
                        redirect: flow.params.redirect_uri.as_deref().unwrap_or_default(),
                        code: &zeus_cloud::SecretString::new(code.expose().to_owned()),
                        client_secret: flow
                            .params
                            .client_secret
                            .as_ref()
                            .map(|s| zeus_cloud::SecretString::new(s.expose().to_owned())),
                        device_id: &flow.device_id,
                    },
                    &slot.cancel,
                )
                .await
        };
        let tokens = match result {
            Ok(t) => t,
            Err(OAuthError::Pending) => return Ok(project(&p.flow_id, flow)),
            Err(OAuthError::SlowDown) => {
                flow.interval_ms = flow.interval_ms.saturating_add(5000).min(300_000);
                flow.next_poll =
                    Instant::now() + std::time::Duration::from_millis(flow.interval_ms);
                return Ok(project(&p.flow_id, flow));
            }
            Err(e) => {
                flow.status = match e {
                    OAuthError::Expired => CloudOAuthState::Expired,
                    OAuthError::Canceled => CloudOAuthState::Canceled,
                    _ => CloudOAuthState::Failed,
                };
                return Err(oauth_error(e));
            }
        };
        if slot.cancel.is_cancelled() {
            flow.status = CloudOAuthState::Canceled;
            return Err(oauth_error(OAuthError::Canceled));
        }
        if Instant::now() >= flow.deadline {
            flow.status = CloudOAuthState::Expired;
            return Err(oauth_error(OAuthError::Expired));
        }
        let metadata = OAuthAccount {
            registration: flow.params.registration.clone(),
            device_id: flow.device_id.clone(),
            refresh_uncertain: false,
        };
        let stored = tokens.encode_private().map_err(oauth_error)?;
        let result = self
            .configure_oauth(
                identity,
                actor,
                CloudAccountsConfigureParams {
                    account_id: flow.params.account_id.clone(),
                    label: flow.params.label.clone(),
                    config: flow.params.config.clone(),
                    max_bytes_per_delivery: flow.params.max_bytes_per_delivery,
                    expected_revision: flow.params.expected_revision,
                    credential: Some(zeus_types::SecretString::new(stored.expose().to_owned())),
                    idempotency_key: format!("oauth-{}", p.flow_id),
                },
                Some(metadata),
            )
            .await?;
        let connected = self.oauth_account_status(&flow.params.account_id)?;
        flow.status = CloudOAuthState::Connected;
        flow.account = Some(result.account);
        flow.token_expires_at = connected.token_expires_at;
        flow.refresh_available = connected.refresh_available;
        flow.local_cleanup_pending = connected.local_cleanup_pending;
        Ok(project(&p.flow_id, flow))
    }
    /// Cancel a pending login; no existing account is changed.
    pub async fn oauth_cancel(
        &self,
        identity: &ClientIdentity,
        actor: Actor,
        p: CloudOAuthCancelParams,
    ) -> ApiResult<CloudOAuthStatus> {
        human(identity, &actor)?;
        let slot = self.oauth.slot(&p.flow_id, identity).await?;
        slot.cancel.cancel();
        let mut guard = slot.flow.lock().await;
        let f = &mut *guard;
        if f.status == CloudOAuthState::Waiting {
            f.status = CloudOAuthState::Canceled;
        }
        Ok(project(&p.flow_id, f))
    }
    /// Human-only lifecycle view; access/refresh tokens and authorization URLs are withheld.
    pub async fn oauth_status(
        &self,
        identity: &ClientIdentity,
        actor: Actor,
        p: CloudOAuthStatusParams,
    ) -> ApiResult<CloudOAuthStatus> {
        human(identity, &actor)?;
        match (p.flow_id, p.account_id) {
            (Some(id), None) => {
                let slot = self.oauth.slot(&id, identity).await?;
                let mut guard = slot.flow.lock().await;
                if slot.cancel.is_cancelled() && guard.status == CloudOAuthState::Waiting {
                    guard.status = CloudOAuthState::Canceled;
                }
                Ok(project(&id, &guard))
            }
            (None, Some(id)) => self.oauth_account_status(&id),
            _ => Err(invalid("select exactly one OAuth flow or account")),
        }
    }
    fn oauth_account_status(&self, id: &str) -> ApiResult<CloudOAuthStatus> {
        let a = self.account(id)?;
        let token = a
            .oauth
            .as_ref()
            .and(a.credential_key.as_ref())
            .map(|key| {
                self.credentials
                    .get(key)
                    .map_err(cloud_error)
                    .and_then(|s| OAuthTokens::decode_private(&s).map_err(oauth_error))
            })
            .transpose();
        let state = if a.view.revoked {
            CloudOAuthState::Revoked
        } else if a.oauth.is_none() {
            CloudOAuthState::Manual
        } else if a.oauth.as_ref().is_some_and(|m| m.refresh_uncertain) {
            CloudOAuthState::SignInRequired
        } else {
            match &token {
                Ok(Some(t)) if t.expires_at_ms > oauth::unix_ms() => CloudOAuthState::Connected,
                Ok(Some(t)) if t.refresh.is_some() => CloudOAuthState::RefreshRequired,
                _ => CloudOAuthState::SignInRequired,
            }
        };
        Ok(CloudOAuthStatus {
            flow_id: None,
            account_id: id.to_owned(),
            state,
            expires_at: None,
            token_expires_at: token.as_ref().ok().and_then(|v| {
                v.as_ref().and_then(|t| {
                    i64::try_from(t.expires_at_ms)
                        .ok()
                        .and_then(|ms| Timestamp::from_unix_millis(ms).ok())
                })
            }),
            refresh_available: !a.view.revoked
                && !a.oauth.as_ref().is_some_and(|m| m.refresh_uncertain)
                && token
                    .as_ref()
                    .ok()
                    .is_some_and(|v| v.as_ref().is_some_and(|t| t.refresh.is_some())),
            local_cleanup_pending: !a.pending_credentials.is_empty(),
            account: Some(a.view),
        })
    }
    /// Resolve only the authorized account, refreshing under a per-key lock before transfer.
    pub(super) async fn account_credential(
        &self,
        account: &Account,
        force: bool,
        cancel: &CancellationToken,
    ) -> ApiResult<Option<zeus_cloud::SecretString>> {
        let Some(key) = &account.credential_key else {
            return Ok(None);
        };
        let Some(metadata) = &account.oauth else {
            return self.credentials.get(key).map(Some).map_err(cloud_error);
        };
        let lock = self.oauth.lock(&account.view.account_id).await;
        let _lock = lock.lock().await;
        let current = self.account(&account.view.account_id)?;
        if current.view.revoked
            || current.view.revision != account.view.revision
            || current.credential_key.as_ref() != Some(key)
        {
            return Err(ApiError::new(
                "api.conflict",
                "OAuth account changed or revoked",
            ));
        }
        let raw = self.credentials.get(key).map_err(cloud_error)?;
        let tokens = OAuthTokens::decode_private(&raw).map_err(oauth_error)?;
        if current.oauth.as_ref().is_some_and(|m| m.refresh_uncertain) {
            return Err(oauth_error(OAuthError::SignInRequired));
        }
        if !force && !tokens.needs_refresh(oauth::unix_ms()) {
            return Ok(Some(tokens.access));
        }
        if cancel.is_cancelled() {
            return Err(oauth_error(OAuthError::Canceled));
        }
        let epoch = self.oauth.network_epoch.lock().await.clone();
        self.admit()?;
        // A refresh can rotate provider credentials. Persist dispatch uncertainty before HTTP;
        // interruption never authorizes replay of a potentially consumed refresh token.
        {
            let _gate = self.gate.lock().await;
            let mut current = self.account(&account.view.account_id)?;
            if current.view.revoked
                || current.view.revision != account.view.revision
                || current.credential_key.as_ref() != Some(key)
            {
                return Err(ApiError::new(
                    "api.conflict",
                    "OAuth account changed before refresh",
                ));
            }
            current.oauth.as_mut().ok_or_else(fail)?.refresh_uncertain = true;
            self.store
                .save(&format!("account:{}", account.view.account_id), &current)?;
        }
        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(oauth_error(OAuthError::Canceled)),
            value = self.oauth.http.refresh(&metadata.registration, &tokens, &epoch) => value.map_err(oauth_error)?,
        };
        let _gate = self.gate.lock().await;
        let current = self.account(&account.view.account_id)?;
        if current.view.revoked
            || current.view.revision != account.view.revision
            || current.credential_key.as_ref() != Some(key)
        {
            return Err(ApiError::new(
                "api.conflict",
                "OAuth account changed during refresh",
            ));
        }
        self.credentials
            .put(key, &next.encode_private().map_err(oauth_error)?)
            .map_err(cloud_error)?;
        let mut current = current;
        current.oauth.as_mut().ok_or_else(fail)?.refresh_uncertain = false;
        self.store
            .save(&format!("account:{}", account.view.account_id), &current)?;
        Ok(Some(next.access))
    }
    /// Explicit refresh requires a fresh verified human terminal.
    pub async fn oauth_refresh(
        &self,
        identity: &ClientIdentity,
        actor: Actor,
        p: CloudOAuthRefreshParams,
    ) -> ApiResult<CloudOAuthStatus> {
        human(identity, &actor)?;
        self.admit()?;
        let _existing = self.account(&p.account_id)?;
        let lock = self.oauth.lock(&format!("manual-{}", p.account_id)).await;
        let _intent = lock.lock().await;
        let (key, request_hash, old) =
            self.receipt(identity, "cloud.oauth.refresh", &p.idempotency_key, &p)?;
        if let Some(receipt) = old {
            if receipt.result["oauth_dispatch_pending"] == true {
                return Err(ApiError::new(
                    "cloud.oauth_outcome_uncertain",
                    "this refresh was already dispatched; inspect account status before a new reviewed intent",
                ));
            }
            return serde_json::from_value(receipt.result).map_err(|_| fail());
        }
        let a = self.account(&p.account_id)?;
        if a.view.revision != p.expected_revision {
            return Err(ApiError::new(
                "api.conflict",
                "OAuth account revision changed",
            ));
        }
        if a.oauth.is_none() {
            return Err(oauth_error(OAuthError::Unsupported));
        }
        // Persist the intent before dispatch as well as the account's refresh-rotation guard.
        // A crash after keyring rotation but before the result receipt must not refresh twice.
        self.store.save(
            &key,
            &Receipt {
                hash: request_hash.clone(),
                created_at: Some(Timestamp::now()),
                result: serde_json::json!({"oauth_dispatch_pending": true}),
            },
        )?;
        self.account_credential(&a, true, &CancellationToken::new())
            .await?;
        let result = self.oauth_account_status(&p.account_id)?;
        self.store.save(
            &key,
            &Receipt {
                hash: request_hash,
                created_at: Some(Timestamp::now()),
                result: serde_json::to_value(&result).map_err(|_| fail())?,
            },
        )?;
        Ok(result)
    }
    /// Disable local authority first; provider revocation success is reported separately.
    pub async fn oauth_disconnect(
        &self,
        identity: &ClientIdentity,
        actor: Actor,
        p: CloudOAuthDisconnectParams,
    ) -> ApiResult<CloudOAuthDisconnectResult> {
        human(identity, &actor)?;
        let _existing = self.account(&p.account_id)?;
        let lock = self
            .oauth
            .lock(&format!("disconnect-{}", p.account_id))
            .await;
        let _intent = lock.lock().await;
        let (receipt_key, request_hash, old) =
            self.receipt(identity, "cloud.oauth.disconnect", &p.idempotency_key, &p)?;
        if let Some(receipt) = old {
            return serde_json::from_value(receipt.result).map_err(|_| fail());
        }
        let a = self.account(&p.account_id)?;
        let token = a
            .oauth
            .as_ref()
            .and(a.credential_key.as_ref())
            .and_then(|k| self.credentials.get(k).ok())
            .and_then(|s| OAuthTokens::decode_private(&s).ok());
        let result = self
            .revoke(
                identity,
                actor,
                CloudAccountsRevokeParams {
                    account_id: p.account_id.clone(),
                    expected_revision: p.expected_revision,
                    idempotency_key: p.idempotency_key.clone(),
                },
            )
            .await?;
        let local_cleanup_pending = !self.account(&p.account_id)?.pending_credentials.is_empty();
        // Save the completed local disable before remote I/O. Retrying after a crash reports
        // the uncertain remote result without silently repeating provider revocation.
        let pending = CloudOAuthDisconnectResult {
            account: result.account.clone(),
            remote_revoked: p.remote.then_some(false),
            local_cleanup_pending,
            reason: p
                .remote
                .then(|| "cloud.oauth_remote_revocation_uncertain".into()),
        };
        self.store.save(
            &receipt_key,
            &Receipt {
                hash: request_hash.clone(),
                created_at: Some(Timestamp::now()),
                result: serde_json::to_value(&pending).map_err(|_| fail())?,
            },
        )?;
        let (remote_revoked, reason) = if !p.remote {
            (None, None)
        } else if let (Some(meta), Some(tokens)) = (a.oauth, token) {
            let epoch = self.oauth.network_epoch.lock().await.clone();
            let remote = match self.admit() {
                Ok(()) => {
                    self.oauth
                        .http
                        .revoke(&meta.registration, &tokens, &epoch)
                        .await
                }
                Err(_) => Err(OAuthError::Canceled),
            };
            match remote {
                Ok(()) => (Some(true), None),
                Err(OAuthError::Unsupported) => (
                    Some(false),
                    Some("cloud.oauth_remote_revocation_unavailable".into()),
                ),
                Err(_) => (
                    Some(false),
                    Some("cloud.oauth_remote_revocation_uncertain".into()),
                ),
            }
        } else {
            (
                Some(false),
                Some("cloud.oauth_remote_revocation_unavailable".into()),
            )
        };
        let result = CloudOAuthDisconnectResult {
            account: result.account,
            remote_revoked,
            local_cleanup_pending,
            reason,
        };
        self.store.save(
            &receipt_key,
            &Receipt {
                hash: request_hash,
                created_at: Some(Timestamp::now()),
                result: serde_json::to_value(&result).map_err(|_| fail())?,
            },
        )?;
        Ok(result)
    }
}

/// Reconcile only Zeus write-ahead key references; active account credentials are preserved.
pub(super) fn reconcile_credential_writes(
    store: &Store,
    credentials: &dyn CredentialStore,
) -> ApiResult<()> {
    let active: std::collections::BTreeSet<_> = store
        .list::<Account>("account:")?
        .into_iter()
        .filter_map(|a| a.credential_key)
        .collect();
    for marker in store.keys("credential-write:")? {
        let key: String = store.get(&marker)?.ok_or_else(fail)?;
        if active.contains(&key) || credentials.delete(&key).is_ok() {
            store.remove(&marker)?;
        }
    }
    Ok(())
}
