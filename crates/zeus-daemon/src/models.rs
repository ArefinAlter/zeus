//! Daemon-owned model configuration, secret storage and budgeted execution.

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};
use zeus_api::methods::admin::{
    LlmAccount, LlmAuthCancelParams, LlmAuthFlow, LlmAuthSignOutParams, LlmAuthSignOutResult,
    LlmAuthStartParams, LlmAuthStartResult, LlmAuthState, LlmAuthStatusParams, LlmAuthStatusResult,
    LlmConfigureParams, LlmConfigureResult, LlmStatusResult, RungStatus,
};
use zeus_api::{ApiError, ApiResult};
use zeus_llm::http_provider::HttpProvider;
use zeus_llm::provider::HttpProviderConfig;
use zeus_llm::runtime::{ModelRuntime, RequestAuthorization, RuntimeSettings, RuntimeStore};
use zeus_llm::secrets::SecretKey;
use zeus_llm::{
    LlmError, LlmProvider, LlmRequest, LlmResponse, PrivacyTier, ProviderKind, SecretStore,
    SpendCaps, TrustedKeys,
};

#[derive(Clone, Default, Serialize, Deserialize)]
struct Configuration {
    settings: RuntimeSettings,
    privacy: PrivacyTier,
    adapters: BTreeMap<ProviderKind, HttpProviderConfig>,
}

