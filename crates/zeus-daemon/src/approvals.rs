//! The security logic of human decisions (ADR 0007), independent of storage.
//!
//! A decision is valid only when **all** of these hold, checked in this order:
//!
//! 1. the caller is a human-kind client and the named channel is a trust surface (an agent never gets a human
//!    actor, whatever surface it names);
//! 2. the approval is still `pending` and has not expired;
//! 3. the decision names the approval's current `request_hash` (the human decided on exactly these facts);
//! 4. the scope is one the approval allows (`approve`), or `never`/absent (`deny`), or absent when the approval
//!    allows no scope.
//!
//! The decision is then applied by the daemon in one journal transaction with its events (01 section 6 rule 2).
//! Per-channel proof (tty ownership, extension gesture attestation) is verified by the connection layer before this
//! function is called; this function never trusts a client's claim about the channel on its own.

use std::collections::VecDeque;

use zeus_api::error::codes;
use zeus_api::methods::sessions::{ApprovalView, ApprovalsDecideParams, DecideOutcome};
use zeus_api::{ApiError, ApiResult, ClientIdentity};
use zeus_types::limits::MAX_PENDING_APPROVALS;
use zeus_types::{Actor, ApprovalState, DecisionOutcome, GrantScope, Hex256, Timestamp};

/// The hash a decision must name: BLAKE3 over the JCS form of the facts (00 section 7).
pub fn facts_hash(facts: &serde_json::Value) -> ApiResult<Hex256> {
    zeus_verify::canonical::request_hash(facts).map_err(|_| {
        ApiError::internal("approval facts cannot be canonicalised (floats or unsafe integers)")
    })
}

/// A decision that passed every check, ready to be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedDecision {
    /// Approve or deny.
    pub outcome: DecisionOutcome,
    /// The scope the human chose.
    pub scope: Option<GrantScope>,
    /// Who decided (human on a trust surface, with the credential id).
    pub actor: Actor,
}

/// Validate a decision against an approval. See the module documentation for the order of checks.
pub fn validate_decision(
    approval: &ApprovalView,
    params: &ApprovalsDecideParams,
    identity: &ClientIdentity,
    now: Timestamp,
) -> ApiResult<ValidatedDecision> {
    let actor = identity.decision_actor(params.channel)?;

    if approval.approval.state != ApprovalState::Pending {
        return Err(ApiError::new(
            zeus_types::codes::API_INVALID_TRANSITION,
            "the approval is already decided or closed",
        ));
    }
    if now >= approval.approval.expires_at {
        return Err(ApiError::new(
            zeus_types::codes::POLICY_APPROVAL_EXPIRED,
            "the approval has expired",
        ));
    }
    if params.request_hash != approval.request_hash {
        return Err(ApiError::new(
            codes::REQUEST_HASH_MISMATCH,
            "the request changed since it was shown; review it again",
        ));
    }

    let allowed = &approval.approval.allowed_scopes;
    let scope_error = || {
        ApiError::new(
            codes::SCOPE_NOT_ALLOWED,
            "that scope is not allowed for this approval",
        )
    };
    let (outcome, scope) = match params.outcome {
        DecideOutcome::Approve => {
            let scope = match (params.scope, allowed.is_empty()) {
                (None, true) => None,
                (Some(scope), false) if allowed.contains(&scope) && scope != GrantScope::Never => {
                    Some(scope)
                }
                _ => return Err(scope_error()),
            };
            (DecisionOutcome::Approve, scope)
        }
        DecideOutcome::Deny => {
            let scope = match params.scope {
                None => None,
                Some(GrantScope::Never) if allowed.contains(&GrantScope::Never) => {
                    Some(GrantScope::Never)
                }
                Some(_) => return Err(scope_error()),
            };
            (DecisionOutcome::Deny, scope)
        }
    };
    Ok(ValidatedDecision {
        outcome,
        scope,
        actor,
    })
}

/// Refuse new approvals when too many are pending, so a hostile page cannot flood the user (01 section 5.3 rule 4).
pub fn check_pending_cap(pending: usize) -> ApiResult<()> {
    if pending >= MAX_PENDING_APPROVALS {
        Err(ApiError::new(
            zeus_types::codes::POLICY_TOO_MANY_PENDING,
            "too many approvals are waiting for a person",
        ))
    } else {
        Ok(())
    }
}

/// Sliding-window limiter for decisions (default 10 per minute, 00 section 10). Time is passed in, so tests are
/// deterministic.
#[derive(Debug, Clone)]
pub struct DecisionLimiter {
    max: usize,
    window_ms: u64,
    stamps: VecDeque<u64>,
}

