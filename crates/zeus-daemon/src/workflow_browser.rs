//! Explicit browser-network review and document bindings for sampling and approved execution.
use super::*;
use zeus_session::protocol::{PageRepresentation, SessionPageRequestBody, SessionPageSubject};
use zeus_types::{GrantId, HttpUrl, Origin};

const PREVIEW_READS: u32 = 32;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct BrowserReview {
    origin: Origin,
    grant_id: GrantId,
    pub(super) profile_id: String,
    request_hash: String,
    pub(super) approval_id: ApprovalId,
    reads: u32,
}

impl WorkflowService {
    /// Attach the authenticated native-message broker; tests without it cannot use browser mode.
    pub fn attach_browser(&self, browser: Arc<crate::browser::BrowserBroker>) {
        let _ = self.browser.set(browser);
    }

    pub(super) async fn prepare_browser_review(
        &self,
        identity: &ClientIdentity,
        id: &PlanId,
        params: &WorkflowsPlanParams,
        old: Option<&Draft>,
    ) -> ApiResult<Option<BrowserReview>> {
        let Some(origin) = &params.session_origin else {
            if params.browser_document_mode.is_some() || old.is_some_and(|d| d.browser.is_some()) {
                return Err(ApiError::invalid_params(
                    "browser draft requires its explicit origin and mode",
                ));
            }
            return Ok(None);
        };
        if params.browser_document_mode != Some(SessionDocumentMode::BrowserOrigin) {
            return Err(ApiError::new(
                "workflow.capability_missing",
                "choose browser_origin mode explicitly",
            ));
        }
        if old.is_some_and(|draft| draft.browser.is_none()) {
            return Err(ApiError::new(
                "api.conflict",
                "changing a public draft to browser mode requires a new draft and review",
            ));
        }
        let urls: Vec<HttpUrl> = params
            .start_urls
            .iter()
            .map(|s| {
                HttpUrl::parse(s).map_err(|_| ApiError::invalid_params("invalid document URL"))
            })
            .collect::<ApiResult<_>>()?;
        if urls
            .iter()
            .any(|u| u.origin() != *origin || u.url().fragment().is_some())
            || !origin.allows_session_release()
        {
            return Err(ApiError::invalid_params(
                "browser documents require one exact HTTPS origin",
            ));
        }
        let browser = self.browser.get().ok_or_else(|| {
            ApiError::new(
                "workflow.capability_missing",
                "browser document support unavailable",
            )
        })?;
        let subject = SessionPageSubject::Plan {
            plan_id: id.to_string(),
        };
        let (grant_id, profile_id) = browser.document_grant(origin, &subject, old.and_then(|d| d.browser.as_ref()).map(|review| review.profile_id.as_str())).map_err(|_| {
            ApiError::new(
                "policy.approval_required",
                "a site grant displayed by the extension and one exact connected browser profile are required",
            )
        })?;
        let binding = serde_json::json!({"plan_id":id,"owner":identity.credential_id.as_str(),"urls":urls,
            "origin":origin,"profile_id":profile_id,"grant_id":grant_id,"mode":"browser_origin","max_reads":PREVIEW_READS,"max_body_bytes":524288});
        let request_hash =
            blake3::hash(&zeus_verify::canonical::to_jcs_bytes(&binding).map_err(|_| fail())?)
                .to_hex()
                .to_string();
        if let Some(review) = old.and_then(|d| d.browser.as_ref()) {
            if review.request_hash != request_hash {
                return Err(ApiError::new(
                    "api.conflict",
                    "changed browser scope requires a new draft and human review",
                ));
            }
            return Ok(Some(review.clone()));
        }
        let scope = serde_json::json!({"request_blake3":request_hash,"origin":origin,"grant_id":grant_id,"profile_id":profile_id,
            "start_urls":urls.iter().map(|u| zeus_workflow::runtime::display_url(u.url().as_str())).collect::<Vec<_>>(),
            "scope":"start pages and discovered pages within this exact origin","max_reads":PREVIEW_READS,"max_body_bytes":524288,"deadline_ms":25000,
            "native_peer_pinning":false,"redirects":"deny","network":"Browser resolves and connects under its own network policy after a public DNS preflight. DNS rebinding protection is weaker than native peer pinning."});
        let approval_id = self
            .consent
            .request_browser_documents(identity, id, scope)
            .await?;
        Ok(Some(BrowserReview {
            origin: origin.clone(),
            grant_id,
            profile_id,
            request_hash,
            approval_id,
            reads: 0,
        }))
    }

