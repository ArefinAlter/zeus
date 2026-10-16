//! Durable approval requests and human decisions; session secrets never enter this store.
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;
use zeus_api::auth::{CredentialId, CredentialStore};
use zeus_api::methods::sessions::*;
use zeus_api::{ApiError, ApiResult, ClientIdentity, ClientKind};
use zeus_engine::ports::{ApprovalPort, ApprovalRequest};
use zeus_journal::Journal;
use zeus_types::*;
use zeus_verify::audit::{AuditEntry, AuditOutcome, AuditSink};

fn error(e: impl ToEnvelope) -> ApiError {
    e.to_envelope().into()
}
fn missing() -> ApiError {
    ApiError::new("api.not_found", "approval or subject not found")
}
fn clean(value: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    use unicode_segmentation::UnicodeSegmentation;
    let text = value.nfc().filter(|c| !matches!(*c, '\u{061c}'|'\u{200b}'..='\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2060}'|'\u{2066}'..='\u{2069}'|'\u{feff}'))
        .map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
    let mut length = 0;
    text.graphemes(true)
        .take_while(|g| {
            length += g.chars().count();
            length <= 255
        })
        .collect()
}

/// Inputs whose trusted facts are bound into one pending approval.
struct ApprovalPreparation<'a> {
    requester: &'a str,
    origin: Origin,
    job: Option<&'a Job>,
    dest_path: Option<&'a str>,
    kind: ApprovalKind,
    reason: &'a str,
    reason_codes: &'a [String],
}

