//! Bounded native-message fetch streams. Credits return only when the engine polls after writing
//! the previous chunk; dropping a response cancels its browser request and releases its slot.
use crate::browser::{BrowserBroker, BrowserConnection};
use async_trait::async_trait;
use futures::{Stream, task::AtomicWaker};
use std::{
    collections::{HashMap, VecDeque},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::oneshot;
use zeus_api::{ApiError, ApiResult};
use zeus_journal::Journal;
use zeus_session::{
    error::{SessionError, SessionResult},
    flow::InboundStream,
    protocol::*,
    provider::*,
};
use zeus_types::{JobState, Timestamp};

const WINDOW: u8 = 2;
const IDLE: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct Access {
    journal: Journal,
    request: ProxyFetchRequest,
    grant_id: zeus_types::GrantId,
}
impl Access {
    fn check(&self) -> SessionResult<()> {
        let grant = authorize(&self.journal, &self.request)?;
        if grant != self.grant_id {
            return Err(SessionError::Protocol(
                "browser stream authority changed; start a fresh stream",
            ));
        }
        Ok(())
    }
}

fn authorize(journal: &Journal, request: &ProxyFetchRequest) -> SessionResult<zeus_types::GrantId> {
    let allowed = journal
        .read(|tx| {
            if tx.meta()?.kill_switch_engaged() {
                return Ok(None);
            }
            let Some(job) = tx.job(request.job_id)? else {
                return Ok(None);
            };
            if request.url.origin() != request.origin
                || job.job.spec.session_origin.as_ref() != Some(&request.origin)
                || matches!(
                    job.job.state,
                    JobState::Paused
                        | JobState::Canceled
                        | JobState::Completed
                        | JobState::Failed
                        | JobState::AwaitingApproval
                )
            {
                return Ok(None);
            }
            let grants = tx.grants()?;
            Ok(
                match zeus_types::grant::resolve_grant(
                    grants.iter(),
                    &request.origin,
                    Some(request.job_id),
                    job.job.spec.source.run_id,
                    Timestamp::now(),
                ) {
                    zeus_types::grant::GrantResolution::Allowed(grant)
                        if grant.decided_by.is_human_on_trust_surface() =>
                    {
                        Some(grant.id)
                    }
                    _ => None,
                },
            )
        })
        .map_err(|_| SessionError::Protocol("consent journal unavailable"))?;
    if let Some(grant) = allowed {
        Ok(grant)
    } else {
        Err(SessionError::Remote(zeus_types::ErrorEnvelope::from_code(
            "session.consent_required",
            "Browser fetch requires current consent and an active job",
        )))
    }
}

struct State {
    request_id: MessageId,
    access: Access,
    inbound: InboundStream,
    start: u64,
    requested_length: Option<u64>,
    chunk_bytes: u32,
    expected_length: Option<u64>,
    head: Option<oneshot::Sender<SessionResult<ProxyHead>>>,
    head_seen: bool,
    queue: VecDeque<SessionResult<ProxyChunk>>,
    closed: bool,
    waker: Arc<AtomicWaker>,
}
impl State {
    fn fail(&mut self, error: SessionError) {
        self.closed = true;
        self.queue.clear();
        if let Some(head) = self.head.take() {
            let _ = head.send(Err(error));
        } else {
            self.queue.push_back(Err(error));
        }
        self.waker.wake();
    }
    fn head(&mut self, re: Option<&MessageId>, head: FetchHeadBody) -> SessionResult<()> {
        if self.head_seen || re != Some(&self.request_id) {
            return Err(SessionError::Protocol(
                "uncorrelated or repeated fetch head",
            ));
        }
        if !(100..=599).contains(&head.status)
            || head
                .headers
                .values()
                .any(|v| v.len() > 8192 || v.contains(['\r', '\n', '\0']))
            || head.headers.values().map(String::len).sum::<usize>() > 16384
        {
            return Err(SessionError::Protocol("invalid browser response head"));
        }
        if head
            .headers
            .get(&ResponseHeaderName::ContentEncoding)
            .is_some_and(|v| !v.eq_ignore_ascii_case("identity"))
        {
            return Err(SessionError::Remote(zeus_types::ErrorEnvelope::from_code(
                "fetch.content_encoded",
                "Browser fetch content encoding changes byte offsets",
            )));
        }
        let length = head
            .headers
            .get(&ResponseHeaderName::ContentLength)
            .map(|v| {
                if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(SessionError::Protocol("invalid content length"));
                }
                v.parse::<u64>()
                    .map_err(|_| SessionError::Protocol("invalid content length"))
            })
            .transpose()?;
        // HTTP classification and strong-validator checks are shared with the direct transport.
        // The stream independently checks actual bytes against the declared body length.
        self.expected_length = if head.status == 206 {
            self.requested_length.or(length)
        } else {
            length
        };
        if head.status == 206
            && length.is_some()
            && self.requested_length.is_some()
            && length != self.requested_length
        {
            return Err(SessionError::Protocol(
                "range body length differs from requested slice",
            ));
        }
        if head.status == 200 && self.start == 0 {
            // A server may ignore the range probe. Its body is the full representation; the
            // engine switches to sequential transfer, with the same bounded credit window.
            let limits = Limits {
                max_chunk_bytes: u64::from(self.chunk_bytes),
                ..Limits::default()
            };
            self.inbound = InboundStream::new(0, length, WINDOW, &limits);
        }
        let headers = head
            .headers
            .into_iter()
            .map(|(name, value)| {
                serde_json::to_value(name)
                    .ok()
                    .and_then(|v| v.as_str().map(|s| (s.to_owned(), value)))
                    .ok_or(SessionError::Protocol("invalid response header name"))
            })
            .collect::<SessionResult<_>>()?;
        self.head_seen = true;
        let result = ProxyHead {
            status: head.status,
            final_url: head.final_url,
            redirected: head.redirected,
            headers,
        };
        self.head
            .take()
            .ok_or(SessionError::Protocol("fetch head was canceled"))?
            .send(Ok(result))
            .map_err(|_| SessionError::ConnectionLost)?;
        Ok(())
    }
    fn chunk(&mut self, chunk: FetchChunkBody) -> SessionResult<()> {
        if !self.head_seen || chunk.data.is_empty() {
            return Err(SessionError::Protocol("empty chunk or chunk before head"));
        }
        self.inbound.accept(&chunk)?;
        if self.queue.len() >= usize::from(WINDOW)
            || self
                .expected_length
                .is_some_and(|length| self.inbound.next_offset() - self.start > length)
        {
            return Err(SessionError::Protocol(
                "browser stream exceeded its bounded window or body",
            ));
        }
        self.queue.push_back(Ok(ProxyChunk {
            offset: chunk.offset,
            data: bytes::Bytes::copy_from_slice(chunk.data.as_bytes()),
        }));
        self.waker.wake();
        Ok(())
    }
    fn end(&mut self, end: FetchEndBody) -> SessionResult<()> {
        if !self.head_seen
            || end.reason != FetchEndReason::Complete
            || end.chunks != self.inbound.chunks()
            || end.bytes != self.inbound.next_offset() - self.start
            || self
                .expected_length
                .is_some_and(|length| length != end.bytes)
        {
            return Err(SessionError::Protocol(
                "browser stream ended with an incomplete or inconsistent body",
            ));
        }
        self.closed = true;
        self.waker.wake();
        Ok(())
    }
}

