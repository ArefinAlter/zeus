//! Exact artifact lifecycle previews, independently approved weakening and durable retry checkpoints.
//!
//! Deletion tombstones records; collection is deliberately not an API operation. Copy materialization is
//! verified and no-replace in the core store. A crash with an uncertain destination fails closed rather than
//! treating a caller-created file as the result of an earlier copy.
use super::*;
use serde::{Deserialize, Serialize};
use zeus_artifact::{Artifact, ArtifactState, ObjectState, Retention};
use zeus_types::{ApprovalId, ApprovalKind, Hex256};

const PREVIEW_TTL_MS: u64 = 15 * 60 * 1_000;

/// Active copy keys let emergency stop and client revocation run while a large core copy finishes.
#[derive(Default)]
pub(super) struct ActiveCopies(Arc<std::sync::Mutex<std::collections::HashSet<String>>>);

pub(super) struct ActiveCopy {
    copies: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    key: String,
}

impl ActiveCopies {
    pub(super) fn contains(&self, key: &str) -> ApiResult<bool> {
        Ok(self.0.lock().map_err(|_| internal())?.contains(key))
    }

    pub(super) fn occupied(&self) -> ApiResult<bool> {
        Ok(!self.0.lock().map_err(|_| internal())?.is_empty())
    }

    pub(super) fn start(&self, key: &str) -> ApiResult<ActiveCopy> {
        let mut keys = self.0.lock().map_err(|_| internal())?;
        if !keys.is_empty() {
            return Err(ApiError::new(
                "artifact.operation_in_progress",
                "an artifact copy is still executing; wait for its receipt",
            ));
        }
        keys.insert(key.to_owned());
        Ok(ActiveCopy {
            copies: self.0.clone(),
            key: key.to_owned(),
        })
    }
}

