//! Persistent daemon backend joining the local API to the transfer engine.

use crate::{Backend, config::DaemonConfig};
use async_trait::async_trait;
use futures::StreamExt;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use zeus_api::methods::{
    admin::*, cloud::*, daemon::*, events::*, files::*, jobs::*, sessions::*, workflows::*,
};
use zeus_api::{ApiError, ApiResult, CallContext, Page};
use zeus_engine::{Engine, EngineBuilder, EngineConfig};
use zeus_journal::{Journal, JournalOptions};
use zeus_types::{
    ApprovalState, EventSeq, GrantState, JobSpec, JobState, Surface, Timestamp, ToEnvelope,
};

fn error(value: impl ToEnvelope) -> ApiError {
    value.to_envelope().into()
}
fn internal() -> ApiError {
    ApiError::internal("daemon state operation failed")
}
fn page<T>(mut items: Vec<T>, request: &zeus_api::PageRequest) -> ApiResult<Page<T>> {
    request.validate()?;
    let offset = request
        .cursor
        .as_deref()
        .unwrap_or("0")
        .parse::<usize>()
        .map_err(|_| ApiError::invalid_params("invalid cursor"))?;
    let limit = request.effective_limit();
    if offset > items.len() {
        return Err(ApiError::invalid_params("cursor outside result"));
    }
    let next = (offset.saturating_add(limit) < items.len()).then(|| (offset + limit).to_string());
    Ok(Page {
        items: items.drain(offset..).take(limit).collect(),
        next_cursor: next,
    })
}

/// One daemon owns this backend and its journal for its whole lifetime.
pub struct RuntimeBackend {
    /// Engine handle shared by the API and browser broker.
    pub engine: Engine,
    /// Durable metadata and grants.
    pub journal: Journal,
    config: DaemonConfig,
    config_path: std::sync::OnceLock<PathBuf>,
    trust: crate::trust::TrustedPrograms,
    consent: Arc<crate::consent::ConsentManager>,
    audit: Arc<zeus_verify::audit::AuditLog>,
    pub(crate) browser: Arc<crate::browser::BrowserBroker>,
    info: DaemonInfo,
    started_at: Timestamp,
    started: Instant,
    /// Graceful shutdown signal.
    pub shutdown: zeus_api::server::ShutdownController,
    mutations: Mutex<()>,
    receipts: crate::mutations::MutationStore,
    models: crate::models::ModelService,
    outbox: Arc<std::sync::Mutex<crate::outbox::Outbox>>,
    pub(crate) workflows: Arc<crate::workflows::WorkflowService>,
    pub(crate) cloud: Arc<crate::cloud::CloudService>,
    subscriptions: Mutex<HashMap<String, (zeus_types::ConnectionId, tokio::task::AbortHandle)>>,
    deferred_audit: crate::deferred_audit::DeferredAudit,
    state_dir: PathBuf,
    credentials: Arc<crate::provision::FileCredentialStore>,
    /// Authenticated connections and their credential, for `clients.revoke`.
    connections: std::sync::Mutex<HashMap<zeus_types::ConnectionId, zeus_api::auth::CredentialId>>,
    admin_limits: std::sync::Mutex<HashMap<String, crate::approvals::DecisionLimiter>>,
    #[cfg(test)]
    fail_kill_switch_audit: std::sync::atomic::AtomicBool,
}

