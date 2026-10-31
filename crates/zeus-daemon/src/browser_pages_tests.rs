//! Native broker tests use an authenticated in-memory relay and real durable grant records.
use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use zeus_api::{ClientIdentity, ClientKind, ClientRole, auth::CredentialId, server::Notifier};
use zeus_types::{Approval, GrantState, IdGenerator, SessionGrant};

struct Fixture {
    _directory: tempfile::TempDir,
    broker: Arc<BrowserBroker>,
    connection: Arc<BrowserConnection>,
    outgoing: tokio::sync::mpsc::Receiver<zeus_api::jsonrpc::Message>,
    request: SessionPageRequestBody,
    grant: SessionGrant,
}
impl Fixture {
    fn new() -> Self {
        Self::with_terminal(false)
    }
    fn with_terminal(terminal: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let clock: zeus_control::SharedClock = Arc::new(zeus_control::SystemClock::new());
        let journal = Journal::open(
            &directory.path().join("test.redb"),
            &Default::default(),
            clock.clone(),
        )
        .unwrap();
        let ids = IdGenerator::new();
        let now = Timestamp::now();
        let profile = "0123456789abcdef0123456789abcdef";
        let run = ids.run_id();
        let mut grant: SessionGrant = serde_json::from_value(json!({"id":ids.grant_id(),"origin":"https://example.test","scope":"workflow","state":"active",
            "created_at":now,"expires_at":now.saturating_add_ms(60000),"run_id":run,"approval_id":ids.approval_id(),
            "decided_by":{"kind":"human","surface":"extension_popup"},"uses":0,"renewable":false})).unwrap();
        if terminal {
            grant.decided_by =
                serde_json::from_value(json!({"kind":"human","surface":"cli_interactive"}))
                    .unwrap();
        }
        let approval: Approval = serde_json::from_value(json!({"id":grant.approval_id,"kind":"workflow_plan","state":"approved","run_id":run,
            "facts":{"browser_document_mode":"browser_origin","browser_profile_id":profile},"allowed_scopes":["workflow"],"reason_codes":[],"summary":{"title":"fixture"},"created_at":now,"expires_at":now.saturating_add_ms(60000)})).unwrap();
        journal.transact(|tx| {
            tx.put_approval(&approval)?;
            let decision = if terminal { json!({"approval_id":approval.id,"outcome":"approve","actor":{"kind":"human","surface":"cli_interactive"},"evidence":{"surface":"cli_interactive","exe_verified":true}}) }
            else { json!({"approval_id":approval.id,"outcome":"approve","evidence":{"extension":{"profile_id":profile}}}) };
            tx.put_approval_decision(approval.id, &decision)?;
            tx.put_grant(&grant)
        }).unwrap();
        let broker = Arc::new(BrowserBroker::new(journal, clock));
        let (notifier, outgoing) = Notifier::channel(64);
        let extension = "a".repeat(32);
        let context = CallContext {
            reply: Default::default(),
            peer: Default::default(),
            attach: Some(zeus_api::methods::daemon::AttachInfo {
                browser: zeus_types::BrowserName::Chrome,
                attested_extension: extension.clone(),
                host_version: "test".into(),
                parent_verified: true,
            }),
            identity: ClientIdentity {
                credential_id: CredentialId::generate(),
                kind: ClientKind::Human,
                role: ClientRole::NativeHost,
                permissions: Default::default(),
                client_name: "test".into(),
            },
            connection_id: ids.connection_id(),
            notifier,
            limits: zeus_api::methods::daemon::ConnectionLimits::STANDARD,
            capabilities: zeus_api::capabilities::all(),
        };
        let hello = serde_json::from_value(json!({"client":"zeus-extension","extension_id":extension,"extension_version":"test","browser":{"name":"chrome","version":"test"},
            "protocol_versions":[1],"capabilities":["session.page"],"profile_id":profile,"sw_instance_id":"0123456789abcdef"})).unwrap();
        let connection = broker.attach(context, hello, true).unwrap();
        let request = SessionPageRequestBody {
            subject: SessionPageSubject::Run { run_id: run },
            grant_id: grant.id,
            profile_id: profile.into(),
            origin: "https://example.test".parse().unwrap(),
            url: "https://example.test/private?token=CANARY".parse().unwrap(),
            representation: PageRepresentation::Html,
            max_body_bytes: 512,
            deadline_ms: 2000,
            incognito: false,
        };
        Self {
            _directory: directory,
            broker,
            connection,
            outgoing,
            request,
            grant,
        }
    }
    async fn next(&mut self) -> DaemonMessage {
        let message = tokio::time::timeout(Duration::from_secs(2), self.outgoing.recv())
            .await
            .unwrap()
            .unwrap();
        let zeus_api::jsonrpc::Message::Notification(notification) = message else {
            panic!("notification expected")
        };
        DaemonMessage::from_json(notification.params.unwrap()["frame"].clone()).unwrap()
    }
    fn response(&self) -> SessionPageDocument {
        SessionPageDocument {
            subject: self.request.subject.clone(),
            grant_id: self.request.grant_id,
            profile_id: self.request.profile_id.clone(),
            final_url: self.request.url.clone(),
            http_status: 200,
            content_type: "text/html".into(),
            link: None,
            data: ChunkData::new(b"PRIVATE BODY".to_vec()),
            issued_at: Timestamp::now(),
        }
    }
}

