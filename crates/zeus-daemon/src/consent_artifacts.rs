//! Artifact tombstones and retention weakening use exact, human-decided destructive-operation facts.
use super::*;
use zeus_api::methods::artifacts::{ArtifactLifecycleOperation, ArtifactLifecyclePlan};

impl ConsentManager {
    pub(crate) async fn request_artifact_mutation(
        &self,
        requester: &ClientIdentity,
        plan: &ArtifactLifecyclePlan,
        hash: Hex256,
    ) -> ApiResult<ApprovalId> {
        let _gate = self.gate.lock().await;
        let credential = self.credential(requester.credential_id.as_str())?;
        let pending = self
            .journal
            .read(|tx| tx.approvals(Some(&[ApprovalState::Pending])))
            .map_err(error)?;
        // A crash after creating an approval but before saving the API checkpoint reuses that exact request.
        if let Some(a) = pending.iter().find(|a| {
            a.kind == ApprovalKind::DestructiveOp
                && a.requested_by.as_deref() == Some(requester.credential_id.as_str())
                && a.facts.as_ref().and_then(|f| f.get("operation_set_blake3"))
                    == Some(&serde_json::json!(hash))
                && a.facts.as_ref().and_then(|f| f.get("artifact_plan"))
                    == Some(&serde_json::json!(plan))
        }) {
            return Ok(a.id);
        }
        self.check_caps(&pending, Some(requester.credential_id.as_str()))?;
        let (reason, title, deletes) = match plan.operation {
            ArtifactLifecycleOperation::Delete => (
                "artifacts.delete",
                format!("Tombstone artifact {} (keeps user files)", plan.artifact_id),
                1,
            ),
            ArtifactLifecycleOperation::SetRetention { .. } => (
                "artifacts.retention",
                format!("Weaken stored-content protection for {}", plan.artifact_id),
                0,
            ),
            ArtifactLifecycleOperation::Materialize { .. } => {
                return Err(ApiError::invalid_params(
                    "materialization has no destructive approval",
                ));
            }
        };
        let now = Timestamp::now();
        let id = IdGenerator::new().approval_id();
        let label = match credential.role {
            zeus_api::ClientRole::Cli | zeus_api::ClientRole::Tui => "cli",
            _ => "agent",
        };
        let facts = serde_json::json!({"t":"zeus.approval.facts.v1","approval_id":id,"revision":1,"kind":"destructive_op",
            "requester":{"kind":credential.kind,"role":credential.role,"client_id":requester.credential_id.as_str(),"label":label,"registered_at":credential.created_at},
            "issued_at":now,"expires_at":plan.expires_at,"allowed_scopes":[],"confirmation":{"approve":"typed_code"},
            "reason_codes":[reason],"agent_says_blake3":null,"operation_set_blake3":hash,"plan_id":null,
            "counts":{"delete":deletes,"overwrite":0,"move":0,"rename":0},"total_bytes":0,"to_trash":false,
            "quarantined_count":u64::from(plan.quarantined),"sample":[],"sample_omitted":0,"artifact_plan":plan});
        let approval: Approval = serde_json::from_value(serde_json::json!({"id":id,"kind":"destructive_op","state":"pending",
            "allowed_scopes":[],"reason_codes":[reason],"summary":{"title":title,"total_bytes":0},"created_at":now,
            "expires_at":plan.expires_at,"request_hash":crate::approvals::facts_hash(&facts)?,"requested_by":requester.credential_id.as_str(),"revision":1,"facts":facts}))
            .map_err(|_| ApiError::internal("cannot bind artifact lifecycle approval"))?;
        self.store(&approval)?;
        Ok(id)
    }
}