impl RuntimeBackend {
    /// Open persistent state and recover unfinished transfers before accepting clients.
    pub async fn open(state_dir: PathBuf, config: DaemonConfig) -> ApiResult<Arc<Self>> {
        config
            .validate()
            .map_err(|_| ApiError::invalid_params("invalid daemon configuration"))?;
        if config.scheduler.segment_size_mib
            != crate::config::SchedulerConfig::default().segment_size_mib
        {
            tracing::warn!(
                "scheduler.segment_size_mib is not used by the engine yet; ranged requests use the commit unit"
            );
        }
        let roots = config.effective_write_roots();
        let default = roots
            .first()
            .cloned()
            .ok_or_else(|| ApiError::invalid_params("no write root configured"))?;
        for root in &roots {
            std::fs::create_dir_all(root).map_err(|_| internal())?;
        }
        let clock: zeus_control::SharedClock = Arc::new(zeus_control::SystemClock::new());
        let journal = Journal::open(
            &state_dir.join("journal.redb"),
            &JournalOptions::default(),
            clock.clone(),
        )
        .map_err(error)?;
        let mut engine_config = EngineConfig::new(roots, default, state_dir.clone());
        engine_config.limits.concurrent_jobs = config.scheduler.concurrent_jobs;
        engine_config.limits.connections_per_job = config.scheduler.connections_per_job;
        engine_config.limits.connections_per_host = config.scheduler.connections_per_host;
        engine_config.timeouts.connect = Duration::from_secs(config.timeouts.connect_s.into());
        engine_config.timeouts.read = Duration::from_secs(config.timeouts.read_s.into());
        engine_config.timeouts.probe = Duration::from_secs(config.timeouts.probe_s.into());
        engine_config.timeouts.session_wait =
            Duration::from_millis(config.timeouts.session_wait_ms);
        engine_config.timeouts.approval_expiry = Duration::from_millis(config.approvals.expiry_ms);
        if let Some(days) = config.tombstone_days {
            engine_config.timeouts.tombstone_retention =
                Duration::from_secs(u64::from(days) * 86400);
        }
        let audit = zeus_verify::audit::AuditLog::open(
            &state_dir.join("audit"),
            None,
            journal.meta().map_err(error)?.store_id,
            env!("CARGO_PKG_VERSION"),
            Default::default(),
        )
        .map_err(error)?;
        let audit = Arc::new(audit);
        let deferred_audit =
            crate::deferred_audit::DeferredAudit::open(&state_dir).map_err(|_| internal())?;
        if deferred_audit.is_pending() {
            let recorded = crate::deferred_audit::DeferredAudit::recorded_ids(&audit);
            deferred_audit.replay(audit.as_ref(), &recorded);
        }
        let credentials = Arc::new(
            crate::provision::FileCredentialStore::load(&state_dir.join("credentials.json"))
                .map_err(|_| internal())?,
        );
        let consent = Arc::new(
            crate::consent::ConsentManager::new(
                journal.clone(),
                audit.clone(),
                credentials.clone(),
            )
            .with_limits(crate::approvals::ConsentLimits::new(
                config.approvals.max_pending,
                config.approvals.decisions_per_minute,
            )),
        );
        let browser = Arc::new(crate::browser::BrowserBroker::new(
            journal.clone(),
            clock.clone(),
        ));
        let artifacts = Arc::new(
            zeus_artifact::ArtifactStore::open(
                &state_dir.join("artifacts"),
                zeus_artifact::StoreConfig {
                    write_roots: config.effective_write_roots(),
                    ..Default::default()
                },
                Arc::new(zeus_fs::RealFs),
                clock.clone(),
                Some(audit.clone()),
            )
            .map_err(error)?,
        );
        let workflows = crate::workflows::WorkflowService::open(
            &state_dir.join("workflows.redb"),
            journal.clone(),
            consent.clone(),
            audit.clone(),
            config.effective_write_roots(),
        )?;
        workflows.recover_audit_receipts(audit.as_ref())?;
        let roots_of = credentials.clone();
        workflows.attach_service_roots(Arc::new(move |id| {
            zeus_api::auth::CredentialId::parse(id)
                .map(|id| roots_of.roots(&id))
                .unwrap_or_default()
        }));
        let engine = EngineBuilder::new(engine_config)
            .clock(clock)
            .session_provider(browser.clone())
            .proxy_fetch(browser.clone())
            .journal(journal.clone())
            .audit(audit.clone())
            .approvals(consent.clone())
            .job_transports(workflows.transport_factory())
            .build()
            .await
            .map_err(error)?;
        workflows.attach_engine(engine.clone())?;
        let trust = crate::trust::TrustedPrograms::load_or_initialize(&state_dir)?;
        let receipts = crate::mutations::MutationStore::open(&state_dir.join("api-receipts.redb"))?;
        let models = crate::models::ModelService::open(
            &state_dir,
            Arc::new(zeus_llm::secrets::OsKeyringStore {
                service: "dev.zeus".into(),
            }),
        )?;
        let cloud = crate::cloud::CloudService::open(
            &state_dir,
            journal.clone(),
            consent.clone(),
            audit.clone(),
            Arc::new(zeus_cloud::OsCredentialStore {
                service: "dev.zeus".into(),
            }),
        )?;
        let outbox = Arc::new(std::sync::Mutex::new(crate::outbox::Outbox::open(
            &state_dir,
            &config.effective_write_roots(),
            journal.clone(),
            audit.clone(),
            artifacts,
        )?));
        Ok(Arc::new(Self {
            engine,
            journal,
            config,
            config_path: Default::default(),
            trust,
            consent,
            audit,
            browser,
            receipts,
            models,
            outbox,
            workflows,
            cloud,
            info: DaemonInfo {
                version: env!("CARGO_PKG_VERSION").into(),
                instance_id: zeus_api::auth::Nonce::generate().as_str()[..32].to_owned(),
            },
            started_at: Timestamp::now(),
            started: Instant::now(),
            shutdown: zeus_api::server::ShutdownController::default(),
            mutations: Mutex::new(()),
            subscriptions: Mutex::new(HashMap::new()),
            deferred_audit,
            state_dir,
            credentials,
            connections: Default::default(),
            admin_limits: Default::default(),
            #[cfg(test)]
            fail_kill_switch_audit: Default::default(),
        }))
    }

    /// Identity reported during the mutual handshake.
    pub fn info(&self) -> DaemonInfo {
        self.info.clone()
    }

    /// The credential store shared with the IPC handshake, so credential changes reach live connections.
    pub fn credentials(&self) -> Arc<crate::provision::FileCredentialStore> {
        self.credentials.clone()
    }
    /// Bind the configuration file selected by startup flags/environment.
    pub fn set_config_path(&self, path: PathBuf) -> ApiResult<()> {
        self.config_path.set(path).map_err(|_| internal())
    }

    /// Flush one bounded batch of durable provenance side effects off the async executor.
    pub async fn flush_outbox(&self) -> ApiResult<()> {
        let outbox = self.outbox.clone();
        tokio::task::spawn_blocking(move || outbox.lock().map_err(|_| internal())?.tick())
            .await
            .map_err(|_| internal())?
    }

    fn caller(ctx: &CallContext) -> zeus_engine::api::Caller {
        zeus_engine::api::Caller {
            client_id: ctx.identity.credential_id.to_string(),
            kind: ctx.identity.action_actor().kind,
        }
    }

    fn admit(&self) -> ApiResult<()> {
        if self.journal.meta().map_err(error)?.kill_switch_engaged() {
            Err(ApiError::new(
                "policy.kill_switch_engaged",
                "kill switch engaged; a human must release it",
            ))
        } else {
            Ok(())
        }
    }

