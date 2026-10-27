//! Approval-gated execution, durable checkpoints and conservative crash reconciliation.
use super::*;
use zeus_cloud::{
    CheckpointPhase, CheckpointSink, CloudError, CloudResult, ImportRequest, ImportStatus,
    PrivateCheckpoint, RemoteArtifact,
};
struct Sink {
    service: Arc<CloudService>,
    id: String,
}
#[async_trait]
impl CheckpointSink for Sink {
    async fn save(&self, checkpoint: &PrivateCheckpoint) -> CloudResult<()> {
        self.service
            .guard(&self.id)
            .map_err(|_| CloudError::Cancelled)?;
        if checkpoint.request.operation_id != self.id {
            return Err(CloudError::CheckpointMismatch);
        }
        self.service.audit(Actor::system(),"cloud.checkpoint",serde_json::json!({"delivery_id":self.id,"provider":checkpoint.provider,"committed_bytes":checkpoint.committed_bytes,"phase":checkpoint.phase})).map_err(|_|CloudError::Checkpoint)?;
        self.service
            .store
            .mutate_delivery(&self.id, |row| {
                if row.stop.is_some() {
                    return Err(invalid("delivery stopped"));
                }
                row.view.committed_bytes = checkpoint.committed_bytes;
                row.upload = checkpoint.request.clone();
                row.checkpoint = Some(checkpoint.clone());
                if checkpoint.phase == CheckpointPhase::ImportPending {
                    row.view.state = CloudDeliveryState::RemotePending;
                }
                Ok(())
            })
            .map_err(|_| CloudError::Checkpoint)
    }
}
enum Outcome {
    Complete(RemoteArtifact),
    Pending,
    Canceled,
    Attention(CloudDeliveryState, &'static str),
}
impl CloudService {
    pub(super) fn authorized(&self, row: &Delivery) -> ApiResult<()> {
        self.admit()?;
        if !self.consent.requester_active(&row.owner) {
            return Err(ApiError::new(
                "cloud.requester_revoked",
                "requesting client credential was revoked",
            ));
        }
        let account = self.account(&row.view.plan.account_id)?;
        if account.view.revoked || account.view.revision != row.view.plan.account_revision {
            return Err(ApiError::new(
                "cloud.account_changed",
                "account revision no longer authorizes this delivery",
            ));
        }
        let now = Timestamp::now();
        if now >= row.view.plan.expires_at
            || row.started_at.is_some_and(|at| {
                now >= at.saturating_add_ms(u64::from(row.view.plan.max_seconds) * 1000)
            })
        {
            return Err(ApiError::new(
                "cloud.deadline_exceeded",
                "cloud delivery authorization expired",
            ));
        }
        let approval_id = row.view.approval_id.ok_or_else(|| {
            ApiError::new(
                "cloud.approval_required",
                "cloud delivery needs human approval",
            )
        })?;
        self.journal
            .read(|tx| {
                let approval =
                    tx.approval(approval_id)?
                        .ok_or(zeus_journal::JournalError::Invariant(
                            "cloud approval missing",
                        ))?;
                let record = tx.approval_decision(approval_id)?.ok_or(
                    zeus_journal::JournalError::Invariant("cloud decision evidence missing"),
                )?;
                if approval.kind != ApprovalKind::CloudDelivery
                    || approval.state != ApprovalState::Approved
                    || approval.expires_at <= now
                    || approval.requested_by.as_deref() != Some(row.owner.as_str())
                    || approval
                        .facts
                        .as_ref()
                        .and_then(|f| f.get("plan_blake3"))
                        .and_then(|v| v.as_str())
                        != Some(row.view.plan.plan_hash.to_string().as_str())
                    || approval
                        .facts
                        .as_ref()
                        .and_then(|f| f.get("delivery_id"))
                        .and_then(|v| v.as_str())
                        != Some(row.view.delivery_id.as_str())
                    || record["outcome"] != "approve"
                    || record["request_hash"]
                        != serde_json::to_value(approval.request_hash).unwrap_or_default()
                    || approval.facts.as_ref().and_then(|f| {
                        crate::approvals::facts_hash(&serde_json::Value::Object(f.clone())).ok()
                    }) != approval.request_hash
                {
                    return Err(zeus_journal::JournalError::Invariant(
                        "cloud approval binding invalid",
                    ));
                }
                if tx.meta()?.kill_switch_engaged() {
                    return Err(zeus_journal::JournalError::Invariant("kill switch engaged"));
                }
                Ok(())
            })
            .map_err(|_| {
                ApiError::new(
                    "cloud.approval_required",
                    "cloud delivery lacks a current, evidenced human approval",
                )
            })
    }
    pub(super) fn guard(&self, id: &str) -> ApiResult<()> {
        let row: Delivery = self
            .store
            .get(&format!("delivery:{id}"))?
            .ok_or_else(missing)?;
        if row.stop.is_some()
            || !matches!(
                row.view.state,
                CloudDeliveryState::Running | CloudDeliveryState::RemotePending
            )
        {
            return Err(ApiError::new(
                "cloud.delivery_stopped",
                "cloud delivery stopped",
            ));
        }
        self.authorized(&row)
    }
    /// Start bounded work or poll an existing remote receipt. Never blindly repeats uncertain mutations.
    pub async fn tick(self: &Arc<Self>) -> ApiResult<()> {
        let _gate = self.gate.lock().await;
        if self.stopping.load(Ordering::Acquire) {
            return Ok(());
        }
        self.retain_hourly();
        self.workers
            .lock()
            .map_err(|_| fail())?
            .retain(|_, w| !w.task.is_finished());
        if self
            .journal
            .meta()
            .map_err(|_| fail())?
            .kill_switch_engaged()
        {
            return Ok(());
        }
        for key in self.store.keys("delivery:")? {
            let mut row: Delivery = self.store.get(&key)?.ok_or_else(missing)?;
            if terminal(row.view.state) {
                if row.view.cleanup_pending {
                    self.cleanup_staged(&row).await?;
                }
                continue;
            }
            if self
                .workers
                .lock()
                .map_err(|_| fail())?
                .contains_key(&row.view.delivery_id)
            {
                continue;
            }
            if self.workers.lock().map_err(|_| fail())?.len() >= 4 {
                break;
            }
            if row.view.state == CloudDeliveryState::AwaitingApproval {
                self.ensure_approval(&mut row).await?;
                let approval = self
                    .journal
                    .read(|tx| {
                        tx.approval(row.view.approval_id.ok_or(
                            zeus_journal::JournalError::Invariant("cloud approval missing"),
                        )?)
                    })
                    .map_err(|_| fail())?
                    .ok_or_else(missing)?;
                match approval.state {
                    ApprovalState::Pending => continue,
                    ApprovalState::Approved => {}
                    _ => {
                        self.stop_delivery(
                            &row.view.delivery_id,
                            CloudControlAction::Cancel,
                            "cloud.approval_closed",
                        )?;
                        continue;
                    }
                }
                self.store.mutate_delivery(&row.view.delivery_id, |r| {
                    r.view.state = CloudDeliveryState::Queued;
                    Ok(())
                })?;
                row.view.state = CloudDeliveryState::Queued;
            }
            let remote_observation =
                row.view.plan.mode == DeliveryMode::RemoteImport && row.checkpoint.is_some();
            let cleanup = row.stop == Some(CloudControlAction::Cancel) && row.checkpoint.is_some();
            if row.view.state == CloudDeliveryState::Paused && !cleanup {
                continue;
            }
            if !remote_observation
                && !cleanup
                && let Err(e) = self.authorized(&row)
            {
                self.stop_delivery(
                    &row.view.delivery_id,
                    CloudControlAction::Pause,
                    e.envelope().code.as_str(),
                )?;
                continue;
            }
            let account = self.account(&row.view.plan.account_id)?;
            if account.view.revoked || account.view.revision != row.view.plan.account_revision {
                continue;
            }
            // Polling is metadata-only and can report a late completion even after the local stop.
            if matches!(
                row.view.state,
                CloudDeliveryState::RemotePending
                    | CloudDeliveryState::RemoteCancellationUnavailable
                    | CloudDeliveryState::Reconciling
            ) && Timestamp::now()
                .duration_since(row.view.updated_at)
                .is_some_and(|d| d < std::time::Duration::from_secs(5))
            {
                continue;
            }
            let id = row.view.delivery_id.clone();
            self.store.mutate_delivery(&id, |r| {
                if r.started_at.is_none() {
                    r.started_at = Some(Timestamp::now());
                }
                if !remote_observation && !cleanup {
                    r.view.state = CloudDeliveryState::Running;
                }
                Ok(())
            })?;
            row = self
                .store
                .get(&format!("delivery:{id}"))?
                .ok_or_else(missing)?;
            let service = self.clone();
            let cancel = CancellationToken::new();
            let task_cancel = cancel.clone();
            let task = tokio::spawn(async move {
                service.run(row, account, task_cancel).await;
            });
            self.workers
                .lock()
                .map_err(|_| fail())?
                .insert(id, Worker { cancel, task });
        }
        Ok(())
    }
    async fn run(self: Arc<Self>, row: Delivery, account: Account, cancel: CancellationToken) {
        let id = row.view.delivery_id.clone();
        let timeout =
            if row.view.plan.mode == DeliveryMode::RemoteImport && row.checkpoint.is_some() {
                std::time::Duration::from_secs(60)
            } else {
                let elapsed = Timestamp::now()
                    .duration_since(row.started_at.unwrap_or_else(Timestamp::now))
                    .unwrap_or_default();
                std::time::Duration::from_secs(u64::from(row.view.plan.max_seconds))
                    .saturating_sub(elapsed)
            };
        let operation = self.operate(&row, &account, &cancel);
        tokio::pin!(operation);
        // Give adapters a bounded chance to kill and reap subprocesses after cancellation.
        // Their result during this grace cannot turn a local stop into an acknowledged success.
        let mut outcome = tokio::select! {
            _=cancel.cancelled()=>{
                let _=tokio::time::timeout(std::time::Duration::from_secs(2),&mut operation).await;
                Err(ApiError::new("cloud.delivery_stopped","local cloud worker stopped"))
            },
            result=tokio::time::timeout(timeout,&mut operation)=>match result{
                Ok(result)=>result,
                Err(_)=>{cancel.cancel();let _=tokio::time::timeout(std::time::Duration::from_secs(2),&mut operation).await;Err(ApiError::new("cloud.deadline_exceeded","cloud operation reached its deadline"))}
            }
        };
        if let Ok(Outcome::Complete(artifact)) = &outcome {
            if artifact.operation_id!=id||artifact.provider!=row.view.plan.provider||artifact.size!=row.view.plan.size_bytes||!artifact.object_path.ends_with(&row.view.plan.remote_name) {
                outcome=Err(ApiError::new("cloud.integrity_failed","provider receipt does not match the approved delivery"));
            }else if let Err(error)=self.audit(Actor::system(),"cloud.delivery_completed",serde_json::json!({"delivery_id":id,"plan_hash":row.view.plan.plan_hash,"size_bytes":artifact.size,"verification":artifact.verification})) {outcome=Err(error);}
        }
        let result = self.store.mutate_delivery(&id, |current| {
            match outcome {
                Ok(Outcome::Complete(artifact)) => {
                    current.view.committed_bytes = artifact.size;
                    current.view.artifact = Some(artifact);
                    current.view.state = CloudDeliveryState::Completed;
                    current.view.reason = current
                        .stop
                        .map(|_| "cloud.completed_before_remote_stop".into());
                }
                Ok(Outcome::Canceled) => {
                    current.view.state = CloudDeliveryState::Canceled;
                    current.view.reason = None;
                }
                Ok(Outcome::Pending) => {
                    current.view.state = if current.stop.is_some() {
                        CloudDeliveryState::RemoteCancellationUnavailable
                    } else {
                        CloudDeliveryState::RemotePending
                    };
                }
                Ok(Outcome::Attention(state, reason)) => {
                    current.view.state = state;
                    current.view.reason = Some(reason.into());
                }
                Err(error) => {
                    if current.stop.is_none() || current.view.reason.is_none() {
                        current.view.reason = Some(error.envelope().code.to_string());
                    }
                    if current.stop.is_none() {
                        current.view.state = if current.checkpoint.is_some() {
                            CloudDeliveryState::Reconciling
                        } else {
                            CloudDeliveryState::Failed
                        };
                    }
                }
            }
            if matches!(
                current.view.state,
                CloudDeliveryState::Completed | CloudDeliveryState::Canceled
            ) && current.view.plan.mode == DeliveryMode::StagedUpload
                && matches!(
                    current.source,
                    CloudSource::PublicUrl { .. } | CloudSource::LocalFile { .. }
                )
            {
                current.view.cleanup_pending = true;
            }
            Ok(current.view.clone())
        });
        if let Ok(view) = result {
            let _=self.audit(Actor::system(),"cloud.state_changed",serde_json::json!({"delivery_id":id,"plan_hash":view.plan.plan_hash,"state":view.state,"reason":view.reason,"committed_bytes":view.committed_bytes,"verification":view.artifact.as_ref().map(|a|a.verification)}));
            if view.cleanup_pending
                && let Ok(Some(current)) = self.store.get::<Delivery>(&format!("delivery:{id}"))
            {
                let _ = self.cleanup_staged(&current).await;
            }
        }
    }
    async fn cleanup_staged(&self, row: &Delivery) -> ApiResult<()> {
        if !matches!(
            row.source,
            CloudSource::PublicUrl { .. } | CloudSource::LocalFile { .. }
        ) || row.view.plan.mode != DeliveryMode::StagedUpload
            || !matches!(
                row.view.state,
                CloudDeliveryState::Completed | CloudDeliveryState::Canceled
            )
        {
            return Ok(());
        }
        validate_id(&row.view.delivery_id, "cld")?;
        let path = self.staging.join(format!("{}.part", row.view.delivery_id));
        let failed = if path.is_symlink() {
            true
        } else {
            match tokio::fs::remove_file(path).await {
                Ok(()) => false,
                Err(e) => e.kind() != std::io::ErrorKind::NotFound,
            }
        };
        self.store.mutate_delivery(&row.view.delivery_id, |stored| {
            stored.view.cleanup_pending = failed;
            if failed {
                stored.view.reason = Some("cloud.local_cleanup_failed".into());
            } else if stored.view.reason.as_deref() == Some("cloud.local_cleanup_failed") {
                stored.view.reason = None;
            }
            Ok(())
        })
    }