impl ConsentManager {
    /// Ask a person about one exact store collection, without expanding the requester's authority.
    pub(crate) async fn request_artifact_maintenance(
        &self,
        requester: &ClientIdentity,
        plan: &zeus_api::methods::artifacts::ArtifactMaintenancePlan,
        hash: Hex256,
    ) -> ApiResult<ApprovalId> {
        use zeus_api::methods::artifacts::ArtifactMaintenanceOperation;
        if !plan.requires_approval {
            return Err(ApiError::invalid_params("maintenance approval is required"));
        }
        let (count, bytes, reason, title) = match plan.operation {
            ArtifactMaintenanceOperation::Collect { .. } => {
                let collection: zeus_artifact::CollectionPlan =
                    serde_json::from_value(plan.details.clone())
                        .map_err(|_| ApiError::invalid_params("invalid collection facts"))?;
                let count = collection.objects.len();
                let bytes = collection.objects.iter().map(|o| o.size_bytes).sum::<u64>();
                (
                    count,
                    bytes,
                    "artifacts.collect",
                    format!(
                        "Permanently collect {count} reviewed stored objects; keeps user files"
                    ),
                )
            }
            ArtifactMaintenanceOperation::Scrub { .. } => {
                let integrity: zeus_artifact::IntegrityPlan =
                    serde_json::from_value(plan.details.clone())
                        .map_err(|_| ApiError::invalid_params("invalid integrity facts"))?;
                (
                    0,
                    integrity.objects.iter().map(|o| o.observed_size).sum(),
                    "artifacts.scrub",
                    "Verify exact stored objects; mismatches move into private damaged storage"
                        .to_owned(),
                )
            }
            ArtifactMaintenanceOperation::Repair { artifact_id, .. } => (
                0,
                plan.details["size_bytes"]
                    .as_u64()
                    .ok_or_else(|| ApiError::invalid_params("repair size missing"))?,
                "artifacts.repair",
                format!(
                    "Restore exact verified stored content for {artifact_id}; keeps user files"
                ),
            ),
            _ => {
                return Err(ApiError::invalid_params(
                    "this operation has no maintenance approval",
                ));
            }
        };
        let _gate = self.gate.lock().await;
        let credential = self.credential(requester.credential_id.as_str())?;
        let pending = self
            .journal
            .read(|tx| tx.approvals(Some(&[ApprovalState::Pending])))
            .map_err(error)?;
        if let Some(a) = pending.iter().find(|a| {
            a.kind == ApprovalKind::DestructiveOp
                && a.requested_by.as_deref() == Some(requester.credential_id.as_str())
                && a.facts.as_ref().and_then(|f| f.get("operation_set_blake3"))
                    == Some(&serde_json::json!(hash))
                && a.facts.as_ref().and_then(|f| f.get("maintenance_plan"))
                    == Some(&serde_json::json!(plan))
        }) {
            return Ok(a.id);
        }
        self.check_caps(&pending, Some(requester.credential_id.as_str()))?;
        let now = Timestamp::now();
        let id = IdGenerator::new().approval_id();
        let facts = serde_json::json!({"t":"zeus.approval.facts.v1","approval_id":id,"revision":1,"kind":"destructive_op",
            "requester":{"kind":credential.kind,"role":credential.role,"client_id":requester.credential_id.as_str(),"label":"artifact maintenance","registered_at":credential.created_at},
            "issued_at":now,"expires_at":plan.expires_at,"allowed_scopes":[],"confirmation":{"approve":"typed_code"},
            "reason_codes":[reason],"agent_says_blake3":null,"operation_set_blake3":hash,"plan_id":null,
            "counts":{"delete":count,"overwrite":0,"move":0,"rename":0},"total_bytes":bytes,"to_trash":false,"quarantined_count":0,"sample":[],"sample_omitted":0,"maintenance_plan":plan});
        let approval:Approval=serde_json::from_value(serde_json::json!({"id":id,"kind":"destructive_op","state":"pending","allowed_scopes":[],"reason_codes":[reason],"summary":{"title":title,"total_bytes":bytes},"created_at":now,"expires_at":plan.expires_at,"request_hash":crate::approvals::facts_hash(&facts)?,"requested_by":requester.credential_id.as_str(),"revision":1,"facts":facts})).map_err(|_|ApiError::internal("cannot bind collection approval"))?;
        self.store(&approval)?;
        Ok(id)
    }
}