    fn browser_preview_authorized(&self, id: &PlanId, owner: &str, review: &BrowserReview) -> bool {
        self.consent.requester_active(owner)
            && self
                .store
                .get::<Draft>(&format!("draft:{id}"))
                .ok()
                .flatten()
                .is_some_and(|d| {
                    d.owner == owner
                        && d.browser.is_some_and(|r| {
                            r.request_hash == review.request_hash
                                && r.approval_id == review.approval_id
                                && r.profile_id == review.profile_id
                                && r.grant_id == review.grant_id
                                && r.origin == review.origin
                                && r.reads <= PREVIEW_READS
                        })
                })
            && self
                .journal
                .read(|tx| tx.approval(review.approval_id))
                .ok()
                .flatten()
                .is_some_and(|a| {
                    a.kind == zeus_types::ApprovalKind::PolicyException
                        && a.state == ApprovalState::Approved
                        && a.requested_by.as_deref() == Some(owner)
                        && a.expires_at > Timestamp::now()
                        && a.facts.as_ref().is_some_and(|f| {
                            f.get("browser_scope")
                                .and_then(|s| s.get("request_blake3"))
                                .and_then(|v| v.as_str())
                                == Some(review.request_hash.as_str())
                                && f["browser_scope"]["profile_id"] == review.profile_id
                                && f["browser_scope"]["grant_id"] == review.grant_id.to_string()
                                && f["browser_scope"]["origin"] == review.origin.as_str()
                        })
                })
    }

    pub(super) fn browser_review_ready(
        &self,
        id: &PlanId,
        owner: &str,
        review: &BrowserReview,
    ) -> bool {
        self.browser_preview_authorized(id, owner, review)
    }

    pub(super) async fn preview_document(
        &self,
        id: &PlanId,
        owner: &str,
        plan: &Plan,
        url: &str,
        accept: &str,
    ) -> RuntimeResult<Document> {
        let key = format!("draft:{id}");
        let mut draft = self
            .store
            .get::<Draft>(&key)
            .map_err(|_| runtime_fail())?
            .ok_or_else(runtime_fail)?;
        let Some(mut review) = draft.browser.clone() else {
            return self
                .network
                .document_with_accept(plan, url, None, accept)
                .await;
        };
        if !self.browser_preview_authorized(id, owner, &review) || review.reads >= PREVIEW_READS {
            return Err(RuntimeError::Policy("workflow.browser_review_required"));
        }
        // Reserve before dispatch. A crash or failed read consumes budget and cannot replay for free.
        review.reads += 1;
        draft.browser = Some(review.clone());
        self.store.put(&key, &draft).map_err(|_| runtime_fail())?;
        let authority = || self.browser_preview_authorized(id, owner, &review);
        self.browser_document(
            plan,
            url,
            accept,
            SessionPageSubject::Plan {
                plan_id: id.to_string(),
            },
            &review.origin,
            (review.grant_id, review.profile_id.clone()),
            &authority,
        )
        .await
    }

