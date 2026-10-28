use super::*;
fn create(store: &InboxStore, owner: &str) -> InboxPackageView {
    store
        .put(
            owner,
            &InboxPackagePutParams {
                package_id: None,
                expected_revision: None,
                untrusted_name: "Fixture package".into(),
                default_dest_dir: None,
                idempotency_key: "create".into(),
            },
        )
        .unwrap()
        .package
}
fn item(url: &str) -> InboxIntakeItem {
    serde_json::from_value(serde_json::json!({"job":{"url":url},"untrusted_label":"fixture"}))
        .unwrap()
}
fn add(
    store: &InboxStore,
    owner: &str,
    package: &InboxPackageView,
    entries: Vec<InboxIntakeItem>,
) -> InboxEntriesAddResult {
    store
        .add(
            owner,
            &InboxEntriesAddParams {
                package_id: package.id.clone(),
                expected_revision: package.revision,
                entries,
                idempotency_key: "add".into(),
            },
        )
        .unwrap()
}
#[test]
fn persistent_intake_deduplicates_exact_requests_and_redacts_every_projection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox.redb");
    let store = InboxStore::open(&path).unwrap();
    let package = create(&store, "owner");
    let first = item("https://example.org/report?token=secret-one#fragment");
    let second = item("https://example.org/report?token=secret-two");
    let result = add(
        &store,
        "owner",
        &package,
        vec![first.clone(), first.clone(), second],
    );
    assert_eq!(result.added_ids.len(), 2);
    assert_eq!(result.duplicate_ids.len(), 1);
    assert_eq!(result.added_ids[0], result.duplicate_ids[0]);
    let params = InboxEntriesAddParams {
        package_id: package.id.clone(),
        expected_revision: package.revision,
        entries: vec![
            first.clone(),
            first.clone(),
            item("https://example.org/report?token=secret-two"),
        ],
        idempotency_key: "add".into(),
    };
    assert!(store.add("owner", &params).unwrap().replayed);
    let mut changed = params.clone();
    changed.entries[0].job.filename = Some("changed.bin".into());
    assert_eq!(
        store
            .add("owner", &changed)
            .unwrap_err()
            .envelope()
            .code
            .as_str(),
        "api.idempotency_conflict"
    );
    drop(store);
    let store = InboxStore::open(&path).unwrap();
    let view = store
        .get(
            "owner",
            &InboxPackageGetParams {
                include_history: false,
                owner_credential_id: None,
                package_id: package.id.clone(),
                page: PageRequest::default(),
                pending_only: false,
            },
        )
        .unwrap();
    assert_eq!(view.entries.items.len(), 2);
    let json = String::from_utf8(encoded(&view).unwrap()).unwrap();
    for secret in [
        "secret-one",
        "secret-two",
        "fragment",
        "hint_url",
        "extra_headers",
    ] {
        assert!(!json.contains(secret), "public projection leaked {secret}");
    }
    assert!(
        store
            .get(
                "other",
                &InboxPackageGetParams {
                    include_history: false,
                    owner_credential_id: None,
                    package_id: package.id,
                    page: PageRequest::default(),
                    pending_only: false
                }
            )
            .is_err()
    );
    assert!(
        store
            .list("other", &InboxPackagesListParams::default())
            .unwrap()
            .items
            .is_empty()
    );
}
#[test]
fn stale_edits_and_cross_owner_or_revision_cursors_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let store = InboxStore::open(&dir.path().join("inbox.redb")).unwrap();
    let package = create(&store, "one");
    let added = add(
        &store,
        "one",
        &package,
        vec![item("https://example.org/a"), item("https://example.org/b")],
    );
    let p = InboxPackageGetParams {
        include_history: false,
        owner_credential_id: None,
        package_id: package.id.clone(),
        page: PageRequest {
            limit: Some(1),
            cursor: None,
        },
        pending_only: false,
    };
    let first = store.get("one", &p).unwrap();
    assert!(first.entries.next_cursor.is_some());
    let stale = InboxEntriesRemoveParams {
        package_id: package.id.clone(),
        expected_revision: package.revision,
        entry_ids: added.added_ids[..1].to_vec(),
        idempotency_key: "remove".into(),
    };
    assert!(store.remove("one", &stale).is_err());
    store
        .remove(
            "one",
            &InboxEntriesRemoveParams {
                expected_revision: added.package.revision,
                ..stale
            },
        )
        .unwrap();
    let old = InboxPackageGetParams {
        page: PageRequest {
            cursor: first.entries.next_cursor,
            ..p.page
        },
        ..p
    };
    assert!(store.get("one", &old).is_err());
    assert!(store.get("two", &old).is_err());
}
#[test]
fn pending_dispatch_recovers_identical_hidden_payload_after_restart_and_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox.redb");
    let store = InboxStore::open(&path).unwrap();
    let pkg = create(&store, "owner");
    let added = add(
        &store,
        "owner",
        &pkg,
        vec![item("https://example.org/a?token=hidden")],
    );
    let p = InboxPreviewParams {
        package_id: pkg.id.clone(),
        expected_revision: added.package.revision,
        entry_ids: vec![],
        idempotency_key: "preview".into(),
    };
    let preview = Preview {
        view: InboxPreviewResult {
            preview_id: store.preview_id(),
            package_id: pkg.id.clone(),
            package_revision: added.package.revision,
            expires_at: Timestamp::now().saturating_add_ms(1000),
            entries: vec![],
            ready_entries: 1,
            remaining_pending: 0,
            replayed: false,
        },
        jobs: vec![item("https://example.org/a?token=hidden").job],
        ids: added.added_ids.clone(),
    };
    let preview = store.save_preview("owner", &p, preview).unwrap();
    let submit = InboxSubmitParams {
        package_id: pkg.id.clone(),
        expected_revision: added.package.revision,
        preview_id: preview.preview_id,
        idempotency_key: "submit".into(),
    };
    let (_, batch) = store.begin("owner", &submit).unwrap();
    let original = encoded(&batch.params).unwrap();
    let id = batch.id.clone();
    let mut expired = store.load("owner", &pkg.id).unwrap();
    expired.previews[0].view.expires_at = Timestamp::UNIX_EPOCH;
    store.save_package("owner", &expired).unwrap();
    assert!(
        store
            .add(
                "owner",
                &InboxEntriesAddParams {
                    package_id: pkg.id.clone(),
                    expected_revision: added.package.revision + 1,
                    entries: vec![item("https://example.org/c")],
                    idempotency_key: "blocked".into()
                }
            )
            .is_err()
    );
    drop(store);
    let store = InboxStore::open(&path).unwrap();
    let (_, recovered) = store.begin("owner", &submit).unwrap();
    assert_eq!(recovered.id, id);
    assert_eq!(encoded(&recovered.params).unwrap(), original);
    let retry = InboxSubmissionRetryParams {
        package_id: pkg.id.clone(),
        batch_id: id.clone(),
        idempotency_key: "recover".into(),
    };
    let recovered = store.retry_batch("owner", &retry).unwrap();
    assert_eq!(encoded(&recovered.params).unwrap(), original);
    let job_id = IdGenerator::new().job_id();
    let result = store
        .finish(
            "owner",
            &pkg.id,
            &recovered,
            EnqueueResult {
                replayed: true,
                dry_run: false,
                jobs: vec![zeus_api::methods::jobs::EnqueuedJob {
                    job_id: Some(job_id),
                    state: Some(zeus_types::JobState::Queued),
                    approval_id: None,
                    plan: None,
                    error: None,
                }],
            },
        )
        .unwrap();
    assert_eq!(result.remaining_pending, 0);
    assert_eq!(result.entries[0].result.job_id, Some(job_id));
    let second = store.retry_batch("owner", &retry).unwrap();
    assert!(second.result.is_some());
    assert!(
        store
            .view_batch(
                "other",
                &InboxSubmissionGetParams {
                    owner_credential_id: None,
                    package_id: pkg.id,
                    batch_id: id
                }
            )
            .is_err()
    );
}
#[test]
fn invalid_intake_and_oversized_batches_do_not_partially_commit() {
    let dir = tempfile::tempdir().unwrap();
    let store = InboxStore::open(&dir.path().join("inbox.redb")).unwrap();
    let pkg = create(&store, "owner");
    let mut invalid_item = item("https://example.org/b");
    invalid_item.untrusted_label = Some("x".repeat(501));
    let p = InboxEntriesAddParams {
        package_id: pkg.id.clone(),
        expected_revision: pkg.revision,
        entries: vec![item("https://example.org/a"), invalid_item],
        idempotency_key: "bad".into(),
    };
    assert!(store.add("owner", &p).is_err());
    assert!(store.load("owner", &pkg.id).unwrap().entries.is_empty());
    let p = InboxEntriesAddParams {
        entries: vec![item("https://example.org/a"); 201],
        ..p
    };
    assert!(store.add("owner", &p).is_err());
}

