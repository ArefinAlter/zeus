#[tokio::test]
async fn browser_network_review_is_bound_owner_scoped_and_never_creates_a_session_grant() {
    let temp = tempfile::tempdir().unwrap();
    let (manager, agent, human) = setup(temp.path());
    let id = zeus_workflow::PlanId::parse("plan_01M45NHJ2AHWMXT8JVJ7BEEJ01").unwrap();
    let scope = serde_json::json!({"request_blake3":"1".repeat(64),"origin":"https://portal.example.org","grant_id":IdGenerator::new().grant_id(),
        "profile_id":"a".repeat(32),"start_urls":["https://portal.example.org/private"],"scope":"start and discovered pages in this origin",
        "max_reads":32,"max_body_bytes":524288,"deadline_ms":25000,"native_peer_pinning":false,"redirects":"deny","network":"Browser networking has no native peer pinning."});
    let first = manager
        .request_browser_documents(&agent, &id, scope.clone())
        .await
        .unwrap();
    assert_eq!(
        first,
        manager
            .request_browser_documents(&agent, &id, scope.clone())
            .await
            .unwrap()
    );
    let other = manager
        .request_browser_documents(&human, &id, scope.clone())
        .await
        .unwrap();
    assert_ne!(first, other);
    let approval = manager.get(&human, first).unwrap();
    assert_eq!(approval.kind, ApprovalKind::PolicyException);
    assert!(approval.allowed_scopes.is_empty());
    let view = ApprovalView::try_from(approval.clone()).unwrap();
    zeus_types::spec_testing::SpecSchemas::load()
        .check(
            true,
            "approvals.v1",
            "ApprovalRequest",
            &serde_json::to_value(view).unwrap(),
        )
        .unwrap();
    let params = ApprovalsDecideParams {
        approval_id: first,
        request_hash: approval.request_hash.unwrap(),
        outcome: DecideOutcome::Approve,
        scope: None,
        channel: Surface::CliInteractive,
        confirmation: ConfirmationLevel::TypedCode,
        idempotency_key: None,
    };
    assert!(
        manager
            .decide(&agent, &params, evidence(&agent, &params))
            .await
            .is_err()
    );
    let (approved, grant) = manager
        .decide(&human, &params, evidence(&human, &params))
        .await
        .unwrap();
    assert_eq!(approved.state, ApprovalState::Approved);
    assert!(grant.is_none());
    assert!(manager.journal.read(|tx| tx.grants()).unwrap().is_empty());
    let mut changed = scope;
    changed["request_blake3"] = serde_json::json!("2".repeat(64));
    assert_ne!(
        first,
        manager
            .request_browser_documents(&agent, &id, changed)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn terminal_workflow_review_binds_the_exact_browser_profile_and_plan() {
    let temp = tempfile::tempdir().unwrap();
    let (manager, agent, human) = setup(temp.path());
    let id = zeus_workflow::PlanId::parse("plan_01M45NHJ2AHWMXT8JVJ7BEEJ01").unwrap();
    let run = IdGenerator::new().run_id();
    // Hashed Plan fences are intentionally outside the core fixture copier's plain-fence set.
    let example = include_str!("../../../docs/spec/08-workflow-plan-v0.md")
        .split("```json validate=workflow-plan.v0#/$defs/Plan")
        .nth(2)
        .unwrap()
        .split_once('\n')
        .unwrap()
        .1
        .split("```")
        .next()
        .unwrap();
    let mut plan: zeus_workflow::Plan = serde_json::from_str(example).unwrap();
    plan.session = Some(zeus_workflow::SessionRequirement {
        origin: "https://portal.example.org".parse().unwrap(),
        scope: GrantScope::Workflow,
        document_mode: Some(zeus_workflow::SessionDocumentMode::BrowserOrigin),
        profile_id: Some("a".repeat(32)),
    });
    let hash = plan.plan_hash().unwrap();
    let dry: zeus_workflow::DryRunReport = serde_json::from_value(serde_json::json!({"complete":true,"plan_hash":hash,"generated_at":Timestamp::now(),"sampled_pages":1,"sample":[],"name_collisions":0,"warnings":[]})).unwrap();
    let approval = manager
        .request_workflow(&id, run, &hash, &plan, &dry, &agent)
        .await
        .unwrap();
    assert_eq!(approval.facts["browser_profile_id"], "a".repeat(32));
    zeus_types::spec_testing::SpecSchemas::load()
        .check(
            true,
            "approvals.v1",
            "ApprovalRequest",
            &serde_json::to_value(&approval).unwrap(),
        )
        .unwrap();
    let params = ApprovalsDecideParams {
        approval_id: approval.approval.id,
        request_hash: approval.request_hash,
        outcome: DecideOutcome::Approve,
        scope: Some(GrantScope::Workflow),
        channel: Surface::CliInteractive,
        confirmation: ConfirmationLevel::TypedCode,
        idempotency_key: None,
    };
    let (_, grant) = manager
        .decide(&human, &params, evidence(&human, &params))
        .await
        .unwrap();
    assert_eq!(grant.unwrap().run_id, Some(run));
    for profile in [false, true] {
        let mut changed = plan.clone();
        if profile {
            changed.session.as_mut().unwrap().profile_id = Some("b".repeat(32));
        } else {
            changed.name.push_str(" changed");
        }
        let changed_hash = changed.plan_hash().unwrap();
        assert_ne!(hash, changed_hash);
        let mut changed_dry = dry.clone();
        changed_dry.plan_hash = changed_hash.clone();
        assert!(
            manager
                .request_workflow(&id, run, &changed_hash, &changed, &changed_dry, &agent)
                .await
                .is_err()
        );
    }
}
