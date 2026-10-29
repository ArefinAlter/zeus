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
                rules: None,
                initial_entries: None,
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
                matched_rule: None,
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

fn rule(
    id: &str,
    origin: Option<&str>,
    suffix: Option<&str>,
    dest: Option<&str>,
    prefix: Option<&str>,
) -> InboxRule {
    InboxRule {
        id: id.into(),
        origin: origin.map(|s| s.parse().unwrap()),
        filename_suffix: suffix.map(str::to_owned),
        dest_dir: dest.map(str::to_owned),
        filename_prefix: prefix.map(str::to_owned),
    }
}
#[test]
fn package_rules_are_first_match_and_never_replace_explicit_destinations_or_authority() {
    let dir = tempfile::tempdir().unwrap();
    let store = InboxStore::open(&dir.path().join("inbox.redb")).unwrap();
    let mut package = create(&store, "owner");
    package.default_dest_dir = Some("fallback".into());
    package.rules = vec![
        rule(
            "pdf",
            Some("https://example.org"),
            Some(".pdf"),
            Some("papers"),
            Some("paper_"),
        ),
        rule("all_pdf", None, Some(".pdf"), Some("other"), Some("other_")),
    ];
    let mut source =
        item("https://example.org/report.PDF?token=ZEUS-CANARY-inbox-rule#ignored").job;
    source.session_origin = Some("https://example.org".parse().unwrap());
    let original = source.clone();
    assert_eq!(apply_rules(&package, &mut source).as_deref(), Some("pdf"));
    assert_eq!(source.dest_dir.as_deref(), Some("papers"));
    assert_eq!(source.filename.as_deref(), Some("paper_report.PDF"));
    assert_eq!(source.url, original.url);
    assert_eq!(source.session_origin, original.session_origin);
    assert_eq!(source.extra_headers, original.extra_headers);
    let mut explicit = original.clone();
    explicit.dest_dir = Some("explicit".into());
    apply_rules(&package, &mut explicit);
    assert_eq!(explicit.dest_dir.as_deref(), Some("explicit"));
    let mut other = item("https://example.org:8443/report.pdf").job;
    assert_eq!(
        apply_rules(&package, &mut other).as_deref(),
        Some("all_pdf")
    );
    assert_eq!(other.dest_dir.as_deref(), Some("other"));
    let mut query = item("https://example.org/report.txt?name=.pdf").job;
    assert_eq!(apply_rules(&package, &mut query), None);
    assert_eq!(query.dest_dir.as_deref(), Some("fallback"));
    // Each new review begins from the retained request, so prefix application never compounds.
    let mut reviewed_again = original.clone();
    apply_rules(&package, &mut reviewed_again);
    assert_eq!(reviewed_again, source);
}
#[test]
fn package_rule_edits_survive_restart_preserve_legacy_clients_and_replay_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox.redb");
    let store = InboxStore::open(&path).unwrap();
    let package = create(&store, "owner");
    let mut params = InboxPackagePutParams {
        package_id: Some(package.id.clone()),
        expected_revision: Some(package.revision),
        untrusted_name: package.untrusted_name.clone(),
        default_dest_dir: None,
        rules: Some(vec![rule("pdf", None, Some(".pdf"), None, Some("paper_"))]),
        initial_entries: None,
        idempotency_key: "rules".into(),
    };
    let saved = store.put("owner", &params).unwrap();
    assert_eq!(saved.package.rules.len(), 1);
    assert!(store.put("owner", &params).unwrap().replayed);
    let mut changed = params.clone();
    changed.rules = Some(vec![]);
    assert_eq!(
        store
            .put("owner", &changed)
            .unwrap_err()
            .envelope()
            .code
            .as_str(),
        "api.idempotency_conflict"
    );
    assert!(store.put("another-owner", &params).is_err());
    drop(store);
    let store = InboxStore::open(&path).unwrap();
    assert_eq!(
        store.load("owner", &package.id).unwrap().view.rules,
        saved.package.rules
    );
    params.expected_revision = Some(saved.package.revision);
    params.idempotency_key = "legacy-edit".into();
    params.rules = None;
    let legacy = store.put("owner", &params).unwrap();
    assert_eq!(legacy.package.rules, saved.package.rules);
    params.expected_revision = Some(legacy.package.revision);
    params.idempotency_key = "clear-rules".into();
    params.rules = Some(vec![]);
    assert!(
        store
            .put("owner", &params)
            .unwrap()
            .package
            .rules
            .is_empty()
    );
    let mut old = serde_json::to_value(package).unwrap();
    old.as_object_mut().unwrap().remove("rules");
    let old: InboxPackageView = serde_json::from_value(old).unwrap();
    assert!(old.rules.is_empty());
}
#[test]
fn package_rules_refuse_unbounded_or_unsafe_matching_and_path_prefixes() {
    let valid = rule("pdf", None, Some(".pdf"), None, Some("paper_"));
    assert!(validate_rules(std::slice::from_ref(&valid)).is_ok());
    let mut bad = valid.clone();
    bad.filename_prefix = Some("../".into());
    assert!(validate_rules(&[bad]).is_err());
    let mut bad = valid.clone();
    bad.filename_suffix = Some(".*".into());
    assert!(validate_rules(&[bad]).is_err());
    let mut bad = valid.clone();
    bad.origin = None;
    bad.filename_suffix = None;
    assert!(validate_rules(&[bad]).is_err());
    let mut bad = valid.clone();
    bad.filename_prefix = None;
    assert!(validate_rules(&[bad]).is_err());
    assert!(validate_rules(&[valid.clone(), valid.clone()]).is_err());
    assert!(validate_rules(&vec![valid; 33]).is_err());
    for origin in [
        "https://example.org/path",
        "https://example.org?token=ZEUS-CANARY-rule-origin",
        "https://u:p@example.org",
        "https://*.example.org",
    ] {
        let parsed = serde_json::from_value::<InboxRule>(
            serde_json::json!({"id":"origin","origin":origin,"dest_dir":"folder"}),
        );
        if origin.contains('*') {
            assert!(parsed.is_err() || validate_rules(&[parsed.unwrap()]).is_err());
        } else {
            assert!(parsed.is_err());
        }
    }
}