#[test]
fn large_explicit_history_returns_a_size_error_without_hiding_package_entries() {
    let dir = tempfile::tempdir().unwrap();
    let store = InboxStore::open(&dir.path().join("inbox.redb")).unwrap();
    let pkg = create(&store, "owner");
    let added = add(
        &store,
        "owner",
        &pkg,
        vec![item("https://example.org/a?secret=retained")],
    );
    let redacted = zeus_types::HttpUrl::parse(&format!("https://example.org/{}", "a".repeat(4000)))
        .unwrap()
        .redact(RedactMode::Log);
    for n in 0..4 {
        let entries = (0..50)
            .map(|_| InboxPreviewEntry {
                entry_id: added.added_ids[0].clone(),
                redacted_url: redacted.clone(),
                filename: Some("fixture.bin".into()),
                dest_path: Some("d".repeat(4000)),
                approval_needed: false,
                warnings: vec![],
                error: None,
            })
            .collect();
        let params = InboxPreviewParams {
            package_id: pkg.id.clone(),
            expected_revision: added.package.revision,
            entry_ids: vec![],
            idempotency_key: format!("large-{n}"),
        };
        let preview = Preview {
            view: InboxPreviewResult {
                preview_id: store.preview_id(),
                package_id: pkg.id.clone(),
                package_revision: added.package.revision,
                expires_at: Timestamp::now().saturating_add_ms(600_000),
                entries,
                ready_entries: 0,
                remaining_pending: 1,
                replayed: false,
            },
            jobs: vec![],
            ids: vec![],
        };
        store.save_preview("owner", &params, preview).unwrap();
    }
    let params = InboxPackageGetParams {
        include_history: false,
        owner_credential_id: None,
        package_id: pkg.id,
        page: PageRequest::default(),
        pending_only: false,
    };
    let ordinary = store.get("owner", &params).unwrap();
    assert_eq!(ordinary.entries.items.len(), 1);
    assert!(ordinary.previews.is_empty());
    assert!(ordinary.submissions.is_empty());
    let error = store
        .get(
            "owner",
            &InboxPackageGetParams {
                include_history: true,
                ..params
            },
        )
        .unwrap_err();
    assert_eq!(error.envelope().code.as_str(), "api.payload_too_large");
}
