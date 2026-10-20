fn handoff_request(requester: &ClientIdentity, n: u64, profile: Option<&str>) -> ApprovalRequest {
    let id = IdGenerator::new().job_id();
    let spec = serde_json::from_value(serde_json::json!({
        "url": format!("https://portal.example.org/files/{n}.pdf"),
        "session_origin": "https://portal.example.org", "filename": format!("paper-{n}.pdf"),
        "expected_size": 1024 + n,
        "source": {"kind":"browser_handoff", "via":"native_messaging",
            "client_id":requester.credential_id.as_str(), "browser": {
                "name":"chrome", "profile_id": profile, "incognito":false,
                "download_id":n, "offer_key":format!("chrome:{n}:2026-10-21T00:00:00.000Z")
            }}
    }))
    .unwrap();
    ApprovalRequest {
        job: Some(Job::new(id, spec, JobState::Queued, Timestamp::now())),
        dest_path: Some(format!("/downloads/paper-{n}.pdf")),
        kind: ApprovalKind::Handoff,
        job_id: Some(id),
        run_id: None,
        origin: Some(Origin::parse("https://portal.example.org").unwrap()),
        allowed_scopes: vec![GrantScope::Once, GrantScope::Site, GrantScope::Never],
        reason_codes: vec!["handoff.interrupted".into()],
        summary: ApprovalSummary {
            title: "paper".into(),
            url_redacted: None,
            total_bytes: None,
            source_kind: Some(SourceKind::BrowserHandoff),
            requested_by: Some(requester.credential_id.to_string()),
        },
    }
}

#[tokio::test]
async fn browser_handoffs_batch_only_with_the_same_bound_browser_context() {
    let temp = tempfile::tempdir().unwrap();
    let (manager, identity) = setup_with_identities(temp.path());
    let requester = identity(ClientKind::Agent, ClientRole::Mcp);
    let other = identity(ClientKind::Agent, ClientRole::Mcp);
    let profile = Some("11111111111111111111111111111111");
    let first = manager
        .request(handoff_request(&requester, 1, profile))
        .await
        .unwrap();
    manager.store(&first).unwrap();
    let second = manager
        .request(handoff_request(&requester, 2, profile))
        .await
        .unwrap();
    manager.store(&second).unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(second.revision, Some(2));
    assert_eq!(first.expires_at, second.expires_at);
    assert_ne!(first.request_hash, second.request_hash);
    let view = ApprovalView::try_from(second.clone()).unwrap();
    assert_eq!(view.facts["transfer"]["total_bytes"], 2051);
    assert_eq!(
        view.facts["transfer"]["job_ids"].as_array().unwrap().len(),
        2
    );
    zeus_types::spec_testing::SpecSchemas::load()
        .check(
            true,
            "approvals.v1",
            "ApprovalRequest",
            &serde_json::to_value(view).unwrap(),
        )
        .unwrap();
    let mut requests = vec![
        handoff_request(&requester, 3, Some("22222222222222222222222222222222")),
        handoff_request(&other, 4, profile),
        handoff_request(&requester, 5, None),
    ];
    let mut firefox = handoff_request(&requester, 6, profile);
    firefox
        .job
        .as_mut()
        .unwrap()
        .spec
        .source
        .browser
        .as_mut()
        .unwrap()
        .name = BrowserName::Firefox;
    requests.push(firefox);
    let mut private = handoff_request(&requester, 7, profile);
    private
        .job
        .as_mut()
        .unwrap()
        .spec
        .source
        .browser
        .as_mut()
        .unwrap()
        .incognito = true;
    requests.push(private);
    let mut other_origin = handoff_request(&requester, 8, profile);
    other_origin.origin = Some(Origin::parse("https://other.example.org").unwrap());
    requests.push(other_origin);
    for request in requests {
        let separate = manager.request(request).await.unwrap();
        assert_ne!(separate.id, first.id);
    }
}

#[tokio::test]
async fn handoff_offer_replay_keeps_its_revision_and_stale_review_cannot_grant() {
    let temp = tempfile::tempdir().unwrap();
    let (manager, requester, human) = setup(temp.path());
    let profile = Some("11111111111111111111111111111111");
    let request = handoff_request(&requester, 1, profile);
    let first = manager.request(request.clone()).await.unwrap();
    manager.store(&first).unwrap();
    let replay = manager.request(request).await.unwrap();
    assert_eq!(replay, first);
    let second = manager
        .request(handoff_request(&requester, 2, profile))
        .await
        .unwrap();
    manager.store(&second).unwrap();
    let stale = ApprovalsDecideParams {
        approval_id: first.id,
        request_hash: first.request_hash.unwrap(),
        outcome: DecideOutcome::Approve,
        scope: Some(GrantScope::Once),
        channel: Surface::CliInteractive,
        confirmation: ConfirmationLevel::Gesture,
        idempotency_key: None,
    };
    let refused = manager
        .decide(&human, &stale, evidence(&human, &stale))
        .await
        .unwrap_err();
    assert_eq!(
        refused.envelope().code.as_str(),
        "policy.request_hash_mismatch"
    );
    assert_eq!(
        refused.envelope().details.as_ref().unwrap()["current_revision"],
        2
    );
    assert!(manager.journal.read(|tx| tx.grants()).unwrap().is_empty());
    manager.withdraw(second.id).await.unwrap();
    let withdrawn = manager.get(&human, second.id).unwrap();
    assert_eq!(withdrawn.state, ApprovalState::Canceled);
    let record = manager
        .journal
        .read(|tx| tx.approval_decision(second.id))
        .unwrap()
        .unwrap();
    assert_eq!(record["system_reason"], "request_withdrawn");
    manager.withdraw(second.id).await.unwrap();
    assert_eq!(manager.get(&human, second.id).unwrap(), withdrawn);
}

#[tokio::test]
async fn handoff_batches_roll_over_after_fifty_jobs() {
    let temp = tempfile::tempdir().unwrap();
    let (manager, requester, _) = setup(temp.path());
    let mut first_id = None;
    for n in 0..51 {
        let approval = manager
            .request(handoff_request(
                &requester,
                n,
                Some("11111111111111111111111111111111"),
            ))
            .await
            .unwrap();
        if n < 50 {
            assert_eq!(*first_id.get_or_insert(approval.id), approval.id);
            assert_eq!(approval.revision, Some(n as u32 + 1));
        } else {
            assert_ne!(first_id.unwrap(), approval.id);
            assert_eq!(approval.revision, Some(1));
        }
        manager.store(&approval).unwrap();
    }
}