/// One model service per daemon. Configuring a provider never sends a network probe.
pub struct ModelService {
    configuration: Mutex<Configuration>,
    store: Arc<RuntimeStore>,
    secrets: Arc<dyn SecretStore>,
    trusted: TrustedKeys,
    accounts: Arc<zeus_llm::chatgpt::ChatgptAccounts>,
    flows: Mutex<BTreeMap<String, PendingFlow>>,
}
impl ModelService {
    /// Open durable state. Keyring access is lazy and never prevents a download-only daemon from starting.
    pub fn open(state_dir: &Path, secrets: Arc<dyn SecretStore>) -> ApiResult<Self> {
        let store = Arc::new(
            RuntimeStore::open(
                &state_dir.join("models.redb"),
                SpendCaps::default(),
                time_now(),
            )
            .map_err(error)?,
        );
        let configuration = match store.configuration().map_err(error)? {
            Some(value) => serde_json::from_value(value)
                .map_err(|_| ApiError::internal("invalid model configuration"))?,
            None => Configuration::default(),
        };
        let accounts = zeus_llm::chatgpt::ChatgptAccounts::open(
            &state_dir.join("chatgpt.redb"),
            secrets.clone(),
        )
        .map_err(error)?;
        Ok(Self {
            configuration: Mutex::new(configuration),
            store,
            secrets,
            trusted: TrustedKeys::pinned(),
            accounts,
            flows: Mutex::new(BTreeMap::new()),
        })
    }
    fn runtime(&self, configuration: &Configuration) -> ApiResult<ModelRuntime> {
        let mut providers: Vec<Arc<dyn LlmProvider>> = Vec::new();
        for config in configuration.adapters.values() {
            if config.kind == ProviderKind::ChatgptOauth {
                if let Some(id) = self.accounts.active().map_err(error)? {
                    let provider = zeus_llm::chatgpt::ChatgptProvider::new(
                        self.accounts.clone(),
                        id,
                        config.model.clone(),
                        config.pricing.ok_or_else(|| {
                            ApiError::invalid_params("ChatGPT pricing ceilings are required")
                        })?,
                        config.timeout_seconds,
                    )
                    .map_err(error)?;
                    providers.push(Arc::new(provider));
                }
            } else {
                providers.push(Arc::new(
                    HttpProvider::new(config.clone(), self.secrets.clone()).map_err(error)?,
                ));
            }
        }
        ModelRuntime::new(
            configuration.settings.clone(),
            self.store.clone(),
            self.trusted.clone(),
            providers,
        )
        .map_err(error)
    }
    fn snapshot(&self) -> ApiResult<Configuration> {
        self.configuration
            .lock()
            .map(|v| v.clone())
            .map_err(|_| ApiError::internal("model configuration unavailable"))
    }
    /// Status never returns model credentials and does not contact providers.
    pub fn status(&self) -> ApiResult<LlmStatusResult> {
        let configuration = self.snapshot()?;
        let runtime = self.runtime(&configuration)?;
        let status = runtime.status().map_err(error)?;
        let spend = self.store.spend_at(time_now()).map_err(error)?;
        Ok(LlmStatusResult {
            rungs: status
                .providers
                .into_iter()
                .map(|r| RungStatus {
                    provider: r.provider,
                    rung: r.provider.rung(),
                    status: r.status,
                    configured: r.configured,
                    reason: r.reason,
                })
                .collect(),
            privacy: configuration.privacy,
            spend_caps: spend.caps(),
            spent_day_micro_usd: spend.day_spent(),
            spent_month_micro_usd: spend.month_spent(),
            policy_sequence: status.policy_sequence,
        })
    }
    /// Apply a change after the backend verifies the trusted human terminal. Secret writes are never logged.
    pub fn configure(&self, params: LlmConfigureParams) -> ApiResult<LlmConfigureResult> {
        let mut guard = self
            .configuration
            .lock()
            .map_err(|_| ApiError::internal("model configuration unavailable"))?;
        let mut configuration = guard.clone();
        if let Some(enabled) = params.enabled {
            configuration.settings.enabled = enabled
        }
        if let Some(privacy) = params.privacy {
            configuration.privacy = privacy
        }
        if let Some(experimental) = params.allow_experimental {
            configuration.settings.allow_experimental = experimental
        }
        let pricing_reviewed = params
            .adapter
            .as_ref()
            .is_some_and(|adapter| adapter.pricing.is_some());
        let provider = params
            .provider
            .or_else(|| params.adapter.as_ref().map(|a| a.kind));
        if let Some(adapter) = params.adapter {
            if params.provider.is_some_and(|p| p != adapter.kind) {
                return Err(ApiError::invalid_params(
                    "provider and adapter kind must match",
                ));
            }
            // Construction validates endpoint/model/dialect/pricing without reading a credential or making a call.
            let mut validation = adapter.clone();
            if validation.kind == ProviderKind::ChatgptOauth {
                if validation.endpoint.as_str() != "https://api.openai.com/v1/responses"
                    || !matches!(
                        validation.dialect,
                        zeus_llm::provider::HttpDialect::OpenAiResponses
                    )
                {
                    return Err(ApiError::invalid_params(
                        "ChatGPT uses the public Responses endpoint",
                    ));
                }
                validation.kind = ProviderKind::ApiKeyOpenaiCompatible;
            }
            HttpProvider::new(validation, self.secrets.clone()).map_err(error)?;
            configuration.adapters.insert(adapter.kind, adapter);
        }
        if let Some(key) = params.api_key {
            let provider = provider
                .ok_or_else(|| ApiError::invalid_params("provider is required for an API key"))?;
            if !matches!(
                provider,
                ProviderKind::ApiKeyOpenaiCompatible
                    | ProviderKind::ApiKeyAnthropic
                    | ProviderKind::OpenrouterOauth
            ) {
                return Err(ApiError::invalid_params(
                    "this provider does not accept API keys",
                ));
            }
            let address = SecretKey {
                provider,
                purpose: "api_key".into(),
            };
            if key.expose().is_empty() {
                self.secrets.delete(&address).map_err(error)?
            } else {
                self.secrets.set(&address, key.expose()).map_err(error)?
            }
        }
        self.store
            .configure_reviewed(
                serde_json::to_value(&configuration)
                    .map_err(|_| ApiError::internal("cannot encode model configuration"))?,
                params.spend_caps,
                pricing_reviewed,
            )
            .map_err(error)?;
        *guard = configuration;
        drop(guard);
        Ok(LlmConfigureResult {
            replayed: false,
            rungs: self.status()?.rungs,
        })
    }
    /// Execute an explicitly requested model task under current consent, privacy and durable caps.
    pub async fn complete(
        &self,
        mut request: LlmRequest,
        authorization: RequestAuthorization,
    ) -> ApiResult<LlmResponse> {
        let configuration = self.snapshot()?;
        if configuration.privacy == PrivacyTier::LocalOnly {
            request.privacy = PrivacyTier::LocalOnly
        }
        self.runtime(&configuration)?
            .complete(request, authorization)
            .await
            .map_err(error)
    }
}
fn time_now() -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc()
}
fn error(error: LlmError) -> ApiError {
    match error {
        LlmError::NotConfigured => ApiError::new(
            "llm.not_configured",
            "model access or provider is not configured",
        ),
        LlmError::PrivacyViolation { .. } => ApiError::new(
            "llm.privacy_violation",
            "model context requires explicit task consent",
        ),
        LlmError::BudgetExceeded { .. } => {
            ApiError::new("llm.budget_exceeded", "model spend limit would be exceeded")
        }
        LlmError::DisabledByPolicy(_) | LlmError::Policy(_) => ApiError::new(
            "llm.disabled_by_policy",
            "model provider is unavailable under current policy",
        ),
        LlmError::AuthFailed => {
            ApiError::new("llm.auth_failed", "model provider rejected its credential")
        }
        LlmError::SchemaViolation => ApiError::new(
            "llm.schema_violation",
            "model configuration or response failed validation",
        ),
        LlmError::SecretStore(_) => {
            ApiError::new("llm.secret_store", "OS credential store is unavailable")
        }
        _ => ApiError::new(
            "llm.provider_unavailable",
            "model operation could not be completed",
        ),
    }
}