/// Owns serialized approval mutations and the write-ahead audit boundary.
pub struct ConsentManager {
    journal: Journal,
    audit: Arc<dyn AuditSink>,
    credentials: Arc<dyn CredentialStore>,
    gate: Mutex<()>,
    decisions: Mutex<crate::approvals::DecisionLimiter>,
    started: std::time::Instant,
}
impl ConsentManager {
    /// Build an approvals manager using the daemon's durable stores.
    pub fn new(
        journal: Journal,
        audit: Arc<dyn AuditSink>,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        Self {
            journal,
            audit,
            credentials,
            gate: Mutex::new(()),
            decisions: Mutex::new(crate::approvals::DecisionLimiter::per_minute()),
            started: std::time::Instant::now(),
        }
    }
    fn credential(&self, id: &str) -> ApiResult<zeus_api::auth::Credential> {
        self.credentials
            .get(&CredentialId::parse(id)?)
            .filter(|c| !c.revoked)
            .ok_or_else(missing)
    }
    /// Find an approval, enforcing requester isolation for unattended clients.
    pub fn get(&self, identity: &ClientIdentity, id: ApprovalId) -> ApiResult<Approval> {
        let approval = self
            .journal
            .read(|tx| tx.approval(id))
            .map_err(error)?
            .ok_or_else(missing)?;
        if identity.kind == ClientKind::Agent
            && approval.requested_by.as_deref() != Some(identity.credential_id.as_str())
        {
            return Err(missing());
        }
        Ok(approval)
    }
    /// List only requests this client is allowed to inspect.
    pub fn list(
        &self,
        identity: &ClientIdentity,
        state: ApprovalState,
        kind: Option<ApprovalKind>,
    ) -> ApiResult<Vec<ApprovalView>> {
        self.journal
            .read(|tx| tx.approvals(Some(&[state])))
            .map_err(error)?
            .into_iter()
            .filter(|a| {
                kind.is_none_or(|k| a.kind == k)
                    && (identity.kind != ClientKind::Agent
                        || a.requested_by.as_deref() == Some(identity.credential_id.as_str()))
            })
            .map(ApprovalView::try_from)
            .collect()
    }
    fn prepare(&self, preparation: ApprovalPreparation<'_>) -> ApiResult<Approval> {
        let ApprovalPreparation {
            requester,
            origin,
            job,
            dest_path,
            kind,
            reason,
            reason_codes,
        } = preparation;
        let credential = self.credential(requester)?;
        let now = Timestamp::now();
        let id = IdGenerator::new().approval_id();
        let expiry = now.saturating_add_ms(kind.default_expiry_ms());
        let mut scopes = Vec::new();
        if job.is_some() {
            scopes.push(GrantScope::Once);
        }
        scopes.push(GrantScope::Site);
        scopes.push(GrantScope::Never);
        let agent_says = clean(reason);
        let agent_says = (!agent_says.is_empty()).then_some(agent_says);
        let grants = self.journal.read(|tx| tx.grants()).map_err(error)?;
        let first = !grants.iter().any(|g| g.origin == origin);
        let reasons = if reason_codes.is_empty() {
            vec![if kind == ApprovalKind::Handoff {
                "handoff.interrupted".to_owned()
            } else {
                "session.requested".to_owned()
            }]
        } else {
            reason_codes.to_vec()
        };
        let destination = dest_path
            .and_then(|p| std::path::Path::new(p).parent())
            .map(|p| p.to_string_lossy().into_owned());
        let mut confirmation = serde_json::Map::new();
        for scope in &scopes {
            if *scope != GrantScope::Never {
                confirmation.insert(
                    serde_json::to_value(scope)
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_owned(),
                    serde_json::json!("gesture"),
                );
            }
        }
        let transfer = job.map(|j| serde_json::json!({"job_ids":[j.id], "total_bytes":j.spec.expected_size,
            "unknown_size_count":u32::from(j.spec.expected_size.is_none()), "dest_dirs":destination.iter().collect::<Vec<_>>(), "dest_dirs_omitted":0}));
        let items = job.map(|j| vec![serde_json::json!({"job_id":j.id, "untrusted_name":clean(j.spec.filename.as_deref().unwrap_or("download")), "untrusted_url":j.spec.url.redact(RedactMode::Log), "total_bytes":j.spec.expected_size})]).unwrap_or_default();
        let label = match credential.role {
            zeus_api::ClientRole::NativeHost => "host",
            zeus_api::ClientRole::Cli | zeus_api::ClientRole::Tui => "cli",
            _ => "agent",
        };
        let mut facts = serde_json::json!({"t":"zeus.approval.facts.v1", "approval_id":id, "revision":1, "kind":kind,
            "requester":{"kind":credential.kind,"role":credential.role,"client_id":requester,"label":label,"registered_at":credential.created_at},
            "issued_at":now,"expires_at":expiry,"allowed_scopes":scopes,"confirmation":confirmation,"reason_codes":reasons,
            "agent_says_blake3":agent_says.as_ref().map(|s|blake3::hash(s.as_bytes()).to_hex().to_string()),
            "origin":origin,"first_time_for_origin":first,"transfer":transfer,"items":items,"items_omitted":0});
        if kind == ApprovalKind::Handoff {
            let browser = job
                .and_then(|j| j.spec.source.browser.as_ref())
                .ok_or_else(|| ApiError::invalid_params("browser context required for handoff"))?;
            facts["browser"] = serde_json::json!({"name":browser.name,"profile_id":browser.profile_id,"incognito":browser.incognito});
        } else {
            facts["scope_hint"] = job
                .and_then(|j| j.spec.grant_scope_hint)
                .map_or(serde_json::Value::Null, |hint| {
                    serde_json::to_value(hint).unwrap()
                });
            facts["run_id"] = serde_json::Value::Null;
        }
        let request_hash = crate::approvals::facts_hash(&facts)?;
        serde_json::from_value(serde_json::json!({"id":id,"kind":kind,"state":"pending","job_id":job.map(|j|j.id),"origin":origin,
            "allowed_scopes":scopes,"reason_codes":reasons,"summary":{"title":if kind==ApprovalKind::Handoff {"Rescue browser download"} else {"Use website session"}},
            "created_at":now,"expires_at":expiry,"request_hash":request_hash,"requested_by":requester,"revision":1,"facts":facts,"agent_says":agent_says}))
            .map_err(|_|ApiError::internal("cannot construct approval"))
    }
    fn store(&self, approval: &Approval) -> ApiResult<()> {
        self.journal
            .transact(|tx| {
                tx.put_approval(approval)?;
                tx.append_event(
                    Timestamp::now(),
                    None,
                    approval.run_id,
                    zeus_types::event::ApprovalRequestedData {
                        approval: approval.clone(),
                    }
                    .into(),
                )?;
                Ok(())
            })
            .map_err(error)
    }
    /// Request consent, without any authority to grant it.
    pub async fn request_session(
        &self,
        identity: &ClientIdentity,
        p: SessionsRequestParams,
    ) -> ApiResult<SessionsRequestResult> {
        let _gate = self.gate.lock().await;
        if !p.origin.allows_session_release() {
            return Err(ApiError::invalid_params("sessions require an HTTPS origin"));
        }
        if p.run_id.is_some() {
            return Err(ApiError::not_implemented("workflow session grants"));
        }
        let job = p
            .job_id
            .map(|id| {
                self.journal
                    .read(|tx| tx.job(id))
                    .map_err(error)?
                    .ok_or_else(missing)
            })
            .transpose()?;
        if job.as_ref().is_some_and(|r| {
            r.job.spec.origin() != p.origin
                || r.job.spec.source.client_id.as_deref() != Some(identity.credential_id.as_str())
        }) {
            return Err(missing());
        }
        let grants = self.journal.read(|tx| tx.grants()).map_err(error)?;
        let effective = grants.iter().filter(|g| {
            g.is_effective_at(Timestamp::now()) && g.applies_to(&p.origin, p.job_id, None)
        });
        let mut covered = None;
        for grant in effective {
            if grant.scope == GrantScope::Never {
                return Ok(SessionsRequestResult {
                    replayed: false,
                    status: SessionRequestStatus::Denied,
                    grant_id: None,
                    approval_id: None,
                    expires_at: None,
                });
            }
            covered = Some(grant.id);
        }
        if let Some(id) = covered {
            return Ok(SessionsRequestResult {
                replayed: false,
                status: SessionRequestStatus::AlreadyGranted,
                grant_id: Some(id),
                approval_id: None,
                expires_at: None,
            });
        }
        let pending = self
            .journal
            .read(|tx| tx.approvals(Some(&[ApprovalState::Pending])))
            .map_err(error)?;
        // Exact repeats reuse the still-bound request; another job never silently joins an old revision.
        if let Some(a) = pending.iter().find(|a| {
            a.origin.as_ref() == Some(&p.origin)
                && a.requested_by.as_deref() == Some(identity.credential_id.as_str())
                && a.job_id == p.job_id
                && a.is_open_at(Timestamp::now())
        }) {
            return Ok(SessionsRequestResult {
                replayed: false,
                status: SessionRequestStatus::PendingUserApproval,
                grant_id: None,
                approval_id: Some(a.id),
                expires_at: Some(a.expires_at),
            });
        }
        crate::approvals::check_pending_cap(pending.len())?;
        let a = self.prepare(ApprovalPreparation {
            requester: identity.credential_id.as_str(),
            origin: p.origin,
            job: job.as_ref().map(|r| &r.job),
            dest_path: job.as_ref().map(|r| r.internal.dest_path.as_str()),
            kind: ApprovalKind::SessionGrant,
            reason: &p.reason,
            reason_codes: &[],
        })?;
        self.store(&a)?;
        Ok(SessionsRequestResult {
            replayed: false,
            status: SessionRequestStatus::PendingUserApproval,
            grant_id: None,
            approval_id: Some(a.id),
            expires_at: Some(a.expires_at),
        })
    }
    /// Bind one completed dry run to an exact plan and durable run before any workflow execution.
    pub async fn request_workflow(
        &self,
        plan_id: &zeus_workflow::PlanId,
        run_id: RunId,
        plan_hash: &str,
        plan: &zeus_workflow::Plan,
        dry_run: &zeus_workflow::DryRunReport,
        requester: &ClientIdentity,
    ) -> ApiResult<ApprovalView> {
        let _gate = self.gate.lock().await;
        if !dry_run.complete
            || dry_run.plan_hash != plan_hash
            || plan
                .plan_hash()
                .map_err(|_| ApiError::invalid_params("invalid plan"))?
                != plan_hash
        {
            return Err(ApiError::invalid_params(
                "workflow requires a complete dry run for this exact plan",
            ));
        }
        let credential = self.credential(requester.credential_id.as_str())?;
        let pending = self
            .journal
            .read(|tx| tx.approvals(Some(&[ApprovalState::Pending])))
            .map_err(error)?;
        let existing = self.journal.read(|tx| tx.approvals(None)).map_err(error)?;
        if let Some(a) = existing.iter().find(|a| {
            a.run_id == Some(run_id)
                && a.requested_by.as_deref() == Some(requester.credential_id.as_str())
        }) {
            if a.facts
                .as_ref()
                .and_then(|f| f.get("plan_blake3"))
                .and_then(|v| v.as_str())
                != Some(plan_hash)
            {
                return Err(ApiError::invalid_params(
                    "run already binds a different plan",
                ));
            }
            return a.clone().try_into();
        }
        crate::approvals::check_pending_cap(pending.len())?;
        let id = IdGenerator::new().approval_id();
        let now = Timestamp::now();
        let expires = now.saturating_add_ms(7 * 86_400_000);
        let scopes = if plan.session.is_some() {
            vec![GrantScope::Workflow]
        } else {
            Vec::new()
        };
        let origin = plan.session.as_ref().map(|s| s.origin.clone());
        let estimate = match (&dry_run.estimate_files, &dry_run.estimate_bytes) {
            (Some(files), Some(bytes)) => {
                serde_json::json!({"files_low":files.low,"files_high":files.high,"bytes_low":bytes.low,"bytes_high":bytes.high})
            }
            _ => serde_json::Value::Null,
        };
        let label = match credential.role {
            zeus_api::ClientRole::Cli | zeus_api::ClientRole::Tui => "cli",
            _ => "agent",
        };
        let facts = serde_json::json!({"t":"zeus.approval.facts.v1","approval_id":id,"revision":1,"kind":"workflow_plan",
            "requester":{"kind":credential.kind,"role":credential.role,"client_id":requester.credential_id.as_str(),"label":label,"registered_at":credential.created_at},
            "issued_at":now,"expires_at":expires,"allowed_scopes":scopes,"confirmation":if scopes.is_empty(){serde_json::json!({"approve":"typed_code"})}else{serde_json::json!({"workflow":"typed_code"})},
            "reason_codes":["workflow.plan_ready"],"agent_says_blake3":null,"plan_id":plan_id,"plan_blake3":plan_hash,"run_id":run_id,"renewal_of":null,
            "untrusted_title":clean(&plan.name),"allowed_domains":plan.allowed_domains.iter().take(20).collect::<Vec<_>>(),"allowed_domains_omitted":plan.allowed_domains.len().saturating_sub(20),
            "session_origin":origin,"budgets":plan.budgets,"pace":{"min_interval_ms":plan.fetch.min_interval_ms,"max_concurrency_per_host":plan.fetch.max_concurrency_per_host},
            "robots_txt":"obey","dest_dir":plan.sinks.dest_dir,"estimate":estimate});
        let a: Approval = serde_json::from_value(serde_json::json!({"id":id,"kind":"workflow_plan","state":"pending","run_id":run_id,"origin":origin,
            "allowed_scopes":scopes,"reason_codes":["workflow.plan_ready"],"summary":{"title":"Run approved download plan"},"created_at":now,"expires_at":expires,
            "request_hash":crate::approvals::facts_hash(&facts)?,"requested_by":requester.credential_id.as_str(),"revision":1,"facts":facts}))
            .map_err(|_|ApiError::internal("cannot bind workflow approval"))?;
        self.store(&a)?;
        a.try_into()
    }
    /// Verify that the requesting credential has not been revoked between planning and execution.
    pub(crate) fn requester_active(&self, id: &str) -> bool {
        self.credential(id).is_ok()
    }