/// Streams belonging to one authenticated browser connection. A reconnect cannot inherit them.
#[derive(Default)]
pub(crate) struct ProxyStreams {
    states: Mutex<HashMap<StreamId, Arc<Mutex<State>>>>,
}
impl ProxyStreams {
    pub(crate) fn new() -> Self {
        Self::default()
    }
    /// Whether a generic correlated nm.error belongs to a fetch waiting for its head.
    pub(crate) fn has_request(&self, re: Option<&MessageId>) -> bool {
        re.is_some_and(|re| {
            self.states.lock().is_ok_and(|states| {
                states
                    .values()
                    .any(|s| s.lock().is_ok_and(|s| &s.request_id == re))
            })
        })
    }
    /// Route fetch.head/chunk/end/error before the ordinary one-response correlator.
    pub(crate) fn handle(
        &self,
        connection: &BrowserConnection,
        re: Option<&MessageId>,
        body: ExtensionBody,
    ) -> ApiResult<()> {
        let id = match &body {
            ExtensionBody::FetchHead(v) => Some(v.stream_id.clone()),
            ExtensionBody::FetchChunk(v) => Some(v.stream_id.clone()),
            ExtensionBody::FetchEnd(v) => Some(v.stream_id.clone()),
            ExtensionBody::FetchError(v) => Some(v.stream_id.clone()),
            ExtensionBody::Error(_) => self.states.lock().ok().and_then(|states| {
                states.iter().find_map(|(id, s)| {
                    s.lock()
                        .ok()
                        .filter(|s| re == Some(&s.request_id))
                        .map(|_| id.clone())
                })
            }),
            _ => return Err(ApiError::invalid_params("not a browser fetch frame")),
        };
        let Some(id) = id else {
            return Ok(());
        };
        let state = self
            .states
            .lock()
            .map_err(|_| ApiError::internal("browser streams unavailable"))?
            .get(&id)
            .cloned();
        let Some(state) = state else {
            return Ok(());
        }; // Late in-flight frames after cancel are ignored.
        let mut state = state
            .lock()
            .map_err(|_| ApiError::internal("browser stream unavailable"))?;
        if state.closed {
            return Ok(());
        }
        let result = state.access.check().and_then(|()| match body {
            ExtensionBody::FetchHead(v) => state.head(re, v),
            ExtensionBody::FetchChunk(v) if re.is_none() => state.chunk(v),
            ExtensionBody::FetchEnd(v) if re.is_none() => state.end(v),
            ExtensionBody::FetchError(v) if re.is_none() || re == Some(&state.request_id) => {
                if v.bytes != state.inbound.next_offset() - state.start {
                    Err(SessionError::Protocol(
                        "fetch error byte count is inconsistent",
                    ))
                } else {
                    Err(SessionError::Remote(v.error))
                }
            }
            ExtensionBody::Error(v) => Err(SessionError::Remote(v.error)),
            _ => Err(SessionError::Protocol(
                "unexpected fetch response correlation",
            )),
        });
        if let Err(error) = result {
            state.fail(error);
            drop(state);
            let _ = connection.send(
                None,
                FetchCancelBody {
                    stream_id: id,
                    reason: FetchCancelReason::ProtocolViolation,
                },
            );
        }
        Ok(())
    }
    pub(crate) fn disconnect(&self) {
        if let Ok(mut states) = self.states.lock() {
            for (_, state) in states.drain() {
                if let Ok(mut state) = state.lock() {
                    state.fail(SessionError::ConnectionLost);
                }
            }
        }
    }
    fn remove(&self, id: &StreamId) {
        if let Ok(mut states) = self.states.lock() {
            states.remove(id);
        }
    }
}