    /// Downgrade an unverified human credential before the connection receives any authority.
    pub fn classify_client(&self, ctx: &mut CallContext) -> ApiResult<()> {
        use zeus_verify::audit::{AuditEntry, AuditOutcome, AuditSink};
        let credential_kind = ctx.identity.kind;
        let verified = self.trust.verified(ctx);
        if matches!(
            credential_kind,
            zeus_api::ClientKind::Human | zeus_api::ClientKind::Admin
        ) && !verified
        {
            ctx.identity.kind = zeus_api::ClientKind::Agent;
            let ceiling = zeus_api::ClientKind::Agent.default_permissions();
            ctx.identity.permissions.retain(|p| ceiling.contains(p));
        }
        let mut entry = AuditEntry::new(
            Timestamp::now(),
            ctx.identity.action_actor(),
            "client.connected",
            AuditOutcome::Ok,
        );
        entry
            .data
            .insert("credential_kind".into(), serde_json::json!(credential_kind));
        entry.data.insert(
            "effective_kind".into(),
            serde_json::json!(ctx.identity.kind),
        );
        entry.data.insert(
            "downgraded".into(),
            serde_json::json!(credential_kind != ctx.identity.kind),
        );
        entry
            .data
            .insert("exe_verified".into(), serde_json::json!(verified));
        entry
            .data
            .insert("peer_pid".into(), serde_json::json!(ctx.peer.pid));
        entry
            .data
            .insert("role".into(), serde_json::json!(ctx.identity.role));
        self.audit.append(entry).map_err(error)?;
        self.credentials
            .touch(&ctx.identity.credential_id, Timestamp::now());
        self.connections
            .lock()
            .map_err(|_| internal())?
            .insert(ctx.connection_id, ctx.identity.credential_id.clone());
        Ok(())
    }

    /// Remove connection-owned work on disconnect without canceling independent downloads.
    pub async fn disconnect(&self, ctx: &CallContext) {
        if let Ok(mut connections) = self.connections.lock() {
            connections.remove(&ctx.connection_id);
        }
        self.browser.disconnect(ctx.connection_id);
        self.subscriptions.lock().await.retain(|_, (owner, task)| {
            if owner == &ctx.connection_id {
                task.abort();
                false
            } else {
                !task.is_finished()
            }
        });
    }
}

#[async_trait]
impl Backend for RuntimeBackend {
    async fn cloud_accounts_list(
        &self,
        _ctx: &CallContext,
        _p: CloudAccountsListParams,
    ) -> ApiResult<CloudAccountsListResult> {
        self.cloud.accounts()
    }
    async fn cloud_accounts_configure(
        &self,
        ctx: &CallContext,
        p: CloudAccountsConfigureParams,
    ) -> ApiResult<CloudAccountMutationResult> {
        let actor = self.model_human(ctx)?;
        self.cloud.configure(&ctx.identity, actor, p).await
    }
    async fn cloud_accounts_revoke(
        &self,
        ctx: &CallContext,
        p: CloudAccountsRevokeParams,
    ) -> ApiResult<CloudAccountMutationResult> {
        let actor = self.model_human(ctx)?;
        self.cloud.revoke(&ctx.identity, actor, p).await
    }
    async fn cloud_plan(
        &self,
        ctx: &CallContext,
        p: CloudPlanParams,
    ) -> ApiResult<CloudPlanResult> {
        self.cloud.plan(&ctx.identity, p).await
    }
    async fn cloud_submit(
        &self,
        ctx: &CallContext,
        p: CloudSubmitParams,
    ) -> ApiResult<CloudMutationResult> {
        self.cloud.submit(&ctx.identity, p).await
    }
    async fn cloud_get(
        &self,
        ctx: &CallContext,
        p: CloudGetParams,
    ) -> ApiResult<CloudDeliveryView> {
        self.cloud.get(&ctx.identity, p)
    }
    async fn cloud_list(
        &self,
        ctx: &CallContext,
        p: CloudListParams,
    ) -> ApiResult<CloudListResult> {
        self.cloud.list(&ctx.identity, p)
    }
    async fn cloud_control(
        &self,
        ctx: &CallContext,
        p: CloudControlParams,
    ) -> ApiResult<CloudMutationResult> {
        self.cloud.control(&ctx.identity, p).await
    }
    async fn workflows_plan(
        &self,
        ctx: &CallContext,
        p: WorkflowsPlanParams,
    ) -> ApiResult<WorkflowsPlanResult> {
        self.admit()?;
        self.workflows.plan(&ctx.identity, p).await
    }
    async fn workflows_dry_run(
        &self,
        ctx: &CallContext,
        p: WorkflowsDryRunParams,
    ) -> ApiResult<WorkflowsDryRunResult> {
        self.admit()?;
        self.workflows.dry_run(&ctx.identity, p).await
    }
    async fn workflows_run(
        &self,
        ctx: &CallContext,
        p: WorkflowsRunParams,
    ) -> ApiResult<WorkflowsRunResult> {
        self.admit()?;
        self.workflows.run(&ctx.identity, p).await
    }
    async fn workflows_get(
        &self,
        ctx: &CallContext,
        p: WorkflowsGetParams,
    ) -> ApiResult<WorkflowsGetResult> {
        self.workflows.get(&ctx.identity, p)
    }
    async fn workflows_list(
        &self,
        ctx: &CallContext,
        p: WorkflowsListParams,
    ) -> ApiResult<WorkflowsListResult> {
        self.workflows.list(&ctx.identity, p)
    }
    async fn workflows_control(
        &self,
        ctx: &CallContext,
        p: WorkflowsControlParams,
    ) -> ApiResult<WorkflowsControlResult> {
        self.workflows.control(&ctx.identity, p).await
    }
    async fn workflows_manifest(
        &self,
        ctx: &CallContext,
        p: WorkflowsManifestParams,
    ) -> ApiResult<WorkflowsManifestResult> {
        self.workflows.manifest(&ctx.identity, p)
    }
    async fn daemon_status(&self, _ctx: &CallContext, _p: StatusParams) -> ApiResult<StatusResult> {
        let status = self.engine.status().await.map_err(error)?;
        Ok(StatusResult {
            daemon: self.info(),
            protocol_version: 1,
            started_at: self.started_at,
            uptime_ms: self.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            head_seq: status.head_seq,
            jobs: status
                .jobs_by_state
                .into_iter()
                .map(|(s, n)| (s, n.min(u32::MAX as u64) as u32))
                .collect(),
            pending_approvals: status.pending_approvals.len() as u32,
            extension_connected: status.extension_connected,
            kill_switch_engaged: self.journal.meta().map_err(error)?.kill_switch_engaged(),
            journal_ok: true,
        })
    }
    async fn daemon_shutdown(
        &self,
        ctx: &CallContext,
        p: ShutdownParams,
    ) -> ApiResult<ShutdownResult> {
        let grace_ms = p.grace_ms.unwrap_or(10_000);
        if grace_ms > 60_000 {
            return Err(ApiError::invalid_params("grace_ms exceeds 60000"));
        }
        self.engine
            .begin_shutdown(zeus_types::event::StopReason::UserRequest)
            .map_err(error)?;
        let grace_ms =
            self.shutdown
                .request(grace_ms, "shutdown_requested", Some(ctx.reply.clone()));
        Ok(ShutdownResult {
            replayed: false,
            accepted: true,
            grace_ms,
        })
    }
    async fn daemon_kill_switch(
        &self,
        ctx: &CallContext,
        p: KillSwitchParams,
    ) -> ApiResult<KillSwitchResult> {
        let surface = p
            .surface
            .ok_or_else(|| ApiError::human_required("daemon.kill_switch"))?;
        let actor = self.trust.terminal(ctx, surface)?;
        self.change_kill_switch(
            actor,
            p.action == KillSwitchAction::Engage,
            p.reason.as_deref(),
        )
        .await
    }

