//! `files.organize` and `files.undo` (05 section 9.6): plans in the caller's view, approval-bound execution.
use super::*;

impl RuntimeBackend {
    /// The write roots this connection may operate in (05 section 7.5).
    pub(super) fn visible_roots(&self, ctx: &CallContext) -> Vec<PathBuf> {
        match Self::scope(ctx).service() {
            Some(_) => self.credentials.roots(&ctx.identity.credential_id),
            None => self.config.effective_write_roots(),
        }
    }

    pub(super) async fn organize(
        &self,
        ctx: &CallContext,
        p: FilesOrganizeParams,
    ) -> ApiResult<FilesOrganizeResult> {
        let plan_now = |organizer: Arc<crate::organizer::Organizer>,
                        roots: Vec<PathBuf>,
                        dir: String,
                        rules: serde_json::Value| async move {
            tokio::task::spawn_blocking(move || organizer.plan(&roots, &dir, &rules))
                .await
                .map_err(|_| internal())?
        };
        if p.dry_run {
            // Dry runs execute nothing and store nothing (05 section 12 rule 6).
            let plan = plan_now(
                self.organizer.clone(),
                self.visible_roots(ctx),
                p.dir.clone(),
                p.rules.clone(),
            )
            .await?;
            return Ok(FilesOrganizeResult {
                replayed: false,
                steps: plan.view(),
                applied: false,
                approval_id: None,
                operation_id: None,
            });
        }
        self.admit()?;
        let _gate = self.mutations.lock().await;
        let (binding, replay) = self.receipt::<FilesOrganizeResult>(
            ctx,
            "files.organize",
            p.idempotency_key.as_deref(),
            &p,
        )?;
        if let Some(mut result) = replay {
            result.replayed = true;
            return Ok(result);
        }
        let plan = plan_now(
            self.organizer.clone(),
            self.visible_roots(ctx),
            p.dir.clone(),
            p.rules.clone(),
        )
        .await?;
        let steps = plan.view();
        let result = if plan.is_empty() {
            FilesOrganizeResult {
                replayed: false,
                steps,
                applied: true,
                approval_id: None,
                operation_id: None,
            }
        } else {
            self.audit_tool_call(
                ctx.identity.action_actor(),
                "files.organize",
                &p,
                &[("steps", serde_json::json!(steps.len()))],
            )?;
            let (operation_id, approval_id) = self.organizer.submit(&ctx.identity, plan).await?;
            FilesOrganizeResult {
                replayed: false,
                steps,
                applied: false,
                approval_id: Some(approval_id),
                operation_id: Some(operation_id),
            }
        };
        self.store_receipt(binding, &result)?;
        Ok(result)
    }

    pub(super) async fn undo(
        &self,
        ctx: &CallContext,
        p: FilesUndoParams,
    ) -> ApiResult<FilesUndoResult> {
        if p.operation_id.is_empty() || p.operation_id.len() > 64 {
            return Err(ApiError::invalid_params(
                "operation_id has 1 to 64 characters",
            ));
        }
        self.admit()?;
        let _gate = self.mutations.lock().await;
        let (binding, replay) =
            self.receipt::<FilesUndoResult>(ctx, "files.undo", p.idempotency_key.as_deref(), &p)?;
        if let Some(mut result) = replay {
            result.replayed = true;
            return Ok(result);
        }
        let organizer = self.organizer.clone();
        let identity = ctx.identity.clone();
        let id = p.operation_id.clone();
        let plan = tokio::task::spawn_blocking(move || organizer.plan_undo(&identity, &id))
            .await
            .map_err(|_| internal())??;
        let steps = plan.view();
        let result = if plan.is_empty() {
            FilesUndoResult {
                replayed: false,
                steps,
                approval_id: None,
                operation_id: None,
            }
        } else {
            self.audit_tool_call(
                ctx.identity.action_actor(),
                "files.undo",
                &p,
                &[("steps", serde_json::json!(steps.len()))],
            )?;
            let (operation_id, approval_id) = self.organizer.submit(&ctx.identity, plan).await?;
            FilesUndoResult {
                replayed: false,
                steps,
                approval_id: Some(approval_id),
                operation_id: Some(operation_id),
            }
        };
        self.store_receipt(binding, &result)?;
        Ok(result)
    }

    /// Sample, ask the model for selectors, then validate the assembled plan like any candidate (M01).
    pub(super) async fn draft_workflow(
        &self,
        ctx: &CallContext,
        actor: zeus_types::Actor,
        p: WorkflowsPlanParams,
        draft: zeus_api::methods::workflows::WorkflowDraftRequest,
    ) -> ApiResult<WorkflowsPlanResult> {
        let sampled = self.workflows.plan(&ctx.identity, p.clone()).await?;
        let (plan, info) = crate::workflow_draft::draft(
            &self.models,
            &p.goal,
            &p.start_urls,
            &sampled.samples,
            draft.max_cost_micro_usd,
        )
        .await?;
        let mut entry = zeus_verify::audit::AuditEntry::new(
            Timestamp::now(),
            actor,
            "tool.call",
            zeus_verify::audit::AuditOutcome::Ok,
        );
        entry
            .data
            .insert("tool".into(), serde_json::json!("workflows.plan"));
        entry.data.insert("draft".into(), serde_json::json!(true));
        entry
            .data
            .insert("provider".into(), serde_json::json!(info.provider));
        entry.data.insert(
            "cost_micro_usd".into(),
            serde_json::json!(info.cost_micro_usd),
        );
        zeus_verify::audit::AuditSink::append(self.audit.as_ref(), entry).map_err(error)?;
        let mut validated = self
            .workflows
            .plan(
                &ctx.identity,
                WorkflowsPlanParams {
                    plan_id: Some(sampled.plan_id),
                    candidate: Some(plan),
                    draft: None,
                    ..p
                },
            )
            .await?;
        validated.draft = Some(info);
        Ok(validated)
    }

    /// Execute approved file operations; the kill switch stops them (07 section 8.2).
    pub(super) async fn run_file_operations(&self) {
        let engaged = self
            .journal
            .meta()
            .map(|m| m.kill_switch_engaged())
            .unwrap_or(true);
        let organizer = self.organizer.clone();
        match tokio::task::spawn_blocking(move || organizer.tick(engaged)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(code = %error.envelope().code, "file operations need attention");
            }
            Err(_) => tracing::warn!("file operation worker stopped unexpectedly"),
        }
    }
}