struct Body {
    connection: Arc<BrowserConnection>,
    id: StreamId,
    state: Arc<Mutex<State>>,
    acknowledge: bool,
    done: bool,
    idle: Pin<Box<tokio::time::Sleep>>,
}
impl Drop for Body {
    fn drop(&mut self) {
        self.connection.proxy.remove(&self.id);
        if !self.done {
            let _ = self.connection.send(
                None,
                FetchCancelBody {
                    stream_id: self.id.clone(),
                    reason: FetchCancelReason::Superseded,
                },
            );
        }
    }
}
impl Stream for Body {
    type Item = SessionResult<ProxyChunk>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        use std::future::Future;
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        let state_arc = this.state.clone();
        let Ok(mut state) = state_arc.lock() else {
            this.done = true;
            return Poll::Ready(Some(Err(SessionError::ConnectionLost)));
        };
        state.waker.register(cx.waker());
        if let Err(error) = state.access.check() {
            state.fail(error);
        }
        // Calling next again is the engine's acknowledgement that it handled the previous chunk.
        // No background task can return credits while disk/commit work is stalled.
        if this.acknowledge && !state.closed {
            this.acknowledge = false;
            if let Some(credits) = state.inbound.credits_to_return(WINDOW, true)
                && this
                    .connection
                    .send(
                        None,
                        FetchCreditBody {
                            stream_id: this.id.clone(),
                            credits,
                        },
                    )
                    .is_err()
            {
                state.fail(SessionError::ConnectionLost);
            }
        }
        if let Some(item) = state.queue.pop_front() {
            this.acknowledge = item.is_ok();
            this.idle.as_mut().reset(tokio::time::Instant::now() + IDLE);
            if item.is_err() {
                this.done = true;
                drop(state);
                this.connection.proxy.remove(&this.id);
                let _ = this.connection.send(
                    None,
                    FetchCancelBody {
                        stream_id: this.id.clone(),
                        reason: FetchCancelReason::Superseded,
                    },
                );
            }
            return Poll::Ready(Some(item));
        }
        if state.closed {
            this.done = true;
            drop(state);
            this.connection.proxy.remove(&this.id);
            return Poll::Ready(None);
        }
        drop(state);
        if this.idle.as_mut().poll(cx).is_ready() {
            this.done = true;
            this.connection.proxy.remove(&this.id);
            let _ = this.connection.send(
                None,
                FetchCancelBody {
                    stream_id: this.id.clone(),
                    reason: FetchCancelReason::Superseded,
                },
            );
            return Poll::Ready(Some(Err(SessionError::Timeout)));
        }
        Poll::Pending
    }
}