    pub(super) async fn run_browser_document(
        &self,
        row: &StoredRun,
        request: &FetchRequest,
    ) -> RuntimeResult<Document> {
        let session = row.plan.session.as_ref().ok_or_else(runtime_fail)?;
        if session.document_mode != Some(SessionDocumentMode::BrowserOrigin)
            || session.profile_id.is_none()
        {
            return Err(RuntimeError::Policy("workflow.capability_missing"));
        }
        let browser = self
            .browser
            .get()
            .ok_or(RuntimeError::Policy("workflow.capability_missing"))?;
        let subject = SessionPageSubject::Run {
            run_id: request.run_id,
        };
        let grant = browser
            .document_grant(&session.origin, &subject, session.profile_id.as_deref())
            .map_err(|_| RuntimeError::Policy("session.consent_required"))?;
        let authority = || {
            self.consent.requester_active(&row.owner)
                && self
                    .store
                    .run(request.run_id)
                    .ok()
                    .flatten()
                    .is_some_and(|current| {
                        current.snapshot.report.status == RunStatus::Running
                            && current.snapshot.report.plan_hash == row.snapshot.report.plan_hash
                            && current.approval_id == row.approval_id
                    })
                && self
                    .journal
                    .read(|tx| {
                        row.approval_id
                            .map(|id| tx.approval(id))
                            .transpose()
                            .map(Option::flatten)
                    })
                    .ok()
                    .flatten()
                    .is_some_and(|a| {
                        a.state == ApprovalState::Approved
                            && a.run_id == Some(request.run_id)
                            && a.requested_by.as_deref() == Some(row.owner.as_str())
                            && a.facts.as_ref().is_some_and(|f| {
                                f.get("plan_blake3").and_then(|v| v.as_str())
                                    == Some(row.snapshot.report.plan_hash.as_str())
                                    && f.get("browser_document_mode").and_then(|v| v.as_str())
                                        == Some("browser_origin")
                                    && f.get("browser_profile_id").and_then(|v| v.as_str())
                                        == session.profile_id.as_deref()
                            })
                    })
        };
        self.browser_document(
            &row.plan,
            &request.url,
            &request.accept,
            subject,
            &session.origin,
            grant,
            &authority,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn browser_document(
        &self,
        plan: &Plan,
        url: &str,
        accept: &str,
        subject: SessionPageSubject,
        origin: &Origin,
        grant: (GrantId, String),
        authority: &(dyn Fn() -> bool + Sync),
    ) -> RuntimeResult<Document> {
        let run = match subject {
            SessionPageSubject::Run { run_id } => Some(run_id),
            _ => None,
        };
        let url = HttpUrl::parse(url).map_err(|_| RuntimeError::Policy("workflow.url_invalid"))?;
        if !authority() {
            return Err(RuntimeError::Policy("policy.approval_required"));
        }
        let _permit = self
            .network
            .browser_preflight(plan, url.url(), origin, run)
            .await?;
        let representation = match accept {
            "application/json" => PageRepresentation::Json,
            "application/xml,text/xml;q=0.9" => PageRepresentation::Xml,
            "text/html,application/xhtml+xml;q=0.9,*/*;q=0.1"
            | "text/html,application/json;q=0.9,*/*;q=0.1" => PageRepresentation::Html,
            _ => return Err(RuntimeError::Policy("workflow.document_accept")),
        };
        let request = SessionPageRequestBody {
            subject,
            grant_id: grant.0,
            profile_id: grant.1,
            origin: origin.clone(),
            url,
            representation,
            max_body_bytes: 524288,
            deadline_ms: 25000,
            incognito: false,
        };
        let document = self
            .browser
            .get()
            .ok_or(RuntimeError::Policy("workflow.capability_missing"))?
            .document(request, authority)
            .await
            .map_err(|_| RuntimeError::Port("workflow.browser_document_unavailable"))?;
        let body = std::str::from_utf8(document.data.as_bytes())
            .map_err(|_| RuntimeError::Policy("workflow.document_encoding"))?
            .to_owned();
        Ok(Document {
            final_url: document.final_url.url().as_str().to_owned(),
            status: document.http_status,
            content_type: document.content_type,
            body,
            headers: document
                .link
                .map(|link| BTreeMap::from([("link".into(), link)]))
                .unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeus_api::auth::{Credential, CredentialId, MemoryCredentialStore, Token};

    #[tokio::test]
    async fn browser_preview_budget_is_durable_owner_bound_and_consumes_failed_reads() {
        let temp = tempfile::tempdir().unwrap();
        let journal = zeus_journal::Journal::open(
            &temp.path().join("journal.redb"),
            &Default::default(),
            Arc::new(zeus_control::SystemClock::new()),
        )
        .unwrap();
        let credentials = Arc::new(MemoryCredentialStore::default());
        let credential_id = CredentialId::generate();
        credentials.insert(
            Credential::new(
                credential_id.clone(),
                ClientKind::Agent,
                zeus_api::ClientRole::Mcp,
                Token::generate().auth_key(),
                Timestamp::now(),
            )
            .unwrap(),
        );
        let audit = Arc::new(zeus_verify::MemoryAuditLog::new(
            zeus_types::IdGenerator::new().chain_id(),
            zeus_types::IdGenerator::new().store_id(),
            "test",
            Timestamp::now(),
            None,
        ));
        let consent = Arc::new(crate::consent::ConsentManager::new(
            journal.clone(),
            audit.clone(),
            credentials,
        ));
        let open = || {
            WorkflowService::open(
                &temp.path().join("workflow.redb"),
                journal.clone(),
                consent.clone(),
                audit.clone(),
                vec![temp.path().to_owned()],
            )
            .unwrap()
        };
        let service = open();
        let id = PlanId::parse("plan_01M45NHJ2AHWMXT8JVJ7BEEJ01").unwrap();
        let review = BrowserReview {
            origin: "https://portal.example.org".parse().unwrap(),
            grant_id: zeus_types::IdGenerator::new().grant_id(),
            profile_id: "a".repeat(32),
            request_hash: "1".repeat(64),
            approval_id: zeus_types::IdGenerator::new().approval_id(),
            reads: 0,
        };
        let approval: zeus_types::Approval = serde_json::from_value(serde_json::json!({"id":review.approval_id,"kind":"policy_exception","state":"approved","allowed_scopes":[],
            "reason_codes":[],"summary":{"title":"fixture"},"created_at":Timestamp::now(),"expires_at":Timestamp::now().saturating_add_ms(60000),"requested_by":credential_id.as_str(),
            "facts":{"browser_scope":{"request_blake3":review.request_hash,"profile_id":review.profile_id,"grant_id":review.grant_id,"origin":review.origin}}})).unwrap();
        journal.transact(|tx| tx.put_approval(&approval)).unwrap();
        let draft = Draft {
            owner: credential_id.to_string(),
            plan: None,
            samples: vec![],
            updated_at: Timestamp::now(),
            dry: None,
            browser: Some(review.clone()),
        };
        service.store.put(&format!("draft:{id}"), &draft).unwrap();
        assert!(service.browser_preview_authorized(&id, credential_id.as_str(), &review));
        assert!(!service.browser_preview_authorized(&id, "another-owner", &review));
        let mut changed = review.clone();
        changed.request_hash = "2".repeat(64);
        assert!(!service.browser_preview_authorized(&id, credential_id.as_str(), &changed));
        changed = review.clone();
        changed.profile_id = "b".repeat(32);
        assert!(!service.browser_preview_authorized(&id, credential_id.as_str(), &changed));
        changed = review.clone();
        changed.grant_id = zeus_types::IdGenerator::new().grant_id();
        assert!(!service.browser_preview_authorized(&id, credential_id.as_str(), &changed));
        let plan = sampling_plan(&["https://portal.example.org/list".into()]).unwrap();
        for _ in 0..PREVIEW_READS {
            let result = service
                .preview_document(
                    &id,
                    credential_id.as_str(),
                    &plan,
                    "http://127.0.0.1/private",
                    "application/json",
                )
                .await;
            assert!(matches!(
                result,
                Err(RuntimeError::Policy("workflow.browser_network_refused"))
            ));
        }
        drop(service);
        let service = open();
        let stored = service
            .store
            .get::<Draft>(&format!("draft:{id}"))
            .unwrap()
            .unwrap();
        assert_eq!(stored.browser.unwrap().reads, PREVIEW_READS);
        assert!(matches!(
            service
                .preview_document(
                    &id,
                    credential_id.as_str(),
                    &plan,
                    "http://127.0.0.1/private",
                    "application/json"
                )
                .await,
            Err(RuntimeError::Policy("workflow.browser_review_required"))
        ));
        let mut revoked = approval;
        revoked.state = ApprovalState::Canceled;
        journal.transact(|tx| tx.put_approval(&revoked)).unwrap();
        assert!(!service.browser_preview_authorized(&id, credential_id.as_str(), &review));
    }
}
