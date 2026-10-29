//! A separate human review for browser-managed document networking; this creates no session grant.
use super::*;

impl ConsentManager {
    pub(crate) async fn request_browser_documents(
        &self,
        requester: &ClientIdentity,
        plan_id: &zeus_workflow::PlanId,
        scope: serde_json::Value,
    ) -> ApiResult<ApprovalId> {
        let _gate = self.gate.lock().await;
        let credential = self.credential(requester.credential_id.as_str())?;
        let existing = self.journal.read(|tx| tx.approvals(None)).map_err(error)?;
        if let Some(a) = existing.iter().find(|a| {
            a.kind == ApprovalKind::PolicyException
                && a.requested_by.as_deref() == Some(requester.credential_id.as_str())
                && a.facts
                    .as_ref()
                    .is_some_and(|f| f.get("browser_scope") == Some(&scope))
                && a.expires_at > Timestamp::now()
        }) {
            return Ok(a.id);
        }
        let pending = existing
            .into_iter()
            .filter(|a| a.state == ApprovalState::Pending)
            .collect::<Vec<_>>();
        self.check_caps(&pending, Some(requester.credential_id.as_str()))?;
        let id = IdGenerator::new().approval_id();
        let now = Timestamp::now();
        let expires = now.saturating_add_ms(900_000);
        let facts = serde_json::json!({"t":"zeus.approval.facts.v1","approval_id":id,"revision":1,"kind":"policy_exception",
            "requester":{"kind":credential.kind,"role":credential.role,"client_id":requester.credential_id.as_str(),"label":"workflow","registered_at":credential.created_at},
            "issued_at":now,"expires_at":expires,"allowed_scopes":[],"confirmation":{"approve":"typed_code"},
            "reason_codes":["workflow.browser_network"],"agent_says_blake3":null,"rule":"browser_managed_network","operation":"read",
            "plan_id":plan_id,"browser_scope":scope});
        let approval: Approval = serde_json::from_value(serde_json::json!({"id":id,"kind":"policy_exception","state":"pending",
            "allowed_scopes":[],"reason_codes":["workflow.browser_network"],"summary":{"title":"Read website documents using the browser network"},
            "created_at":now,"expires_at":expires,"request_hash":crate::approvals::facts_hash(&facts)?,"requested_by":requester.credential_id.as_str(),
            "revision":1,"facts":facts})).map_err(|_| ApiError::internal("cannot bind browser review"))?;
        self.store(&approval)?;
        Ok(id)
    }
}