    async fn operate(
        self: &Arc<Self>,
        row: &Delivery,
        account: &Account,
        cancel: &CancellationToken,
    ) -> ApiResult<Outcome> {
        let credential = account
            .credential_key
            .as_ref()
            .map(|key| self.credentials.get(key))
            .transpose()
            .map_err(cloud_error)?;
        let provider = self
            .factory
            .connect(&account.config, credential, cancel)
            .await
            .map_err(cloud_error)?;
        let sink = Sink {
            service: self.clone(),
            id: row.view.delivery_id.clone(),
        };
        if row.view.plan.mode == DeliveryMode::RemoteImport {
            if let Some(checkpoint) = &row.checkpoint {
                if row.stop.is_some() && provider.capabilities().remote_cancel {
                    provider
                        .cancel_import(checkpoint, cancel)
                        .await
                        .map_err(cloud_error)?;
                    if let Some(artifact) = provider
                        .reconcile(checkpoint, cancel)
                        .await
                        .map_err(cloud_error)?
                    {
                        return Ok(Outcome::Complete(artifact));
                    }
                    return Ok(Outcome::Canceled);
                }
                if checkpoint.phase == CheckpointPhase::Prepared {
                    return Ok(
                        match provider
                            .reconcile(checkpoint, cancel)
                            .await
                            .map_err(cloud_error)?
                        {
                            Some(artifact) => Outcome::Complete(artifact),
                            None => Outcome::Attention(
                                if row.stop.is_some() {
                                    CloudDeliveryState::RemoteCancellationUnavailable
                                } else {
                                    CloudDeliveryState::Reconciling
                                },
                                "cloud.dispatch_uncertain",
                            ),
                        },
                    );
                }
                return Ok(
                    match provider
                        .poll_import(checkpoint, cancel)
                        .await
                        .map_err(cloud_error)?
                    {
                        ImportStatus::Pending => Outcome::Pending,
                        ImportStatus::Complete { artifact } => Outcome::Complete(artifact),
                        ImportStatus::Failed => {
                            Outcome::Attention(CloudDeliveryState::Failed, "cloud.remote_failed")
                        }
                    },
                );
            }
            self.guard(&row.view.delivery_id)?;
            let CloudSource::PublicUrl { url, .. } = &row.source else {
                return Err(invalid("remote_import requires a public URL"));
            };
            let source_url = source::import_url(url, row.view.plan.size_bytes, cancel).await?;
            self.guard(&row.view.delivery_id)?;
            provider
                .import_url(
                    &ImportRequest {
                        upload: row.upload.clone(),
                        source_url,
                    },
                    &sink,
                    cancel,
                )
                .await
                .map_err(cloud_error)?;
            return Ok(Outcome::Pending);
        }
        if let Some(checkpoint) = &row.checkpoint {
            if row.stop == Some(CloudControlAction::Cancel) {
                provider
                    .cleanup(checkpoint, cancel)
                    .await
                    .map_err(cloud_error)?;
                return Ok(
                    if provider
                        .reconcile(checkpoint, cancel)
                        .await
                        .map_err(cloud_error)?
                        .is_none()
                    {
                        Outcome::Canceled
                    } else {
                        Outcome::Attention(
                            CloudDeliveryState::Reconciling,
                            "cloud.cleanup_unconfirmed",
                        )
                    },
                );
            }
            if let Some(artifact) = provider
                .reconcile(checkpoint, cancel)
                .await
                .map_err(cloud_error)?
            {
                return Ok(Outcome::Complete(artifact));
            }
            if !provider.capabilities().resumable_upload
                || row.view.plan.mode != DeliveryMode::StagedUpload
            {
                return Ok(Outcome::Attention(
                    CloudDeliveryState::Reconciling,
                    "cloud.resume_unavailable",
                ));
            }
        }
        self.guard(&row.view.delivery_id)?;
        let (source, request) = source::upload_source(self, row, cancel).await?;
        self.guard(&row.view.delivery_id)?;
        let artifact = provider
            .upload(&request, source, row.checkpoint.clone(), &sink, cancel)
            .await
            .map_err(cloud_error)?;
        Ok(Outcome::Complete(artifact))
    }
}
