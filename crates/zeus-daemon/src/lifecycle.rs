//! Grant and approval lifecycle maintenance, driven by durable state rather than UI connections.
use super::*;
use zeus_types::event::{ApprovalDecidedData, GrantExpiredData, GrantRevokedData};
use zeus_types::*;
use zeus_verify::audit::{AuditEntry, AuditOutcome, AuditSink};
impl RuntimeBackend {
    pub(super) async fn change_kill_switch(
        &self,
        actor: Actor,
        engaged: bool,
        reason: Option<&str>,
    ) -> ApiResult<KillSwitchResult> {
        let _gate = self.mutations.lock().await;
        if reason.is_some_and(|r| r.chars().count() > 280) {
            return Err(ApiError::invalid_params("reason exceeds 280 characters"));
        }
        let changed = self.journal.meta().map_err(error)?.kill_switch_engaged() != engaged;
        let mut entry = AuditEntry::new(
            Timestamp::now(),
            actor.clone(),
            "daemon.kill_switch",
            AuditOutcome::Ok,
        );
        entry.write_ahead = true;
        entry.data.insert(
            "action".into(),
            serde_json::json!(if engaged { "engage" } else { "release" }),
        );
        entry.data.insert(
            "reason_blake3".into(),
            serde_json::json!(reason.map(|s| blake3::hash(s.as_bytes()).to_hex().to_string())),
        );
        let retained = entry.clone();
        let audit = self.append_kill_switch_audit(entry);
        // The emergency stop remains available even when the audit volume is full; its record is retained
        // and appended once the log recovers (04 section 4.6 item 8). Release fails closed.
        if !engaged {
            audit?;
        } else if audit.is_err() {
            self.deferred_audit.retain(&retained);
        }
        let outcome = self
            .engine
            .set_kill_switch(engaged, actor)
            .await
            .map_err(error)?;
        if engaged {
            self.workflows.pause_all_for_kill_switch().await?;
            self.cloud.pause_all_for_kill_switch().await?;
        }
        let pending = self
            .journal
            .read(|tx| tx.approvals(Some(&[ApprovalState::Pending])))
            .map_err(error)?
            .len();
        Ok(KillSwitchResult {
            changed,
            replayed: false,
            engaged,
            grants_revoked: outcome.grants_revoked.min(u32::MAX as u64) as u32,
            jobs_paused: outcome.jobs_paused.min(u32::MAX as u64) as u32,
            approvals_pending: pending.min(u32::MAX as usize) as u32,
        })
    }
    pub(super) async fn revoke(&self, id: GrantId, actor: Actor) -> ApiResult<SessionGrant> {
        let _gate = self.mutations.lock().await;
        let mut grant = self
            .journal
            .read(|tx| tx.grant(id))
            .map_err(error)?
            .ok_or_else(|| ApiError::new("api.not_found", "grant not found"))?;
        if grant.state != GrantState::Revoked {
            let now = Timestamp::now();
            let mut entry = AuditEntry::new(now, actor.clone(), "grant.revoked", AuditOutcome::Ok);
            entry.subject.grant_id = Some(id);
            entry.subject.origin = Some(grant.origin.clone());
            self.audit.append(entry).map_err(error)?;
            grant.state = GrantState::Revoked;
            grant.revoked_at = Some(now);
            grant.revoked_by = Some(actor.clone());
            grant.revocation_reason = Some(RevocationReason::UserRequest);
            self.journal
                .transact(|tx| {
                    tx.put_grant(&grant)?;
                    tx.append_event(
                        now,
                        None,
                        None,
                        GrantRevokedData {
                            grant_id: id,
                            origin: grant.origin.clone(),
                            by: actor,
                        }
                        .into(),
                    )?;
                    Ok(())
                })
                .map_err(error)?;
        }
        self.engine
            .grants_changed(zeus_engine::api::GrantChange {
                origin: grant.origin.clone(),
                kind: zeus_engine::api::GrantChangeKind::Revoked,
            })
            .await
            .map_err(error)?;
        Ok(grant)
    }
    /// Apply a durable decision to every job it covers (07 section 4.7 rule 5). A covered job that already left
    /// the approval, for example by being canceled, is skipped; the maintenance tick rechecks the rest.
    pub(super) async fn apply_decision(
        &self,
        approval: &Approval,
        grant: &Option<SessionGrant>,
    ) -> ApiResult<()> {
        let mut jobs: Vec<JobId> = approval
            .facts
            .as_ref()
            .and_then(|f| f.get("transfer"))
            .and_then(|t| t["job_ids"].as_array())
            .map(|ids| {
                ids.iter()
                    .filter_map(|v| v.as_str()?.parse().ok())
                    .collect()
            })
            .unwrap_or_default();
        if jobs.is_empty() {
            jobs.extend(approval.job_id);
        }
        let single = jobs.len() == 1;
        for job_id in jobs {
            let applied = self
                .engine
                .apply_approval(zeus_engine::api::ApprovalResolution {
                    approval: approval.clone(),
                    grant: grant.clone(),
                    job_id: Some(job_id),
                })
                .await;
            match applied {
                Ok(()) => {}
                Err(e) if single => return Err(error(e)),
                Err(e) => tracing::debug!(
                    code = %e.to_envelope().code,
                    "a batched job no longer waits for this approval"
                ),
            }
        }
        Ok(())
    }
    fn append_kill_switch_audit(
        &self,
        entry: AuditEntry,
    ) -> ApiResult<zeus_verify::audit::AuditRecord> {
        #[cfg(test)]
        if self
            .fail_kill_switch_audit
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(ApiError::new("audit.unavailable", "audit log unavailable"));
        }
        self.audit.append(entry).map_err(error)
    }
    /// Append retained kill-switch records once the audit log accepts writes again.
    pub(super) fn replay_deferred_audit(&self) {
        if self.deferred_audit.is_pending() {
            let audit = self.audit.clone();
            self.deferred_audit
                .replay(audit.as_ref(), &Default::default());
        }
    }
    /// Run one bounded maintenance tick. Replaying terminal approvals repairs interrupted decisions.
    pub async fn maintain(&self) -> ApiResult<()> {
        self.replay_deferred_audit();
        self.run_file_operations().await;
        let now = Timestamp::now();
        let origins = self.journal.transact(|tx| {
            let mut origins = std::collections::BTreeSet::new();
            for mut grant in tx.grants()? {
                if grant.state == GrantState::Active && grant.expires_at.is_some_and(|at| at <= now) {
                    grant.state = GrantState::Expired; tx.put_grant(&grant)?;
                    tx.append_event(now, None, None, GrantExpiredData { grant_id: grant.id, origin: grant.origin.clone() }.into())?;
                    origins.insert(grant.origin);
                }
            }
            for mut approval in tx.approvals(Some(&[ApprovalState::Pending]))? {
                if approval.expires_at <= now {
                    approval.state = ApprovalState::Expired; approval.decided_at = Some(now);
                    approval.decision = Some(Decision { outcome: DecisionOutcome::Expire, scope: None, actor: Actor::system(), grant_id: None });
                    tx.put_approval(&approval)?;
                    if approval.revision.is_some() && approval.request_hash.is_some() {
                        tx.put_approval_decision(approval.id,&serde_json::json!({"approval_id":approval.id,"revision":approval.revision,"request_hash":approval.request_hash,
                            "outcome":"expire","actor":Actor::system(),"decided_at":now,"grant_ids":[],"system_reason":"deadline"}))?;
                    }
                    tx.append_event(now, approval.job_id, approval.run_id, ApprovalDecidedData { approval_id: approval.id, kind: approval.kind,
                        outcome: DecisionOutcome::Expire, scope: None, actor: Actor::system(), grant_id: None }.into())?;
                }
            }
            Ok(origins)
        }).map_err(error)?;
        for origin in origins {
            self.engine
                .grants_changed(zeus_engine::api::GrantChange {
                    origin,
                    kind: zeus_engine::api::GrantChangeKind::Expired,
                })
                .await
                .map_err(error)?;
        }
        for record in self.journal.list_incomplete().map_err(error)? {
            if record.job.state == JobState::AwaitingApproval
                && let Some(id) = record.job.approval_id
                && let Some(approval) = self.journal.read(|tx| tx.approval(id)).map_err(error)?
                && matches!(
                    approval.state,
                    ApprovalState::Approved | ApprovalState::Denied | ApprovalState::Expired
                )
            {
                // A concurrent UI decision may already have applied it; the next tick rechecks.
                let _ = self
                    .engine
                    .apply_approval(zeus_engine::api::ApprovalResolution {
                        approval,
                        grant: None,
                        job_id: Some(record.job.id),
                    })
                    .await;
            }
        }
        self.engine.refresh_sessions().await.map_err(error)?;
        self.flush_outbox().await
    }
}
