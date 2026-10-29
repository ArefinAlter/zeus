//! Additional optional team restrictions applied before every daemon backend dispatch.
#[path = "../../../shared/team_policy.rs"]
pub mod policy;
use serde_json::Value;
use zeus_api::{ApiError, ApiResult};
/// Check the common effect map after the API's normal identity/permission gate.
pub fn admit(guard: &policy::Guard, method: &str, params: &Value) -> ApiResult<()> {
    for feature in policy::method_features(method, params) {
        guard.admit(feature).map_err(|_| ApiError::new(zeus_api::error::codes::PERMISSION_DENIED, "Team policy does not allow this new operation. Refresh the signed team policy or contact its administrator."))?;
    }
    Ok(())
}

/// Classify the actual stored approval kind; display strings/caller parameters never select a feature.
pub fn approval_features(kind: zeus_types::ApprovalKind) -> ApiResult<&'static [&'static str]> {
    use zeus_types::ApprovalKind;
    match kind {
        ApprovalKind::Handoff | ApprovalKind::SessionGrant | ApprovalKind::RetryEscalation => {
            Ok(&["downloads"])
        }
        // Current workflow Plan has only local sinks. A future cloud sink must extend this classification.
        ApprovalKind::WorkflowPlan => Ok(&["workflows", "downloads"]),
        ApprovalKind::CloudDelivery => Ok(&["cloud_delivery"]),
        ApprovalKind::DestructiveOp => Ok(&[]),
        ApprovalKind::PolicyException => {
            Err(ApiError::permission_denied("unknown team policy exception"))
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_and_download_approvals_do_not_require_cloud_entitlement() {
        use zeus_types::ApprovalKind::*;
        assert!(approval_features(DestructiveOp).unwrap().is_empty());
        assert_eq!(approval_features(SessionGrant).unwrap(), &["downloads"]);
        assert_eq!(
            approval_features(WorkflowPlan).unwrap(),
            &["workflows", "downloads"]
        );
        assert_eq!(
            approval_features(CloudDelivery).unwrap(),
            &["cloud_delivery"]
        );
        assert!(approval_features(PolicyException).is_err());
    }
}