    /// Bind one immutable cloud plan; this approval never creates a website-session grant.
    pub async fn request_cloud(
        &self,
        plan: &zeus_api::methods::cloud::CloudPlanView,
        requester: &str,
    ) -> ApiResult<ApprovalView> {
        let _gate = self.gate.lock().await;
        let credential = self.credential(requester)?;
        for approval in self.journal.read(|tx| tx.approvals(None)).map_err(error)? {
            if approval.kind == ApprovalKind::CloudDelivery
                && approval.requested_by.as_deref() == Some(requester)
                && approval
                    .facts
                    .as_ref()
                    .and_then(|f| f.get("delivery_id"))
                    .and_then(|v| v.as_str())
                    == Some(plan.delivery_id.as_str())
            {
                if approval
                    .facts
                    .as_ref()
                    .and_then(|f| f.get("plan_blake3"))
                    .and_then(|v| v.as_str())
                    != Some(plan.plan_hash.to_string().as_str())
                {
                    return Err(ApiError::new(
                        "api.conflict",
                        "delivery already has a different approval binding",
                    ));
                }
                return approval.try_into();
            }
        }
        let pending = self
            .journal
            .read(|tx| tx.approvals(Some(&[ApprovalState::Pending])))
            .map_err(error)?;
        crate::approvals::check_pending_cap(pending.len())?;
        let now = Timestamp::now();
        let id = IdGenerator::new().approval_id();
        let facts = serde_json::json!({"t":"zeus.approval.facts.v1","approval_id":id,"revision":1,"kind":"cloud_delivery",
            "requester":{"kind":credential.kind,"role":credential.role,"client_id":requester,"label":if matches!(credential.role,zeus_api::ClientRole::Cli|zeus_api::ClientRole::Tui){"cli"}else{"agent"},"registered_at":credential.created_at},
            "issued_at":now,"expires_at":plan.expires_at,"allowed_scopes":[],"confirmation":{"approve":"typed_code"},
            "reason_codes":["cloud.delivery_requested"],"agent_says_blake3":null,"delivery_id":plan.delivery_id,"plan_blake3":plan.plan_hash,"plan":plan,
            "shares_full_source_url_with_provider":plan.mode==zeus_cloud::DeliveryMode::RemoteImport,
            "remote_byte_cap_enforced":plan.hard_byte_cap,"remote_cancellation_supported":plan.remote_cancellation_supported});
        let approval:Approval=serde_json::from_value(serde_json::json!({"id":id,"kind":"cloud_delivery","state":"pending",
            "allowed_scopes":[],"reason_codes":["cloud.delivery_requested"],"summary":{"title":"Deliver to configured cloud destination","total_bytes":plan.size_bytes},
            "created_at":now,"expires_at":plan.expires_at,"request_hash":crate::approvals::facts_hash(&facts)?,"requested_by":requester,"revision":1,"facts":facts}))
            .map_err(|_|ApiError::internal("cannot bind cloud approval"))?;
        self.store(&approval)?;
        approval.try_into()
    }
    /// Commit one independently attested human decision. The caller verifies terminal/browser evidence first.
    pub(crate) async fn decide(
        &self,
        identity: &ClientIdentity,
        p: &ApprovalsDecideParams,
        evidence: serde_json::Value,
    ) -> ApiResult<(Approval, Option<SessionGrant>)> {
        let _gate = self.gate.lock().await;
        self.decisions
            .lock()
            .await
            .try_acquire(self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)?;
        if evidence["client_id"] != identity.credential_id.as_str()
            || evidence["surface"] != serde_json::json!(p.channel)
            || evidence["exe_verified"] != true
        {
            return Err(ApiError::human_required("independent channel evidence"));
        }
        if p.scope == Some(GrantScope::Never) && !p.channel.is_approving_surface() {
            return Err(ApiError::human_required(
                "approving surface for never policy",
            ));
        }
        let mut a = self.get(identity, p.approval_id)?;
        if a.kind == ApprovalKind::CloudDelivery
            && !matches!(p.channel, Surface::CliInteractive | Surface::Tui)
        {
            return Err(ApiError::human_required(
                "cloud delivery requires a verified physical terminal",
            ));
        }
        let view = ApprovalView::try_from(a.clone())?;
        if crate::approvals::facts_hash(&view.facts)? != view.request_hash {
            return Err(ApiError::new(
                "policy.request_hash_mismatch",
                "stored approval binding is invalid",
            ));
        }
        let validated = crate::approvals::validate_decision(&view, p, identity, Timestamp::now())?;
        if p.outcome == DecideOutcome::Approve && !p.channel.is_approving_surface() {
            return Err(ApiError::human_required("approval surface"));
        }
        let key = p
            .scope
            .map(|s| {
                serde_json::to_value(s)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .unwrap_or_else(|| "approve".into());
        if p.outcome == DecideOutcome::Approve
            && view.facts["confirmation"][key] == "typed_code"
            && p.confirmation != ConfirmationLevel::TypedCode
        {
            return Err(ApiError::human_required("typed binding code"));
        }
        if p.confirmation == ConfirmationLevel::OsAuth {
            return Err(ApiError::human_required("OS authentication is unavailable"));
        }
        if !matches!(
            a.kind,
            ApprovalKind::SessionGrant
                | ApprovalKind::Handoff
                | ApprovalKind::WorkflowPlan
                | ApprovalKind::CloudDelivery
        ) {
            return Err(ApiError::not_implemented("this approval kind"));
        }
        let now = Timestamp::now();
        let grant = validated.scope.map(|scope| SessionGrant {
            id: IdGenerator::new().grant_id(),
            origin: a.origin.clone().expect("session approval origin"),
            scope,
            state: GrantState::Active,
            created_at: now,
            expires_at: scope.lifetime_ms().map(|ms| now.saturating_add_ms(ms)),
            job_id: if scope == GrantScope::Once {
                a.job_id
            } else {
                None
            },
            run_id: if scope == GrantScope::Workflow {
                a.run_id
            } else {
                None
            },
            approval_id: a.id,
            decided_by: validated.actor.clone(),
            uses: 0,
            last_used_at: None,
            revoked_at: None,
            renewable: scope == GrantScope::Workflow,
            renewal_count: 0,
            renewed_at: None,
            revoked_by: None,
            revocation_reason: None,
        });
        let mut entry = AuditEntry::new(
            now,
            validated.actor.clone(),
            "approval.decided",
            AuditOutcome::Ok,
        );
        entry.subject.approval_id = Some(a.id);
        entry
            .data
            .insert("request_hash".into(), serde_json::json!(view.request_hash));
        entry.data.insert("evidence".into(), evidence.clone());
        entry
            .data
            .insert("outcome".into(), serde_json::json!(validated.outcome));
        entry
            .data
            .insert("confirmation".into(), serde_json::json!(p.confirmation));
        entry.write_ahead = true;
        self.audit.append(entry).map_err(error)?;
        if let Some(grant) = &grant {
            let mut entry = AuditEntry::new(
                now,
                validated.actor.clone(),
                "grant.created",
                AuditOutcome::Ok,
            );
            entry.subject.grant_id = Some(grant.id);
            entry.subject.approval_id = Some(a.id);
            entry.subject.origin = Some(grant.origin.clone());
            entry
                .data
                .insert("scope".into(), serde_json::json!(grant.scope));
            self.audit.append(entry).map_err(error)?;
        }
        a.state = if p.outcome == DecideOutcome::Approve {
            ApprovalState::Approved
        } else {
            ApprovalState::Denied
        };
        a.decided_at = Some(now);
        a.decision = Some(zeus_types::Decision {
            outcome: validated.outcome,
            scope: validated.scope,
            actor: validated.actor.clone(),
            grant_id: grant.as_ref().map(|g| g.id),
        });
        let mut record = serde_json::json!({"approval_id":a.id,"revision":a.revision,"request_hash":a.request_hash,"outcome":validated.outcome,
            "scope":validated.scope,"actor":validated.actor,"decided_at":now,"grant_ids":grant.iter().map(|g|g.id).collect::<Vec<_>>(),"evidence":evidence});
        if validated.scope.is_none() {
            record
                .as_object_mut()
                .ok_or_else(|| ApiError::internal("invalid decision record"))?
                .remove("scope");
        }
        self.journal
            .transact(|tx| {
                let current = tx
                    .approval(a.id)?
                    .ok_or(zeus_journal::JournalError::Invariant("approval missing"))?;
                if p.outcome == DecideOutcome::Approve && tx.meta()?.kill_switch_engaged() {
                    return Err(zeus_journal::JournalError::Invariant("kill switch engaged"));
                }
                if current.state != ApprovalState::Pending || current.request_hash != a.request_hash
                {
                    return Err(zeus_journal::JournalError::Invariant("approval changed"));
                }
                tx.put_approval(&a)?;
                tx.put_approval_decision(a.id, &record)?;
                if let Some(grant) = &grant {
                    tx.put_grant(grant)?;
                    tx.append_event(
                        now,
                        None,
                        a.run_id,
                        zeus_types::event::GrantCreatedData {
                            grant: grant.clone(),
                        }
                        .into(),
                    )?;
                }
                tx.append_event(
                    now,
                    None,
                    a.run_id,
                    zeus_types::event::ApprovalDecidedData {
                        approval_id: a.id,
                        kind: a.kind,
                        outcome: validated.outcome,
                        scope: validated.scope,
                        actor: validated.actor,
                        grant_id: grant.as_ref().map(|g| g.id),
                    }
                    .into(),
                )?;
                Ok(())
            })
            .map_err(error)?;
        Ok((a, grant))
    }
}

#[async_trait]
impl ApprovalPort for ConsentManager {
    async fn request(&self, request: ApprovalRequest) -> Result<Approval, ErrorEnvelope> {
        let _gate = self.gate.lock().await;
        let result = (|| {
            let pending = self
                .journal
                .read(|tx| tx.approvals(Some(&[ApprovalState::Pending])))
                .map_err(error)?;
            if !request.reason_codes.iter().any(|r| r == "kill_switch") {
                crate::approvals::check_pending_cap(pending.len())?;
            }
            let requester = request
                .summary
                .requested_by
                .as_deref()
                .ok_or_else(missing)?;
            let a = self.prepare(ApprovalPreparation {
                requester,
                origin: request.origin.ok_or_else(missing)?,
                job: request.job.as_ref(),
                dest_path: request.dest_path.as_deref(),
                kind: request.kind,
                reason: "",
                reason_codes: &request.reason_codes,
            })?;
            // The engine persists the approval together with its newly created job and events.
            Ok(a)
        })();
        result.map_err(|e: ApiError| e.envelope().clone())
    }
    async fn withdraw(&self, id: ApprovalId) -> Result<(), ErrorEnvelope> {
        self.journal
            .transact(|tx| {
                if let Some(mut approval) = tx.approval(id)?
                    && approval.state == ApprovalState::Pending
                {
                    let now = Timestamp::now();
                    approval.state = ApprovalState::Canceled;
                    approval.decided_at = Some(now);
                    approval.decision = Some(Decision {
                        outcome: DecisionOutcome::Cancel,
                        scope: None,
                        actor: Actor::system(),
                        grant_id: None,
                    });
                    tx.put_approval(&approval)?;
                    if approval.revision.is_some() && approval.request_hash.is_some() {
                        tx.put_approval_decision(
                            id,
                            &serde_json::json!({
                                "approval_id": id,
                                "revision": approval.revision,
                                "request_hash": approval.request_hash,
                                "outcome": "cancel",
                                "actor": Actor::system(),
                                "decided_at": now,
                                "grant_ids": [],
                                "system_reason": "request_withdrawn"
                            }),
                        )?;
                    }
                    tx.append_event(
                        now,
                        approval.job_id,
                        approval.run_id,
                        zeus_types::event::ApprovalDecidedData {
                            approval_id: id,
                            kind: approval.kind,
                            outcome: DecisionOutcome::Cancel,
                            scope: None,
                            actor: Actor::system(),
                            grant_id: None,
                        }
                        .into(),
                    )?;
                }
                Ok(())
            })
            .map_err(|error| error.to_envelope())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeus_api::{
        ClientRole,
        auth::{Credential, MemoryCredentialStore, Token},
    };
    fn setup(path: &std::path::Path) -> (ConsentManager, ClientIdentity, ClientIdentity) {
        let clock = Arc::new(zeus_control::SystemClock::new());
        let journal = Journal::open(&path.join("state.redb"), &Default::default(), clock).unwrap();
        let credentials = Arc::new(MemoryCredentialStore::default());
        let identity = |kind, role| {
            let id = CredentialId::generate();
            credentials.insert(
                Credential::new(
                    id.clone(),
                    kind,
                    role,
                    Token::generate().auth_key(),
                    Timestamp::now(),
                )
                .unwrap(),
            );
            ClientIdentity {
                credential_id: id,
                kind,
                role,
                permissions: kind.default_permissions(),
                client_name: "untrusted forged name".into(),
            }
        };
        let agent = identity(ClientKind::Agent, ClientRole::Mcp);
        let human = identity(ClientKind::Human, ClientRole::Cli);
        let audit = zeus_verify::audit::AuditLog::open(
            &path.join("audit"),
            None,
            journal.meta().unwrap().store_id,
            "0.1.0",
            Default::default(),
        )
        .unwrap();
        (
            ConsentManager::new(journal, Arc::new(audit), credentials),
            agent,
            human,
        )
    }
    fn evidence(identity: &ClientIdentity, p: &ApprovalsDecideParams) -> serde_json::Value {
        serde_json::json!({"client_id":identity.credential_id.as_str(),"surface":p.channel,"exe_verified":true,"credential_kind":identity.kind,
            "effective_kind":identity.kind,"role":identity.role,"tty":"verified","confirmation":p.confirmation,"peer_pid":123})
    }
    fn request() -> SessionsRequestParams {
        SessionsRequestParams {
            origin: Origin::parse("https://example.org").unwrap(),
            scope: zeus_types::job::GrantScopeHint::Site,
            reason: "Need files\u{202e}\nplease".into(),
            job_id: None,
            run_id: None,
            idempotency_key: None,
        }
    }
    #[tokio::test]
    async fn session_requests_are_bound_isolated_and_do_not_grant() {
        let temp = tempfile::tempdir().unwrap();
        let (manager, agent, _) = setup(temp.path());
        let first = manager.request_session(&agent, request()).await.unwrap();
        let second = manager.request_session(&agent, request()).await.unwrap();
        assert_eq!(first.approval_id, second.approval_id);
        let a = manager.get(&agent, first.approval_id.unwrap()).unwrap();
        let view = ApprovalView::try_from(a).unwrap();
        assert_eq!(
            view.request_hash,
            crate::approvals::facts_hash(&view.facts).unwrap()
        );
        zeus_types::spec_testing::SpecSchemas::load()
            .check(
                true,
                "approvals.v1",
                "ApprovalRequest",
                &serde_json::to_value(&view).unwrap(),
            )
            .unwrap();
        assert_eq!(view.facts["requester"]["label"], "agent");
        assert_eq!(view.agent_says.as_deref(), Some("Need files please"));
        assert!(manager.journal.read(|tx| tx.grants()).unwrap().is_empty());
        let mut other = agent;
        other.credential_id = CredentialId::generate();
        assert!(manager.get(&other, view.approval.id).is_err());
    }
    #[tokio::test]
    async fn stale_hash_agent_decision_and_replay_cannot_create_authority() {
        let temp = tempfile::tempdir().unwrap();
        let (manager, agent, human) = setup(temp.path());
        let id = manager
            .request_session(&agent, request())
            .await
            .unwrap()
            .approval_id
            .unwrap();
        let a = manager.get(&human, id).unwrap();
        let mut p = ApprovalsDecideParams {
            approval_id: id,
            request_hash: a.request_hash.unwrap(),
            outcome: DecideOutcome::Approve,
            scope: Some(GrantScope::Site),
            channel: Surface::CliInteractive,
            confirmation: ConfirmationLevel::Gesture,
            idempotency_key: None,
        };
        assert!(
            manager
                .decide(&agent, &p, evidence(&agent, &p))
                .await
                .is_err()
        );
        let correct = p.request_hash;
        p.request_hash =
            crate::approvals::facts_hash(&serde_json::json!({"different":true})).unwrap();
        assert!(
            manager
                .decide(&human, &p, evidence(&human, &p))
                .await
                .is_err()
        );
        p.request_hash = correct;
        let (a, grant) = manager
            .decide(&human, &p, evidence(&human, &p))
            .await
            .unwrap();
        let record = manager
            .journal
            .read(|tx| tx.approval_decision(id))
            .unwrap()
            .unwrap();
        zeus_types::spec_testing::SpecSchemas::load()
            .check(true, "approvals.v1", "DecisionRecord", &record)
            .unwrap();
        assert_eq!(a.state, ApprovalState::Approved);
        assert_eq!(grant.unwrap().scope, GrantScope::Site);
        assert!(
            manager
                .decide(&human, &p, evidence(&human, &p))
                .await
                .is_err()
        );
        assert_eq!(manager.journal.read(|tx| tx.grants()).unwrap().len(), 1);
    }
}
