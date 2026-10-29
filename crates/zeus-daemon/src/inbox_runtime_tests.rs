use super::tests::{context, setup};
use super::*;
async fn package(
    backend: &RuntimeBackend,
    ctx: &CallContext,
    entries: serde_json::Value,
) -> InboxPackageView {
    let pkg = backend
        .inbox_package_put(
            ctx,
            serde_json::from_value(
                serde_json::json!({"untrusted_name":"Runtime fixture","idempotency_key":"create"}),
            )
            .unwrap(),
        )
        .await
        .unwrap()
        .package;
    backend.inbox_entries_add(ctx,serde_json::from_value(serde_json::json!({"package_id":pkg.id,"expected_revision":pkg.revision,"entries":entries,"idempotency_key":"add"})).unwrap()).await.unwrap().package
}
#[tokio::test]
async fn inbox_preview_uses_current_scopes_collision_facts_and_bounded_batches() {
    let dir = tempfile::tempdir().unwrap();
    let backend = setup(dir.path()).await;
    let (ctx, _rx) = context(dir.path(), "agent");
    std::fs::write(dir.path().join("downloads/occupied.bin"), b"keep").unwrap();
    let entries=(0..51).map(|i|serde_json::json!({"job":{"url":format!("https://example.org/{i}"),"filename":if i==0{"occupied.bin".into()}else{format!("item-{i}.bin")}}})).collect::<Vec<_>>();
    let pkg = package(&backend, &ctx, entries.into()).await;
    let preview = backend
        .inbox_preview(
            &ctx,
            InboxPreviewParams {
                package_id: pkg.id.clone(),
                expected_revision: pkg.revision,
                entry_ids: vec![],
                idempotency_key: "review".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(preview.entries.len(), 50);
    assert_eq!(preview.ready_entries, 49);
    assert_eq!(preview.remaining_pending, 1);
    assert!(preview.entries[0].error.is_some());
    assert_eq!(
        std::fs::read(dir.path().join("downloads/occupied.bin")).unwrap(),
        b"keep"
    );
    let (other, _rx) = context(dir.path(), "cli");
    assert!(
        backend
            .inbox_package_get(
                &other,
                InboxPackageGetParams {
                    include_history: false,
                    owner_credential_id: None,
                    package_id: pkg.id,
                    page: Default::default(),
                    pending_only: false
                }
            )
            .await
            .is_err()
    );
    let inspect = InboxPackageGetParams {
        include_history: true,
        owner_credential_id: Some(ctx.identity.credential_id.to_string()),
        package_id: preview.package_id.clone(),
        page: zeus_api::PageRequest {
            limit: Some(100),
            cursor: None,
        },
        pending_only: false,
    };
    assert_eq!(
        backend
            .inbox_package_get(&ctx, inspect.clone())
            .await
            .unwrap_err()
            .envelope()
            .code
            .as_str(),
        "api.human_required"
    );
    let human_view = backend.inbox_package_get(&other, inspect).await.unwrap();
    assert_eq!(human_view.previews[0].preview_id, preview.preview_id);
    assert_eq!(human_view.entries.items.len(), 51);
    assert!(human_view.previews.iter().all(|v| {
        v.entries
            .iter()
            .all(|e| !e.redacted_url.to_string().contains("token="))
    }));
    backend.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn inbox_preview_reports_scope_failure_without_network_or_authority_grant() {
    let dir = tempfile::tempdir().unwrap();
    let backend = setup(dir.path()).await;
    let (ctx, _rx) = context(dir.path(), "agent");
    let pkg=package(&backend,&ctx,serde_json::json!([{"job":{"url":"https://example.org/private?token=secret","dest_dir":dir.path().join("outside").display().to_string(),"filename":"a.bin"}}])).await;
    let preview = backend
        .inbox_preview(
            &ctx,
            InboxPreviewParams {
                package_id: pkg.id,
                expected_revision: pkg.revision,
                entry_ids: vec![],
                idempotency_key: "review".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(preview.ready_entries, 0);
    assert!(preview.entries[0].error.is_some());
    assert!(!serde_json::to_string(&preview).unwrap().contains("secret"));
    assert!(!dir.path().join("outside").exists());
    backend.engine.shutdown().await.unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbox_pending_dispatch_reconciles_existing_enqueue_receipt_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let backend = setup(dir.path()).await;
    let (ctx, _rx) = context(dir.path(), "agent");
    let pkg=package(&backend,&ctx,serde_json::json!([{"job":{"url":"http://127.0.0.1:9/pending-fixture","filename":"fixture.bin"}}])).await;
    let preview = backend
        .inbox_preview(
            &ctx,
            InboxPreviewParams {
                package_id: pkg.id.clone(),
                expected_revision: pkg.revision,
                entry_ids: vec![],
                idempotency_key: "review".into(),
            },
        )
        .await
        .unwrap();
    let p = InboxSubmitParams {
        package_id: pkg.id.clone(),
        expected_revision: pkg.revision,
        preview_id: preview.preview_id,
        idempotency_key: "submit".into(),
    };
    let (_pkg, batch) = backend
        .inbox
        .begin(ctx.identity.credential_id.as_str(), &p)
        .unwrap();
    // Simulate loss of the inbox completion after the real enqueue API durably accepted its batch.
    let accepted = backend
        .jobs_enqueue(&ctx, batch.params.clone())
        .await
        .unwrap();
    let accepted_id = accepted.jobs[0].job_id;
    assert!(
        accepted_id.is_some(),
        "the real enqueue must accept a stable job identity"
    );
    backend.engine.shutdown().await.unwrap();
    drop(backend);
    let backend = setup(dir.path()).await;
    let (ctx, _rx) = context(dir.path(), "agent");
    let result = backend
        .inbox_submission_retry(
            &ctx,
            InboxSubmissionRetryParams {
                package_id: pkg.id.clone(),
                batch_id: batch.id.clone(),
                idempotency_key: "retry".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(result.entries[0].result.job_id, accepted_id);
    assert_eq!(result.entries[0].result.error, accepted.jobs[0].error);
    let replay = backend.inbox_submit(&ctx, p).await.unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.batch_id, batch.id);
    assert!(
        backend
            .inbox
            .load(ctx.identity.credential_id.as_str(), &pkg.id)
            .unwrap()
            .view
            .active_batch
            .is_none()
    );
    backend.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn inbox_rule_changes_invalidate_review_and_outside_destinations_never_gain_scope() {
    let dir = tempfile::tempdir().unwrap();
    let backend = setup(dir.path()).await;
    let (ctx, _rx) = context(dir.path(), "agent");
    let pkg = package(
        &backend,
        &ctx,
        serde_json::json!([{"job":{"url":"https://example.org/report.pdf"}}]),
    )
    .await;
    let configured=backend.inbox_package_put(&ctx,serde_json::from_value(serde_json::json!({"package_id":pkg.id,"expected_revision":pkg.revision,"untrusted_name":"Runtime fixture","rules":[{"id":"papers","filename_suffix":".pdf","filename_prefix":"paper_"}],"idempotency_key":"rules"})).unwrap()).await.unwrap().package;
    let preview = backend
        .inbox_preview(
            &ctx,
            InboxPreviewParams {
                package_id: pkg.id.clone(),
                expected_revision: configured.revision,
                entry_ids: vec![],
                idempotency_key: "rule-preview".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(preview.ready_entries, 1);
    assert_eq!(preview.entries[0].matched_rule.as_deref(), Some("papers"));
    assert_eq!(
        preview.entries[0].filename.as_deref(),
        Some("paper_report.pdf")
    );
    let outside = tempfile::tempdir().unwrap();
    let changed=backend.inbox_package_put(&ctx,serde_json::from_value(serde_json::json!({"package_id":pkg.id,"expected_revision":configured.revision,"untrusted_name":"Runtime fixture","rules":[{"id":"outside","filename_suffix":".pdf","dest_dir":outside.path()}],"idempotency_key":"outside-rules"})).unwrap()).await.unwrap().package;
    let stale = backend
        .inbox_submit(
            &ctx,
            InboxSubmitParams {
                package_id: pkg.id.clone(),
                expected_revision: preview.package_revision,
                preview_id: preview.preview_id,
                idempotency_key: "stale-rule-submit".into(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(stale.envelope().code.as_str(), "api.conflict");
    let refused = backend
        .inbox_preview(
            &ctx,
            InboxPreviewParams {
                package_id: pkg.id,
                expected_revision: changed.revision,
                entry_ids: vec![],
                idempotency_key: "outside-preview".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(refused.ready_entries, 0);
    assert!(refused.entries[0].error.is_some());
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}
