use zeus_types::GrantScope;

async fn approval_batch_fixture(
    root: &Path,
) -> (
    Arc<RuntimeBackend>,
    CallContext,
    CallContext,
    Vec<zeus_types::JobId>,
    zeus_types::ApprovalId,
) {
    let backend = setup(root).await;
    let (agent, _) = context(root, "agent");
    let (human, _) = context(root, "cli");
    let enqueued = backend.jobs_enqueue(&agent, serde_json::from_value(serde_json::json!({"jobs":[
        {"url":"https://portal.example.org/a.pdf?token=ZEUS-CANARY-CANCELED", "filename":"a.pdf", "expected_size":10, "session_origin":"https://portal.example.org"},
        {"url":"https://portal.example.org/b.pdf", "filename":"b.pdf", "session_origin":"https://portal.example.org"},
        {"url":"https://portal.example.org/c.pdf", "filename":"c.pdf", "expected_size":30, "session_origin":"https://portal.example.org"}
    ]})).unwrap()).await.unwrap();
    let ids = enqueued
        .jobs
        .iter()
        .map(|job| job.job_id.unwrap())
        .collect();
    let approval = enqueued.jobs[0].approval_id.unwrap();
    (backend, agent, human, ids, approval)
}

async fn cancel_approval_job(
    backend: &RuntimeBackend,
    ctx: &CallContext,
    job_id: zeus_types::JobId,
) {
    backend
        .jobs_control(
            ctx,
            JobsControlParams {
                job_id,
                action: ControlAction::Cancel,
                priority: None,
                idempotency_key: None,
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn canceling_a_batch_member_rebinds_facts_and_rejects_the_previous_review() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, agent, human, jobs, id) = approval_batch_fixture(dir.path()).await;
    let before = backend.consent.get(&human.identity, id).unwrap();
    let cursor = backend.journal.meta().unwrap().last_event_seq;
    cancel_approval_job(&backend, &agent, jobs[0]).await;
    let after = backend.consent.get(&human.identity, id).unwrap();
    assert_eq!(after.state, ApprovalState::Pending);
    assert_eq!(after.revision, Some(4));
    assert_eq!(after.expires_at, before.expires_at);
    assert_eq!(after.job_id, Some(jobs[1]));
    assert_eq!(after.summary.title, "b.pdf");
    assert_eq!(after.summary.total_bytes, Some(30));
    let view = ApprovalView::try_from(after).unwrap();
    assert_eq!(
        view.request_hash,
        crate::approvals::facts_hash(&view.facts).unwrap()
    );
    assert_eq!(
        view.facts["transfer"]["job_ids"],
        serde_json::json!([jobs[1], jobs[2]])
    );
    assert_eq!(view.facts["transfer"]["total_bytes"], 30);
    assert_eq!(view.facts["transfer"]["unknown_size_count"], 1);
    assert_eq!(view.facts["items"].as_array().unwrap().len(), 2);
    assert_eq!(view.facts["items_omitted"], 0);
    zeus_types::spec_testing::SpecSchemas::load()
        .check(
            true,
            "approvals.v1",
            "ApprovalRequest",
            &serde_json::to_value(&view).unwrap(),
        )
        .unwrap();
    let events = backend
        .journal
        .read(|tx| tx.events_since(zeus_types::EventSeq::new(cursor).ok(), 20))
        .unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.payload.type_name())
            .collect::<Vec<_>>(),
        ["approval.requested", "job.state_changed"]
    );
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("ZEUS-CANARY-")
    );
    if let zeus_types::EventPayload::ApprovalRequested(event) = &events[0].payload {
        assert!(event.approval.facts.is_none());
        assert!(event.approval.agent_says.is_none());
    } else {
        panic!("expected a public approval amendment");
    }
    let p = ApprovalsDecideParams {
        approval_id: id,
        request_hash: before.request_hash.unwrap(),
        outcome: DecideOutcome::Approve,
        scope: Some(GrantScope::Once),
        channel: Surface::CliInteractive,
        confirmation: ConfirmationLevel::Gesture,
        idempotency_key: None,
    };
    let evidence = serde_json::json!({"client_id":human.identity.credential_id.as_str(),"surface":"cli_interactive","exe_verified":true});
    let refused = backend
        .consent
        .decide(&human.identity, &p, evidence)
        .await
        .unwrap_err();
    assert_eq!(
        refused.envelope().code.as_str(),
        "policy.request_hash_mismatch"
    );
    assert_eq!(
        refused.envelope().details.as_ref().unwrap()["current_revision"],
        4
    );
    assert!(backend.journal.read(|tx| tx.grants()).unwrap().is_empty());
    backend.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_last_batch_departure_commits_one_cancel_record_with_s32() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, agent, human, jobs, id) = approval_batch_fixture(dir.path()).await;
    for job in &jobs[..2] {
        cancel_approval_job(&backend, &agent, *job).await;
    }
    let cursor = backend.journal.meta().unwrap().last_event_seq;
    cancel_approval_job(&backend, &agent, jobs[2]).await;
    assert_eq!(
        backend.consent.get(&human.identity, id).unwrap().state,
        ApprovalState::Canceled
    );
    let record = backend
        .journal
        .read(|tx| tx.approval_decision(id))
        .unwrap()
        .unwrap();
    assert_eq!(record["system_reason"], "job_canceled");
    zeus_types::spec_testing::SpecSchemas::load()
        .check(true, "approvals.v1", "DecisionRecord", &record)
        .unwrap();
    let events = backend
        .journal
        .read(|tx| tx.events_since(zeus_types::EventSeq::new(cursor).ok(), 20))
        .unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.payload.type_name())
            .collect::<Vec<_>>(),
        ["approval.decided", "job.state_changed"]
    );
    assert!(
        jobs.iter()
            .all(|id| backend.journal.job(*id).unwrap().unwrap().job.state == JobState::Canceled)
    );
    backend.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn exhausted_batch_revisions_roll_to_a_fresh_binding_without_preventing_cancel() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, agent, human, jobs, old_id) = approval_batch_fixture(dir.path()).await;
    let mut before = backend.consent.get(&human.identity, old_id).unwrap();
    before.revision = Some(1000);
    before
        .facts
        .as_mut()
        .unwrap()
        .insert("revision".into(), serde_json::json!(1000));
    before.request_hash = Some(
        crate::approvals::facts_hash(&serde_json::Value::Object(before.facts.clone().unwrap()))
            .unwrap(),
    );
    backend
        .journal
        .transact(|tx| tx.put_approval(&before))
        .unwrap();
    let cursor = backend.journal.meta().unwrap().last_event_seq;
    cancel_approval_job(&backend, &agent, jobs[0]).await;
    let waiting = backend.journal.job(jobs[1]).unwrap().unwrap();
    let new_id = waiting.job.approval_id.unwrap();
    assert_ne!(new_id, old_id);
    let fresh = backend.consent.get(&human.identity, new_id).unwrap();
    assert_eq!(fresh.state, ApprovalState::Pending);
    assert_eq!(fresh.revision, Some(1));
    assert_eq!(fresh.expires_at, before.expires_at);
    assert_eq!(fresh.allowed_scopes, before.allowed_scopes);
    assert_eq!(fresh.job_id, Some(jobs[1]));
    assert_eq!(
        backend
            .journal
            .job(jobs[2])
            .unwrap()
            .unwrap()
            .job
            .approval_id,
        Some(new_id)
    );
    assert_eq!(
        backend.consent.get(&human.identity, old_id).unwrap().state,
        ApprovalState::Canceled
    );
    assert_eq!(
        backend
            .journal
            .read(|tx| tx.approval_decision(old_id))
            .unwrap()
            .unwrap()["system_reason"],
        "request_withdrawn"
    );
    let view = ApprovalView::try_from(fresh).unwrap();
    assert_eq!(view.facts["approval_id"], new_id.to_string());
    assert_eq!(
        view.request_hash,
        crate::approvals::facts_hash(&view.facts).unwrap()
    );
    zeus_types::spec_testing::SpecSchemas::load()
        .check(
            true,
            "approvals.v1",
            "ApprovalRequest",
            &serde_json::to_value(view).unwrap(),
        )
        .unwrap();
    let events = backend
        .journal
        .read(|tx| tx.events_since(zeus_types::EventSeq::new(cursor).ok(), 20))
        .unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.payload.type_name())
            .collect::<Vec<_>>(),
        [
            "approval.decided",
            "approval.requested",
            "job.state_changed"
        ]
    );
    assert_eq!(
        backend.engine.get(jobs[0]).await.unwrap().state,
        JobState::Canceled
    );
    backend.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn narrowed_native_relay_keeps_batched_handoffs_and_attested_popup_decisions() {
    use zeus_session::protocol as nm;
    let dir = tempfile::tempdir().unwrap();
    let backend = setup(dir.path()).await;
    let (mut ctx, _rx) = context(dir.path(), "host");
    assert_eq!(
        ctx.identity.permissions,
        zeus_api::ClientRole::native_host_permissions()
            .iter()
            .copied()
            .collect()
    );
    assert!(
        zeus_api::policy::authorize(
            Some(&ctx.identity),
            zeus_api::policy::lookup("approvals.decide").unwrap(),
            &serde_json::json!({})
        )
        .is_err()
    );
    let extension = "abcdefghijklmnopabcdefghijklmnop";
    ctx.attach = Some(zeus_api::methods::daemon::AttachInfo {
        browser: zeus_types::BrowserName::Chrome,
        attested_extension: extension.into(),
        host_version: "0.1.0".into(),
        parent_verified: true,
    });
    let hello = serde_json::from_value(serde_json::json!({
        "client":"zeus-extension", "extension_id":extension, "extension_version":"0.1.0",
        "browser":{"name":"chrome","version":"130"}, "protocol_versions":[1],
        "capabilities":["handoff","approvals"], "profile_id":"11111111111111111111111111111111",
        "sw_instance_id":"2222222222222222"
    }))
    .unwrap();
    let connection = backend.browser.attach(ctx, hello, true).unwrap();
    let mut approval_id = None;
    let mut jobs = Vec::new();
    for n in [1, 2, 1] {
        let offer = serde_json::from_value(serde_json::json!({
            "offer_key":format!("chrome:{n}:2026-10-21T00:00:00.000Z"), "trigger":"user_initiated",
            "download":{"browser_download_id":n, "url":format!("https://portal.example.org/{n}.pdf"),
                "final_url":format!("https://portal.example.org/{n}.pdf"), "referrer":null,
                "filename":format!("paper-{n}.pdf"), "mime":null, "bytes_received":0,
                "total_bytes":100, "start_time":"2026-10-21T00:00:00.000Z",
                "incognito":false,"can_resume":false,"interrupt_reason":null}
        })).unwrap();
        let response = backend
            .native_request(
                &connection,
                &nm::MessageId::new(format!("offer{n}")).unwrap(),
                nm::ExtensionBody::HandoffOffer(offer),
            )
            .await
            .unwrap()
            .unwrap();
        let nm::DaemonBody::HandoffAccept(accept) = response else {
            panic!("expected handoff acceptance");
        };
        if let Some(id) = accept.approval_id {
            assert_eq!(*approval_id.get_or_insert(id), id);
        } else {
            assert!(accept.deduplicated);
            assert_eq!(accept.job_id, jobs[0]);
        }
        jobs.push(accept.job_id);
    }
    assert_eq!(jobs[0], jobs[2]);
    let approval_id = approval_id.unwrap();
    let approval = backend
        .consent
        .get(&connection.ctx.identity, approval_id)
        .unwrap();
    assert_eq!(approval.revision, Some(2));
    let mut decide = nm::ApprovalDecideBody {
        approval_id,
        request_hash: approval.request_hash.unwrap(),
        confirmation: Some(nm::ConfirmationLevel::Gesture),
        decision: nm::Decision::Deny,
        scope: None,
        ui: nm::DecideUi {
            surface: nm::ExtensionSurface::ExtensionPopup,
            user_gesture: false,
        },
    };
    assert_eq!(
        backend
            .native_request(
                &connection,
                &nm::MessageId::new("decision1").unwrap(),
                nm::ExtensionBody::ApprovalDecide(decide.clone())
            )
            .await
            .unwrap_err()
            .envelope()
            .code
            .as_str(),
        "api.human_required"
    );
    decide.ui.user_gesture = true;
    assert!(
        backend
            .native_request(
                &connection,
                &nm::MessageId::new("decision2").unwrap(),
                nm::ExtensionBody::ApprovalDecide(decide)
            )
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        backend
            .consent
            .get(&connection.ctx.identity, approval_id)
            .unwrap()
            .state,
        ApprovalState::Denied
    );
    let record = backend
        .journal
        .read(|tx| tx.approval_decision(approval_id))
        .unwrap()
        .unwrap();
    assert_eq!(
        record["evidence"]["extension"]["profile_id"],
        "11111111111111111111111111111111"
    );
    for job in &jobs[..2] {
        assert_eq!(
            backend.engine.get(*job).await.unwrap().state,
            JobState::Failed
        );
    }
    backend.browser.disconnect(connection.ctx.connection_id);
    backend.engine.shutdown().await.unwrap();
}