impl Drop for ActiveCopy {
    fn drop(&mut self) {
        if let Ok(mut keys) = self.copies.lock() {
            keys.remove(&self.key);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Progress {
    before: serde_json::Value,
    approval_id: Option<ApprovalId>,
    started: bool,
}

fn stale() -> ApiError {
    ApiError::new(
        "artifact.plan_stale",
        "artifact state or destination changed; obtain and review a new lifecycle preview",
    )
}

fn plan_hash(plan: &ArtifactLifecyclePlan) -> ApiResult<Hex256> {
    crate::approvals::facts_hash(&serde_json::json!({
        "t": "zeus.artifact.lifecycle.v1", "plan": plan,
    }))
}

fn retention(value: Retention) -> ArtifactRetention {
    match value {
        Retention::Keep => ArtifactRetention::Keep,
        Retention::Evictable => ArtifactRetention::Evictable,
    }
}

fn weakens(record: &Artifact, policy: ArtifactRetention, until: Option<Timestamp>) -> bool {
    policy != ArtifactRetention::Keep
        && (record.retention == Retention::Keep
            || record
                .retain_until
                .is_some_and(|old| until.is_none_or(|new| new < old)))
}

impl RuntimeBackend {
    fn lifecycle_destination(&self, ctx: &CallContext, path: &str) -> ApiResult<PathBuf> {
        let outside = || {
            ApiError::new(
                zeus_types::codes::POLICY_PATH_OUTSIDE_ROOTS,
                "materialization needs a new file inside a permitted write root, outside Zeus state",
            )
        };
        let requested = Path::new(path);
        if path.len() > 4_096 || !requested.is_absolute() {
            return Err(outside());
        }
        let roots = zeus_fs::WriteRoots::open(&self.visible_roots(ctx)).map_err(|_| outside())?;
        let resolved = roots.resolve_dest_dir(requested).map_err(|_| outside())?;
        let state = self.state_dir.canonicalize().map_err(|_| internal())?;
        if resolved.starts_with(state)
            || resolved.file_name().is_none()
            || zeus_fs::finalize::is_zpart(&resolved)
            || resolved.components().any(|c| c.as_os_str() == ".zeus")
        {
            return Err(outside());
        }
        Ok(resolved)
    }

    fn lifecycle_plan(
        &self,
        ctx: &CallContext,
        id: zeus_types::ArtifactId,
        operation: ArtifactLifecycleOperation,
        expires_at: Timestamp,
    ) -> ApiResult<ArtifactLifecyclePlan> {
        let record = self.visible_artifact(ctx, id)?;
        if record.state != ArtifactState::Active {
            return Err(error(zeus_artifact::ArtifactError::NotFound));
        }
        let (operation, requires_approval) = match operation {
            ArtifactLifecycleOperation::Delete => (ArtifactLifecycleOperation::Delete, true),
            ArtifactLifecycleOperation::SetRetention {
                retention,
                retain_until,
            } => {
                if retain_until.is_some_and(|t| t <= Timestamp::now()) {
                    return Err(ApiError::invalid_params(
                        "retain_until must be in the future",
                    ));
                }
                let approval = weakens(&record, retention, retain_until);
                (
                    ArtifactLifecycleOperation::SetRetention {
                        retention,
                        retain_until,
                    },
                    approval,
                )
            }
            ArtifactLifecycleOperation::Materialize { path } => {
                if record.quarantined {
                    return Err(error(zeus_artifact::ArtifactError::QuarantineBlocked));
                }
                match record.object.state {
                    ObjectState::Present => {}
                    ObjectState::Absent => {
                        return Err(error(zeus_artifact::ArtifactError::ObjectAbsent));
                    }
                    ObjectState::Damaged => {
                        return Err(error(zeus_artifact::ArtifactError::ObjectDamaged));
                    }
                }
                let dest = self.lifecycle_destination(ctx, &path)?;
                for candidate in [&dest, &zeus_fs::zpart_path(&dest)] {
                    match std::fs::symlink_metadata(candidate) {
                        Ok(_) => return Err(error(zeus_fs::FsError::AlreadyExists)),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(_) => {
                            return Err(ApiError::new(
                                "fs.io_error",
                                "cannot inspect materialization destination",
                            ));
                        }
                    }
                }
                (
                    ArtifactLifecycleOperation::Materialize {
                        path: dest.display().to_string(),
                    },
                    false,
                )
            }
        };
        Ok(ArtifactLifecyclePlan {
            version: 1,
            artifact_id: id,
            expected_record_blake3: crate::approvals::facts_hash(&self.export_value(id)?)?,
            blake3: record.blake3,
            sha256: record.sha256,
            size_bytes: record.size_bytes,
            quarantined: record.quarantined,
            current_retention: retention(record.retention),
            current_retain_until: record.retain_until,
            operation,
            requires_approval,
            expires_at,
        })
    }

    pub(super) fn artifact_lifecycle_preview(
        &self,
        ctx: &CallContext,
        p: ArtifactsLifecyclePreviewParams,
    ) -> ApiResult<ArtifactsLifecyclePreviewResult> {
        let plan = self.lifecycle_plan(
            ctx,
            p.artifact_id,
            p.operation,
            Timestamp::now().saturating_add_ms(PREVIEW_TTL_MS),
        )?;
        Ok(ArtifactsLifecyclePreviewResult {
            plan_hash: plan_hash(&plan)?,
            plan,
        })
    }

    fn lifecycle_receipt(
        &self,
        plan: &ArtifactLifecyclePlan,
        approval_id: Option<ApprovalId>,
    ) -> ApiResult<ArtifactsLifecycleApplyResult> {
        let path = match &plan.operation {
            ArtifactLifecycleOperation::Materialize { path } => Some(path.clone()),
            _ => None,
        };
        Ok(ArtifactsLifecycleApplyResult {
            applied: true,
            approval_id,
            artifact: Some(self.export_value(plan.artifact_id)?),
            mode: path.as_ref().map(|_| "copy".to_owned()),
            path,
            replayed: false,
        })
    }

    /// Recognize only a completed record mutation after a durable started checkpoint.
    fn lifecycle_reconciled(
        &self,
        ctx: &CallContext,
        plan: &ArtifactLifecyclePlan,
        before: &serde_json::Value,
    ) -> ApiResult<bool> {
        self.visible_artifact(ctx, plan.artifact_id)?;
        let mut current = self.export_value(plan.artifact_id)?;
        let mut previous = before.clone();
        let fields: &[&str] = match &plan.operation {
            ArtifactLifecycleOperation::Delete if current["state"] == "deleted" => {
                &["state", "deleted_at"]
            }
            ArtifactLifecycleOperation::SetRetention {
                retention,
                retain_until,
            } if current["state"] == "active"
                && current["retention"]
                    == serde_json::to_value(retention).map_err(|_| internal())?
                && current["retain_until"]
                    == serde_json::to_value(retain_until).map_err(|_| internal())? =>
            {
                &["retention", "retain_until"]
            }
            _ => return Ok(false),
        };
        for value in [&mut current, &mut previous] {
            let map = value.as_object_mut().ok_or_else(internal)?;
            for field in fields {
                map.remove(*field);
            }
        }
        Ok(current == previous)
    }

    pub(super) async fn artifact_lifecycle_apply(
        &self,
        ctx: &CallContext,
        p: ArtifactsLifecycleApplyParams,
    ) -> ApiResult<ArtifactsLifecycleApplyResult> {
        if p.idempotency_key.is_empty()
            || p.idempotency_key.len() > 128
            || p.idempotency_key.chars().any(char::is_control)
        {
            return Err(ApiError::invalid_params(
                "idempotency_key must be 1–128 UTF-8 bytes without controls",
            ));
        }
        if plan_hash(&p.plan)? != p.plan_hash {
            return Err(stale());
        }
        self.admit()?;
        let gate = self.mutations.lock().await;
        self.admit()?;
        let (binding, replay) = self.receipt::<ArtifactsLifecycleApplyResult>(
            ctx,
            "artifacts.lifecycle.apply",
            Some(&p.idempotency_key),
            &p,
        )?;
        if let Some(mut result) = replay {
            result.replayed = true;
            return Ok(result);
        }
        let (key, mut receipt) = binding.ok_or_else(internal)?;
        if self.artifact_copies.contains(&key)? {
            return Err(ApiError::new(
                "artifact.operation_in_progress",
                "this copy is still executing; wait for its receipt",
            ));
        }
        let previous: Option<Progress> = receipt
            .entries
            .first()
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| internal())?;
        if let Some(progress) = previous.as_ref().filter(|r| r.started) {
            if self.lifecycle_reconciled(ctx, &p.plan, &progress.before)? {
                let mut result = self.lifecycle_receipt(&p.plan, progress.approval_id)?;
                result.replayed = true;
                self.store_receipt(Some((key, receipt)), &result)?;
                return Ok(result);
            }
            if let ArtifactLifecycleOperation::Materialize { path } = &p.plan.operation {
                let dest = self.lifecycle_destination(ctx, path)?;
                if std::fs::symlink_metadata(dest).is_ok() {
                    return Err(ApiError::new(
                        "artifact.lifecycle_reconciliation_required",
                        "an interrupted materialization has an existing destination; inspect it and preview a new destination",
                    ));
                }
            }
        }
        let now = Timestamp::now();
        if p.plan.expires_at <= now || p.plan.expires_at > now.saturating_add_ms(PREVIEW_TTL_MS) {
            return Err(ApiError::new(
                "artifact.plan_expired",
                "obtain and review a new lifecycle preview",
            ));
        }
        let current = self.lifecycle_plan(
            ctx,
            p.plan.artifact_id,
            p.plan.operation.clone(),
            p.plan.expires_at,
        )?;
        if current != p.plan {
            return Err(stale());
        }
        let mut progress = previous.clone().unwrap_or(Progress {
            before: self.export_value(p.plan.artifact_id)?,
            approval_id: None,
            started: false,
        });
        if p.plan.requires_approval {
            let approval_id = match progress.approval_id {
                Some(id) => id,
                None => {
                    let id = self
                        .consent
                        .request_artifact_mutation(&ctx.identity, &p.plan, p.plan_hash)
                        .await?;
                    progress.approval_id = Some(id);
                    receipt.entries =
                        vec![serde_json::to_value(&progress).map_err(|_| internal())?];
                    self.receipts.save(&key, &receipt)?;
                    id
                }
            };
            let approval = self.consent.get(&ctx.identity, approval_id)?;
            if approval.kind != ApprovalKind::DestructiveOp
                || approval.requested_by.as_deref() != Some(ctx.identity.credential_id.as_str())
                || approval
                    .facts
                    .as_ref()
                    .and_then(|f| f.get("operation_set_blake3"))
                    != Some(&serde_json::json!(p.plan_hash))
                || approval.facts.as_ref().and_then(|f| f.get("artifact_plan"))
                    != Some(&serde_json::to_value(&p.plan).map_err(|_| internal())?)
            {
                return Err(stale());
            }
            match approval.state {
                ApprovalState::Pending => {
                    return Ok(ArtifactsLifecycleApplyResult {
                        applied: false,
                        approval_id: Some(approval_id),
                        artifact: None,
                        path: None,
                        mode: None,
                        replayed: previous.is_some(),
                    });
                }
                ApprovalState::Approved if approval.expires_at > now => {}
                _ => {
                    return Err(ApiError::new(
                        "policy.approval_required",
                        "this lifecycle approval is no longer usable; obtain a new preview",
                    ));
                }
            }
        }
        self.admit()?;
        self.audit_tool_call(
            ctx.identity.action_actor(),
            "artifacts.lifecycle.apply",
            &p,
            &[
                ("artifact_id", serde_json::json!(p.plan.artifact_id)),
                ("plan_hash", serde_json::json!(p.plan_hash)),
            ],
        )?;
        progress.started = true;
        receipt.entries = vec![serde_json::to_value(&progress).map_err(|_| internal())?];
        self.receipts.save(&key, &receipt)?;
        let store = self.artifacts.clone();
        let plan = p.plan.clone();
        let copying = matches!(
            plan.operation,
            ArtifactLifecycleOperation::Materialize { .. }
        );
        if !copying && self.artifact_copies.occupied()? {
            return Err(ApiError::new(
                "artifact.operation_in_progress",
                "an artifact copy is still executing; wait before changing records",
            ));
        }
        let active_copy = copying
            .then(|| self.artifact_copies.start(&key))
            .transpose()?;
        let journal = self.journal.clone();
        let credentials = self.credentials.clone();
        let credential = ctx.identity.credential_id.clone();
        let service = Self::scope(ctx).service().is_some();
        let permitted_roots = self.visible_roots(ctx);
        let state_dir = self.state_dir.clone();
        // Core materialize has no cancellation/publication hook. Let emergency stop and revocation
        // proceed during an accepted copy, while a separate key guard prevents duplicate execution.
        let _mutation_gate = if copying {
            drop(gate);
            None
        } else {
            Some(gate)
        };
        let effect = tokio::task::spawn_blocking(move || {
            let effect = (|| -> ApiResult<()> {
                if journal.meta().map_err(error)?.kill_switch_engaged() {
                    return Err(ApiError::new(
                        "policy.kill_switch_engaged",
                        "kill switch engaged before artifact effect",
                    ));
                }
                if zeus_api::auth::CredentialStore::get(credentials.as_ref(), &credential).is_none()
                {
                    return Err(ApiError::new(
                        "api.bad_credential",
                        "artifact requester was revoked",
                    ));
                }
                if let ArtifactLifecycleOperation::Materialize { path } = &plan.operation {
                    let roots = if service {
                        credentials.roots(&credential)
                    } else {
                        permitted_roots
                    };
                    let roots = zeus_fs::WriteRoots::open(&roots).map_err(error)?;
                    let resolved = roots.resolve_dest_dir(Path::new(path)).map_err(error)?;
                    if resolved != Path::new(path)
                        || resolved.starts_with(state_dir.canonicalize().map_err(|_| internal())?)
                    {
                        return Err(stale());
                    }
                }
                match plan.operation {
                    ArtifactLifecycleOperation::Delete => {
                        store.delete(plan.artifact_id).map_err(error)
                    }
                    ArtifactLifecycleOperation::SetRetention {
                        retention,
                        retain_until,
                    } => store
                        .set_retention(
                            plan.artifact_id,
                            match retention {
                                ArtifactRetention::Keep => Retention::Keep,
                                ArtifactRetention::Evictable => Retention::Evictable,
                            },
                            retain_until,
                        )
                        .map_err(error),
                    ArtifactLifecycleOperation::Materialize { path } => store
                        .materialize(
                            plan.artifact_id,
                            Path::new(&path),
                            &[zeus_artifact::MaterializeMode::Copy],
                            zeus_types::OsMetadataMode::MarkOnly,
                            false,
                        )
                        .map(|_| ())
                        .map_err(error),
                }
            })();
            // Keep this guard in the blocking task even when a disconnected caller drops its RPC future.
            (effect, active_copy)
        })
        .await;
        let _completion_gate = if copying {
            Some(self.mutations.lock().await)
        } else {
            None
        };
        let (effect, _active_copy) = effect.map_err(|_| internal())?;
        effect?;
        let result = self.lifecycle_receipt(&p.plan, progress.approval_id)?;
        {
            use zeus_verify::audit::{AuditEntry, AuditOutcome, AuditSink};
            let mut event = AuditEntry::new(
                Timestamp::now(),
                ctx.identity.action_actor(),
                "artifact.lifecycle_completed",
                AuditOutcome::Ok,
            );
            event
                .data
                .insert("artifact_id".into(), serde_json::json!(p.plan.artifact_id));
            event
                .data
                .insert("plan_hash".into(), serde_json::json!(p.plan_hash));
            event
                .data
                .insert("blake3".into(), serde_json::json!(p.plan.blake3));
            self.audit.append(event).map_err(error)?;
        }
        self.store_receipt(Some((key, receipt)), &result)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{context, setup};
    use super::*;
    use zeus_api::methods::sessions::{
        ApprovalView, ApprovalsDecideParams, ConfirmationLevel, DecideOutcome,
    };

    #[tokio::test]
    async fn a_disconnected_copy_keeps_its_execution_guard_until_the_blocking_effect_finishes() {
        let copies = ActiveCopies::default();
        let guard = copies.start("copy").unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let task = tokio::task::spawn_blocking(move || {
            let _ = started_tx.send(());
            finish_rx.recv().unwrap();
            drop(guard);
            let _ = done_tx.send(());
        });
        started_rx.await.unwrap();
        drop(task); // dropping the RPC's join handle does not cancel a running blocking copy
        assert!(copies.contains("copy").unwrap());
        assert!(copies.start("other-copy").is_err());
        finish_tx.send(()).unwrap();
        done_rx.await.unwrap();
        assert!(!copies.occupied().unwrap());
        assert!(copies.start("other-copy").is_ok());
    }

    async fn ingest(
        backend: &RuntimeBackend,
        ctx: &CallContext,
        root: &Path,
        name: &str,
    ) -> zeus_types::ArtifactId {
        let path = root.join("downloads").join(name);
        std::fs::write(&path, b"shared verified content").unwrap();
        let result = backend
            .artifacts_ingest(
                ctx,
                ArtifactsIngestParams {
                    path: path.display().to_string(),
                    name: None,
                    media_type: None,
                    parents: Vec::new(),
                    operation: None,
                    metadata: None,
                    idempotency_key: Some(name.to_owned()),
                },
            )
            .await
            .unwrap();
        serde_json::from_value(result.artifact["id"].clone()).unwrap()
    }

    fn request(
        backend: &RuntimeBackend,
        ctx: &CallContext,
        id: zeus_types::ArtifactId,
        operation: ArtifactLifecycleOperation,
        key: &str,
    ) -> ArtifactsLifecycleApplyParams {
        let preview = backend
            .artifact_lifecycle_preview(
                ctx,
                ArtifactsLifecyclePreviewParams {
                    artifact_id: id,
                    operation,
                },
            )
            .unwrap();
        ArtifactsLifecycleApplyParams {
            plan: preview.plan,
            plan_hash: preview.plan_hash,
            idempotency_key: key.into(),
        }
    }

    async fn approve(backend: &RuntimeBackend, human: &CallContext, id: ApprovalId) {
        let view =
            ApprovalView::try_from(backend.consent.get(&human.identity, id).unwrap()).unwrap();
        zeus_types::spec_testing::SpecSchemas::load()
            .check(
                true,
                "approvals.v1",
                "ApprovalRequest",
                &serde_json::to_value(&view).unwrap(),
            )
            .unwrap();
        let p = ApprovalsDecideParams {
            approval_id: id,
            request_hash: view.request_hash,
            outcome: DecideOutcome::Approve,
            scope: None,
            channel: Surface::CliInteractive,
            confirmation: ConfirmationLevel::TypedCode,
            idempotency_key: None,
        };
        let evidence = serde_json::json!({"client_id":human.identity.credential_id.as_str(),"surface":"cli_interactive",
            "exe_verified":true,"credential_kind":human.identity.kind,"effective_kind":human.identity.kind,
            "role":human.identity.role,"tty":"verified","confirmation":"typed_code","peer_pid":1});
        backend
            .consent
            .decide(&human.identity, &p, evidence)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn retention_strengthens_directly_but_weakening_waits_for_exact_human_approval() {
        let dir = tempfile::tempdir().unwrap();
        let backend = setup(dir.path()).await;
        let (agent, _rx) = context(dir.path(), "agent");
        let (human, _hrx) = context(dir.path(), "cli");
        let id = ingest(&backend, &agent, dir.path(), "protection.txt").await;
        let keep = request(
            &backend,
            &agent,
            id,
            ArtifactLifecycleOperation::SetRetention {
                retention: ArtifactRetention::Keep,
                retain_until: None,
            },
            "keep",
        );
        assert!(!keep.plan.requires_approval);
        assert!(
            backend
                .artifact_lifecycle_apply(&agent, keep.clone())
                .await
                .unwrap()
                .applied
        );
        assert!(
            backend
                .artifact_lifecycle_apply(&agent, keep)
                .await
                .unwrap()
                .replayed
        );
        let evict = request(
            &backend,
            &agent,
            id,
            ArtifactLifecycleOperation::SetRetention {
                retention: ArtifactRetention::Evictable,
                retain_until: None,
            },
            "evict",
        );
        assert!(evict.plan.requires_approval);
        let pending = backend
            .artifact_lifecycle_apply(&agent, evict.clone())
            .await
            .unwrap();
        assert!(!pending.applied && !pending.replayed);
        assert_eq!(
            backend.artifacts.get(id).unwrap().retention,
            Retention::Keep
        );
        let repeat = backend
            .artifact_lifecycle_apply(&agent, evict.clone())
            .await
            .unwrap();
        assert!(repeat.replayed && !repeat.applied);
        assert_eq!(repeat.approval_id, pending.approval_id);
        approve(&backend, &human, pending.approval_id.unwrap()).await;
        assert!(
            backend
                .artifact_lifecycle_apply(&agent, evict)
                .await
                .unwrap()
                .applied
        );
        assert_eq!(
            backend.artifacts.get(id).unwrap().retention,
            Retention::Evictable
        );
        backend.engine.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn tombstone_preserves_shared_content_and_user_files_and_rejects_forged_approval_flag() {
        let dir = tempfile::tempdir().unwrap();
        let backend = setup(dir.path()).await;
        let (agent, _rx) = context(dir.path(), "agent");
        let (human, _hrx) = context(dir.path(), "cli");
        let id = ingest(&backend, &agent, dir.path(), "first.txt").await;
        let other = ingest(&backend, &agent, dir.path(), "second.txt").await;
        let delete = request(
            &backend,
            &agent,
            id,
            ArtifactLifecycleOperation::Delete,
            "delete",
        );
        let mut forged = delete.clone();
        forged.idempotency_key = "forged-delete".into();
        forged.plan.requires_approval = false;
        forged.plan_hash = plan_hash(&forged.plan).unwrap();
        assert_eq!(
            backend
                .artifact_lifecycle_apply(&agent, forged)
                .await
                .unwrap_err()
                .envelope()
                .code
                .as_str(),
            "artifact.plan_stale"
        );
        let pending = backend
            .artifact_lifecycle_apply(&agent, delete.clone())
            .await
            .unwrap();
        assert_eq!(
            backend.artifacts.get(id).unwrap().state,
            ArtifactState::Active
        );
        approve(&backend, &human, pending.approval_id.unwrap()).await;
        let done = backend
            .artifact_lifecycle_apply(&agent, delete.clone())
            .await
            .unwrap();
        assert!(done.applied);
        assert_eq!(done.artifact.unwrap()["state"], "deleted");
        assert_eq!(
            backend.artifacts.get(other).unwrap().object.state,
            ObjectState::Present
        );
        assert!(backend.artifacts.object_path(delete.plan.blake3).is_file());
        for name in ["first.txt", "second.txt"] {
            assert_eq!(
                std::fs::read(dir.path().join("downloads").join(name)).unwrap(),
                b"shared verified content"
            );
        }
        assert!(
            backend
                .artifact_lifecycle_apply(&agent, delete)
                .await
                .unwrap()
                .replayed
        );
        backend.engine.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn copy_materialization_is_verified_confined_non_overwriting_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let backend = setup(dir.path()).await;
        let (agent, _rx) = context(dir.path(), "agent");
        let id = ingest(&backend, &agent, dir.path(), "source.txt").await;
        assert!(
            backend
                .artifact_lifecycle_preview(
                    &agent,
                    ArtifactsLifecyclePreviewParams {
                        artifact_id: id,
                        operation: ArtifactLifecycleOperation::Materialize {
                            path: dir.path().join("outside.txt").display().to_string()
                        },
                    }
                )
                .is_err()
        );
        let dest = dir.path().join("downloads").join("copy.txt");
        let copy = request(
            &backend,
            &agent,
            id,
            ArtifactLifecycleOperation::Materialize {
                path: dest.display().to_string(),
            },
            "copy",
        );
        let done = backend
            .artifact_lifecycle_apply(&agent, copy.clone())
            .await
            .unwrap();
        assert_eq!(done.mode.as_deref(), Some("copy"));
        assert_eq!(std::fs::read(&dest).unwrap(), b"shared verified content");
        std::fs::write(&dest, b"user edit").unwrap();
        assert!(
            backend
                .artifact_lifecycle_apply(&agent, copy)
                .await
                .unwrap()
                .replayed
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"user edit");
        assert!(
            backend
                .artifact_lifecycle_preview(
                    &agent,
                    ArtifactsLifecyclePreviewParams {
                        artifact_id: id,
                        operation: ArtifactLifecycleOperation::Materialize {
                            path: dest.display().to_string()
                        },
                    }
                )
                .is_err()
        );
        backend.engine.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn changed_records_and_destination_collisions_cannot_use_an_earlier_preview() {
        let dir = tempfile::tempdir().unwrap();
        let backend = setup(dir.path()).await;
        let (agent, _rx) = context(dir.path(), "agent");
        let id = ingest(&backend, &agent, dir.path(), "stale.txt").await;
        let stale_plan = request(
            &backend,
            &agent,
            id,
            ArtifactLifecycleOperation::Delete,
            "stale",
        );
        backend
            .artifacts
            .set_retention(id, Retention::Keep, None)
            .unwrap();
        assert_eq!(
            backend
                .artifact_lifecycle_apply(&agent, stale_plan)
                .await
                .unwrap_err()
                .envelope()
                .code
                .as_str(),
            "artifact.plan_stale"
        );
        let dest = dir.path().join("downloads").join("collision.txt");
        let copy = request(
            &backend,
            &agent,
            id,
            ArtifactLifecycleOperation::Materialize {
                path: dest.display().to_string(),
            },
            "collision",
        );
        std::fs::write(&dest, b"preserved").unwrap();
        assert!(
            backend
                .artifact_lifecycle_apply(&agent, copy)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(dest).unwrap(), b"preserved");
        backend.engine.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn started_materialization_after_restart_does_not_adopt_a_foreign_destination() {
        let dir = tempfile::tempdir().unwrap();
        let backend = setup(dir.path()).await;
        let (agent, _rx) = context(dir.path(), "agent");
        let id = ingest(&backend, &agent, dir.path(), "interrupted.txt").await;
        let dest = dir.path().join("downloads").join("uncertain.txt");
        let p = request(
            &backend,
            &agent,
            id,
            ArtifactLifecycleOperation::Materialize {
                path: dest.display().to_string(),
            },
            "interrupted",
        );
        let key = crate::mutations::MutationStore::binding(
            agent.identity.credential_id.as_str(),
            "artifacts.lifecycle.apply",
            &p.idempotency_key,
        )
        .unwrap();
        let mut receipt = backend
            .receipts
            .begin(&key, serde_json::to_value(&p).unwrap())
            .unwrap();
        receipt.entries.push(
            serde_json::to_value(Progress {
                before: backend.export_value(id).unwrap(),
                approval_id: None,
                started: true,
            })
            .unwrap(),
        );
        backend.receipts.save(&key, &receipt).unwrap();
        std::fs::write(&dest, b"foreign destination").unwrap();
        backend.engine.shutdown().await.unwrap();
        drop(backend);
        let recovered = setup(dir.path()).await;
        assert_eq!(
            recovered
                .artifact_lifecycle_apply(&agent, p)
                .await
                .unwrap_err()
                .envelope()
                .code
                .as_str(),
            "artifact.lifecycle_reconciliation_required"
        );
        assert_eq!(std::fs::read(dest).unwrap(), b"foreign destination");
        recovered.engine.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn expired_preview_and_key_rebinding_are_refused_before_an_effect() {
        let dir = tempfile::tempdir().unwrap();
        let backend = setup(dir.path()).await;
        let (agent, _rx) = context(dir.path(), "agent");
        let id = ingest(&backend, &agent, dir.path(), "expired.txt").await;
        let mut expired = request(
            &backend,
            &agent,
            id,
            ArtifactLifecycleOperation::Delete,
            "expired",
        );
        expired.plan.expires_at = Timestamp::UNIX_EPOCH;
        expired.plan_hash = plan_hash(&expired.plan).unwrap();
        assert_eq!(
            backend
                .artifact_lifecycle_apply(&agent, expired)
                .await
                .unwrap_err()
                .envelope()
                .code
                .as_str(),
            "artifact.plan_expired"
        );
        let p = request(
            &backend,
            &agent,
            id,
            ArtifactLifecycleOperation::SetRetention {
                retention: ArtifactRetention::Keep,
                retain_until: None,
            },
            "bound",
        );
        backend
            .artifact_lifecycle_apply(&agent, p.clone())
            .await
            .unwrap();
        let mut changed = p;
        changed.plan.operation = ArtifactLifecycleOperation::Delete;
        changed.plan_hash = plan_hash(&changed.plan).unwrap();
        assert_eq!(
            backend
                .artifact_lifecycle_apply(&agent, changed)
                .await
                .unwrap_err()
                .envelope()
                .code
                .as_str(),
            "api.idempotency_conflict"
        );
        assert_eq!(
            backend.artifacts.get(id).unwrap().state,
            ArtifactState::Active
        );
        backend.engine.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn tombstone_started_before_restart_recovers_its_exact_completed_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let backend = setup(dir.path()).await;
        let (agent, _rx) = context(dir.path(), "agent");
        let (human, _hrx) = context(dir.path(), "cli");
        let id = ingest(&backend, &agent, dir.path(), "recover-delete.txt").await;
        let p = request(
            &backend,
            &agent,
            id,
            ArtifactLifecycleOperation::Delete,
            "recover-delete",
        );
        let pending = backend
            .artifact_lifecycle_apply(&agent, p.clone())
            .await
            .unwrap();
        let approval = pending.approval_id.unwrap();
        approve(&backend, &human, approval).await;
        let key = crate::mutations::MutationStore::binding(
            agent.identity.credential_id.as_str(),
            "artifacts.lifecycle.apply",
            &p.idempotency_key,
        )
        .unwrap();
        let mut receipt = backend
            .receipts
            .begin(&key, serde_json::to_value(&p).unwrap())
            .unwrap();
        receipt.entries = vec![
            serde_json::to_value(Progress {
                before: backend.export_value(id).unwrap(),
                approval_id: Some(approval),
                started: true,
            })
            .unwrap(),
        ];
        backend.receipts.save(&key, &receipt).unwrap();
        backend.artifacts.delete(id).unwrap();
        backend.engine.shutdown().await.unwrap();
        drop(backend);
        let recovered = setup(dir.path()).await;
        let done = recovered.artifact_lifecycle_apply(&agent, p).await.unwrap();
        assert!(done.applied && done.replayed);
        assert_eq!(done.artifact.unwrap()["state"], "deleted");
        assert_eq!(done.approval_id, Some(approval));
        assert!(
            dir.path()
                .join("downloads")
                .join("recover-delete.txt")
                .is_file()
        );
        recovered.engine.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn lifecycle_visibility_and_quarantine_cannot_be_widened_by_a_request() {
        let dir = tempfile::tempdir().unwrap();
        let backend = setup(dir.path()).await;
        let (agent, _rx) = context(dir.path(), "agent");
        let id = ingest(&backend, &agent, dir.path(), "private.txt").await;
        let (mut service, _srx) = context(dir.path(), "a2a");
        service.identity.kind = zeus_api::ClientKind::Service;
        service.identity.role = zeus_api::ClientRole::Service;
        let p = request(
            &backend,
            &agent,
            id,
            ArtifactLifecycleOperation::Delete,
            "foreign",
        );
        assert_eq!(
            backend
                .artifact_lifecycle_preview(
                    &service,
                    ArtifactsLifecyclePreviewParams {
                        artifact_id: id,
                        operation: ArtifactLifecycleOperation::Delete
                    }
                )
                .unwrap_err()
                .envelope()
                .code
                .as_str(),
            "artifact.not_found"
        );
        assert_eq!(
            backend
                .artifact_lifecycle_apply(&service, p)
                .await
                .unwrap_err()
                .envelope()
                .code
                .as_str(),
            "artifact.not_found"
        );
        let source = dir.path().join("downloads").join("quarantined.bin");
        std::fs::write(&source, b"quarantined bytes").unwrap();
        let mut draft = zeus_artifact::ArtifactDraft::local(
            "zeus",
            env!("CARGO_PKG_VERSION"),
            Some("quarantined.bin".into()),
        );
        draft.record.quarantined = true;
        draft.record.quarantine_reasons = vec![zeus_types::QuarantineReason::ExecutableType];
        let record = backend
            .artifacts
            .ingest(&source, draft, Some("quarantined"))
            .unwrap();
        let blocked = backend
            .artifact_lifecycle_preview(
                &agent,
                ArtifactsLifecyclePreviewParams {
                    artifact_id: record.id,
                    operation: ArtifactLifecycleOperation::Materialize {
                        path: dir
                            .path()
                            .join("downloads")
                            .join("escape.bin")
                            .display()
                            .to_string(),
                    },
                },
            )
            .unwrap_err();
        assert_eq!(
            blocked.envelope().code.as_str(),
            "policy.quarantine_blocked"
        );
        let keep = request(
            &backend,
            &agent,
            record.id,
            ArtifactLifecycleOperation::SetRetention {
                retention: ArtifactRetention::Keep,
                retain_until: None,
            },
            "keep-quarantined",
        );
        backend
            .artifact_lifecycle_apply(&agent, keep)
            .await
            .unwrap();
        assert!(backend.artifacts.get(record.id).unwrap().quarantined);
        assert!(!dir.path().join("downloads").join("escape.bin").exists());
        backend.engine.shutdown().await.unwrap();
    }
}