    async fn jobs_probe(&self, ctx: &CallContext, p: ProbeParams) -> ApiResult<ProbeResultView> {
        self.admit()?;
        if p.session_origin.is_some() {
            return Err(ApiError::not_implemented(
                "standalone authenticated probing; enqueue a consent-gated job instead",
            ));
        }
        let probe = self
            .engine
            .probe(zeus_engine::api::ProbeUrlRequest {
                url: p.url,
                session_origin: p.session_origin,
                caller: Self::caller(ctx),
            })
            .await
            .map_err(error)?;
        let approval_needed = probe.requires_session;
        Ok(ProbeResultView {
            probe,
            effective_grant: None,
            approval_needed,
        })
    }
    async fn jobs_enqueue(&self, ctx: &CallContext, p: EnqueueParams) -> ApiResult<EnqueueResult> {
        let _gate = self.mutations.lock().await;
        if p.jobs.is_empty() || p.jobs.len() > 100 {
            return Err(ApiError::invalid_params(
                "jobs must contain 1 to 100 entries",
            ));
        }
        if p.jobs.iter().any(|j| j.run_id.is_some()) {
            return Err(ApiError::invalid_params(
                "run_id is assigned only by the workflow runner",
            ));
        }
        let receipt_key = if p.dry_run {
            None
        } else {
            p.idempotency_key
                .as_deref()
                .map(|key| {
                    crate::mutations::MutationStore::binding(
                        ctx.identity.credential_id.as_str(),
                        "jobs.enqueue",
                        key,
                    )
                })
                .transpose()?
        };
        let mut receipt = receipt_key
            .as_ref()
            .map(|key| {
                self.receipts
                    .begin(key, serde_json::to_value(&p).map_err(|_| internal())?)
            })
            .transpose()?;
        if let Some(result) = receipt.as_ref().and_then(|r| r.result.as_ref()) {
            let mut result: EnqueueResult =
                serde_json::from_value(result.clone()).map_err(|_| internal())?;
            result.replayed = true;
            return Ok(result);
        }
        self.admit()?;
        // A service writes only inside its registered roots; resolve every entry before any job exists.
        let service_dirs = match Self::scope(ctx).service() {
            Some(_) => Some(
                p.jobs
                    .iter()
                    .map(|job| {
                        self.service_destination(
                            &ctx.identity.credential_id,
                            job.dest_dir.as_deref(),
                        )
                    })
                    .collect::<ApiResult<Vec<_>>>()?,
            ),
            None => None,
        };
        let mut entries: Vec<EnqueuedJob> = receipt
            .as_ref()
            .map(|r| {
                r.entries
                    .iter()
                    .cloned()
                    .map(serde_json::from_value)
                    .collect()
            })
            .transpose()
            .map_err(|_| internal())?
            .unwrap_or_default();
        for (index, job) in p.jobs.iter().enumerate() {
            if index < entries.len() {
                continue;
            }
            let mut value = serde_json::to_value(job).map_err(|_| internal())?;
            value["source"] = serde_json::to_value(
                ctx.identity
                    .role
                    .job_source(ctx.identity.credential_id.as_str()),
            )
            .map_err(|_| internal())?;
            if let Some(dirs) = &service_dirs {
                value["dest_dir"] = serde_json::json!(dirs[index]);
            }
            if let Some(key) = &p.idempotency_key {
                if key.is_empty() || key.len() > 128 {
                    return Err(ApiError::invalid_params(
                        "idempotency key must contain 1 to 128 bytes",
                    ));
                }
                value["idempotency_key"] = serde_json::json!(
                    blake3::hash(format!("jobs.enqueue:{key}:{index}").as_bytes())
                        .to_hex()
                        .to_string()
                );
            }
            let spec: JobSpec = serde_json::from_value(value)
                .map_err(|_| ApiError::invalid_params("invalid job"))?;
            spec.validate().map_err(ApiError::from)?;
            if p.dry_run {
                let roots = zeus_fs::WriteRoots::open(&self.config.effective_write_roots())
                    .map_err(error)?;
                let default = self.config.effective_write_roots()[0].clone();
                let dir = roots
                    .resolve_dest_dir(spec.dest_dir.as_deref().map(Path::new).unwrap_or(&default))
                    .map_err(error)?;
                let filename = spec
                    .filename
                    .as_deref()
                    .map(|name| zeus_fs::sanitize_filename(name, &Default::default()))
                    .transpose()
                    .map_err(error)?
                    .map(|n| n.name);
                entries.push(EnqueuedJob {
                    job_id: None,
                    state: None,
                    approval_id: None,
                    plan: Some(PlannedJob {
                        url_redacted: spec.url.redact(zeus_types::RedactMode::Log),
                        dest_path: filename.as_ref().map(|n| dir.join(n).display().to_string()),
                        resolved_filename: filename,
                        approval_needed: spec.session_origin.is_some(),
                        warnings: Vec::new(),
                    }),
                    error: None,
                });
                continue;
            }
            match self
                .engine
                .enqueue(zeus_engine::api::EnqueueRequest {
                    spec,
                    caller: Self::caller(ctx),
                })
                .await
            {
                Ok(result) => entries.push(EnqueuedJob {
                    job_id: Some(result.job.id),
                    state: Some(result.job.state),
                    approval_id: result.approval.map(|a| a.id),
                    plan: None,
                    error: None,
                }),
                Err(e) => entries.push(EnqueuedJob {
                    job_id: None,
                    state: None,
                    approval_id: None,
                    plan: None,
                    error: Some(e.to_envelope()),
                }),
            }
            if let (Some(key), Some(receipt)) = (&receipt_key, &mut receipt) {
                receipt
                    .entries
                    .push(serde_json::to_value(entries.last()).map_err(|_| internal())?);
                self.receipts.save(key, receipt)?;
            }
        }
        let result = EnqueueResult {
            replayed: false,
            jobs: entries,
            dry_run: p.dry_run,
        };
        if let (Some(key), Some(receipt)) = (&receipt_key, &mut receipt) {
            receipt.result = Some(serde_json::to_value(&result).map_err(|_| internal())?);
            self.receipts.save(key, receipt)?;
        }
        Ok(result)
    }
    async fn jobs_list(&self, ctx: &CallContext, p: JobsListParams) -> ApiResult<JobsListResult> {
        p.page.validate()?;
        let scope = Self::scope(ctx);
        let rows = self
            .journal
            .read(|tx| tx.jobs(&zeus_journal::store::JobFilter::default()))
            .map_err(error)?;
        let items = rows
            .into_iter()
            .map(|r| r.job)
            .filter(|j| {
                scope.job(j)
                    && (p.states.is_empty() || p.states.contains(&j.state))
                    && p.run_id.is_none_or(|r| j.spec.source.run_id == Some(r))
                    && p.source_kind.is_none_or(|s| j.spec.source.kind == s)
                    && p.origin.as_ref().is_none_or(|o| &j.spec.origin() == o)
            })
            .map(|j| {
                self.journal
                    .read(|tx| tx.job(j.id))
                    .map_err(error)?
                    .map(|r| r.summary())
                    .ok_or_else(internal)
            })
            .collect::<ApiResult<Vec<_>>>()?;
        page(items, &p.page)
    }
    async fn jobs_get(&self, ctx: &CallContext, p: JobsGetParams) -> ApiResult<JobsGetResult> {
        let job = self.engine.get(p.job_id).await.map_err(error)?;
        Self::scope(ctx).require_job(&job)?;
        let revealed = if p.reveal_urls {
            let surface = if ctx.identity.role == zeus_api::ClientRole::Tui {
                Surface::Tui
            } else {
                Surface::CliInteractive
            };
            let actor = self.trust.terminal(ctx, surface)?;
            use zeus_verify::audit::{AuditEntry, AuditOutcome, AuditSink};
            let mut entry = AuditEntry::new(Timestamp::now(), actor, "tool.call", AuditOutcome::Ok);
            entry.write_ahead = true;
            entry.subject.job_id = Some(job.id);
            entry
                .data
                .insert("method".into(), serde_json::json!("jobs.get"));
            entry
                .data
                .insert("reveal_urls".into(), serde_json::json!(true));
            self.audit.append(entry).map_err(error)?;
            Some(RevealedUrls {
                url: job.spec.url.clone(),
                hint_url: job.spec.hint_url.clone(),
            })
        } else {
            None
        };
        Ok(JobsGetResult {
            job: (&job).into(),
            revealed,
        })
    }
    async fn jobs_wait(&self, ctx: &CallContext, p: JobsWaitParams) -> ApiResult<JobsWaitResult> {
        if p.job_ids.is_empty()
            || p.job_ids.len() > 100
            || !(1..=MAX_WAIT_MS).contains(&p.timeout_ms)
        {
            return Err(ApiError::invalid_params("invalid jobs.wait bounds"));
        }
        self.require_jobs_visible(ctx, &p.job_ids)?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(p.timeout_ms);
        let cursor = p
            .since_seq
            .unwrap_or(self.journal.meta().map_err(error)?.last_event_seq);
        if cursor > self.journal.meta().map_err(error)?.last_event_seq {
            return Err(ApiError::invalid_params("cursor is ahead of the journal"));
        }
        if p.job_ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != p.job_ids.len()
            || p.progress_bytes == Some(0)
        {
            return Err(ApiError::invalid_params(
                "duplicate jobs or zero progress bound",
            ));
        }
        let mut initial_bytes: Option<Vec<u64>> = None;
        loop {
            let mut jobs = Vec::new();
            for id in &p.job_ids {
                jobs.push(self.engine.summary(*id).await.map_err(error)?);
            }
            let states: Vec<_> = jobs.iter().map(|j| j.state).collect();
            let final_state = |s: &JobState| {
                matches!(
                    s,
                    JobState::Completed | JobState::Failed | JobState::Canceled
                )
            };
            let bytes: Vec<_> = jobs.iter().map(|j| j.bytes_done).collect();
            let progressed = p.progress_bytes.is_some_and(|amount| {
                initial_bytes.as_ref().is_some_and(|before| {
                    bytes
                        .iter()
                        .zip(before)
                        .any(|(now, old)| now.saturating_sub(*old) >= amount)
                })
            });
            let ready = progressed
                || match p.until {
                    WaitUntil::AllTerminal => states.iter().all(final_state),
                    WaitUntil::SettledOrBlocked => jobs.iter().any(|j| {
                        (final_state(&j.state)
                            || matches!(
                                j.state,
                                JobState::AwaitingApproval
                                    | JobState::AwaitingSession
                                    | JobState::Paused
                            ))
                            && (p.since_seq.is_none()
                                || j.state_seq.is_some_and(|seq| seq.get() > cursor))
                    }),
                    WaitUntil::AnyChange => jobs
                        .iter()
                        .any(|j| j.state_seq.is_some_and(|seq| seq.get() > cursor)),
                };
            let timed_out = tokio::time::Instant::now() >= deadline;
            if ready || timed_out {
                return Ok(JobsWaitResult {
                    jobs,
                    timed_out: !ready,
                    head_seq: self.journal.meta().map_err(error)?.last_event_seq,
                });
            }
            initial_bytes.get_or_insert(bytes);
            tokio::time::sleep_until(
                deadline.min(tokio::time::Instant::now() + Duration::from_millis(100)),
            )
            .await;
        }
    }
    async fn jobs_control(
        &self,
        ctx: &CallContext,
        p: JobsControlParams,
    ) -> ApiResult<JobsControlResult> {
        self.require_jobs_visible(ctx, &[p.job_id])?;
        let _gate = self.mutations.lock().await;
        let action = match p.action {
            ControlAction::Pause => zeus_engine::api::ControlAction::Pause,
            ControlAction::Cancel => zeus_engine::api::ControlAction::Cancel,
            ControlAction::Resume => {
                self.admit()?;
                zeus_engine::api::ControlAction::Resume
            }
            ControlAction::Retry | ControlAction::RetryNow => {
                self.admit()?;
                zeus_engine::api::ControlAction::Retry
            }
            ControlAction::Reprioritize => zeus_engine::api::ControlAction::SetPriority(
                p.priority
                    .ok_or_else(|| ApiError::invalid_params("priority required"))?,
            ),
        };
        let replayed = p
            .idempotency_key
            .as_deref()
            .map(|key| {
                self.journal
                    .read(|tx| tx.idempotency(ctx.identity.credential_id.as_str(), key))
                    .map(|row| row.is_some())
            })
            .transpose()
            .map_err(error)?
            .unwrap_or(false);
        let job = if let Some(key) = &p.idempotency_key {
            let mut params = serde_json::to_value(&p).map_err(|_| internal())?;
            params
                .as_object_mut()
                .ok_or_else(internal)?
                .remove("idempotency_key");
            self.engine
                .control_idempotent(
                    p.job_id,
                    action,
                    ctx.identity.credential_id.as_str(),
                    key,
                    crate::approvals::facts_hash(&params)?,
                    ctx.identity.action_actor(),
                )
                .await
                .map_err(error)?
        } else {
            self.engine.control(p.job_id, action).await.map_err(error)?;
            self.engine.summary(p.job_id).await.map_err(error)?
        };
        Ok(JobsControlResult { replayed, job })
    }
    async fn jobs_verify(
        &self,
        ctx: &CallContext,
        p: JobsVerifyParams,
    ) -> ApiResult<JobsVerifyResult> {
        let job = self.engine.get(p.job_id).await.map_err(error)?;
        Self::scope(ctx).require_job(&job)?;
        let original = job
            .result
            .ok_or_else(|| ApiError::new("api.invalid_transition", "job is not complete"))?;
        let hashes = self
            .engine
            .verify(p.job_id, p.expected_sha256)
            .await
            .map_err(error)?;
        Ok(JobsVerifyResult {
            sha256: hashes.sha256,
            blake3: hashes.blake3,
            size_bytes: hashes.size_bytes,
            detected_mime: None,
            matches_recorded: original.sha256 == hashes.sha256,
            matches_expected: p.expected_sha256.map(|e| e == hashes.sha256),
            quarantined: job.quarantined,
            quarantine_reasons: job.quarantine_reasons,
        })
    }
    async fn events_subscribe(
        &self,
        ctx: &CallContext,
        p: SubscribeParams,
    ) -> ApiResult<SubscribeResult> {
        self.subscribe_events(ctx, p, None).await
    }
    async fn events_unsubscribe(
        &self,
        ctx: &CallContext,
        p: UnsubscribeParams,
    ) -> ApiResult<UnsubscribeResult> {
        let mut subscriptions = self.subscriptions.lock().await;
        let owned = subscriptions
            .get(&p.subscription_id)
            .is_some_and(|(id, _)| id == &ctx.connection_id);
        if owned && let Some((_, task)) = subscriptions.remove(&p.subscription_id) {
            task.abort();
        }
        Ok(UnsubscribeResult { removed: owned })
    }
    async fn sessions_request(
        &self,
        ctx: &CallContext,
        p: SessionsRequestParams,
    ) -> ApiResult<SessionsRequestResult> {
        self.admit()?;
        self.consent.request_session(&ctx.identity, p).await
    }
    async fn approvals_list(
        &self,
        ctx: &CallContext,
        p: ApprovalsListParams,
    ) -> ApiResult<ApprovalsListResult> {
        page(
            self.consent.list(
                &ctx.identity,
                p.state.unwrap_or(ApprovalState::Pending),
                p.kind,
            )?,
            &p.page,
        )
    }
    async fn approvals_get(
        &self,
        ctx: &CallContext,
        p: ApprovalsGetParams,
    ) -> ApiResult<ApprovalsGetResult> {
        let approval = self.consent.get(&ctx.identity, p.approval_id)?.try_into()?;
        let decision_record = if matches!(
            ctx.identity.kind,
            zeus_api::ClientKind::Human | zeus_api::ClientKind::Admin
        ) {
            self.journal
                .read(|tx| tx.approval_decision(p.approval_id))
                .map_err(error)?
        } else {
            None
        };
        Ok(ApprovalsGetResult {
            approval,
            decision_record,
        })
    }
    async fn approvals_decide(
        &self,
        ctx: &CallContext,
        p: ApprovalsDecideParams,
    ) -> ApiResult<ApprovalsDecideResult> {
        let evidence = self
            .trust
            .terminal_evidence(ctx, p.channel, p.confirmation)?;
        if p.outcome == DecideOutcome::Approve {
            self.admit()?;
        }
        let (approval, grant) = self.consent.decide(&ctx.identity, &p, evidence).await?;
        self.apply_decision(&approval, &grant).await?;
        Ok(ApprovalsDecideResult {
            replayed: false,
            approval: approval.try_into()?,
            grant,
        })
    }
    async fn sessions_list(
        &self,
        ctx: &CallContext,
        p: SessionsListParams,
    ) -> ApiResult<SessionsListResult> {
        let scope = Self::scope(ctx);
        let grants = self
            .journal
            .read(|tx| tx.grants())
            .map_err(error)?
            .into_iter()
            .filter(|g| {
                scope.grant(g, &self.journal)
                    && p.origin.as_ref().is_none_or(|o| o == &g.origin)
                    && (p.include_inactive || g.is_effective_at(Timestamp::now()))
            })
            .collect();
        page(grants, &p.page)
    }
    async fn sessions_revoke(
        &self,
        ctx: &CallContext,
        p: SessionsRevokeParams,
    ) -> ApiResult<SessionsRevokeResult> {
        let actor = self.trust.terminal(ctx, p.surface)?;
        Ok(SessionsRevokeResult {
            replayed: false,
            grant: self.revoke(p.grant_id, actor).await?,
        })
    }
    async fn config_get(
        &self,
        _ctx: &CallContext,
        p: ConfigGetParams,
    ) -> ApiResult<ConfigGetResult> {
        self.read_configuration(p)
    }
    async fn config_set(
        &self,
        ctx: &CallContext,
        p: ConfigSetParams,
    ) -> ApiResult<ConfigSetResult> {
        self.write_configuration(ctx, p).await
    }
    async fn audit_tail(
        &self,
        ctx: &CallContext,
        p: AuditTailParams,
    ) -> ApiResult<AuditTailResult> {
        let scope = Self::scope(ctx);
        let limit = p.limit.unwrap_or(20);
        if !(1..=100).contains(&limit) {
            return Err(ApiError::invalid_params("audit limit must be 1 to 100"));
        }
        let audit = self.audit.clone();
        let (records, head) = tokio::task::spawn_blocking(move || {
            audit.tail(p.after_seq.unwrap_or(0), limit as usize)
        })
        .await
        .map_err(|_| internal())?
        .map_err(error)?;
        Ok(AuditTailResult {
            // A service sees the records its own credential caused (05 section 7.5).
            records: records
                .into_iter()
                .filter(|r| scope.audit(r.actor.client_id.as_deref()))
                .map(serde_json::to_value)
                .collect::<Result<_, _>>()
                .map_err(|_| internal())?,
            head: Some(head),
        })
    }
    async fn audit_verify(
        &self,
        _ctx: &CallContext,
        p: AuditVerifyParams,
    ) -> ApiResult<AuditVerifyResult> {
        let audit = self.audit.clone();
        let journal_head = self.journal.meta().map_err(error)?.audit_head;
        if p.from_seq == Some(0) {
            return Err(ApiError::invalid_params("audit sequence begins at one"));
        }
        // Verification includes the prefix even when the caller names a later start; continuity is preserved.
        let report = tokio::task::spawn_blocking(move || {
            audit.verify_snapshot(&zeus_verify::audit::VerifyOptions {
                journal_head,
                ..Default::default()
            })
        })
        .await
        .map_err(|_| internal())?;
        match report {
            Ok(report) => Ok(AuditVerifyResult {
                ok: true,
                records_checked: report.records,
                first_broken_seq: None,
                head: report.head,
            }),
            Err(zeus_verify::VerifyError::Audit(failure)) => Ok(AuditVerifyResult {
                ok: false,
                records_checked: 0,
                first_broken_seq: failure.seq,
                head: None,
            }),
            Err(e) => Err(error(e)),
        }
    }
    async fn llm_status(
        &self,
        _ctx: &CallContext,
        _p: LlmStatusParams,
    ) -> ApiResult<LlmStatusResult> {
        self.models.status()
    }
    async fn llm_configure(
        &self,
        ctx: &CallContext,
        p: LlmConfigureParams,
    ) -> ApiResult<LlmConfigureResult> {
        let surface = if ctx.identity.role == zeus_api::ClientRole::Tui {
            Surface::Tui
        } else {
            Surface::CliInteractive
        };
        let actor = self.trust.terminal(ctx, surface)?;
        use zeus_verify::audit::{AuditEntry, AuditOutcome, AuditSink};
        let mut entry =
            AuditEntry::new(Timestamp::now(), actor, "config.changed", AuditOutcome::Ok);
        entry.data.insert("key".into(), serde_json::json!("models"));
        self.audit.append(entry).map_err(error)?;
        self.models.configure(p)
    }
    async fn llm_auth_start(
        &self,
        ctx: &CallContext,
        p: LlmAuthStartParams,
    ) -> ApiResult<LlmAuthStartResult> {
        self.model_human(ctx)?;
        self.models
            .auth_start(ctx.identity.credential_id.as_str(), p)
            .await
    }
    async fn llm_auth_status(
        &self,
        ctx: &CallContext,
        p: LlmAuthStatusParams,
    ) -> ApiResult<LlmAuthStatusResult> {
        self.model_human(ctx)?;
        self.models
            .auth_status(ctx.identity.credential_id.as_str(), p)
    }
    async fn llm_auth_cancel(
        &self,
        ctx: &CallContext,
        p: LlmAuthCancelParams,
    ) -> ApiResult<LlmAuthCancelResult> {
        self.model_human(ctx)?;
        self.models
            .auth_cancel(ctx.identity.credential_id.as_str(), p)
    }
    async fn llm_auth_sign_out(
        &self,
        ctx: &CallContext,
        p: LlmAuthSignOutParams,
    ) -> ApiResult<LlmAuthSignOutResult> {
        self.model_human(ctx)?;
        self.models.auth_sign_out(p).await
    }
    async fn doctor_run(&self, _ctx: &CallContext, _p: DoctorParams) -> ApiResult<DoctorResult> {
        let ok = self.journal.meta().is_ok();
        Ok(DoctorResult {
            checks: vec![DoctorCheck {
                id: "journal.readable".into(),
                status: if ok {
                    CheckStatus::Ok
                } else {
                    CheckStatus::Fail
                },
                summary: "Durable journal read check".into(),
                fix: None,
            }],
        })
    }
    async fn files_list(
        &self,
        ctx: &CallContext,
        p: FilesListParams,
    ) -> ApiResult<FilesListResult> {
        // A service lists only its registered roots (05 section 7.5); everyone else the user's write roots.
        let visible = match Self::scope(ctx).service() {
            Some(_) => self.credentials.roots(&ctx.identity.credential_id),
            None => self.config.effective_write_roots(),
        };
        let first = visible.first().cloned().ok_or_else(|| {
            ApiError::new(
                zeus_types::codes::POLICY_PATH_OUTSIDE_ROOTS,
                "no write root is visible to this client",
            )
        })?;
        let roots = zeus_fs::WriteRoots::open(&visible).map_err(error)?;
        let dir = p.dir.as_deref().map(PathBuf::from).unwrap_or(first);
        let dir = roots.resolve_dest_dir(&dir).map_err(error)?;
        let root = roots.root_for(&dir).map_err(error)?;
        let mut pending = vec![dir];
        let mut entries = Vec::new();
        let mut scanned = 0usize;
        while let Some(dir) = pending.pop() {
            for item in std::fs::read_dir(&dir).map_err(|_| internal())? {
                let item = item.map_err(|_| internal())?;
                scanned += 1;
                if scanned > 100_000 {
                    return Err(ApiError::new(
                        "api.payload_too_large",
                        "file scan exceeds 100000 entries; narrow the directory",
                    ));
                }
                if item.file_name() == ".zeus" {
                    continue;
                }
                let kind = item.file_type().map_err(|_| internal())?;
                if kind.is_symlink() {
                    continue;
                }
                let checked = root.check_existing(&item.path()).map_err(error)?;
                if kind.is_dir() {
                    if p.recursive {
                        pending.push(checked);
                    }
                    continue;
                }
                if !kind.is_file()
                    || (!p.include_partial && checked.extension().is_some_and(|e| e == "zpart"))
                {
                    continue;
                }
                let metadata = item.metadata().map_err(|_| internal())?;
                let modified_ms = metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                entries.push(FileEntry {
                    path: checked
                        .strip_prefix(root.path())
                        .map_err(|_| internal())?
                        .display()
                        .to_string(),
                    size_bytes: metadata.len(),
                    modified_at: Timestamp::from_unix_millis(modified_ms)
                        .map_err(|_| internal())?,
                    sha256: None,
                    quarantined: false,
                    job_id: None,
                    source_url_redacted: None,
                });
                if entries.len() > 100_000 {
                    return Err(ApiError::new(
                        "api.payload_too_large",
                        "file scan exceeds 100000 entries; narrow the directory",
                    ));
                }
            }
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        page(entries, &p.page)
    }
    async fn files_roots_register(
        &self,
        ctx: &CallContext,
        p: FilesRootsRegisterParams,
    ) -> ApiResult<FilesRootsRegisterResult> {
        self.register_root(ctx, p).await
    }
    async fn clients_list(
        &self,
        _ctx: &CallContext,
        p: ClientsListParams,
    ) -> ApiResult<ClientsListResult> {
        self.list_clients(p)
    }
    async fn clients_add(
        &self,
        ctx: &CallContext,
        p: ClientsAddParams,
    ) -> ApiResult<ClientsAddResult> {
        self.add_client(ctx, p).await
    }
    async fn clients_revoke(
        &self,
        ctx: &CallContext,
        p: ClientsRevokeParams,
    ) -> ApiResult<ClientsRevokeResult> {
        self.revoke_client(ctx, p).await
    }
}

#[path = "native.rs"]
mod native;

#[path = "clients.rs"]
mod clients;
#[path = "configuration.rs"]
mod configuration;
#[path = "events_backend.rs"]
mod events_backend;
#[path = "lifecycle.rs"]
mod lifecycle;
#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