struct PendingFlow {
    owner: String,
    state: Arc<Mutex<LlmAuthFlow>>,
    task: tokio::task::AbortHandle,
}
impl Drop for PendingFlow {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl ModelService {
    /// Begin an owned browser sign-in after the backend verifies the human channel.
    pub async fn auth_start(
        &self,
        owner: &str,
        params: LlmAuthStartParams,
    ) -> ApiResult<LlmAuthStartResult> {
        let expires_at = zeus_types::Timestamp::now().saturating_add_ms(300_000);
        let flow_id = zeus_api::auth::Nonce::generate().as_str().to_owned();
        let state = Arc::new(Mutex::new(LlmAuthFlow {
            flow_id: flow_id.clone(),
            provider: params.provider,
            state: LlmAuthState::Waiting,
            account_id: None,
            expires_at,
        }));
        let (url, task) = match params.provider {
            ProviderKind::ChatgptOauth => {
                let login = self
                    .accounts
                    .begin(params.account_id.as_deref())
                    .await
                    .map_err(error)?;
                let url = login.authorization_url().to_string();
                let result = state.clone();
                let task = tokio::spawn(async move {
                    let outcome = login.finish().await;
                    if let Ok(mut state) = result.lock() {
                        match outcome {
                            Ok(account) => {
                                state.state = LlmAuthState::Connected;
                                state.account_id = Some(account.client_id)
                            }
                            Err(_) => {
                                state.state = if zeus_types::Timestamp::now() >= state.expires_at {
                                    LlmAuthState::Expired
                                } else {
                                    LlmAuthState::Failed
                                }
                            }
                        }
                    }
                });
                (url, task.abort_handle())
            }
            ProviderKind::OpenrouterOauth => {
                if params.account_id.is_some() {
                    return Err(ApiError::invalid_params(
                        "OpenRouter does not use a ChatGPT account id",
                    ));
                }
                let login = zeus_llm::pkce::OpenRouterLogin::bind()
                    .await
                    .map_err(error)?;
                let url = login.authorization_url().to_string();
                let result = state.clone();
                let secrets = self.secrets.clone();
                let task = tokio::spawn(async move {
                    let outcome = login.finish(secrets.as_ref()).await;
                    if let Ok(mut state) = result.lock() {
                        state.state = if outcome.is_ok() {
                            LlmAuthState::Connected
                        } else if zeus_types::Timestamp::now() >= state.expires_at {
                            LlmAuthState::Expired
                        } else {
                            LlmAuthState::Failed
                        }
                    }
                });
                (url, task.abort_handle())
            }
            _ => {
                return Err(ApiError::invalid_params(
                    "provider does not support browser sign-in",
                ));
            }
        };
        let mut flows = self
            .flows
            .lock()
            .map_err(|_| ApiError::internal("sign-in state unavailable"))?;
        flows.retain(|_, flow| {
            flow.state
                .lock()
                .is_ok_and(|s| s.expires_at > zeus_types::Timestamp::now())
        });
        if flows.len() >= 8 {
            task.abort();
            return Err(ApiError::new(
                "llm.too_many_auth_flows",
                "too many pending sign-ins",
            ));
        }
        flows.insert(
            flow_id.clone(),
            PendingFlow {
                owner: owner.into(),
                state,
                task,
            },
        );
        Ok(LlmAuthStartResult {
            flow_id,
            authorization_url: url,
            expires_at,
        })
    }
    /// Inspect only this credential's flow; account identity is returned only on the trusted human surface.
    pub fn auth_status(
        &self,
        owner: &str,
        params: LlmAuthStatusParams,
    ) -> ApiResult<LlmAuthStatusResult> {
        let flow = match params.flow_id {
            Some(id) => {
                let flows = self
                    .flows
                    .lock()
                    .map_err(|_| ApiError::internal("sign-in state unavailable"))?;
                let flow = flows
                    .get(&id)
                    .filter(|f| f.owner == owner)
                    .ok_or_else(|| ApiError::new("llm.auth_not_found", "sign-in flow not found"))?;
                let mut state = flow
                    .state
                    .lock()
                    .map_err(|_| ApiError::internal("sign-in state unavailable"))?;
                if state.state == LlmAuthState::Waiting
                    && zeus_types::Timestamp::now() >= state.expires_at
                {
                    flow.task.abort();
                    state.state = LlmAuthState::Expired
                }
                Some(state.clone())
            }
            None => None,
        };
        let active = self.accounts.active().map_err(error)?;
        let accounts = self
            .accounts
            .accounts()
            .map_err(error)?
            .into_iter()
            .map(|a| LlmAccount {
                active: active.as_deref() == Some(a.client_id.as_str()),
                account_id: a.client_id,
                email: a.email,
                plan_enabled: a.plan_enabled,
            })
            .collect();
        Ok(LlmAuthStatusResult { flow, accounts })
    }
    /// Cancel only the requesting credential's unfinished listener.
    pub fn auth_cancel(&self, owner: &str, params: LlmAuthCancelParams) -> ApiResult<LlmAuthFlow> {
        let flows = self
            .flows
            .lock()
            .map_err(|_| ApiError::internal("sign-in state unavailable"))?;
        let flow = flows
            .get(&params.flow_id)
            .filter(|f| f.owner == owner)
            .ok_or_else(|| ApiError::new("llm.auth_not_found", "sign-in flow not found"))?;
        let mut state = flow
            .state
            .lock()
            .map_err(|_| ApiError::internal("sign-in state unavailable"))?;
        if state.state == LlmAuthState::Waiting {
            state.state = LlmAuthState::Canceled;
            flow.task.abort()
        }
        Ok(state.clone())
    }
    /// Clear a provider's local credentials; report remote revocation honestly.
    pub async fn auth_sign_out(
        &self,
        params: LlmAuthSignOutParams,
    ) -> ApiResult<LlmAuthSignOutResult> {
        // Abort and drain this provider's listeners before clearing credentials: an exchange
        // already running on another executor thread must not save a token after sign-out.
        let tasks = {
            let flows = self
                .flows
                .lock()
                .map_err(|_| ApiError::internal("sign-in state unavailable"))?;
            let mut tasks = Vec::new();
            for flow in flows.values() {
                let mut state = flow
                    .state
                    .lock()
                    .map_err(|_| ApiError::internal("sign-in state unavailable"))?;
                if state.provider == params.provider && state.state == LlmAuthState::Waiting {
                    state.state = LlmAuthState::Canceled;
                    flow.task.abort();
                    tasks.push(flow.task.clone());
                }
            }
            tasks
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while tasks.iter().any(|task| !task.is_finished()) {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .map_err(|_| ApiError::new("llm.auth_busy", "sign-in cancellation is still finishing"))?;
        let confirmed = match params.provider {
            ProviderKind::ChatgptOauth => {
                let id = params
                    .account_id
                    .or(self.accounts.active().map_err(error)?)
                    .ok_or_else(|| ApiError::invalid_params("no selected ChatGPT account"))?;
                self.accounts.sign_out(&id).await.map_err(error)?
            }
            ProviderKind::OpenrouterOauth => {
                self.secrets
                    .delete(&SecretKey {
                        provider: ProviderKind::OpenrouterOauth,
                        purpose: "api_key".into(),
                    })
                    .map_err(error)?;
                false
            }
            _ => {
                return Err(ApiError::invalid_params(
                    "provider does not support browser sign-in",
                ));
            }
        };
        Ok(LlmAuthSignOutResult {
            cleared: true,
            remote_revocation_confirmed: confirmed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeus_llm::provider::HttpDialect;
    #[tokio::test]
    async fn browser_flows_are_owner_bound_and_sign_out_drains_pending_exchanges() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(zeus_llm::MemorySecretStore::default());
        let service = ModelService::open(dir.path(), secrets.clone()).unwrap();
        let params = LlmAuthStartParams {
            provider: ProviderKind::OpenrouterOauth,
            account_id: None,
        };
        let started = service.auth_start("owner", params.clone()).await.unwrap();
        assert!(!format!("{started:?}").contains("state="));
        assert_eq!(
            service
                .auth_status(
                    "other",
                    LlmAuthStatusParams {
                        flow_id: Some(started.flow_id.clone())
                    }
                )
                .unwrap_err()
                .envelope()
                .code
                .as_str(),
            "llm.auth_not_found"
        );
        assert!(
            service
                .auth_cancel(
                    "other",
                    LlmAuthCancelParams {
                        flow_id: started.flow_id.clone()
                    }
                )
                .is_err()
        );
        assert_eq!(
            service
                .auth_cancel(
                    "owner",
                    LlmAuthCancelParams {
                        flow_id: started.flow_id
                    }
                )
                .unwrap()
                .state,
            LlmAuthState::Canceled
        );
        let pending = service.auth_start("owner", params).await.unwrap();
        let key = SecretKey {
            provider: ProviderKind::OpenrouterOauth,
            purpose: "api_key".into(),
        };
        secrets.set(&key, "test-only-unused").unwrap();
        let result = service
            .auth_sign_out(LlmAuthSignOutParams {
                provider: ProviderKind::OpenrouterOauth,
                account_id: None,
            })
            .await
            .unwrap();
        assert!(result.cleared);
        assert!(!result.remote_revocation_confirmed);
        assert!(secrets.get(&key).unwrap().is_none());
        assert_eq!(
            service
                .auth_status(
                    "owner",
                    LlmAuthStatusParams {
                        flow_id: Some(pending.flow_id)
                    }
                )
                .unwrap()
                .flow
                .unwrap()
                .state,
            LlmAuthState::Canceled
        );
    }
    #[test]
    fn configuration_survives_restart_and_default_is_no_model_access() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(zeus_llm::MemorySecretStore::default());
        let service = ModelService::open(dir.path(), secrets.clone()).unwrap();
        assert!(
            service
                .status()
                .unwrap()
                .rungs
                .iter()
                .all(|r| !r.configured)
        );
        let adapter = HttpProviderConfig {
            kind: ProviderKind::LocalOllama,
            endpoint: "http://127.0.0.1:1/api/chat".parse().unwrap(),
            dialect: HttpDialect::Ollama,
            model: "test-local".into(),
            pricing: None,
            timeout_seconds: 1,
        };
        let result = service
            .configure(LlmConfigureParams {
                enabled: Some(true),
                adapter: Some(adapter),
                privacy: Some(PrivacyTier::LocalOnly),
                spend_caps: Some(SpendCaps {
                    daily_micro_usd: 0,
                    monthly_micro_usd: 0,
                }),
                ..Default::default()
            })
            .unwrap();
        assert!(
            result
                .rungs
                .iter()
                .any(|r| r.provider == ProviderKind::LocalOllama && r.configured)
        );
        drop(service);
        let reopened = ModelService::open(dir.path(), secrets).unwrap();
        let status = reopened.status().unwrap();
        assert_eq!(status.privacy, PrivacyTier::LocalOnly);
        assert_eq!(status.spend_caps.daily_micro_usd, 0);
        assert!(
            status
                .rungs
                .iter()
                .any(|r| r.provider == ProviderKind::LocalOllama && r.configured)
        );
    }
    #[test]
    fn key_is_stored_only_in_secret_store_and_never_status_or_database_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(zeus_llm::MemorySecretStore::default());
        let service = ModelService::open(dir.path(), secrets.clone()).unwrap();
        service
            .configure(LlmConfigureParams {
                provider: Some(ProviderKind::ApiKeyAnthropic),
                api_key: Some("ZEUS-CANARY-secret".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(
            secrets
                .get(&SecretKey {
                    provider: ProviderKind::ApiKeyAnthropic,
                    purpose: "api_key".into()
                })
                .unwrap()
                .is_some()
        );
        assert!(
            !serde_json::to_string(&service.status().unwrap())
                .unwrap()
                .contains("ZEUS-CANARY")
        );
        assert!(
            !service
                .store
                .configuration()
                .unwrap()
                .unwrap()
                .to_string()
                .contains("ZEUS-CANARY")
        );
        service
            .configure(LlmConfigureParams {
                provider: Some(ProviderKind::ApiKeyAnthropic),
                api_key: Some("".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(
            secrets
                .get(&SecretKey {
                    provider: ProviderKind::ApiKeyAnthropic,
                    purpose: "api_key".into()
                })
                .unwrap()
                .is_none()
        );
    }
}