#[test]
fn seeded_intake_commits_atomically_and_recovers_the_same_package_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox.redb");
    let store = InboxStore::open(&path).unwrap();
    let entry = item("https://example.org/a?token=ZEUS-CANARY-seeded");
    let mut params:InboxPackagePutParams=serde_json::from_value(serde_json::json!({"untrusted_name":"Grabber selection","initial_entries":[entry.clone(),entry],"idempotency_key":"stable-intake"})).unwrap();
    let saved = store.put("owner", &params).unwrap();
    assert_eq!(saved.package.pending_entries, 1);
    assert_eq!(saved.package.enqueued_entries, 0);
    assert_eq!(store.metadata("owner").unwrap().len(), 1);
    assert!(
        !serde_json::to_string(&saved)
            .unwrap()
            .contains("ZEUS-CANARY")
    );
    drop(store);
    let store = InboxStore::open(&path).unwrap();
    let replay = store.put("owner", &params).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.package.id, saved.package.id);
    assert_eq!(store.metadata("owner").unwrap().len(), 1);
    params.idempotency_key = "bad-seed".into();
    params.initial_entries.as_mut().unwrap().push(serde_json::from_value(serde_json::json!({"job":{"url":"https://example.org/b","labels":{"zeus.package":"impersonation"}}})).unwrap());
    assert!(store.put("owner", &params).is_err());
    assert_eq!(store.metadata("owner").unwrap().len(), 1);
    params.initial_entries = Some(vec![]);
    assert!(store.put("owner", &params).is_err());
    params.initial_entries = Some(vec![item("https://example.org/c")]);
    params.package_id = Some(saved.package.id);
    params.expected_revision = Some(saved.package.revision);
    assert!(store.put("owner", &params).is_err());
}
