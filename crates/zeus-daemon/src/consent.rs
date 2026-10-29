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

#[path = "consent_artifacts.rs"]
mod artifact_lifecycle;
#[path = "consent_browser.rs"]
mod browser_review;

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

/// Daemon-computed facts of one file operation list (07 `FactsDestructiveOp`).
pub(crate) struct DestructiveFacts {
    pub operation_set_blake3: Hex256,
    pub counts: serde_json::Value,
    pub total_bytes: u64,
    pub quarantined_count: u64,
    pub sample: Vec<serde_json::Value>,
    pub sample_omitted: u64,
    /// Summary title built from the canonical directory (a measured path, not requester text).
    pub title: String,
    /// The list sends files to the trash.
    pub deletes: bool,
}

tokio::task_local! {
    /// The effective kind of the connection whose call is running (05 section 7.4), so approvals the engine raises
    /// on its behalf apply the denial back-off to a downgraded credential (07 section 9.4).
    pub(crate) static REQUESTER_KIND: ClientKind;
}

/// Owns serialized approval mutations and the write-ahead audit boundary.
pub struct ConsentManager {
    journal: Journal,
    audit: Arc<dyn AuditSink>,
    credentials: Arc<dyn CredentialStore>,
    gate: Mutex<()>,
    decisions: Mutex<crate::approvals::DecisionLimiter>,
    limits: crate::approvals::ConsentLimits,
    started: std::time::Instant,
    /// The router's token buckets, which also take the `session_request` token of enqueued jobs that raise a prompt.
    rates: std::sync::OnceLock<Arc<zeus_api::rate::RateLimiter>>,
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
            limits: Default::default(),
            started: std::time::Instant::now(),
            rates: std::sync::OnceLock::new(),
        }
    }
    /// Share the router's token buckets (05 section 13.2); the first call wins.
    pub fn share_rate_limiter(&self, rates: Arc<zeus_api::rate::RateLimiter>) {
        let _ = self.rates.set(rates);
    }
    /// Apply configured caps and the approve-decision rate (07 section 9).
    pub fn with_limits(mut self, limits: crate::approvals::ConsentLimits) -> Self {
        self.decisions = Mutex::new(crate::approvals::DecisionLimiter::new(
            limits.decisions_per_minute,
            60_000,
        ));
        self.limits = limits;
        self
    }
    /// End every pending approval `requester` raised as `cancel` by `system` (05 section 7.6 rule 8), serialised
    /// with decisions and batch joins.
    pub(crate) async fn withdraw_requester(&self, requester: &str) -> ApiResult<()> {
        let _gate = self.gate.lock().await;
        let now = Timestamp::now();
        self.journal
            .transact(|tx| {
                for approval in tx.approvals(Some(&[ApprovalState::Pending]))? {
                    if approval.requested_by.as_deref() == Some(requester) {
                        cancel_pending(tx, approval, "requester_revoked", now)?;
                    }
                }
                Ok(())
            })
            .map_err(error)
    }
    /// Denial back-off (07 section 9.4). Derived from durable decisions, so it survives a restart: after a human
    /// denies an unattended requester a session for an origin, new session requests by that requester for that
    /// origin are refused for 10 minutes, or 24 hours after three denials within a day. A later approval for the
    /// same pair clears it. Human-capable requesters and handoffs are never backed off. `effective` is the kind
    /// of the connection making the request, which is `agent` for a downgraded human or admin credential; without
    /// a connection (approvals the engine raises on its own) the credential's kind decides.
    pub(crate) fn check_denial_backoff(
        &self,
        requester: &str,
        effective: Option<ClientKind>,
        origin: &Origin,
        now: Timestamp,
    ) -> ApiResult<()> {
        const SHORT_MS: u64 = 10 * 60_000;
        const DAY_MS: u64 = 86_400_000;
        let kind = match effective {
            Some(kind) => kind,
            None => match self.credential(requester) {
                Ok(credential) => credential.kind,
                Err(_) => return Ok(()),
            },
        };
        if matches!(kind, ClientKind::Human | ClientKind::Admin) {
            return Ok(());
        }
        let decided = self
            .journal
            .read(|tx| tx.approvals(Some(&[ApprovalState::Approved, ApprovalState::Denied])))
            .map_err(error)?;
        let mut last_approved = None;
        let mut denials = Vec::new();
        for a in decided.iter().filter(|a| {
            a.kind == ApprovalKind::SessionGrant
                && a.requested_by.as_deref() == Some(requester)
                && a.origin.as_ref() == Some(origin)
        }) {
            let (Some(at), Some(decision)) = (a.decided_at, a.decision.as_ref()) else {
                continue;
            };
            match a.state {
                ApprovalState::Approved => last_approved = last_approved.max(Some(at)),
                ApprovalState::Denied if decision.actor.kind == ActorKind::Human => {
                    denials.push(at)
                }
                _ => {}
            }
        }
        let recent: Vec<_> = denials
            .into_iter()
            .filter(|at| last_approved.is_none_or(|approved| *at > approved))
            .filter(|at| {
                now.duration_since(*at)
                    .is_none_or(|d| d.as_millis() < u128::from(DAY_MS))
            })
            .collect();
        let Some(last) = recent.iter().max().copied() else {
            return Ok(());
        };
        let until = last.saturating_add_ms(if recent.len() >= 3 { DAY_MS } else { SHORT_MS });
        if now < until {
            return Err(ApiError::new(
                codes::POLICY_RECENTLY_DENIED,
                "a person recently denied this requester access to the site; ask them directly",
            )
            .with_detail("until", until.to_rfc3339()));
        }
        Ok(())
    }
    fn check_caps(&self, pending: &[Approval], requester: Option<&str>) -> ApiResult<()> {
        let mine = requester.map(|id| {
            pending
                .iter()
                .filter(|a| a.requested_by.as_deref() == Some(id))
                .count()
        });
        crate::approvals::check_pending_cap(pending.len(), mine, &self.limits)
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
        if !identity.kind.is_human_capable()
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
                    && (identity.kind.is_human_capable()
                        || a.requested_by.as_deref() == Some(identity.credential_id.as_str()))
            })
            .map(ApprovalView::try_from)
            .collect()
    }
    /// Join a new job to a compatible pending session or browser handoff approval,
    /// as a new revision with recomputed facts (07 section 4.7). Returns `None` when there is none or it is full.
    /// Callers hold the engine's admission gate, so two joins never race on one revision; the engine writes the
    /// revision only while the stored approval is still pending at the one read here, so a decision committed in
    /// between wins.
    fn join_batch(
        &self,
        pending: &[Approval],
        requester: &str,
        origin: &Origin,
        job: &Job,
        dest_path: Option<&str>,
        reason_codes: &[String],
    ) -> ApiResult<Option<Approval>> {
        const MAX_JOBS: usize = 50;
        const MAX_ITEMS: usize = 20;
        const MAX_DEST_DIRS: usize = 8;
        let now = Timestamp::now();
        let Some(mut approval) = pending
            .iter()
            .find(|a| {
                let kind = if job.spec.source.kind == SourceKind::BrowserHandoff {
                    ApprovalKind::Handoff
                } else {
                    ApprovalKind::SessionGrant
                };
                let same_browser = kind != ApprovalKind::Handoff
                    || job.spec.source.browser.as_ref().is_some_and(|browser| {
                        browser.profile_id.as_ref().is_some_and(|profile| {
                            !profile.is_empty()
                                && a.facts.as_ref().is_some_and(|facts| {
                                    facts["browser"]["profile_id"] == profile.as_str()
                                        && facts["browser"]["name"]
                                            == serde_json::json!(browser.name)
                                        && facts["browser"]["incognito"] == browser.incognito
                                })
                        })
                    });
                a.kind == kind
                    && same_browser
                    && a.requested_by.as_deref() == Some(requester)
                    && a.origin.as_ref() == Some(origin)
                    && a.run_id == job.spec.source.run_id
                    && a.is_open_at(now)
                    && a.facts
                        .as_ref()
                        .and_then(|f| f.get("transfer"))
                        .is_some_and(serde_json::Value::is_object)
            })
            .cloned()
        else {
            return Ok(None);
        };
        let mut facts = serde_json::Value::Object(approval.facts.clone().ok_or_else(missing)?);
        // A retry of an already-covered offer cannot inflate the batch or invalidate an in-flight review.
        if facts["transfer"]["job_ids"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| *id == serde_json::json!(job.id)))
        {
            return Ok(Some(approval));
        }
        let revision = approval.revision.unwrap_or(1).saturating_add(1);
        let ids = facts["transfer"]["job_ids"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(0);
        if ids >= MAX_JOBS || revision > 1000 {
            return Ok(None);
        }
        let transfer = &mut facts["transfer"];
        transfer["job_ids"]
            .as_array_mut()
            .ok_or_else(missing)?
            .push(serde_json::json!(job.id));
        match job.spec.expected_size {
            Some(size) => {
                let known = transfer["total_bytes"].as_u64().unwrap_or(0);
                transfer["total_bytes"] = serde_json::json!(known.saturating_add(size));
            }
            None => {
                let unknown = transfer["unknown_size_count"].as_u64().unwrap_or(0);
                transfer["unknown_size_count"] = serde_json::json!(unknown + 1);
            }
        }
        if let Some(dir) = dest_path
            .and_then(|p| std::path::Path::new(p).parent())
            .map(|p| p.to_string_lossy().into_owned())
        {
            let dirs = transfer["dest_dirs"].as_array_mut().ok_or_else(missing)?;
            if !dirs.iter().any(|d| d.as_str() == Some(dir.as_str())) {
                if dirs.len() < MAX_DEST_DIRS {
                    dirs.push(serde_json::json!(dir));
                } else {
                    let omitted = transfer["dest_dirs_omitted"].as_u64().unwrap_or(0);
                    transfer["dest_dirs_omitted"] = serde_json::json!(omitted + 1);
                }
            }
        }
        let items = facts["items"].as_array_mut().ok_or_else(missing)?;
        if items.len() < MAX_ITEMS {
            items.push(serde_json::json!({"job_id":job.id,
                "untrusted_name":clean(job.spec.filename.as_deref().unwrap_or("download")),
                "untrusted_url":job.spec.url.redact(RedactMode::Log),"total_bytes":job.spec.expected_size}));
        } else {
            let omitted = facts["items_omitted"].as_u64().unwrap_or(0);
            facts["items_omitted"] = serde_json::json!(omitted + 1);
        }
        for code in reason_codes {
            if approval.reason_codes.len() < 8 && !approval.reason_codes.contains(code) {
                approval.reason_codes.push(code.clone());
            }
        }
        facts["reason_codes"] = serde_json::json!(approval.reason_codes);
        facts["revision"] = serde_json::json!(revision);
        facts["issued_at"] = serde_json::json!(now);
        approval.summary.total_bytes = facts["transfer"]["total_bytes"].as_u64();
        approval.revision = Some(revision);
        approval.request_hash = Some(crate::approvals::facts_hash(&facts)?);
        approval.facts = facts.as_object().cloned();
        // `expires_at` never changes: joining cannot extend a deadline.
        Ok(Some(approval))
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
            facts["run_id"] = serde_json::json!(job.and_then(|j| j.spec.source.run_id));
        }
        let request_hash = crate::approvals::facts_hash(&facts)?;
        serde_json::from_value(serde_json::json!({"id":id,"kind":kind,"state":"pending","job_id":job.map(|j|j.id),"run_id":job.and_then(|j|j.spec.source.run_id),"origin":origin,
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
        self.check_denial_backoff(
            identity.credential_id.as_str(),
            Some(identity.kind),
            &p.origin,
            Timestamp::now(),
        )?;
        self.check_caps(&pending, Some(identity.credential_id.as_str()))?;
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
        self.check_caps(&pending, Some(requester.credential_id.as_str()))?;
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
            "session_origin":origin,"browser_document_mode":plan.session.as_ref().and_then(|s| s.document_mode),"browser_profile_id":plan.session.as_ref().and_then(|s| s.profile_id.as_deref()),
            "browser_network":plan.session.as_ref().and_then(|s| s.document_mode).map(|_| "Browser resolves and connects under its own network policy after public DNS preflight. Native peer pinning is unavailable; redirects are denied."),"budgets":plan.budgets,"pace":{"min_interval_ms":plan.fetch.min_interval_ms,"max_concurrency_per_host":plan.fetch.max_concurrency_per_host},
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

    /// Raise a `destructive_op` approval for one stored operation list (07 section 4.4). The approval creates no
    /// grant; approving runs exactly the list whose hash is `operation_set_blake3`.
    pub(crate) async fn request_destructive(
        &self,
        requester: &ClientIdentity,
        facts: DestructiveFacts,
    ) -> ApiResult<ApprovalId> {
        let _gate = self.gate.lock().await;
        let credential = self.credential(requester.credential_id.as_str())?;
        let pending = self
            .journal
            .read(|tx| tx.approvals(Some(&[ApprovalState::Pending])))
            .map_err(error)?;
        self.check_caps(&pending, Some(requester.credential_id.as_str()))?;
        let now = Timestamp::now();
        let expires = now.saturating_add_ms(86_400_000);
        let id = IdGenerator::new().approval_id();
        let label = match credential.role {
            zeus_api::ClientRole::Cli | zeus_api::ClientRole::Tui => "cli",
            _ => "agent",
        };
        let mut reasons = Vec::new();
        if facts.deletes {
            reasons.push("files.delete");
        }
        reasons.push("files.move");
        let value = serde_json::json!({"t":"zeus.approval.facts.v1","approval_id":id,"revision":1,"kind":"destructive_op",
            "requester":{"kind":credential.kind,"role":credential.role,"client_id":requester.credential_id.as_str(),"label":label,"registered_at":credential.created_at},
            "issued_at":now,"expires_at":expires,"allowed_scopes":[],"confirmation":{"approve":"typed_code"},
            "reason_codes":reasons,"agent_says_blake3":null,"operation_set_blake3":facts.operation_set_blake3,"plan_id":null,
            "counts":facts.counts,"total_bytes":facts.total_bytes,"to_trash":true,"quarantined_count":facts.quarantined_count,
            "sample":facts.sample,"sample_omitted":facts.sample_omitted});
        let approval: Approval = serde_json::from_value(serde_json::json!({"id":id,"kind":"destructive_op","state":"pending",
            "allowed_scopes":[],"reason_codes":reasons,"summary":{"title":clean(&facts.title),"total_bytes":facts.total_bytes},
            "created_at":now,"expires_at":expires,"request_hash":crate::approvals::facts_hash(&value)?,
            "requested_by":requester.credential_id.as_str(),"revision":1,"facts":value}))
            .map_err(|_| ApiError::internal("cannot bind file operation approval"))?;
        self.store(&approval)?;
        Ok(id)
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
        self.check_caps(&pending, Some(requester))?;
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
        // Browser surfaces do not render these facts yet, so they fail closed (07 section 4.4).
        if matches!(
            a.kind,
            ApprovalKind::CloudDelivery | ApprovalKind::DestructiveOp
        ) && !matches!(p.channel, Surface::CliInteractive | Surface::Tui)
        {
            return Err(ApiError::human_required(
                "this approval requires a verified physical terminal",
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
        let browser_network_review = a.kind == ApprovalKind::PolicyException
            && a.facts.as_ref().is_some_and(|f| {
                f.get("rule").and_then(|v| v.as_str()) == Some("browser_managed_network")
            });
        if !browser_network_review
            && !matches!(
                a.kind,
                ApprovalKind::SessionGrant
                    | ApprovalKind::Handoff
                    | ApprovalKind::WorkflowPlan
                    | ApprovalKind::CloudDelivery
                    | ApprovalKind::DestructiveOp
            )
        {
            return Err(ApiError::not_implemented("this approval kind"));
        }
        // Only valid approve decisions count against the daemon-wide rate; a flood can always be denied (07 section 9.2).
        if validated.outcome == DecisionOutcome::Approve {
            self.decisions
                .lock()
                .await
                .try_acquire(self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)?;
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
        // A batched `once` approval creates one grant per covered job (07 section 4.7 rule 5).
        let extra: Vec<SessionGrant> = match (&grant, view.facts["transfer"]["job_ids"].as_array())
        {
            (Some(first), Some(ids)) if first.scope == GrantScope::Once => ids
                .iter()
                .filter_map(|v| v.as_str()?.parse::<JobId>().ok())
                .filter(|id| Some(*id) != a.job_id)
                .map(|id| SessionGrant {
                    id: IdGenerator::new().grant_id(),
                    job_id: Some(id),
                    ..first.clone()
                })
                .collect(),
            _ => Vec::new(),
        };
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
        for grant in grant.iter().chain(&extra) {
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
            "scope":validated.scope,"actor":validated.actor,"decided_at":now,"grant_ids":grant.iter().chain(&extra).map(|g|g.id).collect::<Vec<_>>(),"evidence":evidence});
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
                for grant in grant.iter().chain(&extra) {
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
            let requester = request
                .summary
                .requested_by
                .as_deref()
                .ok_or_else(missing)?;
            if matches!(
                request.kind,
                ApprovalKind::SessionGrant | ApprovalKind::Handoff
            ) && let Some(origin) = request.origin.as_ref()
            {
                // Refuses the enqueue, or fails an admitted job (S14) instead of raising another prompt.
                if request.kind == ApprovalKind::SessionGrant {
                    self.check_denial_backoff(
                        requester,
                        REQUESTER_KIND.try_with(|kind| *kind).ok(),
                        origin,
                        Timestamp::now(),
                    )?;
                }
                // A new job joins its requester's pending approval for the origin: one prompt, not one per file.
                // Only enqueue (a job still `queued`) joins, because the engine serialises enqueues.
                if let Some(job) = request.job.as_ref()
                    && job.state == JobState::Queued
                    && let Some(joined) = self.join_batch(
                        &pending,
                        requester,
                        origin,
                        job,
                        request.dest_path.as_deref(),
                        &request.reason_codes,
                    )?
                {
                    return Ok(joined);
                }
            }
            if !request.reason_codes.iter().any(|r| r == "kill_switch") {
                // Engine-raised approvals belong to jobs already admitted; only the daemon-wide cap applies here.
                self.check_caps(&pending, None)?;
            }
            // 05 section 13.2: an enqueued job that raises a session prompt (it joined none) spends one
            // `session_request` token of its requester.
            if request.kind == ApprovalKind::SessionGrant
                && request
                    .job
                    .as_ref()
                    .is_some_and(|job| job.state == JobState::Queued)
                && let Some(rates) = self.rates.get()
                && let Ok(credential) = self.credential(requester)
            {
                // At the rates chosen for the requester: its effective kind and, for a service, its name.
                rates.charge_session_request(&ClientIdentity {
                    credential_id: credential.id,
                    kind: REQUESTER_KIND
                        .try_with(|kind| *kind)
                        .unwrap_or(credential.kind),
                    role: credential.role,
                    permissions: credential.permissions,
                    client_name: String::new(),
                })?;
            }
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
        let _gate = self.gate.lock().await;
        self.journal
            .transact(|tx| {
                if let Some(approval) = tx.approval(id)?
                    && approval.state == ApprovalState::Pending
                {
                    cancel_pending(tx, approval, "request_withdrawn", Timestamp::now())?;
                }
                Ok(())
            })
            .map_err(|error| error.to_envelope())
    }
}

/// End a pending approval as `cancel` by `system` inside `tx`: the approval, its decision record (with
/// `system_reason`) and the `approval.decided` event.
fn cancel_pending(
    tx: &mut dyn zeus_journal::WriteTxn,
    mut approval: Approval,
    system_reason: &str,
    now: Timestamp,
) -> zeus_journal::JournalResult<()> {
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
            approval.id,
            &serde_json::json!({
                "approval_id": approval.id,
                "revision": approval.revision,
                "request_hash": approval.request_hash,
                "outcome": "cancel",
                "actor": Actor::system(),
                "decided_at": now,
                "grant_ids": [],
                "system_reason": system_reason
            }),
        )?;
    }
    tx.append_event(
        now,
        approval.job_id,
        approval.run_id,
        zeus_types::event::ApprovalDecidedData {
            approval_id: approval.id,
            kind: approval.kind,
            outcome: DecisionOutcome::Cancel,
            scope: None,
            actor: Actor::system(),
            grant_id: None,
        }
        .into(),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeus_api::{
        ClientRole,
        auth::{Credential, MemoryCredentialStore, Token},
    };
    fn setup(path: &std::path::Path) -> (ConsentManager, ClientIdentity, ClientIdentity) {
        let (manager, identity) = setup_with_identities(path);
        let agent = identity(ClientKind::Agent, ClientRole::Mcp);
        let human = identity(ClientKind::Human, ClientRole::Cli);
        (manager, agent, human)
    }
    /// A manager and a factory of identities whose credentials it knows.
    fn setup_with_identities(
        path: &std::path::Path,
    ) -> (
        ConsentManager,
        impl Fn(ClientKind, ClientRole) -> ClientIdentity,
    ) {
        let clock = Arc::new(zeus_control::SystemClock::new());
        let journal = Journal::open(&path.join("state.redb"), &Default::default(), clock).unwrap();
        let credentials = Arc::new(MemoryCredentialStore::default());
        let store = credentials.clone();
        let identity = move |kind, role| {
            let id = CredentialId::generate();
            store.insert(
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
            identity,
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
    include!("consent_batch_tests.rs");
    include!("consent_browser_tests.rs");
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
    #[tokio::test]
    async fn configured_caps_bound_pending_requests_and_only_approvals_are_rate_limited() {
        let temp = tempfile::tempdir().unwrap();
        let (manager, agent, human) = setup(temp.path());
        let manager = manager.with_limits(crate::approvals::ConsentLimits::new(3, 1));
        let mut ids = Vec::new();
        for host in ["a", "b", "c"] {
            let mut p = request();
            p.origin = Origin::parse(&format!("https://{host}.example.org")).unwrap();
            ids.push(
                manager
                    .request_session(&agent, p)
                    .await
                    .unwrap()
                    .approval_id
                    .unwrap(),
            );
        }
        let mut p = request();
        p.origin = Origin::parse("https://d.example.org").unwrap();
        let refused = manager.request_session(&agent, p).await.unwrap_err();
        assert_eq!(refused.envelope().code.as_str(), "policy.too_many_pending");
        let decide = |id, outcome| {
            let a = manager.get(&human, id).unwrap();
            ApprovalsDecideParams {
                approval_id: id,
                request_hash: a.request_hash.unwrap(),
                outcome,
                scope: (outcome == DecideOutcome::Approve).then_some(GrantScope::Site),
                channel: Surface::CliInteractive,
                confirmation: ConfirmationLevel::Gesture,
                idempotency_key: None,
            }
        };
        let approve = decide(ids[0], DecideOutcome::Approve);
        manager
            .decide(&human, &approve, evidence(&human, &approve))
            .await
            .unwrap();
        let second = decide(ids[1], DecideOutcome::Approve);
        let limited = manager
            .decide(&human, &second, evidence(&human, &second))
            .await
            .unwrap_err();
        assert_eq!(limited.envelope().code.as_str(), "api.rate_limited");
        // Denials are never rate limited, so a person can always clear a flood.
        for id in &ids[1..] {
            let deny = decide(*id, DecideOutcome::Deny);
            manager
                .decide(&human, &deny, evidence(&human, &deny))
                .await
                .unwrap();
        }
    }
    #[tokio::test]
    async fn new_jobs_of_one_requester_and_origin_share_one_revisioned_approval() {
        let temp = tempfile::tempdir().unwrap();
        let (manager, agent, human) = setup(temp.path());
        let ids = IdGenerator::new();
        let request = |origin: &str, n: u64| {
            let job_id = ids.job_id();
            let mut spec: JobSpec = serde_json::from_value(serde_json::json!({
                "url": format!("{origin}/files/{n}.pdf"), "session_origin": origin,
                "filename": format!("paper-{n}.pdf"), "expected_size": 1000 + n,
                "source": {"kind":"agent","via":"mcp","client_id": agent.credential_id.as_str()}
            }))
            .unwrap();
            spec.source.client_id = Some(agent.credential_id.to_string());
            ApprovalRequest {
                job: Some(Job::new(job_id, spec, JobState::Queued, Timestamp::now())),
                dest_path: Some(
                    temp.path()
                        .join(format!("paper-{n}.pdf"))
                        .display()
                        .to_string(),
                ),
                kind: ApprovalKind::SessionGrant,
                job_id: Some(job_id),
                run_id: None,
                origin: Some(Origin::parse(origin).unwrap()),
                allowed_scopes: vec![GrantScope::Once, GrantScope::Site, GrantScope::Never],
                reason_codes: vec!["session.first_use".into()],
                summary: ApprovalSummary {
                    title: "paper".into(),
                    url_redacted: None,
                    total_bytes: None,
                    source_kind: Some(SourceKind::Agent),
                    requested_by: Some(agent.credential_id.to_string()),
                },
            }
        };
        let mut approval = None;
        let mut jobs = Vec::new();
        for n in 0..3 {
            let r = request("https://portal.example.org", n);
            jobs.push(r.job_id.unwrap());
            // The engine persists what the port returns, with its job; do the same here.
            let a = manager.request(r).await.unwrap();
            manager.store(&a).unwrap();
            approval = Some(a);
        }
        let approval = approval.unwrap();
        assert_eq!(approval.revision, Some(3));
        let pending = manager
            .journal
            .read(|tx| tx.approvals(Some(&[ApprovalState::Pending])))
            .unwrap();
        assert_eq!(pending.len(), 1, "one prompt for three files");
        let view = ApprovalView::try_from(approval.clone()).unwrap();
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
        assert_eq!(
            view.facts["transfer"]["job_ids"].as_array().unwrap().len(),
            3
        );
        assert_eq!(view.facts["transfer"]["total_bytes"], 3003);
        assert_eq!(view.facts["items"].as_array().unwrap().len(), 3);
        // Another origin, or another requester, never joins.
        let other = manager
            .request(request("https://other.example.org", 9))
            .await
            .unwrap();
        assert_ne!(other.id, approval.id);
        // Approving once creates one grant per covered job.
        let p = ApprovalsDecideParams {
            approval_id: approval.id,
            request_hash: view.request_hash,
            outcome: DecideOutcome::Approve,
            scope: Some(GrantScope::Once),
            channel: Surface::CliInteractive,
            confirmation: ConfirmationLevel::Gesture,
            idempotency_key: None,
        };
        manager
            .decide(&human, &p, evidence(&human, &p))
            .await
            .unwrap();
        let grants = manager.journal.read(|tx| tx.grants()).unwrap();
        let mut bound: Vec<_> = grants.iter().filter_map(|g| g.job_id).collect();
        bound.sort();
        jobs.sort();
        assert_eq!(bound, jobs);
        assert!(
            grants
                .iter()
                .all(|g| g.scope == GrantScope::Once && g.approval_id == approval.id)
        );
    }
    #[tokio::test]
    async fn a_denial_backs_off_the_requester_for_that_origin_only() {
        let temp = tempfile::tempdir().unwrap();
        let (manager, agent, human) = setup(temp.path());
        let id = manager
            .request_session(&agent, request())
            .await
            .unwrap()
            .approval_id
            .unwrap();
        let a = manager.get(&human, id).unwrap();
        let deny = ApprovalsDecideParams {
            approval_id: id,
            request_hash: a.request_hash.unwrap(),
            outcome: DecideOutcome::Deny,
            scope: None,
            channel: Surface::CliInteractive,
            confirmation: ConfirmationLevel::Gesture,
            idempotency_key: None,
        };
        manager
            .decide(&human, &deny, evidence(&human, &deny))
            .await
            .unwrap();
        let refused = manager
            .request_session(&agent, request())
            .await
            .unwrap_err();
        assert_eq!(refused.envelope().code.as_str(), "policy.recently_denied");
        let until = refused.envelope().details.as_ref().unwrap()["until"]
            .as_str()
            .map(|s| Timestamp::parse(s).unwrap())
            .unwrap();
        let now = Timestamp::now();
        assert!(until > now && until <= now.saturating_add_ms(10 * 60_000));
        // The back-off lapses after its window and never spreads to other origins or to people.
        let origin = Origin::parse("https://example.org").unwrap();
        let requester = agent.credential_id.as_str();
        assert!(
            manager
                .check_denial_backoff(requester, None, &origin, until.saturating_add_ms(1))
                .is_ok()
        );
        let mut other = request();
        other.origin = Origin::parse("https://other.example.org").unwrap();
        assert!(manager.request_session(&agent, other).await.is_ok());
        assert!(manager.request_session(&human, request()).await.is_ok());
    }

    #[tokio::test]
    async fn a_downgraded_human_capable_credential_is_backed_off() {
        let temp = tempfile::tempdir().unwrap();
        let (manager, identity) = setup_with_identities(temp.path());
        let human = identity(ClientKind::Human, ClientRole::Cli);
        // An unverified process holding an admin credential connects as an agent (05 section 7.4).
        let mut downgraded = identity(ClientKind::Admin, ClientRole::Cli);
        downgraded.kind = ClientKind::Agent;
        downgraded.permissions = ClientKind::Agent.default_permissions();
        let id = manager
            .request_session(&downgraded, request())
            .await
            .unwrap()
            .approval_id
            .unwrap();
        let a = manager.get(&human, id).unwrap();
        let deny = ApprovalsDecideParams {
            approval_id: id,
            request_hash: a.request_hash.unwrap(),
            outcome: DecideOutcome::Deny,
            scope: None,
            channel: Surface::CliInteractive,
            confirmation: ConfirmationLevel::Gesture,
            idempotency_key: None,
        };
        manager
            .decide(&human, &deny, evidence(&human, &deny))
            .await
            .unwrap();
        let refused = manager
            .request_session(&downgraded, request())
            .await
            .unwrap_err();
        assert_eq!(refused.envelope().code.as_str(), "policy.recently_denied");
        // Approvals the engine raises outside any connection fall back to the credential's kind.
        let requester = downgraded.credential_id.as_str();
        let origin = request().origin;
        let now = Timestamp::now();
        assert!(
            manager
                .check_denial_backoff(requester, Some(ClientKind::Agent), &origin, now)
                .is_err()
        );
        assert!(
            manager
                .check_denial_backoff(requester, None, &origin, now)
                .is_ok()
        );
    }
}
