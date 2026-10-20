//! `sessions.*` and `approvals.*`: grants and the human decision.
//!
//! Agents can list grants (they contain no cookies), ask for one (which raises an approval and never grants it),
//! and read the approvals they caused. Only a human-kind client on a trust surface can decide an approval or revoke
//! a grant (ADR 0007).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeus_types::job::GrantScopeHint;
use zeus_types::{
    Approval, ApprovalId, ApprovalKind, ApprovalState, GrantId, GrantScope, Hex256, JobId, Origin,
    RunId, SessionGrant, Surface, Timestamp,
};

use crate::paging::{Page, PageRequest};

/// `sessions.list` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionsListParams {
    /// Paging: `limit` and `cursor` sit beside the filters (00 section 10).
    #[serde(flatten)]
    pub page: PageRequest,
    /// Only this origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// Include expired, revoked and consumed grants.
    #[serde(default)]
    pub include_inactive: bool,
}

/// `sessions.list` result: grants, which never contain cookies.
pub type SessionsListResult = Page<SessionGrant>;

/// `sessions.request` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionsRequestParams {
    /// The origin whose session is needed.
    pub origin: Origin,
    /// The scope the agent suggests; the human chooses.
    pub scope: GrantScopeHint,
    /// Why, in the agent's words. Shown to the human in a separate field labelled "agent says" and treated as
    /// untrusted text. At most 280 characters.
    pub reason: String,
    /// The job that needs it (for `once`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<JobId>,
    /// The run that needs it (for `workflow`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// Idempotency key (00 section 5.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// Outcome of a session request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionRequestStatus {
    /// An effective grant already covers the origin.
    AlreadyGranted,
    /// An approval is waiting for a human. The call returns immediately; poll `approvals.get` or subscribe.
    PendingUserApproval,
    /// The origin is on the never-list.
    Denied,
}

/// `sessions.request` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionsRequestResult {
    /// True when this response replays a durable mutation receipt.
    #[serde(default)]
    pub replayed: bool,
    /// What happened.
    pub status: SessionRequestStatus,
    /// The covering grant, for `already_granted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<GrantId>,
    /// The pending approval, for `pending_user_approval`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<ApprovalId>,
    /// When the pending approval expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
}

/// `sessions.revoke` parameters (human only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionsRevokeParams {
    /// The grant.
    pub grant_id: GrantId,
    /// Which surface the human used; must be a trust surface.
    pub surface: Surface,
}

/// `sessions.revoke` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionsRevokeResult {
    /// True when this response replays a durable mutation receipt.
    #[serde(default)]
    pub replayed: bool,
    /// The grant after revocation (idempotent: revoking a revoked grant returns it unchanged).
    pub grant: SessionGrant,
}

/// An approval plus the daemon-computed binding data (ARCHITECTURE section 7; 01 section 5.3 gains these
/// fields when spec 07 is written).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalView {
    /// The approval entity of 01.
    #[serde(flatten)]
    pub approval: Approval,
    /// BLAKE3 over the JCS form of `facts`: a decision must name it, so it cannot be replayed on another request.
    pub request_hash: Hex256,
    /// The exact facts the hash covers and the human sees, computed by the daemon: origins, counts, total bytes,
    /// destination, expiry, requesting client, plan hash.
    pub facts: serde_json::Value,
    /// Free text supplied by the requester, displayed separately and labelled "agent says". Untrusted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_says: Option<String>,
}

impl TryFrom<Approval> for ApprovalView {
    type Error = crate::ApiError;

    fn try_from(mut approval: Approval) -> Result<Self, Self::Error> {
        let request_hash = approval.request_hash.take().ok_or_else(|| {
            crate::ApiError::new(
                "policy.request_hash_mismatch",
                "legacy approval must be reissued before it can be decided",
            )
        })?;
        let facts = approval.facts.take().ok_or_else(|| {
            crate::ApiError::new("policy.request_hash_mismatch", "approval facts are missing")
        })?;
        let agent_says = approval.agent_says.take();
        Ok(Self {
            approval,
            request_hash,
            facts: serde_json::Value::Object(facts),
            agent_says,
        })
    }
}

/// Confirmation performed by an attested human surface (spec 07).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationLevel {
    /// A deliberate gesture after the arming delay.
    #[default]
    Gesture,
    /// The human typed the request's binding code on the trusted surface.
    TypedCode,
    /// An operating-system authentication prompt was completed.
    OsAuth,
}

/// `approvals.list` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalsListParams {
    /// Paging: `limit` and `cursor` sit beside the filters (00 section 10).
    #[serde(flatten)]
    pub page: PageRequest,
    /// Only this state. Absent: `pending`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<ApprovalState>,
    /// Only this kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ApprovalKind>,
}

/// `approvals.list` result. Agent clients see only the approvals they caused.
pub type ApprovalsListResult = Page<ApprovalView>;

/// `approvals.get` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalsGetParams {
    /// The approval.
    pub approval_id: ApprovalId,
}

/// `approvals.get` returns the request and, for human-capable callers, its private decision evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalsGetResult {
    /// The request this client may inspect.
    pub approval: ApprovalView,
    /// Durable evidence of a completed decision, withheld from unattended clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_record: Option<serde_json::Value>,
}

/// The decision a human takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecideOutcome {
    /// Approve with the chosen scope.
    Approve,
    /// Deny; with scope `never` the origin is also blocked.
    Deny,
}

/// `approvals.decide` parameters (human only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalsDecideParams {
    /// The approval.
    pub approval_id: ApprovalId,
    /// The `request_hash` the human was shown. A different value fails with `policy.request_hash_mismatch`.
    pub request_hash: Hex256,
    /// Approve or deny.
    pub outcome: DecideOutcome,
    /// The scope the human chose; must be in `allowed_scopes` for `approve`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<GrantScope>,
    /// The trust surface the decision was taken on. The daemon verifies it against the connection (terminal with a
    /// controlling tty, pinned extension with a gesture attestation) and rejects non-trust surfaces.
    pub channel: Surface,
    /// Confirmation completed by the human surface. The channel must be verified independently.
    #[serde(default)]
    pub confirmation: ConfirmationLevel,
    /// Optional retry key; it does not permit a different decision on the same approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `approvals.decide` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalsDecideResult {
    /// True when this response replays a durable mutation receipt.
    #[serde(default)]
    pub replayed: bool,
    /// The approval after the decision (immutable from now on).
    pub approval: ApprovalView,
    /// The grant the decision created, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<SessionGrant>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_params_require_hash_and_channel() {
        let missing = serde_json::json!({
            "approval_id": "ap_01M45NHJ3XS0QXG9T42HHN2YR8",
            "outcome": "approve"
        });
        assert!(serde_json::from_value::<ApprovalsDecideParams>(missing).is_err());
    }

    #[test]
    fn session_request_has_no_scope_decision_power() {
        // The agent suggests a scope hint (once, site, workflow); `never` cannot even be expressed.
        let bad = serde_json::json!({
            "origin": "https://portal.example.org", "scope": "never", "reason": "x"
        });
        assert!(serde_json::from_value::<SessionsRequestParams>(bad).is_err());
    }
}
