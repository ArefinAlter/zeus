//! Bounded retained-handle bundle I/O, exact collection approvals, and durable audit replay.
use super::*;
use serde::{Deserialize, Serialize};
use std::{ffi::OsString, io::Read};
use zeus_artifact::{ArtifactBundle, CollectionPlan, Privacy};
use zeus_fs::{DirectoryCap, directory::opened_file_metadata};
use zeus_types::{ApprovalId, ApprovalKind, Hex256};
use zeus_verify::provenance::UrlQueryPrivacy;
const TTL: u64 = 15 * 60 * 1000;
fn stale() -> ApiError {
    ApiError::new(
        "artifact.plan_stale",
        "artifact facts or retained filesystem identities changed; preview again",
    )
}
fn hash(plan: &ArtifactMaintenancePlan) -> ApiResult<Hex256> {
    crate::approvals::facts_hash(
        &serde_json::json!({"t":"zeus.artifact.maintenance.v1","plan":plan}),
    )
}
fn privacy() -> Privacy {
    Privacy {
        url_query: UrlQueryPrivacy::Dropped,
        session_origin_included: false,
    }
}
#[derive(Default)]
struct Prepared {
    bundle: Option<ArtifactBundle>,
    repair: Option<Vec<u8>>,
}
struct HeldFile {
    binding: ArtifactFileBinding,
    parent: DirectoryCap,
    name: OsString,
    input: Option<std::fs::File>,
}
#[derive(Clone, Serialize, Deserialize, Default)]
struct Progress {
    approval_id: Option<ApprovalId>,
    started: bool,
    #[serde(default)]
    last_result: Option<ArtifactsMaintenanceApplyResult>,
}
impl RuntimeBackend {
    fn maintenance_file(&self, ctx: &CallContext, path: &str, source: bool) -> ApiResult<HeldFile> {
        let outside = || {
            ApiError::new(
                zeus_types::codes::POLICY_PATH_OUTSIDE_ROOTS,
                "artifact bundle needs an existing plain parent inside an authorized root, outside Zeus state",
            )
        };
        let path = Path::new(path);
        if !path.is_absolute()
            || path.as_os_str().len() > 4096
            || path.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
            || path.components().any(|c| c.as_os_str() == ".zeus")
        {
            return Err(outside());
        }
        let state = self.state_dir.canonicalize().map_err(|_| internal())?;
        let parent_path = path.parent().ok_or_else(outside)?;
        let name = path.file_name().ok_or_else(outside)?.to_owned();
        zeus_fs::directory::validate_filename(&name).map_err(error)?;
        for configured in self.visible_roots(ctx) {
            let root_path = configured.canonicalize().map_err(|_| outside())?;
            // Windows canonicalization adds a verbatim prefix. Preserve lexical descendants, then
            // resolve them through retained handles; do not canonicalize through an untrusted child link.
            let Ok(relative) = parent_path
                .strip_prefix(&configured)
                .or_else(|_| parent_path.strip_prefix(&root_path))
            else {
                continue;
            };
            let resolved = root_path.join(relative).join(&name);
            if root_path.starts_with(&state) || resolved.starts_with(&state) {
                continue;
            }
            let root = DirectoryCap::open_root(&root_path).map_err(error)?;
            let parent = root.open_dir(relative).map_err(error)?;
            let input = if source {
                Some(parent.open_read(&name).map_err(error)?)
            } else {
                if parent.entry_exists(&name).map_err(error)? {
                    return Err(error(zeus_fs::FsError::AlreadyExists));
                }
                None
            };
            let file_id = input
                .as_ref()
                .map(opened_file_metadata)
                .transpose()
                .map_err(error)?
                .and_then(|m| m.file_id);
            return Ok(HeldFile {
                binding: ArtifactFileBinding {
                    path: resolved.display().to_string(),
                    root: root_path.display().to_string(),
                    root_id: root.file_id().map_err(error)?,
                    parent_id: parent.file_id().map_err(error)?,
                    file_id,
                },
                parent,
                name,
                input,
            });
        }
        Err(outside())
    }
    fn maintenance_owner(&self, ctx: &CallContext) -> ApiResult<()> {
        if Self::scope(ctx).service().is_some() {
            return Err(ApiError::permission_denied(
                "store-wide collection and bundle import require the local owner; services may export their own records",
            ));
        }
        Ok(())
    }
    fn maintenance_plan(
        &self,
        ctx: &CallContext,
        operation: ArtifactMaintenanceOperation,
        expires_at: Timestamp,
        resume_repair: bool,
    ) -> ApiResult<(ArtifactMaintenancePlan, Option<HeldFile>, Prepared)> {
        let root = DirectoryCap::open_root(self.artifacts.root()).map_err(error)?;
        let mut held = None;
        let mut bundle = Prepared::default();
        let (details, requires_approval) = match &operation {
            ArtifactMaintenanceOperation::Export {
                artifact_ids,
                path,
                include_objects,
            } => {
                if artifact_ids.is_empty() || artifact_ids.len() > 64 {
                    return Err(ApiError::invalid_params("export requires1–64 records"));
                }
                for id in artifact_ids {
                    self.visible_artifact(ctx, *id)?;
                }
                held = Some(self.maintenance_file(ctx, path, false)?);
                let value = self
                    .artifacts
                    .export_bundle(artifact_ids, *include_objects, &privacy())
                    .map_err(error)?;
                let details = serde_json::to_value(value.summary()).map_err(|_| internal())?;
                bundle.bundle = Some(value);
                (details, false)
            }
            ArtifactMaintenanceOperation::Import { path } => {
                self.maintenance_owner(ctx)?;
                let mut file = self.maintenance_file(ctx, path, true)?;
                let input = file.input.as_mut().ok_or_else(internal)?;
                let before = opened_file_metadata(input).map_err(error)?;
                let value = ArtifactBundle::read(&mut *input).map_err(error)?;
                let after = opened_file_metadata(input).map_err(error)?;
                if before.file_id != after.file_id
                    || before.len != after.len
                    || before.mtime_ms != after.mtime_ms
                {
                    return Err(stale());
                }
                let details = serde_json::to_value(value.summary()).map_err(|_| internal())?;
                held = Some(file);
                bundle.bundle = Some(value);
                (details, false)
            }
            ArtifactMaintenanceOperation::Scrub { artifact_ids } => {
                self.maintenance_owner(ctx)?;
                if artifact_ids.is_empty() || artifact_ids.len() > 64 {
                    return Err(ApiError::invalid_params(
                        "integrity check needs1–64 records",
                    ));
                }
                for id in artifact_ids {
                    self.visible_artifact(ctx, *id)?;
                }
                let plan = self
                    .artifacts
                    .integrity_preview(artifact_ids)
                    .map_err(error)?;
                (serde_json::to_value(plan).map_err(|_| internal())?, true)
            }
            ArtifactMaintenanceOperation::Repair { artifact_id, path } => {
                self.maintenance_owner(ctx)?;
                let record = self.visible_artifact(ctx, *artifact_id)?;
                if record.state != zeus_artifact::ArtifactState::Active
                    || (!resume_repair
                        && record.object.state == zeus_artifact::ObjectState::Present)
                {
                    return Err(ApiError::invalid_params(
                        "repair needs an active absent or damaged object",
                    ));
                }
                if record.size_bytes > 16 * 1024 * 1024 {
                    return Err(ApiError::invalid_params("repair source exceeds16MiB"));
                }
                let mut file = self.maintenance_file(ctx, path, true)?;
                let input = file.input.as_mut().ok_or_else(internal)?;
                let before = opened_file_metadata(input).map_err(error)?;
                let mut content = Vec::new();
                input
                    .take(16 * 1024 * 1024 + 1)
                    .read_to_end(&mut content)
                    .map_err(|_| internal())?;
                if zeus_verify::hash::hash_bytes(&content) != record.digests() {
                    return Err(error(zeus_artifact::ArtifactError::DigestMismatch));
                }
                let after = opened_file_metadata(file.input.as_ref().ok_or_else(internal)?)
                    .map_err(error)?;
                if before.file_id != after.file_id
                    || before.len != after.len
                    || before.mtime_ms != after.mtime_ms
                {
                    return Err(stale());
                }
                let exported = self.export_value(*artifact_id)?;
                let details = serde_json::json!({"artifact_id":artifact_id,"record_revision":crate::approvals::facts_hash(&exported)?,"object_state":record.object.state,"verification":exported["verification"],"blake3":record.blake3,"sha256":record.sha256,"size_bytes":record.size_bytes});
                held = Some(file);
                bundle.repair = Some(content);
                (details, true)
            }
            ArtifactMaintenanceOperation::Collect {
                quota_pressure,
                limit,
            } => {
                self.maintenance_owner(ctx)?;
                let collection = self
                    .artifacts
                    .collection_preview(*quota_pressure, *limit as usize)
                    .map_err(error)?;
                (
                    serde_json::to_value(collection).map_err(|_| internal())?,
                    true,
                )
            }
        };
        Ok((
            ArtifactMaintenancePlan {
                version: 1,
                store_id: self.artifacts.store_id().map_err(error)?,
                store_root_id: root.file_id().map_err(error)?,
                operation,
                file: held.as_ref().map(|h| h.binding.clone()),
                details,
                requires_approval,
                expires_at,
            },
            held,
            bundle,
        ))
    }
    pub(super) async fn artifact_maintenance_preview(
        &self,
        ctx: &CallContext,
        p: ArtifactsMaintenancePreviewParams,
    ) -> ApiResult<ArtifactsMaintenancePreviewResult> {
        let (plan, _, _) = self.maintenance_plan(
            ctx,
            p.operation,
            Timestamp::now().saturating_add_ms(TTL),
            false,
        )?;
        if plan.requires_approval
            && serde_json::to_vec(&plan).map_err(|_| internal())?.len() > 12 * 1024
        {
            return Err(ApiError::invalid_params(
                "exact approval plan exceeds12KiB; select fewer objects or shorter paths",
            ));
        }
        Ok(ArtifactsMaintenancePreviewResult {
            plan_hash: hash(&plan)?,
            plan,
        })
    }
    pub(super) async fn artifact_maintenance_apply(
        &self,
        ctx: &CallContext,
        p: ArtifactsMaintenanceApplyParams,
    ) -> ApiResult<ArtifactsMaintenanceApplyResult> {
        if p.idempotency_key.is_empty()
            || p.idempotency_key.len() > 128
            || p.idempotency_key.chars().any(char::is_control)
        {
            return Err(ApiError::invalid_params(
                "idempotency_key must be1–128 printable bytes",
            ));
        }
        if p.plan.version != 1 || hash(&p.plan)? != p.plan_hash {
            return Err(stale());
        }
        if p.plan.requires_approval
            && serde_json::to_vec(&p.plan).map_err(|_| internal())?.len() > 12 * 1024
        {
            return Err(ApiError::invalid_params("exact approval plan exceeds12KiB"));
        }
        self.admit()?;
        let gate = self.mutations.lock().await;
        self.admit()?;
        let (binding, replay) = self.receipt::<ArtifactsMaintenanceApplyResult>(
            ctx,
            "artifacts.maintenance.apply",
            Some(&p.idempotency_key),
            &p,
        )?;
        if let Some(mut result) = replay {
            result.replayed = true;
            return Ok(result);
        }
        let (key, mut receipt) = binding.ok_or_else(internal)?;
        if self.artifact_copies.occupied()? {
            return Err(ApiError::new(
                "artifact.operation_in_progress",
                "wait for the active artifact operation receipt",
            ));
        }
        let mut progress: Progress = receipt
            .entries
            .first()
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| internal())?
            .unwrap_or_default();
        let replayed = progress.started || progress.approval_id.is_some();
        let now = Timestamp::now();
        if p.plan.expires_at <= now || p.plan.expires_at > now.saturating_add_ms(TTL) {
            return Err(ApiError::new(
                "artifact.plan_expired",
                "preview the operation again",
            ));
        }
        let is_collection = matches!(
            p.plan.operation,
            ArtifactMaintenanceOperation::Collect { .. }
        );
        let (mut held, bundle) = if is_collection && progress.started {
            // Recovery may contain already-removed exact targets. Core rechecks every remaining reference and identity.
            self.maintenance_owner(ctx)?;
            if !p.plan.requires_approval
                || p.plan.file.is_some()
                || self.artifacts.store_id().map_err(error)? != p.plan.store_id
                || DirectoryCap::open_root(self.artifacts.root())
                    .map_err(error)?
                    .file_id()
                    .map_err(error)?
                    != p.plan.store_root_id
            {
                return Err(stale());
            }
            (None, Prepared::default())
        } else {
            let(mut current,held,bundle)=self.maintenance_plan(ctx,p.plan.operation.clone(),p.plan.expires_at,progress.started).map_err(|e|{
                if progress.started&&matches!(p.plan.operation,ArtifactMaintenanceOperation::Export{..})&&e.envelope().code.as_str()=="fs.already_exists"{ApiError::new("artifact.maintenance_reconciliation_required","an interrupted export has an existing target; inspect it and preview another destination")}else{e}
            })?;
            if progress.started
                && let ArtifactMaintenanceOperation::Repair { artifact_id, .. } = &p.plan.operation
            {
                let mut exported = self.export_value(*artifact_id)?;
                if exported["object"]["state"] == "present" {
                    exported["object"]["state"] = p.plan.details["object_state"].clone();
                    exported["verification"] = p.plan.details["verification"].clone();
                    if serde_json::json!(crate::approvals::facts_hash(&exported)?)
                        != p.plan.details["record_revision"]
                    {
                        return Err(stale());
                    }
                    for field in ["record_revision", "object_state", "verification"] {
                        current.details[field] = p.plan.details[field].clone();
                    }
                }
            }
            if current != p.plan {
                return Err(stale());
            }
            (held, bundle)
        };
        if p.plan.requires_approval {
            let id = match progress.approval_id {
                Some(id) => id,
                None => {
                    let id = self
                        .consent
                        .request_artifact_maintenance(&ctx.identity, &p.plan, p.plan_hash)
                        .await?;
                    progress.approval_id = Some(id);
                    receipt.entries =
                        vec![serde_json::to_value(&progress).map_err(|_| internal())?];
                    self.receipts.save(&key, &receipt)?;
                    id
                }
            };
            let approval = self.consent.get(&ctx.identity, id)?;
            if approval.kind != ApprovalKind::DestructiveOp
                || approval.requested_by.as_deref() != Some(ctx.identity.credential_id.as_str())
                || approval
                    .facts
                    .as_ref()
                    .and_then(|v| v.get("maintenance_plan"))
                    != Some(&serde_json::json!(p.plan))
                || approval
                    .facts
                    .as_ref()
                    .and_then(|v| v.get("operation_set_blake3"))
                    != Some(&serde_json::json!(p.plan_hash))
            {
                return Err(stale());
            }
            match approval.state {
                ApprovalState::Pending => {
                    return Ok(ArtifactsMaintenanceApplyResult {
                        applied: false,
                        verified_objects: vec![],
                        damaged_objects: vec![],
                        status: "awaiting_approval".into(),
                        error_code: None,
                        absent_objects: None,
                        remaining_objects: None,
                        reconciliation_required: false,
                        approval_id: Some(id),
                        artifact_ids: vec![],
                        size_bytes: 0,
                        replayed,
                    });
                }
                ApprovalState::Approved if approval.expires_at > now => (),
                _ => {
                    return Err(ApiError::new(
                        "policy.approval_required",
                        "this collection approval is no longer usable",
                    ));
                }
            }
        }
        self.admit()?;
        self.audit_tool_call(
            ctx.identity.action_actor(),
            "artifacts.maintenance.apply",
            &p,
            &[("plan_hash", serde_json::json!(p.plan_hash))],
        )?;
        let resuming = progress.started;
        progress.started = true;
        receipt.entries = vec![serde_json::to_value(&progress).map_err(|_| internal())?];
        self.receipts.save(&key, &receipt)?;
        let active = Arc::new(self.artifact_copies.start(&key)?);
        let executing = active.clone();
        let store = self.artifacts.clone();
        let plan = p.plan.clone();
        // Accepted bounded core effects can finish during emergency stop; no later effect is authorized by this receipt.
        let credentials = self.credentials.clone();
        let credential = ctx.identity.credential_id.clone();
        let journal = self.journal.clone();
        let service = Self::scope(ctx).service().is_some();
        drop(gate);
        let effect = tokio::task::spawn_blocking(
            move || -> ApiResult<(Vec<zeus_types::ArtifactId>, u64, Option<zeus_artifact::IntegrityResult>)> {
                let _active = executing;
                if journal.meta().map_err(error)?.kill_switch_engaged()
                    || zeus_api::auth::CredentialStore::get(credentials.as_ref(), &credential)
                        .is_none()
                {
                    return Err(ApiError::permission_denied(
                        "operation authority was revoked",
                    ));
                }
                if service && let Some(file) = &held {
                    let root = Path::new(&file.binding.root);
                    if !credentials
                        .roots(&credential)
                        .iter()
                        .any(|p| p.canonicalize().is_ok_and(|p| p == root))
                    {
                        return Err(ApiError::permission_denied("service root was revoked"));
                    }
                }
                match plan.operation {
                    ArtifactMaintenanceOperation::Export { .. } => {
                        let target = held.take().ok_or_else(internal)?;
                        let data = bundle.bundle.ok_or_else(internal)?;
                        let temp = OsString::from(format!(
                            "artifact-{}.zpart",
                            zeus_types::IdGenerator::new().next_ulid()
                        ));
                        let mut output = target.parent.create_new(&temp).map_err(error)?;
                        if let Err(e) = data.write(&mut output) {
                            let _ = target.parent.discard_created(&temp, output);
                            return Err(error(e));
                        }
                        output.sync_all().map_err(|_| internal())?;
                        let identity = opened_file_metadata(&output)
                            .map_err(error)?
                            .file_id
                            .ok_or_else(internal)?;
                        drop(output);
                        let source = target.parent.prepare_rename(&temp).map_err(error)?;
                        if source.metadata().map_err(error)?.file_id.as_ref() != Some(&identity) {
                            return Err(stale());
                        }
                        source
                            .rename_noreplace(&target.parent, &target.name)
                            .map_err(error)?;
                        target.parent.sync().map_err(error)?;
                        Ok((vec![], data.summary().size_bytes, None))
                    }
                    ArtifactMaintenanceOperation::Import { .. } => {
                        let data = bundle.bundle.ok_or_else(internal)?;
                        let records = store.import_bundle(&data).map_err(error)?;
                        Ok((
                            records.into_iter().map(|r| r.id).collect(),
                            data.summary().size_bytes, None,
                        ))
                    }
                    ArtifactMaintenanceOperation::Scrub{artifact_ids}=>{
                        let integrity:zeus_artifact::IntegrityPlan=serde_json::from_value(plan.details).map_err(|_|stale())?;
                        let result=store.integrity_apply(&integrity).map_err(error)?;
                        Ok((artifact_ids,integrity.objects.iter().map(|o|o.observed_size).sum(),Some(result)))
                    }
                    ArtifactMaintenanceOperation::Repair{artifact_id,..}=>{
                        let content=bundle.repair.ok_or_else(internal)?;store.repair_from_bytes(artifact_id,&content).map_err(error)?;Ok((vec![artifact_id],content.len() as u64,None))
                    }
                    ArtifactMaintenanceOperation::Collect { .. } => {
                        let collection: CollectionPlan =
                            serde_json::from_value(plan.details).map_err(|_| stale())?;
                        store
                            .collection_apply(&collection, resuming)
                            .map_err(error)?;
                        Ok((
                            vec![],
                            collection.objects.iter().map(|o| o.size_bytes).sum(), None,
                        ))
                    }
                }
            },
        )
        .await
        .map_err(|_| internal()).and_then(|result|result);
        let effect = match effect {
            Ok(value) => value,
            Err(failure) => {
                let code = failure.envelope().code.as_str().to_owned();
                let mut result = ArtifactsMaintenanceApplyResult {
                    applied: false,
                    verified_objects: vec![],
                    damaged_objects: vec![],
                    status: if code == "api.permission_denied" {
                        "cancelled_before_start"
                    } else {
                        "interrupted"
                    }
                    .into(),
                    error_code: Some(code),
                    approval_id: progress.approval_id,
                    artifact_ids: vec![],
                    size_bytes: 0,
                    replayed,
                    absent_objects: None,
                    remaining_objects: None,
                    reconciliation_required: matches!(
                        p.plan.operation,
                        ArtifactMaintenanceOperation::Export { .. }
                            | ArtifactMaintenanceOperation::Scrub { .. }
                    ),
                };
                match &p.plan.operation {
                    ArtifactMaintenanceOperation::Import { .. } => {
                        if let Ok(summary) = serde_json::from_value::<zeus_artifact::BundleSummary>(
                            p.plan.details.clone(),
                        ) {
                            result.artifact_ids = self
                                .artifacts
                                .bundle_import_progress(&summary)
                                .unwrap_or_default();
                        }
                    }
                    ArtifactMaintenanceOperation::Collect { .. } => {
                        if let Ok(plan) =
                            serde_json::from_value::<CollectionPlan>(p.plan.details.clone())
                            && let Ok((absent, remaining)) =
                                self.artifacts.collection_progress(&plan)
                        {
                            result.absent_objects = Some(absent);
                            result.remaining_objects = Some(remaining);
                        }
                    }
                    _ => (),
                }
                progress.last_result = Some(result.clone());
                receipt.entries = vec![serde_json::to_value(&progress).map_err(|_| internal())?];
                self.receipts.save(&key, &receipt)?;
                return Ok(result);
            }
        };
        let result = ArtifactsMaintenanceApplyResult {
            applied: true,
            verified_objects: effect
                .2
                .as_ref()
                .map(|r| r.verified.clone())
                .unwrap_or_default(),
            damaged_objects: effect.2.map(|r| r.damaged).unwrap_or_default(),
            status: "completed".into(),
            error_code: None,
            absent_objects: if is_collection {
                p.plan.details["objects"].as_array().map(|v| v.len() as u32)
            } else {
                None
            },
            remaining_objects: is_collection.then_some(0),
            reconciliation_required: false,
            approval_id: progress.approval_id,
            artifact_ids: effect.0,
            size_bytes: effect.1,
            replayed,
        };
        {
            use zeus_verify::audit::{AuditEntry, AuditOutcome, AuditSink};
            let mut event = AuditEntry::new(
                Timestamp::now(),
                ctx.identity.action_actor(),
                "artifact.maintenance_completed",
                AuditOutcome::Ok,
            );
            event
                .data
                .insert("plan_hash".into(), serde_json::json!(p.plan_hash));
            event
                .data
                .insert("size_bytes".into(), serde_json::json!(result.size_bytes));
            event.data.insert(
                "record_count".into(),
                serde_json::json!(result.artifact_ids.len()),
            );
            self.audit.append(event).map_err(error)?;
        }
        self.store_receipt(Some((key, receipt)), &result)?;
        Ok(result)
    }
    pub(super) async fn artifact_maintenance_status(
        &self,
        ctx: &CallContext,
        p: ArtifactsMaintenanceStatusParams,
    ) -> ApiResult<ArtifactsMaintenanceStatusResult> {
        if p.idempotency_key.is_empty()
            || p.idempotency_key.len() > 128
            || p.idempotency_key.chars().any(char::is_control)
            || hash(&p.plan)? != p.plan_hash
        {
            return Err(ApiError::invalid_params(
                "use the exact saved plan and retry key",
            ));
        }
        let key = crate::mutations::MutationStore::binding(
            ctx.identity.credential_id.as_str(),
            "artifacts.maintenance.apply",
            &p.idempotency_key,
        )?;
        let receipt = self
            .receipts
            .lookup(&key, serde_json::to_value(&p).map_err(|_| internal())?)?
            .ok_or_else(|| {
                ApiError::new(
                    "api.not_found",
                    "this owner has no saved maintenance request",
                )
            })?;
        if let Some(value) = receipt.result {
            let mut result: ArtifactsMaintenanceApplyResult =
                serde_json::from_value(value).map_err(|_| internal())?;
            result.replayed = true;
            return Ok(result);
        }
        let progress: Progress = receipt
            .entries
            .first()
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| internal())?
            .ok_or_else(|| {
                ApiError::new(
                    "api.not_found",
                    "this owner has no saved maintenance request",
                )
            })?;
        if let Some(mut result) = progress.last_result {
            result.replayed = true;
            return Ok(result);
        }
        let running = self.artifact_copies.contains(&key)?;
        let mut result = ArtifactsMaintenanceApplyResult {
            applied: false,
            status: if running {
                "in_progress"
            } else if progress.started {
                "reconciliation_required"
            } else {
                "awaiting_approval"
            }
            .into(),
            error_code: None,
            approval_id: progress.approval_id,
            artifact_ids: vec![],
            size_bytes: 0,
            replayed: true,
            absent_objects: None,
            remaining_objects: None,
            reconciliation_required: progress.started && !running,
            verified_objects: vec![],
            damaged_objects: vec![],
        };
        if !running {
            match p.plan.operation {
                ArtifactMaintenanceOperation::Import { .. } => {
                    if let Ok(summary) =
                        serde_json::from_value::<zeus_artifact::BundleSummary>(p.plan.details)
                    {
                        result.artifact_ids = self
                            .artifacts
                            .bundle_import_progress(&summary)
                            .unwrap_or_default();
                    }
                }
                ArtifactMaintenanceOperation::Collect { .. } => {
                    if let Ok(plan) = serde_json::from_value::<CollectionPlan>(p.plan.details)
                        && let Ok((absent, remaining)) = self.artifacts.collection_progress(&plan)
                    {
                        result.absent_objects = Some(absent);
                        result.remaining_objects = Some(remaining);
                    }
                }
                _ => (),
            }
        }
        Ok(result)
    }
    pub(super) async fn artifact_events(
        &self,
        ctx: &CallContext,
        p: ArtifactsEventsParams,
    ) -> ApiResult<ArtifactsEventsResult> {
        let limit = p.limit.unwrap_or(20) as usize;
        if !(1..=100).contains(&limit) {
            return Err(ApiError::invalid_params("event limit must be1–100"));
        }
        let mut events = Vec::new();
        let mut more = false;
        let mut earliest_seq = None;
        let head=self.audit.visit_records(|record|{
            earliest_seq.get_or_insert(record.seq);
            if record.seq<=p.after_seq||!record.action.starts_with("artifact."){return Ok(());}
            let id=record.data.get("artifact_id").and_then(|v|serde_json::from_value::<zeus_types::ArtifactId>(v.clone()).ok());
            let visible=match id{Some(id)=>self.visible_artifact(ctx,id).is_ok(),None=>Self::scope(ctx).service().is_none() || Self::scope(ctx).audit(record.actor.client_id.as_deref())};
            if !visible{return Ok(());}
            if events.len()==limit{more=true;return Ok(());}
            let phase=if matches!(record.action.as_str(),"artifact.deleted"|"artifact.gc_sweep"|"artifact.scrub_intent"){"intent"}else if record.action.ends_with("_completed"){"completed"}else{"recorded"};
            let mut event=serde_json::json!({"seq":record.seq,"ts":record.ts,"action":record.action,"phase":phase,"hash":record.hash,"prev_hash":record.prev_hash});
            for field in ["artifact_id","blake3","sha256","size_bytes","objects","bytes","digests_blake3","reason","fields","plan_hash","record_count"]{if let Some(value)=record.data.get(field){event[field]=value.clone();}}
            events.push(event);Ok(())
        }).map_err(error)?;
        if p.chain_id.is_some_and(|c| c != head.chain_id) || p.after_seq > head.seq {
            return Err(ApiError::new(
                "artifact.event_resync_required",
                "audit chain changed or cursor is beyond its head; refresh visible artifacts",
            ));
        }
        let next_seq = if more {
            events
                .last()
                .and_then(|v| v["seq"].as_u64())
                .unwrap_or(p.after_seq)
        } else {
            head.seq
        };
        let earliest_seq = earliest_seq.unwrap_or(head.seq);
        Ok(ArtifactsEventsResult {
            earliest_seq,
            history_truncated: p.after_seq.saturating_add(1) < earliest_seq,
            events,
            next_seq,
            has_more: more,
            head,
            store_format_version: 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{context, setup};
    use super::*;
    use zeus_api::methods::sessions::{
        ApprovalView, ApprovalsDecideParams, ConfirmationLevel, DecideOutcome,
    };
    fn stored(backend: &RuntimeBackend, root: &Path, name: &str) -> zeus_types::ArtifactId {
        let path = root.join("downloads").join(name);
        std::fs::write(&path, name.as_bytes()).unwrap();
        backend
            .artifacts
            .ingest(
                &path,
                zeus_artifact::ArtifactDraft::local("zeus", "test", Some(name.into())),
                None,
            )
            .unwrap()
            .id
    }
    async fn plan(
        backend: &RuntimeBackend,
        ctx: &CallContext,
        operation: ArtifactMaintenanceOperation,
        key: &str,
    ) -> ArtifactsMaintenanceApplyParams {
        let preview = backend
            .artifact_maintenance_preview(ctx, ArtifactsMaintenancePreviewParams { operation })
            .await
            .unwrap();
        ArtifactsMaintenanceApplyParams {
            plan: preview.plan,
            plan_hash: preview.plan_hash,
            idempotency_key: key.into(),
        }
    }
    async fn approve(backend: &RuntimeBackend, ctx: &CallContext, id: ApprovalId) {
        let view = ApprovalView::try_from(backend.consent.get(&ctx.identity, id).unwrap()).unwrap();
        let p = ApprovalsDecideParams {
            approval_id: id,
            request_hash: view.request_hash,
            outcome: DecideOutcome::Approve,
            scope: None,
            channel: Surface::CliInteractive,
            confirmation: ConfirmationLevel::TypedCode,
            idempotency_key: None,
        };
        let evidence = serde_json::json!({"client_id":ctx.identity.credential_id.as_str(),"surface":"cli_interactive","exe_verified":true,"credential_kind":ctx.identity.kind,"effective_kind":ctx.identity.kind,"role":ctx.identity.role,"tty":"verified","confirmation":"typed_code","peer_pid":1});
        backend
            .consent
            .decide(&ctx.identity, &p, evidence)
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn maintenance_export_is_exact_no_replace_and_completed_retries_preserve_later_user_edits()
     {
        let dir = tempfile::tempdir().unwrap();
        let backend = setup(dir.path()).await;
        let (ctx, _rx) = context(dir.path(), "agent");
        let id = stored(&backend, dir.path(), "report.txt");
        let path = dir.path().join("downloads/report.zeusab");
        let request = plan(
            &backend,
            &ctx,
            ArtifactMaintenanceOperation::Export {
                artifact_ids: vec![id],
                path: path.display().to_string(),
                include_objects: true,
            },
            "export",
        )
        .await;
        assert!(!path.exists());
        let result = backend
            .artifact_maintenance_apply(&ctx, request.clone())
            .await
            .unwrap();
        assert!(result.applied);
        assert_eq!(result.status, "completed");
        let bundle = ArtifactBundle::read(std::fs::File::open(&path).unwrap()).unwrap();
        assert_eq!(bundle.summary().objects.len(), 1);
        std::fs::write(&path, b"later user content").unwrap();
        let replay = backend
            .artifact_maintenance_apply(&ctx, request.clone())
            .await
            .unwrap();
        assert!(replay.replayed);
        let status = backend
            .artifact_maintenance_status(&ctx, request.clone())
            .await
            .unwrap();
        assert!(status.applied && status.replayed);
        let (other, _other_rx) = context(dir.path(), "cli");
        assert!(
            backend
                .artifact_maintenance_status(&other, request.clone())
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"later user content");
        let mut different = request;
        different.plan.expires_at = different.plan.expires_at.saturating_add_ms(1);
        different.plan_hash = hash(&different.plan).unwrap();
        assert!(
            backend
                .artifact_maintenance_apply(&ctx, different)
                .await
                .is_err()
        );
        backend.engine.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn maintenance_status_reads_expired_partial_receipt_without_resuming_effects() {
        let dir = tempfile::tempdir().unwrap();
        let backend = setup(dir.path()).await;
        let (ctx, _rx) = context(dir.path(), "agent");
        let id = stored(&backend, dir.path(), "interrupted.txt");
        backend
            .artifacts
            .set_retention(id, zeus_artifact::Retention::Evictable, None)
            .unwrap();
        let mut request = plan(
            &backend,
            &ctx,
            ArtifactMaintenanceOperation::Collect {
                quota_pressure: true,
                limit: 1,
            },
            "expired-partial",
        )
        .await;
        request.plan.expires_at = Timestamp::UNIX_EPOCH;
        request.plan_hash = hash(&request.plan).unwrap();
        assert!(
            backend
                .artifact_maintenance_status(&ctx, request.clone())
                .await
                .is_err()
        );
        let key = crate::mutations::MutationStore::binding(
            ctx.identity.credential_id.as_str(),
            "artifacts.maintenance.apply",
            &request.idempotency_key,
        )
        .unwrap();
        assert!(
            backend
                .receipts
                .lookup(&key, serde_json::to_value(&request).unwrap())
                .unwrap()
                .is_none()
        );
        let mut receipt = backend
            .receipts
            .begin(&key, serde_json::to_value(&request).unwrap())
            .unwrap();
        receipt.entries = vec![
            serde_json::to_value(Progress {
                started: true,
                approval_id: None,
                last_result: None,
            })
            .unwrap(),
        ];
        backend.receipts.save(&key, &receipt).unwrap();
        let status = backend
            .artifact_maintenance_status(&ctx, request.clone())
            .await
            .unwrap();
        assert_eq!(status.status, "reconciliation_required");
        assert!(status.reconciliation_required);
        assert_eq!(status.absent_objects, Some(0));
        assert_eq!(status.remaining_objects, Some(1));
        assert!(
            backend
                .artifact_maintenance_apply(&ctx, request)
                .await
                .is_err()
        );
        assert!(
            backend
                .artifacts
                .object_path(backend.artifacts.get(id).unwrap().blake3)
                .exists()
        );
        backend.engine.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn maintenance_destination_replacement_and_forged_approval_flags_fail_before_effects() {
        let dir = tempfile::tempdir().unwrap();
        let backend = setup(dir.path()).await;
        let (ctx, _rx) = context(dir.path(), "agent");
        let id = stored(&backend, dir.path(), "report.txt");
        let folder = dir.path().join("downloads/export");
        std::fs::create_dir(&folder).unwrap();
        let request = plan(
            &backend,
            &ctx,
            ArtifactMaintenanceOperation::Export {
                artifact_ids: vec![id],
                path: folder.join("result.zeusab").display().to_string(),
                include_objects: false,
            },
            "export",
        )
        .await;
        std::fs::rename(&folder, dir.path().join("downloads/old")).unwrap();
        std::fs::create_dir(&folder).unwrap();
        assert!(
            backend
                .artifact_maintenance_apply(&ctx, request)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
        backend
            .artifacts
            .set_retention(id, zeus_artifact::Retention::Evictable, None)
            .unwrap();
        let mut collect = plan(
            &backend,
            &ctx,
            ArtifactMaintenanceOperation::Collect {
                quota_pressure: true,
                limit: 5,
            },
            "collect",
        )
        .await;
        collect.plan.requires_approval = false;
        collect.plan_hash = hash(&collect.plan).unwrap();
        assert!(
            backend
                .artifact_maintenance_apply(&ctx, collect)
                .await
                .is_err()
        );
        assert!(
            backend
                .artifacts
                .object_path(backend.artifacts.get(id).unwrap().blake3)
                .exists()
        );
        backend.engine.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn maintenance_collection_waits_for_human_and_retention_changes_invalidate_the_decision()
    {
        let dir = tempfile::tempdir().unwrap();
        let backend = setup(dir.path()).await;
        let (agent, _rx) = context(dir.path(), "agent");
        let (human, _hrx) = context(dir.path(), "cli");
        let id = stored(&backend, dir.path(), "review.txt");
        backend
            .artifacts
            .set_retention(id, zeus_artifact::Retention::Evictable, None)
            .unwrap();
        let request = plan(
            &backend,
            &agent,
            ArtifactMaintenanceOperation::Collect {
                quota_pressure: true,
                limit: 5,
            },
            "collect",
        )
        .await;
        let pending = backend
            .artifact_maintenance_apply(&agent, request.clone())
            .await
            .unwrap();
        assert_eq!(pending.status, "awaiting_approval");
        assert!(!pending.applied);
        let status = backend
            .artifact_maintenance_status(&agent, request.clone())
            .await
            .unwrap();
        assert_eq!(status.status, "awaiting_approval");
        assert_eq!(status.approval_id, pending.approval_id);
        assert!(!status.applied && status.replayed);
        assert!(
            backend
                .artifacts
                .object_path(backend.artifacts.get(id).unwrap().blake3)
                .exists()
        );
        let approval = pending.approval_id.unwrap();
        approve(&backend, &human, approval).await;
        backend
            .artifacts
            .set_retention(id, zeus_artifact::Retention::Keep, None)
            .unwrap();
        assert!(
            backend
                .artifact_maintenance_apply(&agent, request)
                .await
                .is_err()
        );
        backend
            .artifacts
            .set_retention(id, zeus_artifact::Retention::Evictable, None)
            .unwrap();
        let request = plan(
            &backend,
            &agent,
            ArtifactMaintenanceOperation::Collect {
                quota_pressure: true,
                limit: 5,
            },
            "collect-fresh",
        )
        .await;
        let pending = backend
            .artifact_maintenance_apply(&agent, request.clone())
            .await
            .unwrap();
        approve(&backend, &human, pending.approval_id.unwrap()).await;
        let done = backend
            .artifact_maintenance_apply(&agent, request.clone())
            .await
            .unwrap();
        assert!(done.applied);
        assert_eq!(done.absent_objects, Some(1));
        assert_eq!(done.remaining_objects, Some(0));
        assert_eq!(
            backend.artifacts.get(id).unwrap().object.state,
            zeus_artifact::ObjectState::Absent
        );
        assert!(dir.path().join("downloads/review.txt").exists());
        assert!(
            backend
                .artifact_maintenance_apply(&agent, request)
                .await
                .unwrap()
                .replayed
        );
        backend.engine.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn maintenance_import_checks_whole_source_before_effects_and_replays_committed_identity()
    {
        let source_dir = tempfile::tempdir().unwrap();
        let source = setup(source_dir.path()).await;
        let id = stored(&source, source_dir.path(), "source.txt");
        let bundle = source
            .artifacts
            .export_bundle(&[id], true, &privacy())
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let backend = setup(dir.path()).await;
        let (ctx, _rx) = context(dir.path(), "agent");
        let path = dir.path().join("downloads/input.zeusab");
        bundle.write(std::fs::File::create(&path).unwrap()).unwrap();
        let request = plan(
            &backend,
            &ctx,
            ArtifactMaintenanceOperation::Import {
                path: path.display().to_string(),
            },
            "import",
        )
        .await;
        std::fs::write(&path, b"corrupt bundle").unwrap();
        assert!(
            backend
                .artifact_maintenance_apply(&ctx, request)
                .await
                .is_err()
        );
        assert!(
            backend
                .artifacts
                .list(&Default::default())
                .unwrap()
                .is_empty()
        );
        bundle.write(std::fs::File::create(&path).unwrap()).unwrap();
        let request = plan(
            &backend,
            &ctx,
            ArtifactMaintenanceOperation::Import {
                path: path.display().to_string(),
            },
            "import-corrected",
        )
        .await;
        let done = backend
            .artifact_maintenance_apply(&ctx, request.clone())
            .await
            .unwrap();
        assert!(done.applied);
        assert_eq!(done.artifact_ids.len(), 1);
        assert_eq!(
            backend
                .artifacts
                .get(done.artifact_ids[0])
                .unwrap()
                .imports
                .len(),
            1
        );
        assert_eq!(
            backend
                .artifact_maintenance_apply(&ctx, request)
                .await
                .unwrap()
                .artifact_ids,
            done.artifact_ids
        );
        backend.engine.shutdown().await.unwrap();
        source.engine.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn maintenance_event_replay_returns_oldest_page_and_distinguishes_intents() {
        let dir = tempfile::tempdir().unwrap();
        let backend = setup(dir.path()).await;
        let (ctx, _rx) = context(dir.path(), "agent");
        let id = stored(&backend, dir.path(), "event.txt");
        backend
            .artifacts
            .set_retention(id, zeus_artifact::Retention::Keep, None)
            .unwrap();
        backend
            .artifacts
            .set_retention(id, zeus_artifact::Retention::Evictable, None)
            .unwrap();
        backend.artifacts.delete(id).unwrap();
        let first = backend
            .artifact_events(
                &ctx,
                ArtifactsEventsParams {
                    after_seq: 0,
                    limit: Some(1),
                    chain_id: None,
                },
            )
            .await
            .unwrap();
        assert!(first.has_more);
        assert_eq!(first.events[0]["action"], "artifact.updated");
        assert!(!first.history_truncated);
        let next = backend
            .artifact_events(
                &ctx,
                ArtifactsEventsParams {
                    after_seq: first.next_seq,
                    limit: Some(100),
                    chain_id: Some(first.head.chain_id),
                },
            )
            .await
            .unwrap();
        assert_eq!(next.events.len(), 2);
        assert_eq!(next.events[1]["action"], "artifact.deleted");
        assert_eq!(next.events[1]["phase"], "intent");
        assert!(!next.events[0].as_object().unwrap().contains_key("path"));
        assert!(
            backend
                .artifact_events(
                    &ctx,
                    ArtifactsEventsParams {
                        after_seq: 0,
                        limit: Some(20),
                        chain_id: Some(zeus_types::IdGenerator::new().chain_id())
                    }
                )
                .await
                .is_err()
        );
        backend.engine.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn maintenance_repair_requires_exact_source_and_human_decision() {
        let dir = tempfile::tempdir().unwrap();
        let backend = setup(dir.path()).await;
        let (agent, _rx) = context(dir.path(), "agent");
        let (human, _hrx) = context(dir.path(), "cli");
        let content = b"repair this content";
        let digests = zeus_verify::hash::hash_bytes(content);
        let record = backend
            .artifacts
            .ingest_record_only(
                digests,
                zeus_artifact::ArtifactDraft::local("zeus", "test", None),
                None,
                None,
            )
            .unwrap();
        let source = dir.path().join("downloads/repair.bin");
        std::fs::write(&source, content).unwrap();
        let request = plan(
            &backend,
            &agent,
            ArtifactMaintenanceOperation::Repair {
                artifact_id: record.id,
                path: source.display().to_string(),
            },
            "repair",
        )
        .await;
        let pending = backend
            .artifact_maintenance_apply(&agent, request.clone())
            .await
            .unwrap();
        assert!(!pending.applied);
        assert_eq!(
            backend.artifacts.get(record.id).unwrap().object.state,
            zeus_artifact::ObjectState::Absent
        );
        approve(&backend, &human, pending.approval_id.unwrap()).await;
        let done = backend
            .artifact_maintenance_apply(&agent, request.clone())
            .await
            .unwrap();
        assert!(done.applied);
        assert_eq!(done.artifact_ids, vec![record.id]);
        assert_eq!(
            backend.artifacts.get(record.id).unwrap().object.state,
            zeus_artifact::ObjectState::Present
        );
        assert_eq!(std::fs::read(&source).unwrap(), content);
        assert!(
            backend
                .artifact_maintenance_apply(&agent, request)
                .await
                .unwrap()
                .replayed
        );
        backend.engine.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn maintenance_scrub_waits_for_human_before_quarantining_mismatched_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let backend = setup(dir.path()).await;
        let (agent, _rx) = context(dir.path(), "agent");
        let (human, _hrx) = context(dir.path(), "cli");
        let id = stored(&backend, dir.path(), "integrity.txt");
        let record = backend.artifacts.get(id).unwrap();
        let path = backend.artifacts.object_path(record.blake3);
        std::fs::rename(&path, dir.path().join("saved-original")).unwrap();
        std::fs::write(&path, b"bad").unwrap();
        let request = plan(
            &backend,
            &agent,
            ArtifactMaintenanceOperation::Scrub {
                artifact_ids: vec![id],
            },
            "scrub",
        )
        .await;
        let pending = backend
            .artifact_maintenance_apply(&agent, request.clone())
            .await
            .unwrap();
        assert!(!pending.applied);
        assert!(path.exists());
        approve(&backend, &human, pending.approval_id.unwrap()).await;
        let done = backend
            .artifact_maintenance_apply(&agent, request.clone())
            .await
            .unwrap();
        assert!(done.applied);
        assert_eq!(done.damaged_objects, vec![record.blake3]);
        assert_eq!(
            backend.artifacts.get(id).unwrap().object.state,
            zeus_artifact::ObjectState::Damaged
        );
        assert!(!path.exists());
        assert!(dir.path().join("downloads/integrity.txt").exists());
        assert!(
            backend
                .artifact_maintenance_apply(&agent, request)
                .await
                .unwrap()
                .replayed
        );
        backend.engine.shutdown().await.unwrap();
    }
}