#[tokio::test]
async fn page_broker_binds_response_and_sends_correlated_cancel() {
    for malformed in [false, true] {
        let mut f = Fixture::new();
        let broker = f.broker.clone();
        let request = f.request.clone();
        let task = tokio::spawn(async move { broker.document(request, &|| true).await });
        let message = f.next().await;
        assert!(matches!(message.body, DaemonBody::SessionPageRequest(_)));
        let mut document = f.response();
        if malformed {
            document.profile_id = "f".repeat(32);
        }
        f.broker
            .response(
                f.connection.ctx.connection_id,
                &message.id,
                SessionPageResponseBody::Fetched {
                    document: Box::new(document),
                }
                .into(),
            )
            .unwrap();
        let result = task.await.unwrap();
        assert_eq!(result.is_ok(), !malformed);
        let cancel = f.next().await;
        assert!(
            matches!(cancel.body, DaemonBody::SessionPageCancel(body) if body.request_id == message.id)
        );
    }
}

#[tokio::test]
async fn page_broker_stops_on_review_revocation_grant_revocation_and_task_cancellation() {
    for mode in 0..3 {
        let mut f = Fixture::new();
        let live = Arc::new(AtomicBool::new(true));
        let live_in = live.clone();
        let broker = f.broker.clone();
        let request = f.request.clone();
        let task = tokio::spawn(async move {
            broker
                .document(request, &|| live_in.load(Ordering::SeqCst))
                .await
        });
        let first = f.next().await;
        match mode {
            0 => live.store(false, Ordering::SeqCst),
            1 => {
                f.grant.state = GrantState::Revoked;
                f.broker
                    .journal
                    .transact(|tx| tx.put_grant(&f.grant))
                    .unwrap();
            }
            _ => task.abort(),
        }
        let cancel = f.next().await;
        assert!(
            matches!(cancel.body, DaemonBody::SessionPageCancel(body) if body.request_id == first.id)
        );
        assert!(!matches!(task.await, Ok(Ok(_))));
    }
}

#[tokio::test]
async fn page_broker_cannot_replace_workflow_with_site_grant_or_switch_profile() {
    let f = Fixture::new();
    for change in 0..3 {
        let mut request = f.request.clone();
        match change {
            0 => {
                request.subject = SessionPageSubject::Plan {
                    plan_id: "plan_01M45NHJ2AHWMXT8JVJ7BEEJ01".into(),
                }
            }
            1 => request.profile_id = "f".repeat(32),
            _ => request.grant_id = IdGenerator::new().grant_id(),
        }
        assert!(f.broker.document(request, &|| true).await.is_err());
    }
}

#[tokio::test]
async fn terminal_workflow_grant_stays_bound_when_another_profile_connects() {
    let f = Fixture::with_terminal(true);
    assert!(f.broker.authorize_document(&f.request).is_ok());
    // The same reviewed profile also binds file-session leases created by this workflow.
    assert_eq!(
        f.broker.grant_profile(f.grant.id, None).unwrap().as_deref(),
        Some(f.request.profile_id.as_str())
    );
    assert!(
        f.broker
            .grant_profile(f.grant.id, Some(&"f".repeat(32)))
            .is_err()
    );
    let (notifier, _outgoing) = Notifier::channel(64);
    let mut context = f.connection.ctx.clone();
    context.connection_id = IdGenerator::new().connection_id();
    context.notifier = notifier;
    let mut hello = f.connection.hello.clone();
    hello.profile_id = "f".repeat(32);
    let _other = f.broker.attach(context, hello, true).unwrap();
    // No profile was chosen yet: a new review must not pick either one implicitly.
    assert!(
        f.broker
            .document_grant(&f.request.origin, &f.request.subject, None)
            .is_err()
    );
    assert!(f.broker.authorize_document(&f.request).is_ok());
    let mut changed = f.request.clone();
    changed.profile_id = "f".repeat(32);
    assert!(f.broker.authorize_document(&changed).is_err());
    changed = f.request.clone();
    changed.subject = SessionPageSubject::Run {
        run_id: IdGenerator::new().run_id(),
    };
    assert!(f.broker.authorize_document(&changed).is_err());
}

#[tokio::test]
async fn expired_terminal_grant_cannot_authorize_browser_documents() {
    let mut f = Fixture::with_terminal(true);
    f.grant.expires_at = Some(Timestamp::UNIX_EPOCH);
    f.broker
        .journal
        .transact(|tx| tx.put_grant(&f.grant))
        .unwrap();
    assert!(f.broker.authorize_document(&f.request).is_err());
}

#[tokio::test]
async fn workflow_without_document_mode_cannot_authorize_browser_pages() {
    let f = Fixture::with_terminal(true);
    f.broker
        .journal
        .transact(|tx| {
            let mut approval = tx.approval(f.grant.approval_id)?.unwrap();
            approval
                .facts
                .as_mut()
                .unwrap()
                .remove("browser_document_mode");
            tx.put_approval(&approval)
        })
        .unwrap();
    assert!(f.broker.authorize_document(&f.request).is_err());
}

#[tokio::test]
async fn incomplete_workflow_review_cannot_borrow_a_connected_profile() {
    let f = Fixture::with_terminal(true);
    for profile in [None, Some(json!(null)), Some(json!(17)), Some(json!(""))] {
        f.broker
            .journal
            .transact(|tx| {
                let mut approval = tx.approval(f.grant.approval_id)?.unwrap();
                let facts = approval.facts.as_mut().unwrap();
                if let Some(value) = profile {
                    facts.insert("browser_profile_id".into(), value);
                } else {
                    facts.remove("browser_profile_id");
                }
                tx.put_approval(&approval)
            })
            .unwrap();
        assert!(
            f.broker
                .grant_profile(f.grant.id, Some(&f.request.profile_id))
                .is_err()
        );
        assert!(f.broker.authorize_document(&f.request).is_err());
    }
}