impl DecisionLimiter {
    /// `max` decisions per `window_ms`.
    pub fn new(max: usize, window_ms: u64) -> Self {
        Self {
            max,
            window_ms,
            stamps: VecDeque::new(),
        }
    }

    /// The default: 10 decisions per minute.
    pub fn per_minute() -> Self {
        Self::new(10, 60_000)
    }

    /// Record a decision at `now_ms` (a monotonic clock), or return how long to wait.
    pub fn try_acquire(&mut self, now_ms: u64) -> Result<(), ApiError> {
        while self
            .stamps
            .front()
            .is_some_and(|&t| now_ms.saturating_sub(t) >= self.window_ms)
        {
            self.stamps.pop_front();
        }
        if self.stamps.len() >= self.max {
            let oldest = self.stamps.front().copied().unwrap_or(now_ms);
            let retry_after_ms = self
                .window_ms
                .saturating_sub(now_ms.saturating_sub(oldest))
                .max(1);
            return Err(
                ApiError::new(codes::RATE_LIMITED, "too many decisions; slow down")
                    .with_detail("retry_after_ms", retry_after_ms),
            );
        }
        self.stamps.push_back(now_ms);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeus_api::auth::CredentialId;
    use zeus_api::{ClientKind, ClientRole};
    use zeus_types::{Approval, ApprovalKind, IdGenerator, Surface};

    fn approval(allowed: &[GrantScope], expires_in_ms: u64) -> ApprovalView {
        let now = Timestamp::now();
        let facts = serde_json::json!({ "origin": "https://portal.example.org", "files": 3, "total_bytes": 1024 });
        ApprovalView {
            approval: serde_json::from_value::<Approval>(serde_json::json!({
                "id": IdGenerator::new().approval_id().to_string(),
                "kind": "session_grant",
                "state": "pending",
                "allowed_scopes": allowed,
                "reason_codes": ["session.first_use"],
                "summary": { "title": "x" },
                "created_at": now.to_rfc3339(),
                "expires_at": now.saturating_add_ms(expires_in_ms).to_rfc3339(),
            }))
            .unwrap(),
            request_hash: facts_hash(&facts).unwrap(),
            facts,
            agent_says: Some("please".into()),
        }
    }

    fn identity(kind: ClientKind, role: ClientRole) -> ClientIdentity {
        ClientIdentity {
            credential_id: CredentialId::generate(),
            kind,
            role,
            permissions: kind.default_permissions(),
            client_name: "t".into(),
        }
    }

    fn params(
        view: &ApprovalView,
        outcome: DecideOutcome,
        scope: Option<GrantScope>,
        channel: Surface,
    ) -> ApprovalsDecideParams {
        ApprovalsDecideParams {
            approval_id: view.approval.id,
            request_hash: view.request_hash,
            outcome,
            scope,
            channel,
        }
    }

    const ALL: &[GrantScope] = &[
        GrantScope::Once,
        GrantScope::Site,
        GrantScope::Workflow,
        GrantScope::Never,
    ];

    #[test]
    fn a_human_on_a_trust_surface_can_approve_with_an_allowed_scope() {
        let view = approval(ALL, 60_000);
        let human = identity(ClientKind::Human, ClientRole::Cli);
        let decision = validate_decision(
            &view,
            &params(
                &view,
                DecideOutcome::Approve,
                Some(GrantScope::Site),
                Surface::CliInteractive,
            ),
            &human,
            Timestamp::now(),
        )
        .unwrap();
        assert_eq!(decision.outcome, DecisionOutcome::Approve);
        assert_eq!(decision.scope, Some(GrantScope::Site));
        assert!(decision.actor.is_human_on_trust_surface());
    }

    #[test]
    fn an_agent_can_never_decide_whatever_surface_it_names() {
        let view = approval(ALL, 60_000);
        let agent = identity(ClientKind::Agent, ClientRole::Mcp);
        for surface in [
            Surface::CliInteractive,
            Surface::ExtensionPopup,
            Surface::Mcp,
            Surface::Jsonrpc,
        ] {
            let error = validate_decision(
                &view,
                &params(
                    &view,
                    DecideOutcome::Approve,
                    Some(GrantScope::Once),
                    surface,
                ),
                &agent,
                Timestamp::now(),
            )
            .unwrap_err();
            assert_eq!(
                error.envelope().code.as_str(),
                codes::HUMAN_REQUIRED,
                "{surface:?}"
            );
        }
    }

    #[test]
    fn a_human_client_on_a_non_trust_surface_is_refused() {
        let view = approval(ALL, 60_000);
        let human = identity(ClientKind::Human, ClientRole::Cli);
        let error = validate_decision(
            &view,
            &params(
                &view,
                DecideOutcome::Approve,
                Some(GrantScope::Once),
                Surface::Mcp,
            ),
            &human,
            Timestamp::now(),
        )
        .unwrap_err();
        assert_eq!(error.envelope().code.as_str(), codes::HUMAN_REQUIRED);
    }

    #[test]
    fn a_stale_or_forged_request_hash_is_refused() {
        let view = approval(ALL, 60_000);
        let human = identity(ClientKind::Human, ClientRole::Cli);
        let mut p = params(
            &view,
            DecideOutcome::Approve,
            Some(GrantScope::Once),
            Surface::Tui,
        );
        p.request_hash = facts_hash(&serde_json::json!({ "origin": "https://evil.example", "files": 3, "total_bytes": 1024 })).unwrap();
        let error = validate_decision(&view, &p, &human, Timestamp::now()).unwrap_err();
        assert_eq!(error.envelope().code.as_str(), codes::REQUEST_HASH_MISMATCH);
    }

    #[test]
    fn decided_or_expired_approvals_cannot_be_decided() {
        let human = identity(ClientKind::Human, ClientRole::Cli);
        let mut view = approval(ALL, 60_000);
        let p = params(
            &view,
            DecideOutcome::Approve,
            Some(GrantScope::Once),
            Surface::Tui,
        );
        view.approval.state = ApprovalState::Approved;
        assert_eq!(
            validate_decision(&view, &p, &human, Timestamp::now())
                .unwrap_err()
                .envelope()
                .code
                .as_str(),
            zeus_types::codes::API_INVALID_TRANSITION
        );
        let view = approval(ALL, 1);
        let p = params(
            &view,
            DecideOutcome::Approve,
            Some(GrantScope::Once),
            Surface::Tui,
        );
        let later = Timestamp::now().saturating_add_ms(10);
        assert_eq!(
            validate_decision(&view, &p, &human, later)
                .unwrap_err()
                .envelope()
                .code
                .as_str(),
            zeus_types::codes::POLICY_APPROVAL_EXPIRED
        );
    }

    #[test]
    fn scope_rules() {
        let human = identity(ClientKind::Human, ClientRole::Cli);
        let handoff = approval(
            &[GrantScope::Once, GrantScope::Site, GrantScope::Never],
            60_000,
        );
        let now = Timestamp::now();
        let check = |view: &ApprovalView, outcome, scope| {
            validate_decision(
                view,
                &params(view, outcome, scope, Surface::CliInteractive),
                &human,
                now,
            )
        };
        assert!(check(&handoff, DecideOutcome::Approve, Some(GrantScope::Workflow)).is_err());
        assert!(check(&handoff, DecideOutcome::Approve, None).is_err());
        assert!(
            check(&handoff, DecideOutcome::Approve, Some(GrantScope::Never)).is_err(),
            "approving with scope never is nonsense"
        );
        assert!(check(&handoff, DecideOutcome::Deny, Some(GrantScope::Never)).is_ok());
        assert!(check(&handoff, DecideOutcome::Deny, None).is_ok());
        assert!(check(&handoff, DecideOutcome::Deny, Some(GrantScope::Site)).is_err());

        let plain = approval(&[], 60_000);
        assert!(check(&plain, DecideOutcome::Approve, None).is_ok());
        assert!(check(&plain, DecideOutcome::Approve, Some(GrantScope::Once)).is_err());
        let _ = ApprovalKind::Handoff;
    }

    #[test]
    fn the_facts_hash_ignores_key_order_and_rejects_floats() {
        let a = facts_hash(&serde_json::json!({"a": 1, "b": [1, 2]})).unwrap();
        let b = facts_hash(&serde_json::json!({"b": [1, 2], "a": 1})).unwrap();
        assert_eq!(a, b);
        assert!(facts_hash(&serde_json::json!({"ratio": 0.5})).is_err());
    }

    #[test]
    fn pending_cap_and_rate_limit() {
        assert!(check_pending_cap(MAX_PENDING_APPROVALS - 1).is_ok());
        assert!(check_pending_cap(MAX_PENDING_APPROVALS).is_err());

        let mut limiter = DecisionLimiter::new(2, 1000);
        limiter.try_acquire(0).unwrap();
        limiter.try_acquire(100).unwrap();
        let error = limiter.try_acquire(200).unwrap_err();
        assert_eq!(error.envelope().code.as_str(), codes::RATE_LIMITED);
        assert_eq!(
            error.envelope().details.as_ref().unwrap()["retry_after_ms"],
            800
        );
        limiter.try_acquire(1000).unwrap();
        assert!(limiter.try_acquire(1050).is_err());
    }
}