#[async_trait]
impl ProxyFetch for BrowserBroker {
    async fn fetch_range(&self, request: ProxyFetchRequest) -> SessionResult<ProxyResponse> {
        let grant = authorize(&self.journal, &request)?;
        let profile = self.grant_profile(grant, request.profile_id.as_deref())?;
        let connection = self.select(profile.as_deref(), capability::FETCH_PROXY)?;
        let access = Access {
            journal: self.journal.clone(),
            request: request.clone(),
            grant_id: grant,
        };
        let limits = Limits::default();
        if request
            .range
            .is_some_and(|r| r.is_empty() || r.len() > limits.max_range_bytes)
            || !(1024..=limits.max_chunk_bytes).contains(&u64::from(request.chunk_bytes))
            || request
                .request_headers
                .values()
                .any(|v| v.len() > 4096 || v.contains(['\r', '\n', '\0']))
            || request.if_range.as_ref().is_some_and(|v| {
                v.len() > 4096 || v.contains(['\r', '\n', '\0']) || v.starts_with("W/")
            })
        {
            return Err(SessionError::Protocol(
                "invalid browser fetch request limits",
            ));
        }
        let suffix = zeus_types::IdGenerator::new().next_ulid().to_string();
        let id = StreamId::new(format!("s{suffix}")).map_err(SessionError::Protocol)?;
        let request_id = MessageId::new(format!("p{suffix}")).map_err(SessionError::Protocol)?;
        let (head_tx, head_rx) = oneshot::channel();
        let start = request.range.map_or(0, |r| r.start);
        let length = request.range.map(|r| r.len());
        let mut stream_limits = limits;
        stream_limits.max_chunk_bytes = u64::from(request.chunk_bytes);
        let state = Arc::new(Mutex::new(State {
            request_id: request_id.clone(),
            access,
            inbound: InboundStream::new(start, length, WINDOW, &stream_limits),
            start,
            requested_length: length,
            chunk_bytes: request.chunk_bytes,
            expected_length: None,
            head: Some(head_tx),
            head_seen: false,
            queue: VecDeque::with_capacity(usize::from(WINDOW)),
            closed: false,
            waker: Arc::new(AtomicWaker::new()),
        }));
        {
            let mut states = connection
                .proxy
                .states
                .lock()
                .map_err(|_| SessionError::ConnectionLost)?;
            if states.len() >= usize::from(limits.max_concurrent_streams) {
                return Err(SessionError::Remote(zeus_types::ErrorEnvelope::from_code(
                    "fetch.extension_busy",
                    "Browser stream limit reached",
                )));
            }
            states.insert(id.clone(), state.clone());
        }
        // This guard exists before sending: task cancellation while awaiting the head also cancels.
        let body = Body {
            connection: connection.clone(),
            id: id.clone(),
            state,
            acknowledge: false,
            done: false,
            idle: Box::pin(tokio::time::sleep(IDLE)),
        };
        let frame = DaemonMessage::new(
            request_id,
            FetchRangeBody {
                stream_id: id,
                job_id: request.job_id,
                url: request.url,
                origin: request.origin,
                range: request.range.map(|r| FetchRange {
                    offset: r.start,
                    length: r.len(),
                }),
                if_range: request.if_range,
                request_headers: request.request_headers,
                chunk_bytes: request.chunk_bytes,
                credits: WINDOW,
                deadline_ms: request.deadline.as_millis().clamp(1000, 25_000) as u64,
            },
        );
        connection.ctx.notifier.notify("nm.frame",serde_json::json!({"frame":frame.to_json().map_err(|_|SessionError::Protocol("cannot encode browser fetch"))?}))
            .map_err(|_|SessionError::ConnectionLost)?;
        let head = tokio::time::timeout(
            request
                .deadline
                .clamp(Duration::from_secs(1), Duration::from_secs(25)),
            head_rx,
        )
        .await
        .map_err(|_| SessionError::Timeout)?
        .map_err(|_| SessionError::ConnectionLost)??;
        body.state
            .lock()
            .map_err(|_| SessionError::ConnectionLost)?
            .access
            .check()?;
        Ok(ProxyResponse {
            head,
            body: Box::pin(body),
        })
    }
    fn available(&self, profile_id: Option<&str>) -> bool {
        self.select(profile_id, capability::FETCH_PROXY).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use serde_json::json;
    use zeus_api::{
        CallContext, ClientIdentity, ClientKind, ClientRole, auth::CredentialId, server::Notifier,
    };
    use zeus_types::{GrantState, IdGenerator, SessionGrant};

    struct Fixture {
        _directory: tempfile::TempDir,
        broker: Arc<BrowserBroker>,
        connection: Arc<BrowserConnection>,
        outgoing: tokio::sync::mpsc::Receiver<zeus_api::jsonrpc::Message>,
        request: ProxyFetchRequest,
        grant: SessionGrant,
    }
    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let clock: zeus_control::SharedClock = Arc::new(zeus_control::SystemClock::new());
            let journal = Journal::open(
                &directory.path().join("test.redb"),
                &Default::default(),
                clock.clone(),
            )
            .unwrap();
            let ids = IdGenerator::new();
            let job_id = ids.job_id();
            let now = Timestamp::now();
            let job=zeus_types::Job::new(job_id,serde_json::from_value(json!({"url":"https://example.test/file","session_origin":"https://example.test","source":{"kind":"cli","via":"cli"}})).unwrap(),JobState::Downloading,now);
            let internal=serde_json::from_value(json!({"epoch":1,"dest_path":"fixture.bin","zpart_path":"fixture.bin.zpart","zpart_file_id":null,"total_bytes":4096,"data_complete":false,"bound_validators":{},"range_support":"unknown"})).unwrap();
            let grant:SessionGrant=serde_json::from_value(json!({"id":ids.grant_id(),"origin":"https://example.test","scope":"once","state":"active","created_at":now,"expires_at":now.saturating_add_ms(60000),"job_id":job_id,"approval_id":ids.approval_id(),"decided_by":{"kind":"human","surface":"cli_interactive"},"uses":0,"renewable":false})).unwrap();
            journal
                .transact(|tx| {
                    tx.put_job(&zeus_journal::records::JobRecord::new(job, internal))?;
                    tx.put_grant(&grant)
                })
                .unwrap();
            let broker = Arc::new(BrowserBroker::new(journal, clock));
            let (notifier, outgoing) = Notifier::channel(64);
            let extension = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned();
            let ctx = CallContext {
                reply: Default::default(),
                peer: Default::default(),
                attach: Some(zeus_api::methods::daemon::AttachInfo {
                    browser: zeus_types::BrowserName::Chrome,
                    attested_extension: extension.clone(),
                    host_version: "0.1.0".into(),
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
            let hello:HelloBody=serde_json::from_value(json!({"client":"zeus-extension","extension_id":extension,"extension_version":"0.1.0","browser":{"name":"chrome","version":"test"},"protocol_versions":[1],"capabilities":["fetch_proxy"],"profile_id":"0123456789abcdef0123456789abcdef","sw_instance_id":"0123456789abcdef"})).unwrap();
            let connection = broker.attach(ctx, hello, true).unwrap();
            let request = ProxyFetchRequest {
                job_id,
                url: "https://example.test/file".parse().unwrap(),
                origin: "https://example.test".parse().unwrap(),
                range: Some(zeus_types::ByteRange::new(0, 4096).unwrap()),
                if_range: Some("\"fixture\"".into()),
                request_headers: Default::default(),
                chunk_bytes: 1024,
                deadline: Duration::from_secs(5),
                profile_id: None,
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
        async fn outgoing(&mut self) -> DaemonMessage {
            let message = tokio::time::timeout(Duration::from_secs(1), self.outgoing.recv())
                .await
                .unwrap()
                .unwrap();
            let zeus_api::jsonrpc::Message::Notification(notification) = message else {
                panic!("notification expected")
            };
            DaemonMessage::from_json(notification.params.unwrap()["frame"].clone()).unwrap()
        }
        async fn start(&mut self) -> (ProxyResponse, StreamId) {
            let broker = self.broker.clone();
            let request = self.request.clone();
            let task = tokio::spawn(async move { broker.fetch_range(request).await });
            let message = self.outgoing().await;
            let DaemonBody::FetchRange(request) = message.body else {
                panic!("fetch request expected")
            };
            assert_eq!(request.credits, WINDOW);
            self.connection
                .proxy
                .handle(
                    &self.connection,
                    Some(&message.id),
                    FetchHeadBody {
                        stream_id: request.stream_id.clone(),
                        status: 206,
                        final_url: request.url,
                        redirected: false,
                        headers: std::collections::BTreeMap::from([
                            (ResponseHeaderName::ContentLength, "4096".into()),
                            (ResponseHeaderName::ContentRange, "bytes 0-4095/4096".into()),
                            (ResponseHeaderName::Etag, "\"fixture\"".into()),
                        ]),
                    }
                    .into(),
                )
                .unwrap();
            (task.await.unwrap().unwrap(), request.stream_id)
        }
        fn chunk(&self, id: &StreamId, index: u64, offset: u64) {
            self.connection
                .proxy
                .handle(
                    &self.connection,
                    None,
                    FetchChunkBody {
                        stream_id: id.clone(),
                        index,
                        offset,
                        length: 1024,
                        data: ChunkData::new(vec![7; 1024]),
                    }
                    .into(),
                )
                .unwrap();
        }
    }
    #[tokio::test]
    async fn credits_follow_consumer_acknowledgements_and_end_counts_are_verified() {
        let mut f = Fixture::new();
        let (mut response, id) = f.start().await;
        f.chunk(&id, 0, 0);
        f.chunk(&id, 1, 1024);
        assert!(f.outgoing.try_recv().is_err());
        assert_eq!(response.body.next().await.unwrap().unwrap().offset, 0);
        assert!(
            f.outgoing.try_recv().is_err(),
            "handing out a chunk must not replenish its credit"
        );
        assert_eq!(response.body.next().await.unwrap().unwrap().offset, 1024);
        assert!(matches!(
            f.outgoing().await.body,
            DaemonBody::FetchCredit(FetchCreditBody { credits: 1, .. })
        ));
        f.chunk(&id, 2, 2048);
        assert_eq!(response.body.next().await.unwrap().unwrap().offset, 2048);
        assert!(matches!(
            f.outgoing().await.body,
            DaemonBody::FetchCredit(_)
        ));
        f.chunk(&id, 3, 3072);
        f.connection
            .proxy
            .handle(
                &f.connection,
                None,
                FetchEndBody {
                    stream_id: id,
                    reason: FetchEndReason::Complete,
                    chunks: 4,
                    bytes: 4096,
                }
                .into(),
            )
            .unwrap();
        assert_eq!(response.body.next().await.unwrap().unwrap().offset, 3072);
        assert!(response.body.next().await.is_none());
        assert!(f.connection.proxy.states.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn an_unverified_host_cannot_replace_a_trusted_browser_profile() {
        let mut f = Fixture::new();
        let mut forged = f.connection.ctx.clone();
        forged.connection_id = IdGenerator::new().connection_id();
        forged.attach.as_mut().unwrap().parent_verified = false;
        assert!(
            f.broker
                .attach(forged, f.connection.hello.clone(), false)
                .is_err()
        );
        let registered = f.broker.connection(f.connection.ctx.connection_id).unwrap();
        assert!(Arc::ptr_eq(&registered, &f.connection));
        let (response, _) = f.start().await;
        drop(response);
        assert!(matches!(
            f.outgoing().await.body,
            DaemonBody::FetchCancel(_)
        ));
    }
    #[tokio::test]
    async fn excess_credit_wrong_offset_and_false_end_never_reach_the_sink() {
        for violation in 0..3 {
            let mut f = Fixture::new();
            let (mut response, id) = f.start().await;
            match violation {
                0 => {
                    f.chunk(&id, 0, 0);
                    f.chunk(&id, 1, 1024);
                    f.chunk(&id, 2, 2048);
                }
                1 => f.chunk(&id, 0, 1),
                _ => {
                    f.connection
                        .proxy
                        .handle(
                            &f.connection,
                            None,
                            FetchEndBody {
                                stream_id: id,
                                reason: FetchEndReason::Complete,
                                chunks: 0,
                                bytes: 4096,
                            }
                            .into(),
                        )
                        .unwrap();
                }
            }
            assert!(response.body.next().await.unwrap().is_err());
            assert!(matches!(
                f.outgoing().await.body,
                DaemonBody::FetchCancel(_)
            ));
        }
    }
    #[tokio::test]
    async fn revocation_and_kill_switch_discard_already_buffered_chunks() {
        for kill in [false, true] {
            let mut f = Fixture::new();
            let (mut response, id) = f.start().await;
            f.chunk(&id, 0, 0);
            if kill {
                f.broker
                    .journal
                    .transact(|tx| tx.set_kill_switch(true))
                    .unwrap();
            } else {
                let mut grant = f.grant.clone();
                grant.state = GrantState::Revoked;
                f.broker
                    .journal
                    .transact(|tx| tx.put_grant(&grant))
                    .unwrap();
            }
            assert!(
                response.body.next().await.unwrap().is_err(),
                "no buffered bytes after authority is removed"
            );
            assert!(matches!(
                f.outgoing().await.body,
                DaemonBody::FetchCancel(_)
            ));
        }
    }
    #[tokio::test]
    async fn drop_disconnect_and_idle_timeout_release_browser_slots() {
        let mut f = Fixture::new();
        let (response, _) = f.start().await;
        drop(response);
        assert!(matches!(
            f.outgoing().await.body,
            DaemonBody::FetchCancel(_)
        ));
        assert!(f.connection.proxy.states.lock().unwrap().is_empty());
        let (mut response, _) = f.start().await;
        f.connection.proxy.disconnect();
        assert!(response.body.next().await.unwrap().is_err());
        assert!(f.connection.proxy.states.lock().unwrap().is_empty());
    }
    #[tokio::test(start_paused = true)]
    async fn idle_streams_and_unanswered_heads_expire() {
        let mut f = Fixture::new();
        let (mut response, _) = f.start().await;
        tokio::time::advance(IDLE + Duration::from_secs(1)).await;
        assert!(matches!(
            response.body.next().await,
            Some(Err(SessionError::Timeout))
        ));
        assert!(matches!(
            f.outgoing().await.body,
            DaemonBody::FetchCancel(_)
        ));
        let broker = f.broker.clone();
        let request = f.request.clone();
        let task = tokio::spawn(async move { broker.fetch_range(request).await });
        assert!(matches!(f.outgoing().await.body, DaemonBody::FetchRange(_)));
        tokio::time::advance(Duration::from_secs(6)).await;
        assert!(matches!(task.await.unwrap(), Err(SessionError::Timeout)));
        assert!(matches!(
            f.outgoing().await.body,
            DaemonBody::FetchCancel(_)
        ));
        assert!(f.connection.proxy.states.lock().unwrap().is_empty());
    }
}
