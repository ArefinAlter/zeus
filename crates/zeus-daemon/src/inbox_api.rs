//! Intake orchestration delegates dispatch to the existing authenticated enqueue implementation.
use super::*;
use crate::inbox::{self, Preview};
use std::collections::HashSet;

impl RuntimeBackend {
    pub(super) fn inbox_inspect_owner(
        &self,
        ctx: &CallContext,
        requested: Option<&str>,
    ) -> ApiResult<String> {
        let own = ctx.identity.credential_id.as_str();
        let Some(requested) = requested else {
            return Ok(own.to_owned());
        };
        if !ctx.identity.kind.is_human_capable() {
            return Err(ApiError::human_required("cross-owner inbox inspection"));
        }
        let id = zeus_api::auth::CredentialId::parse(requested)
            .map_err(|_| inbox::invalid("invalid inspected credential identifier"))?;
        Ok(id.to_string())
    }

    pub(super) async fn inbox_preview_impl(
        &self,
        ctx: &CallContext,
        p: InboxPreviewParams,
    ) -> ApiResult<InboxPreviewResult> {
        self.admit()?;
        let _gate = self.inbox.gate.lock().await;
        let owner = ctx.identity.credential_id.as_str();
        if let Some(r) = self
            .inbox
            .replay(owner, "preview", &p.idempotency_key, &p)?
        {
            return Ok(r);
        }
        let package = self.inbox.load(owner, &p.package_id)?;
        let candidates = package.candidates(&p)?;
        let mut jobs = vec![];
        let mut ids = vec![];
        let mut entries = vec![];
        let mut paths = HashSet::new();
        for entry in candidates {
            let mut job = entry.item.job.clone();
            if job.dest_dir.is_none() {
                job.dest_dir = package.view.default_dest_dir.clone();
            }
            // Resolve a deterministic filename now, rather than letting a future response choose it.
            if job.filename.is_none() {
                job.filename = Some(
                    job.url
                        .url()
                        .path_segments()
                        .and_then(|mut v| v.next_back())
                        .filter(|v| !v.is_empty())
                        .unwrap_or("download.bin")
                        .to_owned(),
                );
            }
            job.overwrite = Some(zeus_types::Overwrite::Fail);
            job.labels
                .insert("zeus.package".into(), package.view.id.to_string());
            job.labels.insert("zeus.inbox".into(), entry.id.to_string());
            let result = self
                .jobs_enqueue(
                    ctx,
                    EnqueueParams {
                        jobs: vec![job.clone()],
                        dry_run: true,
                        idempotency_key: None,
                    },
                )
                .await;
            let mut row = InboxPreviewEntry {
                entry_id: entry.id.clone(),
                redacted_url: job.url.redact(zeus_types::RedactMode::Log),
                filename: None,
                dest_path: None,
                approval_needed: job.session_origin.is_some(),
                warnings: vec![],
                error: None,
            };
            match result {
                Ok(result) => {
                    let planned = result
                        .jobs
                        .into_iter()
                        .next()
                        .and_then(|j| j.plan)
                        .ok_or_else(inbox::failed)?;
                    row.filename = planned.resolved_filename;
                    row.dest_path = planned.dest_path;
                    row.approval_needed = planned.approval_needed;
                    row.warnings = planned.warnings;
                    if let (Some(filename), Some(path)) = (&row.filename, &row.dest_path) {
                        let key = if cfg!(windows) {
                            path.to_lowercase()
                        } else {
                            path.clone()
                        };
                        let collision = std::fs::symlink_metadata(path);
                        if collision.is_ok()
                            || collision
                                .as_ref()
                                .is_err_and(|e| e.kind() != std::io::ErrorKind::NotFound)
                            || !paths.insert(key)
                        {
                            row.error=Some(inbox::conflict("destination occupied or duplicated in this batch; rename or choose another folder").0);
                        } else {
                            job.filename = Some(filename.clone());
                            job.dest_dir =
                                Path::new(path).parent().map(|p| p.display().to_string());
                            jobs.push(job);
                            ids.push(entry.id);
                        }
                    } else {
                        row.error = Some(
                            inbox::invalid("a concrete destination and filename are required").0,
                        );
                    }
                }
                Err(error) => {
                    let mut envelope = error.0;
                    envelope.message = "request cannot be planned; inspect the error code".into();
                    envelope.details = None;
                    row.error = Some(envelope);
                }
            }
            entries.push(row);
        }
        let view = InboxPreviewResult {
            preview_id: self.inbox.preview_id(),
            package_id: p.package_id.clone(),
            package_revision: package.view.revision,
            expires_at: Timestamp::now().saturating_add_ms(600_000),
            ready_entries: jobs.len() as u32,
            remaining_pending: package
                .view
                .pending_entries
                .saturating_sub(entries.len() as u32),
            entries,
            replayed: false,
        };
        self.inbox
            .save_preview(owner, &p, Preview { view, jobs, ids })
    }
    async fn inbox_dispatch(
        &self,
        ctx: &CallContext,
        package: &InboxPackageId,
        batch: crate::inbox::Batch,
    ) -> ApiResult<InboxSubmitResult> {
        let owner = ctx.identity.credential_id.as_str();
        if let Some(mut result) = batch.result.clone() {
            result.replayed = true;
            return Ok(result);
        }
        // Never mint a replacement request/key after an uncertain return. jobs.enqueue owns its durable
        // per-entry receipts and engine idempotency, so this exact retry recovers partial acceptance.
        match self.jobs_enqueue(ctx, batch.params.clone()).await {
            Ok(result) => self.inbox.finish(owner, package, &batch, result),
            Err(error) => {
                self.inbox.record_error(
                    owner,
                    package,
                    &batch.id,
                    error.envelope().code.as_str(),
                )?;
                let mut safe = error;
                safe.0.message = "batch remains pending; inspect and retry this same batch".into();
                safe.0.details = None;
                Err(safe)
            }
        }
    }
    pub(super) async fn inbox_submit_impl(
        &self,
        ctx: &CallContext,
        p: InboxSubmitParams,
    ) -> ApiResult<InboxSubmitResult> {
        let _gate = self.inbox.gate.lock().await;
        let owner = ctx.identity.credential_id.as_str();
        let (_package, batch) = self.inbox.begin(owner, &p)?;
        self.inbox_dispatch(ctx, &p.package_id, batch).await
    }
    pub(super) async fn inbox_retry_impl(
        &self,
        ctx: &CallContext,
        p: InboxSubmissionRetryParams,
    ) -> ApiResult<InboxSubmitResult> {
        let _gate = self.inbox.gate.lock().await;
        let owner = ctx.identity.credential_id.as_str();
        let batch = self.inbox.retry_batch(owner, &p)?;
        self.inbox_dispatch(ctx, &p.package_id, batch).await
    }
}
