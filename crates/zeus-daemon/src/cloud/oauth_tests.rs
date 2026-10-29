//! End-to-end daemon/keyring contract fixtures; no person or real provider signs in.
use super::*;
use zeus_api::methods::cloud_oauth::*;
use zeus_cloud::oauth::{OAuthHttp, OAuthRegistration, OAuthTokens};
#[derive(Default)]
struct Vault {
    values: std::sync::Mutex<BTreeMap<String, zeus_cloud::SecretString>>,
    fail_put: AtomicBool,
    fail_delete: AtomicBool,
}
impl CredentialStore for Vault {
    fn get(&self, key: &str) -> CloudResult<zeus_cloud::SecretString> {
        self.values
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or(CloudError::CredentialUnavailable)
    }
    fn put(&self, key: &str, value: &zeus_cloud::SecretString) -> CloudResult<()> {
        if self.fail_put.load(Ordering::SeqCst) {
            return Err(CloudError::CredentialUnavailable);
        }
        self.values
            .lock()
            .unwrap()
            .insert(key.into(), value.clone());
        Ok(())
    }
    fn delete(&self, key: &str) -> CloudResult<()> {
        if self.fail_delete.load(Ordering::SeqCst) {
            return Err(CloudError::CredentialUnavailable);
        }
        self.values.lock().unwrap().remove(key);
        Ok(())
    }
}
#[derive(Clone)]
struct Wire {
    calls: Arc<AtomicUsize>,
    fail: Arc<AtomicBool>,
    blocked: Arc<AtomicBool>,
    entered: Arc<tokio::sync::Notify>,
}
async fn token(
    axum::extract::State(state): axum::extract::State<Wire>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    state.calls.fetch_add(1, Ordering::SeqCst);
    state.entered.notify_one();
    if state.blocked.load(Ordering::SeqCst) {
        std::future::pending::<()>().await;
    }
    if state.fail.load(Ordering::SeqCst) {
        return (axum::http::StatusCode::SERVICE_UNAVAILABLE,axum::Json(serde_json::json!({"error":"server_error","error_description":"ZEUS-CANARY-provider-body"}))).into_response();
    }
    axum::Json(serde_json::json!({"access_token":"ZEUS-CANARY-access","refresh_token":"ZEUS-CANARY-refresh","expires_in":3600,"token_type":"bearer","scope":"https://www.googleapis.com/auth/drive.file"})).into_response()
}
async fn revoke() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({}))
}
async fn fixture() -> (Fixture, Arc<Vault>, Wire, tokio::task::JoinHandle<()>) {
    let mut f = Fixture::new().await;
    let vault = Arc::new(Vault::default());
    let wire = Wire {
        calls: Arc::new(AtomicUsize::new(0)),
        fail: Arc::new(AtomicBool::new(false)),
        blocked: Arc::new(AtomicBool::new(false)),
        entered: Arc::new(tokio::sync::Notify::new()),
    };
    let app = axum::Router::new()
        .route("/token", axum::routing::post(token))
        .route("/revoke", axum::routing::post(revoke))
        .with_state(wire.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let service = Arc::get_mut(&mut f.service).unwrap();
    service.credentials = vault.clone();
    service.oauth.http = OAuthHttp::loopback_fixture(&base).unwrap();
    (f, vault, wire, task)
}
fn begin_params() -> CloudOAuthBeginParams {
    CloudOAuthBeginParams {
        account_id: "oauth".into(),
        label: "OAuth fixture".into(),
        config: ProviderConfig::GoogleDrive {
            parent_id: "root".into(),
        },
        max_bytes_per_delivery: 1000000,
        expected_revision: None,
        registration: OAuthRegistration::defaults(ProviderKind::GoogleDrive, "fixture-app".into())
            .unwrap(),
        client_secret: None,
        redirect_uri: Some("http://127.0.0.1:54321/oauth/callback".into()),
        idempotency_key: "fixture-login".into(),
    }
}
async fn connect(f: &Fixture) -> CloudOAuthStatus {
    let start = f
        .service
        .oauth_begin(
            &f.human,
            Actor::human(Surface::CliInteractive),
            begin_params(),
        )
        .await
        .unwrap();
    let state = url::Url::parse(start.authorization_url.expose())
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    f.service
        .oauth_finish(
            &f.human,
            Actor::human(Surface::CliInteractive),
            CloudOAuthFinishParams {
                flow_id: start.status.flow_id.unwrap(),
                code: Some(zeus_types::SecretString::new("fixture-code".into())),
                state: Some(zeus_types::SecretString::new(state)),
            },
        )
        .await
        .unwrap()
}
fn expire(f: &Fixture, vault: &Vault) {
    let a = f.service.account("oauth").unwrap();
    let key = a.credential_key.unwrap();
    let value = vault.get(&key).unwrap();
    let mut tokens = OAuthTokens::decode_private(&value).unwrap();
    tokens.expires_at_ms = 1;
    vault.put(&key, &tokens.encode_private().unwrap()).unwrap();
}
#[tokio::test]
async fn agents_and_unverified_actors_cannot_start_and_wrong_state_never_exchanges() {
    let (f, _, wire, task) = fixture().await;
    assert!(
        f.service
            .oauth_begin(
                &f.agent,
                Actor::human(Surface::CliInteractive),
                begin_params()
            )
            .await
            .is_err()
    );
    assert!(
        f.service
            .oauth_begin(&f.human, Actor::system(), begin_params())
            .await
            .is_err()
    );
    let start = f
        .service
        .oauth_begin(
            &f.human,
            Actor::human(Surface::CliInteractive),
            begin_params(),
        )
        .await
        .unwrap();
    let changed = f
        .service
        .oauth_begin(
            &f.human,
            Actor::human(Surface::CliInteractive),
            CloudOAuthBeginParams {
                label: "changed".into(),
                ..begin_params()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(changed.envelope().code.as_str(), "api.idempotency_conflict");
    assert!(
        f.service
            .oauth_finish(
                &f.human,
                Actor::human(Surface::CliInteractive),
                CloudOAuthFinishParams {
                    flow_id: start.status.flow_id.clone().unwrap(),
                    code: Some(zeus_types::SecretString::new("fixture-code".into())),
                    state: Some(zeus_types::SecretString::new("wrong".into()))
                }
            )
            .await
            .is_err()
    );
    assert_eq!(wire.calls.load(Ordering::SeqCst), 0);
    assert!(f.service.account("oauth").is_err());
    let canceled = f
        .service
        .oauth_cancel(
            &f.human,
            Actor::human(Surface::CliInteractive),
            CloudOAuthCancelParams {
                flow_id: start.status.flow_id.unwrap(),
            },
        )
        .await
        .unwrap();
    assert_eq!(canceled.state, CloudOAuthState::Canceled);
    task.abort();
}
#[tokio::test]
async fn account_scope_and_keyring_bundle_publish_together_without_secret_records() {
    let (f, vault, wire, task) = fixture().await;
    let result = connect(&f).await;
    assert_eq!(result.state, CloudOAuthState::Connected);
    assert!(result.refresh_available && result.token_expires_at.is_some());
    let a = f.service.account("oauth").unwrap();
    assert!(a.oauth.is_some());
    assert!(!serde_json::to_string(&a).unwrap().contains("ZEUS-CANARY"));
    let raw = vault.get(a.credential_key.as_ref().unwrap()).unwrap();
    assert!(OAuthTokens::decode_private(&raw).unwrap().refresh.is_some());
    let access = f
        .service
        .account_credential(&a, false, &CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(access.expose(), "ZEUS-CANARY-access");
    assert_eq!(wire.calls.load(Ordering::SeqCst), 1);
    let status = f
        .service
        .oauth_status(
            &f.human,
            Actor::human(Surface::CliInteractive),
            CloudOAuthStatusParams {
                flow_id: None,
                account_id: Some("oauth".into()),
            },
        )
        .await
        .unwrap();
    assert!(status.refresh_available);
    assert!(
        !serde_json::to_string(&status)
            .unwrap()
            .contains("ZEUS-CANARY")
    );
    task.abort();
}
#[tokio::test]
async fn concurrent_expired_resolution_refreshes_once_and_manual_retry_uses_receipt() {
    let (f, vault, wire, task) = fixture().await;
    connect(&f).await;
    expire(&f, &vault);
    let a = f.service.account("oauth").unwrap();
    let cancel = CancellationToken::new();
    let status = f
        .service
        .oauth_status(
            &f.human,
            Actor::human(Surface::CliInteractive),
            CloudOAuthStatusParams {
                flow_id: None,
                account_id: Some("oauth".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(status.state, CloudOAuthState::RefreshRequired);
    assert!(status.refresh_available);
    let (one, two) = tokio::join!(
        f.service.account_credential(&a, false, &cancel),
        f.service.account_credential(&a, false, &cancel)
    );
    assert!(one.is_ok() && two.is_ok());
    assert_eq!(wire.calls.load(Ordering::SeqCst), 2);
    let p = CloudOAuthRefreshParams {
        account_id: "oauth".into(),
        expected_revision: a.view.revision,
        idempotency_key: "refresh-once".into(),
    };
    f.service
        .oauth_refresh(&f.human, Actor::human(Surface::CliInteractive), p.clone())
        .await
        .unwrap();
    f.service
        .oauth_refresh(&f.human, Actor::human(Surface::CliInteractive), p)
        .await
        .unwrap();
    assert_eq!(wire.calls.load(Ordering::SeqCst), 3);
    task.abort();
}
#[tokio::test]
async fn interrupted_refresh_stays_uncertain_across_restart_and_is_not_replayed() {
    let (f, vault, wire, task) = fixture().await;
    connect(&f).await;
    // An explicit refresh may be interrupted while the old access token is still fresh.
    wire.fail.store(true, Ordering::SeqCst);
    let a = f.service.account("oauth").unwrap();
    assert!(
        f.service
            .account_credential(&a, true, &CancellationToken::new())
            .await
            .is_err()
    );
    assert!(
        f.service
            .account("oauth")
            .unwrap()
            .oauth
            .unwrap()
            .refresh_uncertain
    );
    let Fixture {
        dir,
        service,
        provider,
        agent: _,
        human,
        credentials: _,
    } = f;
    let journal = service.journal.clone();
    let consent = service.consent.clone();
    let audit = service.audit.clone();
    service.shutdown().await;
    drop(service);
    let reopened = CloudService::open_with_factory(
        dir.path(),
        journal,
        consent,
        audit,
        vault,
        Arc::new(Factory(provider)),
    )
    .unwrap();
    let current = reopened.account("oauth").unwrap();
    assert!(
        reopened
            .account_credential(&current, false, &CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(wire.calls.load(Ordering::SeqCst), 2);
    let status = reopened
        .oauth_status(
            &human,
            Actor::human(Surface::CliInteractive),
            CloudOAuthStatusParams {
                flow_id: None,
                account_id: Some("oauth".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(status.state, CloudOAuthState::SignInRequired);
    assert!(!status.refresh_available);
    task.abort();
}
#[tokio::test]
async fn disconnect_retains_failed_keyring_cleanup_and_remote_receipt_is_stable() {
    let (f, vault, _, task) = fixture().await;
    connect(&f).await;
    let a = f.service.account("oauth").unwrap();
    vault.fail_delete.store(true, Ordering::SeqCst);
    let p = CloudOAuthDisconnectParams {
        account_id: "oauth".into(),
        expected_revision: a.view.revision,
        remote: true,
        idempotency_key: "disconnect-once".into(),
    };
    let first = f
        .service
        .oauth_disconnect(&f.human, Actor::human(Surface::CliInteractive), p.clone())
        .await
        .unwrap();
    assert!(first.account.revoked && first.local_cleanup_pending);
    assert_eq!(first.remote_revoked, Some(true));
    let replay = f
        .service
        .oauth_disconnect(&f.human, Actor::human(Surface::CliInteractive), p)
        .await
        .unwrap();
    assert_eq!(replay.remote_revoked, Some(true));
    vault.fail_delete.store(false, Ordering::SeqCst);
    let cleaned = f
        .service
        .oauth_disconnect(
            &f.human,
            Actor::human(Surface::CliInteractive),
            CloudOAuthDisconnectParams {
                account_id: "oauth".into(),
                expected_revision: first.account.revision,
                remote: false,
                idempotency_key: "cleanup-retry".into(),
            },
        )
        .await
        .unwrap();
    assert!(!cleaned.local_cleanup_pending);
    assert!(
        f.service
            .account("oauth")
            .unwrap()
            .pending_credentials
            .is_empty()
    );
    task.abort();
}
#[tokio::test]
async fn failed_keyring_write_never_publishes_destination_authority() {
    let (f, vault, wire, task) = fixture().await;
    vault.fail_put.store(true, Ordering::SeqCst);
    let start = f
        .service
        .oauth_begin(
            &f.human,
            Actor::human(Surface::CliInteractive),
            begin_params(),
        )
        .await
        .unwrap();
    let state = url::Url::parse(start.authorization_url.expose())
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let result = f
        .service
        .oauth_finish(
            &f.human,
            Actor::human(Surface::CliInteractive),
            CloudOAuthFinishParams {
                flow_id: start.status.flow_id.unwrap(),
                code: Some(zeus_types::SecretString::new("fixture-code".into())),
                state: Some(zeus_types::SecretString::new(state)),
            },
        )
        .await;
    assert!(result.is_err());
    assert_eq!(wire.calls.load(Ordering::SeqCst), 1);
    assert!(f.service.account("oauth").is_err());
    assert!(vault.values.lock().unwrap().is_empty());
    task.abort();
}
#[tokio::test]
async fn refresh_dispatch_receipt_survives_a_missing_final_result_without_replay() {
    let (f, _, wire, task) = fixture().await;
    connect(&f).await;
    let p = CloudOAuthRefreshParams {
        account_id: "oauth".into(),
        expected_revision: 1,
        idempotency_key: "interrupted-intent".into(),
    };
    // This is the durable state after dispatch/keyring rotation but before final receipt save.
    let (key, hash, _) = f
        .service
        .receipt(&f.human, "cloud.oauth.refresh", &p.idempotency_key, &p)
        .unwrap();
    f.service
        .store
        .save(
            &key,
            &Receipt {
                hash,
                created_at: Some(Timestamp::now()),
                result: serde_json::json!({"oauth_dispatch_pending":true}),
            },
        )
        .unwrap();
    let error = f
        .service
        .oauth_refresh(&f.human, Actor::human(Surface::CliInteractive), p)
        .await
        .unwrap_err();
    assert_eq!(
        error.envelope().code.as_str(),
        "cloud.oauth_outcome_uncertain"
    );
    assert_eq!(error.envelope().class, zeus_types::ErrorClass::Conflict);
    assert!(!error.envelope().retryable);
    assert_eq!(wire.calls.load(Ordering::SeqCst), 1);
    task.abort();
}
#[tokio::test]
async fn interrupted_keyring_write_cleanup_preserves_active_keys_and_retries_failures() {
    let (f, vault, _, task) = fixture().await;
    connect(&f).await;
    let active = f.service.account("oauth").unwrap().credential_key.unwrap();
    let orphan = "unfinished-r1";
    vault
        .put(
            orphan,
            &zeus_cloud::SecretString::new("ZEUS-CANARY-orphan".into()),
        )
        .unwrap();
    for key in [&active, orphan] {
        f.service
            .store
            .save(&format!("credential-write:{key}"), &key)
            .unwrap();
    }
    vault.fail_delete.store(true, Ordering::SeqCst);
    super::super::oauth::reconcile_credential_writes(&f.service.store, vault.as_ref()).unwrap();
    assert!(vault.get(&active).is_ok());
    assert!(vault.get(orphan).is_ok());
    assert_eq!(f.service.store.keys("credential-write:").unwrap().len(), 1);
    vault.fail_delete.store(false, Ordering::SeqCst);
    super::super::oauth::reconcile_credential_writes(&f.service.store, vault.as_ref()).unwrap();
    assert!(vault.get(&active).is_ok());
    assert!(vault.get(orphan).is_err());
    assert!(
        f.service
            .store
            .keys("credential-write:")
            .unwrap()
            .is_empty()
    );
    task.abort();
}
#[tokio::test]
async fn kill_switch_pause_cancels_pending_authorization_without_publishing_scope() {
    let (f, _, wire, task) = fixture().await;
    let begin = f
        .service
        .oauth_begin(
            &f.human,
            Actor::human(Surface::CliInteractive),
            begin_params(),
        )
        .await
        .unwrap();
    f.service.pause_all_for_kill_switch().await.unwrap();
    let status = f
        .service
        .oauth_status(
            &f.human,
            Actor::human(Surface::CliInteractive),
            CloudOAuthStatusParams {
                flow_id: begin.status.flow_id,
                account_id: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(status.state, CloudOAuthState::Canceled);
    assert!(f.service.account("oauth").is_err());
    assert_eq!(wire.calls.load(Ordering::SeqCst), 0);
    task.abort();
}

#[tokio::test]
async fn global_pause_cancels_in_flight_refresh_and_preserves_rotation_uncertainty() {
    let (f, vault, wire, task) = fixture().await;
    connect(&f).await;
    // Drain the completed exchange notification before waiting for refresh dispatch.
    wire.entered.notified().await;
    expire(&f, &vault);
    wire.blocked.store(true, Ordering::SeqCst);
    let a = f.service.account("oauth").unwrap();
    let service = f.service.clone();
    let refresh = tokio::spawn(async move {
        service
            .account_credential(&a, false, &CancellationToken::new())
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), wire.entered.notified())
        .await
        .unwrap();
    f.service.pause_all_for_kill_switch().await.unwrap();
    let error = tokio::time::timeout(std::time::Duration::from_secs(2), refresh)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.envelope().code.as_str(), "cloud.oauth_canceled");
    assert!(
        f.service
            .account("oauth")
            .unwrap()
            .oauth
            .unwrap()
            .refresh_uncertain
    );
    task.abort();
}

#[tokio::test]
async fn independent_state_stores_never_overwrite_or_delete_each_others_account_key() {
    let (first, vault, _, server_one) = fixture().await;
    connect(&first).await;
    let (mut second, _, _, server_two) = fixture().await;
    Arc::get_mut(&mut second.service).unwrap().credentials = vault.clone();
    connect(&second).await;
    let one = first.service.account("oauth").unwrap();
    let two = second.service.account("oauth").unwrap();
    let key_one = one.credential_key.unwrap();
    let key_two = two.credential_key.unwrap();
    assert_ne!(key_one, key_two);
    assert!(
        key_one.len() <= 96
            && key_one
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    );
    assert!(vault.get(&key_one).is_ok() && vault.get(&key_two).is_ok());
    first
        .service
        .oauth_disconnect(
            &first.human,
            Actor::human(Surface::CliInteractive),
            CloudOAuthDisconnectParams {
                account_id: "oauth".into(),
                expected_revision: 1,
                remote: false,
                idempotency_key: "first-only".into(),
            },
        )
        .await
        .unwrap();
    assert!(vault.get(&key_one).is_err());
    assert!(vault.get(&key_two).is_ok());
    server_one.abort();
    server_two.abort();
}
